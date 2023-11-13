// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{anyhow, Error};
use fidl_fuchsia_power_broker::{BinaryPowerLevel, Permissions, PowerLevel, PowerLevelError};
use futures::channel::mpsc::{unbounded, UnboundedReceiver, UnboundedSender};
use std::collections::HashMap;
use uuid::Uuid;

use crate::credentials::*;
use crate::topology::*;

pub struct Broker {
    catalog: Catalog,
    // The current level for each element, as reported to the broker.
    current: Levels,
    // The level for each element required by the topology.
    required: Levels,
}

impl Broker {
    pub fn new() -> Self {
        Broker { catalog: Catalog::new(), current: Levels::new(), required: Levels::new() }
    }

    pub fn lookup_credentials(&self, token: Token) -> Option<Credential> {
        self.catalog.topology.lookup_credentials(token)
    }

    pub fn get_current_level(&self, id: &ElementID) -> Result<PowerLevel, PowerLevelError> {
        if let Some(level) = self.current.get(id) {
            Ok(level)
        } else {
            Err(PowerLevelError::NotFound)
        }
    }

    pub fn update_current_level(&mut self, id: &ElementID, level: &PowerLevel) {
        tracing::debug!("update_current_level({:?}, {:?})", id, level);
        self.current.update(id, level);
        // Some previously pending claims may now be ready to be activated:
        let active_claims_for_required_element =
            self.catalog.active_claims.for_required_element(id);
        // Find the active claims requiring this element level.
        let claims_satisfied: Vec<&Claim> = active_claims_for_required_element
            .iter()
            .filter(|c| level.satisfies(&c.dependency.requires.level))
            .collect();
        tracing::debug!("claims_satisfied = {:?})", &claims_satisfied);
        for claim in claims_satisfied {
            // Look for pending claims that were (at least partially) blocked
            // by a claim on this element level that is now satisfied.
            // (They may still have other blockers, though.)
            let pending_claims = self
                .catalog
                .pending_claims
                .for_required_element(&claim.dependency.dependent.element);
            tracing::debug!(
                "pending_claims.for_required_element({:?}) = {:?})",
                &claim.dependency.dependent.element,
                &pending_claims
            );
            let claims_activated = self.check_claims_to_activate(&pending_claims);
            tracing::debug!("claims_activated = {:?})", &claims_activated);
            self.update_required_levels(&claims_activated);
        }
        // Find claims to drop
        let claims_to_drop_for_element = self.catalog.active_claims.to_drop_for_element(id);
        let claims_dropped = self.check_claims_to_drop(&claims_to_drop_for_element);
        self.update_required_levels(&claims_dropped);
    }

    pub fn subscribe_current_level(
        &mut self,
        id: &ElementID,
    ) -> UnboundedReceiver<Option<PowerLevel>> {
        self.current.subscribe(id)
    }

    #[allow(dead_code)]
    pub fn get_required_level(&self, id: &ElementID) -> PowerLevel {
        // TODO(b/299637587): Support different Power Levels
        self.required.get(id).unwrap_or(PowerLevel::Binary(BinaryPowerLevel::Off))
    }

    pub fn subscribe_required_level(
        &mut self,
        id: &ElementID,
    ) -> UnboundedReceiver<Option<PowerLevel>> {
        self.required.subscribe(id)
    }

    pub fn acquire_lease(
        &mut self,
        element: &ElementID,
        level: &PowerLevel,
    ) -> Result<Lease, Error> {
        let (original_lease, claims) = self.catalog.create_lease_and_claims(element, level)?;
        let activated_claims = self.check_claims_to_activate(&claims);
        self.update_required_levels(&activated_claims);
        Ok(original_lease)
    }

    pub fn drop_lease(&mut self, lease_id: &LeaseID) -> Result<(), Error> {
        let (_, claims) = self.catalog.drop(lease_id)?;
        let dropped_claims = self.check_claims_to_drop(&claims);
        self.update_required_levels(&dropped_claims);
        Ok(())
    }

    fn update_required_levels(&mut self, claims: &Vec<Claim>) {
        for claim in claims {
            let new_min_level = self.catalog.calc_min_level(&claim.dependency.requires.element);
            tracing::debug!(
                "update required level({:?}, {:?})",
                &claim.dependency.requires.element,
                new_min_level
            );
            self.required.update(&claim.dependency.requires.element, &new_min_level);
        }
    }

    /// Examines a Vec of claims and activates any claim for which all the
    /// dependencies of its required elements are met.
    /// Returns a Vec of activated claims.
    fn check_claims_to_activate(&mut self, claims: &Vec<Claim>) -> Vec<Claim> {
        tracing::debug!("check_claims_to_activate: {:?}", claims);
        let mut claims_to_activate = Vec::new();
        for claim in claims {
            let check_deps = self
                .catalog
                .topology
                .get_direct_deps(&claim.dependency.requires)
                .into_iter()
                .try_for_each(|dep: Dependency| {
                    match self.get_current_level(&dep.requires.element) {
                        Ok(current) => {
                            if !current.satisfies(&dep.requires.level) {
                                Err(anyhow!(
                                    "element {:?} at {:?}, requires {:?}",
                                    &dep.requires.element,
                                    &current,
                                    &dep.requires.level
                                ))
                            } else {
                                tracing::debug!("dep {:?} satisfied", dep);
                                Ok(())
                            }
                        }
                        Err(err) => Err(anyhow!(": {:?}", err)),
                    }
                });
            // If there were any errors, some dependencies are not satisfied,
            // so we can't activate this claim.
            if let Err(err) = check_deps {
                tracing::debug!("claim {:?} cannot be activated: {:?}", claim, err);
                continue;
            }
            tracing::debug!("will activate claim: {:?}", claim);
            claims_to_activate.push(claim.clone());
        }
        for claim in &claims_to_activate {
            self.catalog.activate_claim(&claim.id);
        }
        claims_to_activate
    }

    /// Examines a Vec of claims and drops any that no longer have any
    /// dependents. Returns a Vec of dropped claims.
    fn check_claims_to_drop(&mut self, claims: &Vec<Claim>) -> Vec<Claim> {
        tracing::debug!("check_claims_to_drop: {:?}", claims);
        let mut claims_to_drop = Vec::new();
        for claim_to_check in claims {
            let mut has_dependents = false;
            // Only claims belonging to the same lease can be a dependent.
            for related_claim in self.catalog.active_claims.for_lease(&claim_to_check.lease_id) {
                if related_claim.dependency.requires == claim_to_check.dependency.dependent {
                    has_dependents = true;
                    break;
                }
            }
            if has_dependents {
                continue;
            }
            tracing::debug!("will drop claim: {:?}", claim_to_check);
            claims_to_drop.push(claim_to_check.clone());
        }
        for claim in &claims_to_drop {
            self.catalog.drop_claim(&claim.id);
        }
        claims_to_drop
    }

    pub fn add_element(
        &mut self,
        name: &str,
        credentials_to_register: Vec<CredentialToRegister>,
    ) -> Result<ElementID, AddElementError> {
        self.catalog.topology.add_element(name, credentials_to_register)
    }

    pub fn remove_element(&mut self, token: Token) -> Result<(), RemoveElementError> {
        self.catalog.topology.remove_element(token)
    }

    /// Checks authorization from tokens, and if valid, adds a dependency to the Topology.
    pub fn add_dependency(
        &mut self,
        dependent_token: Token,
        dependent_level: PowerLevel,
        requires_token: Token,
        requires_level: PowerLevel,
    ) -> Result<(), AddDependencyError> {
        let Some(dependent_cred) = self.lookup_credentials(dependent_token) else {
            return Err(AddDependencyError::NotAuthorized);
        };
        if !dependent_cred.contains(Permissions::MODIFY_DEPENDENCY) {
            return Err(AddDependencyError::NotAuthorized);
        }
        let Some(requires_cred) = self.lookup_credentials(requires_token) else {
            return Err(AddDependencyError::NotAuthorized);
        };
        if !requires_cred.contains(Permissions::MODIFY_DEPENDENT) {
            return Err(AddDependencyError::NotAuthorized);
        }
        self.catalog.topology.add_direct_dep(&Dependency {
            dependent: ElementLevel {
                element: dependent_cred.get_element().clone(),
                level: dependent_level,
            },
            requires: ElementLevel {
                element: requires_cred.get_element().clone(),
                level: requires_level,
            },
        })
    }

    /// Checks authorization from tokens, and if valid, removes a dependency from the Topology.
    pub fn remove_dependency(
        &mut self,
        dependent_token: Token,
        dependent_level: PowerLevel,
        requires_token: Token,
        requires_level: PowerLevel,
    ) -> Result<(), RemoveDependencyError> {
        let Some(dependent_cred) = self.lookup_credentials(dependent_token) else {
            return Err(RemoveDependencyError::NotAuthorized);
        };
        if !dependent_cred.contains(Permissions::MODIFY_DEPENDENCY) {
            return Err(RemoveDependencyError::NotAuthorized);
        }
        let Some(requires_cred) = self.lookup_credentials(requires_token) else {
            return Err(RemoveDependencyError::NotAuthorized);
        };
        if !requires_cred.contains(Permissions::MODIFY_DEPENDENT) {
            return Err(RemoveDependencyError::NotAuthorized);
        }
        self.catalog.topology.remove_direct_dep(&Dependency {
            dependent: ElementLevel {
                element: dependent_cred.get_element().clone(),
                level: dependent_level,
            },
            requires: ElementLevel {
                element: requires_cred.get_element().clone(),
                level: requires_level,
            },
        })
    }

    pub fn register_credentials(
        &mut self,
        token: Token,
        credentials_to_register: Vec<CredentialToRegister>,
    ) -> Result<(), RegisterCredentialsError> {
        self.catalog.topology.register_credentials(token, credentials_to_register)
    }

    pub fn unregister_credentials(
        &mut self,
        token: Token,
        tokens_to_unregister: Vec<Token>,
    ) -> Result<(), UnregisterCredentialsError> {
        self.catalog.topology.unregister_credentials(token, tokens_to_unregister)
    }
}

type LeaseID = String;

#[derive(Clone, Debug, Eq, Hash, Ord, PartialOrd, PartialEq)]
pub struct Lease {
    pub id: LeaseID,
    pub element: ElementID,
    pub level: PowerLevel,
}

impl Lease {
    fn new(element: &ElementID, level: &PowerLevel) -> Self {
        let id = LeaseID::from(Uuid::new_v4().as_simple().to_string());
        Lease { id: id.clone(), element: element.clone(), level: level.clone() }
    }
}

type ClaimID = String;

#[derive(Clone, Debug, Eq, Hash, Ord, PartialOrd, PartialEq)]
struct Claim {
    pub id: ClaimID,
    pub dependency: Dependency,
    pub lease_id: LeaseID,
}

impl Claim {
    fn new(dependency: Dependency, lease_id: &LeaseID) -> Self {
        Claim {
            id: ClaimID::from(Uuid::new_v4().as_simple().to_string()),
            dependency,
            lease_id: lease_id.clone(),
        }
    }
}

#[derive(Debug)]
struct Catalog {
    topology: Topology,
    leases: HashMap<LeaseID, Lease>,
    /// Active claims affect the required level communicated by Power Broker.
    /// All dependencies of their required element are met.
    active_claims: ClaimTracker,
    /// Pending claims do not yet affect the required level communicated by
    /// Power Broker. Some dependencies of their required element are not met.
    pending_claims: ClaimTracker,
}

impl Catalog {
    fn new() -> Self {
        Catalog {
            topology: Topology::new(),
            leases: HashMap::new(),
            active_claims: ClaimTracker::new(),
            pending_claims: ClaimTracker::new(),
        }
    }

    /// Calculates the required level for each element, according to the
    /// Minimum Power Level Policy. Only active claims are considered here.
    fn calc_min_level(&self, element: &ElementID) -> PowerLevel {
        let no_claims = Vec::new();
        let element_claims = self
            .active_claims
            .claims_by_required_element
            .get(element)
            // Treat both missing key and empty vec as no claims.
            .unwrap_or(&no_claims)
            .iter()
            .filter_map(|id| self.active_claims.claims.get(id));
        element_claims
            .map(|x| x.dependency.requires.level)
            .max()
            // No claims, default to OFF
            // TODO(b/299637587): support other power level types.
            .unwrap_or(PowerLevel::Binary(BinaryPowerLevel::Off))
    }

    /// Creates a new lease for the given element and level along with all
    /// claims necessary to satisfy this lease and adds them to pending_claims.
    /// Returns the new lease, and a Vec of all claims created.
    fn create_lease_and_claims(
        &mut self,
        element: &ElementID,
        level: &PowerLevel,
    ) -> Result<(Lease, Vec<Claim>), Error> {
        tracing::debug!("acquire({:?}, {:?})", &element, &level);
        // TODO: Add lease validation and control.
        let lease = Lease::new(&element, level);
        self.leases.insert(lease.id.clone(), lease.clone());
        // Create claims for all of the transitive dependencies.
        let mut claims = Vec::new();
        let element_level = ElementLevel { element: element.clone(), level: level.clone() };
        for dependency in self.topology.get_all_deps(&element_level) {
            // TODO: Make sure this is permitted by Limiters (once we have them).
            let dep_lease = Claim::new(dependency, &lease.id);
            claims.push(dep_lease);
        }
        for claim in claims.iter() {
            tracing::debug!("adding pending claim: {:?}", &claim);
            self.pending_claims.add(claim.clone());
        }
        Ok((lease, claims))
    }

    /// Drops an existing lease, and initiates process of releasing all
    /// associated claims.
    /// Returns the dropped lease, and a Vec of all active claims that have
    /// been marked to drop.
    fn drop(&mut self, lease_id: &LeaseID) -> Result<(Lease, Vec<Claim>), Error> {
        tracing::debug!("drop({:?})", &lease_id);
        let lease = self.leases.remove(lease_id).ok_or(anyhow!("{lease_id} not found"))?;
        let active_claims = self.active_claims.for_lease(&lease.id);
        tracing::debug!("active_claim_ids: {:?}", &active_claims);
        let pending_claims = self.pending_claims.for_lease(&lease.id);
        tracing::debug!("pending_claim_ids: {:?}", &pending_claims);
        // Pending claims should be dropped immediately.
        for claim in pending_claims {
            if let Some(removed) = self.pending_claims.remove(&claim.id) {
                tracing::debug!("removing pending claim: {:?}", &removed);
            } else {
                tracing::error!("cannot remove pending claim: not found: {}", claim.id);
            }
        }
        // Active claims should be marked to drop in an orderly sequence.
        let claims_marked_to_drop = self.active_claims.mark_lease_claims_to_drop(&lease.id);
        Ok((lease, claims_marked_to_drop))
    }

    /// Activates a pending claim, moving it to active_claims.
    fn activate_claim(&mut self, claim_id: &ClaimID) {
        tracing::debug!("activate_claim: {:?}", claim_id);
        self.pending_claims.move_to(&claim_id, &mut self.active_claims);
    }

    /// Drops a claim, removing it from active_claims.
    fn drop_claim(&mut self, claim_id: &ClaimID) {
        tracing::debug!("drop_claim: {:?}", claim_id);
        self.active_claims.remove(&claim_id);
    }
}

#[derive(Debug)]
struct ClaimTracker {
    claims: HashMap<ClaimID, Claim>,
    claims_by_required_element: HashMap<ElementID, Vec<ClaimID>>,
    claims_by_lease: HashMap<LeaseID, Vec<ClaimID>>,
    claims_to_drop_by_element: HashMap<ElementID, Vec<ClaimID>>,
}

impl ClaimTracker {
    fn new() -> Self {
        ClaimTracker {
            claims: HashMap::new(),
            claims_by_required_element: HashMap::new(),
            claims_by_lease: HashMap::new(),
            claims_to_drop_by_element: HashMap::new(),
        }
    }

    fn add(&mut self, claim: Claim) {
        self.claims_by_required_element
            .entry(claim.dependency.requires.element.clone())
            .or_insert(Vec::new())
            .push(claim.id.clone());
        self.claims_by_lease
            .entry(claim.lease_id.clone())
            .or_insert(Vec::new())
            .push(claim.id.clone());
        self.claims.insert(claim.id.clone(), claim);
    }

    fn remove(&mut self, id: &ClaimID) -> Option<Claim> {
        let Some(claim) = self.claims.remove(id) else {
            return None;
        };
        if let Some(claim_ids) =
            self.claims_by_required_element.get_mut(&claim.dependency.requires.element)
        {
            claim_ids.retain(|x| x != id);
            if claim_ids.is_empty() {
                self.claims_by_required_element.remove(&claim.dependency.requires.element);
            }
        }
        if let Some(claim_ids) = self.claims_by_lease.get_mut(&claim.lease_id) {
            claim_ids.retain(|x| x != id);
            if claim_ids.is_empty() {
                self.claims_by_lease.remove(&claim.lease_id);
            }
        }
        if let Some(claim_ids) =
            self.claims_to_drop_by_element.get_mut(&claim.dependency.dependent.element)
        {
            claim_ids.retain(|x| x != id);
            if claim_ids.is_empty() {
                self.claims_to_drop_by_element.remove(&claim.dependency.dependent.element);
            }
        }
        Some(claim)
    }

    /// Marks all claims associated with this lease to drop. They will be
    /// removed in an orderly sequence (each claim will be removed only once
    /// all claims dependent on it have already been dropped).
    /// Returns a Vec of Claims marked to drop.
    fn mark_lease_claims_to_drop(&mut self, lease_id: &LeaseID) -> Vec<Claim> {
        let claims_marked = self.for_lease(lease_id);
        for claim in &claims_marked {
            self.claims_to_drop_by_element
                .entry(claim.dependency.dependent.element.clone())
                .or_insert(Vec::new())
                .push(claim.id.clone());
        }
        claims_marked
    }

    /// Removes claim from this tracker, and adds it to recipient.
    fn move_to(&mut self, id: &ClaimID, recipient: &mut ClaimTracker) {
        if let Some(claim) = self.remove(id) {
            recipient.add(claim);
        }
    }

    fn for_claim_ids(&self, claim_ids: &Vec<ClaimID>) -> Vec<Claim> {
        claim_ids.iter().map(|id| self.claims.get(id)).filter_map(|f| f).cloned().collect()
    }

    fn for_required_element(&self, element_id: &ElementID) -> Vec<Claim> {
        let Some(claim_ids) = self.claims_by_required_element.get(element_id) else {
            return Vec::new();
        };
        self.for_claim_ids(claim_ids)
    }

    fn for_lease(&self, lease_id: &LeaseID) -> Vec<Claim> {
        let Some(claim_ids) = self.claims_by_lease.get(lease_id) else {
            return Vec::new();
        };
        self.for_claim_ids(claim_ids)
    }

    fn to_drop_for_element(&self, element_id: &ElementID) -> Vec<Claim> {
        let Some(claim_ids) = self.claims_to_drop_by_element.get(element_id) else {
            return Vec::new();
        };
        self.for_claim_ids(claim_ids)
    }
}

/// Holds PowerLevels for each element and publishes updates to subscribers.
#[derive(Debug)]
struct Levels {
    level_map: HashMap<ElementID, PowerLevel>,
    channels: HashMap<ElementID, Vec<UnboundedSender<Option<PowerLevel>>>>,
}

impl Levels {
    fn new() -> Self {
        Levels { level_map: HashMap::new(), channels: HashMap::new() }
    }

    fn get(&self, id: &ElementID) -> Option<PowerLevel> {
        self.level_map.get(id).cloned()
    }

    fn update(&mut self, id: &ElementID, level: &PowerLevel) {
        tracing::debug!("Levels.update({:?}): {:?}", &id, &level);
        self.level_map.insert(id.clone(), level.clone());
        let mut senders_to_retain = Vec::new();
        if let Some(senders) = self.channels.remove(id) {
            for sender in senders {
                tracing::debug!("Levels.update send: {:?}", level.clone());
                if let Err(err) = sender.unbounded_send(Some(level.clone())) {
                    if err.is_disconnected() {
                        tracing::debug!(
                            "Levels.update sender disconnected, will be pruned: {:?}",
                            &sender
                        );
                        continue;
                    }
                    tracing::error!("Levels.update send failed: {:?}", err)
                }
                senders_to_retain.push(sender);
            }
        }
        // Prune invalid senders.
        self.channels.insert(id.clone(), senders_to_retain);
    }

    fn subscribe(&mut self, id: &ElementID) -> UnboundedReceiver<Option<PowerLevel>> {
        let (sender, receiver) = unbounded::<Option<PowerLevel>>();
        sender.unbounded_send(self.get(id)).expect("initial send failed");
        self.channels.entry(id.clone()).or_insert(Vec::new()).push(sender);
        receiver
    }
}

/// A PowerLevel satisfies a required PowerLevel if it is
/// greater than or equal to it on the same scale.
trait SatisfyPowerLevel {
    fn satisfies(&self, required: &PowerLevel) -> bool;
}

impl SatisfyPowerLevel for PowerLevel {
    fn satisfies(&self, required: &PowerLevel) -> bool {
        match required {
            PowerLevel::Binary(BinaryPowerLevel::Off) => matches!(self, PowerLevel::Binary(_)),
            PowerLevel::Binary(BinaryPowerLevel::On) => {
                self == &PowerLevel::Binary(BinaryPowerLevel::On)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fidl_fuchsia_power_broker::{BinaryPowerLevel, Permissions, PowerLevel};
    use fuchsia_zircon::{self as zx, HandleBased};

    #[fuchsia::test]
    fn test_levels() {
        let mut levels = Levels::new();

        levels.update(&"A".into(), &PowerLevel::Binary(BinaryPowerLevel::On));
        assert_eq!(levels.get(&"A".into()), Some(PowerLevel::Binary(BinaryPowerLevel::On)));
        assert_eq!(levels.get(&"B".into()), None);

        levels.update(&"A".into(), &PowerLevel::Binary(BinaryPowerLevel::Off));
        levels.update(&"B".into(), &PowerLevel::Binary(BinaryPowerLevel::On));
        assert_eq!(levels.get(&"A".into()), Some(PowerLevel::Binary(BinaryPowerLevel::Off)));
        assert_eq!(levels.get(&"B".into()), Some(PowerLevel::Binary(BinaryPowerLevel::On)));
    }

    #[fuchsia::test]
    fn test_levels_subscribe() {
        let mut levels = Levels::new();

        let mut receiver_a = levels.subscribe(&"A".into());
        let mut receiver_b = levels.subscribe(&"B".into());

        levels.update(&"A".into(), &PowerLevel::Binary(BinaryPowerLevel::On));
        assert_eq!(levels.get(&"A".into()), Some(PowerLevel::Binary(BinaryPowerLevel::On)));
        assert_eq!(levels.get(&"B".into()), None);

        levels.update(&"A".into(), &PowerLevel::Binary(BinaryPowerLevel::Off));
        levels.update(&"B".into(), &PowerLevel::Binary(BinaryPowerLevel::On));
        assert_eq!(levels.get(&"A".into()), Some(PowerLevel::Binary(BinaryPowerLevel::Off)));
        assert_eq!(levels.get(&"B".into()), Some(PowerLevel::Binary(BinaryPowerLevel::On)));

        let mut received_a = Vec::new();
        while let Ok(Some(level)) = receiver_a.try_next() {
            received_a.push(level)
        }
        assert_eq!(
            received_a,
            vec![
                None,
                Some(PowerLevel::Binary(BinaryPowerLevel::On)),
                Some(PowerLevel::Binary(BinaryPowerLevel::Off))
            ]
        );
        let mut received_b = Vec::new();
        while let Ok(Some(level)) = receiver_b.try_next() {
            received_b.push(level)
        }
        assert_eq!(received_b, vec![None, Some(PowerLevel::Binary(BinaryPowerLevel::On))]);
    }

    #[fuchsia::test]
    fn test_broker_lease_direct() {
        // Create a topology of a child element with two direct dependencies.
        // P1 <- C -> P2
        let mut broker = Broker::new();
        let (child_token, child_broker_token) = zx::EventPair::create();
        let child_cred = CredentialToRegister {
            broker_token: child_broker_token.into(),
            permissions: Permissions::MODIFY_DEPENDENCY,
        };
        let child: ElementID =
            broker.add_element("C", vec![child_cred]).expect("add_element failed");
        let (parent1_token, parent1_broker_token) = zx::EventPair::create();
        let parent1_cred = CredentialToRegister {
            broker_token: parent1_broker_token.into(),
            permissions: Permissions::MODIFY_DEPENDENCY | Permissions::MODIFY_DEPENDENT,
        };
        let parent1: ElementID =
            broker.add_element("P1", vec![parent1_cred]).expect("add_element failed");
        let (parent2_token, parent2_broker_token) = zx::EventPair::create();
        let parent2_cred = CredentialToRegister {
            broker_token: parent2_broker_token.into(),
            permissions: Permissions::MODIFY_DEPENDENT,
        };
        let parent2: ElementID =
            broker.add_element("P2", vec![parent2_cred]).expect("add_element failed");
        broker
            .add_dependency(
                child_token.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("dup failed").into(),
                PowerLevel::Binary(BinaryPowerLevel::On),
                parent1_token.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("dup failed").into(),
                PowerLevel::Binary(BinaryPowerLevel::On),
            )
            .expect("add_direct_dep failed");
        broker
            .add_dependency(
                child_token.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("dup failed").into(),
                PowerLevel::Binary(BinaryPowerLevel::On),
                parent2_token.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("dup failed").into(),
                PowerLevel::Binary(BinaryPowerLevel::On),
            )
            .expect("add_direct_dep failed");
        assert_eq!(
            broker.catalog.calc_min_level(&parent1.clone()),
            PowerLevel::Binary(BinaryPowerLevel::Off),
            "Parent 1 should start with min level OFF"
        );
        assert_eq!(
            broker.catalog.calc_min_level(&parent2.clone()),
            PowerLevel::Binary(BinaryPowerLevel::Off),
            "Parent 2 should start with min level OFF"
        );

        // Acquire the lease, which should result in two claims, one
        // for each dependency.
        let lease = broker
            .acquire_lease(&child, &PowerLevel::Binary(BinaryPowerLevel::On))
            .expect("acquire failed");
        assert_eq!(
            broker.catalog.calc_min_level(&parent1.clone()),
            PowerLevel::Binary(BinaryPowerLevel::On),
            "Parent 1 should now have min level ON from direct claim"
        );
        assert_eq!(
            broker.catalog.calc_min_level(&parent2.clone()),
            PowerLevel::Binary(BinaryPowerLevel::On),
            "Parent 2 should now have min level ON from direct claim"
        );

        // Now drop the lease and verify both claims are also dropped.
        broker.drop_lease(&lease.id).expect("drop failed");
        assert_eq!(
            broker.catalog.calc_min_level(&parent1.clone()),
            PowerLevel::Binary(BinaryPowerLevel::Off),
            "Parent 1 should now have min level OFF from dropped claim"
        );
        assert_eq!(
            broker.catalog.calc_min_level(&parent2.clone()),
            PowerLevel::Binary(BinaryPowerLevel::Off),
            "Parent 2 should now have min level OFF from dropped claim"
        );

        // Try dropping the lease one more time, which should result in an error.
        let extra_drop = broker.drop_lease(&lease.id);
        assert!(extra_drop.is_err());
    }

    #[fuchsia::test]
    fn test_broker_lease_transitive() {
        // Create a topology of a child element with two chained transitive
        // dependencies.
        // C -> P -> GP
        let mut broker = Broker::new();
        let (child_token, child_broker_token) = zx::EventPair::create();
        let child_cred = CredentialToRegister {
            broker_token: child_broker_token.into(),
            permissions: Permissions::MODIFY_DEPENDENCY,
        };
        let child: ElementID =
            broker.add_element("C", vec![child_cred]).expect("add_element failed");
        let (parent_token, parent_broker_token) = zx::EventPair::create();
        let parent_cred = CredentialToRegister {
            broker_token: parent_broker_token.into(),
            permissions: Permissions::MODIFY_DEPENDENCY | Permissions::MODIFY_DEPENDENT,
        };
        let parent: ElementID =
            broker.add_element("P", vec![parent_cred]).expect("add_element failed");
        let (grandparent_token, grandparent_broker_token) = zx::EventPair::create();
        let grandparent_cred = CredentialToRegister {
            broker_token: grandparent_broker_token.into(),
            permissions: Permissions::MODIFY_DEPENDENT,
        };
        let grandparent: ElementID =
            broker.add_element("GP", vec![grandparent_cred]).expect("add_element failed");
        broker
            .add_dependency(
                child_token.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("dup failed").into(),
                PowerLevel::Binary(BinaryPowerLevel::On),
                parent_token.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("dup failed").into(),
                PowerLevel::Binary(BinaryPowerLevel::On),
            )
            .expect("add_direct_dep failed");
        broker
            .add_dependency(
                parent_token.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("dup failed").into(),
                PowerLevel::Binary(BinaryPowerLevel::On),
                grandparent_token
                    .duplicate_handle(zx::Rights::SAME_RIGHTS)
                    .expect("dup failed")
                    .into(),
                PowerLevel::Binary(BinaryPowerLevel::On),
            )
            .expect("add_direct_dep failed");
        assert_eq!(
            broker.catalog.calc_min_level(&parent.clone()),
            PowerLevel::Binary(BinaryPowerLevel::Off),
            "Parent should start with min level OFF"
        );
        assert_eq!(
            broker.catalog.calc_min_level(&grandparent.clone()),
            PowerLevel::Binary(BinaryPowerLevel::Off),
            "Grandparent should start with min level OFF"
        );

        // Acquire the lease, which should result in two claims, one
        // for the direct parent dependency, and one for the transitive
        // grandparent dependency.
        let lease = broker
            .acquire_lease(&child.clone(), &PowerLevel::Binary(BinaryPowerLevel::On))
            .expect("acquire failed");
        assert_eq!(
            broker.catalog.calc_min_level(&parent.clone()),
            PowerLevel::Binary(BinaryPowerLevel::Off),
            "Parent should now have min level OFF, waiting on Grandparent to turn ON"
        );
        assert_eq!(
            broker.catalog.calc_min_level(&grandparent.clone()),
            PowerLevel::Binary(BinaryPowerLevel::On),
            "Grandparent should now have min level ON because of no dependencies"
        );

        // Raise Grandparent power level to ON, now Parent claim should be active.
        broker
            .update_current_level(&grandparent.clone(), &PowerLevel::Binary(BinaryPowerLevel::On));
        assert_eq!(
            broker.catalog.calc_min_level(&parent.clone()),
            PowerLevel::Binary(BinaryPowerLevel::On),
            "Parent should now have min level ON"
        );
        assert_eq!(
            broker.catalog.calc_min_level(&grandparent.clone()),
            PowerLevel::Binary(BinaryPowerLevel::On),
            "Grandparent should now have min level ON"
        );

        // Now drop the lease and verify Parent claim is dropped, but
        // Grandparent claim is not yet dropped.
        broker.drop_lease(&lease.id).expect("drop failed");
        assert_eq!(
            broker.catalog.calc_min_level(&parent.clone()),
            PowerLevel::Binary(BinaryPowerLevel::Off),
            "Parent should now have min level OFF after lease drop"
        );
        assert_eq!(
            broker.catalog.calc_min_level(&grandparent.clone()),
            PowerLevel::Binary(BinaryPowerLevel::On),
            "Grandparent should still have min level ON"
        );

        // Lower Parent power level to OFF, now Grandparent claim should be
        // dropped and should have min level OFF.
        broker.update_current_level(&parent.clone(), &PowerLevel::Binary(BinaryPowerLevel::Off));
        tracing::info!("catalog after update_current_level: {:?}", &broker.catalog);
        assert_eq!(
            broker.catalog.calc_min_level(&parent.clone()),
            PowerLevel::Binary(BinaryPowerLevel::Off),
            "Parent should have min level OFF"
        );
        assert_eq!(
            broker.catalog.calc_min_level(&grandparent.clone()),
            PowerLevel::Binary(BinaryPowerLevel::Off),
            "Grandparent should now have min level OFF"
        );
    }

    #[fuchsia::test]
    fn test_broker_lease_shared() {
        // Create a topology of two child elements with a shared
        // parent and grandparent
        // C1 \
        //     > P -> GP
        // C2 /
        let mut broker = Broker::new();
        let (child1_token, child1_broker_token) = zx::EventPair::create();
        let child1_cred = CredentialToRegister {
            broker_token: child1_broker_token.into(),
            permissions: Permissions::MODIFY_DEPENDENCY,
        };
        let child1: ElementID =
            broker.add_element("C1", vec![child1_cred]).expect("add_element failed");
        let (child2_token, child2_broker_token) = zx::EventPair::create();
        let child2_cred = CredentialToRegister {
            broker_token: child2_broker_token.into(),
            permissions: Permissions::MODIFY_DEPENDENCY,
        };
        let child2: ElementID =
            broker.add_element("C2", vec![child2_cred]).expect("add_element failed");
        let (parent_token, parent_broker_token) = zx::EventPair::create();
        let parent_cred = CredentialToRegister {
            broker_token: parent_broker_token.into(),
            permissions: Permissions::MODIFY_DEPENDENCY | Permissions::MODIFY_DEPENDENT,
        };
        let parent: ElementID =
            broker.add_element("P", vec![parent_cred]).expect("add_element failed");
        let (grandparent_token, grandparent_broker_token) = zx::EventPair::create();
        let grandparent_cred = CredentialToRegister {
            broker_token: grandparent_broker_token.into(),
            permissions: Permissions::MODIFY_DEPENDENT,
        };
        let grandparent: ElementID =
            broker.add_element("GP", vec![grandparent_cred]).expect("add_element failed");
        broker
            .add_dependency(
                child1_token.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("dup failed").into(),
                PowerLevel::Binary(BinaryPowerLevel::On),
                parent_token.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("dup failed").into(),
                PowerLevel::Binary(BinaryPowerLevel::On),
            )
            .expect("add_direct_dep failed");
        broker
            .add_dependency(
                child2_token.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("dup failed").into(),
                PowerLevel::Binary(BinaryPowerLevel::On),
                parent_token.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("dup failed").into(),
                PowerLevel::Binary(BinaryPowerLevel::On),
            )
            .expect("add_direct_dep failed");
        broker
            .add_dependency(
                parent_token.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("dup failed").into(),
                PowerLevel::Binary(BinaryPowerLevel::On),
                grandparent_token
                    .duplicate_handle(zx::Rights::SAME_RIGHTS)
                    .expect("dup failed")
                    .into(),
                PowerLevel::Binary(BinaryPowerLevel::On),
            )
            .expect("add_direct_dep failed");
        assert_eq!(
            broker.catalog.calc_min_level(&parent.clone()),
            PowerLevel::Binary(BinaryPowerLevel::Off),
            "Parent should start with min level OFF"
        );
        assert_eq!(
            broker.catalog.calc_min_level(&grandparent.clone()),
            PowerLevel::Binary(BinaryPowerLevel::Off),
            "Grandparent should start with min level OFF"
        );

        // Acquire a lease for the first child.
        let lease1 = broker
            .acquire_lease(&child1, &PowerLevel::Binary(BinaryPowerLevel::On))
            .expect("acquire failed");
        assert_eq!(
            broker.catalog.calc_min_level(&parent.clone()),
            PowerLevel::Binary(BinaryPowerLevel::Off),
            "Parent should now have min level OFF, waiting on Grandparent to turn ON"
        );
        assert_eq!(
            broker.catalog.calc_min_level(&grandparent.clone()),
            PowerLevel::Binary(BinaryPowerLevel::On),
            "Grandparent should now have min level ON because of no dependencies"
        );

        // Raise Grandparent power level to ON, now Parent claim should be active.
        broker
            .update_current_level(&grandparent.clone(), &PowerLevel::Binary(BinaryPowerLevel::On));
        assert_eq!(
            broker.catalog.calc_min_level(&parent.clone()),
            PowerLevel::Binary(BinaryPowerLevel::On),
            "Parent should now have min level ON"
        );
        assert_eq!(
            broker.catalog.calc_min_level(&grandparent.clone()),
            PowerLevel::Binary(BinaryPowerLevel::On),
            "Grandparent should now have min level ON"
        );

        // Acquire a lease for the second child.
        let lease2 = broker
            .acquire_lease(&child2, &PowerLevel::Binary(BinaryPowerLevel::On))
            .expect("acquire failed");
        assert_eq!(
            broker.catalog.calc_min_level(&parent.clone()),
            PowerLevel::Binary(BinaryPowerLevel::On),
            "Parent should still have min level ON"
        );
        assert_eq!(
            broker.catalog.calc_min_level(&grandparent.clone()),
            PowerLevel::Binary(BinaryPowerLevel::On),
            "Grandparent should still have min level ON"
        );

        // Now drop the first lease. Both Parent and Grandparent should still
        // have min level ON.
        broker.drop_lease(&lease1.id).expect("drop failed");
        assert_eq!(
            broker.catalog.calc_min_level(&parent.clone()),
            PowerLevel::Binary(BinaryPowerLevel::On),
            "Parent should still have min level ON from the second claim"
        );
        assert_eq!(
            broker.catalog.calc_min_level(&grandparent.clone()),
            PowerLevel::Binary(BinaryPowerLevel::On),
            "Grandparent should still have min level ON from the second claim"
        );

        // Now drop the second lease. Parent should now have min level OFF.
        // Grandparent should still have min level ON.
        broker.drop_lease(&lease2.id).expect("drop failed");
        assert_eq!(
            broker.catalog.calc_min_level(&parent.clone()),
            PowerLevel::Binary(BinaryPowerLevel::Off),
            "Parent should now have min level OFF"
        );
        assert_eq!(
            broker.catalog.calc_min_level(&grandparent.clone()),
            PowerLevel::Binary(BinaryPowerLevel::On),
            "Grandparent should still have min level ON"
        );

        // Lower Parent power level to OFF, now Grandparent claim should be
        // dropped and should have min level OFF.
        broker.update_current_level(&parent.clone(), &PowerLevel::Binary(BinaryPowerLevel::Off));
        assert_eq!(
            broker.catalog.calc_min_level(&parent.clone()),
            PowerLevel::Binary(BinaryPowerLevel::Off),
            "Parent should have min level OFF"
        );
        assert_eq!(
            broker.catalog.calc_min_level(&grandparent.clone()),
            PowerLevel::Binary(BinaryPowerLevel::Off),
            "Grandparent should now have min level OFF"
        );
    }

    #[fuchsia::test]
    fn test_add_remove_dependency() {
        let mut broker = Broker::new();
        let (token_adamantium_all, token_adamantium_all_broker) = zx::EventPair::create();
        let credential_adamantium_all = CredentialToRegister {
            broker_token: token_adamantium_all_broker.into(),
            permissions: Permissions::all(),
        };
        let (token_vibranium_all, token_vibranium_all_broker) = zx::EventPair::create();
        let credential_vibranium_all = CredentialToRegister {
            broker_token: token_vibranium_all_broker.into(),
            permissions: Permissions::all(),
        };
        broker
            .add_element("Adamantium", vec![credential_adamantium_all])
            .expect("add_element failed");
        broker
            .add_element("Vibranium", vec![credential_vibranium_all])
            .expect("add_element failed");

        // Only MODIFY_DEPENDENCY and MODIFY_DEPENDENT for the respective
        // elements should be required to add and remove a dependency:
        let (token_adamantium_mod_dependency_only, token_adamantium_mod_dependency_only_broker) =
            zx::EventPair::create();
        let credential_adamantium_mod_dependency_only = CredentialToRegister {
            broker_token: token_adamantium_mod_dependency_only_broker.into(),
            permissions: Permissions::MODIFY_DEPENDENCY,
        };
        broker
            .register_credentials(
                token_adamantium_all
                    .duplicate_handle(zx::Rights::SAME_RIGHTS)
                    .expect("duplicate_handle failed")
                    .into(),
                vec![credential_adamantium_mod_dependency_only],
            )
            .expect("register_credentials failed");
        let (token_vibranium_mod_dependent_only, token_vibranium_mod_dependent_only_broker) =
            zx::EventPair::create();
        let credential_vibranium_mod_dependent_only = CredentialToRegister {
            broker_token: token_vibranium_mod_dependent_only_broker.into(),
            permissions: Permissions::MODIFY_DEPENDENT,
        };
        broker
            .register_credentials(
                token_vibranium_all
                    .duplicate_handle(zx::Rights::SAME_RIGHTS)
                    .expect("duplicate_handle failed")
                    .into(),
                vec![credential_vibranium_mod_dependent_only],
            )
            .expect("register_credentials failed");
        broker
            .add_dependency(
                token_adamantium_mod_dependency_only
                    .duplicate_handle(zx::Rights::SAME_RIGHTS)
                    .expect("dup failed")
                    .into(),
                PowerLevel::Binary(BinaryPowerLevel::On),
                token_vibranium_mod_dependent_only
                    .duplicate_handle(zx::Rights::SAME_RIGHTS)
                    .expect("dup failed")
                    .into(),
                PowerLevel::Binary(BinaryPowerLevel::On),
            )
            .expect("add_dependency failed");

        broker
            .remove_dependency(
                token_adamantium_mod_dependency_only
                    .duplicate_handle(zx::Rights::SAME_RIGHTS)
                    .expect("dup failed")
                    .into(),
                PowerLevel::Binary(BinaryPowerLevel::On),
                token_vibranium_mod_dependent_only
                    .duplicate_handle(zx::Rights::SAME_RIGHTS)
                    .expect("dup failed")
                    .into(),
                PowerLevel::Binary(BinaryPowerLevel::On),
            )
            .expect("remove_dependency failed");

        // Adding should return NotAuthorized if missing MODIFY_DEPENDENCY for
        // dependency.level
        let (token_adamantium_none, token_adamantium_none_broker) = zx::EventPair::create();
        let credential_adamantium_none = CredentialToRegister {
            broker_token: token_adamantium_none_broker.into(),
            permissions: Permissions::empty(),
        };
        broker
            .register_credentials(
                token_adamantium_all
                    .duplicate_handle(zx::Rights::SAME_RIGHTS)
                    .expect("duplicate_handle failed")
                    .into(),
                vec![credential_adamantium_none],
            )
            .expect("register_credentials failed");
        let res_add_missing_mod_dependency = broker.add_dependency(
            token_adamantium_none
                .duplicate_handle(zx::Rights::SAME_RIGHTS)
                .expect("dup failed")
                .into(),
            PowerLevel::Binary(BinaryPowerLevel::On),
            token_vibranium_mod_dependent_only
                .duplicate_handle(zx::Rights::SAME_RIGHTS)
                .expect("dup failed")
                .into(),
            PowerLevel::Binary(BinaryPowerLevel::On),
        );
        assert!(matches!(res_add_missing_mod_dependency, Err(AddDependencyError::NotAuthorized)));

        // Adding should return NotAuthorized if missing MODIFY_DEPENDENT for
        // dependency.requires
        let (token_vibranium_none, token_vibranium_none_broker) = zx::EventPair::create();
        let credential_vibranium_none = CredentialToRegister {
            broker_token: token_vibranium_none_broker.into(),
            permissions: Permissions::empty(),
        };
        broker
            .register_credentials(
                token_vibranium_all
                    .duplicate_handle(zx::Rights::SAME_RIGHTS)
                    .expect("duplicate_handle failed")
                    .into(),
                vec![credential_vibranium_none],
            )
            .expect("register_credentials failed");
        let res_add_missing_mod_dependent = broker.add_dependency(
            token_adamantium_mod_dependency_only
                .duplicate_handle(zx::Rights::SAME_RIGHTS)
                .expect("dup failed")
                .into(),
            PowerLevel::Binary(BinaryPowerLevel::On),
            token_vibranium_none
                .duplicate_handle(zx::Rights::SAME_RIGHTS)
                .expect("dup failed")
                .into(),
            PowerLevel::Binary(BinaryPowerLevel::On),
        );
        assert!(matches!(res_add_missing_mod_dependent, Err(AddDependencyError::NotAuthorized)));

        // Adding with extra permissions should work.
        broker
            .add_dependency(
                token_adamantium_all
                    .duplicate_handle(zx::Rights::SAME_RIGHTS)
                    .expect("dup failed")
                    .into(),
                PowerLevel::Binary(BinaryPowerLevel::On),
                token_vibranium_all
                    .duplicate_handle(zx::Rights::SAME_RIGHTS)
                    .expect("dup failed")
                    .into(),
                PowerLevel::Binary(BinaryPowerLevel::On),
            )
            .expect("add_dependency with extra permissions failed");

        // Removing should return NotAuthorized if missing MODIFY_DEPENDENCY for
        // dependency.level
        let res_remove_missing_mod_dependency = broker.remove_dependency(
            token_adamantium_none
                .duplicate_handle(zx::Rights::SAME_RIGHTS)
                .expect("dup failed")
                .into(),
            PowerLevel::Binary(BinaryPowerLevel::On),
            token_vibranium_mod_dependent_only
                .duplicate_handle(zx::Rights::SAME_RIGHTS)
                .expect("dup failed")
                .into(),
            PowerLevel::Binary(BinaryPowerLevel::On),
        );
        assert!(matches!(
            res_remove_missing_mod_dependency,
            Err(RemoveDependencyError::NotAuthorized)
        ));

        // Removing should return NotAuthorized if missing MODIFY_DEPENDENT for
        // dependency.requires
        let res_remove_missing_mod_dependent = broker.remove_dependency(
            token_adamantium_mod_dependency_only
                .duplicate_handle(zx::Rights::SAME_RIGHTS)
                .expect("dup failed")
                .into(),
            PowerLevel::Binary(BinaryPowerLevel::On),
            token_vibranium_none
                .duplicate_handle(zx::Rights::SAME_RIGHTS)
                .expect("dup failed")
                .into(),
            PowerLevel::Binary(BinaryPowerLevel::On),
        );
        assert!(matches!(
            res_remove_missing_mod_dependent,
            Err(RemoveDependencyError::NotAuthorized)
        ));

        // Removing with extra permissions should work.
        broker
            .remove_dependency(
                token_adamantium_all
                    .duplicate_handle(zx::Rights::SAME_RIGHTS)
                    .expect("dup failed")
                    .into(),
                PowerLevel::Binary(BinaryPowerLevel::On),
                token_vibranium_all
                    .duplicate_handle(zx::Rights::SAME_RIGHTS)
                    .expect("dup failed")
                    .into(),
                PowerLevel::Binary(BinaryPowerLevel::On),
            )
            .expect("remove_dependency with extra permissions failed");
    }
}
