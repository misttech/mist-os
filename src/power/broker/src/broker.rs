// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{anyhow, Error};
use fidl_fuchsia_power_broker::{
    self as fpb, BinaryPowerLevel, DependencyType, LeaseStatus, Permissions, PowerLevel,
    RegisterDependencyTokenError, UnregisterDependencyTokenError,
};
use futures::channel::mpsc::{unbounded, UnboundedReceiver, UnboundedSender};
use itertools::Itertools;
use std::collections::HashMap;
use std::collections::HashSet;
use std::hash::Hash;
use std::iter::repeat;
use uuid::Uuid;

use crate::credentials::*;
use crate::topology::*;

pub struct Broker {
    catalog: Catalog,
    credentials: Registry,
    // The current level for each element, as reported to the broker.
    current: SubscribeMap<ElementID, PowerLevel>,
    // The level for each element required by the topology.
    required: SubscribeMap<ElementID, PowerLevel>,
}

impl Broker {
    pub fn new() -> Self {
        Broker {
            catalog: Catalog::new(),
            credentials: Registry::new(),
            current: SubscribeMap::new(),
            required: SubscribeMap::new(),
        }
    }

    fn lookup_credentials(&self, token: Token) -> Option<Credential> {
        self.credentials.lookup(token)
    }

    fn unregister_all_credentials_for_element(&mut self, element_id: &ElementID) {
        self.credentials.unregister_all_for_element(element_id)
    }

    pub fn register_dependency_token(
        &mut self,
        element_id: &ElementID,
        token: Token,
        dependency_type: DependencyType,
    ) -> Result<(), RegisterDependencyTokenError> {
        let permissions = match dependency_type {
            DependencyType::Active => Permissions::MODIFY_ACTIVE_DEPENDENT,
            DependencyType::Passive => Permissions::MODIFY_PASSIVE_DEPENDENT,
        };
        match self
            .credentials
            .register(element_id, CredentialToRegister { broker_token: token, permissions })
        {
            Err(RegisterCredentialsError::AlreadyInUse) => {
                Err(RegisterDependencyTokenError::AlreadyInUse)
            }
            Err(RegisterCredentialsError::Internal) => Err(RegisterDependencyTokenError::Internal),
            Ok(_) => Ok(()),
        }
    }

    pub fn unregister_dependency_token(
        &mut self,
        element_id: &ElementID,
        token: Token,
    ) -> Result<(), UnregisterDependencyTokenError> {
        let Some(credential) = self.lookup_credentials(token) else {
            tracing::debug!("unregister_dependency_token: token not found");
            return Err(UnregisterDependencyTokenError::NotFound);
        };
        if credential.get_element() != element_id {
            tracing::debug!(
                "unregister_dependency_token: token is registered to {:?}, not {:?}",
                &credential.get_element(),
                &element_id,
            );
            return Err(UnregisterDependencyTokenError::NotAuthorized);
        }
        self.credentials.unregister(&credential);
        Ok(())
    }

    fn current_level_satisfies(&self, required: &ElementLevel) -> bool {
        self.current
            .get(&required.element_id)
            // If current level is unknown, required is not satisfied.
            .is_some_and(|current| current.satisfies(required.level))
    }

    pub fn update_current_level(
        &mut self,
        element_id: &ElementID,
        level: PowerLevel,
    ) -> Result<(), fpb::CurrentLevelError> {
        tracing::debug!("update_current_level({:?}, {:?})", element_id, level);
        self.current.update(element_id, level);
        // Some previously pending claims may now be ready to be activated,
        // because the active claims upon which they depend may now be
        // satisfied.
        // Find active or passive claims that are now satisfied by the new
        // current level:
        let claims_for_required_element = self
            .catalog
            .active_claims
            .for_required_element(element_id)
            .into_iter()
            .chain(self.catalog.passive_claims.for_required_element(element_id));
        let claims_satisfied: Vec<Claim> =
            claims_for_required_element.filter(|c| level.satisfies(c.requires().level)).collect();
        tracing::debug!("claims_satisfied = {:?})", &claims_satisfied);
        let mut leases_to_check_if_satisfied = HashSet::new();
        for claim in &claims_satisfied {
            leases_to_check_if_satisfied.insert(&claim.lease_id);
            // Look for pending claims that were (at least partially) blocked
            // by a claim on this element level that is now satisfied.
            // (They may still have other blockers, though.)
            let pending_claims =
                self.catalog.pending_claims.for_required_element(&claim.dependent().element_id);
            tracing::debug!(
                "pending_claims.for_required_element({:?}) = {:?})",
                &claim.dependent().element_id,
                &pending_claims
            );
            let claims_activated = self.activate_claims_if_dependencies_satisfied(pending_claims);
            tracing::debug!("claims_activated = {:?})", &claims_activated);
            self.update_required_levels(element_ids_required_by_claims(&claims_activated));
        }
        tracing::debug!("leases_to_check_if_satisfied = {:?})", &leases_to_check_if_satisfied);
        for lease_id in leases_to_check_if_satisfied {
            self.update_lease_status(lease_id);
        }
        // Find claims to drop
        let claims_to_drop_for_element = self.catalog.active_claims.to_drop_for_element(element_id);
        let claims_dropped = self.drop_claims_with_no_dependents(&claims_to_drop_for_element);
        self.update_required_levels(element_ids_required_by_claims(&claims_dropped));
        Ok(())
    }

    #[cfg(test)]
    pub fn get_current_level(&mut self, element_id: &ElementID) -> Option<PowerLevel> {
        self.current.get(element_id)
    }

    pub fn watch_current_level(
        &mut self,
        element_id: &ElementID,
    ) -> UnboundedReceiver<Option<PowerLevel>> {
        self.current.subscribe(element_id)
    }

    #[cfg(test)]
    pub fn get_required_level(&mut self, element_id: &ElementID) -> Option<PowerLevel> {
        self.required.get(element_id)
    }

    pub fn watch_required_level(
        &mut self,
        element_id: &ElementID,
    ) -> UnboundedReceiver<Option<PowerLevel>> {
        self.required.subscribe(element_id)
    }

    pub fn acquire_lease(
        &mut self,
        element_id: &ElementID,
        level: PowerLevel,
    ) -> Result<Lease, fpb::LeaseError> {
        let (lease, claims) = self.catalog.create_lease_and_claims(element_id, level);
        let claims_activated = self.activate_claims_if_dependencies_satisfied(claims);
        self.update_required_levels(element_ids_required_by_claims(&claims_activated));
        self.update_lease_status(&lease.id);
        Ok(lease)
    }

    pub fn drop_lease(&mut self, lease_id: &LeaseID) -> Result<(), Error> {
        let (_, claims) = self.catalog.drop(lease_id)?;
        let claims_dropped = self.drop_claims_with_no_dependents(&claims);
        self.update_required_levels(element_ids_required_by_claims(&claims_dropped));
        self.catalog.lease_status.remove(lease_id);
        Ok(())
    }

    fn calculate_lease_status(&self, lease_id: &LeaseID) -> LeaseStatus {
        // If a lease has any Pending claims, it is Pending.
        if !self.catalog.pending_claims.for_lease(lease_id).is_empty() {
            return LeaseStatus::Pending;
        }
        // If a lease has any active claims that have not been satisfied
        // it is still Pending.
        for claim in self.catalog.active_claims.for_lease(lease_id) {
            if !self.current_level_satisfies(claim.requires()) {
                return LeaseStatus::Pending;
            }
        }
        // If a lease has any passive claims that have not been satisfied
        // it is still Pending.
        for claim in self.catalog.passive_claims.for_lease(lease_id) {
            if !self.current_level_satisfies(claim.requires()) {
                return LeaseStatus::Pending;
            }
        }
        // All claims are active and satisfied, so the lease is Satisfied.
        LeaseStatus::Satisfied
    }

    pub fn update_lease_status(&mut self, lease_id: &LeaseID) {
        let status = self.calculate_lease_status(lease_id);
        self.catalog.lease_status.update(lease_id, status);
    }

    #[cfg(test)]
    pub fn get_lease_status(&mut self, lease_id: &LeaseID) -> Option<LeaseStatus> {
        self.catalog.get_lease_status(lease_id)
    }

    pub fn watch_lease_status(
        &mut self,
        lease_id: &LeaseID,
    ) -> UnboundedReceiver<Option<LeaseStatus>> {
        self.catalog.watch_lease_status(lease_id)
    }

    fn update_required_levels(&mut self, element_ids: Vec<&ElementID>) {
        for element_id in element_ids {
            let new_required_level = self.catalog.calculate_required_level(element_id);
            tracing::debug!("update required level({:?}, {:?})", element_id, new_required_level);
            self.required.update(element_id, new_required_level);
        }
    }

    /// Examines a Vec of pending claims and activates each claim for which the
    /// required element is already at the required level (and thus the claim
    /// is already satisfied), or all of the dependencies of its required
    /// ElementLevel are met.
    /// For example, let us imagine elements A, B, C and D where A depends on B
    /// and B depends on C and D. In order to activate the A->B claim, all
    /// dependencies of B (i.e. B->C and B->D) must first be satisfied.
    /// Returns a Vec of activated claims.
    fn activate_claims_if_dependencies_satisfied(&mut self, claims: Vec<Claim>) -> Vec<Claim> {
        tracing::debug!("activate_claims_if_dependencies_satisfied: {:?}", claims);
        let claims_to_activate: Vec<Claim> = claims
            .into_iter()
            .filter(|c| {
                // If the required element is already at the required level,
                // then the claim can immediately be activated (and is
                // already satisfied).
                self.current_level_satisfies(c.requires())
                // Otherwise, it can only be activated if all of its
                // dependencies are satisfied.
                    || self.all_dependencies_satisfied(&c.requires())
            })
            .collect();
        for claim in &claims_to_activate {
            self.catalog.activate_pending_claim(&claim.id);
        }
        claims_to_activate
    }

    /// Examines the direct active and passive dependencies of an element level
    /// and returns true if they are all satisfied (current level >= required).
    fn all_dependencies_satisfied(&self, element_level: &ElementLevel) -> bool {
        let active_dependencies = self.catalog.topology.direct_active_dependencies(&element_level);
        let passive_dependencies =
            self.catalog.topology.direct_passive_dependencies(&element_level);
        active_dependencies.into_iter().chain(passive_dependencies).all(|dep| {
            if !self.current_level_satisfies(&dep.requires) {
                tracing::debug!(
                    "dependency {dep:?} of element_level {element_level:?} is not satisfied: \
                    current level of {:?} = {:?}, {:?} required",
                    &dep.requires.element_id,
                    self.current.get(&dep.requires.element_id),
                    &dep.requires.level
                );
                return false;
            }
            return true;
        })
    }

    /// Examines a Vec of claims and drops any that no longer have any
    /// dependents. Returns a Vec of dropped claims.
    fn drop_claims_with_no_dependents(&mut self, claims: &Vec<Claim>) -> Vec<Claim> {
        tracing::debug!("check_claims_to_drop: {:?}", claims);
        let mut claims_to_drop = Vec::new();
        for claim_to_check in claims {
            let mut has_dependents = false;
            // Only claims belonging to the same lease can be a dependent.
            for related_claim in self.catalog.active_claims.for_lease(&claim_to_check.lease_id) {
                if related_claim.requires() == claim_to_check.dependent() {
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
            self.catalog.drop_active_claim(&claim.id);
        }
        claims_to_drop
    }

    pub fn add_element(
        &mut self,
        name: &str,
        initial_current_level: PowerLevel,
        valid_levels: Vec<PowerLevel>,
        level_dependencies: Vec<fpb::LevelDependency>,
        active_dependency_tokens: Vec<Token>,
        passive_dependency_tokens: Vec<Token>,
    ) -> Result<ElementID, AddElementError> {
        if valid_levels.len() < 1 {
            return Err(AddElementError::Invalid);
        }
        let id = self.catalog.topology.add_element(name, valid_levels.to_vec())?;
        self.current.update(&id, initial_current_level);
        let Some(minimum_level) = self.catalog.topology.minimum_level(&id) else {
            self.remove_element(&id);
            return Err(AddElementError::Internal);
        };
        self.required.update(&id, minimum_level);
        for dependency in level_dependencies {
            if let Err(err) = self.add_dependency(
                &id,
                dependency.dependency_type,
                dependency.dependent_level,
                dependency.requires_token.into(),
                dependency.requires_level,
            ) {
                // Clean up by removing the element we just added.
                self.remove_element(&id);
                return Err(match err {
                    ModifyDependencyError::AlreadyExists => AddElementError::Invalid,
                    ModifyDependencyError::Invalid => AddElementError::Invalid,
                    ModifyDependencyError::NotFound(_) => AddElementError::Invalid,
                    ModifyDependencyError::NotAuthorized => AddElementError::NotAuthorized,
                });
            };
        }
        let labeled_dependency_tokens = active_dependency_tokens
            .into_iter()
            .zip(repeat(DependencyType::Active))
            .chain(passive_dependency_tokens.into_iter().zip(repeat(DependencyType::Passive)));
        for (token, dependency_type) in labeled_dependency_tokens {
            if let Err(err) = self.register_dependency_token(&id, token, dependency_type) {
                match err {
                    RegisterDependencyTokenError::Internal => {
                        tracing::debug!("can't register_dependency_token for {:?}: internal", &id);
                        return Err(AddElementError::Internal);
                    }
                    RegisterDependencyTokenError::AlreadyInUse => {
                        tracing::debug!(
                            "can't register_dependency_token for {:?}: already in use",
                            &id
                        );
                        return Err(AddElementError::Invalid);
                    }
                    fpb::RegisterDependencyTokenErrorUnknown!() => {
                        tracing::warn!(
                            "unknown RegisterDependencyTokenError received: {}",
                            err.into_primitive()
                        );
                        return Err(AddElementError::Internal);
                    }
                }
            }
        }
        Ok(id)
    }

    #[cfg(test)]
    fn element_exists(&self, element_id: &ElementID) -> bool {
        self.catalog.topology.element_exists(element_id)
    }

    pub fn remove_element(&mut self, element_id: &ElementID) {
        self.catalog.topology.remove_element(element_id);
        self.unregister_all_credentials_for_element(element_id);
        self.current.remove(element_id);
        self.required.remove(element_id);
    }

    /// Checks authorization from requires_token, and if valid, adds an active
    /// or passive dependency to the Topology, according to dependency_type.
    pub fn add_dependency(
        &mut self,
        element_id: &ElementID,
        dependency_type: DependencyType,
        dependent_level: PowerLevel,
        requires_token: Token,
        requires_level: PowerLevel,
    ) -> Result<(), ModifyDependencyError> {
        let Some(requires_cred) = self.lookup_credentials(requires_token) else {
            return Err(ModifyDependencyError::NotAuthorized);
        };
        let dependency = Dependency {
            dependent: ElementLevel { element_id: element_id.clone(), level: dependent_level },
            requires: ElementLevel {
                element_id: requires_cred.get_element().clone(),
                level: requires_level,
            },
        };
        match dependency_type {
            DependencyType::Active => {
                if !requires_cred.contains(Permissions::MODIFY_ACTIVE_DEPENDENT) {
                    return Err(ModifyDependencyError::NotAuthorized);
                }
                self.catalog.topology.add_active_dependency(&dependency)
            }
            DependencyType::Passive => {
                if !requires_cred.contains(Permissions::MODIFY_PASSIVE_DEPENDENT) {
                    return Err(ModifyDependencyError::NotAuthorized);
                }
                self.catalog.topology.add_passive_dependency(&dependency)
            }
        }
    }

    /// Checks authorization from requires_token, and if valid, removes a dependency from the Topology.
    pub fn remove_dependency(
        &mut self,
        element_id: &ElementID,
        dependency_type: DependencyType,
        dependent_level: PowerLevel,
        requires_token: Token,
        requires_level: PowerLevel,
    ) -> Result<(), ModifyDependencyError> {
        let Some(requires_cred) = self.lookup_credentials(requires_token) else {
            return Err(ModifyDependencyError::NotAuthorized);
        };
        let dependency = Dependency {
            dependent: ElementLevel { element_id: element_id.clone(), level: dependent_level },
            requires: ElementLevel {
                element_id: requires_cred.get_element().clone(),
                level: requires_level,
            },
        };
        match dependency_type {
            DependencyType::Active => {
                if !requires_cred.contains(Permissions::MODIFY_ACTIVE_DEPENDENT) {
                    return Err(ModifyDependencyError::NotAuthorized);
                }
                self.catalog.topology.remove_active_dependency(&dependency)
            }
            DependencyType::Passive => {
                if !requires_cred.contains(Permissions::MODIFY_PASSIVE_DEPENDENT) {
                    return Err(ModifyDependencyError::NotAuthorized);
                }
                self.catalog.topology.remove_passive_dependency(&dependency)
            }
        }
    }
}

pub type LeaseID = String;

#[derive(Clone, Debug, Eq, Hash, Ord, PartialOrd, PartialEq)]
pub struct Lease {
    pub id: LeaseID,
    pub element_id: ElementID,
    pub level: PowerLevel,
}

impl Lease {
    fn new(element_id: &ElementID, level: PowerLevel) -> Self {
        let id = LeaseID::from(Uuid::new_v4().as_simple().to_string());
        Lease { id: id.clone(), element_id: element_id.clone(), level: level.clone() }
    }
}

type ClaimID = String;

#[derive(Clone, Debug, Eq, Hash, Ord, PartialOrd, PartialEq)]
struct Claim {
    pub id: ClaimID,
    dependency: Dependency,
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

    fn dependent(&self) -> &ElementLevel {
        &self.dependency.dependent
    }

    fn requires(&self) -> &ElementLevel {
        &self.dependency.requires
    }
}

/// Returns a Vec of unique ElementIDs required by claims.
fn element_ids_required_by_claims(claims: &Vec<Claim>) -> Vec<&ElementID> {
    claims.into_iter().map(|c| &c.requires().element_id).unique().collect()
}

#[derive(Debug)]
struct Catalog {
    topology: Topology,
    leases: HashMap<LeaseID, Lease>,
    lease_status: SubscribeMap<LeaseID, LeaseStatus>,
    /// Active claims affect the required level communicated by Power Broker.
    /// All dependencies of their required element are met.
    active_claims: ClaimTracker,
    /// Pending claims do not yet affect the required level communicated by
    /// Power Broker. Some dependencies of their required element are not met.
    /// Once all dependencies are met, they will be moved to active claims.
    pending_claims: ClaimTracker,
    /// Passive claims must be satisfied in order for their lease to be
    /// satisfied but since they do not affect the required levels of the
    /// element they depend upon, something else must raise the required
    /// element's level in order for a passive claim to be satisfied (either
    /// another active claim, or an unmanaged element raising its own level).
    passive_claims: ClaimTracker,
}

impl Catalog {
    fn new() -> Self {
        Catalog {
            topology: Topology::new(),
            leases: HashMap::new(),
            lease_status: SubscribeMap::new(),
            active_claims: ClaimTracker::new(),
            pending_claims: ClaimTracker::new(),
            passive_claims: ClaimTracker::new(),
        }
    }

    /// Calculates the required level for each element, according to the
    /// Minimum Power Level Policy. Only active claims are considered here.
    fn calculate_required_level(&self, element_id: &ElementID) -> PowerLevel {
        let no_claims = Vec::new();
        let element_claims = self
            .active_claims
            .claims_by_required_element_id
            .get(element_id)
            // Treat both missing key and empty vec as no claims.
            .unwrap_or(&no_claims)
            .iter()
            .filter_map(|id| self.active_claims.claims.get(id));
        // Return the maximum of all active claims:
        if let Some(required_level) = element_claims.map(|x| x.requires().level).max() {
            required_level
        } else {
            // No claims, return default level:
            if let Some(default_level) = self.topology.minimum_level(element_id) {
                default_level
            } else {
                // TODO(https://fxbug.dev/333930405): This can be `tracing::error!` again if
                // dropping a lease after its corresponding element is gone is handled properly.
                // See `session-manager-integration-test.cm` that exercises that case.
                tracing::warn!("calculate_required_level: no minimum_level for {:?}", element_id);
                BinaryPowerLevel::Off.into_primitive()
            }
        }
    }

    /// Creates a new lease for the given element and level along with all
    /// claims necessary to satisfy this lease and adds them to pending_claims.
    /// Returns the new lease, and a Vec of pending claims created.
    fn create_lease_and_claims(
        &mut self,
        element_id: &ElementID,
        level: PowerLevel,
    ) -> (Lease, Vec<Claim>) {
        tracing::debug!("acquire({:?}, {:?})", &element_id, &level);
        // TODO: Add lease validation and control.
        let lease = Lease::new(&element_id, level);
        self.leases.insert(lease.id.clone(), lease.clone());
        let mut pending_claims = Vec::new();
        let element_level = ElementLevel { element_id: element_id.clone(), level: level.clone() };
        let (active_dependencies, passive_dependencies) =
            self.topology.all_active_and_passive_dependencies(&element_level);
        // Create pending claims for each of the active dependencies.
        for dependency in active_dependencies {
            let pending_claim = Claim::new(dependency, &lease.id);
            pending_claims.push(pending_claim);
        }
        for claim in pending_claims.iter() {
            tracing::debug!("adding pending claim: {:?}", &claim);
            self.pending_claims.add(claim.clone());
        }
        // Create passive claims for each of the passive dependencies.
        for dependency in passive_dependencies {
            let passive_claim = Claim::new(dependency, &lease.id);
            self.passive_claims.add(passive_claim);
        }
        (lease, pending_claims)
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
        let passive_claims = self.passive_claims.for_lease(&lease.id);
        tracing::debug!("pending_claim_ids: {:?}", &passive_claims);
        // Pending claims should be dropped immediately.
        for claim in pending_claims {
            if let Some(removed) = self.pending_claims.remove(&claim.id) {
                tracing::debug!("removing pending claim: {:?}", &removed);
            } else {
                tracing::error!("cannot remove pending claim: not found: {}", claim.id);
            }
        }
        // Passive claims should be dropped immediately.
        for claim in passive_claims {
            if let Some(removed) = self.passive_claims.remove(&claim.id) {
                tracing::debug!("removing passive claim: {:?}", &removed);
            } else {
                tracing::error!("cannot remove passive claim: not found: {}", claim.id);
            }
        }
        // Active claims should be marked to drop in an orderly sequence.
        let claims_marked_to_drop = self.active_claims.mark_lease_claims_to_drop(&lease.id);
        Ok((lease, claims_marked_to_drop))
    }

    /// Activates a pending claim, moving it to active_claims.
    fn activate_pending_claim(&mut self, claim_id: &ClaimID) {
        tracing::debug!("activate_claim: {:?}", claim_id);
        self.pending_claims.move_to(&claim_id, &mut self.active_claims);
    }

    /// Drops an active claim, removing it from active_claims.
    fn drop_active_claim(&mut self, claim_id: &ClaimID) {
        tracing::debug!("drop_claim: {:?}", claim_id);
        self.active_claims.remove(&claim_id);
    }

    #[cfg(test)]
    pub fn get_lease_status(&mut self, lease_id: &LeaseID) -> Option<LeaseStatus> {
        self.lease_status.get(lease_id)
    }

    fn watch_lease_status(&mut self, lease_id: &LeaseID) -> UnboundedReceiver<Option<LeaseStatus>> {
        self.lease_status.subscribe(lease_id)
    }
}

#[derive(Debug)]
struct ClaimTracker {
    claims: HashMap<ClaimID, Claim>,
    claims_by_required_element_id: HashMap<ElementID, Vec<ClaimID>>,
    claims_by_lease: HashMap<LeaseID, Vec<ClaimID>>,
    claims_to_drop_by_element_id: HashMap<ElementID, Vec<ClaimID>>,
}

impl ClaimTracker {
    fn new() -> Self {
        ClaimTracker {
            claims: HashMap::new(),
            claims_by_required_element_id: HashMap::new(),
            claims_by_lease: HashMap::new(),
            claims_to_drop_by_element_id: HashMap::new(),
        }
    }

    fn add(&mut self, claim: Claim) {
        self.claims_by_required_element_id
            .entry(claim.requires().element_id.clone())
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
            self.claims_by_required_element_id.get_mut(&claim.requires().element_id)
        {
            claim_ids.retain(|x| x != id);
            if claim_ids.is_empty() {
                self.claims_by_required_element_id.remove(&claim.requires().element_id);
            }
        }
        if let Some(claim_ids) = self.claims_by_lease.get_mut(&claim.lease_id) {
            claim_ids.retain(|x| x != id);
            if claim_ids.is_empty() {
                self.claims_by_lease.remove(&claim.lease_id);
            }
        }
        if let Some(claim_ids) =
            self.claims_to_drop_by_element_id.get_mut(&claim.dependent().element_id)
        {
            claim_ids.retain(|x| x != id);
            if claim_ids.is_empty() {
                self.claims_to_drop_by_element_id.remove(&claim.dependent().element_id);
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
            self.claims_to_drop_by_element_id
                .entry(claim.dependent().element_id.clone())
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
        let Some(claim_ids) = self.claims_by_required_element_id.get(element_id) else {
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
        let Some(claim_ids) = self.claims_to_drop_by_element_id.get(element_id) else {
            return Vec::new();
        };
        self.for_claim_ids(claim_ids)
    }
}

/// SubscribeMap is a wrapper around a HashMap that stores values V by key K
/// and allows subscribers to register a channel on which they will receive
/// updates whenever the value stored changes.
#[derive(Debug)]
struct SubscribeMap<K: Clone + Hash + Eq, V: Clone + PartialEq> {
    values: HashMap<K, V>,
    senders: HashMap<K, Vec<UnboundedSender<Option<V>>>>,
}

impl<K: Clone + Hash + Eq, V: Clone + PartialEq> SubscribeMap<K, V> {
    fn new() -> Self {
        SubscribeMap { values: HashMap::new(), senders: HashMap::new() }
    }

    fn get(&self, key: &K) -> Option<V> {
        self.values.get(key).cloned()
    }

    fn update(&mut self, key: &K, value: V) {
        // If the value hasn't changed, this is a no-op.
        if let Some(current) = self.get(key) {
            if current == value {
                return;
            }
        }
        self.values.insert(key.clone(), value.clone());
        let mut senders_to_retain = Vec::new();
        if let Some(senders) = self.senders.remove(&key) {
            for sender in senders {
                if let Err(err) = sender.unbounded_send(Some(value.clone())) {
                    if err.is_disconnected() {
                        continue;
                    }
                }
                senders_to_retain.push(sender);
            }
        }
        // Prune invalid senders.
        self.senders.insert(key.clone(), senders_to_retain);
    }

    fn subscribe(&mut self, key: &K) -> UnboundedReceiver<Option<V>> {
        let (sender, receiver) = unbounded::<Option<V>>();
        sender.unbounded_send(self.get(key)).expect("initial send failed");
        self.senders.entry(key.clone()).or_insert(Vec::new()).push(sender);
        receiver
    }

    fn remove(&mut self, key: &K) {
        self.values.remove(key);
        self.senders.remove(key);
    }
}

/// A PowerLevel satisfies a required PowerLevel if it is
/// greater than or equal to it on the same scale.
trait SatisfyPowerLevel {
    fn satisfies(&self, required: PowerLevel) -> bool;
}

impl SatisfyPowerLevel for PowerLevel {
    fn satisfies(&self, required: PowerLevel) -> bool {
        self >= &required
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fidl_fuchsia_power_broker::DependencyToken;
    use fuchsia_zircon::{self as zx, HandleBased};
    use power_broker_client::BINARY_POWER_LEVELS;

    #[fuchsia::test]
    fn test_binary_satisfy_power_level() {
        for (level, required, want) in [
            (BinaryPowerLevel::Off.into_primitive(), BinaryPowerLevel::On.into_primitive(), false),
            (BinaryPowerLevel::Off.into_primitive(), BinaryPowerLevel::Off.into_primitive(), true),
            (BinaryPowerLevel::On.into_primitive(), BinaryPowerLevel::Off.into_primitive(), true),
            (BinaryPowerLevel::On.into_primitive(), BinaryPowerLevel::On.into_primitive(), true),
        ] {
            let got = level.satisfies(required);
            assert_eq!(
                got, want,
                "{:?}.satisfies({:?}) = {:?}, want {:?}",
                level, required, got, want
            );
        }
    }

    #[fuchsia::test]
    fn test_user_defined_satisfy_power_level() {
        for (level, required, want) in [
            (0, 1, false),
            (0, 0, true),
            (1, 0, true),
            (1, 1, true),
            (255, 0, true),
            (255, 1, true),
            (255, 255, true),
            (1, 255, false),
            (35, 36, false),
            (35, 35, true),
        ] {
            let got = level.satisfies(required);
            assert_eq!(
                got, want,
                "{:?}.satisfies({:?}) = {:?}, want {:?}",
                level, required, got, want
            );
        }
    }

    #[fuchsia::test]
    fn test_levels() {
        let mut levels = SubscribeMap::<ElementID, PowerLevel>::new();

        levels.update(&"A".into(), BinaryPowerLevel::On.into_primitive());
        assert_eq!(levels.get(&"A".into()), Some(BinaryPowerLevel::On.into_primitive()));
        assert_eq!(levels.get(&"B".into()), None);

        levels.update(&"A".into(), BinaryPowerLevel::Off.into_primitive());
        levels.update(&"B".into(), BinaryPowerLevel::On.into_primitive());
        assert_eq!(levels.get(&"A".into()), Some(BinaryPowerLevel::Off.into_primitive()));
        assert_eq!(levels.get(&"B".into()), Some(BinaryPowerLevel::On.into_primitive()));

        levels.update(&"UD1".into(), 145);
        assert_eq!(levels.get(&"UD1".into()), Some(145));
        assert_eq!(levels.get(&"UD2".into()), None);
    }

    #[fuchsia::test]
    fn test_levels_subscribe() {
        let mut levels = SubscribeMap::<ElementID, PowerLevel>::new();

        let mut receiver_a = levels.subscribe(&"A".into());
        let mut receiver_b = levels.subscribe(&"B".into());

        levels.update(&"A".into(), BinaryPowerLevel::On.into_primitive());
        assert_eq!(levels.get(&"A".into()), Some(BinaryPowerLevel::On.into_primitive()));
        assert_eq!(levels.get(&"B".into()), None);

        levels.update(&"A".into(), BinaryPowerLevel::Off.into_primitive());
        levels.update(&"B".into(), BinaryPowerLevel::On.into_primitive());
        assert_eq!(levels.get(&"A".into()), Some(BinaryPowerLevel::Off.into_primitive()));
        assert_eq!(levels.get(&"B".into()), Some(BinaryPowerLevel::On.into_primitive()));

        let mut received_a = Vec::new();
        while let Ok(Some(level)) = receiver_a.try_next() {
            received_a.push(level)
        }
        assert_eq!(
            received_a,
            vec![
                None,
                Some(BinaryPowerLevel::On.into_primitive()),
                Some(BinaryPowerLevel::Off.into_primitive())
            ]
        );
        let mut received_b = Vec::new();
        while let Ok(Some(level)) = receiver_b.try_next() {
            received_b.push(level)
        }
        assert_eq!(received_b, vec![None, Some(BinaryPowerLevel::On.into_primitive())]);
    }

    #[fuchsia::test]
    fn test_initialize_current_and_required_levels() {
        let mut broker = Broker::new();
        let latinum = broker
            .add_element(
                "Latinum",
                7,
                vec![5, 2, 7], // unsorted, should still choose the minimum value
                vec![],
                vec![],
                vec![],
            )
            .expect("add_element failed");
        assert_eq!(broker.get_current_level(&latinum), Some(7));
        assert_eq!(broker.get_required_level(&latinum), Some(2));
    }

    #[fuchsia::test]
    fn test_add_element_dependency_never_and_unregistered() {
        let mut broker = Broker::new();
        let token_mithril = DependencyToken::create();
        let never_registered_token = DependencyToken::create();
        let mithril = broker
            .add_element(
                "Mithril",
                BinaryPowerLevel::Off.into_primitive(),
                BINARY_POWER_LEVELS.to_vec(),
                vec![],
                vec![token_mithril
                    .duplicate_handle(zx::Rights::SAME_RIGHTS)
                    .expect("dup failed")
                    .into()],
                vec![],
            )
            .expect("add_element failed");

        // This should fail, because the token was never registered.
        let add_element_not_authorized_res = broker.add_element(
            "Silver",
            BinaryPowerLevel::Off.into_primitive(),
            BINARY_POWER_LEVELS.to_vec(),
            vec![fpb::LevelDependency {
                dependency_type: DependencyType::Active,
                dependent_level: BinaryPowerLevel::On.into_primitive(),
                requires_token: never_registered_token
                    .duplicate_handle(zx::Rights::SAME_RIGHTS)
                    .expect("dup failed"),
                requires_level: BinaryPowerLevel::On.into_primitive(),
            }],
            vec![],
            vec![],
        );
        assert!(matches!(add_element_not_authorized_res, Err(AddElementError::NotAuthorized)));

        // Add element with a valid token should succeed.
        broker
            .add_element(
                "Silver",
                BinaryPowerLevel::Off.into_primitive(),
                BINARY_POWER_LEVELS.to_vec(),
                vec![fpb::LevelDependency {
                    dependency_type: DependencyType::Active,
                    dependent_level: BinaryPowerLevel::On.into_primitive(),
                    requires_token: token_mithril
                        .duplicate_handle(zx::Rights::SAME_RIGHTS)
                        .expect("dup failed"),
                    requires_level: BinaryPowerLevel::On.into_primitive(),
                }],
                vec![],
                vec![],
            )
            .expect("add_element failed");

        // Unregister token_mithril, then try to add again, which should fail.
        broker
            .unregister_dependency_token(
                &mithril,
                token_mithril.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("dup failed").into(),
            )
            .expect("unregister_dependency_token failed");

        let add_element_not_authorized_res: Result<ElementID, AddElementError> = broker
            .add_element(
                "Silver",
                BinaryPowerLevel::Off.into_primitive(),
                BINARY_POWER_LEVELS.to_vec(),
                vec![fpb::LevelDependency {
                    dependency_type: DependencyType::Active,
                    dependent_level: BinaryPowerLevel::On.into_primitive(),
                    requires_token: token_mithril
                        .duplicate_handle(zx::Rights::SAME_RIGHTS)
                        .expect("dup failed"),
                    requires_level: BinaryPowerLevel::On.into_primitive(),
                }],
                vec![],
                vec![],
            );
        assert!(matches!(add_element_not_authorized_res, Err(AddElementError::NotAuthorized)));
    }

    #[fuchsia::test]
    fn test_remove_element() {
        let mut broker = Broker::new();
        let unobtanium = broker
            .add_element(
                "Unobtainium",
                BinaryPowerLevel::Off.into_primitive(),
                BINARY_POWER_LEVELS.to_vec(),
                vec![],
                vec![],
                vec![],
            )
            .expect("add_element failed");
        assert_eq!(broker.element_exists(&unobtanium), true);

        broker.remove_element(&unobtanium);
        assert_eq!(broker.element_exists(&unobtanium), false);
    }

    #[fuchsia::test]
    fn test_broker_lease_direct() {
        // Create a topology of a child element with two direct dependencies.
        // P1 <- C -> P2
        let mut broker = Broker::new();
        let parent1_token = DependencyToken::create();
        let parent1: ElementID = broker
            .add_element(
                "P1",
                BinaryPowerLevel::Off.into_primitive(),
                BINARY_POWER_LEVELS.to_vec(),
                vec![],
                vec![parent1_token
                    .duplicate_handle(zx::Rights::SAME_RIGHTS)
                    .expect("dup failed")
                    .into()],
                vec![],
            )
            .expect("add_element failed");
        let parent2_token = DependencyToken::create();
        let parent2: ElementID = broker
            .add_element(
                "P2",
                BinaryPowerLevel::Off.into_primitive(),
                BINARY_POWER_LEVELS.to_vec(),
                vec![],
                vec![parent2_token
                    .duplicate_handle(zx::Rights::SAME_RIGHTS)
                    .expect("dup failed")
                    .into()],
                vec![],
            )
            .expect("add_element failed");
        let child = broker
            .add_element(
                "C",
                BinaryPowerLevel::Off.into_primitive(),
                BINARY_POWER_LEVELS.to_vec(),
                vec![
                    fpb::LevelDependency {
                        dependency_type: DependencyType::Active,
                        dependent_level: BinaryPowerLevel::On.into_primitive(),
                        requires_token: parent1_token
                            .duplicate_handle(zx::Rights::SAME_RIGHTS)
                            .expect("dup failed"),
                        requires_level: BinaryPowerLevel::On.into_primitive(),
                    },
                    fpb::LevelDependency {
                        dependency_type: DependencyType::Active,
                        dependent_level: BinaryPowerLevel::On.into_primitive(),
                        requires_token: parent2_token
                            .duplicate_handle(zx::Rights::SAME_RIGHTS)
                            .expect("dup failed"),
                        requires_level: BinaryPowerLevel::On.into_primitive(),
                    },
                ],
                vec![],
                vec![],
            )
            .expect("add_element failed");
        assert_eq!(
            broker.catalog.calculate_required_level(&parent1),
            BinaryPowerLevel::Off.into_primitive(),
            "Parent 1 should start with required level OFF"
        );
        assert_eq!(
            broker.catalog.calculate_required_level(&parent2),
            BinaryPowerLevel::Off.into_primitive(),
            "Parent 2 should start with required level OFF"
        );

        // Acquire the lease, which should result in two claims, one
        // for each dependency.
        let lease = broker
            .acquire_lease(&child, BinaryPowerLevel::On.into_primitive())
            .expect("acquire failed");
        assert_eq!(
            broker.catalog.calculate_required_level(&parent1),
            BinaryPowerLevel::On.into_primitive(),
            "Parent 1 should now have required level ON from direct claim"
        );
        assert_eq!(
            broker.catalog.calculate_required_level(&parent2),
            BinaryPowerLevel::On.into_primitive(),
            "Parent 2 should now have required level ON from direct claim"
        );

        // Now drop the lease and verify both claims are also dropped.
        broker.drop_lease(&lease.id).expect("drop failed");
        assert_eq!(
            broker.catalog.calculate_required_level(&parent1),
            BinaryPowerLevel::Off.into_primitive(),
            "Parent 1 should now have required level OFF from dropped claim"
        );
        assert_eq!(
            broker.catalog.calculate_required_level(&parent2),
            BinaryPowerLevel::Off.into_primitive(),
            "Parent 2 should now have required level OFF from dropped claim"
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
        let grandparent_token = DependencyToken::create();
        let grandparent: ElementID = broker
            .add_element(
                "GP",
                BinaryPowerLevel::Off.into_primitive(),
                BINARY_POWER_LEVELS.to_vec(),
                vec![],
                vec![grandparent_token
                    .duplicate_handle(zx::Rights::SAME_RIGHTS)
                    .expect("dup failed")
                    .into()],
                vec![],
            )
            .expect("add_element failed");
        let parent_token = DependencyToken::create();
        let parent: ElementID = broker
            .add_element(
                "P",
                BinaryPowerLevel::Off.into_primitive(),
                BINARY_POWER_LEVELS.to_vec(),
                vec![],
                vec![parent_token
                    .duplicate_handle(zx::Rights::SAME_RIGHTS)
                    .expect("dup failed")
                    .into()],
                vec![],
            )
            .expect("add_element failed");
        let child = broker
            .add_element(
                "C",
                BinaryPowerLevel::Off.into_primitive(),
                BINARY_POWER_LEVELS.to_vec(),
                vec![fpb::LevelDependency {
                    dependency_type: DependencyType::Active,
                    dependent_level: BinaryPowerLevel::On.into_primitive(),
                    requires_token: parent_token
                        .duplicate_handle(zx::Rights::SAME_RIGHTS)
                        .expect("dup failed"),
                    requires_level: BinaryPowerLevel::On.into_primitive(),
                }],
                vec![],
                vec![],
            )
            .expect("add_element failed");
        broker
            .add_dependency(
                &parent,
                DependencyType::Active,
                BinaryPowerLevel::On.into_primitive(),
                grandparent_token
                    .duplicate_handle(zx::Rights::SAME_RIGHTS)
                    .expect("dup failed")
                    .into(),
                BinaryPowerLevel::On.into_primitive(),
            )
            .expect("add_dependency failed");
        assert_eq!(
            broker.catalog.calculate_required_level(&parent),
            BinaryPowerLevel::Off.into_primitive(),
            "Parent should start with required level OFF"
        );
        assert_eq!(
            broker.catalog.calculate_required_level(&grandparent),
            BinaryPowerLevel::Off.into_primitive(),
            "Grandparent should start with required level OFF"
        );

        // Acquire the lease, which should result in two claims, one
        // for the direct parent dependency, and one for the transitive
        // grandparent dependency.
        let lease = broker
            .acquire_lease(&child, BinaryPowerLevel::On.into_primitive())
            .expect("acquire failed");
        assert_eq!(
            broker.catalog.calculate_required_level(&parent),
            BinaryPowerLevel::Off.into_primitive(),
            "Parent should now have required level OFF, waiting on Grandparent to turn ON"
        );
        assert_eq!(
            broker.catalog.calculate_required_level(&grandparent),
            BinaryPowerLevel::On.into_primitive(),
            "Grandparent should now have required level ON because of no dependencies"
        );

        // Raise Grandparent power level to ON, now Parent claim should be active.
        broker
            .update_current_level(&grandparent, BinaryPowerLevel::On.into_primitive())
            .expect("update_current_level failed");
        assert_eq!(
            broker.catalog.calculate_required_level(&parent),
            BinaryPowerLevel::On.into_primitive(),
            "Parent should now have required level ON"
        );
        assert_eq!(
            broker.catalog.calculate_required_level(&grandparent),
            BinaryPowerLevel::On.into_primitive(),
            "Grandparent should now have required level ON"
        );

        // Now drop the lease and verify Parent claim is dropped, but
        // Grandparent claim is not yet dropped.
        broker.drop_lease(&lease.id).expect("drop failed");
        assert_eq!(
            broker.catalog.calculate_required_level(&parent),
            BinaryPowerLevel::Off.into_primitive(),
            "Parent should now have required level OFF after lease drop"
        );
        assert_eq!(
            broker.catalog.calculate_required_level(&grandparent),
            BinaryPowerLevel::On.into_primitive(),
            "Grandparent should still have required level ON"
        );

        // Lower Parent power level to OFF, now Grandparent claim should be
        // dropped and should have required level OFF.
        broker
            .update_current_level(&parent, BinaryPowerLevel::Off.into_primitive())
            .expect("update_current_level failed");
        tracing::info!("catalog after update_current_level: {:?}", &broker.catalog);
        assert_eq!(
            broker.catalog.calculate_required_level(&parent),
            BinaryPowerLevel::Off.into_primitive(),
            "Parent should have required level OFF"
        );
        assert_eq!(
            broker.catalog.calculate_required_level(&grandparent),
            BinaryPowerLevel::Off.into_primitive(),
            "Grandparent should now have required level OFF"
        );
    }

    #[fuchsia::test]
    fn test_broker_lease_shared() {
        // Create a topology of two child elements with a shared
        // parent and grandparent
        // C1 \
        //     > P -> GP
        // C2 /
        // Child 1 requires Parent at 50 to support its own level of 5.
        // Parent requires Grandparent at 200 to support its own level of 50.
        // C1 -> P -> GP
        //  5 -> 50 -> 200
        // Child 2 requires Parent at 30 to support its own level of 3.
        // Parent requires Grandparent at 90 to support its own level of 30.
        // C2 -> P -> GP
        //  3 -> 30 -> 90
        // Grandparent has a minimum required level of 10.
        // All other elements have a minimum of 0.
        let mut broker = Broker::new();
        let grandparent_token = DependencyToken::create();
        let grandparent: ElementID = broker
            .add_element(
                "GP",
                10,
                vec![10, 90, 200],
                vec![],
                vec![grandparent_token
                    .duplicate_handle(zx::Rights::SAME_RIGHTS)
                    .expect("dup failed")
                    .into()],
                vec![],
            )
            .expect("add_element failed");
        let parent_token = DependencyToken::create();
        let parent: ElementID = broker
            .add_element(
                "P",
                0,
                vec![0, 30, 50],
                vec![
                    fpb::LevelDependency {
                        dependency_type: DependencyType::Active,
                        dependent_level: 50,
                        requires_token: grandparent_token
                            .duplicate_handle(zx::Rights::SAME_RIGHTS)
                            .expect("dup failed"),
                        requires_level: 200,
                    },
                    fpb::LevelDependency {
                        dependency_type: DependencyType::Active,
                        dependent_level: 30,
                        requires_token: grandparent_token
                            .duplicate_handle(zx::Rights::SAME_RIGHTS)
                            .expect("dup failed"),
                        requires_level: 90,
                    },
                ],
                vec![parent_token
                    .duplicate_handle(zx::Rights::SAME_RIGHTS)
                    .expect("dup failed")
                    .into()],
                vec![],
            )
            .expect("add_element failed");
        let child1 = broker
            .add_element(
                "C1",
                0,
                vec![0, 5],
                vec![fpb::LevelDependency {
                    dependency_type: DependencyType::Active,
                    dependent_level: 5,
                    requires_token: parent_token
                        .duplicate_handle(zx::Rights::SAME_RIGHTS)
                        .expect("dup failed"),
                    requires_level: 50,
                }],
                vec![],
                vec![],
            )
            .expect("add_element failed");
        let child2 = broker
            .add_element(
                "C2",
                0,
                vec![0, 3],
                vec![fpb::LevelDependency {
                    dependency_type: DependencyType::Active,
                    dependent_level: 3,
                    requires_token: parent_token
                        .duplicate_handle(zx::Rights::SAME_RIGHTS)
                        .expect("dup failed"),
                    requires_level: 30,
                }],
                vec![],
                vec![],
            )
            .expect("add_element failed");

        // Initially, Grandparent should have a default required level of 10
        // and Parent should have a default required level of 0.
        assert_eq!(
            broker.catalog.calculate_required_level(&parent),
            0,
            "Parent should start with required level 0"
        );
        assert_eq!(
            broker.catalog.calculate_required_level(&grandparent),
            10,
            "Grandparent should start with required level at its default of 10"
        );

        // Acquire lease for Child 1. Initially, Grandparent should have
        // required level 200 and Parent should have required level 0
        // because Child 1 has a dependency on Parent and Parent has a
        // dependency on Grandparent. Grandparent has no dependencies so its
        // level should be raised first.
        let lease1 = broker.acquire_lease(&child1, 5).expect("acquire failed");
        assert_eq!(
            broker.catalog.calculate_required_level(&parent),
            0,
            "Parent should now have required level 0, waiting on Grandparent to reach required level"
        );
        assert_eq!(
            broker.catalog.calculate_required_level(&grandparent),
            200,
            "Grandparent should now have required level 100 because of parent dependency and it has no dependencies of its own"
        );

        // Raise Grandparent's current level to 200. Now Parent claim should
        // be active, because its dependency on Grandparent is unblocked
        // raising its required level to 50.
        broker.update_current_level(&grandparent, 200).expect("update_current_level failed");
        assert_eq!(
            broker.catalog.calculate_required_level(&parent),
            50,
            "Parent should now have required level 50"
        );
        assert_eq!(
            broker.catalog.calculate_required_level(&grandparent),
            200,
            "Grandparent should still have required level 200"
        );

        // Update Parent's current level to 50.
        // Parent and Grandparent should have required levels of 50 and 200.
        broker.update_current_level(&parent, 50).expect("update_current_level failed");
        assert_eq!(
            broker.catalog.calculate_required_level(&parent),
            50,
            "Parent should now have required level 50"
        );
        assert_eq!(
            broker.catalog.calculate_required_level(&grandparent),
            200,
            "Grandparent should still have required level 200"
        );

        // Acquire a lease for Child 2. Though Child 2 has nominal
        // requirements of Parent at 30 and Grandparent at 100, they are
        // superseded by Child 1's requirements of 50 and 200.
        let lease2 = broker.acquire_lease(&child2, 3).expect("acquire failed");
        assert_eq!(
            broker.catalog.calculate_required_level(&parent),
            50,
            "Parent should still have required level 50"
        );
        assert_eq!(
            broker.catalog.calculate_required_level(&grandparent),
            200,
            "Grandparent should still have required level 100"
        );

        // Drop lease for Child 1. Parent's required level should immediately
        // drop to 30. Grandparent's required level will remain at 200 for now.
        broker.drop_lease(&lease1.id).expect("drop failed");
        assert_eq!(
            broker.catalog.calculate_required_level(&parent),
            30,
            "Parent should still have required level 2 from the second claim"
        );
        assert_eq!(
            broker.catalog.calculate_required_level(&grandparent),
            200,
            "Grandparent should still have required level 100 from the second claim"
        );

        // Lower Parent's current level to 30. Now Grandparent's required level
        // should drop to 90.
        broker.update_current_level(&parent, 30).expect("update_current_level failed");
        assert_eq!(
            broker.catalog.calculate_required_level(&parent),
            30,
            "Parent should have required level 30"
        );
        assert_eq!(
            broker.catalog.calculate_required_level(&grandparent),
            90,
            "Grandparent should now have required level 90"
        );

        // Drop lease for Child 2, Parent should have required level 0.
        // Grandparent should still have required level 90.
        broker.drop_lease(&lease2.id).expect("drop failed");
        assert_eq!(
            broker.catalog.calculate_required_level(&parent),
            0,
            "Parent should now have required level 0"
        );
        assert_eq!(
            broker.catalog.calculate_required_level(&grandparent),
            90,
            "Grandparent should still have required level 90"
        );

        // Lower Parent's current level to 0. Grandparent claim should now be
        // dropped and have its default required level of 10.
        broker.update_current_level(&parent, 0).expect("update_current_level failed");
        assert_eq!(
            broker.catalog.calculate_required_level(&parent),
            0,
            "Parent should have required level 0"
        );
        assert_eq!(
            broker.catalog.calculate_required_level(&grandparent),
            10,
            "Grandparent should now have required level 10"
        );
    }

    #[fuchsia::test]
    async fn test_broker_lease_passive() {
        // B has an active dependency on A.
        // C has a passive dependency on B (and transitively, a passive dependency on A).
        // D has an active dependency on B (and transitively, an active dependency on A).
        //  A     B     C     D
        // ON <= ON
        //       ON <- ON
        //       ON <======= ON
        let mut broker = Broker::new();
        let token_a = DependencyToken::create();
        let element_a = broker
            .add_element(
                "A",
                BinaryPowerLevel::Off.into_primitive(),
                BINARY_POWER_LEVELS.to_vec(),
                vec![],
                vec![token_a.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("dup failed").into()],
                vec![],
            )
            .expect("add_element failed");
        let token_b_active = DependencyToken::create();
        let token_b_passive = DependencyToken::create();
        let element_b = broker
            .add_element(
                "B",
                BinaryPowerLevel::Off.into_primitive(),
                BINARY_POWER_LEVELS.to_vec(),
                vec![fpb::LevelDependency {
                    dependency_type: DependencyType::Active,
                    dependent_level: BinaryPowerLevel::On.into_primitive(),
                    requires_token: token_a
                        .duplicate_handle(zx::Rights::SAME_RIGHTS)
                        .expect("dup failed")
                        .into(),
                    requires_level: BinaryPowerLevel::On.into_primitive(),
                }],
                vec![token_b_active
                    .duplicate_handle(zx::Rights::SAME_RIGHTS)
                    .expect("dup failed")
                    .into()],
                vec![token_b_passive
                    .duplicate_handle(zx::Rights::SAME_RIGHTS)
                    .expect("dup failed")
                    .into()],
            )
            .expect("add_element failed");
        let element_c = broker
            .add_element(
                "C",
                BinaryPowerLevel::Off.into_primitive(),
                BINARY_POWER_LEVELS.to_vec(),
                vec![fpb::LevelDependency {
                    dependency_type: DependencyType::Passive,
                    dependent_level: BinaryPowerLevel::On.into_primitive(),
                    requires_token: token_b_passive
                        .duplicate_handle(zx::Rights::SAME_RIGHTS)
                        .expect("dup failed")
                        .into(),
                    requires_level: BinaryPowerLevel::On.into_primitive(),
                }],
                vec![],
                vec![],
            )
            .expect("add_element failed");
        let element_d = broker
            .add_element(
                "D",
                BinaryPowerLevel::Off.into_primitive(),
                BINARY_POWER_LEVELS.to_vec(),
                vec![fpb::LevelDependency {
                    dependency_type: DependencyType::Active,
                    dependent_level: BinaryPowerLevel::On.into_primitive(),
                    requires_token: token_b_active
                        .duplicate_handle(zx::Rights::SAME_RIGHTS)
                        .expect("dup failed")
                        .into(),
                    requires_level: BinaryPowerLevel::On.into_primitive(),
                }],
                vec![],
                vec![],
            )
            .expect("add_element failed");

        // Initial required level for A and B should be OFF.
        // Set A and B's current level to OFF.
        assert_eq!(
            broker.get_required_level(&element_a),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_b),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        broker
            .update_current_level(&element_a, BinaryPowerLevel::Off.into_primitive())
            .expect("update_current_level failed");
        broker
            .update_current_level(&element_b, BinaryPowerLevel::Off.into_primitive())
            .expect("update_current_level failed");

        // Lease C, A & B's required levels should remain OFF because of
        // passive claim.
        // Lease C should remain unsatisfied.
        // TODO(b/311419716): When we have lease rejection, reject this lease.
        let lease_c = broker
            .acquire_lease(&element_c, BinaryPowerLevel::On.into_primitive())
            .expect("acquire failed");
        assert_eq!(
            broker.get_required_level(&element_a),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_b),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(broker.get_lease_status(&lease_c.id), Some(LeaseStatus::Pending));

        // Lease D, A should have required level ON because of D's transitive active claim.
        // Lease C & D should become satisfied.
        let lease_d = broker
            .acquire_lease(&element_d, BinaryPowerLevel::On.into_primitive())
            .expect("acquire failed");
        assert_eq!(
            broker.get_required_level(&element_a),
            Some(BinaryPowerLevel::On.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_b),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(broker.get_lease_status(&lease_c.id), Some(LeaseStatus::Pending));
        assert_eq!(broker.get_lease_status(&lease_d.id), Some(LeaseStatus::Pending));

        // Update A's current level to ON.
        // B should now have required level ON because of D's active claim and
        // its dependency on A being satisfied.
        broker
            .update_current_level(&element_a, BinaryPowerLevel::On.into_primitive())
            .expect("update_current_level failed");
        assert_eq!(
            broker.get_required_level(&element_a),
            Some(BinaryPowerLevel::On.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_b),
            Some(BinaryPowerLevel::On.into_primitive())
        );
        assert_eq!(broker.get_lease_status(&lease_c.id), Some(LeaseStatus::Pending));
        assert_eq!(broker.get_lease_status(&lease_d.id), Some(LeaseStatus::Pending));

        // Update B's current level to ON.
        // Lease C & D should become satisfied.
        broker
            .update_current_level(&element_b, BinaryPowerLevel::On.into_primitive())
            .expect("update_current_level failed");
        assert_eq!(broker.get_lease_status(&lease_c.id), Some(LeaseStatus::Satisfied));
        assert_eq!(broker.get_lease_status(&lease_d.id), Some(LeaseStatus::Satisfied));

        // Drop Lease on D, B's required level should become OFF.
        broker.drop_lease(&lease_d.id).expect("drop_lease failed");
        assert_eq!(
            broker.get_required_level(&element_a),
            Some(BinaryPowerLevel::On.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_b),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        // TODO(b/308659273): When we have lease revocation, verify lease C was revoked.

        // Update B's current level to OFF. A's required level should become OFF.
        broker
            .update_current_level(&element_b, BinaryPowerLevel::Off.into_primitive())
            .expect("update_current_level failed");
        assert_eq!(
            broker.get_required_level(&element_a),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_b),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
    }

    #[fuchsia::test]
    fn test_add_remove_dependency() {
        let mut broker = Broker::new();
        let token_active_adamantium = DependencyToken::create();
        let token_passive_adamantium = DependencyToken::create();
        let token_active_vibranium = DependencyToken::create();
        let token_passive_vibranium = DependencyToken::create();
        let adamantium = broker
            .add_element(
                "Adamantium",
                BinaryPowerLevel::Off.into_primitive(),
                BINARY_POWER_LEVELS.to_vec(),
                vec![],
                vec![token_active_adamantium
                    .duplicate_handle(zx::Rights::SAME_RIGHTS)
                    .expect("dup failed")
                    .into()],
                vec![token_passive_adamantium
                    .duplicate_handle(zx::Rights::SAME_RIGHTS)
                    .expect("dup failed")
                    .into()],
            )
            .expect("add_element failed");
        broker
            .add_element(
                "Vibranium",
                BinaryPowerLevel::Off.into_primitive(),
                BINARY_POWER_LEVELS.to_vec(),
                vec![],
                vec![token_active_vibranium
                    .duplicate_handle(zx::Rights::SAME_RIGHTS)
                    .expect("dup failed")
                    .into()],
                vec![token_passive_vibranium
                    .duplicate_handle(zx::Rights::SAME_RIGHTS)
                    .expect("dup failed")
                    .into()],
            )
            .expect("add_element failed");

        // Adding should return NotAuthorized if token is not registered
        let unregistered_token = DependencyToken::create();
        let res_add_not_authorized = broker.add_dependency(
            &adamantium,
            DependencyType::Active,
            BinaryPowerLevel::On.into_primitive(),
            unregistered_token.into(),
            BinaryPowerLevel::On.into_primitive(),
        );
        assert!(matches!(res_add_not_authorized, Err(ModifyDependencyError::NotAuthorized)));

        // Adding should return NotAuthorized if token is of wrong dependency type
        let res_add_wrong_type = broker.add_dependency(
            &adamantium,
            DependencyType::Active,
            BinaryPowerLevel::On.into_primitive(),
            token_passive_vibranium
                .duplicate_handle(zx::Rights::SAME_RIGHTS)
                .expect("dup failed")
                .into(),
            BinaryPowerLevel::On.into_primitive(),
        );
        assert!(matches!(res_add_wrong_type, Err(ModifyDependencyError::NotAuthorized)));

        // Add of invalid dependent level should fail.
        const INVALID_POWER_LEVEL: PowerLevel = 5;
        let res_add_invalid_dep_level = broker.add_dependency(
            &adamantium,
            DependencyType::Active,
            INVALID_POWER_LEVEL,
            token_active_vibranium
                .duplicate_handle(zx::Rights::SAME_RIGHTS)
                .expect("dup failed")
                .into(),
            BinaryPowerLevel::On.into_primitive(),
        );
        assert!(
            matches!(res_add_invalid_dep_level, Err(ModifyDependencyError::Invalid)),
            "{:?}",
            res_add_invalid_dep_level
        );

        // Add of invalid required level should fail.
        let res_add_invalid_req_level = broker.add_dependency(
            &adamantium,
            DependencyType::Active,
            BinaryPowerLevel::On.into_primitive(),
            token_active_vibranium
                .duplicate_handle(zx::Rights::SAME_RIGHTS)
                .expect("dup failed")
                .into(),
            INVALID_POWER_LEVEL,
        );
        assert!(
            matches!(res_add_invalid_req_level, Err(ModifyDependencyError::Invalid)),
            "{:?}",
            res_add_invalid_req_level
        );

        // Valid add of active type should succeed
        broker
            .add_dependency(
                &adamantium,
                DependencyType::Active,
                BinaryPowerLevel::On.into_primitive(),
                token_active_vibranium
                    .duplicate_handle(zx::Rights::SAME_RIGHTS)
                    .expect("dup failed")
                    .into(),
                BinaryPowerLevel::On.into_primitive(),
            )
            .expect("add_dependency failed");

        // Adding an overlapping dependency with a different type should fail
        let res_add_overlap = broker.add_dependency(
            &adamantium,
            DependencyType::Passive,
            BinaryPowerLevel::On.into_primitive(),
            token_passive_vibranium
                .duplicate_handle(zx::Rights::SAME_RIGHTS)
                .expect("dup failed")
                .into(),
            BinaryPowerLevel::On.into_primitive(),
        );
        assert!(
            matches!(res_add_overlap, Err(ModifyDependencyError::AlreadyExists)),
            "{res_add_overlap:?}"
        );

        // Removing should return NotAuthorized if token is not registered
        let unregistered_token = DependencyToken::create();
        let res_remove_not_authorized = broker.remove_dependency(
            &adamantium,
            DependencyType::Active,
            BinaryPowerLevel::On.into_primitive(),
            unregistered_token.into(),
            BinaryPowerLevel::On.into_primitive(),
        );
        assert!(matches!(res_remove_not_authorized, Err(ModifyDependencyError::NotAuthorized)));

        // Removing should return NotAuthorized if token is of wrong type
        let res_remove_wrong_type = broker.remove_dependency(
            &adamantium,
            DependencyType::Active,
            BinaryPowerLevel::On.into_primitive(),
            token_passive_vibranium
                .duplicate_handle(zx::Rights::SAME_RIGHTS)
                .expect("dup failed")
                .into(),
            BinaryPowerLevel::On.into_primitive(),
        );
        assert!(matches!(res_remove_wrong_type, Err(ModifyDependencyError::NotAuthorized)));

        // Valid remove of active type should succeed
        broker
            .remove_dependency(
                &adamantium,
                DependencyType::Active,
                BinaryPowerLevel::On.into_primitive(),
                token_active_vibranium
                    .duplicate_handle(zx::Rights::SAME_RIGHTS)
                    .expect("dup failed")
                    .into(),
                BinaryPowerLevel::On.into_primitive(),
            )
            .expect("remove_dependency failed");

        // Valid add of passive type should succeed
        broker
            .add_dependency(
                &adamantium,
                DependencyType::Passive,
                BinaryPowerLevel::On.into_primitive(),
                token_passive_vibranium
                    .duplicate_handle(zx::Rights::SAME_RIGHTS)
                    .expect("dup failed")
                    .into(),
                BinaryPowerLevel::On.into_primitive(),
            )
            .expect("add_dependency failed");

        // Valid remove of passive type should succeed
        broker
            .remove_dependency(
                &adamantium,
                DependencyType::Passive,
                BinaryPowerLevel::On.into_primitive(),
                token_passive_vibranium
                    .duplicate_handle(zx::Rights::SAME_RIGHTS)
                    .expect("dup failed")
                    .into(),
                BinaryPowerLevel::On.into_primitive(),
            )
            .expect("remove_dependency failed");
    }
}
