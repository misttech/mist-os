// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{anyhow, Context, Error};
use async_utils::hanging_get::server::{HangingGet, Publisher, Subscriber};
use fidl_fuchsia_power_broker::{
    self as fpb, DependencyType, LeaseStatus, Permissions, RegisterDependencyTokenError,
    RequiredLevelWatchResponder, UnregisterDependencyTokenError,
};
use fuchsia_inspect::{InspectType as IType, Node as INode};
use futures::channel::mpsc::{unbounded, UnboundedReceiver, UnboundedSender};
use itertools::Itertools;
use std::cmp::max;
use std::collections::{HashMap, HashSet};
use std::fmt::{self, Debug};
use std::hash::Hash;
use std::iter::repeat;
use uuid::Uuid;

use crate::credentials::*;
use crate::topology::*;

/// If true, use non-random IDs for ease of debugging.
const ID_DEBUG_MODE: bool = false;

/// Max value for inspect event history.
const INSPECT_GRAPH_EVENT_BUFFER_SIZE: usize = 512;

// Below are a series of type aliases for convenience
type StateChangeFn = Box<dyn Fn(&IndexedPowerLevel, RequiredLevelWatchResponder) -> bool>;
type RequiredLevelHangingGet =
    HangingGet<IndexedPowerLevel, RequiredLevelWatchResponder, StateChangeFn>;
type RequiredLevelPublisher =
    Publisher<IndexedPowerLevel, RequiredLevelWatchResponder, StateChangeFn>;
pub type RequiredLevelSubscriber =
    Subscriber<IndexedPowerLevel, RequiredLevelWatchResponder, StateChangeFn>;

struct RequiredLevelAdmin {
    /// We pass new power level values to the publisher, which takes care of updating the remote
    /// clients using hanging-gets.
    publisher: RequiredLevelPublisher,
    /// We use this to vend a new `RequiredLevelSubscriber` for each new `RequiredLevel.Watch`
    /// request stream.
    hanging_get: RequiredLevelHangingGet,
    /// Cached `IndexedPowerLevel` value. Simply used to determine if the value has changed.
    level: IndexedPowerLevel,
}

pub struct Broker {
    catalog: Catalog,
    credentials: Registry,
    // The current level for each element, as reported to the broker.
    current: SubscribeMap<ElementID, IndexedPowerLevel>,
    // The level for each element required by the topology.
    required: HashMap<ElementID, RequiredLevelAdmin>,
    _inspect_node: INode,
}

impl Broker {
    pub fn new(inspect: INode) -> Self {
        Broker {
            catalog: Catalog::new(&inspect),
            credentials: Registry::new(),
            current: SubscribeMap::new(None),
            required: HashMap::new(),
            _inspect_node: inspect,
        }
    }

    fn lookup_credentials(&self, token: Token) -> Option<Credential> {
        self.credentials.lookup(token)
    }

    fn unregister_all_credentials_for_element(&mut self, element_id: &ElementID) {
        self.credentials.unregister_all_for_element(element_id)
    }

    #[cfg(test)]
    pub fn get_unsatisfiable_element_id(&self) -> ElementID {
        self.catalog.topology.get_unsatisfiable_element_id()
    }

    #[cfg(test)]
    pub fn get_unsatisfiable_element_name(&self) -> String {
        self.catalog.topology.get_unsatisfiable_element_name()
    }

    #[cfg(test)]
    pub fn get_unsatisfiable_element_levels(&self) -> Vec<u64> {
        self.catalog.topology.get_unsatisfiable_element_levels()
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

    pub fn update_current_level(&mut self, element_id: &ElementID, level: IndexedPowerLevel) {
        tracing::debug!("update_current_level({element_id}, {level:?})");
        let prev_level = self.update_current_level_internal(element_id, level);
        if prev_level.as_ref() == Some(&level) {
            return;
        }
        if prev_level.is_none() || prev_level.unwrap() < level {
            // The level was increased, look for activated assertive or pending
            // opportunistic claims that are newly satisfied by the new current level:
            let claims_for_required_element: Vec<Claim> = self
                .catalog
                .assertive_claims
                .activated
                .for_required_element(element_id)
                .into_iter()
                .chain(self.catalog.opportunistic_claims.pending.for_required_element(element_id))
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
            // satisfied, other previously pending assertive claims requiring the
            // dependent may now be ready to be activated (though they may not
            // if the dependent has other unsatisfied dependencies). Look for
            // all pending assertive claims on this dependent, and pass them to
            // activate_assertive_claims_if_dependencies_satisfied(), which will check
            // if all dependencies of the dependent are now satisfied, and if
            // so, activate the pending claims on dependent, raising its
            // required level:
            for dependent in dependents_of_claims_satisfied {
                let pending_assertive_claims_on_dependent =
                    self.catalog.assertive_claims.pending.for_required_element(&dependent);
                tracing::debug!(
                    "update_current_level({element_id}): pending_assertive_claims_on_dependent({dependent}) = {})",
                    &pending_assertive_claims_on_dependent.iter().join(", ")
                );
                self.activate_assertive_claims_if_dependencies_satisfied(
                    pending_assertive_claims_on_dependent,
                );
            }
            // For pending opportunistic claims, if the current level satisfies them,
            // and their lease is no longer contingent, activate the claim.
            for claim in self.catalog.opportunistic_claims.pending.for_required_element(element_id)
            {
                if !level.satisfies(claim.requires().level) {
                    continue;
                }
                if self.is_lease_contingent(&claim.lease_id) {
                    continue;
                }
                self.catalog.opportunistic_claims.activate_claim(&claim.id)
            }
            // Find the set of leases for all claims satisfied:
            let leases_to_check_if_satisfied: HashSet<LeaseID> =
                claims_satisfied.into_iter().map(|c| c.lease_id).collect();
            // Update the status of all leases whose claims were satisfied.
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
            // If the level was lowered, find activated claims whose lease has
            // become contingent or dropped and see if any of these claims no
            // longer have any dependents and thus can be deactivated or
            // dropped.
            let assertive_claims_to_deactivate = self
                .catalog
                .assertive_claims
                .activated
                .marked_to_deactivate_for_element(element_id);
            let assertive_claims_to_drop =
                self.find_claims_with_no_dependents(&assertive_claims_to_deactivate);
            let opportunistic_claims_to_deactivate = self
                .catalog
                .opportunistic_claims
                .activated
                .marked_to_deactivate_for_element(element_id);
            let opportunistic_claims_to_drop =
                self.find_claims_with_no_dependents(&opportunistic_claims_to_deactivate);
            self.drop_or_deactivate_assertive_claims(&assertive_claims_to_drop);
            self.drop_or_deactivate_opportunistic_claims(&opportunistic_claims_to_drop);
        }
    }

    #[cfg(test)]
    pub fn get_current_level(&mut self, element_id: &ElementID) -> Option<IndexedPowerLevel> {
        self.current.get(element_id)
    }

    pub fn watch_current_level(
        &mut self,
        element_id: &ElementID,
    ) -> UnboundedReceiver<Option<IndexedPowerLevel>> {
        self.current.subscribe(element_id)
    }

    fn update_current_level_internal(
        &mut self,
        element_id: &ElementID,
        level: IndexedPowerLevel,
    ) -> Option<IndexedPowerLevel> {
        let previous = self.current.update(&element_id, level);
        // Only update inspect if the value has changed.
        if previous != Some(level) {
            if let Ok(elem_inspect) = self.catalog.topology.inspect_for_element(&element_id) {
                elem_inspect.borrow_mut().meta().set("current_level", level.level);
            }
        }
        previous
    }

    #[cfg(test)]
    pub fn get_required_level(&self, element_id: &ElementID) -> Option<IndexedPowerLevel> {
        self.required.get(element_id).map(|e| e.level)
    }

    pub fn new_required_level_subscriber(
        &mut self,
        element_id: &ElementID,
    ) -> RequiredLevelSubscriber {
        self.required
            .get_mut(element_id)
            .ok_or(anyhow!("Element ({element_id}) not added"))
            .unwrap()
            .hanging_get
            .new_subscriber()
    }

    fn update_required_level(
        &mut self,
        element_id: &ElementID,
        level: IndexedPowerLevel,
    ) -> Option<IndexedPowerLevel> {
        let previous = self.required.get(element_id).map(|e| e.level);
        if previous == Some(level) {
            return previous;
        }
        if let Some(required_level) = self.required.get_mut(element_id) {
            required_level.publisher.set(level);
            required_level.level = level;
        } else {
            let hanging_get = RequiredLevelHangingGet::new(
                level,
                Box::new(|level: &IndexedPowerLevel, res: RequiredLevelWatchResponder| -> bool {
                    if let Err(error) = res.send(Ok(level.level)).context("response failed") {
                        tracing::warn!(?error, "Failed to send required level to client");
                    }
                    true
                }),
            );
            let publisher = hanging_get.new_publisher();
            self.required
                .insert(element_id.clone(), RequiredLevelAdmin { hanging_get, publisher, level });
        }

        if let Ok(elem_inspect) = self.catalog.topology.inspect_for_element(element_id) {
            elem_inspect.borrow_mut().meta().set("required_level", level.level);
        }
        previous
    }

    pub fn acquire_lease(
        &mut self,
        element_id: &ElementID,
        level: IndexedPowerLevel,
    ) -> Result<Lease, fpb::LeaseError> {
        tracing::debug!("acquire_lease({element_id}@{level})");
        let (lease, assertive_claims) = self.catalog.create_lease_and_claims(element_id, level);
        if self.is_lease_contingent(&lease.id) {
            // Lease is blocked on opportunistic claims, update status and return.
            tracing::debug!(
                "acquire_lease({element_id}@{level}): {} is contingent on opportunistic claims",
                &lease.id
            );
            self.update_lease_status(&lease.id);
            return Ok(lease);
        }
        // Activate all pending assertive claims that have all of their
        // dependencies satisfied.
        self.activate_assertive_claims_if_dependencies_satisfied(assertive_claims.clone());
        // For pending opportunistic claims, if the current level satisfies them,
        // and their lease is no longer contingent, activate the claim.
        // (If the current level does not satisfy the claim, it will be
        // activated as part of update_current_level once it does.)
        for claim in self.catalog.opportunistic_claims.pending.for_lease(&lease.id) {
            if self.current_level_satisfies(claim.requires()) {
                self.catalog.opportunistic_claims.activate_claim(&claim.id)
            }
        }
        self.update_lease_status(&lease.id);
        // Other contingent leases may need to update their status if any new
        // assertive claims would satisfy their opportunistic claims.
        for assertive_claim in assertive_claims {
            tracing::debug!(
                "check if assertive claim {assertive_claim} would satisfy opportunistic claims"
            );
            let assertive_claim_requires = &assertive_claim.requires().clone();
            let opportunistic_claims_for_req_element = self
                .catalog
                .opportunistic_claims
                .pending
                .for_required_element(&assertive_claim_requires.element_id);
            tracing::debug!(
                "opportunistic_claims_for_req_element[{}]",
                opportunistic_claims_for_req_element.iter().join(", ")
            );
            let opportunistic_claims_possibly_affected = opportunistic_claims_for_req_element
                .iter()
                // Only consider claims for leases other than assertive_claim's
                .filter(|c| c.lease_id != lease.id)
                // Only consider opportunistic claims that would be satisfied by assertive_claim
                .filter(|c| assertive_claim_requires.level.satisfies(c.requires().level));
            for opportunistic_claim in opportunistic_claims_possibly_affected {
                tracing::debug!(
                    "assertive claim {assertive_claim} may have changed status of lease {}",
                    &opportunistic_claim.lease_id
                );
                if !self.is_lease_contingent(&opportunistic_claim.lease_id) {
                    tracing::debug!(
                        "assertive claim {assertive_claim} changed status of lease {}",
                        &opportunistic_claim.lease_id
                    );
                    self.on_lease_transition_to_noncontingent(&opportunistic_claim.lease_id)
                }
            }
        }
        Ok(lease)
    }

    /// Runs when a lease becomes no longer contingent.
    fn on_lease_transition_to_noncontingent(&mut self, lease_id: &LeaseID) {
        // Reset any assertive or opportunistic claims that were previously marked to
        // deactivate. Since they weren't already deactivated, they must
        // already be currently satisfied.
        for claim in self.catalog.assertive_claims.activated.for_lease(lease_id) {
            self.catalog.assertive_claims.activated.remove_from_claims_to_deactivate(&claim.id)
        }
        for claim in self.catalog.opportunistic_claims.activated.for_lease(lease_id) {
            self.catalog.opportunistic_claims.activated.remove_from_claims_to_deactivate(&claim.id)
        }
        // Activate any pending assertive claims for this lease whose
        // required elements have their dependencies satisfied.
        let pending_claims = self.catalog.assertive_claims.pending.for_lease(&lease_id);
        self.activate_assertive_claims_if_dependencies_satisfied(pending_claims);
        // Activate pending opportunistic claims for this lease, if they are
        // (already) currently satisfied.
        for claim in self.catalog.opportunistic_claims.pending.for_lease(&lease_id) {
            if !self.current_level_satisfies(claim.requires()) {
                continue;
            }
            self.catalog.opportunistic_claims.activate_claim(&claim.id)
        }
        self.update_lease_status(&lease_id);
    }

    pub fn drop_lease(&mut self, lease_id: &LeaseID) -> Result<(), Error> {
        let (lease, assertive_claims, opportunistic_claims) = self.catalog.drop(lease_id)?;
        let assertive_claims_dropped = self.find_claims_with_no_dependents(&assertive_claims);
        let opportunistic_claims_dropped =
            self.find_claims_with_no_dependents(&opportunistic_claims);
        self.drop_or_deactivate_assertive_claims(&assertive_claims_dropped);
        self.drop_or_deactivate_opportunistic_claims(&opportunistic_claims_dropped);
        self.catalog.lease_status.remove(lease_id);
        // Update the required level of the formerly leased element.
        self.update_required_levels(&vec![&lease.element_id]);
        Ok(())
    }

    /// Returns true if the lease has one or more opportunistic claims with no assertive
    /// claims belonging to other leases that would satisfy them. Such assertive claims
    /// must not themselves be contingent. If a lease is contingent on any of its
    /// opportunistic claims, none of its claims should be activated.
    fn is_lease_contingent(&self, lease_id: &LeaseID) -> bool {
        let opportunistic_claims = self
            .catalog
            .opportunistic_claims
            .activated
            .for_lease(lease_id)
            .into_iter()
            .chain(self.catalog.opportunistic_claims.pending.for_lease(lease_id).into_iter());
        for claim in opportunistic_claims {
            // If there is no other lease with an assertive or pending claim that
            // would satisfy each opportunistic claim, the lease is Contingent.
            let activated_claims = self
                .catalog
                .assertive_claims
                .activated
                .for_required_element(&claim.requires().element_id);
            let pending_claims = self
                .catalog
                .assertive_claims
                .pending
                .for_required_element(&claim.requires().element_id);
            let matching_assertive_claim = activated_claims
                .into_iter()
                .chain(pending_claims)
                // Consider an assertive claim to match if is part of this lease. This captures the
                // scenario where a lease is 'self-satisfying' - it holds an assertive claim for an
                // element and a opportunistic claim for the same element (at the same or lower level).
                .filter(|c| lease_id == &c.lease_id || !self.is_lease_contingent(&c.lease_id))
                .find(|c| c.requires().level.satisfies(claim.requires().level));
            if let Some(matching_assertive_claim) = matching_assertive_claim {
                tracing::debug!("{matching_assertive_claim} satisfies opportunistic {claim}");
            } else {
                return true;
            }
        }
        false
    }

    fn calculate_lease_status(&self, lease_id: &LeaseID) -> LeaseStatus {
        // If the lease has any Pending assertive claims, it is still Pending.
        if !self.catalog.assertive_claims.pending.for_lease(lease_id).is_empty() {
            return LeaseStatus::Pending;
        }

        // If the lease has any Pending opportunistic claims, it is still Pending.
        if !self.catalog.opportunistic_claims.pending.for_lease(lease_id).is_empty() {
            return LeaseStatus::Pending;
        }

        // If the lease has any opportunistic claims that have not been satisfied
        // it is still Pending.
        let opportunistic_claims = self.catalog.opportunistic_claims.activated.for_lease(lease_id);
        for claim in opportunistic_claims {
            if !self.current_level_satisfies(claim.requires()) {
                return LeaseStatus::Pending;
            }
        }

        // If the lease is contingent on any opportunistic claims, it is Pending.
        // (The opportunistic claims could have been previously satisfied, but then
        // an assertive claim was subsequently dropped).
        if self.is_lease_contingent(lease_id) {
            return LeaseStatus::Pending;
        }

        // If the lease has any assertive claims that have not been satisfied
        // it is still Pending.
        for claim in self.catalog.assertive_claims.activated.for_lease(lease_id) {
            if !self.current_level_satisfies(claim.requires()) {
                return LeaseStatus::Pending;
            }
        }
        // All claims are assertive and satisfied, so the lease is Satisfied.
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
        // Lease has transitioned from satisfied to pending and contingent.
        if prev_status.as_ref() == Some(&LeaseStatus::Satisfied)
            && status == LeaseStatus::Pending
            && self.is_lease_contingent(lease_id)
        {
            // Mark all activated claims of this lease to be deactivated once
            // they are no longer in use.
            tracing::debug!(
                "drop(lease:{lease_id}): marking activated assertive claims to deactivate"
            );
            let assertive_claims_to_deactivate =
                self.catalog.assertive_claims.activated.mark_to_deactivate(&lease_id);
            self.update_required_levels(&element_ids_required_by_claims(
                &assertive_claims_to_deactivate,
            ));
            tracing::debug!(
                "drop(lease:{lease_id}): marking activated opportunistic claims to deactivate"
            );
            let opportunistic_claims_to_deactivate =
                self.catalog.opportunistic_claims.activated.mark_to_deactivate(&lease_id);
            self.update_required_levels(&element_ids_required_by_claims(
                &opportunistic_claims_to_deactivate,
            ));
            // Scan assertive claims of this lease. If those assertive claims have any
            // opportunistic claims sharing the required element-level of D's assertive claims,
            // then re-evaluate their lease to determine if it's newly pending.
            for assertive_claim in self.catalog.assertive_claims.activated.for_lease(lease_id) {
                let element_of_pending_lease = &assertive_claim.requires().element_id;
                for opportunistic_claim in self
                    .catalog
                    .opportunistic_claims
                    .activated
                    .for_required_element(element_of_pending_lease)
                {
                    if &opportunistic_claim.lease_id != lease_id
                        && assertive_claim
                            .requires()
                            .level
                            .satisfies(opportunistic_claim.requires().level)
                    {
                        self.update_lease_status(&opportunistic_claim.lease_id);
                    }
                }
            }
        }
        Some(status)
    }

    #[cfg(test)]
    pub fn get_lease_status(&self, lease_id: &LeaseID) -> Option<LeaseStatus> {
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
            self.update_required_level(element_id, new_required_level);
        }
    }

    /// Examines a Vec of pending assertive claims and activates each for which
    /// its lease is not contingent on its opportunistic claims and either the
    /// required element is already at the required level (and thus the claim
    /// is already satisfied) or all of the dependencies of its required
    /// ElementLevel are met. Updates required levels of affected elements.
    /// For example, let us imagine elements A, B, C and D where A depends on B
    /// and B depends on C and D. In order to activate the A->B claim, all
    /// dependencies of B (i.e. B->C and B->D) must first be satisfied.
    fn activate_assertive_claims_if_dependencies_satisfied(
        &mut self,
        pending_assertive_claims: Vec<Claim>,
    ) {
        tracing::debug!(
            "activate_assertive_claims_if_dependencies_satisfied: pending_assertive_claims[{}]",
            pending_assertive_claims.iter().join(", ")
        );
        // Skip any claims whose leases are still contingent on opportunistic claims.
        let contingent_lease_ids: HashSet<LeaseID> = pending_assertive_claims
            .iter()
            .filter(|c| self.is_lease_contingent(&c.lease_id))
            .map(|c| c.lease_id.clone())
            .collect();
        let claims_to_activate: Vec<Claim> = pending_assertive_claims
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
            self.catalog.assertive_claims.activate_claim(&claim.id);
        }
        self.update_required_levels(&element_ids_required_by_claims(&claims_to_activate));
    }

    /// Examines the direct assertive and opportunistic dependencies of an element level
    /// and returns true if they are all satisfied (current level >= required).
    fn all_dependencies_satisfied(&self, element_level: &ElementLevel) -> bool {
        let assertive_dependencies =
            self.catalog.topology.direct_assertive_dependencies(&element_level);
        let opportunistic_dependencies =
            self.catalog.topology.direct_opportunistic_dependencies(&element_level);
        assertive_dependencies.into_iter().chain(opportunistic_dependencies).all(|dep| {
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
                self.catalog.assertive_claims.activated.for_lease(&claim_to_check.lease_id)
            {
                if related_claim.requires() == claim_to_check.dependent() {
                    tracing::debug!(
                        "won't drop {claim_to_check}, has assertive dependent {related_claim}"
                    );
                    has_dependents = true;
                    break;
                }
            }
            if has_dependents {
                continue;
            }
            for related_claim in
                self.catalog.opportunistic_claims.activated.for_lease(&claim_to_check.lease_id)
            {
                if related_claim.requires() == claim_to_check.dependent() {
                    tracing::debug!(
                        "won't drop {claim_to_check}, has opportunistic dependent {related_claim}"
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

    /// Takes a Vec of assertive claims, deactivates them if their lease is open,
    /// or drops them if their lease has been dropped. Then updates lease
    /// status of leases affected and required levels of elements affected.
    fn drop_or_deactivate_assertive_claims(&mut self, claims: &Vec<Claim>) {
        for claim in claims {
            tracing::debug!("deactivate assertive claim: {claim}");
            if self.catalog.is_lease_dropped(&claim.lease_id) {
                self.catalog.assertive_claims.drop_claim(&claim.id);
            } else {
                self.catalog.assertive_claims.deactivate_claim(&claim.id);
            }
        }
        let element_ids_affected = element_ids_required_by_claims(claims);
        self.update_required_levels(&element_ids_affected);
        // Update the status of all leases with opportunistic claims no longer satisfied.
        let mut leases_affected = HashSet::new();
        for element_id in element_ids_affected {
            // Calculate the maximum level required by assertive claims only to
            // determine if there are still any assertive claims that would
            // satisfy these opportunistic claims.
            let max_required_by_assertive = max_level_required_by_claims(
                &self.catalog.assertive_claims.activated.for_required_element(element_id),
            )
            .unwrap_or(self.catalog.minimum_level(element_id));
            for opportunistic_claim in
                self.catalog.opportunistic_claims.activated.for_required_element(element_id)
            {
                if !max_required_by_assertive.satisfies(opportunistic_claim.requires().level) {
                    tracing::debug!("opportunistic_claim {opportunistic_claim} no longer satisfied, must reevaluate lease {}", opportunistic_claim.lease_id);
                    leases_affected.insert(opportunistic_claim.lease_id);
                }
            }
        }
        // These leases have opportunistic claims that are no longer satisfied,
        // so update_lease_status will update them to pending since they are
        // now contingent.
        for lease_id in leases_affected {
            self.update_lease_status(&lease_id);
        }
    }

    /// Takes a Vec of opportunistic claims, deactivates them if their lease is open,
    /// or drops them if their lease has been dropped. Then updates required
    /// levels of elements affected.
    fn drop_or_deactivate_opportunistic_claims(&mut self, claims: &Vec<Claim>) {
        for claim in claims {
            if self.catalog.is_lease_dropped(&claim.lease_id) {
                tracing::debug!("drop opportunistic claim: {claim}");
                self.catalog.opportunistic_claims.drop_claim(&claim.id);
            } else {
                tracing::debug!("deactivate opportunistic claim: {claim}");
                self.catalog.opportunistic_claims.deactivate_claim(&claim.id);
            }
        }
        self.update_required_levels(&element_ids_required_by_claims(claims));
    }

    pub fn add_element(
        &mut self,
        name: &str,
        initial_current_level: fpb::PowerLevel,
        valid_levels: Vec<fpb::PowerLevel>,
        level_dependencies: Vec<fpb::LevelDependency>,
        assertive_dependency_tokens: Vec<Token>,
        opportunistic_dependency_tokens: Vec<Token>,
    ) -> Result<ElementID, AddElementError> {
        if valid_levels.len() < 1 {
            return Err(AddElementError::Invalid);
        }
        let id = self.catalog.topology.add_element(name, valid_levels.to_vec())?;
        let initial_current_level = self
            .catalog
            .topology
            .get_level_index(&id, initial_current_level)
            .ok_or(AddElementError::Invalid)?;
        self.update_current_level_internal(&id, *initial_current_level);
        let minimum_level = self.catalog.topology.minimum_level(&id);
        self.update_required_level(&id, minimum_level);
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
        let labeled_dependency_tokens =
            assertive_dependency_tokens.into_iter().zip(repeat(DependencyType::Active)).chain(
                opportunistic_dependency_tokens.into_iter().zip(repeat(DependencyType::Passive)),
            );
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

    pub fn get_level_index(
        &self,
        element_id: &ElementID,
        level: fpb::PowerLevel,
    ) -> Option<&IndexedPowerLevel> {
        self.catalog.topology.get_level_index(element_id, level)
    }

    /// Checks authorization from requires_token, and if valid, adds an assertive
    /// or opportunistic dependency to the Topology, according to dependency_type.
    pub fn add_dependency(
        &mut self,
        element_id: &ElementID,
        dependency_type: DependencyType,
        dependent_level: fpb::PowerLevel,
        requires_token: Token,
        requires_level: fpb::PowerLevel,
    ) -> Result<(), ModifyDependencyError> {
        let Some(requires_cred) = self.lookup_credentials(requires_token) else {
            return Err(ModifyDependencyError::NotAuthorized);
        };
        let dependent_level = self
            .catalog
            .topology
            .get_level_index(&element_id, dependent_level)
            .ok_or(ModifyDependencyError::Invalid)?;
        let requires_level = self
            .catalog
            .topology
            .get_level_index(requires_cred.get_element(), requires_level)
            .ok_or(ModifyDependencyError::Invalid)?;
        let dependency = Dependency {
            dependent: ElementLevel { element_id: element_id.clone(), level: *dependent_level },
            requires: ElementLevel {
                element_id: requires_cred.get_element().clone(),
                level: *requires_level,
            },
        };
        match dependency_type {
            DependencyType::Active => {
                if !requires_cred.contains(Permissions::MODIFY_ACTIVE_DEPENDENT) {
                    return Err(ModifyDependencyError::NotAuthorized);
                }
                self.catalog.topology.add_assertive_dependency(&dependency)
            }
            DependencyType::Passive => {
                if !requires_cred.contains(Permissions::MODIFY_PASSIVE_DEPENDENT) {
                    return Err(ModifyDependencyError::NotAuthorized);
                }
                self.catalog.topology.add_opportunistic_dependency(&dependency)
            }
        }
    }
}

pub type LeaseID = String;

#[derive(Clone, Debug, Eq, Hash, Ord, PartialOrd, PartialEq)]
pub struct Lease {
    pub id: LeaseID,
    pub element_id: ElementID,
    pub level: IndexedPowerLevel,
}

impl Lease {
    fn new(element_id: &ElementID, level: IndexedPowerLevel) -> Self {
        let uuid = LeaseID::from(Uuid::new_v4().as_simple().to_string());
        let id = if ID_DEBUG_MODE { format!("{element_id}@{level}:{uuid:.6}") } else { uuid };
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
        write!(f, "Claim{{{}:{:.6}: {}}}", self.lease_id, self.id, self.dependency)
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
fn max_level_required_by_claims(claims: &Vec<Claim>) -> Option<IndexedPowerLevel> {
    claims.into_iter().map(|x| x.requires().level).max()
}

#[derive(Debug)]
struct Catalog {
    topology: Topology,
    leases: HashMap<LeaseID, Lease>,
    lease_status: SubscribeMap<LeaseID, LeaseStatus>,
    /// Assertive claims can be either Pending or Activated.
    /// Pending assertive claims do not yet affect the required levels of their
    /// required elements. Some dependencies of their required element are not
    /// satisfied.
    /// Activated assertive claims affect the required level of the claim's
    /// required element.
    /// Each assertive claim will start as Pending, and will be Activated once all
    /// dependencies of its required element are satisfied.
    assertive_claims: ClaimActivationTracker,
    ///Opportunistic claims can also be either Pending or Activated.
    /// Pending opportunistic claims do not affect the required levels of their
    /// required elements.
    /// Activated opportunistic claims will keep the required level of their
    /// required elements from dropping until the lease holder has a chance
    /// to drop the lease and release its claims.
    /// Each opportunistic claim will start as pending and will be activated when
    /// the current level of their required element satisfies their required
    /// level. In order for this to happen, an assertive claim from
    /// another lease must have been activated and then satisfied.
    opportunistic_claims: ClaimActivationTracker,
}

impl Catalog {
    fn new(inspect_parent: &INode) -> Self {
        Catalog {
            topology: Topology::new(
                inspect_parent.create_child("topology"),
                INSPECT_GRAPH_EVENT_BUFFER_SIZE,
            ),
            leases: HashMap::new(),
            lease_status: SubscribeMap::new(Some(inspect_parent.create_child("leases"))),
            assertive_claims: ClaimActivationTracker::new(),
            opportunistic_claims: ClaimActivationTracker::new(),
        }
    }

    fn minimum_level(&self, element_id: &ElementID) -> IndexedPowerLevel {
        self.topology.minimum_level(element_id)
    }

    /// Returns true if the lease was dropped (or never existed).
    fn is_lease_dropped(&self, lease_id: &LeaseID) -> bool {
        !self.leases.contains_key(lease_id)
    }

    /// Calculates the required level for each element, according to the
    /// Minimum Power Level Policy.
    /// The required level is equal to the maximum of all **activated** assertive
    /// or opportunistic claims on the element, the maximum level of all satisfied
    /// leases on the element, or the element's minimum level if there are
    /// no activated claims or satisfied leases.
    fn calculate_required_level(&self, element_id: &ElementID) -> IndexedPowerLevel {
        let minimum_level = self.minimum_level(element_id);
        let mut activated_claims = self.assertive_claims.activated.for_required_element(element_id);
        activated_claims.extend(
            self.opportunistic_claims.activated.for_required_element(element_id).into_iter(),
        );
        max(
            max_level_required_by_claims(&activated_claims).unwrap_or(minimum_level),
            self.calculate_level_required_by_leases(element_id).unwrap_or(minimum_level),
        )
    }

    /// Calculates the maximum level of all satisfied leases on the element.
    fn calculate_level_required_by_leases(
        &self,
        element_id: &ElementID,
    ) -> Option<IndexedPowerLevel> {
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

    // Given a set of assertive and opportunistic claims, filter out any redundant claims. A claim
    // is redundant if there exists another *assertive* claim between the *same pair of elements*
    // at an *equal or higher level*.
    fn filter_out_redundant_claims(
        &self,
        redundant_assertive_claims: &Vec<Claim>,
        redudnant_opportunistic_claims: &Vec<Claim>,
    ) -> (Vec<Claim>, Vec<Claim>) {
        let mut assertive_claims: Vec<Claim> = redundant_assertive_claims.clone();
        let mut opportunistic_claims: Vec<Claim> = redudnant_opportunistic_claims.clone();
        let mut essential_assertive_claims: Vec<Claim> = Vec::new();
        let mut essential_opportunistic_claims: Vec<Claim> = Vec::new();
        let mut observed_pairs: HashMap<(ElementID, ElementID), ElementLevel> = HashMap::new();
        assertive_claims.sort_unstable_by_key(|claim| {
            (
                claim.dependent().element_id.clone(),
                claim.requires().element_id.clone(),
                usize::MAX - claim.requires().level.index,
            )
        });
        opportunistic_claims.sort_unstable_by_key(|claim| {
            (
                claim.dependent().element_id.clone(),
                claim.requires().element_id.clone(),
                usize::MAX - claim.requires().level.index,
            )
        });
        for claim in assertive_claims {
            let element_pair =
                (claim.dependent().element_id.clone(), claim.requires().element_id.clone());
            if observed_pairs.contains_key(&element_pair) {
                continue;
            } else {
                observed_pairs.insert(element_pair, claim.requires().clone());
            }
            essential_assertive_claims.push(claim.clone());
        }
        for claim in opportunistic_claims {
            let element_pair =
                (claim.dependent().element_id.clone(), claim.requires().element_id.clone());
            if observed_pairs.contains_key(&element_pair) {
                if let Some(requires) = observed_pairs.get(&element_pair) {
                    if requires.level.satisfies(claim.requires().level) {
                        continue;
                    }
                }
            } else {
                observed_pairs.insert(element_pair, claim.requires().clone());
            }
            essential_opportunistic_claims.push(claim.clone());
        }
        (essential_assertive_claims, essential_opportunistic_claims)
    }

    /// Creates a new lease for the given element and level along with all
    /// claims necessary to satisfy this lease and adds them to pending_claims.
    /// Returns the new lease and the Vec of assertive (pending) claims created.
    fn create_lease_and_claims(
        &mut self,
        element_id: &ElementID,
        level: IndexedPowerLevel,
    ) -> (Lease, Vec<Claim>) {
        tracing::debug!("create_lease_and_claims({element_id}@{level})");
        // TODO: Add lease validation and control.
        let lease = Lease::new(&element_id, level);
        if let Ok(elem_inspect) = self.topology.inspect_for_element(element_id) {
            elem_inspect
                .borrow_mut()
                .meta()
                .set(format!("lease_{}", lease.id.clone()), format!("level_{}", level));
        }
        self.leases.insert(lease.id.clone(), lease.clone());
        let element_level = ElementLevel { element_id: element_id.clone(), level: level.clone() };
        let (assertive_dependencies, opportunistic_dependencies) =
            self.topology.all_assertive_and_opportunistic_dependencies(&element_level);
        // Create all possible claims from the assertive and opportunistic dependencies.
        let assertive_claims = assertive_dependencies
            .into_iter()
            .map(|dependency| Claim::new(dependency.clone(), &lease.id))
            .collect::<Vec<Claim>>();
        let opportunistic_claims = opportunistic_dependencies
            .into_iter()
            .map(|dependency| Claim::new(dependency.clone(), &lease.id))
            .collect::<Vec<Claim>>();
        // Filter claims down to only the essential (i.e. non-redundant) claims.
        let (essential_assertive_claims, essential_opportunistic_claims) =
            self.filter_out_redundant_claims(&assertive_claims, &opportunistic_claims);
        for claim in &essential_assertive_claims {
            self.assertive_claims.pending.add(claim.clone());
        }
        for claim in &essential_opportunistic_claims {
            self.opportunistic_claims.pending.add(claim.clone());
        }
        (lease, essential_assertive_claims)
    }

    /// Drops an existing lease, and initiates process of releasing all
    /// associated claims.
    /// Returns the dropped lease, a Vec of assertive claims marked to deactivate,
    /// and a Vec of opportunistic claims marked to deactivate.
    fn drop(&mut self, lease_id: &LeaseID) -> Result<(Lease, Vec<Claim>, Vec<Claim>), Error> {
        tracing::debug!("drop(lease:{lease_id})");
        let lease = self.leases.remove(lease_id).ok_or(anyhow!("{lease_id} not found"))?;
        self.lease_status.remove(lease_id);
        if let Ok(elem_inspect) = self.topology.inspect_for_element(&lease.element_id) {
            elem_inspect.borrow_mut().meta().remove(format!("lease_{}", lease.id.clone()).as_str());
        }
        tracing::debug!("dropping lease({:?})", &lease);
        // Pending claims should be dropped immediately.
        let pending_assertive_claims = self.assertive_claims.pending.for_lease(&lease.id);
        for claim in pending_assertive_claims {
            if let Some(removed) = self.assertive_claims.pending.remove(&claim.id) {
                tracing::debug!("removing pending claim: {:?}", &removed);
            } else {
                tracing::error!("cannot remove pending assertive claim: not found: {}", claim.id);
            }
        }
        let pending_opportunistic_claims = self.opportunistic_claims.pending.for_lease(&lease.id);
        for claim in pending_opportunistic_claims {
            if let Some(removed) = self.opportunistic_claims.pending.remove(&claim.id) {
                tracing::debug!("removing pending opportunistic claim: {:?}", &removed);
            } else {
                tracing::error!(
                    "cannot remove pending opportunistic claim: not found: {}",
                    claim.id
                );
            }
        }
        // Assertive andOpportunistic claims should be marked to deactivate in an orderly sequence.
        tracing::debug!("drop(lease:{lease_id}): marking activated assertive claims to deactivate");
        let assertive_claims_to_deactivate =
            self.assertive_claims.activated.mark_to_deactivate(&lease.id);
        tracing::debug!(
            "drop(lease:{lease_id}): marking activated opportunistic claims to deactivate"
        );
        let opportunistic_claims_to_deactivate =
            self.opportunistic_claims.activated.mark_to_deactivate(&lease.id);
        Ok((lease, assertive_claims_to_deactivate, opportunistic_claims_to_deactivate))
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

impl fmt::Display for ClaimActivationTracker {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "pending: [{}], activated: [{}]",
            self.pending.claims.values().join(", "),
            self.activated.claims.values().join(", ")
        )
    }
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

    /// Deactivates an activated claim, moving it to pending.
    fn deactivate_claim(&mut self, claim_id: &ClaimID) {
        tracing::debug!("deactivate_claim: {claim_id}");
        self.activated.move_to(&claim_id, &mut self.pending);
        self.activated.remove_from_claims_to_deactivate(&claim_id);
    }

    /// Removes a claim from both pending and activated.
    fn drop_claim(&mut self, claim_id: &ClaimID) {
        tracing::debug!("drop_claim: {claim_id}");
        self.pending.remove(&claim_id);
        self.activated.remove(&claim_id);
    }
}

#[derive(Debug)]
struct ClaimLookup {
    claims: HashMap<ClaimID, Claim>,
    claims_by_required_element_id: HashMap<ElementID, Vec<ClaimID>>,
    claims_by_lease: HashMap<LeaseID, Vec<ClaimID>>,
    claims_to_deactivate_by_element_id: HashMap<ElementID, Vec<ClaimID>>,
}

impl ClaimLookup {
    fn new() -> Self {
        Self {
            claims: HashMap::new(),
            claims_by_required_element_id: HashMap::new(),
            claims_by_lease: HashMap::new(),
            claims_to_deactivate_by_element_id: HashMap::new(),
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
        self.remove_from_claims_to_deactivate(id);
        Some(claim)
    }

    fn remove_from_claims_to_deactivate(&mut self, id: &ClaimID) {
        let Some(claim) = self.claims.remove(id) else {
            return;
        };
        if let Some(claim_ids) =
            self.claims_to_deactivate_by_element_id.get_mut(&claim.dependent().element_id)
        {
            claim_ids.retain(|x| x != id);
            if claim_ids.is_empty() {
                self.claims_to_deactivate_by_element_id.remove(&claim.dependent().element_id);
            }
        }
    }
    /// Marks all claims associated with a lease to deactivate.
    /// They will be deactivated in an orderly sequence (each claim will be
    /// deactivated only once all claims dependent on it have already been
    /// deactivated).
    /// Returns a Vec of Claims marked to drop.
    fn mark_to_deactivate(&mut self, lease_id: &LeaseID) -> Vec<Claim> {
        let claims_marked = self.for_lease(lease_id);
        tracing::debug!(
            "marking claims to deactivate for lease {lease_id}: [{}]",
            &claims_marked.iter().join(", ")
        );
        for claim in &claims_marked {
            self.claims_to_deactivate_by_element_id
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
    /// been dropped. See ClaimLookup::mark_to_deactivate for more details.
    fn marked_to_deactivate_for_element(&self, element_id: &ElementID) -> Vec<Claim> {
        let Some(claim_ids) = self.claims_to_deactivate_by_element_id.get(element_id) else {
            return Vec::new();
        };
        self.for_claim_ids(claim_ids)
    }
}

trait Inspectable {
    type Value;
    fn track_inspect_with(&self, value: Self::Value, parent: &INode) -> Box<dyn IType>;
}

impl Inspectable for &ElementID {
    type Value = IndexedPowerLevel;
    fn track_inspect_with(&self, value: Self::Value, parent: &INode) -> Box<dyn IType> {
        Box::new(parent.create_uint(self.to_string(), value.level.into()))
    }
}

impl Inspectable for &LeaseID {
    type Value = LeaseStatus;
    fn track_inspect_with(&self, value: Self::Value, parent: &INode) -> Box<dyn IType> {
        Box::new(parent.create_string(*self, format!("{:?}", value)))
    }
}

#[derive(Debug)]
struct Data<V: Clone + PartialEq> {
    value: Option<V>,
    senders: Vec<UnboundedSender<Option<V>>>,
    _inspect: Option<Box<dyn IType>>,
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
    inspect: Option<INode>,
}

impl<K: Clone + Hash + Eq, V: Clone + PartialEq> SubscribeMap<K, V> {
    fn new(inspect: Option<INode>) -> Self {
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
        let _inspect = self.inspect.as_mut().map(|inspect| key.track_inspect_with(value, &inspect));
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

/// A IndexedPowerLevel satisfies a required IndexedPowerLevel if it is
/// greater than or equal to it on the same scale.
trait SatisfyPowerLevel {
    fn satisfies(&self, required: IndexedPowerLevel) -> bool;
}

impl SatisfyPowerLevel for IndexedPowerLevel {
    fn satisfies(&self, required: IndexedPowerLevel) -> bool {
        self >= &required
    }
}

impl SatisfyPowerLevel for Option<IndexedPowerLevel> {
    fn satisfies(&self, required: IndexedPowerLevel) -> bool {
        self.is_some() && self.unwrap().satisfies(required)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use diagnostics_assertions::{assert_data_tree, AnyProperty};
    use fidl_fuchsia_power_broker::{BinaryPowerLevel, DependencyToken};
    use fuchsia_zircon::{self as zx, HandleBased};
    use lazy_static::lazy_static;
    use power_broker_client::BINARY_POWER_LEVELS;

    lazy_static! {
        static ref TOPOLOGY_UNSATISFIABLE_MAX_LEVEL: String =
            format!("{}p", IndexedPowerLevel::MAX.to_string());
    }

    // Convenience aliases.
    const OFF: IndexedPowerLevel =
        IndexedPowerLevel { level: BinaryPowerLevel::Off.into_primitive(), index: 0 };
    const ON: IndexedPowerLevel =
        IndexedPowerLevel { level: BinaryPowerLevel::On.into_primitive(), index: 1 };

    const ZERO: IndexedPowerLevel = IndexedPowerLevel::from_same_level_and_index(0);
    const ONE: IndexedPowerLevel = IndexedPowerLevel::from_same_level_and_index(1);
    const TWO: IndexedPowerLevel = IndexedPowerLevel::from_same_level_and_index(2);
    const THREE: IndexedPowerLevel = IndexedPowerLevel::from_same_level_and_index(3);

    #[track_caller]
    fn assert_lease_cleaned_up(catalog: &Catalog, lease_id: &LeaseID) {
        assert!(!catalog.leases.contains_key(lease_id));
        assert!(catalog.lease_status.get(lease_id).is_none());
        assert!(catalog.assertive_claims.activated.for_lease(lease_id).is_empty());
        assert!(catalog.assertive_claims.pending.for_lease(lease_id).is_empty());
        assert!(catalog.opportunistic_claims.activated.for_lease(lease_id).is_empty());
        assert!(catalog.opportunistic_claims.pending.for_lease(lease_id).is_empty());
    }

    #[fuchsia::test]
    fn test_binary_satisfy_power_level() {
        for (level, required, want) in
            [(OFF, ON, false), (OFF, OFF, true), (ON, OFF, true), (ON, ON, true)]
        {
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
            let level = IndexedPowerLevel::from_same_level_and_index(level);
            let required = IndexedPowerLevel::from_same_level_and_index(required);
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
            let level = level.map(|l| IndexedPowerLevel::from_same_level_and_index(l));
            let required = IndexedPowerLevel::from_same_level_and_index(required);
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
        let mut levels = SubscribeMap::<ElementID, IndexedPowerLevel>::new(None);

        levels.update(&"A".into(), ON);
        assert_eq!(levels.get(&"A".into()), Some(ON));
        assert_eq!(levels.get(&"B".into()), None);

        levels.update(&"A".into(), OFF);
        levels.update(&"B".into(), ON);
        assert_eq!(levels.get(&"A".into()), Some(OFF));
        assert_eq!(levels.get(&"B".into()), Some(ON));

        levels.update(&"UD1".into(), IndexedPowerLevel::from_same_level_and_index(145));
        assert_eq!(
            levels.get(&"UD1".into()),
            Some(IndexedPowerLevel::from_same_level_and_index(145))
        );
        assert_eq!(levels.get(&"UD2".into()), None);

        levels.update(&"A".into(), ON);
        levels.remove(&"B".into());
        assert_eq!(levels.get(&"B".into()), None);
    }

    #[fuchsia::test]
    fn test_levels_subscribe() {
        let mut levels = SubscribeMap::<ElementID, IndexedPowerLevel>::new(None);

        let mut receiver_a = levels.subscribe(&"A".into());
        let mut receiver_b = levels.subscribe(&"B".into());

        levels.update(&"A".into(), ON);
        assert_eq!(levels.get(&"A".into()), Some(ON));
        assert_eq!(levels.get(&"B".into()), None);

        levels.update(&"A".into(), OFF);
        levels.update(&"B".into(), ON);
        assert_eq!(levels.get(&"A".into()), Some(OFF));
        assert_eq!(levels.get(&"B".into()), Some(ON));

        let mut received_a = Vec::new();
        while let Ok(Some(level)) = receiver_a.try_next() {
            received_a.push(level)
        }
        assert_eq!(received_a, vec![None, Some(ON), Some(OFF)]);
        let mut received_b = Vec::new();
        while let Ok(Some(level)) = receiver_b.try_next() {
            received_b.push(level)
        }
        assert_eq!(received_b, vec![None, Some(ON)]);
    }

    fn create_test_claim(
        dependent_element_id: ElementID,
        dependent_element_level: fpb::PowerLevel,
        requires_element_id: ElementID,
        requires_element_level: fpb::PowerLevel,
    ) -> Claim {
        Claim::new(
            Dependency {
                dependent: ElementLevel {
                    element_id: dependent_element_id,
                    level: IndexedPowerLevel::from_same_level_and_index(dependent_element_level),
                },
                requires: ElementLevel {
                    element_id: requires_element_id,
                    level: IndexedPowerLevel::from_same_level_and_index(requires_element_level),
                },
            },
            &LeaseID::new(),
        )
    }

    #[fuchsia::test]
    fn test_filter_out_redundant_claims() {
        let inspect = fuchsia_inspect::component::inspector();
        let inspect_node = inspect.root().create_child("test");
        let broker = Broker::new(inspect_node);

        let claim_a_1_b_1 = create_test_claim("A".into(), 1, "B".into(), 1);
        let claim_a_2_b_2 = create_test_claim("A".into(), 2, "B".into(), 2);
        let claim_a_1_c_1 = create_test_claim("A".into(), 1, "C".into(), 1);
        let claim_b_1_c_1 = create_test_claim("B".into(), 1, "C".into(), 1);
        let claim_a_2_c_2 = create_test_claim("A".into(), 2, "C".into(), 2);

        //  A     B
        //  1 ==> 1 (redundant with A@2=>B@2)
        //  2 ==> 2
        let (essential_assertive_claims, essential_opportunistic_claims) =
            broker.catalog.filter_out_redundant_claims(
                &vec![claim_a_1_b_1.clone(), claim_a_2_b_2.clone()],
                &vec![],
            );
        assert_eq!(essential_assertive_claims, vec![claim_a_2_b_2.clone()]);
        assert_eq!(essential_opportunistic_claims, vec![]);

        //  A     B
        //  1 --> 1 (redundant with A@2=>B@2)
        //  2 ==> 2
        let (essential_assertive_claims, essential_opportunistic_claims) =
            broker.catalog.filter_out_redundant_claims(
                &vec![claim_a_2_b_2.clone()],
                &vec![claim_a_1_b_1.clone()],
            );
        assert_eq!(essential_assertive_claims, vec![claim_a_2_b_2.clone()]);
        assert_eq!(essential_opportunistic_claims, vec![]);

        //  A     B
        //  1 ==> 1 (not redundant, opportunistic claims cannot satisfy assertive claims)
        //  2 --> 2
        let (essential_assertive_claims, essential_opportunistic_claims) =
            broker.catalog.filter_out_redundant_claims(
                &vec![claim_a_1_b_1.clone()],
                &vec![claim_a_2_b_2.clone()],
            );
        assert_eq!(essential_assertive_claims, vec![claim_a_1_b_1.clone()]);
        assert_eq!(essential_opportunistic_claims, vec![claim_a_2_b_2.clone()]);

        //  A     B
        //  1 --> 1 (redundant with A@2->B@2)
        //  2 --> 2
        let (essential_assertive_claims, essential_opportunistic_claims) =
            broker.catalog.filter_out_redundant_claims(
                &vec![],
                &vec![claim_a_1_b_1.clone(), claim_a_2_b_2.clone()],
            );
        assert_eq!(essential_assertive_claims, vec![]);
        assert_eq!(essential_opportunistic_claims, vec![claim_a_2_b_2.clone()]);

        //  A     B     C
        //  1 ========> 1 (not redundant, not between same elements)
        //  2 ==> 2
        let (essential_assertive_claims, essential_opportunistic_claims) =
            broker.catalog.filter_out_redundant_claims(
                &vec![claim_a_1_c_1.clone(), claim_a_2_b_2.clone()],
                &vec![],
            );
        assert_eq!(essential_assertive_claims, vec![claim_a_2_b_2.clone(), claim_a_1_c_1.clone()]);
        assert_eq!(essential_opportunistic_claims, vec![]);

        //  A     B     C
        //  1 --------> 1 (not redundant, not between same elements)
        //  2 --> 2
        let (essential_assertive_claims, essential_opportunistic_claims) =
            broker.catalog.filter_out_redundant_claims(
                &vec![],
                &vec![claim_a_1_c_1.clone(), claim_a_2_b_2.clone()],
            );
        assert_eq!(essential_assertive_claims, vec![]);
        assert_eq!(
            essential_opportunistic_claims,
            vec![claim_a_2_b_2.clone(), claim_a_1_c_1.clone()]
        );

        //  A     B     C
        //  1 ==> 1 ==> 1 (not redundant, A@2=>C@2 cannot satisfy B@1=>C@1, not between same elements)
        //  2 ========> 2
        let (essential_assertive_claims, essential_opportunistic_claims) =
            broker.catalog.filter_out_redundant_claims(
                &vec![claim_a_1_b_1.clone(), claim_b_1_c_1.clone(), claim_a_2_c_2.clone()],
                &vec![],
            );
        assert_eq!(
            essential_assertive_claims,
            vec![claim_a_1_b_1.clone(), claim_a_2_c_2.clone(), claim_b_1_c_1.clone()]
        );
        assert_eq!(essential_opportunistic_claims, vec![]);

        //  A     B     C
        //  1 ==> 1 --> 1 (not redundant, A@2=>C@2 - B@1->C@1, not between same elements)
        //  2 ========> 2
        let (essential_assertive_claims, essential_opportunistic_claims) =
            broker.catalog.filter_out_redundant_claims(
                &vec![claim_a_1_b_1.clone(), claim_a_2_c_2.clone()],
                &vec![claim_b_1_c_1.clone()],
            );
        assert_eq!(essential_assertive_claims, vec![claim_a_1_b_1.clone(), claim_a_2_c_2.clone()]);
        assert_eq!(essential_opportunistic_claims, vec![claim_b_1_c_1.clone()]);

        //  A     B     C
        //  1 ==> 1 ==> 1 (not redundant, A@2->C@2 - B@1=>C@1, not between same elements)
        //  2 --------> 2
        let (essential_assertive_claims, essential_opportunistic_claims) =
            broker.catalog.filter_out_redundant_claims(
                &vec![claim_a_1_b_1.clone(), claim_b_1_c_1.clone()],
                &vec![claim_a_2_c_2.clone()],
            );
        assert_eq!(essential_assertive_claims, vec![claim_a_1_b_1.clone(), claim_b_1_c_1.clone()]);
        assert_eq!(essential_opportunistic_claims, vec![claim_a_2_c_2.clone()]);

        //  A     B     C
        //  1 ==> 1 --> 1 (not redundant, A@2->C@2 - B@1->C@1, not between same elements)
        //  2 --------> 2
        let (essential_assertive_claims, essential_opportunistic_claims) =
            broker.catalog.filter_out_redundant_claims(
                &vec![claim_a_1_b_1.clone()],
                &vec![claim_b_1_c_1.clone(), claim_a_2_c_2.clone()],
            );
        assert_eq!(essential_assertive_claims, vec![claim_a_1_b_1.clone()]);
        assert_eq!(
            essential_opportunistic_claims,
            vec![claim_a_2_c_2.clone(), claim_b_1_c_1.clone()]
        );
    }

    #[fuchsia::test]
    fn test_initialize_current_and_broker_status() {
        let inspect = fuchsia_inspect::component::inspector();
        let inspect_node = inspect.root().create_child("test");
        let mut broker = Broker::new(inspect_node);
        let latinum = broker
            .add_element(
                "Latinum",
                7,
                // Unsorted. The order declares the order of increasing power.
                vec![5, 2, 7],
                vec![],
                vec![],
                vec![],
            )
            .expect("add_element failed");
        assert_eq!(
            broker.get_current_level(&latinum),
            Some(IndexedPowerLevel { level: 7, index: 2 })
        );
        assert_eq!(
            broker.get_required_level(&latinum),
            Some(IndexedPowerLevel { level: 5, index: 0 })
        );

        assert_data_tree!(inspect, root: {
            test: {
                leases: {},
                topology: {
                    "fuchsia.inspect.Graph": {
                        topology: {
                            broker.get_unsatisfiable_element_id().to_string() => {
                                meta: {
                                    name: broker.get_unsatisfiable_element_name(),
                                    valid_levels: broker.get_unsatisfiable_element_levels(),
                                    required_level: "unset",
                                    current_level: "unset",
                                },
                                relationships: {}
                            },
                            latinum.to_string() => {
                                meta: {
                                    name: "Latinum",
                                    valid_levels: vec![5u64, 2u64, 7u64],
                                    current_level: 7u64,
                                    required_level: 5u64,
                                },
                                relationships: {},
                            },
                        },
                        "events": {
                            "0": {
                                "@time": AnyProperty,
                                "vertex_id": broker.get_unsatisfiable_element_id().to_string(),
                                "event": "add_vertex",
                                "meta": {
                                    "current_level": "unset",
                                    "required_level": "unset",
                                },
                            },
                            "1": {
                                "@time": AnyProperty,
                                "vertex_id": latinum.to_string(),
                                "event": "add_vertex",
                                "meta": {
                                    "current_level": "unset",
                                    "required_level": "unset",
                                },
                            },
                            "2": {
                                "@time": AnyProperty,
                                "vertex_id": latinum.to_string(),
                                "event": "update_key",
                                "key": "current_level",
                                "update": 7u64,
                            },
                            "3": {
                                "@time": AnyProperty,
                                "vertex_id": latinum.to_string(),
                                "event": "update_key",
                                "key": "required_level",
                                "update": 5u64,
                            },
                        },
        }}}});
    }

    #[fuchsia::test]
    fn test_current_required_level_inspect() {
        let inspect = fuchsia_inspect::component::inspector();
        let inspect_node = inspect.root().create_child("test");
        let mut broker = Broker::new(inspect_node);
        let latinum = broker
            .add_element("Latinum", 2, vec![0, 1, 2], vec![], vec![], vec![])
            .expect("add_element failed");
        assert_eq!(broker.get_current_level(&latinum), Some(TWO));
        assert_eq!(broker.get_required_level(&latinum), Some(ZERO));

        // Update required level to 1.
        broker.update_required_level(&latinum, ONE);

        // Update required level to 1 again, should have no additional effect.
        broker.update_required_level(&latinum, ONE);

        // Update current level to 1.
        broker.update_current_level(&latinum, ONE);

        // Update current level to 1 again, should have no additional effect.
        broker.update_current_level(&latinum, ONE);

        assert_data_tree!(inspect, root: {
            test: {
                leases: {},
                topology: {
                    "fuchsia.inspect.Graph": {
                        topology: {
                            broker.get_unsatisfiable_element_id().to_string() => {
                                meta: {
                                    name: broker.get_unsatisfiable_element_name(),
                                    valid_levels: broker.get_unsatisfiable_element_levels(),
                                    required_level: "unset",
                                    current_level: "unset",
                                },
                                relationships: {}
                            },
                            latinum.to_string() => {
                                meta: {
                                    name: "Latinum",
                                    valid_levels: vec![0u64, 1u64, 2u64],
                                    current_level: 1u64,
                                    required_level: 1u64,
                                },
                                relationships: {},
                            },
                        },
                        "events": {
                            "0": {
                                "@time": AnyProperty,
                                "vertex_id": broker.get_unsatisfiable_element_id().to_string(),
                                "event": "add_vertex",
                                "meta": {
                                    "current_level": "unset",
                                    "required_level": "unset",
                                },
                            },
                            "1": {
                                "@time": AnyProperty,
                                "vertex_id": latinum.to_string(),
                                "event": "add_vertex",
                                "meta": {
                                    "current_level": "unset",
                                    "required_level": "unset",
                                },
                            },
                            "2": {
                                "@time": AnyProperty,
                                "vertex_id": latinum.to_string(),
                                "event": "update_key",
                                "key": "current_level",
                                "update": 2u64,
                            },
                            "3": {
                                "@time": AnyProperty,
                                "vertex_id": latinum.to_string(),
                                "event": "update_key",
                                "key": "required_level",
                                "update": 0u64,
                            },
                            "4": {
                                "@time": AnyProperty,
                                "vertex_id": latinum.to_string(),
                                "event": "update_key",
                                "key": "required_level",
                                "update": 1u64,
                            },
                            "5": {
                                "@time": AnyProperty,
                                "vertex_id": latinum.to_string(),
                                "event": "update_key",
                                "key": "current_level",
                                "update": 1u64,
                            },
                        },
        }}}});
    }

    #[fuchsia::test]
    fn test_add_element_dependency_never_and_unregistered() {
        let inspect = fuchsia_inspect::component::inspector();
        let inspect_node = inspect.root().create_child("test");
        let mut broker = Broker::new(inspect_node);
        let token_mithril = DependencyToken::create();
        let never_registered_token = DependencyToken::create();
        let mithril = broker
            .add_element(
                "Mithril",
                OFF.level,
                BINARY_POWER_LEVELS.to_vec(),
                vec![],
                vec![token_mithril
                    .duplicate_handle(zx::Rights::SAME_RIGHTS)
                    .expect("dup failed")
                    .into()],
                vec![],
            )
            .expect("add_element failed");
        let v01: Vec<u64> = BINARY_POWER_LEVELS.iter().map(|&v| v as u64).collect();
        assert_data_tree!(inspect, root: {
            test: {
                leases: {},
                topology: {
                    "fuchsia.inspect.Graph": {
                        topology: {
                            broker.get_unsatisfiable_element_id().to_string() => {
                                meta: {
                                    name: broker.get_unsatisfiable_element_name(),
                                    valid_levels: broker.get_unsatisfiable_element_levels(),
                                    required_level: "unset",
                                    current_level: "unset",
                                },
                                relationships: {}
                            },
                            mithril.to_string() => {
                                meta: {
                                    name: "Mithril",
                                    valid_levels: v01.clone(),
                                    current_level: OFF.level as u64,
                                    required_level: OFF.level as u64,
                                },
                                relationships: {},
                            },
                        },
                        "events": contains {},
                    },
                },
        }});

        // This should fail, because the token was never registered.
        let add_element_not_authorized_res = broker.add_element(
            "Silver",
            OFF.level,
            BINARY_POWER_LEVELS.to_vec(),
            vec![fpb::LevelDependency {
                dependency_type: DependencyType::Active,
                dependent_level: ON.level,
                requires_token: never_registered_token
                    .duplicate_handle(zx::Rights::SAME_RIGHTS)
                    .expect("dup failed"),
                requires_level: ON.level,
            }],
            vec![],
            vec![],
        );
        assert!(matches!(add_element_not_authorized_res, Err(AddElementError::NotAuthorized)));

        // Add element with a valid token should succeed.
        let silver = broker
            .add_element(
                "Silver",
                OFF.level,
                BINARY_POWER_LEVELS.to_vec(),
                vec![fpb::LevelDependency {
                    dependency_type: DependencyType::Active,
                    dependent_level: ON.level,
                    requires_token: token_mithril
                        .duplicate_handle(zx::Rights::SAME_RIGHTS)
                        .expect("dup failed"),
                    requires_level: ON.level,
                }],
                vec![],
                vec![],
            )
            .expect("add_element failed");
        assert_data_tree!(inspect, root: {
            test: {
                leases: {},
                topology: {
                    "fuchsia.inspect.Graph": {
                        topology: {
                            broker.get_unsatisfiable_element_id().to_string() => {
                                meta: {
                                    name: broker.get_unsatisfiable_element_name(),
                                    valid_levels: broker.get_unsatisfiable_element_levels(),
                                    required_level: "unset",
                                    current_level: "unset",
                                },
                                relationships: {}
                            },
                            mithril.to_string() => {
                                meta: {
                                    name: "Mithril",
                                    valid_levels: v01.clone(),
                                    current_level: OFF.level as u64,
                                    required_level: OFF.level as u64,
                                },
                                relationships: {},
                            },
                            silver.to_string() => {
                                meta: {
                                    name: "Silver",
                                    valid_levels: v01.clone(),
                                    current_level: OFF.level as u64,
                                    required_level: OFF.level as u64,
                                },
                                relationships: {
                                    mithril.to_string() => {
                                        edge_id: AnyProperty,
                                        meta: { "1": "1" },
                                    },
                                },
                            },
                        },
                        "events": contains {},
        }}}});

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
                OFF.level,
                BINARY_POWER_LEVELS.to_vec(),
                vec![fpb::LevelDependency {
                    dependency_type: DependencyType::Active,
                    dependent_level: ON.level,
                    requires_token: token_mithril
                        .duplicate_handle(zx::Rights::SAME_RIGHTS)
                        .expect("dup failed"),
                    requires_level: ON.level,
                }],
                vec![],
                vec![],
            );
        assert!(matches!(add_element_not_authorized_res, Err(AddElementError::NotAuthorized)));
    }

    #[fuchsia::test]
    fn test_remove_element() {
        let inspect = fuchsia_inspect::component::inspector();
        let inspect_node = inspect.root().create_child("test");
        let mut broker = Broker::new(inspect_node);
        let unobtanium = broker
            .add_element(
                "Unobtainium",
                OFF.level,
                BINARY_POWER_LEVELS.to_vec(),
                vec![],
                vec![],
                vec![],
            )
            .expect("add_element failed");
        assert_eq!(broker.element_exists(&unobtanium), true);
        let v01: Vec<u64> = BINARY_POWER_LEVELS.iter().map(|&v| v as u64).collect();
        assert_data_tree!(inspect, root: {
            test: {
                leases: {},
                topology: {
                    "fuchsia.inspect.Graph": {
                        topology: {
                            broker.get_unsatisfiable_element_id().to_string() => {
                                meta: {
                                    name: broker.get_unsatisfiable_element_name(),
                                    valid_levels: broker.get_unsatisfiable_element_levels(),
                                    required_level: "unset",
                                    current_level: "unset",
                                },
                                relationships: {}
                            },
                            unobtanium.to_string() => {
                                meta: {
                                    name: "Unobtainium",
                                    valid_levels: v01.clone(),
                                    current_level: OFF.level as u64,
                                    required_level: OFF.level as u64,
                                },
                                relationships: {},
                            },
                        },
                        "events": {
                            "0": {
                                "@time": AnyProperty,
                                "vertex_id": broker.get_unsatisfiable_element_id().to_string(),
                                "event": "add_vertex",
                                "meta": {
                                    "current_level": "unset",
                                    "required_level": "unset",
                                },
                            },
                            "1": {
                                "@time": AnyProperty,
                                "vertex_id": unobtanium.to_string(),
                                "event": "add_vertex",
                                "meta": {
                                    "current_level": "unset",
                                    "required_level": "unset",
                                },
                            },
                            "2": {
                                "@time": AnyProperty,
                                "vertex_id": unobtanium.to_string(),
                                "event": "update_key",
                                "key": "current_level",
                                "update": OFF.level as u64,
                            },
                            "3": {
                                "@time": AnyProperty,
                                "vertex_id": unobtanium.to_string(),
                                "event": "update_key",
                                "key": "required_level",
                                "update": OFF.level as u64,
                            },
                        },
                    },
        }}});

        broker.remove_element(&unobtanium);
        assert_eq!(broker.element_exists(&unobtanium), false);
        assert_data_tree!(inspect, root: {
            test: {
                leases: {},
                topology: {
                    "fuchsia.inspect.Graph": {
                        topology: {
                            broker.get_unsatisfiable_element_id().to_string() => {
                                meta: {
                                    name: broker.get_unsatisfiable_element_name(),
                                    valid_levels: broker.get_unsatisfiable_element_levels(),
                                    required_level: "unset",
                                    current_level: "unset",
                                },
                                relationships: {}
                            },
                        },
                        "events": {
                            "0": {
                                "@time": AnyProperty,
                                "vertex_id": broker.get_unsatisfiable_element_id().to_string(),
                                "event": "add_vertex",
                                "meta": {
                                    "current_level": "unset",
                                    "required_level": "unset",
                                },
                            },
                            "1": {
                                "@time": AnyProperty,
                                "vertex_id": unobtanium.to_string(),
                                "event": "add_vertex",
                                "meta": {
                                    "current_level": "unset",
                                    "required_level": "unset",
                                },
                            },
                            "2": {
                                "@time": AnyProperty,
                                "vertex_id": unobtanium.to_string(),
                                "event": "update_key",
                                "key": "current_level",
                                "update": OFF.level as u64,
                            },
                            "3": {
                                "@time": AnyProperty,
                                "vertex_id": unobtanium.to_string(),
                                "event": "update_key",
                                "key": "required_level",
                                "update": OFF.level as u64,
                            },
                            "4": {
                                "@time": AnyProperty,
                                "vertex_id": unobtanium.to_string(),
                                "event": "remove_vertex",
                            },
                        },
        }}}});
    }

    struct BrokerStatusMatcher {
        lease: LeaseMatcher,
        required_level: RequiredLevelMatcher,
    }

    impl BrokerStatusMatcher {
        fn new() -> Self {
            Self { lease: LeaseMatcher::new(), required_level: RequiredLevelMatcher::new() }
        }

        #[track_caller]
        fn assert_matches(&self, broker: &Broker) {
            self.lease.assert_matches(broker);
            self.required_level.assert_matches(broker);
        }
    }

    struct RequiredLevelMatcher {
        elements: HashMap<ElementID, IndexedPowerLevel>,
    }

    impl RequiredLevelMatcher {
        fn new() -> Self {
            Self { elements: HashMap::new() }
        }

        fn update(&mut self, id: &ElementID, required_level: IndexedPowerLevel) {
            self.elements.insert(id.clone(), required_level);
        }

        #[track_caller]
        fn assert_matches(&self, broker: &Broker) {
            for (id, expected) in &self.elements {
                let rl = broker.get_required_level(id).unwrap();
                assert_eq!(rl, *expected, "get_required_level({id}) = {rl}, expected = {expected}");
            }
        }
    }

    struct LeaseMatcher {
        leases: HashMap<LeaseID, (LeaseStatus, bool)>,
    }

    impl LeaseMatcher {
        fn new() -> Self {
            Self { leases: HashMap::new() }
        }

        fn update(&mut self, id: &LeaseID, status: LeaseStatus, contingent: bool) {
            self.leases.insert(id.clone(), (status, contingent));
        }

        fn remove(&mut self, id: &LeaseID) {
            self.leases.remove(id);
        }

        #[track_caller]
        fn assert_matches(&self, broker: &Broker) {
            for (id, (expected_status, expected_contingent)) in &self.leases {
                let status = broker.get_lease_status(id).unwrap();
                let contingent = broker.is_lease_contingent(id);
                assert_eq!(
                    status, *expected_status,
                    "get_lease_status({id}) = {status:?}, expected = {expected_status:?}"
                );
                assert_eq!(
                    contingent, *expected_contingent,
                    "is_lease_contingent({id}) = {contingent}, expected = {expected_contingent}"
                );
            }
        }
    }

    #[fuchsia::test]
    fn test_broker_lease_direct() {
        // Create a topology of a child element with two direct assertive dependencies.
        // P1 <= C => P2
        let inspect = fuchsia_inspect::component::inspector();
        let inspect_node = inspect.root().create_child("test");
        let mut broker = Broker::new(inspect_node);
        let parent1_token = DependencyToken::create();
        let parent1: ElementID = broker
            .add_element(
                "P1",
                OFF.level,
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
                OFF.level,
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
                OFF.level,
                BINARY_POWER_LEVELS.to_vec(),
                vec![
                    fpb::LevelDependency {
                        dependency_type: DependencyType::Active,
                        dependent_level: ON.level,
                        requires_token: parent1_token
                            .duplicate_handle(zx::Rights::SAME_RIGHTS)
                            .expect("dup failed"),
                        requires_level: ON.level,
                    },
                    fpb::LevelDependency {
                        dependency_type: DependencyType::Active,
                        dependent_level: ON.level,
                        requires_token: parent2_token
                            .duplicate_handle(zx::Rights::SAME_RIGHTS)
                            .expect("dup failed"),
                        requires_level: ON.level,
                    },
                ],
                vec![],
                vec![],
            )
            .expect("add_element failed");

        let mut broker_status = BrokerStatusMatcher::new();

        // All elements should start with required level OFF.
        broker_status.required_level.update(&parent1, OFF);
        broker_status.required_level.update(&parent2, OFF);
        broker_status.required_level.update(&child, OFF);
        broker_status.assert_matches(&broker);

        let v01: Vec<u64> = BINARY_POWER_LEVELS.iter().map(|&v| v as u64).collect();
        assert_data_tree!(inspect, root: {
            test: {
                leases: {},
                topology: {
                    "fuchsia.inspect.Graph": {
                        topology: {
                            broker.get_unsatisfiable_element_id().to_string() => {
                                meta: {
                                    name: broker.get_unsatisfiable_element_name(),
                                    valid_levels: broker.get_unsatisfiable_element_levels(),
                                    required_level: "unset",
                                    current_level: "unset",
                                },
                                relationships: {}
                            },
                            parent1.to_string() => {
                                meta: {
                                    name: "P1",
                                    valid_levels: v01.clone(),
                                    current_level: OFF.level as u64,
                                    required_level: OFF.level as u64,
                                },
                                relationships: {},
                            },
                            parent2.to_string() => {
                                meta: {
                                    name: "P2",
                                    valid_levels: v01.clone(),
                                    current_level: OFF.level as u64,
                                    required_level: OFF.level as u64,
                                },
                                relationships: {},
                            },
                            child.to_string() => {
                                meta: {
                                    name: "C",
                                    valid_levels: v01.clone(),
                                    current_level: OFF.level as u64,
                                    required_level: OFF.level as u64,
                                },
                                relationships: {
                                    parent1.to_string() => {
                                        edge_id: AnyProperty,
                                        meta: { "1": "1" },
                                    },
                                    parent2.to_string() => {
                                        edge_id: AnyProperty,
                                        meta: { "1": "1" },
                                    },
                                },
                            },
                        },
                        "events": contains {},
                    },
        }}});

        // Acquire the lease, which should result in two claims, one
        // for each dependency.
        let lease = broker.acquire_lease(&child, ON).expect("acquire failed");
        // Parent 1's required level should become ON from direct claim
        broker_status.required_level.update(&parent1, ON);
        // Parent 2's required level should become ON from direct claim
        broker_status.required_level.update(&parent2, ON);
        // The lease should be Pending.
        broker_status.lease.update(&lease.id, LeaseStatus::Pending, false);
        broker_status.assert_matches(&broker);

        // Update P1's current level to ON.
        // The lease should still be Pending.
        broker.update_current_level(&parent1, ON);
        // No required levels should change.
        broker_status.assert_matches(&broker);

        // Update P2's current level to ON.
        // The lease should now be Satisfied.
        broker.update_current_level(&parent2, ON);
        // Child's required level should become ON
        broker_status.required_level.update(&child, ON);
        broker_status.lease.update(&lease.id, LeaseStatus::Satisfied, false);
        broker_status.assert_matches(&broker);
        assert_data_tree!(inspect, root: {
            test: {
                leases: {
                    lease.id.clone() => "Satisfied",
                },
                topology: {
                    "fuchsia.inspect.Graph": {
                        topology: {
                            broker.get_unsatisfiable_element_id().to_string() => {
                                meta: {
                                    name: broker.get_unsatisfiable_element_name(),
                                    valid_levels: broker.get_unsatisfiable_element_levels(),
                                    required_level: "unset",
                                    current_level: "unset",
                                },
                                relationships: {}
                            },
                            parent1.to_string() => {
                                meta: {
                                    name: "P1",
                                    valid_levels: v01.clone(),
                                    current_level: ON.level as u64,
                                    required_level: ON.level as u64,
                                },
                                relationships: {},
                            },
                            parent2.to_string() => {
                                meta: {
                                    name: "P2",
                                    valid_levels: v01.clone(),
                                    current_level: ON.level as u64,
                                    required_level: ON.level as u64,
                                },
                                relationships: {},
                            },
                            child.to_string() => {
                                meta: {
                                    name: "C",
                                    valid_levels: v01.clone(),
                                    current_level: OFF.level as u64,
                                    required_level: ON.level as u64,
                                    format!("lease_{}", lease.id.clone()) => "level_1",
                                },
                                relationships: {
                                    parent1.to_string() => {
                                        edge_id: AnyProperty,
                                        meta: { "1": "1" },
                                    },
                                    parent2.to_string() => {
                                        edge_id: AnyProperty,
                                        meta: { "1": "1" },
                                    },
                                },
                            },
                        },
                        "events": contains {},
        }}}});

        // Update Child's current level to ON.
        broker.update_current_level(&child, ON);
        // No required levels should change.
        broker_status.assert_matches(&broker);

        // Now drop the lease and verify both claims are also dropped.
        broker.drop_lease(&lease.id).expect("drop failed");
        // Parent 1's required level should become OFF from dropped claim
        broker_status.required_level.update(&parent1, OFF);
        // Parent 2's required level should become OFF from dropped claim
        broker_status.required_level.update(&parent2, OFF);
        // Child's required level should become OFF
        broker_status.required_level.update(&child, OFF);
        broker_status.lease.remove(&lease.id);
        broker_status.assert_matches(&broker);

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
        let inspect = fuchsia_inspect::component::inspector();
        let inspect_node = inspect.root().create_child("test");
        let mut broker = Broker::new(inspect_node);
        let grandparent_token = DependencyToken::create();
        let grandparent: ElementID = broker
            .add_element(
                "GP",
                OFF.level,
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
                OFF.level,
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
                OFF.level,
                BINARY_POWER_LEVELS.to_vec(),
                vec![fpb::LevelDependency {
                    dependency_type: DependencyType::Active,
                    dependent_level: ON.level,
                    requires_token: parent_token
                        .duplicate_handle(zx::Rights::SAME_RIGHTS)
                        .expect("dup failed"),
                    requires_level: ON.level,
                }],
                vec![],
                vec![],
            )
            .expect("add_element failed");
        broker
            .add_dependency(
                &parent,
                DependencyType::Active,
                ON.level,
                grandparent_token
                    .duplicate_handle(zx::Rights::SAME_RIGHTS)
                    .expect("dup failed")
                    .into(),
                ON.level,
            )
            .expect("add_dependency failed");
        let mut broker_status = BrokerStatusMatcher::new();

        // All elements should start with required level OFF.
        broker_status.required_level.update(&parent, OFF);
        broker_status.required_level.update(&grandparent, OFF);
        broker_status.required_level.update(&child, OFF);
        broker_status.assert_matches(&broker);

        // Acquire the lease, which should result in two claims, one
        // for the direct parent dependency, and one for the transitive
        // grandparent dependency.
        let lease = broker.acquire_lease(&child, ON).expect("acquire failed");
        // Parent's required level should remain OFF, waiting on Grandparent to turn ON
        // Grandparent's required level should become ON because it has no dependencies
        broker_status.required_level.update(&grandparent, ON);
        broker_status.assert_matches(&broker);
        assert_eq!(broker.get_lease_status(&lease.id), Some(LeaseStatus::Pending));

        // Raise Grandparent power level to ON, now Parent claim should be enforced.
        broker.update_current_level(&grandparent, ON);
        assert_eq!(broker.get_required_level(&parent).unwrap(), ON,);
        // Parent's required level should become ON because Grandparent is now ON.
        broker_status.required_level.update(&parent, ON);
        broker_status.assert_matches(&broker);
        assert_eq!(broker.get_lease_status(&lease.id), Some(LeaseStatus::Pending));

        // Raise Parent power level to ON, now lease should be Satisfied.
        broker.update_current_level(&parent, ON);
        // Child's required level should become ON now that the lease is satisfied.
        broker_status.required_level.update(&child, ON);
        broker_status.assert_matches(&broker);
        assert_eq!(broker.get_lease_status(&lease.id), Some(LeaseStatus::Satisfied));

        // Raise Child power level to ON.
        // All required levels should still be ON.
        // Lease should still be Satisfied.
        broker.update_current_level(&parent, ON);
        broker_status.assert_matches(&broker);
        assert_eq!(broker.get_lease_status(&lease.id), Some(LeaseStatus::Satisfied));

        // Now drop the lease and verify Parent claim is dropped, but
        // Grandparent claim is not yet dropped.
        broker.drop_lease(&lease.id).expect("drop failed");
        // Parent's required level should become OFF after lease drop
        // Grandparent's required level should remain ON
        // Child's required level should become OFF.
        broker_status.required_level.update(&parent, OFF);
        broker_status.required_level.update(&child, OFF);
        broker_status.assert_matches(&broker);

        // Lower Parent power level to OFF, now Grandparent claim should be
        // dropped and should have required level OFF.
        broker.update_current_level(&parent, OFF);
        broker_status.required_level.update(&grandparent, OFF);
        broker_status.assert_matches(&broker);

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
        let inspect = fuchsia_inspect::component::inspector();
        let inspect_node = inspect.root().create_child("test");
        let mut broker = Broker::new(inspect_node);
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

        // Initially, all elements should be at their default required levels.
        // Grandparent should have a default required level of 10
        // and all others should have a default required level of 0.
        let mut broker_status = BrokerStatusMatcher::new();
        broker_status.required_level.update(&parent, ZERO);
        broker_status
            .required_level
            .update(&grandparent, IndexedPowerLevel { level: 10, index: 0 });
        broker_status.required_level.update(&child1, ZERO);
        broker_status.required_level.update(&child2, ZERO);
        broker_status.assert_matches(&broker);

        // Acquire lease for Child 1. Initially, Grandparent should have
        // required level 200 and Parent should have required level 0
        // because Child 1 has a dependency on Parent and Parent has a
        // dependency on Grandparent. Grandparent has no dependencies so its
        // level should be raised first.
        let lease1 = broker
            .acquire_lease(&child1, IndexedPowerLevel { level: 5, index: 1 })
            .expect("acquire failed");
        assert_eq!(
            broker.get_required_level(&parent).unwrap().level,
            0,
            "Parent's required level should become 0, waiting on Grandparent to reach required level"
        );
        assert_eq!(
            broker.get_required_level(&grandparent).unwrap().level,
            200,
            "Grandparent's required level should become 100 because of parent dependency and it has no dependencies of its own"
        );
        assert_eq!(
            broker.get_required_level(&child1).unwrap().level,
            0,
            "Child 1's required level should remain 0"
        );
        assert_eq!(
            broker.get_required_level(&child2).unwrap().level,
            0,
            "Child 2's required level should remain 0"
        );
        assert_eq!(broker.get_lease_status(&lease1.id), Some(LeaseStatus::Pending));

        // Raise Grandparent's current level to 200. Now Parent claim should
        // be enforced, because its dependency on Grandparent is unblocked
        // raising its required level to 50.
        broker.update_current_level(&grandparent, IndexedPowerLevel { level: 200, index: 2 });
        assert_eq!(
            broker.get_required_level(&parent).unwrap().level,
            50,
            "Parent's required level should become 50"
        );
        assert_eq!(
            broker.get_required_level(&grandparent).unwrap().level,
            200,
            "Grandparent's required level should remain 200"
        );
        assert_eq!(
            broker.get_required_level(&child1).unwrap().level,
            0,
            "Child 1's required level should remain 0"
        );
        assert_eq!(
            broker.get_required_level(&child2).unwrap().level,
            0,
            "Child 2's required level should remain 0"
        );
        assert_eq!(broker.get_lease_status(&lease1.id), Some(LeaseStatus::Pending));

        // Update Parent's current level to 50.
        // Parent and Grandparent should have required levels of 50 and 200.
        broker.update_current_level(&parent, IndexedPowerLevel { level: 50, index: 2 });
        assert_eq!(
            broker.get_required_level(&parent).unwrap().level,
            50,
            "Parent's required level should become 50"
        );
        assert_eq!(
            broker.get_required_level(&grandparent).unwrap().level,
            200,
            "Grandparent's required level should remain 200"
        );
        assert_eq!(
            broker.get_required_level(&child1).unwrap().level,
            5,
            "Child 1's required level should become 5"
        );
        assert_eq!(
            broker.get_required_level(&child2).unwrap().level,
            0,
            "Child 2's required level should remain 0"
        );
        assert_eq!(broker.get_lease_status(&lease1.id), Some(LeaseStatus::Satisfied));

        // Acquire a lease for Child 2. Though Child 2 has nominal
        // requirements of Parent at 30 and Grandparent at 100, they are
        // superseded by Child 1's requirements of 50 and 200.
        let lease2 = broker
            .acquire_lease(&child2, IndexedPowerLevel { level: 3, index: 1 })
            .expect("acquire failed");
        assert_eq!(
            broker.get_required_level(&parent).unwrap().level,
            50,
            "Parent's required level should remain 50"
        );
        assert_eq!(
            broker.get_required_level(&grandparent).unwrap().level,
            200,
            "Grandparent's required level should remain 200"
        );
        assert_eq!(
            broker.get_required_level(&child1).unwrap().level,
            5,
            "Child 1's required level should remain 5"
        );
        assert_eq!(
            broker.get_required_level(&child2).unwrap().level,
            3,
            "Child 2's required level should become 3"
        );
        assert_eq!(broker.get_lease_status(&lease1.id), Some(LeaseStatus::Satisfied));
        assert_eq!(broker.get_lease_status(&lease2.id), Some(LeaseStatus::Satisfied));

        // Drop lease for Child 1. Parent's required level should immediately
        // drop to 30. Grandparent's required level will remain at 200 for now.
        broker.drop_lease(&lease1.id).expect("drop failed");
        assert_eq!(
            broker.get_required_level(&parent).unwrap().level,
            30,
            "Parent's required level should drop to 30"
        );
        assert_eq!(
            broker.get_required_level(&grandparent).unwrap().level,
            200,
            "Grandparent's required level should remain 200"
        );
        assert_eq!(
            broker.get_required_level(&child1).unwrap().level,
            0,
            "Child 1's required level should become 0"
        );
        assert_eq!(
            broker.get_required_level(&child2).unwrap().level,
            3,
            "Child 2's required level should remain 3"
        );
        assert_eq!(broker.get_lease_status(&lease2.id), Some(LeaseStatus::Satisfied));

        // Lower Parent's current level to 30. Now Grandparent's required level
        // should drop to 90.
        broker.update_current_level(&parent, IndexedPowerLevel { level: 30, index: 1 });
        assert_eq!(
            broker.get_required_level(&parent).unwrap().level,
            30,
            "Parent should have required level 30"
        );
        assert_eq!(
            broker.get_required_level(&grandparent).unwrap().level,
            90,
            "Grandparent's required level should become 90"
        );
        assert_eq!(
            broker.get_required_level(&child1).unwrap().level,
            0,
            "Child 1's required level should become 0"
        );
        assert_eq!(
            broker.get_required_level(&child2).unwrap().level,
            3,
            "Child 2's required level should remain 3"
        );
        assert_eq!(broker.get_lease_status(&lease2.id), Some(LeaseStatus::Satisfied));

        // Drop lease for Child 2, Parent should have required level 0.
        // Grandparent's required level should remain 90.
        broker.drop_lease(&lease2.id).expect("drop failed");
        assert_eq!(
            broker.get_required_level(&parent).unwrap().level,
            0,
            "Parent's required level should become 0"
        );
        assert_eq!(
            broker.get_required_level(&grandparent).unwrap().level,
            90,
            "Grandparent's required level should remain 90"
        );
        assert_eq!(
            broker.get_required_level(&child1).unwrap().level,
            0,
            "Child 1's required level should become 0"
        );
        assert_eq!(
            broker.get_required_level(&child2).unwrap().level,
            0,
            "Child 2's required level should become 0"
        );

        // Lower Parent's current level to 0. Grandparent claim should now be
        // dropped and have its default required level of 10.
        broker.update_current_level(&parent, ZERO);
        assert_eq!(
            broker.get_required_level(&parent).unwrap().level,
            0,
            "Parent should have required level 0"
        );
        assert_eq!(
            broker.get_required_level(&grandparent).unwrap().level,
            10,
            "Grandparent's required level should become 10"
        );
        assert_eq!(
            broker.get_required_level(&child1).unwrap().level,
            0,
            "Child 1's required level should remain 0"
        );
        assert_eq!(
            broker.get_required_level(&child2).unwrap().level,
            0,
            "Child 2's required level should remain 0"
        );

        assert_lease_cleaned_up(&broker.catalog, &lease1.id);
        assert_lease_cleaned_up(&broker.catalog, &lease2.id);
    }

    #[fuchsia::test]
    async fn test_lease_opportunistic_direct() {
        // Tests that a lease with a opportunistic claim is Contingent while there
        // are no other leases with assertive claims that would satisfy its
        // opportunistic claim.
        //
        // B has an assertive dependency on A.
        // C has a opportunistic dependency on A.
        //  A     B     C
        // ON <= ON
        // ON <------- ON
        let inspect = fuchsia_inspect::component::inspector();
        let inspect_node = inspect.root().create_child("test");
        let mut broker = Broker::new(inspect_node);
        let token_a_assertive = DependencyToken::create();
        let token_a_opportunistic = DependencyToken::create();
        let element_a = broker
            .add_element(
                "A",
                OFF.level,
                BINARY_POWER_LEVELS.to_vec(),
                vec![],
                vec![token_a_assertive
                    .duplicate_handle(zx::Rights::SAME_RIGHTS)
                    .expect("dup failed")
                    .into()],
                vec![token_a_opportunistic
                    .duplicate_handle(zx::Rights::SAME_RIGHTS)
                    .expect("dup failed")
                    .into()],
            )
            .expect("add_element failed");
        let element_b = broker
            .add_element(
                "B",
                OFF.level,
                BINARY_POWER_LEVELS.to_vec(),
                vec![fpb::LevelDependency {
                    dependency_type: DependencyType::Active,
                    dependent_level: ON.level,
                    requires_token: token_a_assertive
                        .duplicate_handle(zx::Rights::SAME_RIGHTS)
                        .expect("dup failed")
                        .into(),
                    requires_level: ON.level,
                }],
                vec![],
                vec![],
            )
            .expect("add_element failed");
        let element_c = broker
            .add_element(
                "C",
                OFF.level,
                BINARY_POWER_LEVELS.to_vec(),
                vec![fpb::LevelDependency {
                    dependency_type: DependencyType::Passive,
                    dependent_level: ON.level,
                    requires_token: token_a_opportunistic
                        .duplicate_handle(zx::Rights::SAME_RIGHTS)
                        .expect("dup failed")
                        .into(),
                    requires_level: ON.level,
                }],
                vec![],
                vec![],
            )
            .expect("add_element failed");

        let mut broker_status = BrokerStatusMatcher::new();

        // Initial required levels for all elements should be OFF.
        // Set all current levels to OFF.
        broker.update_current_level(&element_a, OFF);
        broker.update_current_level(&element_b, OFF);
        broker.update_current_level(&element_c, OFF);
        broker_status.required_level.update(&element_a, OFF);
        broker_status.required_level.update(&element_b, OFF);
        broker_status.required_level.update(&element_c, OFF);
        broker_status.assert_matches(&broker);

        // Lease C.
        // A's required level should remain OFF because of C's opportunistic claim.
        // B's required level should remain OFF.
        // C's required level should remain OFF because the lease is still Pending.
        // Lease C should be pending and contingent on its opportunistic claim.
        let lease_c = broker.acquire_lease(&element_c, ON).expect("acquire failed");
        let lease_c_id = lease_c.id.clone();
        broker_status.assert_matches(&broker);
        assert_eq!(broker.get_lease_status(&lease_c.id), Some(LeaseStatus::Pending));
        assert_eq!(broker.is_lease_contingent(&lease_c.id), true);

        // Lease B.
        // A's required level should become ON because of B's assertive claim.
        // B's required level should remain OFF because the lease is still Pending.
        // C's required level should remain OFF because the lease is still Pending.
        // Lease B should be pending.
        // Lease C should remain pending but is no longer contingent because
        // B's assertive claim unblocks C's opportunistic claim.
        let lease_b = broker.acquire_lease(&element_b, ON).expect("acquire failed");
        let lease_b_id = lease_b.id.clone();
        broker_status.required_level.update(&element_a, ON);
        broker_status.assert_matches(&broker);
        assert_eq!(broker.get_lease_status(&lease_b.id), Some(LeaseStatus::Pending));
        assert_eq!(broker.get_lease_status(&lease_c.id), Some(LeaseStatus::Pending));
        assert_eq!(broker.is_lease_contingent(&lease_b.id), false);
        assert_eq!(broker.is_lease_contingent(&lease_c.id), false);

        // Update A's current level to ON.
        // A's required level should remain ON.
        // B's required level should become ON because the lease is Satisfied.
        // C's required level should become ON because the lease is Satisfied.
        // Lease B & C should become satisfied.
        broker.update_current_level(&element_a, ON);
        broker_status.required_level.update(&element_b, ON);
        broker_status.required_level.update(&element_c, ON);
        broker_status.assert_matches(&broker);
        assert_eq!(broker.get_lease_status(&lease_b.id), Some(LeaseStatus::Satisfied));
        assert_eq!(broker.get_lease_status(&lease_c.id), Some(LeaseStatus::Satisfied));
        assert_eq!(broker.is_lease_contingent(&lease_b.id), false);
        assert_eq!(broker.is_lease_contingent(&lease_c.id), false);
        let v01: Vec<u64> = BINARY_POWER_LEVELS.iter().map(|&v| v as u64).collect();
        assert_data_tree!(inspect, root: {
            test: {
                leases: {
                    lease_b_id.clone() => "Satisfied",
                    lease_c_id.clone() => "Satisfied",
                },
                topology: {
                    "fuchsia.inspect.Graph": {
                        topology: {
                            broker.get_unsatisfiable_element_id().to_string() => {
                                meta: {
                                    name: broker.get_unsatisfiable_element_name(),
                                    valid_levels: broker.get_unsatisfiable_element_levels(),
                                    required_level: "unset",
                                    current_level: "unset",
                                },
                                relationships: {}
                            },
                            element_a.to_string() => {
                                meta: {
                                    name: "A",
                                    valid_levels: v01.clone(),
                                    current_level: ON.level as u64,
                                    required_level: ON.level as u64,
                                },
                                relationships: {},
                            },
                            element_b.to_string() => {
                                meta: {
                                    name: "B",
                                    valid_levels: v01.clone(),
                                    current_level: OFF.level as u64,
                                    required_level: ON.level as u64,
                                    format!("lease_{}", lease_b_id.clone()) => "level_1",
                                },
                                relationships: {
                                    element_a.to_string() => {
                                        edge_id: AnyProperty,
                                        meta: { "1": "1" },
                                    },
                                },
                            },
                            element_c.to_string() => {
                                meta: {
                                    name: "C",
                                    valid_levels: v01.clone(),
                                    current_level: OFF.level as u64,
                                    required_level: ON.level as u64,
                                    format!("lease_{}", lease_c_id.clone()) => "level_1",
                                },
                                relationships: {
                                    element_a.to_string() => {
                                        edge_id: AnyProperty,
                                        meta: { "1": "1p" },
                                    },
                                },
                            },
                        },
                        "events": contains {},
        }}}});

        // Drop Lease on B.
        // A's required level should remain ON.
        // B's required level should become OFF because the lease was dropped.
        // C's required level should become OFF because the lease is now Pending.
        // Lease C should now be pending and contingent.
        broker.drop_lease(&lease_b.id).expect("drop_lease failed");
        broker_status.required_level.update(&element_b, OFF);
        broker_status.required_level.update(&element_c, OFF);
        broker_status.assert_matches(&broker);
        assert_eq!(broker.get_lease_status(&lease_c.id), Some(LeaseStatus::Pending));
        assert_eq!(broker.is_lease_contingent(&lease_c.id), true);

        // Drop Lease on C.
        // A's required level should become OFF.
        // B's required level should remain OFF.
        // C's required level should remain OFF.
        broker.drop_lease(&lease_c.id).expect("drop_lease failed");
        broker_status.required_level.update(&element_a, OFF);
        broker_status.assert_matches(&broker);

        // Update A's current level to OFF.
        // A, B & C's required levels should remain OFF.
        broker.update_current_level(&element_a, OFF);
        broker_status.assert_matches(&broker);

        // Leases C & B should be cleaned up.
        assert_lease_cleaned_up(&broker.catalog, &lease_b_id);
        assert_lease_cleaned_up(&broker.catalog, &lease_c_id);

        assert_data_tree!(inspect, root: {
            test: {
                leases: {},
                topology: {
                    "fuchsia.inspect.Graph": {
                        topology: {
                            broker.get_unsatisfiable_element_id().to_string() => {
                                meta: {
                                    name: broker.get_unsatisfiable_element_name(),
                                    valid_levels: broker.get_unsatisfiable_element_levels(),
                                    required_level: "unset",
                                    current_level: "unset",
                                },
                                relationships: {}
                            },
                            element_a.to_string() => {
                                meta: {
                                    name: "A",
                                    valid_levels: v01.clone(),
                                    current_level: OFF.level as u64,
                                    required_level: OFF.level as u64,
                                },
                                relationships: {},
                            },
                            element_b.to_string() => {
                                meta: {
                                    name: "B",
                                    valid_levels: v01.clone(),
                                    current_level: OFF.level as u64,
                                    required_level: OFF.level as u64,
                                },
                                relationships: {
                                    element_a.to_string() => {
                                        edge_id: AnyProperty,
                                        meta: { "1": "1" }},
                                },
                            },
                            element_c.to_string() => {
                                meta: {
                                    name: "C",
                                    valid_levels: v01.clone(),
                                    current_level: OFF.level as u64,
                                    required_level: OFF.level as u64,
                                },
                                relationships: {
                                    element_a.to_string() => {
                                        edge_id: AnyProperty,
                                        meta: { "1": "1p" },
                                    },
                                },
                            },
                        },
                        "events": contains {},
        }}}});
    }

    #[fuchsia::test]
    async fn test_drop_opportunistic_lease_before_assertive_claim_satisifed() {
        // Tests that if a lease has a opportunistic claim that has been satisfied by
        // an assertive claim, and then the lease is dropped *before* the assertive claim
        // was satisfied, that the opportunistic claim should not be enforced, even though
        // it would have, had the lease not been dropped prematurely.
        //
        // A has an assertive dependency on B.
        // C has a opportunistic dependency on B.
        //
        //  A     B     C
        // ON => ON <- ON
        let inspect = fuchsia_inspect::component::inspector();
        let inspect_node = inspect.root().create_child("test");
        let mut broker = Broker::new(inspect_node);
        let token_b_assertive = DependencyToken::create();
        let token_b_opportunistic = DependencyToken::create();
        let element_b = broker
            .add_element(
                "B",
                OFF.level,
                BINARY_POWER_LEVELS.to_vec(),
                vec![],
                vec![token_b_assertive
                    .duplicate_handle(zx::Rights::SAME_RIGHTS)
                    .expect("dup failed")
                    .into()],
                vec![token_b_opportunistic
                    .duplicate_handle(zx::Rights::SAME_RIGHTS)
                    .expect("dup failed")
                    .into()],
            )
            .expect("add_element failed");
        let element_a = broker
            .add_element(
                "A",
                OFF.level,
                BINARY_POWER_LEVELS.to_vec(),
                vec![fpb::LevelDependency {
                    dependency_type: DependencyType::Active,
                    dependent_level: ON.level,
                    requires_token: token_b_assertive
                        .duplicate_handle(zx::Rights::SAME_RIGHTS)
                        .expect("dup failed")
                        .into(),
                    requires_level: ON.level,
                }],
                vec![],
                vec![],
            )
            .expect("add_element failed");
        let element_c = broker
            .add_element(
                "C",
                OFF.level,
                BINARY_POWER_LEVELS.to_vec(),
                vec![fpb::LevelDependency {
                    dependency_type: DependencyType::Passive,
                    dependent_level: ON.level,
                    requires_token: token_b_opportunistic
                        .duplicate_handle(zx::Rights::SAME_RIGHTS)
                        .expect("dup failed")
                        .into(),
                    requires_level: ON.level,
                }],
                vec![],
                vec![],
            )
            .expect("add_element failed");

        // Initial required level for A, B & C should be OFF.
        // Set A, B & C's current level to OFF.
        let mut broker_status = BrokerStatusMatcher::new();

        // Initial required level for A, B & C should be OFF.
        // Set A, B & C's current level to OFF.
        broker.update_current_level(&element_a, OFF);
        broker.update_current_level(&element_b, OFF);
        broker.update_current_level(&element_c, OFF);
        broker_status.required_level.update(&element_a, OFF);
        broker_status.required_level.update(&element_b, OFF);
        broker_status.required_level.update(&element_c, OFF);
        broker_status.assert_matches(&broker);

        // Lease C.
        // All required levels should be OFF, as B is not on and opportunistic.
        // Lease C should be pending and contingent.
        let lease_c = broker.acquire_lease(&element_c, ON).expect("acquire failed");
        let lease_c_id = lease_c.id.clone();
        broker_status.assert_matches(&broker);
        assert_eq!(broker.get_lease_status(&lease_c.id), Some(LeaseStatus::Pending));
        assert_eq!(broker.is_lease_contingent(&lease_c.id), true);

        // Lease A.
        // B's required level should be ON.
        // A and C's required levels should be OFF.
        // Lease A should be pending and non-contingent.
        // Lease C should be pending and non-contingent.
        let lease_a = broker.acquire_lease(&element_a, ON).expect("acquire failed");
        let lease_a_id = lease_a.id.clone();
        broker_status.required_level.update(&element_b, ON);
        broker_status.assert_matches(&broker);
        assert_eq!(broker.get_lease_status(&lease_a.id), Some(LeaseStatus::Pending));
        assert_eq!(broker.get_lease_status(&lease_c.id), Some(LeaseStatus::Pending));
        assert_eq!(broker.is_lease_contingent(&lease_a.id), false);
        assert_eq!(broker.is_lease_contingent(&lease_c.id), false);

        // Drop Lease on A.
        // All required levels should be OFF as there is no longer an assertive claim.
        // Lease C should be pending and contingent.
        broker.drop_lease(&lease_a.id).expect("drop_lease failed");
        broker_status.required_level.update(&element_b, OFF);
        broker_status.assert_matches(&broker);
        assert_eq!(broker.get_lease_status(&lease_c.id), Some(LeaseStatus::Pending));
        assert_eq!(broker.is_lease_contingent(&lease_c.id), true);

        // Drop Lease ON C.
        // All required levels should remain OFF.
        broker.drop_lease(&lease_c.id).expect("drop_lease failed");
        broker_status.assert_matches(&broker);

        // Leases A & C should be cleaned up.
        assert_lease_cleaned_up(&broker.catalog, &lease_a_id);
        assert_lease_cleaned_up(&broker.catalog, &lease_c_id);
    }

    #[fuchsia::test]
    async fn test_lease_opportunistic_immediate() {
        // Tests that a lease with a opportunistic claim is immediately satisfied if
        // there are already leases with assertive claims that would satisfy its
        // opportunistic claim.
        //
        // B has an assertive dependency on A.
        // C has a opportunistic dependency on A.
        //  A     B     C
        // ON <= ON
        // ON <------- ON
        let inspect = fuchsia_inspect::component::inspector();
        let inspect_node = inspect.root().create_child("test");
        let mut broker = Broker::new(inspect_node);
        let token_a_assertive = DependencyToken::create();
        let token_a_opportunistic = DependencyToken::create();
        let element_a = broker
            .add_element(
                "A",
                OFF.level,
                BINARY_POWER_LEVELS.to_vec(),
                vec![],
                vec![token_a_assertive
                    .duplicate_handle(zx::Rights::SAME_RIGHTS)
                    .expect("dup failed")
                    .into()],
                vec![token_a_opportunistic
                    .duplicate_handle(zx::Rights::SAME_RIGHTS)
                    .expect("dup failed")
                    .into()],
            )
            .expect("add_element failed");
        let element_b = broker
            .add_element(
                "B",
                OFF.level,
                BINARY_POWER_LEVELS.to_vec(),
                vec![fpb::LevelDependency {
                    dependency_type: DependencyType::Active,
                    dependent_level: ON.level,
                    requires_token: token_a_assertive
                        .duplicate_handle(zx::Rights::SAME_RIGHTS)
                        .expect("dup failed")
                        .into(),
                    requires_level: ON.level,
                }],
                vec![],
                vec![],
            )
            .expect("add_element failed");
        let element_c = broker
            .add_element(
                "C",
                OFF.level,
                BINARY_POWER_LEVELS.to_vec(),
                vec![fpb::LevelDependency {
                    dependency_type: DependencyType::Passive,
                    dependent_level: ON.level,
                    requires_token: token_a_opportunistic
                        .duplicate_handle(zx::Rights::SAME_RIGHTS)
                        .expect("dup failed")
                        .into(),
                    requires_level: ON.level,
                }],
                vec![],
                vec![],
            )
            .expect("add_element failed");

        // Initial required level for A, B & C should be OFF.
        // Set A, B & C's current level to OFF.
        let mut broker_status = BrokerStatusMatcher::new();

        // Initial required level for A, B & C should be OFF.
        // Set A, B & C's current level to OFF.
        broker.update_current_level(&element_a, OFF);
        broker.update_current_level(&element_b, OFF);
        broker.update_current_level(&element_c, OFF);
        broker_status.required_level.update(&element_a, OFF);
        broker_status.required_level.update(&element_b, OFF);
        broker_status.required_level.update(&element_c, OFF);
        broker_status.assert_matches(&broker);

        // Lease B.
        // A's required level should become ON because of B's assertive claim.
        // B's required level should be OFF because A is not yet on.
        // C's required level should remain OFF.
        // Lease B should be pending.
        let lease_b = broker.acquire_lease(&element_b, ON).expect("acquire failed");
        let lease_b_id = lease_b.id.clone();
        broker_status.required_level.update(&element_a, ON);
        broker_status.assert_matches(&broker);
        assert_eq!(broker.get_lease_status(&lease_b.id), Some(LeaseStatus::Pending));

        // Update A's current level to ON.
        // A's required level should remain ON.
        // B's required level should become ON.
        // C's required level should remain OFF.
        // Lease B should become satisfied.
        broker.update_current_level(&element_a, ON);
        broker_status.required_level.update(&element_b, ON);
        broker_status.assert_matches(&broker);
        assert_eq!(broker.get_lease_status(&lease_b.id), Some(LeaseStatus::Satisfied));

        // Lease C.
        // A's required level should remain ON.
        // B's required level should remain ON.
        // C's required level should become ON because its dependencies are
        // already satisfied.
        // Lease B should still be satisfied.
        // Lease C should be immediately satisfied because B's assertive claim on
        // A satisfies C's opportunistic claim.
        let lease_c = broker.acquire_lease(&element_c, ON).expect("acquire failed");
        let lease_c_id = lease_c.id.clone();
        broker_status.required_level.update(&element_c, ON);
        broker_status.assert_matches(&broker);
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
        broker_status.required_level.update(&element_b, OFF);
        broker_status.required_level.update(&element_c, OFF);
        broker_status.assert_matches(&broker);
        assert_eq!(broker.get_lease_status(&lease_c.id), Some(LeaseStatus::Pending));
        assert_eq!(broker.is_lease_contingent(&lease_c.id), true);

        // Drop Lease on C.
        // A's required level should become OFF.
        // B's required level should remain OFF.
        // C's required level should remain OFF.
        broker.drop_lease(&lease_c.id).expect("drop_lease failed");
        broker_status.required_level.update(&element_a, OFF);
        broker_status.assert_matches(&broker);

        // Update A's current level to OFF.
        // A's required level should remain OFF.
        // B's required level should remain OFF.
        // C's required level should remain OFF.
        broker.update_current_level(&element_a, OFF);
        broker_status.assert_matches(&broker);

        // Leases C & B should be cleaned up.
        assert_lease_cleaned_up(&broker.catalog, &lease_b_id);
        assert_lease_cleaned_up(&broker.catalog, &lease_c_id);
    }

    #[fuchsia::test]
    async fn test_lease_opportunistic_partially_satisfied() {
        // Tests that a lease with two opportunistic claims, one which is satisfied
        // initially by a second lease, and then the other that is satisfied
        // by a third lease, correctly becomes satisfied.
        //
        // B has an assertive dependency on A.
        // C has a opportunistic dependency on A and E.
        // D has an assertive dependency on E.
        //  A     B     C     D     E
        // ON <= ON
        // ON <------- ON -------> ON
        //                   ON => ON
        let inspect = fuchsia_inspect::component::inspector();
        let inspect_node = inspect.root().create_child("test");
        let mut broker = Broker::new(inspect_node);
        let token_a_assertive = DependencyToken::create();
        let token_a_opportunistic = DependencyToken::create();
        let element_a = broker
            .add_element(
                "A",
                OFF.level,
                BINARY_POWER_LEVELS.to_vec(),
                vec![],
                vec![token_a_assertive
                    .duplicate_handle(zx::Rights::SAME_RIGHTS)
                    .expect("dup failed")
                    .into()],
                vec![token_a_opportunistic
                    .duplicate_handle(zx::Rights::SAME_RIGHTS)
                    .expect("dup failed")
                    .into()],
            )
            .expect("add_element failed");
        let element_b = broker
            .add_element(
                "B",
                OFF.level,
                BINARY_POWER_LEVELS.to_vec(),
                vec![fpb::LevelDependency {
                    dependency_type: DependencyType::Active,
                    dependent_level: ON.level,
                    requires_token: token_a_assertive
                        .duplicate_handle(zx::Rights::SAME_RIGHTS)
                        .expect("dup failed")
                        .into(),
                    requires_level: ON.level,
                }],
                vec![],
                vec![],
            )
            .expect("add_element failed");
        let token_e_assertive = DependencyToken::create();
        let token_e_opportunistic = DependencyToken::create();
        let element_e = broker
            .add_element(
                "E",
                OFF.level,
                BINARY_POWER_LEVELS.to_vec(),
                vec![],
                vec![token_e_assertive
                    .duplicate_handle(zx::Rights::SAME_RIGHTS)
                    .expect("dup failed")
                    .into()],
                vec![token_e_opportunistic
                    .duplicate_handle(zx::Rights::SAME_RIGHTS)
                    .expect("dup failed")
                    .into()],
            )
            .expect("add_element failed");
        let element_c = broker
            .add_element(
                "C",
                OFF.level,
                BINARY_POWER_LEVELS.to_vec(),
                vec![
                    fpb::LevelDependency {
                        dependency_type: DependencyType::Passive,
                        dependent_level: ON.level,
                        requires_token: token_a_opportunistic
                            .duplicate_handle(zx::Rights::SAME_RIGHTS)
                            .expect("dup failed")
                            .into(),
                        requires_level: ON.level,
                    },
                    fpb::LevelDependency {
                        dependency_type: DependencyType::Passive,
                        dependent_level: ON.level,
                        requires_token: token_e_opportunistic
                            .duplicate_handle(zx::Rights::SAME_RIGHTS)
                            .expect("dup failed")
                            .into(),
                        requires_level: ON.level,
                    },
                ],
                vec![],
                vec![],
            )
            .expect("add_element failed");
        let element_d = broker
            .add_element(
                "D",
                OFF.level,
                BINARY_POWER_LEVELS.to_vec(),
                vec![fpb::LevelDependency {
                    dependency_type: DependencyType::Active,
                    dependent_level: ON.level,
                    requires_token: token_e_assertive
                        .duplicate_handle(zx::Rights::SAME_RIGHTS)
                        .expect("dup failed")
                        .into(),
                    requires_level: ON.level,
                }],
                vec![],
                vec![],
            )
            .expect("add_element failed");

        let mut broker_status = BrokerStatusMatcher::new();

        // Initial required level for A, B, C, D & E should be OFF.
        // Set A, B, C, D & E's current level to OFF.
        broker.update_current_level(&element_a, OFF);
        broker.update_current_level(&element_b, OFF);
        broker.update_current_level(&element_c, OFF);
        broker.update_current_level(&element_d, OFF);
        broker.update_current_level(&element_e, OFF);
        broker_status.required_level.update(&element_a, OFF);
        broker_status.required_level.update(&element_b, OFF);
        broker_status.required_level.update(&element_c, OFF);
        broker_status.required_level.update(&element_d, OFF);
        broker_status.required_level.update(&element_e, OFF);
        broker_status.assert_matches(&broker);

        // Lease B.
        // A's required level should become ON because of B's assertive claim.
        // B, C, D & E's required levels should remain OFF.
        // Lease B should be pending.
        let lease_b = broker.acquire_lease(&element_b, ON).expect("acquire failed");
        let lease_b_id = lease_b.id.clone();
        broker_status.required_level.update(&element_a, ON);
        broker_status.assert_matches(&broker);
        assert_eq!(broker.get_lease_status(&lease_b.id), Some(LeaseStatus::Pending));

        // Update A's current level to ON.
        // A's required level should remain ON.
        // B's required level should become ON.
        // C, D & E's required level should remain OFF.
        // Lease B should become satisfied.
        broker.update_current_level(&element_a, ON);
        broker_status.required_level.update(&element_b, ON);
        broker_status.assert_matches(&broker);
        assert_eq!(broker.get_lease_status(&lease_b.id), Some(LeaseStatus::Satisfied));

        // Lease C.
        // A & B's required levels should remain ON.
        // C's required level should remain OFF because its lease is pending and contingent.
        // D & E's required level should remain OFF.
        // Lease B should still be satisfied.
        // Lease C should be contingent because while B's assertive claim on
        // A satisfies C's opportunistic claim on A, nothing satisfies C's opportunistic
        // claim on E.
        let lease_c = broker.acquire_lease(&element_c, ON).expect("acquire failed");
        let lease_c_id = lease_c.id.clone();
        broker_status.assert_matches(&broker);
        assert_eq!(broker.get_lease_status(&lease_b.id), Some(LeaseStatus::Satisfied));
        assert_eq!(broker.get_lease_status(&lease_c.id), Some(LeaseStatus::Pending));
        assert_eq!(broker.is_lease_contingent(&lease_c.id), true);

        // Lease D.
        // A & B's required levels should remain ON.
        // C & D's required levels should remain OFF because their dependencies are not yet satisfied.
        // E's required level should become ON because of D's assertive claim.
        // Lease B should still be satisfied.
        // Lease C should be pending, but no longer contingent.
        // Lease D should be pending.
        let lease_d = broker.acquire_lease(&element_d, ON).expect("acquire failed");
        let lease_d_id = lease_d.id.clone();
        broker_status.required_level.update(&element_e, ON);
        broker_status.assert_matches(&broker);
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
        broker.update_current_level(&element_e, ON);
        broker_status.required_level.update(&element_c, ON);
        broker_status.required_level.update(&element_d, ON);
        broker_status.assert_matches(&broker);
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
        broker_status.required_level.update(&element_b, OFF);
        broker_status.required_level.update(&element_c, OFF);
        broker_status.assert_matches(&broker);
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
        broker_status.required_level.update(&element_d, OFF);
        broker_status.assert_matches(&broker);
        assert_eq!(broker.get_lease_status(&lease_c.id), Some(LeaseStatus::Pending));
        assert_eq!(broker.is_lease_contingent(&lease_c.id), true);

        // Drop Lease on C.
        // A & E's required levels should become OFF.
        // B, C & D's required levels should remain OFF.
        broker.drop_lease(&lease_c.id).expect("drop_lease failed");
        broker_status.required_level.update(&element_a, OFF);
        broker_status.required_level.update(&element_e, OFF);
        broker_status.assert_matches(&broker);

        // Update A's current level to OFF.
        // All required levels should remain OFF.
        broker.update_current_level(&element_a, OFF);
        broker_status.assert_matches(&broker);

        // Update E's current level to OFF.
        // All required levels should remain OFF.
        broker.update_current_level(&element_e, OFF);
        broker_status.assert_matches(&broker);

        // Leases C & B should be cleaned up.
        assert_lease_cleaned_up(&broker.catalog, &lease_b_id);
        assert_lease_cleaned_up(&broker.catalog, &lease_c_id);
        assert_lease_cleaned_up(&broker.catalog, &lease_d_id);
    }

    #[fuchsia::test]
    async fn test_lease_opportunistic_reacquire() {
        // Tests that a lease with a opportunistic claim is dropped and reacquired
        // will not prevent power-down.
        //
        // B has an assertive dependency on A.
        // C has a opportunistic dependency on A.
        //  A     B     C
        // ON <= ON
        // ON <------- ON
        let inspect = fuchsia_inspect::component::inspector();
        let inspect_node = inspect.root().create_child("test");
        let mut broker = Broker::new(inspect_node);
        let token_a_assertive = DependencyToken::create();
        let token_a_opportunistic = DependencyToken::create();
        let element_a = broker
            .add_element(
                "A",
                OFF.level,
                BINARY_POWER_LEVELS.to_vec(),
                vec![],
                vec![token_a_assertive
                    .duplicate_handle(zx::Rights::SAME_RIGHTS)
                    .expect("dup failed")
                    .into()],
                vec![token_a_opportunistic
                    .duplicate_handle(zx::Rights::SAME_RIGHTS)
                    .expect("dup failed")
                    .into()],
            )
            .expect("add_element failed");
        let element_b = broker
            .add_element(
                "B",
                OFF.level,
                BINARY_POWER_LEVELS.to_vec(),
                vec![fpb::LevelDependency {
                    dependency_type: DependencyType::Active,
                    dependent_level: ON.level,
                    requires_token: token_a_assertive
                        .duplicate_handle(zx::Rights::SAME_RIGHTS)
                        .expect("dup failed")
                        .into(),
                    requires_level: ON.level,
                }],
                vec![],
                vec![],
            )
            .expect("add_element failed");
        let element_c = broker
            .add_element(
                "C",
                OFF.level,
                BINARY_POWER_LEVELS.to_vec(),
                vec![fpb::LevelDependency {
                    dependency_type: DependencyType::Passive,
                    dependent_level: ON.level,
                    requires_token: token_a_opportunistic
                        .duplicate_handle(zx::Rights::SAME_RIGHTS)
                        .expect("dup failed")
                        .into(),
                    requires_level: ON.level,
                }],
                vec![],
                vec![],
            )
            .expect("add_element failed");

        // All initial required levels should be OFF.
        // Set all current levels to OFF.
        assert_eq!(broker.get_required_level(&element_a), Some(OFF));
        assert_eq!(broker.get_required_level(&element_b), Some(OFF));
        assert_eq!(broker.get_required_level(&element_c), Some(OFF));
        broker.update_current_level(&element_a, OFF);
        broker.update_current_level(&element_b, OFF);
        broker.update_current_level(&element_c, OFF);

        // Lease C.
        // A's required level should remain OFF because of C's opportunistic claim.
        // B's required level should remain OFF.
        // C's required level should remain OFF because its lease is pending and contingent.
        // Lease C should be pending and contingent on its opportunistic claim.
        let lease_c = broker.acquire_lease(&element_c, ON).expect("acquire failed");
        let lease_c_id = lease_c.id.clone();
        assert_eq!(broker.get_required_level(&element_a), Some(OFF));
        assert_eq!(broker.get_required_level(&element_b), Some(OFF));
        assert_eq!(broker.get_required_level(&element_c), Some(OFF));
        assert_eq!(broker.get_lease_status(&lease_c.id), Some(LeaseStatus::Pending));
        assert_eq!(broker.is_lease_contingent(&lease_c.id), true);

        // Lease B.
        // A's required level should become ON because of B's assertive claim.
        // B's required level should remain OFF because its dependency is not satisfied.
        // C's required level should remain OFF because its dependency is not satisfied.
        // Lease B should be pending.
        // Lease C should remain pending but no longer contingent because
        // B's assertive claim would satisfy C's opportunistic claim.
        let lease_b = broker.acquire_lease(&element_b, ON).expect("acquire failed");
        let lease_b_id = lease_b.id.clone();
        assert_eq!(broker.get_required_level(&element_a), Some(ON));
        assert_eq!(broker.get_required_level(&element_b), Some(OFF));
        assert_eq!(broker.get_required_level(&element_c), Some(OFF));
        assert_eq!(broker.get_lease_status(&lease_b.id), Some(LeaseStatus::Pending));
        assert_eq!(broker.get_lease_status(&lease_c.id), Some(LeaseStatus::Pending));
        assert_eq!(broker.is_lease_contingent(&lease_b.id), false);
        assert_eq!(broker.is_lease_contingent(&lease_c.id), false);

        // Update A's current level to ON.
        // A's required level should remain ON.
        // B's required level should become ON because its dependency is satisfied.
        // C's required level should become ON because its dependency is satisfied.
        // Lease B & C should become satisfied.
        broker.update_current_level(&element_a, ON);
        assert_eq!(broker.get_required_level(&element_a), Some(ON));
        assert_eq!(broker.get_required_level(&element_b), Some(ON));
        assert_eq!(broker.get_required_level(&element_c), Some(ON));
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
        assert_eq!(broker.get_required_level(&element_a), Some(ON));
        assert_eq!(broker.get_required_level(&element_b), Some(OFF));
        assert_eq!(broker.get_required_level(&element_c), Some(OFF));
        assert_eq!(broker.get_lease_status(&lease_c.id), Some(LeaseStatus::Pending));
        assert_eq!(broker.is_lease_contingent(&lease_c.id), true);

        // Drop Lease on C.
        // A's required level should become OFF.
        // B's required level should remain OFF.
        // C's required level should remain OFF because it is no longer leased.
        broker.drop_lease(&lease_c.id).expect("drop_lease failed");
        assert_eq!(broker.get_required_level(&element_a), Some(OFF));
        assert_eq!(broker.get_required_level(&element_b), Some(OFF));
        assert_eq!(broker.get_required_level(&element_c), Some(OFF));

        // Reacquire Lease on C.
        // A's required level should remain OFF despite C's new opportunistic claim.
        // B's required level should remain OFF.
        // C's required level should remain OFF because its lease is pending and contingent.
        // The lease on C should remain Pending and contingent even though A's current level is ON.
        let lease_c_reacquired = broker.acquire_lease(&element_c, ON).expect("acquire failed");
        let lease_c_reacquired_id = lease_c_reacquired.id.clone();
        assert_eq!(broker.get_required_level(&element_a), Some(OFF));
        assert_eq!(broker.get_required_level(&element_b), Some(OFF));
        assert_eq!(broker.get_required_level(&element_c), Some(OFF));
        assert_eq!(broker.get_lease_status(&lease_c_reacquired.id), Some(LeaseStatus::Pending));
        assert_eq!(broker.is_lease_contingent(&lease_c_reacquired.id), true);

        // Update A's current level to OFF.
        // A, B & C's required levels should remain OFF.
        broker.update_current_level(&element_a, OFF);
        assert_eq!(broker.get_required_level(&element_a), Some(OFF));
        assert_eq!(broker.get_required_level(&element_b), Some(OFF));
        assert_eq!(broker.get_required_level(&element_c), Some(OFF));

        // Drop reacquired lease on C.
        // A, B & C's required levels should remain OFF.
        broker.drop_lease(&lease_c_reacquired.id).expect("drop_lease failed");
        assert_eq!(broker.get_required_level(&element_a), Some(OFF));
        assert_eq!(broker.get_required_level(&element_b), Some(OFF));
        assert_eq!(broker.get_required_level(&element_c), Some(OFF));

        // All leases should be cleaned up.
        assert_lease_cleaned_up(&broker.catalog, &lease_b_id);
        assert_lease_cleaned_up(&broker.catalog, &lease_c_id);
        assert_lease_cleaned_up(&broker.catalog, &lease_c_reacquired_id);
    }

    #[fuchsia::test]
    async fn test_lease_opportunistic_reuse() {
        // Tests that a lease with a opportunistic claim can be reused after
        // the current level of the consumer element is lowered.
        //
        // B has an assertive dependency on A.
        // C has a opportunistic dependency on A.
        //  A     B     C
        // ON <= ON
        // ON <------- ON
        let inspect = fuchsia_inspect::component::inspector();
        let inspect_node = inspect.root().create_child("test");
        let mut broker = Broker::new(inspect_node);
        let token_a_assertive = DependencyToken::create();
        let token_a_opportunistic = DependencyToken::create();
        let element_a = broker
            .add_element(
                "A",
                OFF.level,
                BINARY_POWER_LEVELS.to_vec(),
                vec![],
                vec![token_a_assertive
                    .duplicate_handle(zx::Rights::SAME_RIGHTS)
                    .expect("dup failed")
                    .into()],
                vec![token_a_opportunistic
                    .duplicate_handle(zx::Rights::SAME_RIGHTS)
                    .expect("dup failed")
                    .into()],
            )
            .expect("add_element failed");
        let element_b = broker
            .add_element(
                "B",
                OFF.level,
                BINARY_POWER_LEVELS.to_vec(),
                vec![fpb::LevelDependency {
                    dependency_type: DependencyType::Active,
                    dependent_level: ON.level,
                    requires_token: token_a_assertive
                        .duplicate_handle(zx::Rights::SAME_RIGHTS)
                        .expect("dup failed")
                        .into(),
                    requires_level: ON.level,
                }],
                vec![],
                vec![],
            )
            .expect("add_element failed");
        let element_c = broker
            .add_element(
                "C",
                OFF.level,
                BINARY_POWER_LEVELS.to_vec(),
                vec![fpb::LevelDependency {
                    dependency_type: DependencyType::Passive,
                    dependent_level: ON.level,
                    requires_token: token_a_opportunistic
                        .duplicate_handle(zx::Rights::SAME_RIGHTS)
                        .expect("dup failed")
                        .into(),
                    requires_level: ON.level,
                }],
                vec![],
                vec![],
            )
            .expect("add_element failed");

        // All initial required levels should be OFF.
        // Set all current levels to OFF.
        assert_eq!(broker.get_required_level(&element_a), Some(OFF));
        assert_eq!(broker.get_required_level(&element_b), Some(OFF));
        assert_eq!(broker.get_required_level(&element_c), Some(OFF));
        broker.update_current_level(&element_a, OFF);
        broker.update_current_level(&element_b, OFF);
        broker.update_current_level(&element_c, OFF);

        // Lease C.
        // A's required level should remain OFF because of C's opportunistic claim.
        // B's required level should remain OFF.
        // C's required level should remain OFF because its lease is pending and contingent.
        // Lease C should be pending and contingent on its opportunistic claim.
        let lease_c = broker.acquire_lease(&element_c, ON).expect("acquire failed");
        let lease_c_id = lease_c.id.clone();
        assert_eq!(broker.get_required_level(&element_a), Some(OFF));
        assert_eq!(broker.get_required_level(&element_b), Some(OFF));
        assert_eq!(broker.get_required_level(&element_c), Some(OFF));
        assert_eq!(broker.get_lease_status(&lease_c.id), Some(LeaseStatus::Pending));
        assert_eq!(broker.is_lease_contingent(&lease_c.id), true);

        // Lease B.
        // A's required level should become ON because of B's assertive claim.
        // B's required level should remain OFF because its dependency is not satisfied.
        // C's required level should remain OFF because its dependency is not satisfied.
        // Lease B should be pending.
        // Lease C should remain pending but no longer contingent because
        // B's assertive claim would satisfy C's opportunistic claim.
        let lease_b = broker.acquire_lease(&element_b, ON).expect("acquire failed");
        let lease_b_id = lease_b.id.clone();
        assert_eq!(broker.get_required_level(&element_a), Some(ON));
        assert_eq!(broker.get_required_level(&element_b), Some(OFF));
        assert_eq!(broker.get_required_level(&element_c), Some(OFF));
        assert_eq!(broker.get_lease_status(&lease_b.id), Some(LeaseStatus::Pending));
        assert_eq!(broker.get_lease_status(&lease_c.id), Some(LeaseStatus::Pending));
        assert_eq!(broker.is_lease_contingent(&lease_b.id), false);
        assert_eq!(broker.is_lease_contingent(&lease_c.id), false);

        // Update A's current level to ON.
        // A's required level should remain ON.
        // B's required level should become ON because its dependency is satisfied.
        // C's required level should become ON because its dependency is satisfied.
        // Lease B & C should become satisfied.
        broker.update_current_level(&element_a, ON);
        assert_eq!(broker.get_required_level(&element_a), Some(ON));
        assert_eq!(broker.get_required_level(&element_b), Some(ON));
        assert_eq!(broker.get_required_level(&element_c), Some(ON));
        assert_eq!(broker.get_lease_status(&lease_b.id), Some(LeaseStatus::Satisfied));
        assert_eq!(broker.get_lease_status(&lease_c.id), Some(LeaseStatus::Satisfied));
        assert_eq!(broker.is_lease_contingent(&lease_b.id), false);
        assert_eq!(broker.is_lease_contingent(&lease_c.id), false);

        // Update C's current level to ON.
        // A's required level should remain ON.
        // B's required level should remain ON.
        // C's required level should remain ON.
        // Lease B & C should remain satisfied.
        broker.update_current_level(&element_c, ON);
        assert_eq!(broker.get_required_level(&element_a), Some(ON));
        assert_eq!(broker.get_required_level(&element_b), Some(ON));
        assert_eq!(broker.get_required_level(&element_c), Some(ON));
        assert_eq!(broker.get_lease_status(&lease_b.id), Some(LeaseStatus::Satisfied));
        assert_eq!(broker.get_lease_status(&lease_c.id), Some(LeaseStatus::Satisfied));
        assert_eq!(broker.is_lease_contingent(&lease_b.id), false);
        assert_eq!(broker.is_lease_contingent(&lease_c.id), false);

        // Drop Lease on B.
        // A's required level should remain ON.
        // B's required level should become OFF because it is no longer leased.
        // C's required level should become OFF because it now pending and contingent.
        // Lease C should now be pending and contingent.
        broker.drop_lease(&lease_b.id).expect("drop_lease failed");
        assert_eq!(broker.get_required_level(&element_a), Some(ON));
        assert_eq!(broker.get_required_level(&element_b), Some(OFF));
        assert_eq!(broker.get_required_level(&element_c), Some(OFF));
        assert_eq!(broker.get_lease_status(&lease_c.id), Some(LeaseStatus::Pending));
        assert_eq!(broker.is_lease_contingent(&lease_c.id), true);

        // Update C's current level to OFF.
        // A's required level should become OFF.
        // B's required level should remain OFF.
        // C's required level should remain OFF because its lease is pending and contingent.
        // Lease C should still be pending and contingent.
        broker.update_current_level(&element_c, OFF);
        assert_eq!(broker.get_required_level(&element_a), Some(OFF));
        assert_eq!(broker.get_required_level(&element_b), Some(OFF));
        assert_eq!(broker.get_required_level(&element_c), Some(OFF));
        assert_eq!(broker.get_lease_status(&lease_c.id), Some(LeaseStatus::Pending));
        assert_eq!(broker.is_lease_contingent(&lease_c.id), true);

        // Update A's current level to OFF.
        // A, B & C's required levels should remain OFF.
        // Lease C should still be pending and contingent.
        broker.update_current_level(&element_a, OFF);
        assert_eq!(broker.get_required_level(&element_a), Some(OFF));
        assert_eq!(broker.get_required_level(&element_b), Some(OFF));
        assert_eq!(broker.get_required_level(&element_c), Some(OFF));
        assert_eq!(broker.get_lease_status(&lease_c.id), Some(LeaseStatus::Pending));
        assert_eq!(broker.is_lease_contingent(&lease_c.id), true);

        // Update B's current level to OFF.
        // A, B & C's required levels should remain OFF.
        // Lease C should still be pending and contingent.
        broker.update_current_level(&element_b, OFF);
        assert_eq!(broker.get_required_level(&element_a), Some(OFF));
        assert_eq!(broker.get_required_level(&element_b), Some(OFF));
        assert_eq!(broker.get_required_level(&element_c), Some(OFF));
        assert_eq!(broker.get_lease_status(&lease_c.id), Some(LeaseStatus::Pending));
        assert_eq!(broker.is_lease_contingent(&lease_c.id), true);

        // Reacquire Lease on B.
        // A's required level should become ON because of B's assertive claim.
        // B's required level should remain OFF because its dependency is not satisfied.
        // C's required level should remain OFF because its dependency is not satisfied.
        // Lease B should be pending.
        // Lease C should remain pending but no longer contingent because
        // B's assertive claim would satisfy C's opportunistic claim.
        let lease_b_reacquired = broker.acquire_lease(&element_b, ON).expect("acquire failed");
        let lease_b_reacquired_id = lease_b_reacquired.id.clone();
        assert_eq!(broker.get_required_level(&element_a), Some(ON));
        assert_eq!(broker.get_required_level(&element_b), Some(OFF));
        assert_eq!(broker.get_required_level(&element_c), Some(OFF));
        assert_eq!(broker.get_lease_status(&lease_b_reacquired.id), Some(LeaseStatus::Pending));
        assert_eq!(broker.get_lease_status(&lease_c.id), Some(LeaseStatus::Pending));
        assert_eq!(broker.is_lease_contingent(&lease_b_reacquired.id), false);
        assert_eq!(broker.is_lease_contingent(&lease_c.id), false);

        // Update A's current level to ON.
        // A's required level should remain ON.
        // B's required level should become ON because its dependency is satisfied.
        // C's required level should become ON because its dependency is satisfied.
        // Lease B & C should become satisfied.
        broker.update_current_level(&element_a, ON);
        assert_eq!(broker.get_required_level(&element_a), Some(ON));
        assert_eq!(broker.get_required_level(&element_b), Some(ON));
        assert_eq!(broker.get_required_level(&element_c), Some(ON));
        assert_eq!(broker.get_lease_status(&lease_b_reacquired.id), Some(LeaseStatus::Satisfied));
        assert_eq!(broker.get_lease_status(&lease_c.id), Some(LeaseStatus::Satisfied));
        assert_eq!(broker.is_lease_contingent(&lease_b_reacquired.id), false);
        assert_eq!(broker.is_lease_contingent(&lease_c.id), false);

        // Drop reacquired lease on B.
        // A's required level should remain ON.
        // B's required level should become OFF because it is no longer leased.
        // C's required level should become OFF because it now pending and contingent.
        // Lease C should now be pending and contingent.
        broker.drop_lease(&lease_b_reacquired.id).expect("drop_lease failed");
        assert_eq!(broker.get_required_level(&element_a), Some(ON));
        assert_eq!(broker.get_required_level(&element_b), Some(OFF));
        assert_eq!(broker.get_required_level(&element_c), Some(OFF));
        assert_eq!(broker.get_lease_status(&lease_c.id), Some(LeaseStatus::Pending));
        assert_eq!(broker.is_lease_contingent(&lease_c.id), true);

        // Drop lease on C.
        // A's required level should become OFF.
        // B's required level should remain OFF.
        // C's required level should remain OFF because its lease is pending and contingent.
        broker.drop_lease(&lease_c.id).expect("drop_lease failed");
        assert_eq!(broker.get_required_level(&element_a), Some(OFF));
        assert_eq!(broker.get_required_level(&element_b), Some(OFF));
        assert_eq!(broker.get_required_level(&element_c), Some(OFF));

        // All leases should be cleaned up.
        assert_lease_cleaned_up(&broker.catalog, &lease_b_id);
        assert_lease_cleaned_up(&broker.catalog, &lease_c_id);
        assert_lease_cleaned_up(&broker.catalog, &lease_b_reacquired_id);
    }

    #[fuchsia::test]
    async fn test_lease_opportunistic_contingent() {
        // Tests that a lease with a opportunistic claim does not affect required
        // levels if it has not yet been satisfied.
        //
        // B has an assertive dependency on A @ 3.
        // C has a opportunistic dependency on A @ 2.
        // D has an assertive dependency on A @ 1.
        //  A     B     C     D
        //  3 <== 1
        //  2 <-------- 1
        //  1 <============== 1
        let inspect = fuchsia_inspect::component::inspector();
        let inspect_node = inspect.root().create_child("test");
        let mut broker = Broker::new(inspect_node);
        let token_a_assertive = DependencyToken::create();
        let token_a_opportunistic = DependencyToken::create();
        let element_a = broker
            .add_element(
                "A",
                0,
                vec![0, 1, 2, 3],
                vec![],
                vec![token_a_assertive
                    .duplicate_handle(zx::Rights::SAME_RIGHTS)
                    .expect("dup failed")
                    .into()],
                vec![token_a_opportunistic
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
                    requires_token: token_a_assertive
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
                    requires_token: token_a_opportunistic
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
                    requires_token: token_a_assertive
                        .duplicate_handle(zx::Rights::SAME_RIGHTS)
                        .expect("dup failed")
                        .into(),
                    requires_level: 1,
                }],
                vec![],
                vec![],
            )
            .expect("add_element failed");
        let mut broker_status = BrokerStatusMatcher::new();

        // Initial required level for all elements should be 0.
        // Set all current levels to 0.
        broker_status.required_level.update(&element_a, ZERO);
        broker_status.required_level.update(&element_b, ZERO);
        broker_status.required_level.update(&element_c, ZERO);
        broker_status.required_level.update(&element_d, ZERO);
        broker_status.assert_matches(&broker);
        broker.update_current_level(&element_a, ZERO);
        broker.update_current_level(&element_b, ZERO);
        broker.update_current_level(&element_c, ZERO);
        broker.update_current_level(&element_d, ZERO);

        // Lease C.
        let lease_c = broker.acquire_lease(&element_c, ONE).expect("acquire failed");
        let lease_c_id = lease_c.id.clone();
        // A's required level should remain 0 despite C's opportunistic claim.
        // B's required level should remain 0.
        // C's required level should remain 0 because its lease is pending and contingent.
        // D's required level should remain 0.
        broker_status.assert_matches(&broker);
        // Lease C should be pending and contingent on its passive claim.
        assert_eq!(broker.get_lease_status(&lease_c.id), Some(LeaseStatus::Pending));
        assert_eq!(broker.is_lease_contingent(&lease_c.id), true);

        // Lease B.
        let lease_b = broker.acquire_lease(&element_b, ONE).expect("acquire failed");
        let lease_b_id = lease_b.id.clone();
        // A's required level should become 3 because of B's assertive claim.
        // B's required level should remain 0 because its dependency is not yet satisfied.
        // C's required level should remain 0 because its dependency is not yet satisfied.
        // D's required level should remain 0.
        broker_status.required_level.update(&element_a, THREE);
        broker_status.assert_matches(&broker);
        // Lease B should be pending.
        // Lease C should remain pending but no longer contingent because
        // B's assertive claim would satisfy C's opportunistic claim.
        assert_eq!(broker.get_lease_status(&lease_b.id), Some(LeaseStatus::Pending));
        assert_eq!(broker.get_lease_status(&lease_c.id), Some(LeaseStatus::Pending));
        assert_eq!(broker.is_lease_contingent(&lease_c.id), false);

        // Update A's current level to 3.
        broker.update_current_level(&element_a, THREE);
        // A's required level should remain 3.
        // B's required level should become 1 because its dependency is now satisfied.
        // C's required level should become 1 because its dependency is now satisfied.
        // D's required level should remain 0.
        broker_status.required_level.update(&element_b, ONE);
        broker_status.required_level.update(&element_c, ONE);
        broker_status.assert_matches(&broker);
        // Lease B & C should become satisfied.
        assert_eq!(broker.get_lease_status(&lease_b.id), Some(LeaseStatus::Satisfied));
        assert_eq!(broker.get_lease_status(&lease_c.id), Some(LeaseStatus::Satisfied));
        assert_eq!(broker.is_lease_contingent(&lease_c.id), false);

        // Drop Lease on B.
        broker.drop_lease(&lease_b.id).expect("drop_lease failed");
        // A's required level should drop to 2 until C drops its opportunistic claim.
        // B's required level should become 0 because it is no longer leased.
        // C's required level should become 0 because it is now pending and contingent.
        // D's required level should remain 0.
        broker_status.required_level.update(&element_a, TWO);
        broker_status.required_level.update(&element_b, ZERO);
        broker_status.required_level.update(&element_c, ZERO);
        broker_status.assert_matches(&broker);
        // Lease C should now be pending and contingent.
        assert_eq!(broker.get_lease_status(&lease_c.id), Some(LeaseStatus::Pending));
        assert_eq!(broker.is_lease_contingent(&lease_c.id), true);

        // Update A's current level to 2.
        // A's required level should remain 2.
        // B's required level should remain 0.
        // C's required level should remain 0.
        // D's required level should remain 0.
        broker_status.assert_matches(&broker);
        // Lease C should still be pending and contingent.
        assert_eq!(broker.get_lease_status(&lease_c.id), Some(LeaseStatus::Pending));
        assert_eq!(broker.is_lease_contingent(&lease_c.id), true);

        // Drop Lease on C.
        broker.drop_lease(&lease_c.id).expect("drop_lease failed");
        // A's required level should become 0.
        // B's required level should remain 0.
        // C's required level should remain 0.
        // D's required level should remain 0.
        broker_status.required_level.update(&element_a, ZERO);
        broker_status.assert_matches(&broker);

        // Reacquire Lease on C.
        // The lease on C should remain Pending even though A's current level is 2.
        let lease_c_reacquired = broker.acquire_lease(&element_c, ONE).expect("acquire failed");
        let lease_c_reacquired_id = lease_c_reacquired.id.clone();
        // A's required level should remain 0 despite C's new opportunistic claim.
        // B's required level should remain 0.
        // C's required level should remain 0.
        // D's required level should remain 0.
        broker_status.assert_matches(&broker);
        assert_eq!(broker.get_lease_status(&lease_c_reacquired.id), Some(LeaseStatus::Pending));
        assert_eq!(broker.is_lease_contingent(&lease_c_reacquired.id), true);

        // Acquire Lease on D.
        let lease_d = broker.acquire_lease(&element_d, ONE).expect("acquire failed");
        let lease_d_id = lease_d.id.clone();
        // A's required level should become 1, but this is not enough to
        // satisfy C's opportunistic claim.
        // B's required level should remain 0.
        // C's required level should remain 0 even though A's current level is 2.
        // D's required level should become 1 immediately because its dependency is already satisfied.
        broker_status.required_level.update(&element_a, ONE);
        broker_status.required_level.update(&element_d, ONE);
        broker_status.assert_matches(&broker);
        // The lease on D should immediately be satisfied by A's current level of 2.
        // The lease on C should remain Pending even though A's current level is 2.
        assert_eq!(broker.get_lease_status(&lease_c_reacquired.id), Some(LeaseStatus::Pending));
        assert_eq!(broker.get_lease_status(&lease_d.id), Some(LeaseStatus::Satisfied));
        assert_eq!(broker.is_lease_contingent(&lease_c_reacquired.id), true);

        // Update A's current level to 1.
        broker.update_current_level(&element_a, ONE);
        // A's required level should remain 1.
        // B's required level should remain 0.
        // C's required level should remain 0.
        // D's required level should remain 1.
        broker_status.assert_matches(&broker);
        // The lease on C should remain Pending.
        // The lease on D should still be satisfied.
        assert_eq!(broker.get_lease_status(&lease_c_reacquired.id), Some(LeaseStatus::Pending));
        assert_eq!(broker.is_lease_contingent(&lease_c_reacquired.id), true);
        assert_eq!(broker.get_lease_status(&lease_d.id), Some(LeaseStatus::Satisfied));

        // Drop reacquired lease on C.
        broker.drop_lease(&lease_c_reacquired.id).expect("drop_lease failed");
        // A's required level should remain 1.
        // B's required level should remain 0.
        // C's required level should remain 0.
        // D's required level should remain 1.
        broker_status.assert_matches(&broker);
        // The lease on D should still be satisfied.
        assert_eq!(broker.get_lease_status(&lease_d.id), Some(LeaseStatus::Satisfied));

        // Drop lease on D.
        broker.drop_lease(&lease_d.id).expect("drop_lease failed");
        // A's required level should become 0.
        // B's required level should remain 0.
        // C's required level should remain 0.
        // D's required level should become 0 because it is no longer leased.
        broker_status.required_level.update(&element_a, ZERO);
        broker_status.required_level.update(&element_d, ZERO);
        broker_status.assert_matches(&broker);

        // All leases should be cleaned up.
        assert_lease_cleaned_up(&broker.catalog, &lease_b_id);
        assert_lease_cleaned_up(&broker.catalog, &lease_c_id);
        assert_lease_cleaned_up(&broker.catalog, &lease_c_reacquired_id);
        assert_lease_cleaned_up(&broker.catalog, &lease_d_id);
    }

    #[fuchsia::test]
    async fn test_lease_opportunistic_levels() {
        // Tests that a lease with a opportunistic claim remains unsatisfied
        // if a lease with an assertive but lower level claim on the required
        // element is added.
        //
        // B @ 10 has an assertive dependency on A @ 1.
        // B @ 20 has an assertive dependency on A @ 1.
        // C @ 5 has a opportunistic dependency on A @ 2.
        //  A     B     C
        //  2 <= 20
        //  1 <= 10
        //  2 <-------- 5
        let inspect = fuchsia_inspect::component::inspector();
        let inspect_node = inspect.root().create_child("test");
        let mut broker = Broker::new(inspect_node);
        let token_a_assertive = DependencyToken::create();
        let token_a_opportunistic = DependencyToken::create();
        let element_a = broker
            .add_element(
                "A",
                0,
                vec![0, 1, 2],
                vec![],
                vec![token_a_assertive
                    .duplicate_handle(zx::Rights::SAME_RIGHTS)
                    .expect("dup failed")
                    .into()],
                vec![token_a_opportunistic
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
                        requires_token: token_a_assertive
                            .duplicate_handle(zx::Rights::SAME_RIGHTS)
                            .expect("dup failed")
                            .into(),
                        requires_level: 1,
                    },
                    fpb::LevelDependency {
                        dependency_type: DependencyType::Active,
                        dependent_level: 20,
                        requires_token: token_a_assertive
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
                    requires_token: token_a_opportunistic
                        .duplicate_handle(zx::Rights::SAME_RIGHTS)
                        .expect("dup failed")
                        .into(),
                    requires_level: 2,
                }],
                vec![],
                vec![],
            )
            .expect("add_element failed");

        let mut broker_status = BrokerStatusMatcher::new();

        // Initial required level for A, B & C should be OFF.
        // Set A, B & C's current level to OFF.
        broker_status.required_level.update(&element_a, ZERO);
        broker_status.required_level.update(&element_b, ZERO);
        broker_status.required_level.update(&element_c, ZERO);
        broker.update_current_level(&element_a, ZERO);
        broker.update_current_level(&element_b, ZERO);
        broker.update_current_level(&element_c, ZERO);
        broker_status.assert_matches(&broker);

        // Lease C @ 5.
        // A's required level should remain 0 because of C's opportunistic claim.
        // B's required level should remain 0.
        // C's required level should remain 0 because its lease is pending and contingent.
        // Lease C should be pending because it is contingent on a opportunistic claim.
        let lease_c = broker
            .acquire_lease(&element_c, IndexedPowerLevel { level: 5, index: 1 })
            .expect("acquire failed");
        let lease_c_id = lease_c.id.clone();
        broker_status.assert_matches(&broker);
        assert_eq!(broker.get_lease_status(&lease_c.id), Some(LeaseStatus::Pending));
        assert_eq!(broker.is_lease_contingent(&lease_c.id), true);

        // Lease B @ 10.
        // A's required level should become 1 because of B's assertive claim.
        // B's required level should remain 0 because its dependency is not yet satisfied.
        // C's required level should remain 0 because its lease is pending and contingent.
        // Lease B @ 10 should be pending.
        // Lease C should remain pending because A @ 1 does not satisfy its claim.
        let lease_b_10 = broker
            .acquire_lease(&element_b, IndexedPowerLevel { level: 10, index: 1 })
            .expect("acquire failed");
        let lease_b_10_id = lease_b_10.id.clone();
        broker_status.required_level.update(&element_a, ONE);
        broker_status.assert_matches(&broker);
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
        broker.update_current_level(&element_a, ONE);
        broker_status.required_level.update(&element_b, IndexedPowerLevel { level: 10, index: 1 });
        broker_status.assert_matches(&broker);
        assert_eq!(broker.get_lease_status(&lease_b_10.id), Some(LeaseStatus::Satisfied));
        assert_eq!(broker.get_lease_status(&lease_c.id), Some(LeaseStatus::Pending));
        assert_eq!(broker.is_lease_contingent(&lease_c.id), true);

        // Lease B @ 20.
        // A's required level should become 2 because of the new lease's assertive claim.
        // B's required level should remain 10 because the new lease's claim is not yet satisfied.
        // C's required level should remain 0 because its dependency is not yet satisfied.
        // Lease B @ 10 should still be satisfied.
        // Lease B @ 20 should be pending.
        // Lease C should remain pending because A is not yet at 2, but
        // should no longer be contingent on its opportunistic claim.
        let lease_b_20 = broker
            .acquire_lease(&element_b, IndexedPowerLevel { level: 20, index: 2 })
            .expect("acquire failed");
        let lease_b_20_id = lease_b_20.id.clone();
        broker_status.required_level.update(&element_a, TWO);
        broker_status.assert_matches(&broker);
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
        // Lease C should become satisfied because A @ 2 satisfies its opportunistic claim.
        broker.update_current_level(&element_a, TWO);
        broker_status.required_level.update(&element_b, IndexedPowerLevel { level: 20, index: 2 });
        broker_status.required_level.update(&element_c, IndexedPowerLevel { level: 5, index: 1 });
        broker_status.assert_matches(&broker);
        assert_eq!(broker.get_lease_status(&lease_b_10.id), Some(LeaseStatus::Satisfied));
        assert_eq!(broker.get_lease_status(&lease_b_20.id), Some(LeaseStatus::Satisfied));
        assert_eq!(broker.get_lease_status(&lease_c.id), Some(LeaseStatus::Satisfied));
        assert_eq!(broker.is_lease_contingent(&lease_c.id), false);

        // Drop Lease on B @ 20.
        // A's required level should remain 2 because of C's opportunistic claim.
        // B's required level should become 10 because the higher lease has been dropped.
        // C's required level should become 0 because its lease is now pending and contingent.
        // Lease B @ 10 should still be satisfied.
        // Lease C should now be pending and contingent.
        broker.drop_lease(&lease_b_20.id).expect("drop_lease failed");
        broker_status.required_level.update(&element_b, IndexedPowerLevel { level: 10, index: 1 });
        broker_status.required_level.update(&element_c, ZERO);
        broker_status.assert_matches(&broker);
        assert_eq!(broker.get_lease_status(&lease_b_10.id), Some(LeaseStatus::Satisfied));
        assert_eq!(broker.get_lease_status(&lease_c.id), Some(LeaseStatus::Pending));
        assert_eq!(broker.is_lease_contingent(&lease_c.id), true);

        // Drop Lease on C.
        // A's required level should become 1 because C has dropped its claim,
        // but B's lower lease is still being held.
        // B's required level should remain 10.
        // C's required level should remain 0.
        // Lease B @ 10 should still be satisfied.
        broker.drop_lease(&lease_c.id).expect("drop_lease failed");
        broker_status.required_level.update(&element_a, ONE);
        broker_status.assert_matches(&broker);
        assert_eq!(broker.get_lease_status(&lease_b_10.id), Some(LeaseStatus::Satisfied));

        // Update A's current level to 1.
        // A's required level should remain 1.
        // B's required level should remain 10.
        // C's required level should remain 0.
        // Lease B @ 10 should still be satisfied.
        broker.update_current_level(&element_a, ONE);
        broker_status.assert_matches(&broker);
        assert_eq!(broker.get_lease_status(&lease_b_10.id), Some(LeaseStatus::Satisfied));

        // Drop Lease on B @ 10.
        // A's required level should drop to 0 because all claims have been dropped.
        // B's required level should become 0 because its leases have been dropped.
        // C's required level should remain 0.
        broker.drop_lease(&lease_b_10.id).expect("drop_lease failed");
        broker_status.required_level.update(&element_a, ZERO);
        broker_status.required_level.update(&element_b, ZERO);
        broker_status.required_level.update(&element_c, ZERO);
        broker_status.assert_matches(&broker);

        // Update A's current level to 0.
        // A's required level should remain 0.
        // B's required level should remain 0.
        // C's required level should remain 0.
        broker.update_current_level(&element_a, ZERO);
        broker_status.assert_matches(&broker);

        // Leases should be cleaned up.
        assert_lease_cleaned_up(&broker.catalog, &lease_b_10_id);
        assert_lease_cleaned_up(&broker.catalog, &lease_b_20_id);
        assert_lease_cleaned_up(&broker.catalog, &lease_c_id);
    }

    #[fuchsia::test]
    async fn test_lease_opportunistic_independent_assertive() {
        // Tests that independent assertive claims of a lease are not activated
        // while a lease is Contingent (has one or more opportunistic claims
        // but no other leases have assertive claims that would satisfy them).
        //
        // B has an assertive dependency on A.
        // C has a opportunistic dependency on A.
        // C has an assertive dependency on D.
        //  A     B     C     D
        // ON <= ON
        // ON <------- ON => ON
        let inspect = fuchsia_inspect::component::inspector();
        let inspect_node = inspect.root().create_child("test");
        let mut broker = Broker::new(inspect_node);
        let token_a_assertive = DependencyToken::create();
        let token_a_opportunistic = DependencyToken::create();
        let element_a = broker
            .add_element(
                "A",
                OFF.level,
                BINARY_POWER_LEVELS.to_vec(),
                vec![],
                vec![token_a_assertive
                    .duplicate_handle(zx::Rights::SAME_RIGHTS)
                    .expect("dup failed")
                    .into()],
                vec![token_a_opportunistic
                    .duplicate_handle(zx::Rights::SAME_RIGHTS)
                    .expect("dup failed")
                    .into()],
            )
            .expect("add_element failed");
        let element_b = broker
            .add_element(
                "B",
                OFF.level,
                BINARY_POWER_LEVELS.to_vec(),
                vec![fpb::LevelDependency {
                    dependency_type: DependencyType::Active,
                    dependent_level: ON.level,
                    requires_token: token_a_assertive
                        .duplicate_handle(zx::Rights::SAME_RIGHTS)
                        .expect("dup failed")
                        .into(),
                    requires_level: ON.level,
                }],
                vec![],
                vec![],
            )
            .expect("add_element failed");
        let token_d_assertive = DependencyToken::create();
        let element_d = broker
            .add_element(
                "D",
                OFF.level,
                BINARY_POWER_LEVELS.to_vec(),
                vec![],
                vec![token_d_assertive
                    .duplicate_handle(zx::Rights::SAME_RIGHTS)
                    .expect("dup failed")
                    .into()],
                vec![],
            )
            .expect("add_element failed");
        let element_c = broker
            .add_element(
                "C",
                OFF.level,
                BINARY_POWER_LEVELS.to_vec(),
                vec![
                    fpb::LevelDependency {
                        dependency_type: DependencyType::Passive,
                        dependent_level: ON.level,
                        requires_token: token_a_opportunistic
                            .duplicate_handle(zx::Rights::SAME_RIGHTS)
                            .expect("dup failed")
                            .into(),
                        requires_level: ON.level,
                    },
                    fpb::LevelDependency {
                        dependency_type: DependencyType::Active,
                        dependent_level: ON.level,
                        requires_token: token_d_assertive
                            .duplicate_handle(zx::Rights::SAME_RIGHTS)
                            .expect("dup failed")
                            .into(),
                        requires_level: ON.level,
                    },
                ],
                vec![],
                vec![],
            )
            .expect("add_element failed");

        let mut broker_status = BrokerStatusMatcher::new();

        // Initial required level for all elements should be OFF.
        // Set all current level to OFF.
        broker_status.required_level.update(&element_a, OFF);
        broker_status.required_level.update(&element_b, OFF);
        broker_status.required_level.update(&element_c, OFF);
        broker_status.required_level.update(&element_d, OFF);
        broker.update_current_level(&element_a, OFF);
        broker.update_current_level(&element_b, OFF);
        broker.update_current_level(&element_c, OFF);
        broker.update_current_level(&element_d, OFF);
        broker_status.assert_matches(&broker);

        // Lease C.
        // A's required level should remain OFF because C's opportunistic claim
        // does not raise the required level of A.
        // B's required level should remain OFF.
        // C's required level should remain OFF because its lease is pending and contingent.
        // D's required level should remain OFF C's opportunistic claim on A has
        // no other assertive claims that would satisfy it.
        // Lease C should be pending and contingent because its opportunistic
        // claim has no other assertive claim that would satisfy it.
        let lease_c = broker.acquire_lease(&element_c, ON).expect("acquire failed");
        let lease_c_id = lease_c.id.clone();
        broker_status.assert_matches(&broker);
        assert_eq!(broker.get_lease_status(&lease_c.id), Some(LeaseStatus::Pending));
        assert_eq!(broker.is_lease_contingent(&lease_c.id), true);

        // Lease B.
        // A's required level should become ON because of B's assertive claim.
        // B's required level should remain OFF because its dependency is not yet satisfied.
        // C's required level should remain OFF because its dependencies are not yet satisfied.
        // D's required level should become ON because C's opportunistic claim would
        // be satisfied by B's assertive claim.
        // Lease B should be pending.
        // Lease C should be pending but no longer contingent because B's
        // assertive claim would satisfy C's opportunistic claim.
        let lease_b = broker.acquire_lease(&element_b, ON).expect("acquire failed");
        let lease_b_id = lease_b.id.clone();
        broker_status.required_level.update(&element_a, ON);
        broker_status.required_level.update(&element_d, ON);
        broker_status.assert_matches(&broker);
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
        broker.update_current_level(&element_a, ON);
        broker_status.required_level.update(&element_b, ON);
        broker_status.assert_matches(&broker);
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
        broker.update_current_level(&element_d, ON);
        broker_status.required_level.update(&element_c, ON);
        broker_status.assert_matches(&broker);
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
        broker_status.required_level.update(&element_b, OFF);
        broker_status.required_level.update(&element_c, OFF);
        broker_status.assert_matches(&broker);
        assert_eq!(broker.get_lease_status(&lease_c.id), Some(LeaseStatus::Pending));
        assert_eq!(broker.is_lease_contingent(&lease_c.id), true);

        // Drop Lease on C.
        // A's required level should become OFF because C has dropped its opportunistic claim.
        // B's required level should remain OFF.
        // C's required level should remain OFF.
        // D's required level should become OFF because C has dropped its assertive claim.
        broker.drop_lease(&lease_c.id).expect("drop_lease failed");
        broker_status.required_level.update(&element_a, OFF);
        broker_status.required_level.update(&element_d, OFF);
        broker_status.assert_matches(&broker);

        // Update A's current level to OFF.
        // All required levels should remain OFF.
        broker.update_current_level(&element_a, OFF);
        broker_status.assert_matches(&broker);

        // Update D's current level to OFF.
        // All required levels should remain OFF.
        broker.update_current_level(&element_d, OFF);
        broker_status.assert_matches(&broker);

        // Leases C & B should be cleaned up.
        assert_lease_cleaned_up(&broker.catalog, &lease_b_id);
        assert_lease_cleaned_up(&broker.catalog, &lease_c_id);
    }

    #[fuchsia::test]
    async fn test_assertive_claims_do_not_satisfy_opportunistic_claims_while_contingent() {
        // Test that assertive claims from a contingent lease do not satisfy opportunistic claims until that
        // lease is made non-contingent.
        //
        // B has an assertive dependency on C.
        // D has an assertive dependency on A and a opportunistic dependency on C.
        // E has a opportunistic dependency on A.
        //  A     B     C     D     E
        //       ON => ON
        // ON <============= ON
        //             ON <- ON
        // ON <------------------- ON
        let inspect = fuchsia_inspect::component::inspector();
        let inspect_node = inspect.root().create_child("test");
        let mut broker = Broker::new(inspect_node);
        let token_a_assertive = DependencyToken::create();
        let token_a_opportunistic = DependencyToken::create();
        let element_a = broker
            .add_element(
                "A",
                OFF.level,
                BINARY_POWER_LEVELS.to_vec(),
                vec![],
                vec![token_a_assertive
                    .duplicate_handle(zx::Rights::SAME_RIGHTS)
                    .expect("dup failed")
                    .into()],
                vec![token_a_opportunistic
                    .duplicate_handle(zx::Rights::SAME_RIGHTS)
                    .expect("dup failed")
                    .into()],
            )
            .expect("add_element failed");
        let token_c_assertive = DependencyToken::create();
        let token_c_opportunistic = DependencyToken::create();
        let element_c = broker
            .add_element(
                "C",
                OFF.level,
                BINARY_POWER_LEVELS.to_vec(),
                vec![],
                vec![token_c_assertive
                    .duplicate_handle(zx::Rights::SAME_RIGHTS)
                    .expect("dup failed")
                    .into()],
                vec![token_c_opportunistic
                    .duplicate_handle(zx::Rights::SAME_RIGHTS)
                    .expect("dup failed")
                    .into()],
            )
            .expect("add_element failed");
        let token_b_assertive = DependencyToken::create();
        let element_b = broker
            .add_element(
                "B",
                OFF.level,
                BINARY_POWER_LEVELS.to_vec(),
                vec![fpb::LevelDependency {
                    dependency_type: DependencyType::Active,
                    dependent_level: ON.level,
                    requires_token: token_c_assertive
                        .duplicate_handle(zx::Rights::SAME_RIGHTS)
                        .expect("dup failed")
                        .into(),
                    requires_level: ON.level,
                }],
                vec![token_b_assertive
                    .duplicate_handle(zx::Rights::SAME_RIGHTS)
                    .expect("dup failed")
                    .into()],
                vec![],
            )
            .expect("add_element failed");
        let element_d = broker
            .add_element(
                "D",
                OFF.level,
                BINARY_POWER_LEVELS.to_vec(),
                vec![
                    fpb::LevelDependency {
                        dependency_type: DependencyType::Active,
                        dependent_level: ON.level,
                        requires_token: token_a_assertive
                            .duplicate_handle(zx::Rights::SAME_RIGHTS)
                            .expect("dup failed")
                            .into(),
                        requires_level: ON.level,
                    },
                    fpb::LevelDependency {
                        dependency_type: DependencyType::Passive,
                        dependent_level: ON.level,
                        requires_token: token_c_opportunistic
                            .duplicate_handle(zx::Rights::SAME_RIGHTS)
                            .expect("dup failed")
                            .into(),
                        requires_level: ON.level,
                    },
                ],
                vec![],
                vec![],
            )
            .expect("add_element failed");
        let element_e = broker
            .add_element(
                "E",
                OFF.level,
                BINARY_POWER_LEVELS.to_vec(),
                vec![fpb::LevelDependency {
                    dependency_type: DependencyType::Passive,
                    dependent_level: ON.level,
                    requires_token: token_a_opportunistic
                        .duplicate_handle(zx::Rights::SAME_RIGHTS)
                        .expect("dup failed")
                        .into(),
                    requires_level: ON.level,
                }],
                vec![],
                vec![],
            )
            .expect("add_element failed");

        let mut broker_status = BrokerStatusMatcher::new();

        // Initial required levels for all elements should be OFF.
        // Set all current levels to OFF.
        broker.update_current_level(&element_a, OFF);
        broker.update_current_level(&element_b, OFF);
        broker.update_current_level(&element_c, OFF);
        broker.update_current_level(&element_d, OFF);
        broker.update_current_level(&element_e, OFF);
        broker_status.required_level.update(&element_a, OFF);
        broker_status.required_level.update(&element_b, OFF);
        broker_status.required_level.update(&element_c, OFF);
        broker_status.required_level.update(&element_d, OFF);
        broker_status.required_level.update(&element_e, OFF);
        broker_status.assert_matches(&broker);

        // Lease E.
        // A's required level should remain OFF because of E's opportunistic claim.
        // B, C, D & E's required level are unaffected and should remain OFF.
        // Lease E should be pending and contingent on its opportunistic claim.
        let lease_e = broker.acquire_lease(&element_e, ON).expect("acquire failed");
        let lease_e_id = lease_e.id.clone();
        broker_status.assert_matches(&broker);
        assert_eq!(broker.get_lease_status(&lease_e.id), Some(LeaseStatus::Pending));
        assert_eq!(broker.is_lease_contingent(&lease_e.id), true);

        // Lease D.
        // A's required level should remain OFF. While Lease D has an assertive claim on A, it is not
        // activated until it is not contingent on C.
        // C's required level should remain OFF because of D's opportunistic claim.
        // B, D & E's required level are unaffected and should remain OFF.
        // Lease D should be pending and contingent on its opportunistic claim.
        // Lease E should be pending and contingent on its opportunistic claim, which is not satisfied
        // by Lease D's assertive claim, as Lease D is contingent.
        let lease_d = broker.acquire_lease(&element_d, ON).expect("acquire failed");
        let lease_d_id = lease_d.id.clone();
        broker_status.assert_matches(&broker);
        assert_eq!(broker.get_lease_status(&lease_d.id), Some(LeaseStatus::Pending));
        assert_eq!(broker.get_lease_status(&lease_e.id), Some(LeaseStatus::Pending));
        assert_eq!(broker.is_lease_contingent(&lease_d.id), true);
        assert_eq!(broker.is_lease_contingent(&lease_e.id), true);

        // Lease B.
        // A should have required level ON, because D is no longer contingent on C.
        // C should have required level ON, because of B's assertive claim.
        // B, D & E's required level are unaffected and should remain OFF.
        // Lease B should be pending and not contingent.
        // Lease D should be pending and not contingent.
        // Lease E should be pending and not contingent.
        let lease_b = broker.acquire_lease(&element_b, ON).expect("acquire failed");
        let lease_b_id = lease_b.id.clone();
        broker_status.required_level.update(&element_a, ON);
        broker_status.required_level.update(&element_c, ON);
        broker_status.assert_matches(&broker);
        assert_eq!(broker.get_lease_status(&lease_b.id), Some(LeaseStatus::Pending));
        assert_eq!(broker.get_lease_status(&lease_d.id), Some(LeaseStatus::Pending));
        assert_eq!(broker.get_lease_status(&lease_e.id), Some(LeaseStatus::Pending));
        assert_eq!(broker.is_lease_contingent(&lease_b.id), false);
        assert_eq!(broker.is_lease_contingent(&lease_d.id), false);
        assert_eq!(broker.is_lease_contingent(&lease_e.id), false);

        // Update C's current level to ON.
        // A should still have required level ON.
        // B should have required level ON, now that it's lease is satisfied.
        // C should still have required level ON.
        // D & E's required level are unaffected and should remain OFF.
        // Lease B should now be satisfied and not contingent.
        // Lease D and E should be pending as A is not yet ON, and not contingent.
        broker.update_current_level(&element_c, ON);
        broker_status.required_level.update(&element_b, ON);
        broker_status.assert_matches(&broker);
        assert_eq!(broker.get_lease_status(&lease_b.id), Some(LeaseStatus::Satisfied));
        assert_eq!(broker.get_lease_status(&lease_d.id), Some(LeaseStatus::Pending));
        assert_eq!(broker.get_lease_status(&lease_e.id), Some(LeaseStatus::Pending));
        assert_eq!(broker.is_lease_contingent(&lease_b.id), false);
        assert_eq!(broker.is_lease_contingent(&lease_d.id), false);
        assert_eq!(broker.is_lease_contingent(&lease_e.id), false);

        // Update A's current level to ON.
        // A should still have required level ON.
        // B should still have required level ON.
        // C should still have required level ON.
        // D & E's required level should be ON, now that their leases are satisfied.
        // Lease B, D and E should now all be satisfied.
        broker.update_current_level(&element_a, ON);
        broker_status.required_level.update(&element_d, ON);
        broker_status.required_level.update(&element_e, ON);
        broker_status.assert_matches(&broker);
        assert_eq!(broker.get_lease_status(&lease_b.id), Some(LeaseStatus::Satisfied));
        assert_eq!(broker.get_lease_status(&lease_d.id), Some(LeaseStatus::Satisfied));
        assert_eq!(broker.get_lease_status(&lease_e.id), Some(LeaseStatus::Satisfied));
        assert_eq!(broker.is_lease_contingent(&lease_b.id), false);
        assert_eq!(broker.is_lease_contingent(&lease_d.id), false);
        assert_eq!(broker.is_lease_contingent(&lease_e.id), false);

        // Drop lease for B.
        // A should still have required level ON.
        // C should still have required level ON.
        // B, D & E's required levels are now OFF as their leases are no longer satisfied.
        // Lease D should drop to pending and contingent.
        // Lease E should drop to pending and contingent.
        broker.drop_lease(&lease_b.id).expect("drop_lease failed");
        broker_status.required_level.update(&element_b, OFF);
        broker_status.required_level.update(&element_d, OFF);
        broker_status.required_level.update(&element_e, OFF);
        broker_status.assert_matches(&broker);
        assert_eq!(broker.get_lease_status(&lease_d.id), Some(LeaseStatus::Pending));
        assert_eq!(broker.get_lease_status(&lease_e.id), Some(LeaseStatus::Pending));
        assert_eq!(broker.is_lease_contingent(&lease_d.id), true);
        assert_eq!(broker.is_lease_contingent(&lease_e.id), true);

        // Drop lease for D.
        // A should still have required level ON.
        // C should have required level OFF (no leases require C anymore).
        // B, D & E's required level are unaffected and should remain OFF.
        // Lease E should remain at pending and contingent.
        broker.drop_lease(&lease_d.id).expect("drop_lease failed");
        broker_status.required_level.update(&element_c, OFF);
        broker_status.assert_matches(&broker);
        assert_eq!(broker.get_lease_status(&lease_e.id), Some(LeaseStatus::Pending));
        assert_eq!(broker.is_lease_contingent(&lease_e.id), true);

        // Drop lease for E.
        // A should have required level OFF.
        // C should still have required level OFF.
        // B, D & E's required level are unaffected and should remain OFF.
        broker.drop_lease(&lease_e.id).expect("drop_lease failed");
        broker_status.required_level.update(&element_a, OFF);
        broker_status.assert_matches(&broker);

        // Leases B, D and E should be cleaned up.
        assert_lease_cleaned_up(&broker.catalog, &lease_b_id);
        assert_lease_cleaned_up(&broker.catalog, &lease_d_id);
        assert_lease_cleaned_up(&broker.catalog, &lease_e_id);
    }

    #[fuchsia::test]
    async fn test_lease_opportunistic_chained() {
        // Tests that assertive dependencies, which depend on opportunistic dependencies,
        // which in turn depend on assertive dependencies, all work as expected.
        //
        // B has an assertive dependency on A.
        // C has a opportunistic dependency on B (and transitively, a opportunistic dependency on A).
        // D has an assertive dependency on B (and transitively, an assertive dependency on A).
        // E has an assertive dependency on C (and transitively, a opportunistic dependency on A & B).
        //  A     B     C     D     E
        // ON <= ON
        //       ON <- ON <======= ON
        //       ON <======= ON
        let inspect = fuchsia_inspect::component::inspector();
        let inspect_node = inspect.root().create_child("test");
        let mut broker = Broker::new(inspect_node);
        let token_a = DependencyToken::create();
        let element_a = broker
            .add_element(
                "A",
                OFF.level,
                BINARY_POWER_LEVELS.to_vec(),
                vec![],
                vec![token_a.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("dup failed").into()],
                vec![],
            )
            .expect("add_element failed");
        let token_b_assertive = DependencyToken::create();
        let token_b_opportunistic = DependencyToken::create();
        let element_b = broker
            .add_element(
                "B",
                OFF.level,
                BINARY_POWER_LEVELS.to_vec(),
                vec![fpb::LevelDependency {
                    dependency_type: DependencyType::Active,
                    dependent_level: ON.level,
                    requires_token: token_a
                        .duplicate_handle(zx::Rights::SAME_RIGHTS)
                        .expect("dup failed")
                        .into(),
                    requires_level: ON.level,
                }],
                vec![token_b_assertive
                    .duplicate_handle(zx::Rights::SAME_RIGHTS)
                    .expect("dup failed")
                    .into()],
                vec![token_b_opportunistic
                    .duplicate_handle(zx::Rights::SAME_RIGHTS)
                    .expect("dup failed")
                    .into()],
            )
            .expect("add_element failed");
        let token_c_assertive = DependencyToken::create();
        let element_c = broker
            .add_element(
                "C",
                OFF.level,
                BINARY_POWER_LEVELS.to_vec(),
                vec![fpb::LevelDependency {
                    dependency_type: DependencyType::Passive,
                    dependent_level: ON.level,
                    requires_token: token_b_opportunistic
                        .duplicate_handle(zx::Rights::SAME_RIGHTS)
                        .expect("dup failed")
                        .into(),
                    requires_level: ON.level,
                }],
                vec![token_c_assertive
                    .duplicate_handle(zx::Rights::SAME_RIGHTS)
                    .expect("dup failed")
                    .into()],
                vec![],
            )
            .expect("add_element failed");
        let element_d = broker
            .add_element(
                "D",
                OFF.level,
                BINARY_POWER_LEVELS.to_vec(),
                vec![fpb::LevelDependency {
                    dependency_type: DependencyType::Active,
                    dependent_level: ON.level,
                    requires_token: token_b_assertive
                        .duplicate_handle(zx::Rights::SAME_RIGHTS)
                        .expect("dup failed")
                        .into(),
                    requires_level: ON.level,
                }],
                vec![],
                vec![],
            )
            .expect("add_element failed");
        let element_e = broker
            .add_element(
                "E",
                OFF.level,
                BINARY_POWER_LEVELS.to_vec(),
                vec![fpb::LevelDependency {
                    dependency_type: DependencyType::Active,
                    dependent_level: ON.level,
                    requires_token: token_c_assertive
                        .duplicate_handle(zx::Rights::SAME_RIGHTS)
                        .expect("dup failed")
                        .into(),
                    requires_level: ON.level,
                }],
                vec![],
                vec![],
            )
            .expect("add_element failed");

        let mut broker_status = BrokerStatusMatcher::new();

        // Initial required levels for all elements should be OFF.
        // Set all current levels to OFF.
        broker.update_current_level(&element_a, OFF);
        broker.update_current_level(&element_b, OFF);
        broker.update_current_level(&element_c, OFF);
        broker.update_current_level(&element_d, OFF);
        broker.update_current_level(&element_e, OFF);
        broker_status.required_level.update(&element_a, OFF);
        broker_status.required_level.update(&element_b, OFF);
        broker_status.required_level.update(&element_c, OFF);
        broker_status.required_level.update(&element_d, OFF);
        broker_status.required_level.update(&element_e, OFF);
        broker_status.assert_matches(&broker);

        // Lease E.
        // A, B, & C's required levels should remain OFF because of C's opportunistic claim.
        // D's required level should remain OFF.
        // E's required level should remain OFF because its lease is pending and contingent.
        // Lease E should be pending and contingent on its opportunistic claim.
        let lease_e = broker.acquire_lease(&element_e, ON).expect("acquire failed");
        let lease_e_id = lease_e.id.clone();
        broker_status.assert_matches(&broker);
        assert_eq!(broker.get_lease_status(&lease_e.id), Some(LeaseStatus::Pending));
        assert_eq!(broker.is_lease_contingent(&lease_e.id), true);

        // Lease D.
        // A's required level should become ON because of D's transitive assertive claim.
        // B's required level should remain OFF because A is not yet ON.
        // C's required level should remain OFF because B is not yet ON.
        // D's required level should remain OFF because B is not yet ON.
        // E's required level should remain OFF because C is not yet ON.
        // Lease D should be pending.
        // Lease E should remain pending but is no longer contingent.
        let lease_d = broker.acquire_lease(&element_d, ON).expect("acquire failed");
        let lease_d_id = lease_d.id.clone();
        broker_status.required_level.update(&element_a, ON);
        broker_status.assert_matches(&broker);
        assert_eq!(broker.get_lease_status(&lease_d.id), Some(LeaseStatus::Pending));
        assert_eq!(broker.get_lease_status(&lease_e.id), Some(LeaseStatus::Pending));
        assert_eq!(broker.is_lease_contingent(&lease_d.id), false);
        assert_eq!(broker.is_lease_contingent(&lease_e.id), false);

        // Update A's current level to ON.
        // A's required level should remain ON.
        // B's required level should become ON because of D's assertive claim and
        // its dependency on A being satisfied.
        // C's required level should remain OFF because B is not ON.
        // D's required level should remain OFF because B is not yet ON.
        // E's required level should remain OFF because C is not yet ON.
        // Lease D & E should remain pending.
        broker.update_current_level(&element_a, ON);
        broker_status.required_level.update(&element_b, ON);
        broker_status.assert_matches(&broker);
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
        broker.update_current_level(&element_b, ON);
        broker_status.required_level.update(&element_c, ON);
        broker_status.required_level.update(&element_d, ON);
        broker_status.assert_matches(&broker);
        assert_eq!(broker.get_lease_status(&lease_d.id), Some(LeaseStatus::Satisfied));
        assert_eq!(broker.get_lease_status(&lease_e.id), Some(LeaseStatus::Pending));
        assert_eq!(broker.is_lease_contingent(&lease_d.id), false);
        assert_eq!(broker.is_lease_contingent(&lease_e.id), false);

        // Update C's current level to ON.
        // A, B, C & D's required level should remain ON.
        // E's required level should become ON because C is now ON.
        // Lease E should become satisfied.
        broker.update_current_level(&element_c, ON);
        broker_status.required_level.update(&element_e, ON);
        broker_status.assert_matches(&broker);
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
        broker_status.required_level.update(&element_d, OFF);
        broker_status.required_level.update(&element_e, OFF);
        broker_status.assert_matches(&broker);
        assert_eq!(broker.get_lease_status(&lease_e.id), Some(LeaseStatus::Pending));
        assert_eq!(broker.is_lease_contingent(&lease_e.id), true);

        // Drop Lease on E.
        // A's required level should remain ON because B is still ON.
        // B's required level should remain ON because C is still ON.
        // C's required level should become OFF because E has dropped its claim.
        // D's required level should remain OFF.
        // E's required level should remain OFF.
        broker.drop_lease(&lease_e.id).expect("drop_lease failed");
        broker_status.required_level.update(&element_c, OFF);
        broker_status.assert_matches(&broker);

        // Update C's current level to OFF.
        // A's required level should remain ON because B is still ON.
        // B's required level should become OFF because C is now OFF.
        // C's required level should remain OFF.
        // D's required level should remain OFF.
        // E's required level should remain OFF.
        broker.update_current_level(&element_c, OFF);
        broker_status.required_level.update(&element_b, OFF);
        broker_status.assert_matches(&broker);

        // Update B's current level to OFF.
        // A's required level should become OFF because B is now OFF.
        // B, C, D & E's required levels should remain OFF.
        broker.update_current_level(&element_b, OFF);
        broker_status.required_level.update(&element_a, OFF);
        broker_status.assert_matches(&broker);

        // Update A's current level to OFF.
        // All required levels should remain OFF.
        broker.update_current_level(&element_b, OFF);
        broker_status.assert_matches(&broker);

        // Leases D and E should be cleaned up.
        assert_lease_cleaned_up(&broker.catalog, &lease_d_id);
        assert_lease_cleaned_up(&broker.catalog, &lease_e_id);
    }

    #[fuchsia::test]
    async fn test_lease_cumulative_implicit_dependency() {
        // Tests that cumulative implicit dependencies are properly resolved when a lease is
        // acquired. Verifies a simple case of assertive dependencies only.
        //
        // A[1] has an assertive dependency on B[1].
        // A[2] has an assertive dependency on C[1].
        //
        // A[2] has an implicit, assertive dependency on B[1].
        //
        //  A     B     C
        //  1 ==> 1
        //  2 ========> 1
        let inspect = fuchsia_inspect::component::inspector();
        let inspect_node = inspect.root().create_child("test");
        let mut broker = Broker::new(inspect_node);

        let v012_u8: Vec<u8> = vec![0, 1, 2];

        let token_b_assertive = DependencyToken::create();
        let token_c_assertive = DependencyToken::create();
        let element_b = broker
            .add_element(
                "B",
                0,
                v012_u8.clone(),
                vec![],
                vec![token_b_assertive
                    .duplicate_handle(zx::Rights::SAME_RIGHTS)
                    .expect("dup failed")
                    .into()],
                vec![],
            )
            .expect("add_element failed");
        let element_c = broker
            .add_element(
                "C",
                0,
                v012_u8.clone(),
                vec![],
                vec![token_c_assertive
                    .duplicate_handle(zx::Rights::SAME_RIGHTS)
                    .expect("dup failed")
                    .into()],
                vec![],
            )
            .expect("add_element failed");
        let element_a = broker
            .add_element(
                "A",
                0,
                v012_u8.clone(),
                vec![
                    fpb::LevelDependency {
                        dependency_type: DependencyType::Active,
                        dependent_level: 1,
                        requires_token: token_b_assertive
                            .duplicate_handle(zx::Rights::SAME_RIGHTS)
                            .expect("dup failed")
                            .into(),
                        requires_level: 1,
                    },
                    fpb::LevelDependency {
                        dependency_type: DependencyType::Active,
                        dependent_level: 2,
                        requires_token: token_c_assertive
                            .duplicate_handle(zx::Rights::SAME_RIGHTS)
                            .expect("dup failed")
                            .into(),
                        requires_level: 1,
                    },
                ],
                vec![],
                vec![],
            )
            .expect("add_element failed");

        let mut broker_status = BrokerStatusMatcher::new();

        // Initial required levels for all elements should be 0.
        // Set all current levels to 0.
        broker.update_current_level(&element_a, ZERO);
        broker.update_current_level(&element_b, ZERO);
        broker.update_current_level(&element_c, ZERO);
        broker_status.required_level.update(&element_a, ZERO);
        broker_status.required_level.update(&element_b, ZERO);
        broker_status.required_level.update(&element_c, ZERO);
        broker_status.assert_matches(&broker);

        // Lease A[2].
        //
        // A has two assertive dependencies, B[1] and C[1].
        //
        // A's required level should not change.
        // B and C's required level should be 1.
        //
        // A's lease is pending and not contingent.
        let lease_a = broker.acquire_lease(&element_a, TWO).expect("acquire failed");
        let lease_a_id = lease_a.id.clone();
        broker_status.required_level.update(&element_b, ONE);
        broker_status.required_level.update(&element_c, ONE);
        broker_status.assert_matches(&broker);
        assert_eq!(broker.get_lease_status(&lease_a.id), Some(LeaseStatus::Pending));
        assert_eq!(broker.is_lease_contingent(&lease_a.id), false);

        // Update B, C's current level to 1.
        //
        // A's current level should now be 2.
        // B and C's current level should not change.
        //
        // A's lease should be satisfied and not contingent.
        broker.update_current_level(&element_b, ONE);
        broker.update_current_level(&element_c, ONE);
        broker_status.required_level.update(&element_a, TWO);
        broker_status.assert_matches(&broker);
        assert_eq!(broker.get_lease_status(&lease_a.id), Some(LeaseStatus::Satisfied));
        assert_eq!(broker.is_lease_contingent(&lease_a.id), false);

        // Drop Lease A.
        //
        // A, B and C's required level should be 0.
        //
        // Lease A should be pending and not contingent.
        broker.drop_lease(&lease_a.id).expect("drop_lease failed");
        broker_status.required_level.update(&element_a, ZERO);
        broker_status.required_level.update(&element_b, ZERO);
        broker_status.required_level.update(&element_c, ZERO);
        broker_status.assert_matches(&broker);

        // All leases should be cleaned up.
        assert_lease_cleaned_up(&broker.catalog, &lease_a_id);
    }

    #[fuchsia::test]
    async fn test_lease_cumulative_implicit_transitive_dependency() {
        // Tests that cumulative implicit transitive dependencies (both opportunistic
        // and assertive) are properly requested when a lease is acquired.
        //
        // A[1] has an assertive dependency on D[1].
        // A[2] has a opportunistic dependency on C[1].
        // A[3] has an assertive dependency on B[2].
        // D[1] has an assertive dependency on E[1].
        // D[1] has a opportunistic dependency on B[1].
        // F[1] has an assertive dependency on C[1].
        //
        // A[3] has an implicit, transitive, opportunistic dependency on B[1].
        // A[3] has an implicit, opportunistic dependency on C[1].
        // A[3] has an implicit, assertive dependency on D[1].
        // A[3] has an implicit, transitive, assertive dependency on E[1].
        //
        //  A     B     C     D     E     F
        //  1 ==============> 1 ==> 1
        //        1 <-------- 1
        //  2 --------> 1 <============== 1
        //  3 ==> 2
        let inspect = fuchsia_inspect::component::inspector();
        let inspect_node = inspect.root().create_child("test");
        let mut broker = Broker::new(inspect_node);

        let v0123_u8: Vec<u8> = vec![0, 1, 2, 3];

        let token_b_assertive = DependencyToken::create();
        let token_b_opportunistic = DependencyToken::create();
        let token_c_assertive = DependencyToken::create();
        let token_c_opportunistic = DependencyToken::create();
        let token_d_assertive = DependencyToken::create();
        let token_e_assertive = DependencyToken::create();
        let element_b = broker
            .add_element(
                "B",
                0,
                v0123_u8.clone(),
                vec![],
                vec![token_b_assertive
                    .duplicate_handle(zx::Rights::SAME_RIGHTS)
                    .expect("dup failed")
                    .into()],
                vec![token_b_opportunistic
                    .duplicate_handle(zx::Rights::SAME_RIGHTS)
                    .expect("dup failed")
                    .into()],
            )
            .expect("add_element failed");
        let element_e = broker
            .add_element(
                "E",
                0,
                v0123_u8.clone(),
                vec![],
                vec![token_e_assertive
                    .duplicate_handle(zx::Rights::SAME_RIGHTS)
                    .expect("dup failed")
                    .into()],
                vec![],
            )
            .expect("add_element failed");
        let element_d = broker
            .add_element(
                "D",
                0,
                v0123_u8.clone(),
                vec![
                    fpb::LevelDependency {
                        dependency_type: DependencyType::Active,
                        dependent_level: 1,
                        requires_token: token_e_assertive
                            .duplicate_handle(zx::Rights::SAME_RIGHTS)
                            .expect("dup failed")
                            .into(),
                        requires_level: 1,
                    },
                    fpb::LevelDependency {
                        dependency_type: DependencyType::Passive,
                        dependent_level: 1,
                        requires_token: token_b_opportunistic
                            .duplicate_handle(zx::Rights::SAME_RIGHTS)
                            .expect("dup failed")
                            .into(),
                        requires_level: 1,
                    },
                ],
                vec![token_d_assertive
                    .duplicate_handle(zx::Rights::SAME_RIGHTS)
                    .expect("dup failed")
                    .into()],
                vec![],
            )
            .expect("add_element failed");
        let element_c = broker
            .add_element(
                "C",
                0,
                v0123_u8.clone(),
                vec![],
                vec![token_c_assertive
                    .duplicate_handle(zx::Rights::SAME_RIGHTS)
                    .expect("dup failed")
                    .into()],
                vec![token_c_opportunistic
                    .duplicate_handle(zx::Rights::SAME_RIGHTS)
                    .expect("dup failed")
                    .into()],
            )
            .expect("add_element failed");
        let element_f = broker
            .add_element(
                "F",
                0,
                v0123_u8.clone(),
                vec![fpb::LevelDependency {
                    dependency_type: DependencyType::Active,
                    dependent_level: 1,
                    requires_token: token_c_assertive
                        .duplicate_handle(zx::Rights::SAME_RIGHTS)
                        .expect("dup failed")
                        .into(),
                    requires_level: 1,
                }],
                vec![],
                vec![],
            )
            .expect("add_element failed");
        let element_a = broker
            .add_element(
                "A",
                0,
                v0123_u8.clone(),
                vec![
                    fpb::LevelDependency {
                        dependency_type: DependencyType::Active,
                        dependent_level: 1,
                        requires_token: token_d_assertive
                            .duplicate_handle(zx::Rights::SAME_RIGHTS)
                            .expect("dup failed")
                            .into(),
                        requires_level: 1,
                    },
                    fpb::LevelDependency {
                        dependency_type: DependencyType::Passive,
                        dependent_level: 2,
                        requires_token: token_c_opportunistic
                            .duplicate_handle(zx::Rights::SAME_RIGHTS)
                            .expect("dup failed")
                            .into(),
                        requires_level: 1,
                    },
                    fpb::LevelDependency {
                        dependency_type: DependencyType::Active,
                        dependent_level: 3,
                        requires_token: token_b_assertive
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

        // Initial required level for all elements should be OFF.
        // Set all current levels to OFF.
        assert_eq!(broker.get_required_level(&element_a), Some(ZERO));
        assert_eq!(broker.get_required_level(&element_b), Some(ZERO));
        assert_eq!(broker.get_required_level(&element_c), Some(ZERO));
        assert_eq!(broker.get_required_level(&element_d), Some(ZERO));
        assert_eq!(broker.get_required_level(&element_e), Some(ZERO));
        assert_eq!(broker.get_required_level(&element_f), Some(ZERO));
        broker.update_current_level(&element_a, ZERO);
        broker.update_current_level(&element_b, ZERO);
        broker.update_current_level(&element_c, ZERO);
        broker.update_current_level(&element_d, ZERO);
        broker.update_current_level(&element_e, ZERO);
        broker.update_current_level(&element_f, ZERO);

        // Lease A[3].
        //
        // A has two opportunistic dependencies, on B[1] and D[1].
        // A, B, C, D, E and F's required levels should not change.
        //
        // A's lease is pending and contingent.
        let lease_a = broker.acquire_lease(&element_a, THREE).expect("acquire failed");
        let lease_a_id = lease_a.id.clone();
        assert_eq!(broker.get_required_level(&element_a), Some(ZERO));
        assert_eq!(broker.get_required_level(&element_b), Some(ZERO));
        assert_eq!(broker.get_required_level(&element_c), Some(ZERO));
        assert_eq!(broker.get_required_level(&element_d), Some(ZERO));
        assert_eq!(broker.get_required_level(&element_e), Some(ZERO));
        assert_eq!(broker.get_required_level(&element_f), Some(ZERO));
        assert_eq!(broker.get_lease_status(&lease_a.id), Some(LeaseStatus::Pending));
        assert_eq!(broker.is_lease_contingent(&lease_a.id), true);

        // Lease F[1].
        //
        // F has an assertive claim on C[1].
        // A has an assertive claim on B[2] that should now satisfy D[1]->B[1].
        // B's required level should now be 2.
        // C's required level should now be 1.
        // E's required level should now be 1.
        // A, D and F's required level should not change.
        //
        // A's lease is pending and not contingent.
        // F's lease is pending and not contingent.
        let lease_f = broker.acquire_lease(&element_f, ONE).expect("acquire failed");
        let lease_f_id = lease_f.id.clone();
        assert_eq!(broker.get_required_level(&element_a), Some(ZERO));
        assert_eq!(broker.get_required_level(&element_b), Some(TWO));
        assert_eq!(broker.get_required_level(&element_c), Some(ONE));
        assert_eq!(broker.get_required_level(&element_d), Some(ZERO));
        assert_eq!(broker.get_required_level(&element_e), Some(ONE));
        assert_eq!(broker.get_required_level(&element_f), Some(ZERO));
        assert_eq!(broker.get_lease_status(&lease_a.id), Some(LeaseStatus::Pending));
        assert_eq!(broker.get_lease_status(&lease_f.id), Some(LeaseStatus::Pending));
        assert_eq!(broker.is_lease_contingent(&lease_a.id), false);
        assert_eq!(broker.is_lease_contingent(&lease_f.id), false);

        // Update B, C and E's current level to 1.
        //
        // D's required level should now be 1.
        // A, B, C, E, and F's required level should not change.
        //
        // A's lease should be pending and not contingent.
        // F's lease should be satisfied and not contingent.
        broker.update_current_level(&element_b, ONE);
        broker.update_current_level(&element_c, ONE);
        broker.update_current_level(&element_e, ONE);
        assert_eq!(broker.get_required_level(&element_a), Some(ZERO));
        assert_eq!(broker.get_required_level(&element_b), Some(TWO));
        assert_eq!(broker.get_required_level(&element_c), Some(ONE));
        assert_eq!(broker.get_required_level(&element_d), Some(ONE));
        assert_eq!(broker.get_required_level(&element_e), Some(ONE));
        assert_eq!(broker.get_required_level(&element_f), Some(ONE));
        assert_eq!(broker.get_lease_status(&lease_a.id), Some(LeaseStatus::Pending));
        assert_eq!(broker.get_lease_status(&lease_f.id), Some(LeaseStatus::Satisfied));
        assert_eq!(broker.is_lease_contingent(&lease_a.id), false);
        assert_eq!(broker.is_lease_contingent(&lease_f.id), false);

        // Update B's current level to 2 and D's current level to 1.
        //
        // A's required level should now be 3.
        // D's required level should now be 1.
        // B, C, E, and F's required level should not change.
        //
        // A's lease should be satisfied and not contingent.
        // F's lease should be satisfied and not contingent.
        broker.update_current_level(&element_b, TWO);
        broker.update_current_level(&element_d, ONE);
        assert_eq!(broker.get_required_level(&element_a), Some(THREE));
        assert_eq!(broker.get_required_level(&element_b), Some(TWO));
        assert_eq!(broker.get_required_level(&element_c), Some(ONE));
        assert_eq!(broker.get_required_level(&element_d), Some(ONE));
        assert_eq!(broker.get_required_level(&element_e), Some(ONE));
        assert_eq!(broker.get_required_level(&element_f), Some(ONE));
        assert_eq!(broker.get_lease_status(&lease_a.id), Some(LeaseStatus::Satisfied));
        assert_eq!(broker.get_lease_status(&lease_f.id), Some(LeaseStatus::Satisfied));
        assert_eq!(broker.is_lease_contingent(&lease_a.id), false);
        assert_eq!(broker.is_lease_contingent(&lease_f.id), false);

        // Update A's current level to 3 and F's current level to 1.
        //
        // No required levels should change.
        //
        // Both leases should be satisfied and not contingent.
        broker.update_current_level(&element_a, THREE);
        broker.update_current_level(&element_f, ONE);
        assert_eq!(broker.get_required_level(&element_a), Some(THREE));
        assert_eq!(broker.get_required_level(&element_b), Some(TWO));
        assert_eq!(broker.get_required_level(&element_c), Some(ONE));
        assert_eq!(broker.get_required_level(&element_d), Some(ONE));
        assert_eq!(broker.get_required_level(&element_e), Some(ONE));
        assert_eq!(broker.get_required_level(&element_f), Some(ONE));
        assert_eq!(broker.get_lease_status(&lease_a.id), Some(LeaseStatus::Satisfied));
        assert_eq!(broker.get_lease_status(&lease_f.id), Some(LeaseStatus::Satisfied));
        assert_eq!(broker.is_lease_contingent(&lease_a.id), false);
        assert_eq!(broker.is_lease_contingent(&lease_f.id), false);

        // Drop Lease F.
        //
        // B's required level should stay at 2.
        // C's required level should stay at 1.
        // D's required level should stay at 1.
        // E's required level should stay at 1.
        //
        // Lease A should be pending and contingent.
        broker.drop_lease(&lease_f.id).expect("drop_lease failed");
        assert_eq!(broker.get_required_level(&element_a), Some(ZERO));
        assert_eq!(broker.get_required_level(&element_b), Some(TWO));
        assert_eq!(broker.get_required_level(&element_c), Some(ONE));
        assert_eq!(broker.get_required_level(&element_d), Some(ONE));
        assert_eq!(broker.get_required_level(&element_e), Some(ONE));
        assert_eq!(broker.get_required_level(&element_f), Some(ZERO));
        assert_eq!(broker.get_lease_status(&lease_a.id), Some(LeaseStatus::Pending));
        assert_eq!(broker.is_lease_contingent(&lease_a.id), true);

        // Drop Lease A.
        //
        // B's required level should stay at 1 (still used by D[1]).
        // E's required level should stay at 1 (still used by D[1]).
        //
        // All required levels should drop to 0.
        broker.drop_lease(&lease_a.id).expect("drop_lease failed");
        assert_eq!(broker.get_required_level(&element_a), Some(ZERO));
        assert_eq!(broker.get_required_level(&element_b), Some(ONE));
        assert_eq!(broker.get_required_level(&element_c), Some(ZERO));
        assert_eq!(broker.get_required_level(&element_d), Some(ZERO));
        assert_eq!(broker.get_required_level(&element_e), Some(ONE));
        assert_eq!(broker.get_required_level(&element_f), Some(ZERO));

        // Update D's current level to 0.
        //
        // B's required level should drop to 0.
        broker.update_current_level(&element_d, ZERO);
        assert_eq!(broker.get_required_level(&element_a), Some(ZERO));
        assert_eq!(broker.get_required_level(&element_b), Some(ZERO));
        assert_eq!(broker.get_required_level(&element_c), Some(ZERO));
        assert_eq!(broker.get_required_level(&element_d), Some(ZERO));
        assert_eq!(broker.get_required_level(&element_e), Some(ZERO));
        assert_eq!(broker.get_required_level(&element_f), Some(ZERO));

        // All leases should be cleaned up.
        assert_lease_cleaned_up(&broker.catalog, &lease_a_id);
        assert_lease_cleaned_up(&broker.catalog, &lease_f_id);
    }

    #[fuchsia::test]
    async fn test_lease_implicit_dependency_self_satisfying() {
        // Tests that cumulative implicit dependencies (both opportunistic and assertive) are properly
        // requested when a lease is acquired and when the requested element can satisfy it's
        // opportunistic dependency with it's own assertive dependency.
        //
        // A[1] has a opportunistic dependency on B[1].
        // A[2] has an assertive dependency on B[2].
        // A[3] has an assertive dependency on C[2].
        // B[2] has a opportunistic dependency on C[1].
        //
        // A[3] has an implicit, opportunistic dependency on B[1].
        // A[3] has an implicit, assertive dependency on B[2].
        // A[2] has an implicit, transitive, opportunistic dependency on C[1].
        //
        // As A[3]->C[2] satisfies the opportunistic dependency B[2]->C[1], and A[2]->B[2] satisfies the
        // opportunistic dependency A[1]->B[1], this lease would be self-satisfying.
        //
        //  A     B     C
        //  1 --> 1
        //  2 ==> 2 --> 1
        //  3 ========> 2
        let inspect = fuchsia_inspect::component::inspector();
        let inspect_node = inspect.root().create_child("test");
        let mut broker = Broker::new(inspect_node);

        let v0123_u8: Vec<u8> = vec![0, 1, 2, 3];

        let token_b_assertive = DependencyToken::create();
        let token_b_opportunistic = DependencyToken::create();
        let token_c_assertive = DependencyToken::create();
        let token_c_opportunistic = DependencyToken::create();
        let element_c = broker
            .add_element(
                "C",
                0,
                v0123_u8.clone(),
                vec![],
                vec![token_c_assertive
                    .duplicate_handle(zx::Rights::SAME_RIGHTS)
                    .expect("dup failed")
                    .into()],
                vec![token_c_opportunistic
                    .duplicate_handle(zx::Rights::SAME_RIGHTS)
                    .expect("dup failed")
                    .into()],
            )
            .expect("add_element failed");
        let element_b = broker
            .add_element(
                "B",
                0,
                v0123_u8.clone(),
                vec![fpb::LevelDependency {
                    dependency_type: DependencyType::Passive,
                    dependent_level: 2,
                    requires_token: token_c_opportunistic
                        .duplicate_handle(zx::Rights::SAME_RIGHTS)
                        .expect("dup failed")
                        .into(),
                    requires_level: 1,
                }],
                vec![token_b_assertive
                    .duplicate_handle(zx::Rights::SAME_RIGHTS)
                    .expect("dup failed")
                    .into()],
                vec![token_b_opportunistic
                    .duplicate_handle(zx::Rights::SAME_RIGHTS)
                    .expect("dup failed")
                    .into()],
            )
            .expect("add_element failed");
        let element_a = broker
            .add_element(
                "A",
                0,
                v0123_u8.clone(),
                vec![
                    fpb::LevelDependency {
                        dependency_type: DependencyType::Passive,
                        dependent_level: 1,
                        requires_token: token_b_opportunistic
                            .duplicate_handle(zx::Rights::SAME_RIGHTS)
                            .expect("dup failed")
                            .into(),
                        requires_level: 1,
                    },
                    fpb::LevelDependency {
                        dependency_type: DependencyType::Active,
                        dependent_level: 2,
                        requires_token: token_b_assertive
                            .duplicate_handle(zx::Rights::SAME_RIGHTS)
                            .expect("dup failed")
                            .into(),
                        requires_level: 2,
                    },
                    fpb::LevelDependency {
                        dependency_type: DependencyType::Active,
                        dependent_level: 3,
                        requires_token: token_c_assertive
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

        // Initial required level for all elements should be OFF.
        // Set all current levels to OFF.
        assert_eq!(broker.get_required_level(&element_a), Some(ZERO));
        assert_eq!(broker.get_required_level(&element_b), Some(ZERO));
        assert_eq!(broker.get_required_level(&element_c), Some(ZERO));
        broker.update_current_level(&element_a, ZERO);
        broker.update_current_level(&element_b, ZERO);
        broker.update_current_level(&element_c, ZERO);

        // Lease A[3].
        //
        // C's required level should now be 2.
        // A and B's required level should not change.
        //
        // A's lease should be pending and not contingent.
        let lease_a = broker.acquire_lease(&element_a, THREE).expect("acquire failed");
        let lease_a_id = lease_a.id.clone();
        assert_eq!(broker.get_required_level(&element_a), Some(ZERO));
        assert_eq!(broker.get_required_level(&element_b), Some(ZERO));
        assert_eq!(broker.get_required_level(&element_c), Some(TWO));
        assert_eq!(broker.get_lease_status(&lease_a.id), Some(LeaseStatus::Pending));
        assert_eq!(broker.is_lease_contingent(&lease_a.id), false);

        // Update C's current level to 2.
        //
        // B's required level should now be 2.
        // A and C's required level should not change.
        //
        // A's lease should be pending and not contingent.
        broker.update_current_level(&element_c, TWO);
        assert_eq!(broker.get_required_level(&element_a), Some(ZERO));
        assert_eq!(broker.get_required_level(&element_b), Some(TWO));
        assert_eq!(broker.get_required_level(&element_c), Some(TWO));
        assert_eq!(broker.get_lease_status(&lease_a.id), Some(LeaseStatus::Pending));
        assert_eq!(broker.is_lease_contingent(&lease_a.id), false);

        // Update B's current level to 2.
        //
        // A's required level should now be 3.
        // B and C's required level should not change.
        //
        // A's lease should be satisfied and not contingent.
        broker.update_current_level(&element_b, TWO);
        assert_eq!(broker.get_required_level(&element_a), Some(THREE));
        assert_eq!(broker.get_required_level(&element_b), Some(TWO));
        assert_eq!(broker.get_required_level(&element_c), Some(TWO));
        assert_eq!(broker.get_lease_status(&lease_a.id), Some(LeaseStatus::Satisfied));
        assert_eq!(broker.is_lease_contingent(&lease_a.id), false);

        // Drop Lease A[3].
        //
        // C's required level should stay at 1 (required by B[2]).
        broker.drop_lease(&lease_a.id).expect("drop_lease failed");
        assert_eq!(broker.get_required_level(&element_a), Some(ZERO));
        assert_eq!(broker.get_required_level(&element_b), Some(ZERO));
        assert_eq!(broker.get_required_level(&element_c), Some(ONE));

        // Update B's current level to 0.
        //
        // A and B's required level should not change.
        // C's required level should now be 0.
        broker.update_current_level(&element_b, ZERO);
        assert_eq!(broker.get_required_level(&element_a), Some(ZERO));
        assert_eq!(broker.get_required_level(&element_b), Some(ZERO));
        assert_eq!(broker.get_required_level(&element_c), Some(ZERO));

        // All leases should be cleaned up.
        assert_lease_cleaned_up(&broker.catalog, &lease_a_id);
    }

    #[fuchsia::test]
    fn test_removing_element_permanently_prevents_lease_satisfaction() {
        // Tests that if element A depends on element B, and element B is removed, that new leases
        // on element A will never be satisfied.
        //
        // B has an assertive dependency on A.
        // C has a opportunistic dependency on A.
        //  A     B     C
        // ON <= ON
        //    <------- ON
        let inspect = fuchsia_inspect::component::inspector();
        let inspect_node = inspect.root().create_child("test");
        let mut broker = Broker::new(inspect_node);
        let token_a_assertive = DependencyToken::create();
        let token_a_opportunistic = DependencyToken::create();
        let element_a = broker
            .add_element(
                "A",
                OFF.level,
                BINARY_POWER_LEVELS.to_vec(),
                vec![],
                vec![token_a_assertive
                    .duplicate_handle(zx::Rights::SAME_RIGHTS)
                    .expect("dup failed")
                    .into()],
                vec![token_a_opportunistic
                    .duplicate_handle(zx::Rights::SAME_RIGHTS)
                    .expect("dup failed")
                    .into()],
            )
            .expect("add_element failed");
        let element_b = broker
            .add_element(
                "B",
                OFF.level,
                BINARY_POWER_LEVELS.to_vec(),
                vec![fpb::LevelDependency {
                    dependency_type: DependencyType::Active,
                    dependent_level: ON.level,
                    requires_token: token_a_assertive
                        .duplicate_handle(zx::Rights::SAME_RIGHTS)
                        .expect("dup failed")
                        .into(),
                    requires_level: ON.level,
                }],
                vec![],
                vec![],
            )
            .expect("add_element failed");
        let element_c = broker
            .add_element(
                "C",
                OFF.level,
                BINARY_POWER_LEVELS.to_vec(),
                vec![fpb::LevelDependency {
                    dependency_type: DependencyType::Passive,
                    dependent_level: ON.level,
                    requires_token: token_a_opportunistic
                        .duplicate_handle(zx::Rights::SAME_RIGHTS)
                        .expect("dup failed")
                        .into(),
                    requires_level: ON.level,
                }],
                vec![],
                vec![],
            )
            .expect("add_element failed");

        // Initial required level for all elements should be OFF.
        // Set all current levels to OFF.
        assert_eq!(broker.get_required_level(&element_a), Some(OFF));
        assert_eq!(broker.get_required_level(&element_b), Some(OFF));
        assert_eq!(broker.get_required_level(&element_c), Some(OFF));

        // Remove A.
        // B & C's required level should remain OFF.
        broker.remove_element(&element_a);
        assert_eq!(broker.get_required_level(&element_b), Some(OFF));
        assert_eq!(broker.get_required_level(&element_c), Some(OFF));

        // Lease B & C.
        // B & C's required level should remain OFF.
        // Both leases should not be satisfied, but B and C should now be contingent, as the should
        // have a new opportunistic dependency on the topology unsatisfiable element.
        let lease_b = broker.acquire_lease(&element_b, ON).expect("acquire failed");
        let lease_c = broker.acquire_lease(&element_c, ON).expect("acquire failed");
        broker.update_current_level(&element_a, ON);
        assert_eq!(broker.get_required_level(&element_b), Some(OFF));
        assert_eq!(broker.get_required_level(&element_c), Some(OFF));
        assert_eq!(broker.get_lease_status(&lease_b.id), Some(LeaseStatus::Pending));
        assert_eq!(broker.get_lease_status(&lease_c.id), Some(LeaseStatus::Pending));
        assert_eq!(broker.is_lease_contingent(&lease_b.id), true);
        assert_eq!(broker.is_lease_contingent(&lease_c.id), true);

        broker.drop_lease(&lease_b.id).expect("drop_lease failed");
        broker.drop_lease(&lease_c.id).expect("drop_lease failed");

        // Leases should be cleaned up.
        assert_lease_cleaned_up(&broker.catalog, &lease_b.id);
        assert_lease_cleaned_up(&broker.catalog, &lease_c.id);
    }
}
