// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{anyhow, Error};
use fidl_fuchsia_power_broker::{
    self as fpb, DependencyType, LeaseStatus, Permissions, PowerLevel,
    RegisterDependencyTokenError, UnregisterDependencyTokenError,
};
use fuchsia_inspect::component;
use futures::channel::mpsc::{unbounded, UnboundedReceiver, UnboundedSender};
use itertools::Itertools;
use std::cmp::max;
use std::collections::HashMap;
use std::collections::HashSet;
use std::fmt::{self, Debug};
use std::hash::Hash;
use std::iter::repeat;
use uuid::Uuid;

use crate::credentials::*;
use crate::topology::*;

/// If true, use non-random IDs for ease of debugging.
const ID_DEBUG_MODE: bool = false;

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
        let insp_root = component::inspector().root();
        let insp_cur_levels = insp_root.create_child("current-levels");
        let insp_req_levels = insp_root.create_child("required-levels");
        Broker {
            catalog: Catalog::new(),
            credentials: Registry::new(),
            current: SubscribeMap::new(insp_cur_levels),
            required: SubscribeMap::new(insp_req_levels),
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

    pub fn update_current_level(&mut self, element_id: &ElementID, level: PowerLevel) {
        tracing::debug!("update_current_level({element_id}, {level:?})");
        let prev_level = self.current.update(element_id, level);
        if prev_level.as_ref() == Some(&level) {
            return;
        }
        if prev_level.is_none() || prev_level.unwrap() < level {
            // The level was increased, look for activated active or pending
            // passive claims that are newly satisfied by the new current level:
            let claims_for_required_element: Vec<Claim> = self
                .catalog
                .active_claims
                .activated
                .for_required_element(element_id)
                .into_iter()
                .chain(self.catalog.passive_claims.pending.for_required_element(element_id))
                .collect();
            tracing::debug!(
                "update_current_level({element_id}): claims_satisfied = {})",
                &claims_for_required_element.iter().join(", ")
            );
            // Find claims that are newly satisfied by level:
            let claims_satisfied: Vec<Claim> = claims_for_required_element
                .into_iter()
                .filter(|c| {
                    level.satisfies(c.requires().level) && !prev_level.satisfies(c.requires().level)
                })
                .collect();
            tracing::debug!(
                "update_current_level({element_id}): claims_satisfied = {})",
                &claims_satisfied.iter().join(", ")
            );
            // Find the set of dependents for all claims satisfied:
            let dependents_of_claims_satisfied: HashSet<ElementID> =
                claims_satisfied.iter().map(|c| c.dependent().element_id.clone()).collect();
            // Because at least one of the dependencies of the dependent was
            // satisfied, other previously pending active claims requiring the
            // dependent may now be ready to be activated (though they may not
            // if the dependent has other unsatisfied dependencies). Look for
            // all pending active claims on this dependent, and pass them to
            // activate_active_claims_if_dependencies_satisfied(), which will check
            // if all dependencies of the dependent are now satisfied, and if
            // so, activate the pending claims on dependent, raising its
            // required level:
            for dependent in dependents_of_claims_satisfied {
                let pending_active_claims_on_dependent =
                    self.catalog.active_claims.pending.for_required_element(&dependent);
                tracing::debug!(
                    "update_current_level({element_id}): pending_active_claims_on_dependent({dependent}) = {})",
                    &pending_active_claims_on_dependent.iter().join(", ")
                );
                self.activate_active_claims_if_dependencies_satisfied(
                    pending_active_claims_on_dependent,
                );
            }
            // For pending passive claims, if the current level satisfies them,
            // and their lease is no longer contingent, activate the claim.
            for claim in self.catalog.passive_claims.pending.for_required_element(element_id) {
                if !level.satisfies(claim.requires().level) {
                    continue;
                }
                if self.is_lease_contingent(&claim.lease_id) {
                    continue;
                }
                self.catalog.passive_claims.activate_claim(&claim.id)
            }
            // Update the status of all leases whose claims were satisfied.
            let leases_to_check_if_satisfied: HashSet<LeaseID> =
                claims_satisfied.into_iter().map(|c| c.lease_id).collect();
            tracing::debug!(
                "update_current_level({element_id}): leases_to_check_if_satisfied = {:?})",
                &leases_to_check_if_satisfied
            );
            for lease_id in leases_to_check_if_satisfied {
                self.update_lease_status(&lease_id);
            }
            return;
        }
        if prev_level.unwrap() > level {
            // If the level was lowered, find orphaned claims whose lease has
            // been dropped and see if any of these claims no longer have any
            // dependents and thus can be dropped.
            let orphaned_active_claims_for_element =
                self.catalog.active_claims.activated.orphaned_for_element(element_id);
            let active_claims_to_drop =
                self.find_claims_with_no_dependents(&orphaned_active_claims_for_element);
            let orphaned_passive_claims_for_element =
                self.catalog.passive_claims.activated.orphaned_for_element(element_id);
            let passive_claims_to_drop =
                self.find_claims_with_no_dependents(&orphaned_passive_claims_for_element);
            self.drop_active_claims(&active_claims_to_drop);
            self.drop_passive_claims(&passive_claims_to_drop);
        }
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
        tracing::debug!("acquire_lease({element_id}@{level})");
        let (lease, active_claims) = self.catalog.create_lease_and_claims(element_id, level);
        if self.is_lease_contingent(&lease.id) {
            // Lease is blocked on passive claims, update status and return.
            tracing::debug!(
                "acquire_lease({element_id}@{level}): {} is contingent on passive claims",
                &lease.id
            );
            self.update_lease_status(&lease.id);
            return Ok(lease);
        }
        // Activate all pending active claims that have all of their
        // dependencies satisfied.
        self.activate_active_claims_if_dependencies_satisfied(active_claims.clone());
        // For pending passive claims, if the current level satisfies them,
        // and their lease is no longer contingent, activate the claim.
        // (If the current level does not satisfy the claim, it will be
        // activated as part of update_current_level once it does.)
        for claim in self.catalog.passive_claims.pending.for_lease(&lease.id) {
            if self.current_level_satisfies(claim.requires()) {
                self.catalog.passive_claims.activate_claim(&claim.id)
            }
        }
        self.update_lease_status(&lease.id);
        // Other contingent leases may need to update their status if any new
        // active claims would satisfy their passive claims.
        for active_claim in active_claims {
            tracing::debug!("check if active claim {active_claim} would satisfy passive claims");
            let active_claim_requires = &active_claim.requires().clone();
            let passive_claims_for_req_element = self
                .catalog
                .passive_claims
                .pending
                .for_required_element(&active_claim_requires.element_id);
            tracing::debug!(
                "passive_claims_for_req_element[{}]",
                passive_claims_for_req_element.iter().join(", ")
            );
            let passive_claims_possibly_affected = passive_claims_for_req_element
                .iter()
                // Only consider claims for leases other than active_claim's
                .filter(|c| c.lease_id != lease.id)
                // Only consider passive claims that would be satisfied by active_claim
                .filter(|c| active_claim_requires.level.satisfies(c.requires().level));
            for passive_claim in passive_claims_possibly_affected {
                tracing::debug!(
                    "active claim {active_claim} may have changed status of lease {}",
                    &passive_claim.lease_id
                );
                if !self.is_lease_contingent(&passive_claim.lease_id) {
                    tracing::debug!(
                        "active claim {active_claim} changed status of lease {}",
                        &passive_claim.lease_id
                    );
                    // Activate any pending claims for this lease whose
                    // required elements have their dependencies satisfied.
                    let pending_claims =
                        self.catalog.active_claims.pending.for_lease(&passive_claim.lease_id);
                    self.activate_active_claims_if_dependencies_satisfied(pending_claims);
                    // Activate passive claims for this lease, if they are
                    // (already) currently satisfied.
                    for claim in
                        self.catalog.passive_claims.pending.for_lease(&passive_claim.lease_id)
                    {
                        if !self.current_level_satisfies(claim.requires()) {
                            continue;
                        }
                        self.catalog.passive_claims.activate_claim(&claim.id)
                    }
                    self.update_lease_status(&passive_claim.lease_id);
                }
            }
        }
        Ok(lease)
    }

    pub fn drop_lease(&mut self, lease_id: &LeaseID) -> Result<(), Error> {
        let (lease, active_claims, passive_claims) = self.catalog.drop(lease_id)?;
        let active_claims_dropped = self.find_claims_with_no_dependents(&active_claims);
        let passive_claims_dropped = self.find_claims_with_no_dependents(&passive_claims);
        self.drop_active_claims(&active_claims_dropped);
        self.drop_passive_claims(&passive_claims_dropped);
        self.catalog.lease_status.remove(lease_id);
        // Update the required level of the formerly leased element.
        self.update_required_levels(&vec![&lease.element_id]);
        Ok(())
    }

    /// Returns true if the lease has one or more passive claims with no active
    /// claims belonging to other leases that would satisfy them.
    /// (If a lease is contingent on any of its passive claims, none of its
    /// claims should be activated.)
    fn is_lease_contingent(&self, lease_id: &LeaseID) -> bool {
        let passive_claims = self
            .catalog
            .passive_claims
            .activated
            .for_lease(lease_id)
            .into_iter()
            .chain(self.catalog.passive_claims.pending.for_lease(lease_id).into_iter());
        for claim in passive_claims {
            // If there is no other lease with an active or pending claim that
            // would satisfy each passive claim, the lease is Contingent.
            let activated_claims = self
                .catalog
                .active_claims
                .activated
                .for_required_element(&claim.requires().element_id);
            let pending_claims = self
                .catalog
                .active_claims
                .pending
                .for_required_element(&claim.requires().element_id);
            let matching_active_claim_found = activated_claims
                .into_iter()
                .chain(pending_claims)
                .any(|c| c.requires().level.satisfies(claim.requires().level));
            if !matching_active_claim_found {
                return true;
            }
        }
        false
    }

    fn calculate_lease_status(&self, lease_id: &LeaseID) -> LeaseStatus {
        // If the lease has any Pending active claims, it is still Pending.
        if !self.catalog.active_claims.pending.for_lease(lease_id).is_empty() {
            return LeaseStatus::Pending;
        }

        // If the lease has any Pending passive claims, it is still Pending.
        if !self.catalog.passive_claims.pending.for_lease(lease_id).is_empty() {
            return LeaseStatus::Pending;
        }

        // If the lease has any passive claims that have not been satisfied
        // it is still Pending.
        let passive_claims = self.catalog.passive_claims.activated.for_lease(lease_id);
        for claim in passive_claims {
            if !self.current_level_satisfies(claim.requires()) {
                return LeaseStatus::Pending;
            }
        }

        // If the lease is contingent on any passive claims, it is Pending.
        // (The passive claims could have been previously satisfied, but then
        // an active claim was subsequently dropped).
        if self.is_lease_contingent(lease_id) {
            return LeaseStatus::Pending;
        }

        // If the lease has any active claims that have not been satisfied
        // it is still Pending.
        for claim in self.catalog.active_claims.activated.for_lease(lease_id) {
            if !self.current_level_satisfies(claim.requires()) {
                return LeaseStatus::Pending;
            }
        }
        // All claims are active and satisfied, so the lease is Satisfied.
        LeaseStatus::Satisfied
    }

    // Reevaluates the lease and updates the LeaseStatus.
    // Returns the new status if changed, None otherwise.
    pub fn update_lease_status(&mut self, lease_id: &LeaseID) -> Option<LeaseStatus> {
        let status = self.calculate_lease_status(lease_id);
        let prev_status = self.catalog.lease_status.update(lease_id, status);
        if prev_status.as_ref() == Some(&status) {
            // LeaseStatus was not changed.
            return None;
        };
        // The lease_status changed, update the required level of the leased
        // element.
        if let Some(lease) = self.catalog.leases.get(lease_id) {
            self.update_required_levels(&vec![&lease.element_id.clone()]);
        } else {
            tracing::warn!("update_lease_status: lease {lease_id} not found");
        }
        tracing::debug!("update_lease_status({lease_id}) to {status:?}");
        Some(status)
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

    fn update_required_levels(&mut self, element_ids: &Vec<&ElementID>) {
        for element_id in element_ids {
            let new_required_level = self.catalog.calculate_required_level(element_id);
            tracing::debug!("update required level({:?}, {:?})", element_id, new_required_level);
            self.required.update(element_id, new_required_level);
        }
    }

    /// Examines a Vec of pending active claims and activates each for which
    /// its lease is not contingent on its passive claims and either the
    /// required element is already at the required level (and thus the claim
    /// is already satisfied) or all of the dependencies of its required
    /// ElementLevel are met. Updates required levels of affected elements.
    /// For example, let us imagine elements A, B, C and D where A depends on B
    /// and B depends on C and D. In order to activate the A->B claim, all
    /// dependencies of B (i.e. B->C and B->D) must first be satisfied.
    fn activate_active_claims_if_dependencies_satisfied(
        &mut self,
        pending_active_claims: Vec<Claim>,
    ) {
        tracing::debug!(
            "activate_active_claims_if_dependencies_satisfied: pending_active_claims[{}]",
            pending_active_claims.iter().join(", ")
        );
        // Skip any claims whose leases are still contingent on passive claims.
        let contingent_lease_ids: HashSet<LeaseID> = pending_active_claims
            .iter()
            .filter(|c| self.is_lease_contingent(&c.lease_id))
            .map(|c| c.lease_id.clone())
            .collect();
        let claims_to_activate: Vec<Claim> = pending_active_claims
            .into_iter()
            .filter(|c| !contingent_lease_ids.contains(&c.lease_id))
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
            self.catalog.active_claims.activate_claim(&claim.id);
        }
        self.update_required_levels(&element_ids_required_by_claims(&claims_to_activate));
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

    /// Examines a Vec of claims and returns any that no longer have any
    /// other claims within their lease that require their dependent.
    fn find_claims_with_no_dependents(&mut self, claims: &Vec<Claim>) -> Vec<Claim> {
        tracing::debug!("find_claims_with_no_dependents: [{}]", claims.iter().join(", "));
        let mut claims_to_drop = Vec::new();

        for claim_to_check in claims {
            let mut has_dependents = false;
            // Only claims belonging to the same lease can be a dependent.
            for related_claim in
                self.catalog.active_claims.activated.for_lease(&claim_to_check.lease_id)
            {
                if related_claim.requires() == claim_to_check.dependent() {
                    tracing::debug!(
                        "won't drop {claim_to_check}, has active dependent {related_claim}"
                    );
                    has_dependents = true;
                    break;
                }
            }
            if has_dependents {
                continue;
            }
            for related_claim in
                self.catalog.passive_claims.activated.for_lease(&claim_to_check.lease_id)
            {
                if related_claim.requires() == claim_to_check.dependent() {
                    tracing::debug!(
                        "won't drop {claim_to_check}, has passive dependent {related_claim}"
                    );
                    has_dependents = true;
                    break;
                }
            }
            if has_dependents {
                continue;
            }
            tracing::debug!("will drop {claim_to_check}");
            claims_to_drop.push(claim_to_check.clone());
        }
        claims_to_drop
    }

    fn drop_active_claims(&mut self, claims: &Vec<Claim>) {
        for claim in claims {
            tracing::debug!("drop active claim: {claim}");
            self.catalog.drop_activated_active_claim(&claim.id);
        }
        let element_ids_affected = element_ids_required_by_claims(claims);
        self.update_required_levels(&element_ids_affected);
        // Update the status of all leases with passive claims no longer satisfied.
        let mut leases_affected = HashSet::new();
        for element_id in element_ids_affected {
            // Calculate the maximum level required by active claims only to
            // determine if there are still any active claims that would
            // satisfy these passive claims.
            let max_required_by_active = max_level_required_by_claims(
                &self.catalog.active_claims.activated.for_required_element(element_id),
            )
            .unwrap_or(self.catalog.minimum_level(element_id));
            for passive_claim in
                self.catalog.passive_claims.activated.for_required_element(element_id)
            {
                if !max_required_by_active.satisfies(passive_claim.requires().level) {
                    tracing::debug!("passive_claim {passive_claim} no longer satisfied, must reevaluate lease {}", passive_claim.lease_id);
                    leases_affected.insert(passive_claim.lease_id);
                }
            }
        }
        // These leases have passive claims that are no longer satisfied,
        // so update_lease_status will update them to pending since they are
        // now contingent.
        for lease_id in leases_affected {
            self.update_lease_status(&lease_id);
        }
    }

    fn drop_passive_claims(&mut self, claims: &Vec<Claim>) {
        for claim in claims {
            tracing::debug!("drop passive claim: {claim}");
            self.catalog.drop_activated_passive_claim(&claim.id);
        }
        self.update_required_levels(&element_ids_required_by_claims(claims));
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
        let minimum_level = self.catalog.topology.minimum_level(&id);
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
        let id = if ID_DEBUG_MODE {
            format!("{element_id}@{level}")
        } else {
            LeaseID::from(Uuid::new_v4().as_simple().to_string())
        };
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

impl fmt::Display for Claim {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Claim{{{:.6}:{:.6}: {}}}", self.lease_id, self.id, self.dependency)
    }
}

impl Claim {
    fn new(dependency: Dependency, lease_id: &LeaseID) -> Self {
        let mut id = ClaimID::from(Uuid::new_v4().as_simple().to_string());
        if ID_DEBUG_MODE {
            id = format!("{id:.6}");
        }
        Claim { id, dependency, lease_id: lease_id.clone() }
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

/// Returns the maximum level required by claims, or None if empty.
fn max_level_required_by_claims(claims: &Vec<Claim>) -> Option<PowerLevel> {
    claims.into_iter().map(|x| x.requires().level).max()
}

#[derive(Debug)]
struct Catalog {
    topology: Topology,
    leases: HashMap<LeaseID, Lease>,
    lease_status: SubscribeMap<LeaseID, LeaseStatus>,
    /// Active claims can be either Pending or Activated.
    /// Pending active claims do not yet affect the required levels of their
    /// required elements. Some dependencies of their required element are not
    /// satisfied.
    /// Activated active claims affect the required level of the claim's
    /// required element.
    /// Each active claim will start as Pending, and will be Activated once all
    /// dependencies of its required element are satisfied.
    active_claims: ClaimActivationTracker,
    /// Passive claims can also be either Pending or Activated.
    /// Pending passive claims do not affect the required levels of their
    /// required elements.
    /// Activated passive claims will keep the required level of their
    /// required elements from dropping until the lease holder has a chance
    /// to drop the lease and release its claims.
    /// Each passive claim will start as pending and will be activated when
    /// the current level of their required element satisfies their required
    /// level. In order for this to happen, an active claim from
    /// another lease must have been activated and then satisfied.
    passive_claims: ClaimActivationTracker,
}

impl Catalog {
    fn new() -> Self {
        Catalog {
            topology: Topology::new(),
            leases: HashMap::new(),
            lease_status: SubscribeMap::new(component::inspector().root().create_child("leases")),
            active_claims: ClaimActivationTracker::new(),
            passive_claims: ClaimActivationTracker::new(),
        }
    }

    fn minimum_level(&self, element_id: &ElementID) -> PowerLevel {
        self.topology.minimum_level(element_id)
    }

    /// Calculates the required level for each element, according to the
    /// Minimum Power Level Policy.
    /// The required level is equal to the maximum of all **activated** active
    /// or passive claims on the element, the maximum level of all satisfied
    /// leases on the element, or the element's minimum level if there are
    /// no activated claims or satisfied leases.
    fn calculate_required_level(&self, element_id: &ElementID) -> PowerLevel {
        let minimum_level = self.minimum_level(element_id);
        let mut activated_claims = self.active_claims.activated.for_required_element(element_id);
        activated_claims
            .extend(self.passive_claims.activated.for_required_element(element_id).into_iter());
        max(
            max_level_required_by_claims(&activated_claims).unwrap_or(minimum_level),
            self.calculate_level_required_by_leases(element_id).unwrap_or(minimum_level),
        )
    }

    /// Calculates the maximum level of all satisfied leases on the element.
    fn calculate_level_required_by_leases(&self, element_id: &ElementID) -> Option<PowerLevel> {
        self.satisfied_leases_for_element(element_id).iter().map(|l| l.level).max()
    }

    /// Returns all satisfied leases for an element.
    fn satisfied_leases_for_element(&self, element_id: &ElementID) -> Vec<Lease> {
        // TODO(336609941): Consider optimizing this.
        self.leases
            .values()
            .filter(|l| l.element_id == *element_id)
            .filter(|l| self.get_lease_status(&l.id) == Some(LeaseStatus::Satisfied))
            .cloned()
            .collect()
    }

    /// Creates a new lease for the given element and level along with all
    /// claims necessary to satisfy this lease and adds them to pending_claims.
    /// Returns the new lease and the Vec of active (pending) claims created.
    fn create_lease_and_claims(
        &mut self,
        element_id: &ElementID,
        level: PowerLevel,
    ) -> (Lease, Vec<Claim>) {
        tracing::debug!("create_lease_and_claims({element_id}@{level})");
        // TODO: Add lease validation and control.
        let lease = Lease::new(&element_id, level);
        self.leases.insert(lease.id.clone(), lease.clone());
        let mut active_claims = Vec::new();
        let element_level = ElementLevel { element_id: element_id.clone(), level: level.clone() };
        let (active_dependencies, passive_dependencies) =
            self.topology.all_active_and_passive_dependencies(&element_level);
        // Create pending active claims for each of the active dependencies.
        for dependency in active_dependencies {
            let active_claim = Claim::new(dependency, &lease.id);
            tracing::debug!("lease({element_id}@{level}): created active claim {active_claim}");
            self.active_claims.pending.add(active_claim.clone());
            active_claims.push(active_claim);
        }
        // Create pending passive claims for each of the passive dependencies.
        for dependency in passive_dependencies {
            let passive_claim = Claim::new(dependency, &lease.id);
            tracing::debug!("lease({element_id}@{level}): created passive claim {passive_claim}");
            self.passive_claims.pending.add(passive_claim.clone());
        }
        (lease, active_claims)
    }

    /// Drops an existing lease, and initiates process of releasing all
    /// associated claims.
    /// Returns the dropped lease, a Vec of active claims that have
    /// been orphaned, and a Vec of passive claims that have been
    /// orphaned.
    fn drop(&mut self, lease_id: &LeaseID) -> Result<(Lease, Vec<Claim>, Vec<Claim>), Error> {
        tracing::debug!("drop(lease:{lease_id})");
        let lease = self.leases.remove(lease_id).ok_or(anyhow!("{lease_id} not found"))?;
        tracing::debug!("dropping lease({:?})", &lease);
        // Pending claims should be dropped immediately.
        let pending_active_claims = self.active_claims.pending.for_lease(&lease.id);
        for claim in pending_active_claims {
            if let Some(removed) = self.active_claims.pending.remove(&claim.id) {
                tracing::debug!("removing pending claim: {:?}", &removed);
            } else {
                tracing::error!("cannot remove pending active claim: not found: {}", claim.id);
            }
        }
        let pending_passive_claims = self.passive_claims.pending.for_lease(&lease.id);
        for claim in pending_passive_claims {
            if let Some(removed) = self.passive_claims.pending.remove(&claim.id) {
                tracing::debug!("removing pending passive claim: {:?}", &removed);
            } else {
                tracing::error!("cannot remove pending passive claim: not found: {}", claim.id);
            }
        }
        // Active and Passive claims should be marked to drop in an orderly sequence.
        tracing::debug!("drop(lease:{lease_id}): marking activated active claims orphaned");
        let active_claims_orphaned = self.active_claims.activated.mark_orphaned(&lease.id);
        tracing::debug!("drop(lease:{lease_id}): marking activated passive claims orphaned");
        let passive_claims_orphaned = self.passive_claims.activated.mark_orphaned(&lease.id);
        Ok((lease, active_claims_orphaned, passive_claims_orphaned))
    }

    /// Drops an activated active claim, removing it from active_claims.
    fn drop_activated_active_claim(&mut self, claim_id: &ClaimID) {
        self.active_claims.activated.remove(&claim_id);
    }

    /// Drops an activated passive claim, removing it from passive_claims.
    fn drop_activated_passive_claim(&mut self, claim_id: &ClaimID) {
        self.passive_claims.activated.remove(&claim_id);
    }

    pub fn get_lease_status(&self, lease_id: &LeaseID) -> Option<LeaseStatus> {
        self.lease_status.get(lease_id)
    }

    fn watch_lease_status(&mut self, lease_id: &LeaseID) -> UnboundedReceiver<Option<LeaseStatus>> {
        self.lease_status.subscribe(lease_id)
    }
}

/// ClaimActivationTracker divides a set of claims into Pending and Activated
/// states, each of which can separately be accessed as a ClaimLookup.
/// Pending claims have not yet taken effect because of some prerequisite.
/// Activated claims are in effect.
/// For more details on how Pending and Activated are used, see the docs on
/// Catalog above.
#[derive(Debug)]
struct ClaimActivationTracker {
    pending: ClaimLookup,
    activated: ClaimLookup,
}

impl ClaimActivationTracker {
    fn new() -> Self {
        Self { pending: ClaimLookup::new(), activated: ClaimLookup::new() }
    }

    /// Activates a pending claim, moving it to activated.
    fn activate_claim(&mut self, claim_id: &ClaimID) {
        tracing::debug!("activate_claim: {claim_id}");
        self.pending.move_to(&claim_id, &mut self.activated);
    }
}

#[derive(Debug)]
struct ClaimLookup {
    claims: HashMap<ClaimID, Claim>,
    claims_by_required_element_id: HashMap<ElementID, Vec<ClaimID>>,
    claims_by_lease: HashMap<LeaseID, Vec<ClaimID>>,
    orphaned_claims_by_element_id: HashMap<ElementID, Vec<ClaimID>>,
}

impl ClaimLookup {
    fn new() -> Self {
        Self {
            claims: HashMap::new(),
            claims_by_required_element_id: HashMap::new(),
            claims_by_lease: HashMap::new(),
            orphaned_claims_by_element_id: HashMap::new(),
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
            self.orphaned_claims_by_element_id.get_mut(&claim.dependent().element_id)
        {
            claim_ids.retain(|x| x != id);
            if claim_ids.is_empty() {
                self.orphaned_claims_by_element_id.remove(&claim.dependent().element_id);
            }
        }
        Some(claim)
    }

    /// Marks all claims associated with a dropped lease as orphaned. They will
    /// be removed in an orderly sequence (each claim will be removed only once
    /// all claims dependent on it have already been dropped).
    /// Returns a Vec of Claims marked to drop.
    fn mark_orphaned(&mut self, lease_id: &LeaseID) -> Vec<Claim> {
        let claims_marked = self.for_lease(lease_id);
        tracing::debug!(
            "marking claims orphaned for lease {lease_id}: [{}]",
            &claims_marked.iter().join(", ")
        );
        for claim in &claims_marked {
            self.orphaned_claims_by_element_id
                .entry(claim.dependent().element_id.clone())
                .or_insert(Vec::new())
                .push(claim.id.clone());
        }
        claims_marked
    }

    /// Removes claim from this lookup, and adds it to recipient.
    fn move_to(&mut self, id: &ClaimID, recipient: &mut ClaimLookup) {
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

    /// Claims with element_id as a dependent that belong to leases which have
    /// been dropped. See ClaimLookup::mark_orphaned for more details.
    fn orphaned_for_element(&self, element_id: &ElementID) -> Vec<Claim> {
        let Some(claim_ids) = self.orphaned_claims_by_element_id.get(element_id) else {
            return Vec::new();
        };
        self.for_claim_ids(claim_ids)
    }
}

trait Inspectable {
    type Value;
    fn track_inspect_with(
        &self,
        value: Self::Value,
        parent: &fuchsia_inspect::Node,
    ) -> Box<dyn fuchsia_inspect::InspectType>;
}

impl Inspectable for &ElementID {
    type Value = PowerLevel;
    fn track_inspect_with(
        &self,
        value: Self::Value,
        parent: &fuchsia_inspect::Node,
    ) -> Box<dyn fuchsia_inspect::InspectType> {
        Box::new(parent.create_uint(self.to_string(), value.into()))
    }
}

impl Inspectable for &LeaseID {
    type Value = LeaseStatus;
    fn track_inspect_with(
        &self,
        value: Self::Value,
        parent: &fuchsia_inspect::Node,
    ) -> Box<dyn fuchsia_inspect::InspectType> {
        Box::new(parent.create_string(*self, format!("{:?}", value)))
    }
}

#[derive(Debug)]
struct Data<V: Clone + PartialEq> {
    value: Option<V>,
    senders: Vec<UnboundedSender<Option<V>>>,
    _inspect: Option<Box<dyn fuchsia_inspect::InspectType>>,
}

impl<V: Clone + PartialEq> Default for Data<V> {
    fn default() -> Self {
        Data { value: None, senders: Vec::new(), _inspect: None }
    }
}

/// SubscribeMap is a wrapper around a HashMap that stores values V by key K
/// and allows subscribers to register a channel on which they will receive
/// updates whenever the value stored changes.
#[derive(Debug)]
struct SubscribeMap<K: Clone + Hash + Eq, V: Clone + PartialEq> {
    values: HashMap<K, Data<V>>,
    inspect: fuchsia_inspect::Node,
}

impl<K: Clone + Hash + Eq, V: Clone + PartialEq> SubscribeMap<K, V> {
    fn new(inspect: fuchsia_inspect::Node) -> Self {
        SubscribeMap { values: HashMap::new(), inspect }
    }

    fn get(&self, key: &K) -> Option<V> {
        self.values.get(key).map(|d| d.value.clone()).flatten()
    }

    // update updates the value for key.
    // Returns previous value, if any.
    fn update<'a>(&mut self, key: &'a K, value: V) -> Option<V>
    where
        &'a K: Inspectable<Value = V>,
        V: Copy,
    {
        let previous = self.get(key);
        // If the value hasn't changed, this is a no-op, return.
        if previous.as_ref() == Some(&value) {
            return previous;
        }
        let mut senders = Vec::new();
        if let Some(Data { value: _, senders: old_senders, _inspect: _ }) = self.values.remove(&key)
        {
            // Prune invalid senders.
            for sender in old_senders {
                if let Err(err) = sender.unbounded_send(Some(value.clone())) {
                    if err.is_disconnected() {
                        continue;
                    }
                }
                senders.push(sender);
            }
        }
        let _inspect = Some(key.track_inspect_with(value, &self.inspect));
        let value = Some(value);
        self.values.insert(key.clone(), Data { value, senders, _inspect });
        previous
    }

    fn subscribe(&mut self, key: &K) -> UnboundedReceiver<Option<V>> {
        let (sender, receiver) = unbounded::<Option<V>>();
        sender.unbounded_send(self.get(key)).expect("initial send should not fail");
        self.values.entry(key.clone()).or_default().senders.push(sender);
        receiver
    }

    fn remove(&mut self, key: &K) {
        self.values.remove(key);
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

impl SatisfyPowerLevel for Option<PowerLevel> {
    fn satisfies(&self, required: PowerLevel) -> bool {
        self.is_some() && self.unwrap().satisfies(required)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use diagnostics_assertions::assert_data_tree;
    use fidl_fuchsia_power_broker::{BinaryPowerLevel, DependencyToken};
    use fuchsia_zircon::{self as zx, HandleBased};
    use power_broker_client::BINARY_POWER_LEVELS;

    fn assert_lease_cleaned_up(catalog: &Catalog, lease_id: &LeaseID) {
        assert!(!catalog.leases.contains_key(lease_id));
        assert!(catalog.lease_status.get(lease_id).is_none());
        assert!(catalog.active_claims.activated.for_lease(lease_id).is_empty());
        assert!(catalog.active_claims.pending.for_lease(lease_id).is_empty());
        assert!(catalog.passive_claims.activated.for_lease(lease_id).is_empty());
        assert!(catalog.passive_claims.pending.for_lease(lease_id).is_empty());
    }

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
    fn test_option_satisfy_power_level() {
        for (level, required, want) in [
            (None, 0, false),
            (None, 1, false),
            (Some(0), 1, false),
            (Some(0), 0, true),
            (Some(1), 0, true),
            (Some(1), 1, true),
            (Some(255), 0, true),
            (Some(255), 1, true),
            (Some(255), 255, true),
            (Some(1), 255, false),
            (Some(35), 36, false),
            (Some(35), 35, true),
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
        let inspect = fuchsia_inspect::component::inspector();
        let mut levels =
            SubscribeMap::<ElementID, PowerLevel>::new(inspect.root().create_child("test"));

        levels.update(&"A".into(), BinaryPowerLevel::On.into_primitive());
        assert_eq!(levels.get(&"A".into()), Some(BinaryPowerLevel::On.into_primitive()));
        assert_eq!(levels.get(&"B".into()), None);
        assert_data_tree!(inspect, root: { test:
        {
          "A": 1u64,
        }});

        levels.update(&"A".into(), BinaryPowerLevel::Off.into_primitive());
        levels.update(&"B".into(), BinaryPowerLevel::On.into_primitive());
        assert_eq!(levels.get(&"A".into()), Some(BinaryPowerLevel::Off.into_primitive()));
        assert_eq!(levels.get(&"B".into()), Some(BinaryPowerLevel::On.into_primitive()));
        assert_data_tree!(inspect, root: { test:
        {
          "A": 0u64,
          "B": 1u64,
        }});

        levels.update(&"UD1".into(), 145);
        assert_eq!(levels.get(&"UD1".into()), Some(145));
        assert_eq!(levels.get(&"UD2".into()), None);

        levels.update(&"A".into(), BinaryPowerLevel::On.into_primitive());
        levels.remove(&"B".into());
        assert_eq!(levels.get(&"B".into()), None);

        assert_data_tree!(inspect, root: { test:
        {
          "A": 1u64,
          "UD1": 145u64,
        }});
    }

    #[fuchsia::test]
    fn test_levels_subscribe() {
        let inspect = fuchsia_inspect::component::inspector();
        let mut levels =
            SubscribeMap::<ElementID, PowerLevel>::new(inspect.root().create_child("test"));

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

        assert_data_tree!(inspect, root: { test:
        {
          "A": 0u64,
          "B": 1u64,
        }});
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
        // Create a topology of a child element with two direct active dependencies.
        // P1 <= C => P2
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
            broker.get_required_level(&parent1).unwrap(),
            BinaryPowerLevel::Off.into_primitive(),
            "Parent 1 should start with required level OFF"
        );
        assert_eq!(
            broker.get_required_level(&parent2).unwrap(),
            BinaryPowerLevel::Off.into_primitive(),
            "Parent 2 should start with required level OFF"
        );
        assert_eq!(
            broker.get_required_level(&child).unwrap(),
            BinaryPowerLevel::Off.into_primitive(),
            "Child should start with required level OFF"
        );

        // Acquire the lease, which should result in two claims, one
        // for each dependency.
        // The lease should be Pending.
        let lease = broker
            .acquire_lease(&child, BinaryPowerLevel::On.into_primitive())
            .expect("acquire failed");
        assert_eq!(
            broker.get_required_level(&parent1).unwrap(),
            BinaryPowerLevel::On.into_primitive(),
            "Parent 1's required level should become ON from direct claim"
        );
        assert_eq!(
            broker.get_required_level(&parent2).unwrap(),
            BinaryPowerLevel::On.into_primitive(),
            "Parent 2's required level should become ON from direct claim"
        );
        assert_eq!(
            broker.get_required_level(&child).unwrap(),
            BinaryPowerLevel::Off.into_primitive(),
            "Child should have required level OFF until its dependencies are satisfied"
        );
        assert_eq!(broker.get_lease_status(&lease.id), Some(LeaseStatus::Pending));

        // Update P1's current level to ON.
        // The lease should still be Pending.
        broker.update_current_level(&parent1, BinaryPowerLevel::On.into_primitive());
        assert_eq!(
            broker.get_required_level(&parent1).unwrap(),
            BinaryPowerLevel::On.into_primitive(),
            "Parent 1's required level should remain ON"
        );
        assert_eq!(
            broker.get_required_level(&parent2).unwrap(),
            BinaryPowerLevel::On.into_primitive(),
            "Parent 2's required level should remain ON"
        );
        assert_eq!(
            broker.get_required_level(&child).unwrap(),
            BinaryPowerLevel::Off.into_primitive(),
            "Child should have still required level OFF -- P2 is not yet ON"
        );
        assert_eq!(broker.get_lease_status(&lease.id), Some(LeaseStatus::Pending));

        // Update P2's current level to ON.
        // The lease should now be Satisfied.
        broker.update_current_level(&parent2, BinaryPowerLevel::On.into_primitive());
        assert_eq!(
            broker.get_required_level(&parent1).unwrap(),
            BinaryPowerLevel::On.into_primitive(),
            "Parent 1's required level should remain ON"
        );
        assert_eq!(
            broker.get_required_level(&parent2).unwrap(),
            BinaryPowerLevel::On.into_primitive(),
            "Parent 2's required level should remain ON"
        );
        assert_eq!(
            broker.get_required_level(&child).unwrap(),
            BinaryPowerLevel::On.into_primitive(),
            "Child's required level should become ON"
        );
        assert_eq!(broker.get_lease_status(&lease.id), Some(LeaseStatus::Satisfied));

        // Update Child's current level to ON.
        broker.update_current_level(&child, BinaryPowerLevel::On.into_primitive());
        assert_eq!(
            broker.get_required_level(&parent1).unwrap(),
            BinaryPowerLevel::On.into_primitive(),
            "Parent 1's required level should remain ON"
        );
        assert_eq!(
            broker.get_required_level(&parent2).unwrap(),
            BinaryPowerLevel::On.into_primitive(),
            "Parent 2's required level should remain ON"
        );
        assert_eq!(
            broker.get_required_level(&child).unwrap(),
            BinaryPowerLevel::On.into_primitive(),
            "Child's required level should remain ON"
        );
        assert_eq!(broker.get_lease_status(&lease.id), Some(LeaseStatus::Satisfied));

        // Now drop the lease and verify both claims are also dropped.
        broker.drop_lease(&lease.id).expect("drop failed");
        assert_eq!(
            broker.get_required_level(&parent1).unwrap(),
            BinaryPowerLevel::Off.into_primitive(),
            "Parent 1's required level should become OFF from dropped claim"
        );
        assert_eq!(
            broker.get_required_level(&parent2).unwrap(),
            BinaryPowerLevel::Off.into_primitive(),
            "Parent 2's required level should become OFF from dropped claim"
        );
        assert_eq!(
            broker.get_required_level(&child).unwrap(),
            BinaryPowerLevel::Off.into_primitive(),
            "Child's required level should become OFF"
        );

        // Try dropping the lease one more time, which should result in an error.
        let extra_drop = broker.drop_lease(&lease.id);
        assert!(extra_drop.is_err());

        assert_lease_cleaned_up(&broker.catalog, &lease.id);
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
            broker.get_required_level(&parent).unwrap(),
            BinaryPowerLevel::Off.into_primitive(),
            "Parent should start with required level OFF"
        );
        assert_eq!(
            broker.get_required_level(&grandparent).unwrap(),
            BinaryPowerLevel::Off.into_primitive(),
            "Grandparent should start with required level OFF"
        );
        assert_eq!(
            broker.get_required_level(&child).unwrap(),
            BinaryPowerLevel::Off.into_primitive(),
            "Child should start with required level OFF"
        );

        // Acquire the lease, which should result in two claims, one
        // for the direct parent dependency, and one for the transitive
        // grandparent dependency.
        let lease = broker
            .acquire_lease(&child, BinaryPowerLevel::On.into_primitive())
            .expect("acquire failed");
        assert_eq!(
            broker.get_required_level(&parent).unwrap(),
            BinaryPowerLevel::Off.into_primitive(),
            "Parent's required level should become OFF, waiting on Grandparent to turn ON"
        );
        assert_eq!(
            broker.get_required_level(&grandparent).unwrap(),
            BinaryPowerLevel::On.into_primitive(),
            "Grandparent's required level should become ON because it has no dependencies"
        );
        assert_eq!(
            broker.get_required_level(&child).unwrap(),
            BinaryPowerLevel::Off.into_primitive(),
            "Child should have required level OFF"
        );
        assert_eq!(broker.get_lease_status(&lease.id), Some(LeaseStatus::Pending));

        // Raise Grandparent power level to ON, now Parent claim should be active.
        broker.update_current_level(&grandparent, BinaryPowerLevel::On.into_primitive());
        assert_eq!(
            broker.get_required_level(&parent).unwrap(),
            BinaryPowerLevel::On.into_primitive(),
            "Parent's required level should become ON"
        );
        assert_eq!(
            broker.get_required_level(&grandparent).unwrap(),
            BinaryPowerLevel::On.into_primitive(),
            "Grandparent's required level should become ON"
        );
        assert_eq!(
            broker.get_required_level(&child).unwrap(),
            BinaryPowerLevel::Off.into_primitive(),
            "Child's required level should remain OFF"
        );
        assert_eq!(broker.get_lease_status(&lease.id), Some(LeaseStatus::Pending));

        // Raise Parent power level to ON, now lease should be Satisfied.
        broker.update_current_level(&parent, BinaryPowerLevel::On.into_primitive());
        assert_eq!(
            broker.get_required_level(&parent).unwrap(),
            BinaryPowerLevel::On.into_primitive(),
            "Parent's required level should become ON"
        );
        assert_eq!(
            broker.get_required_level(&grandparent).unwrap(),
            BinaryPowerLevel::On.into_primitive(),
            "Grandparent's required level should become ON"
        );
        assert_eq!(
            broker.get_required_level(&child).unwrap(),
            BinaryPowerLevel::On.into_primitive(),
            "Child's required level should become ON"
        );
        assert_eq!(broker.get_lease_status(&lease.id), Some(LeaseStatus::Satisfied));

        // Raise Child power level to ON.
        // All required levels should still be ON.
        // Lease should still be Satisfied.
        broker.update_current_level(&parent, BinaryPowerLevel::On.into_primitive());
        assert_eq!(
            broker.get_required_level(&parent).unwrap(),
            BinaryPowerLevel::On.into_primitive(),
            "Parent's required level should become ON"
        );
        assert_eq!(
            broker.get_required_level(&grandparent).unwrap(),
            BinaryPowerLevel::On.into_primitive(),
            "Grandparent's required level should become ON"
        );
        assert_eq!(
            broker.get_required_level(&child).unwrap(),
            BinaryPowerLevel::On.into_primitive(),
            "Child's required level should become ON"
        );
        assert_eq!(broker.get_lease_status(&lease.id), Some(LeaseStatus::Satisfied));

        // Now drop the lease and verify Parent claim is dropped, but
        // Grandparent claim is not yet dropped.
        broker.drop_lease(&lease.id).expect("drop failed");
        assert_eq!(
            broker.get_required_level(&parent).unwrap(),
            BinaryPowerLevel::Off.into_primitive(),
            "Parent's required level should become OFF after lease drop"
        );
        assert_eq!(
            broker.get_required_level(&grandparent).unwrap(),
            BinaryPowerLevel::On.into_primitive(),
            "Grandparent's required level should remain ON"
        );
        assert_eq!(
            broker.get_required_level(&child).unwrap(),
            BinaryPowerLevel::Off.into_primitive(),
            "Child's required level should become OFF"
        );

        // Lower Parent power level to OFF, now Grandparent claim should be
        // dropped and should have required level OFF.
        broker.update_current_level(&parent, BinaryPowerLevel::Off.into_primitive());
        assert_eq!(
            broker.get_required_level(&parent).unwrap(),
            BinaryPowerLevel::Off.into_primitive(),
            "Parent should have required level OFF"
        );
        assert_eq!(
            broker.get_required_level(&grandparent).unwrap(),
            BinaryPowerLevel::Off.into_primitive(),
            "Grandparent's required level should become OFF"
        );
        assert_eq!(
            broker.get_required_level(&child).unwrap(),
            BinaryPowerLevel::Off.into_primitive(),
            "Child's required level should remain OFF"
        );

        assert_lease_cleaned_up(&broker.catalog, &lease.id);
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
            broker.get_required_level(&parent).unwrap(),
            0,
            "Parent should start with required level 0"
        );
        assert_eq!(
            broker.get_required_level(&grandparent).unwrap(),
            10,
            "Grandparent should start with required level at its default of 10"
        );
        assert_eq!(
            broker.get_required_level(&child1).unwrap(),
            0,
            "Child 1 should start with required level 0"
        );
        assert_eq!(
            broker.get_required_level(&child2).unwrap(),
            0,
            "Child 2 should start with required level 0"
        );

        // Acquire lease for Child 1. Initially, Grandparent should have
        // required level 200 and Parent should have required level 0
        // because Child 1 has a dependency on Parent and Parent has a
        // dependency on Grandparent. Grandparent has no dependencies so its
        // level should be raised first.
        let lease1 = broker.acquire_lease(&child1, 5).expect("acquire failed");
        assert_eq!(
            broker.get_required_level(&parent).unwrap(),
            0,
            "Parent's required level should become 0, waiting on Grandparent to reach required level"
        );
        assert_eq!(
            broker.get_required_level(&grandparent).unwrap(),
            200,
            "Grandparent's required level should become 100 because of parent dependency and it has no dependencies of its own"
        );
        assert_eq!(
            broker.get_required_level(&child1).unwrap(),
            0,
            "Child 1's required level should remain 0"
        );
        assert_eq!(
            broker.get_required_level(&child2).unwrap(),
            0,
            "Child 2's required level should remain 0"
        );
        assert_eq!(broker.get_lease_status(&lease1.id), Some(LeaseStatus::Pending));

        // Raise Grandparent's current level to 200. Now Parent claim should
        // be active, because its dependency on Grandparent is unblocked
        // raising its required level to 50.
        broker.update_current_level(&grandparent, 200);
        assert_eq!(
            broker.get_required_level(&parent).unwrap(),
            50,
            "Parent's required level should become 50"
        );
        assert_eq!(
            broker.get_required_level(&grandparent).unwrap(),
            200,
            "Grandparent's required level should remain 200"
        );
        assert_eq!(
            broker.get_required_level(&child1).unwrap(),
            0,
            "Child 1's required level should remain 0"
        );
        assert_eq!(
            broker.get_required_level(&child2).unwrap(),
            0,
            "Child 2's required level should remain 0"
        );
        assert_eq!(broker.get_lease_status(&lease1.id), Some(LeaseStatus::Pending));

        // Update Parent's current level to 50.
        // Parent and Grandparent should have required levels of 50 and 200.
        broker.update_current_level(&parent, 50);
        assert_eq!(
            broker.get_required_level(&parent).unwrap(),
            50,
            "Parent's required level should become 50"
        );
        assert_eq!(
            broker.get_required_level(&grandparent).unwrap(),
            200,
            "Grandparent's required level should remain 200"
        );
        assert_eq!(
            broker.get_required_level(&child1).unwrap(),
            5,
            "Child 1's required level should become 5"
        );
        assert_eq!(
            broker.get_required_level(&child2).unwrap(),
            0,
            "Child 2's required level should remain 0"
        );
        assert_eq!(broker.get_lease_status(&lease1.id), Some(LeaseStatus::Satisfied));

        // Acquire a lease for Child 2. Though Child 2 has nominal
        // requirements of Parent at 30 and Grandparent at 100, they are
        // superseded by Child 1's requirements of 50 and 200.
        let lease2 = broker.acquire_lease(&child2, 3).expect("acquire failed");
        assert_eq!(
            broker.get_required_level(&parent).unwrap(),
            50,
            "Parent's required level should remain 50"
        );
        assert_eq!(
            broker.get_required_level(&grandparent).unwrap(),
            200,
            "Grandparent's required level should remain 200"
        );
        assert_eq!(
            broker.get_required_level(&child1).unwrap(),
            5,
            "Child 1's required level should remain 5"
        );
        assert_eq!(
            broker.get_required_level(&child2).unwrap(),
            3,
            "Child 2's required level should become 3"
        );
        assert_eq!(broker.get_lease_status(&lease1.id), Some(LeaseStatus::Satisfied));
        assert_eq!(broker.get_lease_status(&lease2.id), Some(LeaseStatus::Satisfied));

        // Drop lease for Child 1. Parent's required level should immediately
        // drop to 30. Grandparent's required level will remain at 200 for now.
        broker.drop_lease(&lease1.id).expect("drop failed");
        assert_eq!(
            broker.get_required_level(&parent).unwrap(),
            30,
            "Parent's required level should drop to 30"
        );
        assert_eq!(
            broker.get_required_level(&grandparent).unwrap(),
            200,
            "Grandparent's required level should remain 200"
        );
        assert_eq!(
            broker.get_required_level(&child1).unwrap(),
            0,
            "Child 1's required level should become 0"
        );
        assert_eq!(
            broker.get_required_level(&child2).unwrap(),
            3,
            "Child 2's required level should remain 3"
        );
        assert_eq!(broker.get_lease_status(&lease2.id), Some(LeaseStatus::Satisfied));

        // Lower Parent's current level to 30. Now Grandparent's required level
        // should drop to 90.
        broker.update_current_level(&parent, 30);
        assert_eq!(
            broker.get_required_level(&parent).unwrap(),
            30,
            "Parent should have required level 30"
        );
        assert_eq!(
            broker.get_required_level(&grandparent).unwrap(),
            90,
            "Grandparent's required level should become 90"
        );
        assert_eq!(
            broker.get_required_level(&child1).unwrap(),
            0,
            "Child 1's required level should become 0"
        );
        assert_eq!(
            broker.get_required_level(&child2).unwrap(),
            3,
            "Child 2's required level should remain 3"
        );
        assert_eq!(broker.get_lease_status(&lease2.id), Some(LeaseStatus::Satisfied));

        // Drop lease for Child 2, Parent should have required level 0.
        // Grandparent's required level should remain 90.
        broker.drop_lease(&lease2.id).expect("drop failed");
        assert_eq!(
            broker.get_required_level(&parent).unwrap(),
            0,
            "Parent's required level should become 0"
        );
        assert_eq!(
            broker.get_required_level(&grandparent).unwrap(),
            90,
            "Grandparent's required level should remain 90"
        );
        assert_eq!(
            broker.get_required_level(&child1).unwrap(),
            0,
            "Child 1's required level should become 0"
        );
        assert_eq!(
            broker.get_required_level(&child2).unwrap(),
            0,
            "Child 2's required level should become 0"
        );

        // Lower Parent's current level to 0. Grandparent claim should now be
        // dropped and have its default required level of 10.
        broker.update_current_level(&parent, 0);
        assert_eq!(
            broker.get_required_level(&parent).unwrap(),
            0,
            "Parent should have required level 0"
        );
        assert_eq!(
            broker.get_required_level(&grandparent).unwrap(),
            10,
            "Grandparent's required level should become 10"
        );
        assert_eq!(
            broker.get_required_level(&child1).unwrap(),
            0,
            "Child 1's required level should remain 0"
        );
        assert_eq!(
            broker.get_required_level(&child2).unwrap(),
            0,
            "Child 2's required level should remain 0"
        );

        assert_lease_cleaned_up(&broker.catalog, &lease1.id);
        assert_lease_cleaned_up(&broker.catalog, &lease2.id);
    }

    #[fuchsia::test]
    async fn test_lease_passive_direct() {
        // Tests that a lease with a passive claim is Contingent while there
        // are no other leases with active claims that would satisfy its
        // passive claim.
        //
        // B has an active dependency on A.
        // C has a passive dependency on A.
        //  A     B     C
        // ON <= ON
        // ON <------- ON
        let mut broker = Broker::new();
        let token_a_active = DependencyToken::create();
        let token_a_passive = DependencyToken::create();
        let element_a = broker
            .add_element(
                "A",
                BinaryPowerLevel::Off.into_primitive(),
                BINARY_POWER_LEVELS.to_vec(),
                vec![],
                vec![token_a_active
                    .duplicate_handle(zx::Rights::SAME_RIGHTS)
                    .expect("dup failed")
                    .into()],
                vec![token_a_passive
                    .duplicate_handle(zx::Rights::SAME_RIGHTS)
                    .expect("dup failed")
                    .into()],
            )
            .expect("add_element failed");
        let element_b = broker
            .add_element(
                "B",
                BinaryPowerLevel::Off.into_primitive(),
                BINARY_POWER_LEVELS.to_vec(),
                vec![fpb::LevelDependency {
                    dependency_type: DependencyType::Active,
                    dependent_level: BinaryPowerLevel::On.into_primitive(),
                    requires_token: token_a_active
                        .duplicate_handle(zx::Rights::SAME_RIGHTS)
                        .expect("dup failed")
                        .into(),
                    requires_level: BinaryPowerLevel::On.into_primitive(),
                }],
                vec![],
                vec![],
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
                    requires_token: token_a_passive
                        .duplicate_handle(zx::Rights::SAME_RIGHTS)
                        .expect("dup failed")
                        .into(),
                    requires_level: BinaryPowerLevel::On.into_primitive(),
                }],
                vec![],
                vec![],
            )
            .expect("add_element failed");

        // Initial required level for A, B & C should be OFF.
        // Set A, B & C's current level to OFF.
        assert_eq!(
            broker.get_required_level(&element_a),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_b),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_c),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        broker.update_current_level(&element_a, BinaryPowerLevel::Off.into_primitive());
        broker.update_current_level(&element_b, BinaryPowerLevel::Off.into_primitive());
        broker.update_current_level(&element_c, BinaryPowerLevel::Off.into_primitive());

        // Lease C.
        // A's required level should remain OFF because of C's passive claim.
        // B's required level should remain OFF.
        // C's required level should remain OFF because the lease is still Pending.
        // Lease C should be pending and contingent on its passive claim.
        let lease_c = broker
            .acquire_lease(&element_c, BinaryPowerLevel::On.into_primitive())
            .expect("acquire failed");
        let lease_c_id = lease_c.id.clone();
        assert_eq!(
            broker.get_required_level(&element_a),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_b),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_c),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(broker.get_lease_status(&lease_c.id), Some(LeaseStatus::Pending));
        assert_eq!(broker.is_lease_contingent(&lease_c.id), true);

        // Lease B.
        // A's required level should become ON because of B's active claim.
        // B's required level should remain OFF because the lease is still Pending.
        // C's required level should remain OFF because the lease is still Pending.
        // Lease B should be pending.
        // Lease C should remain pending but is no longer contingent because
        // B's active claim unblocks C's passive claim.
        let lease_b = broker
            .acquire_lease(&element_b, BinaryPowerLevel::On.into_primitive())
            .expect("acquire failed");
        let lease_b_id = lease_b.id.clone();
        assert_eq!(
            broker.get_required_level(&element_a),
            Some(BinaryPowerLevel::On.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_b),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_c),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(broker.get_lease_status(&lease_b.id), Some(LeaseStatus::Pending));
        assert_eq!(broker.get_lease_status(&lease_c.id), Some(LeaseStatus::Pending));
        assert_eq!(broker.is_lease_contingent(&lease_b.id), false);
        assert_eq!(broker.is_lease_contingent(&lease_c.id), false);

        // Update A's current level to ON.
        // A's required level should remain ON.
        // B's required level should become ON because the lease is Satisfied.
        // C's required level should become ON because the lease is Satisfied.
        // Lease B & C should become satisfied.
        broker.update_current_level(&element_a, BinaryPowerLevel::On.into_primitive());
        assert_eq!(
            broker.get_required_level(&element_a),
            Some(BinaryPowerLevel::On.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_b),
            Some(BinaryPowerLevel::On.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_c),
            Some(BinaryPowerLevel::On.into_primitive())
        );
        assert_eq!(broker.get_lease_status(&lease_b.id), Some(LeaseStatus::Satisfied));
        assert_eq!(broker.get_lease_status(&lease_c.id), Some(LeaseStatus::Satisfied));
        assert_eq!(broker.is_lease_contingent(&lease_b.id), false);
        assert_eq!(broker.is_lease_contingent(&lease_c.id), false);

        // Drop Lease on B.
        // A's required level should remain ON.
        // B's required level should become OFF because the lease was dropped.
        // C's required level should become OFF because the lease is now Pending.
        // Lease C should now be pending and contingent.
        broker.drop_lease(&lease_b.id).expect("drop_lease failed");
        assert_eq!(
            broker.get_required_level(&element_a),
            Some(BinaryPowerLevel::On.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_b),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_c),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(broker.get_lease_status(&lease_c.id), Some(LeaseStatus::Pending));
        assert_eq!(broker.is_lease_contingent(&lease_c.id), true);

        // Drop Lease on C.
        // A's required level should become OFF.
        // B's required level should remain OFF.
        // C's required level should remain OFF.
        broker.drop_lease(&lease_c.id).expect("drop_lease failed");
        assert_eq!(
            broker.get_required_level(&element_a),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_b),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_c),
            Some(BinaryPowerLevel::Off.into_primitive())
        );

        // Update A's current level to OFF.
        // A, B & C's required levels should remain OFF.
        broker.update_current_level(&element_a, BinaryPowerLevel::Off.into_primitive());
        assert_eq!(
            broker.get_required_level(&element_a),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_b),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_c),
            Some(BinaryPowerLevel::Off.into_primitive())
        );

        // Leases C & B should be cleaned up.
        assert_lease_cleaned_up(&broker.catalog, &lease_b_id);
        assert_lease_cleaned_up(&broker.catalog, &lease_c_id);
    }

    #[fuchsia::test]
    async fn test_lease_passive_immediate() {
        // Tests that a lease with a passive claim is immediately satisfied if
        // there are already leases with active claims that would satisfy its
        // passive claim.
        //
        // B has an active dependency on A.
        // C has a passive dependency on A.
        //  A     B     C
        // ON <= ON
        // ON <------- ON
        let mut broker = Broker::new();
        let token_a_active = DependencyToken::create();
        let token_a_passive = DependencyToken::create();
        let element_a = broker
            .add_element(
                "A",
                BinaryPowerLevel::Off.into_primitive(),
                BINARY_POWER_LEVELS.to_vec(),
                vec![],
                vec![token_a_active
                    .duplicate_handle(zx::Rights::SAME_RIGHTS)
                    .expect("dup failed")
                    .into()],
                vec![token_a_passive
                    .duplicate_handle(zx::Rights::SAME_RIGHTS)
                    .expect("dup failed")
                    .into()],
            )
            .expect("add_element failed");
        let element_b = broker
            .add_element(
                "B",
                BinaryPowerLevel::Off.into_primitive(),
                BINARY_POWER_LEVELS.to_vec(),
                vec![fpb::LevelDependency {
                    dependency_type: DependencyType::Active,
                    dependent_level: BinaryPowerLevel::On.into_primitive(),
                    requires_token: token_a_active
                        .duplicate_handle(zx::Rights::SAME_RIGHTS)
                        .expect("dup failed")
                        .into(),
                    requires_level: BinaryPowerLevel::On.into_primitive(),
                }],
                vec![],
                vec![],
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
                    requires_token: token_a_passive
                        .duplicate_handle(zx::Rights::SAME_RIGHTS)
                        .expect("dup failed")
                        .into(),
                    requires_level: BinaryPowerLevel::On.into_primitive(),
                }],
                vec![],
                vec![],
            )
            .expect("add_element failed");

        // Initial required level for A, B & C should be OFF.
        // Set A, B & C's current level to OFF.
        assert_eq!(
            broker.get_required_level(&element_a),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_b),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_c),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        broker.update_current_level(&element_a, BinaryPowerLevel::Off.into_primitive());
        broker.update_current_level(&element_b, BinaryPowerLevel::Off.into_primitive());
        broker.update_current_level(&element_c, BinaryPowerLevel::Off.into_primitive());

        // Lease B.
        // A's required level should become ON because of B's active claim.
        // B's required level should be OFF because A is not yet on.
        // C's required level should remain OFF.
        // Lease B should be pending.
        let lease_b = broker
            .acquire_lease(&element_b, BinaryPowerLevel::On.into_primitive())
            .expect("acquire failed");
        let lease_b_id = lease_b.id.clone();
        assert_eq!(
            broker.get_required_level(&element_a),
            Some(BinaryPowerLevel::On.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_b),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_c),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(broker.get_lease_status(&lease_b.id), Some(LeaseStatus::Pending));

        // Update A's current level to ON.
        // A's required level should remain ON.
        // B's required level should become ON.
        // C's required level should remain OFF.
        // Lease B should become satisfied.
        broker.update_current_level(&element_a, BinaryPowerLevel::On.into_primitive());
        assert_eq!(
            broker.get_required_level(&element_a),
            Some(BinaryPowerLevel::On.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_b),
            Some(BinaryPowerLevel::On.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_c),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(broker.get_lease_status(&lease_b.id), Some(LeaseStatus::Satisfied));

        // Lease C.
        // A's required level should remain ON.
        // B's required level should remain ON.
        // C's required level should become ON because its dependencies are
        // already satisfied.
        // Lease B should still be satisfied.
        // Lease C should be immediately satisfied because B's active claim on
        // A satisfies C's passive claim.
        let lease_c = broker
            .acquire_lease(&element_c, BinaryPowerLevel::On.into_primitive())
            .expect("acquire failed");
        let lease_c_id = lease_c.id.clone();
        assert_eq!(
            broker.get_required_level(&element_a),
            Some(BinaryPowerLevel::On.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_b),
            Some(BinaryPowerLevel::On.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_c),
            Some(BinaryPowerLevel::On.into_primitive())
        );
        assert_eq!(broker.get_lease_status(&lease_b.id), Some(LeaseStatus::Satisfied));
        assert_eq!(broker.get_lease_status(&lease_c.id), Some(LeaseStatus::Satisfied));
        assert_eq!(broker.is_lease_contingent(&lease_c.id), false);

        // Drop Lease on B.
        // A's required level should remain ON.
        // B's required level should become OFF because it is no longer leased.
        // C's required level should become OFF because its lease is now
        // pending and contingent.
        // Lease C should now be pending and contingent.
        broker.drop_lease(&lease_b.id).expect("drop_lease failed");
        assert_eq!(
            broker.get_required_level(&element_a),
            Some(BinaryPowerLevel::On.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_b),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_c),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(broker.get_lease_status(&lease_c.id), Some(LeaseStatus::Pending));
        assert_eq!(broker.is_lease_contingent(&lease_c.id), true);

        // Drop Lease on C.
        // A's required level should become OFF.
        // B's required level should remain OFF.
        // C's required level should remain OFF.
        broker.drop_lease(&lease_c.id).expect("drop_lease failed");
        assert_eq!(
            broker.get_required_level(&element_a),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_b),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_c),
            Some(BinaryPowerLevel::Off.into_primitive())
        );

        // Update A's current level to OFF.
        // A's required level should remain OFF.
        // B's required level should remain OFF.
        // C's required level should remain OFF.
        broker.update_current_level(&element_a, BinaryPowerLevel::Off.into_primitive());
        assert_eq!(
            broker.get_required_level(&element_a),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_b),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_c),
            Some(BinaryPowerLevel::Off.into_primitive())
        );

        // Leases C & B should be cleaned up.
        assert_lease_cleaned_up(&broker.catalog, &lease_b_id);
        assert_lease_cleaned_up(&broker.catalog, &lease_c_id);
    }

    #[fuchsia::test]
    async fn test_lease_passive_partially_satisfied() {
        // Tests that a lease with two passive claims, one which is satisfied
        // initially by a second lease, and then the other that is satisfied
        // by a third lease, correctly becomes satisfied.
        //
        // B has an active dependency on A.
        // C has a passive dependency on A and E.
        // D has an active dependency on E.
        //  A     B     C     D     E
        // ON <= ON
        // ON <------- ON -------> ON
        //                   ON => ON
        let mut broker = Broker::new();
        let token_a_active = DependencyToken::create();
        let token_a_passive = DependencyToken::create();
        let element_a = broker
            .add_element(
                "A",
                BinaryPowerLevel::Off.into_primitive(),
                BINARY_POWER_LEVELS.to_vec(),
                vec![],
                vec![token_a_active
                    .duplicate_handle(zx::Rights::SAME_RIGHTS)
                    .expect("dup failed")
                    .into()],
                vec![token_a_passive
                    .duplicate_handle(zx::Rights::SAME_RIGHTS)
                    .expect("dup failed")
                    .into()],
            )
            .expect("add_element failed");
        let element_b = broker
            .add_element(
                "B",
                BinaryPowerLevel::Off.into_primitive(),
                BINARY_POWER_LEVELS.to_vec(),
                vec![fpb::LevelDependency {
                    dependency_type: DependencyType::Active,
                    dependent_level: BinaryPowerLevel::On.into_primitive(),
                    requires_token: token_a_active
                        .duplicate_handle(zx::Rights::SAME_RIGHTS)
                        .expect("dup failed")
                        .into(),
                    requires_level: BinaryPowerLevel::On.into_primitive(),
                }],
                vec![],
                vec![],
            )
            .expect("add_element failed");
        let token_e_active = DependencyToken::create();
        let token_e_passive = DependencyToken::create();
        let element_e = broker
            .add_element(
                "E",
                BinaryPowerLevel::Off.into_primitive(),
                BINARY_POWER_LEVELS.to_vec(),
                vec![],
                vec![token_e_active
                    .duplicate_handle(zx::Rights::SAME_RIGHTS)
                    .expect("dup failed")
                    .into()],
                vec![token_e_passive
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
                vec![
                    fpb::LevelDependency {
                        dependency_type: DependencyType::Passive,
                        dependent_level: BinaryPowerLevel::On.into_primitive(),
                        requires_token: token_a_passive
                            .duplicate_handle(zx::Rights::SAME_RIGHTS)
                            .expect("dup failed")
                            .into(),
                        requires_level: BinaryPowerLevel::On.into_primitive(),
                    },
                    fpb::LevelDependency {
                        dependency_type: DependencyType::Passive,
                        dependent_level: BinaryPowerLevel::On.into_primitive(),
                        requires_token: token_e_passive
                            .duplicate_handle(zx::Rights::SAME_RIGHTS)
                            .expect("dup failed")
                            .into(),
                        requires_level: BinaryPowerLevel::On.into_primitive(),
                    },
                ],
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
                    requires_token: token_e_active
                        .duplicate_handle(zx::Rights::SAME_RIGHTS)
                        .expect("dup failed")
                        .into(),
                    requires_level: BinaryPowerLevel::On.into_primitive(),
                }],
                vec![],
                vec![],
            )
            .expect("add_element failed");

        // Initial required level for all elements should be OFF.
        // Set all elements' current levels to OFF.
        assert_eq!(
            broker.get_required_level(&element_a),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_b),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_c),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_d),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_e),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        broker.update_current_level(&element_a, BinaryPowerLevel::Off.into_primitive());
        broker.update_current_level(&element_b, BinaryPowerLevel::Off.into_primitive());
        broker.update_current_level(&element_c, BinaryPowerLevel::Off.into_primitive());
        broker.update_current_level(&element_d, BinaryPowerLevel::Off.into_primitive());
        broker.update_current_level(&element_e, BinaryPowerLevel::Off.into_primitive());

        // Lease B.
        // A's required level should become ON because of B's active claim.
        // B, C, D & E's required levels should remain OFF.
        // Lease B should be pending.
        let lease_b = broker
            .acquire_lease(&element_b, BinaryPowerLevel::On.into_primitive())
            .expect("acquire failed");
        let lease_b_id = lease_b.id.clone();
        assert_eq!(
            broker.get_required_level(&element_a),
            Some(BinaryPowerLevel::On.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_b),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_c),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_d),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_e),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(broker.get_lease_status(&lease_b.id), Some(LeaseStatus::Pending));

        // Update A's current level to ON.
        // A's required level should remain ON.
        // B's required level should become ON.
        // C, D & E's required level should remain OFF.
        // Lease B should become satisfied.
        broker.update_current_level(&element_a, BinaryPowerLevel::On.into_primitive());
        assert_eq!(
            broker.get_required_level(&element_a),
            Some(BinaryPowerLevel::On.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_b),
            Some(BinaryPowerLevel::On.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_c),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_d),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_e),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(broker.get_lease_status(&lease_b.id), Some(LeaseStatus::Satisfied));

        // Lease C.
        // A & B's required levels should remain ON.
        // C's required level should remain OFF because its lease is pending and contingent.
        // D & E's required level should remain OFF.
        // Lease B should still be satisfied.
        // Lease C should be contingent because while B's active claim on
        // A satisfies C's passive claim on A, nothing satisfies C's passive
        // claim on E.
        let lease_c = broker
            .acquire_lease(&element_c, BinaryPowerLevel::On.into_primitive())
            .expect("acquire failed");
        let lease_c_id = lease_c.id.clone();
        assert_eq!(
            broker.get_required_level(&element_a),
            Some(BinaryPowerLevel::On.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_b),
            Some(BinaryPowerLevel::On.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_c),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_d),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_e),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(broker.get_lease_status(&lease_b.id), Some(LeaseStatus::Satisfied));
        assert_eq!(broker.get_lease_status(&lease_c.id), Some(LeaseStatus::Pending));
        assert_eq!(broker.is_lease_contingent(&lease_c.id), true);

        // Lease D.
        // A & B's required levels should remain ON.
        // C & D's required levels should remain OFF because their dependencies are not yet satisfied.
        // E's required level should become ON because of D's active claim.
        // Lease B should still be satisfied.
        // Lease C should be pending, but no longer contingent.
        // Lease D should be pending.
        let lease_d = broker
            .acquire_lease(&element_d, BinaryPowerLevel::On.into_primitive())
            .expect("acquire failed");
        let lease_d_id = lease_d.id.clone();
        assert_eq!(
            broker.get_required_level(&element_a),
            Some(BinaryPowerLevel::On.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_b),
            Some(BinaryPowerLevel::On.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_c),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_d),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_e),
            Some(BinaryPowerLevel::On.into_primitive())
        );
        assert_eq!(broker.get_lease_status(&lease_b.id), Some(LeaseStatus::Satisfied));
        assert_eq!(broker.get_lease_status(&lease_c.id), Some(LeaseStatus::Pending));
        assert_eq!(broker.is_lease_contingent(&lease_c.id), false);
        assert_eq!(broker.get_lease_status(&lease_d.id), Some(LeaseStatus::Pending));

        // Update E's current level to ON.
        // A & B's required level should remain ON.
        // C & D's required levels should become ON because their dependencies are now satisfied.
        // E's required level should remain ON.
        // Lease B should still be satisfied.
        // Lease C should become satisfied.
        // Lease D should become satisfied.
        broker.update_current_level(&element_e, BinaryPowerLevel::On.into_primitive());
        assert_eq!(
            broker.get_required_level(&element_a),
            Some(BinaryPowerLevel::On.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_b),
            Some(BinaryPowerLevel::On.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_c),
            Some(BinaryPowerLevel::On.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_d),
            Some(BinaryPowerLevel::On.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_e),
            Some(BinaryPowerLevel::On.into_primitive())
        );
        assert_eq!(broker.get_lease_status(&lease_b.id), Some(LeaseStatus::Satisfied));
        assert_eq!(broker.get_lease_status(&lease_c.id), Some(LeaseStatus::Satisfied));
        assert_eq!(broker.is_lease_contingent(&lease_c.id), false);
        assert_eq!(broker.get_lease_status(&lease_d.id), Some(LeaseStatus::Satisfied));

        // Drop Lease on B.
        // A's required level should remain ON because C has not yet dropped its lease.
        // B's required level should become OFF because it is no longer leased.
        // C's required level should become OFF because it is now pending and contingent.
        // D & E's required level should remain ON.
        // Lease C should now be pending and contingent.
        broker.drop_lease(&lease_b.id).expect("drop_lease failed");
        assert_eq!(
            broker.get_required_level(&element_a),
            Some(BinaryPowerLevel::On.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_b),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_c),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_d),
            Some(BinaryPowerLevel::On.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_e),
            Some(BinaryPowerLevel::On.into_primitive())
        );
        assert_eq!(broker.get_lease_status(&lease_c.id), Some(LeaseStatus::Pending));
        assert_eq!(broker.is_lease_contingent(&lease_c.id), true);

        // Drop Lease on D.
        // A's required level should remain ON because C has not yet dropped its lease.
        // B's required level should remain OFF.
        // C's required level should remain OFF.
        // D's required level should become OFF because it is no longer leased.
        // E's required level should remain ON because C has not yet dropped its lease.
        // Lease C should still be pending and contingent.
        broker.drop_lease(&lease_d.id).expect("drop_lease failed");
        assert_eq!(
            broker.get_required_level(&element_a),
            Some(BinaryPowerLevel::On.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_b),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_c),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_d),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_e),
            Some(BinaryPowerLevel::On.into_primitive())
        );
        assert_eq!(broker.get_lease_status(&lease_c.id), Some(LeaseStatus::Pending));
        assert_eq!(broker.is_lease_contingent(&lease_c.id), true);

        // Drop Lease on C.
        // A & E's required levels should become OFF.
        // B, C & D's required levels should remain OFF.
        broker.drop_lease(&lease_c.id).expect("drop_lease failed");
        assert_eq!(
            broker.get_required_level(&element_a),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_b),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_c),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_d),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_e),
            Some(BinaryPowerLevel::Off.into_primitive())
        );

        // Update A's current level to OFF.
        // All required levels should remain OFF.
        broker.update_current_level(&element_a, BinaryPowerLevel::Off.into_primitive());
        assert_eq!(
            broker.get_required_level(&element_a),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_b),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_c),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_d),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_e),
            Some(BinaryPowerLevel::Off.into_primitive())
        );

        // Update E's current level to OFF.
        // All required levels should remain OFF.
        broker.update_current_level(&element_e, BinaryPowerLevel::Off.into_primitive());
        assert_eq!(
            broker.get_required_level(&element_a),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_b),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_c),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_d),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_e),
            Some(BinaryPowerLevel::Off.into_primitive())
        );

        // Leases C & B should be cleaned up.
        assert_lease_cleaned_up(&broker.catalog, &lease_b_id);
        assert_lease_cleaned_up(&broker.catalog, &lease_c_id);
        assert_lease_cleaned_up(&broker.catalog, &lease_d_id);
    }

    #[fuchsia::test]
    async fn test_lease_passive_reacquire() {
        // Tests that a lease with a passive claim is dropped and reacquired
        // will not prevent power-down.
        //
        // B has an active dependency on A.
        // C has a passive dependency on A.
        //  A     B     C
        // ON <= ON
        // ON <------- ON
        let mut broker = Broker::new();
        let token_a_active = DependencyToken::create();
        let token_a_passive = DependencyToken::create();
        let element_a = broker
            .add_element(
                "A",
                BinaryPowerLevel::Off.into_primitive(),
                BINARY_POWER_LEVELS.to_vec(),
                vec![],
                vec![token_a_active
                    .duplicate_handle(zx::Rights::SAME_RIGHTS)
                    .expect("dup failed")
                    .into()],
                vec![token_a_passive
                    .duplicate_handle(zx::Rights::SAME_RIGHTS)
                    .expect("dup failed")
                    .into()],
            )
            .expect("add_element failed");
        let element_b = broker
            .add_element(
                "B",
                BinaryPowerLevel::Off.into_primitive(),
                BINARY_POWER_LEVELS.to_vec(),
                vec![fpb::LevelDependency {
                    dependency_type: DependencyType::Active,
                    dependent_level: BinaryPowerLevel::On.into_primitive(),
                    requires_token: token_a_active
                        .duplicate_handle(zx::Rights::SAME_RIGHTS)
                        .expect("dup failed")
                        .into(),
                    requires_level: BinaryPowerLevel::On.into_primitive(),
                }],
                vec![],
                vec![],
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
                    requires_token: token_a_passive
                        .duplicate_handle(zx::Rights::SAME_RIGHTS)
                        .expect("dup failed")
                        .into(),
                    requires_level: BinaryPowerLevel::On.into_primitive(),
                }],
                vec![],
                vec![],
            )
            .expect("add_element failed");

        // All initial required levels should be OFF.
        // Set all current levels to OFF.
        assert_eq!(
            broker.get_required_level(&element_a),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_b),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_c),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        broker.update_current_level(&element_a, BinaryPowerLevel::Off.into_primitive());
        broker.update_current_level(&element_b, BinaryPowerLevel::Off.into_primitive());
        broker.update_current_level(&element_c, BinaryPowerLevel::Off.into_primitive());

        // Lease C.
        // A's required level should remain OFF because of C's passive claim.
        // B's required level should remain OFF.
        // C's required level should remain OFF because its lease is pending and contingent.
        // Lease C should be pending and contingent on its passive claim.
        let lease_c = broker
            .acquire_lease(&element_c, BinaryPowerLevel::On.into_primitive())
            .expect("acquire failed");
        let lease_c_id = lease_c.id.clone();
        assert_eq!(
            broker.get_required_level(&element_a),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_b),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_c),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(broker.get_lease_status(&lease_c.id), Some(LeaseStatus::Pending));
        assert_eq!(broker.is_lease_contingent(&lease_c.id), true);

        // Lease B.
        // A's required level should become ON because of B's active claim.
        // B's required level should remain OFF because its dependency is not satisfied.
        // C's required level should remain OFF because its dependency is not satisfied.
        // Lease B should be pending.
        // Lease C should remain pending but no longer contingent because
        // B's active claim would satisfy C's passive claim.
        let lease_b = broker
            .acquire_lease(&element_b, BinaryPowerLevel::On.into_primitive())
            .expect("acquire failed");
        let lease_b_id = lease_b.id.clone();
        assert_eq!(
            broker.get_required_level(&element_a),
            Some(BinaryPowerLevel::On.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_b),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_c),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(broker.get_lease_status(&lease_b.id), Some(LeaseStatus::Pending));
        assert_eq!(broker.get_lease_status(&lease_c.id), Some(LeaseStatus::Pending));
        assert_eq!(broker.is_lease_contingent(&lease_b.id), false);
        assert_eq!(broker.is_lease_contingent(&lease_c.id), false);

        // Update A's current level to ON.
        // A's required level should remain ON.
        // B's required level should become ON because its dependency is satisfied.
        // C's required level should become ON because its dependency is satisfied.
        // Lease B & C should become satisfied.
        broker.update_current_level(&element_a, BinaryPowerLevel::On.into_primitive());
        assert_eq!(
            broker.get_required_level(&element_a),
            Some(BinaryPowerLevel::On.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_b),
            Some(BinaryPowerLevel::On.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_c),
            Some(BinaryPowerLevel::On.into_primitive())
        );
        assert_eq!(broker.get_lease_status(&lease_b.id), Some(LeaseStatus::Satisfied));
        assert_eq!(broker.get_lease_status(&lease_c.id), Some(LeaseStatus::Satisfied));
        assert_eq!(broker.is_lease_contingent(&lease_b.id), false);
        assert_eq!(broker.is_lease_contingent(&lease_c.id), false);

        // Drop Lease on B.
        // A's required level should remain ON.
        // B's required level should become OFF because it is no longer leased.
        // C's required level should become OFF because it now pending and contingent.
        // Lease C should now be Contingent.
        broker.drop_lease(&lease_b.id).expect("drop_lease failed");
        assert_eq!(
            broker.get_required_level(&element_a),
            Some(BinaryPowerLevel::On.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_b),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_c),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(broker.get_lease_status(&lease_c.id), Some(LeaseStatus::Pending));
        assert_eq!(broker.is_lease_contingent(&lease_c.id), true);

        // Drop Lease on C.
        // A's required level should become OFF.
        // B's required level should remain OFF.
        // C's required level should remain OFF because it is no longer leased.
        broker.drop_lease(&lease_c.id).expect("drop_lease failed");
        assert_eq!(
            broker.get_required_level(&element_a),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_b),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_c),
            Some(BinaryPowerLevel::Off.into_primitive())
        );

        // Reacquire Lease on C.
        // A's required level should remain OFF despite C's new passive claim.
        // B's required level should remain OFF.
        // C's required level should remain OFF because its lease is pending and contingent.
        // The lease on C should remain Pending and contingent even though A's current level is ON.
        let lease_c_reacquired = broker
            .acquire_lease(&element_c, BinaryPowerLevel::On.into_primitive())
            .expect("acquire failed");
        let lease_c_reacquired_id = lease_c_reacquired.id.clone();
        assert_eq!(
            broker.get_required_level(&element_a),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_b),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_c),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(broker.get_lease_status(&lease_c_reacquired.id), Some(LeaseStatus::Pending));
        assert_eq!(broker.is_lease_contingent(&lease_c_reacquired.id), true);

        // Update A's current level to OFF.
        // A, B & C's required levels should remain OFF.
        broker.update_current_level(&element_a, BinaryPowerLevel::Off.into_primitive());
        assert_eq!(
            broker.get_required_level(&element_a),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_b),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_c),
            Some(BinaryPowerLevel::Off.into_primitive())
        );

        // Drop reacquired lease on C.
        // A, B & C's required levels should remain OFF.
        broker.drop_lease(&lease_c_reacquired.id).expect("drop_lease failed");
        assert_eq!(
            broker.get_required_level(&element_a),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_b),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_c),
            Some(BinaryPowerLevel::Off.into_primitive())
        );

        // All leases should be cleaned up.
        assert_lease_cleaned_up(&broker.catalog, &lease_b_id);
        assert_lease_cleaned_up(&broker.catalog, &lease_c_id);
        assert_lease_cleaned_up(&broker.catalog, &lease_c_reacquired_id);
    }

    #[fuchsia::test]
    async fn test_lease_passive_contingent() {
        // Tests that a lease with a passive claim does not affect required
        // levels if it has not yet been satisfied.
        //
        // B has an active dependency on A @ 3.
        // C has a passive dependency on A @ 2.
        // D has an active dependency on A @ 1.
        //  A     B     C     D
        //  3 <== 1
        //  2 <-------- 1
        //  1 <============== 1
        let mut broker = Broker::new();
        let token_a_active = DependencyToken::create();
        let token_a_passive = DependencyToken::create();
        let element_a = broker
            .add_element(
                "A",
                0,
                vec![0, 1, 2, 3],
                vec![],
                vec![token_a_active
                    .duplicate_handle(zx::Rights::SAME_RIGHTS)
                    .expect("dup failed")
                    .into()],
                vec![token_a_passive
                    .duplicate_handle(zx::Rights::SAME_RIGHTS)
                    .expect("dup failed")
                    .into()],
            )
            .expect("add_element failed");
        let element_b = broker
            .add_element(
                "B",
                0,
                vec![0, 1],
                vec![fpb::LevelDependency {
                    dependency_type: DependencyType::Active,
                    dependent_level: 1,
                    requires_token: token_a_active
                        .duplicate_handle(zx::Rights::SAME_RIGHTS)
                        .expect("dup failed")
                        .into(),
                    requires_level: 3,
                }],
                vec![],
                vec![],
            )
            .expect("add_element failed");
        let element_c = broker
            .add_element(
                "C",
                0,
                vec![0, 1],
                vec![fpb::LevelDependency {
                    dependency_type: DependencyType::Passive,
                    dependent_level: 1,
                    requires_token: token_a_passive
                        .duplicate_handle(zx::Rights::SAME_RIGHTS)
                        .expect("dup failed")
                        .into(),
                    requires_level: 2,
                }],
                vec![],
                vec![],
            )
            .expect("add_element failed");
        let element_d = broker
            .add_element(
                "D",
                0,
                vec![0, 1],
                vec![fpb::LevelDependency {
                    dependency_type: DependencyType::Active,
                    dependent_level: 1,
                    requires_token: token_a_active
                        .duplicate_handle(zx::Rights::SAME_RIGHTS)
                        .expect("dup failed")
                        .into(),
                    requires_level: 1,
                }],
                vec![],
                vec![],
            )
            .expect("add_element failed");

        // Initial required level for all elements should be 0.
        // Set all current levels to 0.
        assert_eq!(broker.get_required_level(&element_a), Some(0));
        assert_eq!(broker.get_required_level(&element_b), Some(0));
        assert_eq!(broker.get_required_level(&element_c), Some(0));
        assert_eq!(broker.get_required_level(&element_d), Some(0));
        broker.update_current_level(&element_a, 0);
        broker.update_current_level(&element_b, 0);
        broker.update_current_level(&element_c, 0);
        broker.update_current_level(&element_d, 0);

        // Lease C.
        // A's required level should remain 0 despite C's passive claim.
        // B's required level should remain 0.
        // C's required level should remain 0 because its lease is pending and contingent.
        // D's required level should remain 0.
        // Lease C should be pending and contingent on its passive claim.
        let lease_c = broker.acquire_lease(&element_c, 1).expect("acquire failed");
        let lease_c_id = lease_c.id.clone();
        assert_eq!(broker.get_required_level(&element_a), Some(0));
        assert_eq!(broker.get_required_level(&element_b), Some(0));
        assert_eq!(broker.get_required_level(&element_c), Some(0));
        assert_eq!(broker.get_required_level(&element_d), Some(0));
        assert_eq!(broker.get_lease_status(&lease_c.id), Some(LeaseStatus::Pending));
        assert_eq!(broker.is_lease_contingent(&lease_c.id), true);

        // Lease B.
        // A's required level should become 3 because of B's active claim.
        // B's required level should remain 0 because its dependency is not yet satisfied.
        // C's required level should remain 0 because its dependency is not yet satisfied.
        // D's required level should remain 0.
        // Lease B should be pending.
        // Lease C should remain pending but no longer contingent because
        // B's active claim would satisfy C's passive claim.
        let lease_b = broker.acquire_lease(&element_b, 1).expect("acquire failed");
        let lease_b_id = lease_b.id.clone();
        assert_eq!(broker.get_required_level(&element_a), Some(3));
        assert_eq!(broker.get_required_level(&element_b), Some(0));
        assert_eq!(broker.get_required_level(&element_c), Some(0));
        assert_eq!(broker.get_required_level(&element_d), Some(0));
        assert_eq!(broker.get_lease_status(&lease_b.id), Some(LeaseStatus::Pending));
        assert_eq!(broker.get_lease_status(&lease_c.id), Some(LeaseStatus::Pending));
        assert_eq!(broker.is_lease_contingent(&lease_c.id), false);

        // Update A's current level to 3.
        // A's required level should remain 3.
        // B's required level should become 1 because its dependency is now satisfied.
        // C's required level should become 1 because its dependency is now satisfied.
        // D's required level should remain 0.
        // Lease B & C should become satisfied.
        broker.update_current_level(&element_a, 3);
        assert_eq!(broker.get_required_level(&element_a), Some(3));
        assert_eq!(broker.get_required_level(&element_b), Some(1));
        assert_eq!(broker.get_required_level(&element_c), Some(1));
        assert_eq!(broker.get_required_level(&element_d), Some(0));
        assert_eq!(broker.get_lease_status(&lease_b.id), Some(LeaseStatus::Satisfied));
        assert_eq!(broker.get_lease_status(&lease_c.id), Some(LeaseStatus::Satisfied));
        assert_eq!(broker.is_lease_contingent(&lease_c.id), false);

        // Drop Lease on B.
        // A's required level should drop to 2 until C drops its passive claim.
        // B's required level should become 0 because it is no longer leased.
        // C's required level should become 0 because it is now pending and contingent.
        // D's required level should remain 0.
        // Lease C should now be pending and contingent.
        broker.drop_lease(&lease_b.id).expect("drop_lease failed");
        assert_eq!(broker.get_required_level(&element_a), Some(2));
        assert_eq!(broker.get_required_level(&element_b), Some(0));
        assert_eq!(broker.get_required_level(&element_c), Some(0));
        assert_eq!(broker.get_required_level(&element_d), Some(0));
        assert_eq!(broker.get_lease_status(&lease_c.id), Some(LeaseStatus::Pending));
        assert_eq!(broker.is_lease_contingent(&lease_c.id), true);

        // Update A's current level to 2.
        // A's required level should remain 2.
        // B's required level should remain 0.
        // C's required level should remain 0.
        // D's required level should remain 0.
        // Lease C should still be pending and contingent.
        broker.update_current_level(&element_a, 2);
        assert_eq!(broker.get_required_level(&element_a), Some(2));
        assert_eq!(broker.get_required_level(&element_b), Some(0));
        assert_eq!(broker.get_required_level(&element_c), Some(0));
        assert_eq!(broker.get_required_level(&element_d), Some(0));
        assert_eq!(broker.get_lease_status(&lease_c.id), Some(LeaseStatus::Pending));
        assert_eq!(broker.is_lease_contingent(&lease_c.id), true);

        // Drop Lease on C.
        // A's required level should become 0.
        // B's required level should remain 0.
        // C's required level should remain 0.
        // D's required level should remain 0.
        broker.drop_lease(&lease_c.id).expect("drop_lease failed");
        assert_eq!(broker.get_required_level(&element_a), Some(0));
        assert_eq!(broker.get_required_level(&element_b), Some(0));
        assert_eq!(broker.get_required_level(&element_c), Some(0));
        assert_eq!(broker.get_required_level(&element_d), Some(0));

        // Reacquire Lease on C.
        // A's required level should remain 0 despite C's new passive claim.
        // B's required level should remain 0.
        // C's required level should remain 0.
        // D's required level should remain 0.
        // The lease on C should remain Pending even though A's current level is 2.
        let lease_c_reacquired = broker.acquire_lease(&element_c, 1).expect("acquire failed");
        let lease_c_reacquired_id = lease_c_reacquired.id.clone();
        assert_eq!(broker.get_required_level(&element_a), Some(0));
        assert_eq!(broker.get_required_level(&element_b), Some(0));
        assert_eq!(broker.get_required_level(&element_c), Some(0));
        assert_eq!(broker.get_required_level(&element_d), Some(0));
        assert_eq!(broker.get_lease_status(&lease_c_reacquired.id), Some(LeaseStatus::Pending));
        assert_eq!(broker.is_lease_contingent(&lease_c_reacquired.id), true);

        // Acquire Lease on D.
        // A's required level should become 1, but this is not enough to
        // satisfy C's passive claim.
        // B's required level should remain 0.
        // C's required level should remain 0 even though A's current level is 2.
        // D's required level should become 1 immediately because its dependency is already satisfied.
        // The lease on D should immediately be satisfied by A's current level of 2.
        // The lease on C should remain Pending even though A's current level is 2.
        let lease_d = broker.acquire_lease(&element_d, 1).expect("acquire failed");
        let lease_d_id = lease_d.id.clone();
        assert_eq!(broker.get_required_level(&element_a), Some(1));
        assert_eq!(broker.get_required_level(&element_b), Some(0));
        assert_eq!(broker.get_required_level(&element_c), Some(0));
        assert_eq!(broker.get_required_level(&element_d), Some(1));
        assert_eq!(broker.get_lease_status(&lease_c_reacquired.id), Some(LeaseStatus::Pending));
        assert_eq!(broker.get_lease_status(&lease_d.id), Some(LeaseStatus::Satisfied));
        assert_eq!(broker.is_lease_contingent(&lease_c_reacquired.id), true);

        // Update A's current level to 1.
        // A's required level should remain 1.
        // B's required level should remain 0.
        // C's required level should remain 0.
        // D's required level should remain 1.
        // The lease on C should remain Pending.
        // The lease on D should still be satisfied.
        broker.update_current_level(&element_a, 1);
        assert_eq!(broker.get_required_level(&element_a), Some(1));
        assert_eq!(broker.get_required_level(&element_b), Some(0));
        assert_eq!(broker.get_required_level(&element_c), Some(0));
        assert_eq!(broker.get_required_level(&element_d), Some(1));
        assert_eq!(broker.get_lease_status(&lease_c_reacquired.id), Some(LeaseStatus::Pending));
        assert_eq!(broker.is_lease_contingent(&lease_c_reacquired.id), true);
        assert_eq!(broker.get_lease_status(&lease_d.id), Some(LeaseStatus::Satisfied));

        // Drop reacquired lease on C.
        // A's required level should remain 1.
        // B's required level should remain 0.
        // C's required level should remain 0.
        // D's required level should remain 1.
        // The lease on D should still be satisfied.
        broker.drop_lease(&lease_c_reacquired.id).expect("drop_lease failed");
        assert_eq!(broker.get_required_level(&element_a), Some(1));
        assert_eq!(broker.get_required_level(&element_b), Some(0));
        assert_eq!(broker.get_required_level(&element_c), Some(0));
        assert_eq!(broker.get_required_level(&element_d), Some(1));
        assert_eq!(broker.get_lease_status(&lease_d.id), Some(LeaseStatus::Satisfied));

        // Drop lease on D.
        // A's required level should become 0.
        // B's required level should remain 0.
        // C's required level should remain 0.
        // D's required level should become 0 because it is no longer leased.
        broker.drop_lease(&lease_d.id).expect("drop_lease failed");
        assert_eq!(broker.get_required_level(&element_a), Some(0));
        assert_eq!(broker.get_required_level(&element_b), Some(0));
        assert_eq!(broker.get_required_level(&element_c), Some(0));
        assert_eq!(broker.get_required_level(&element_d), Some(0));

        // All leases should be cleaned up.
        assert_lease_cleaned_up(&broker.catalog, &lease_b_id);
        assert_lease_cleaned_up(&broker.catalog, &lease_c_id);
        assert_lease_cleaned_up(&broker.catalog, &lease_c_reacquired_id);
        assert_lease_cleaned_up(&broker.catalog, &lease_d_id);
    }

    #[fuchsia::test]
    async fn test_lease_passive_levels() {
        // Tests that a lease with a passive claim remains unsatisfied
        // if a lease with an active but lower level claim on the required
        // element is added.
        //
        // B @ 10 has an active dependency on A @ 1.
        // B @ 20 has an active dependency on A @ 1.
        // C @ 5 has a passive dependency on A @ 2.
        //  A     B     C
        //  2 <= 20
        //  1 <= 10
        //  2 <-------- 5
        let mut broker = Broker::new();
        let token_a_active = DependencyToken::create();
        let token_a_passive = DependencyToken::create();
        let element_a = broker
            .add_element(
                "A",
                0,
                vec![0, 1, 2],
                vec![],
                vec![token_a_active
                    .duplicate_handle(zx::Rights::SAME_RIGHTS)
                    .expect("dup failed")
                    .into()],
                vec![token_a_passive
                    .duplicate_handle(zx::Rights::SAME_RIGHTS)
                    .expect("dup failed")
                    .into()],
            )
            .expect("add_element failed");
        let element_b = broker
            .add_element(
                "B",
                0,
                vec![0, 10, 20],
                vec![
                    fpb::LevelDependency {
                        dependency_type: DependencyType::Active,
                        dependent_level: 10,
                        requires_token: token_a_active
                            .duplicate_handle(zx::Rights::SAME_RIGHTS)
                            .expect("dup failed")
                            .into(),
                        requires_level: 1,
                    },
                    fpb::LevelDependency {
                        dependency_type: DependencyType::Active,
                        dependent_level: 20,
                        requires_token: token_a_active
                            .duplicate_handle(zx::Rights::SAME_RIGHTS)
                            .expect("dup failed")
                            .into(),
                        requires_level: 2,
                    },
                ],
                vec![],
                vec![],
            )
            .expect("add_element failed");
        let element_c = broker
            .add_element(
                "C",
                0,
                vec![0, 5],
                vec![fpb::LevelDependency {
                    dependency_type: DependencyType::Passive,
                    dependent_level: 5,
                    requires_token: token_a_passive
                        .duplicate_handle(zx::Rights::SAME_RIGHTS)
                        .expect("dup failed")
                        .into(),
                    requires_level: 2,
                }],
                vec![],
                vec![],
            )
            .expect("add_element failed");

        // Initial required level for all elements should be 0.
        // Set all current levels to 0.
        assert_eq!(broker.get_required_level(&element_a), Some(0));
        assert_eq!(broker.get_required_level(&element_b), Some(0));
        assert_eq!(broker.get_required_level(&element_c), Some(0));
        broker.update_current_level(&element_a, 0);
        broker.update_current_level(&element_b, 0);
        broker.update_current_level(&element_c, 0);

        // Lease C @ 5.
        // A's required level should remain 0 because of C's passive claim.
        // B's required level should remain 0.
        // C's required level should remain 0 because its lease is pending and contingent.
        // Lease C should be pending because it is contingent on a passive claim.
        let lease_c = broker.acquire_lease(&element_c, 5).expect("acquire failed");
        let lease_c_id = lease_c.id.clone();
        assert_eq!(broker.get_required_level(&element_a), Some(0));
        assert_eq!(broker.get_required_level(&element_b), Some(0));
        assert_eq!(broker.get_required_level(&element_c), Some(0));
        assert_eq!(broker.get_lease_status(&lease_c.id), Some(LeaseStatus::Pending));
        assert_eq!(broker.is_lease_contingent(&lease_c.id), true);

        // Lease B @ 10.
        // A's required level should become 1 because of B's active claim.
        // B's required level should remain 0 because its dependency is not yet satisfied.
        // C's required level should remain 0 because its lease is pending and contingent.
        // Lease B @ 10 should be pending.
        // Lease C should remain pending because A @ 1 does not satisfy its claim.
        let lease_b_10 = broker.acquire_lease(&element_b, 10).expect("acquire failed");
        let lease_b_10_id = lease_b_10.id.clone();
        assert_eq!(broker.get_required_level(&element_a), Some(1));
        assert_eq!(broker.get_required_level(&element_b), Some(0));
        assert_eq!(broker.get_required_level(&element_c), Some(0));
        assert_eq!(broker.get_lease_status(&lease_b_10.id), Some(LeaseStatus::Pending));
        assert_eq!(broker.get_lease_status(&lease_c.id), Some(LeaseStatus::Pending));
        assert_eq!(broker.is_lease_contingent(&lease_c.id), true);

        // Update A's current level to 1.
        // A's required level should remain 1.
        // B's required level should become 10 because its dependency is now satisfied.
        // C's required level should remain 0 because A @ 1 does not satisfy its claim.
        // Lease B @ 10 should become satisfied.
        // Lease C should remain pending and contingent because A @ 1 does not
        // satisfy its claim.
        broker.update_current_level(&element_a, 1);
        assert_eq!(broker.get_required_level(&element_a), Some(1));
        assert_eq!(broker.get_required_level(&element_b), Some(10));
        assert_eq!(broker.get_required_level(&element_c), Some(0));
        assert_eq!(broker.get_lease_status(&lease_b_10.id), Some(LeaseStatus::Satisfied));
        assert_eq!(broker.get_lease_status(&lease_c.id), Some(LeaseStatus::Pending));
        assert_eq!(broker.is_lease_contingent(&lease_c.id), true);

        // Lease B @ 20.
        // A's required level should become 2 because of the new lease's active claim.
        // B's required level should remain 10 because the new lease's claim is not yet satisfied.
        // C's required level should remain 0 because its dependency is not yet satisfied.
        // Lease B @ 10 should still be satisfied.
        // Lease B @ 20 should be pending.
        // Lease C should remain pending because A is not yet at 2, but
        // should no longer be contingent on its passive claim.
        let lease_b_20 = broker.acquire_lease(&element_b, 20).expect("acquire failed");
        let lease_b_20_id = lease_b_20.id.clone();
        assert_eq!(broker.get_required_level(&element_a), Some(2));
        assert_eq!(broker.get_required_level(&element_b), Some(10));
        assert_eq!(broker.get_required_level(&element_c), Some(0));
        assert_eq!(broker.get_lease_status(&lease_b_10.id), Some(LeaseStatus::Satisfied));
        assert_eq!(broker.get_lease_status(&lease_b_20.id), Some(LeaseStatus::Pending));
        assert_eq!(broker.get_lease_status(&lease_c.id), Some(LeaseStatus::Pending));
        assert_eq!(broker.is_lease_contingent(&lease_c.id), false);

        // Update A's current level to 2.
        // A's required level should remain 2.
        // B's required level should become 20 because the new lease's claim is now satisfied.
        // C's required level should become 5 because its dependency is now satisfied.
        // Lease B @ 10 should still be satisfied.
        // Lease B @ 20 should become satisfied.
        // Lease C should become satisfied because A @ 2 satisfies its passive claim.
        broker.update_current_level(&element_a, 2);
        assert_eq!(broker.get_required_level(&element_a), Some(2));
        assert_eq!(broker.get_required_level(&element_b), Some(20));
        assert_eq!(broker.get_required_level(&element_c), Some(5));
        assert_eq!(broker.get_lease_status(&lease_b_10.id), Some(LeaseStatus::Satisfied));
        assert_eq!(broker.get_lease_status(&lease_b_20.id), Some(LeaseStatus::Satisfied));
        assert_eq!(broker.get_lease_status(&lease_c.id), Some(LeaseStatus::Satisfied));
        assert_eq!(broker.is_lease_contingent(&lease_c.id), false);

        // Drop Lease on B @ 20.
        // A's required level should remain 2 because of C's passive claim.
        // B's required level should become 10 because the higher lease has been dropped.
        // C's required level should become 0 because its lease is now pending and contingent.
        // Lease B @ 10 should still be satisfied.
        // Lease C should now be pending and contingent.
        broker.drop_lease(&lease_b_20.id).expect("drop_lease failed");
        assert_eq!(broker.get_required_level(&element_a), Some(2));
        assert_eq!(broker.get_required_level(&element_b), Some(10));
        assert_eq!(broker.get_required_level(&element_c), Some(0));
        assert_eq!(broker.get_lease_status(&lease_b_10.id), Some(LeaseStatus::Satisfied));
        assert_eq!(broker.get_lease_status(&lease_c.id), Some(LeaseStatus::Pending));
        assert_eq!(broker.is_lease_contingent(&lease_c.id), true);

        // Drop Lease on C.
        // A's required level should become 1 because C has dropped its claim,
        // but B's lower lease is still active.
        // B's required level should remain 10.
        // C's required level should remain 0.
        // Lease B @ 10 should still be satisfied.
        broker.drop_lease(&lease_c.id).expect("drop_lease failed");
        assert_eq!(broker.get_required_level(&element_a), Some(1));
        assert_eq!(broker.get_required_level(&element_b), Some(10));
        assert_eq!(broker.get_required_level(&element_c), Some(0));
        assert_eq!(broker.get_lease_status(&lease_b_10.id), Some(LeaseStatus::Satisfied));

        // Update A's current level to 1.
        // A's required level should remain 1.
        // B's required level should remain 10.
        // C's required level should remain 0.
        // Lease B @ 10 should still be satisfied.
        broker.update_current_level(&element_a, 1);
        assert_eq!(broker.get_required_level(&element_a), Some(1));
        assert_eq!(broker.get_required_level(&element_b), Some(10));
        assert_eq!(broker.get_required_level(&element_c), Some(0));
        assert_eq!(broker.get_lease_status(&lease_b_10.id), Some(LeaseStatus::Satisfied));

        // Drop Lease on B @ 10.
        // A's required level should drop to 0 because all claims have been dropped.
        // B's required level should become 0 because its leases have been dropped.
        // C's required level should remain 0.
        broker.drop_lease(&lease_b_10.id).expect("drop_lease failed");
        assert_eq!(broker.get_required_level(&element_a), Some(0));
        assert_eq!(broker.get_required_level(&element_b), Some(0));
        assert_eq!(broker.get_required_level(&element_c), Some(0));

        // Update A's current level to 0.
        // A's required level should remain 0.
        // B's required level should remain 0.
        // C's required level should remain 0.
        broker.update_current_level(&element_a, 0);
        assert_eq!(broker.get_required_level(&element_a), Some(0));
        assert_eq!(broker.get_required_level(&element_b), Some(0));
        assert_eq!(broker.get_required_level(&element_c), Some(0));

        // Leases should be cleaned up.
        assert_lease_cleaned_up(&broker.catalog, &lease_b_10_id);
        assert_lease_cleaned_up(&broker.catalog, &lease_b_20_id);
        assert_lease_cleaned_up(&broker.catalog, &lease_c_id);
    }

    #[fuchsia::test]
    async fn test_lease_passive_independent_active() {
        // Tests that independent active claims of a lease are not activated
        // while a lease is Contingent (has one or more passive claims
        // but no other leases have active claims that would satisfy them).
        //
        // B has an active dependency on A.
        // C has a passive dependency on A.
        // C has an active dependency on D.
        //  A     B     C     D
        // ON <= ON
        // ON <------- ON => ON
        let mut broker = Broker::new();
        let token_a_active = DependencyToken::create();
        let token_a_passive = DependencyToken::create();
        let element_a = broker
            .add_element(
                "A",
                BinaryPowerLevel::Off.into_primitive(),
                BINARY_POWER_LEVELS.to_vec(),
                vec![],
                vec![token_a_active
                    .duplicate_handle(zx::Rights::SAME_RIGHTS)
                    .expect("dup failed")
                    .into()],
                vec![token_a_passive
                    .duplicate_handle(zx::Rights::SAME_RIGHTS)
                    .expect("dup failed")
                    .into()],
            )
            .expect("add_element failed");
        let element_b = broker
            .add_element(
                "B",
                BinaryPowerLevel::Off.into_primitive(),
                BINARY_POWER_LEVELS.to_vec(),
                vec![fpb::LevelDependency {
                    dependency_type: DependencyType::Active,
                    dependent_level: BinaryPowerLevel::On.into_primitive(),
                    requires_token: token_a_active
                        .duplicate_handle(zx::Rights::SAME_RIGHTS)
                        .expect("dup failed")
                        .into(),
                    requires_level: BinaryPowerLevel::On.into_primitive(),
                }],
                vec![],
                vec![],
            )
            .expect("add_element failed");
        let token_d_active = DependencyToken::create();
        let element_d = broker
            .add_element(
                "D",
                BinaryPowerLevel::Off.into_primitive(),
                BINARY_POWER_LEVELS.to_vec(),
                vec![],
                vec![token_d_active
                    .duplicate_handle(zx::Rights::SAME_RIGHTS)
                    .expect("dup failed")
                    .into()],
                vec![],
            )
            .expect("add_element failed");
        let element_c = broker
            .add_element(
                "C",
                BinaryPowerLevel::Off.into_primitive(),
                BINARY_POWER_LEVELS.to_vec(),
                vec![
                    fpb::LevelDependency {
                        dependency_type: DependencyType::Passive,
                        dependent_level: BinaryPowerLevel::On.into_primitive(),
                        requires_token: token_a_passive
                            .duplicate_handle(zx::Rights::SAME_RIGHTS)
                            .expect("dup failed")
                            .into(),
                        requires_level: BinaryPowerLevel::On.into_primitive(),
                    },
                    fpb::LevelDependency {
                        dependency_type: DependencyType::Active,
                        dependent_level: BinaryPowerLevel::On.into_primitive(),
                        requires_token: token_d_active
                            .duplicate_handle(zx::Rights::SAME_RIGHTS)
                            .expect("dup failed")
                            .into(),
                        requires_level: BinaryPowerLevel::On.into_primitive(),
                    },
                ],
                vec![],
                vec![],
            )
            .expect("add_element failed");

        // Initial required level for all elements should be OFF.
        // Set all current levels to OFF.
        assert_eq!(
            broker.get_required_level(&element_a),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_b),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_c),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_d),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        broker.update_current_level(&element_a, BinaryPowerLevel::Off.into_primitive());
        broker.update_current_level(&element_b, BinaryPowerLevel::Off.into_primitive());
        broker.update_current_level(&element_c, BinaryPowerLevel::Off.into_primitive());
        broker.update_current_level(&element_d, BinaryPowerLevel::Off.into_primitive());

        // Lease C.
        // A's required level should remain OFF because C's passive claim
        // does not raise the required level of A.
        // B's required level should remain OFF.
        // C's required level should remain OFF because its lease is pending and contingent.
        // D's required level should remain OFF C's passive claim on A has
        // no other active claims that would satisfy it.
        // Lease C should be pending and contingent because its passive
        // claim has no other active claim that would satisfy it.
        let lease_c = broker
            .acquire_lease(&element_c, BinaryPowerLevel::On.into_primitive())
            .expect("acquire failed");
        let lease_c_id = lease_c.id.clone();
        assert_eq!(
            broker.get_required_level(&element_a),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_b),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_c),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_d),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(broker.get_lease_status(&lease_c.id), Some(LeaseStatus::Pending));
        assert_eq!(broker.is_lease_contingent(&lease_c.id), true);

        // Lease B.
        // A's required level should become ON because of B's active claim.
        // B's required level should remain OFF because its dependency is not yet satisfied.
        // C's required level should remain OFF because its dependencies are not yet satisfied.
        // D's required level should become ON because C's passive claim would
        // be satisfied by B's active claim.
        // Lease B should be pending.
        // Lease C should be pending but no longer contingent because B's
        // active claim would satisfy C's passive claim.
        let lease_b = broker
            .acquire_lease(&element_b, BinaryPowerLevel::On.into_primitive())
            .expect("acquire failed");
        let lease_b_id = lease_b.id.clone();
        assert_eq!(
            broker.get_required_level(&element_a),
            Some(BinaryPowerLevel::On.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_b),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_c),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_d),
            Some(BinaryPowerLevel::On.into_primitive())
        );
        assert_eq!(broker.get_lease_status(&lease_b.id), Some(LeaseStatus::Pending));
        assert_eq!(broker.get_lease_status(&lease_c.id), Some(LeaseStatus::Pending));
        assert_eq!(broker.is_lease_contingent(&lease_b.id), false);
        assert_eq!(broker.is_lease_contingent(&lease_c.id), false);

        // Update A's current level to ON.
        // A's required level should remain ON.
        // B's required level should become ON because its dependency is now satisfied.
        // C's required level should remain OFF because its dependencies are not yet satisfied.
        // D's required level should remain ON.
        // Lease B should become satisfied.
        // Lease C should still be pending.
        broker.update_current_level(&element_a, BinaryPowerLevel::On.into_primitive());
        assert_eq!(
            broker.get_required_level(&element_a),
            Some(BinaryPowerLevel::On.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_b),
            Some(BinaryPowerLevel::On.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_c),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_d),
            Some(BinaryPowerLevel::On.into_primitive())
        );
        assert_eq!(broker.get_lease_status(&lease_b.id), Some(LeaseStatus::Satisfied));
        assert_eq!(broker.get_lease_status(&lease_c.id), Some(LeaseStatus::Pending));
        assert_eq!(broker.is_lease_contingent(&lease_b.id), false);
        assert_eq!(broker.is_lease_contingent(&lease_c.id), false);

        // Update D's current level to ON.
        // A's required level should remain ON.
        // B's required level should remain ON.
        // C's required level should become ON because its dependencies are now satisfied.
        // D's required level should remain ON.
        // Lease B should still be satisfied.
        // Lease C should become satisfied.
        broker.update_current_level(&element_d, BinaryPowerLevel::On.into_primitive());
        assert_eq!(
            broker.get_required_level(&element_a),
            Some(BinaryPowerLevel::On.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_b),
            Some(BinaryPowerLevel::On.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_c),
            Some(BinaryPowerLevel::On.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_d),
            Some(BinaryPowerLevel::On.into_primitive())
        );
        assert_eq!(broker.get_lease_status(&lease_b.id), Some(LeaseStatus::Satisfied));
        assert_eq!(broker.get_lease_status(&lease_c.id), Some(LeaseStatus::Satisfied));
        assert_eq!(broker.is_lease_contingent(&lease_b.id), false);
        assert_eq!(broker.is_lease_contingent(&lease_c.id), false);

        // Drop Lease on B.
        // A's required level should remain ON.
        // B's required level should become OFF because it is no longer leased.
        // C's required level should become OFF because its lease is now pending and contingent.
        // D's required level should remain ON until C has dropped the lease.
        // Lease C should now be pending and contingent.
        broker.drop_lease(&lease_b.id).expect("drop_lease failed");
        assert_eq!(
            broker.get_required_level(&element_a),
            Some(BinaryPowerLevel::On.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_b),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_c),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(broker.get_lease_status(&lease_c.id), Some(LeaseStatus::Pending));
        assert_eq!(broker.is_lease_contingent(&lease_c.id), true);

        // Drop Lease on C.
        // A's required level should become OFF because C has dropped its passive claim.
        // B's required level should remain OFF.
        // C's required level should remain OFF.
        // D's required level should become OFF because C has dropped its active claim.
        broker.drop_lease(&lease_c.id).expect("drop_lease failed");
        assert_eq!(
            broker.get_required_level(&element_a),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_b),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_c),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_d),
            Some(BinaryPowerLevel::Off.into_primitive())
        );

        // Update A's current level to OFF.
        // All required levels should remain OFF.
        broker.update_current_level(&element_a, BinaryPowerLevel::Off.into_primitive());
        assert_eq!(
            broker.get_required_level(&element_a),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_b),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_c),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_d),
            Some(BinaryPowerLevel::Off.into_primitive())
        );

        // Update D's current level to OFF.
        // All required levels should remain OFF.
        broker.update_current_level(&element_d, BinaryPowerLevel::Off.into_primitive());
        assert_eq!(
            broker.get_required_level(&element_a),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_b),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_c),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_d),
            Some(BinaryPowerLevel::Off.into_primitive())
        );

        // Leases C & B should be cleaned up.
        assert_lease_cleaned_up(&broker.catalog, &lease_b_id);
        assert_lease_cleaned_up(&broker.catalog, &lease_c_id);
    }

    #[fuchsia::test]
    async fn test_lease_passive_chained() {
        // Tests that active dependencies, which depend on passive dependencies,
        // which in turn depend on active dependencies, all work as expected.
        //
        // B has an active dependency on A.
        // C has a passive dependency on B (and transitively, a passive dependency on A).
        // D has an active dependency on B (and transitively, an active dependency on A).
        // E has an active dependency on C (and transitively, a passive dependency on A & B).
        //  A     B     C     D     E
        // ON <= ON
        //       ON <- ON <======= ON
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
        let token_c_active = DependencyToken::create();
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
                vec![token_c_active
                    .duplicate_handle(zx::Rights::SAME_RIGHTS)
                    .expect("dup failed")
                    .into()],
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
        let element_e = broker
            .add_element(
                "E",
                BinaryPowerLevel::Off.into_primitive(),
                BINARY_POWER_LEVELS.to_vec(),
                vec![fpb::LevelDependency {
                    dependency_type: DependencyType::Active,
                    dependent_level: BinaryPowerLevel::On.into_primitive(),
                    requires_token: token_c_active
                        .duplicate_handle(zx::Rights::SAME_RIGHTS)
                        .expect("dup failed")
                        .into(),
                    requires_level: BinaryPowerLevel::On.into_primitive(),
                }],
                vec![],
                vec![],
            )
            .expect("add_element failed");

        // Initial required level for all elements should be OFF.
        // Set all current levels to OFF.
        assert_eq!(
            broker.get_required_level(&element_a),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_b),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_c),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_d),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_e),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        broker.update_current_level(&element_a, BinaryPowerLevel::Off.into_primitive());
        broker.update_current_level(&element_b, BinaryPowerLevel::Off.into_primitive());
        broker.update_current_level(&element_c, BinaryPowerLevel::Off.into_primitive());
        broker.update_current_level(&element_d, BinaryPowerLevel::Off.into_primitive());
        broker.update_current_level(&element_e, BinaryPowerLevel::Off.into_primitive());

        // Lease E.
        // A, B, & C's required levels should remain OFF because of C's passive claim.
        // D's required level should remain OFF.
        // E's required level should remain OFF because its lease is pending and contingent.
        // Lease E should be pending and contingent on its passive claim.
        let lease_e = broker
            .acquire_lease(&element_e, BinaryPowerLevel::On.into_primitive())
            .expect("acquire failed");
        let lease_e_id = lease_e.id.clone();
        assert_eq!(
            broker.get_required_level(&element_a),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_b),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_c),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_d),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_e),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(broker.get_lease_status(&lease_e.id), Some(LeaseStatus::Pending));
        assert_eq!(broker.is_lease_contingent(&lease_e.id), true);

        // Lease D.
        // A's required level should become ON because of D's transitive active claim.
        // B's required level should remain OFF because A is not yet ON.
        // C's required level should remain OFF because B is not yet ON.
        // D's required level should remain OFF because B is not yet ON.
        // E's required level should remain OFF because C is not yet ON.
        // Lease D should be pending.
        // Lease E should remain pending but is no longer contingent.
        let lease_d = broker
            .acquire_lease(&element_d, BinaryPowerLevel::On.into_primitive())
            .expect("acquire failed");
        let lease_d_id = lease_d.id.clone();
        assert_eq!(
            broker.get_required_level(&element_a),
            Some(BinaryPowerLevel::On.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_b),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_c),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_d),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_e),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(broker.get_lease_status(&lease_d.id), Some(LeaseStatus::Pending));
        assert_eq!(broker.get_lease_status(&lease_e.id), Some(LeaseStatus::Pending));
        assert_eq!(broker.is_lease_contingent(&lease_d.id), false);
        assert_eq!(broker.is_lease_contingent(&lease_e.id), false);

        // Update A's current level to ON.
        // A's required level should remain ON.
        // B's required level should become ON because of D's active claim and
        // its dependency on A being satisfied.
        // C's required level should remain OFF because B is not ON.
        // D's required level should remain OFF because B is not yet ON.
        // E's required level should remain OFF because C is not yet ON.
        // Lease D & E should remain pending.
        broker.update_current_level(&element_a, BinaryPowerLevel::On.into_primitive());
        assert_eq!(
            broker.get_required_level(&element_a),
            Some(BinaryPowerLevel::On.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_b),
            Some(BinaryPowerLevel::On.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_c),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_d),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_e),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(broker.get_lease_status(&lease_d.id), Some(LeaseStatus::Pending));
        assert_eq!(broker.get_lease_status(&lease_e.id), Some(LeaseStatus::Pending));
        assert_eq!(broker.is_lease_contingent(&lease_d.id), false);
        assert_eq!(broker.is_lease_contingent(&lease_e.id), false);

        // Update B's current level to ON.
        // A & B's required level should remain ON.
        // C's required level should become ON because B is now ON.
        // D's required level should become ON because B is now ON.
        // E's required level should remain OFF because C is not yet ON.
        // Lease D should become satisfied.
        broker.update_current_level(&element_b, BinaryPowerLevel::On.into_primitive());
        assert_eq!(
            broker.get_required_level(&element_a),
            Some(BinaryPowerLevel::On.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_b),
            Some(BinaryPowerLevel::On.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_c),
            Some(BinaryPowerLevel::On.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_d),
            Some(BinaryPowerLevel::On.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_e),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(broker.get_lease_status(&lease_d.id), Some(LeaseStatus::Satisfied));
        assert_eq!(broker.get_lease_status(&lease_e.id), Some(LeaseStatus::Pending));
        assert_eq!(broker.is_lease_contingent(&lease_d.id), false);
        assert_eq!(broker.is_lease_contingent(&lease_e.id), false);

        // Update C's current level to ON.
        // A, B, C & D's required level should remain ON.
        // E's required level should become ON because C is now ON.
        // Lease E should become satisfied.
        broker.update_current_level(&element_c, BinaryPowerLevel::On.into_primitive());
        assert_eq!(
            broker.get_required_level(&element_a),
            Some(BinaryPowerLevel::On.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_b),
            Some(BinaryPowerLevel::On.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_c),
            Some(BinaryPowerLevel::On.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_d),
            Some(BinaryPowerLevel::On.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_e),
            Some(BinaryPowerLevel::On.into_primitive())
        );
        assert_eq!(broker.get_lease_status(&lease_e.id), Some(LeaseStatus::Satisfied));
        assert_eq!(broker.get_lease_status(&lease_d.id), Some(LeaseStatus::Satisfied));
        assert_eq!(broker.is_lease_contingent(&lease_d.id), false);
        assert_eq!(broker.is_lease_contingent(&lease_e.id), false);

        // Drop Lease on D.
        // A, B & C's required levels should remain ON.
        // D's required level should become OFF because it is no longer leased.
        // E's required level should become OFF because its lease is now pending and contingent.
        // Lease E should become pending and contingent.
        broker.drop_lease(&lease_d.id).expect("drop_lease failed");
        assert_eq!(
            broker.get_required_level(&element_a),
            Some(BinaryPowerLevel::On.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_b),
            Some(BinaryPowerLevel::On.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_c),
            Some(BinaryPowerLevel::On.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_d),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_e),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(broker.get_lease_status(&lease_e.id), Some(LeaseStatus::Pending));
        assert_eq!(broker.is_lease_contingent(&lease_e.id), true);

        // Drop Lease on E.
        // A's required level should remain ON because B is still ON.
        // B's required level should remain ON because C is still ON.
        // C's required level should become OFF because E has dropped its claim.
        // D's required level should remain OFF.
        // E's required level should remain OFF.
        broker.drop_lease(&lease_e.id).expect("drop_lease failed");
        assert_eq!(
            broker.get_required_level(&element_a),
            Some(BinaryPowerLevel::On.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_b),
            Some(BinaryPowerLevel::On.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_c),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_d),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_e),
            Some(BinaryPowerLevel::Off.into_primitive())
        );

        // Update C's current level to OFF.
        // A's required level should remain ON because B is still ON.
        // B's required level should become OFF because C is now OFF.
        // C's required level should remain OFF.
        // D's required level should remain OFF.
        // E's required level should remain OFF.
        broker.update_current_level(&element_c, BinaryPowerLevel::Off.into_primitive());
        assert_eq!(
            broker.get_required_level(&element_a),
            Some(BinaryPowerLevel::On.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_b),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_c),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_d),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_e),
            Some(BinaryPowerLevel::Off.into_primitive())
        );

        // Update B's current level to OFF.
        // A's required level should become OFF because B is now OFF.
        // B, C, D & E's required levels should remain OFF.
        broker.update_current_level(&element_b, BinaryPowerLevel::Off.into_primitive());
        assert_eq!(
            broker.get_required_level(&element_a),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_b),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_c),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_d),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_e),
            Some(BinaryPowerLevel::Off.into_primitive())
        );

        // Update A's current level to OFF.
        // All required levels should remain OFF.
        broker.update_current_level(&element_b, BinaryPowerLevel::Off.into_primitive());
        assert_eq!(
            broker.get_required_level(&element_a),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_b),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_c),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_d),
            Some(BinaryPowerLevel::Off.into_primitive())
        );
        assert_eq!(
            broker.get_required_level(&element_e),
            Some(BinaryPowerLevel::Off.into_primitive())
        );

        // Leases D and E should be cleaned up.
        assert_lease_cleaned_up(&broker.catalog, &lease_d_id);
        assert_lease_cleaned_up(&broker.catalog, &lease_e_id);
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
