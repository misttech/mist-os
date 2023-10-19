// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{anyhow, Error};
use fidl_fuchsia_power_broker::{BinaryPowerLevel, PowerLevel, PowerLevelError};
use futures::channel::mpsc::{unbounded, UnboundedReceiver, UnboundedSender};
use std::collections::HashMap;
use uuid::Uuid;

use crate::topology::*;

pub struct Broker {
    lease_catalog: Catalog,
    // The current level for each element, as reported to the broker.
    current: Levels,
    // The level for each element required by the topology.
    required: Levels,
}

impl Broker {
    pub fn new() -> Self {
        Broker { lease_catalog: Catalog::new(), current: Levels::new(), required: Levels::new() }
    }

    pub fn get_current_level(&self, id: &ElementID) -> Result<PowerLevel, PowerLevelError> {
        if let Some(level) = self.current.get(id) {
            Ok(level)
        } else {
            Err(PowerLevelError::NotFound)
        }
    }

    pub fn update_current_level(&mut self, id: &ElementID, level: &PowerLevel) {
        self.current.update(id, level)
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
        let (original_lease, claims) = self.lease_catalog.acquire(element, level)?;
        self.update_required_levels(&claims);
        Ok(original_lease)
    }

    pub fn drop_lease(&mut self, lease_id: &LeaseID) -> Result<(), Error> {
        let (_, claims) = self.lease_catalog.drop(lease_id)?;
        self.update_required_levels(&claims);
        Ok(())
    }

    fn update_required_levels(&mut self, claims: &Vec<Claim>) {
        for claim in claims {
            let new_min_level =
                self.lease_catalog.calc_min_level(&claim.dependency.requires.element);
            self.required.update(&claim.dependency.requires.element, &new_min_level);
        }
    }

    pub fn add_element(&mut self, name: &str, _dependencies: Vec<Dependency>) -> ElementID {
        self.lease_catalog.topology.add_element(name)
    }

    pub fn remove_element(&mut self, element: &ElementID) -> Result<(), RemoveElementError> {
        self.lease_catalog.topology.remove_element(element)
    }

    pub fn add_dependency(&mut self, dependency: &Dependency) -> Result<(), AddDependencyError> {
        self.lease_catalog.topology.add_direct_dep(dependency)
    }

    pub fn remove_dependency(
        &mut self,
        dependency: &Dependency,
    ) -> Result<(), RemoveDependencyError> {
        self.lease_catalog.topology.remove_direct_dep(dependency)
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

struct Catalog {
    topology: Topology,
    leases: HashMap<LeaseID, Lease>,
    claims: HashMap<ClaimID, Claim>,
    claims_by_element: HashMap<ElementID, Vec<ClaimID>>,
    claims_by_lease: HashMap<LeaseID, Vec<ClaimID>>,
}

impl Catalog {
    fn new() -> Self {
        Catalog {
            topology: Topology::new(),
            leases: HashMap::new(),
            claims: HashMap::new(),
            claims_by_element: HashMap::new(),
            claims_by_lease: HashMap::new(),
        }
    }

    fn calc_min_level(&self, element: &ElementID) -> PowerLevel {
        let no_claims = Vec::new();
        let element_claims = self
            .claims_by_element
            .get(element)
            // Treat both missing key and empty vec as no claims.
            .unwrap_or(&no_claims)
            .iter()
            .filter_map(|id| self.claims.get(id));
        element_claims
            .map(|x| x.dependency.requires.level)
            .max()
            // No claims, default to OFF
            // TODO(b/299637587): support other power level types.
            .unwrap_or(PowerLevel::Binary(BinaryPowerLevel::Off))
    }

    fn add_claim(&mut self, claim: Claim) {
        self.claims_by_element
            .entry(claim.dependency.requires.element.clone())
            .or_insert(Vec::new())
            .push(claim.id.clone());
        self.claims_by_lease
            .entry(claim.lease_id.clone())
            .or_insert(Vec::new())
            .push(claim.id.clone());
        self.claims.insert(claim.id.clone(), claim);
    }

    fn remove_claim(&mut self, id: &ClaimID) -> Option<Claim> {
        let Some(claim) = self.claims.remove(id) else {
            return None;
        };
        if let Some(claim_ids) = self.claims_by_element.get_mut(&claim.dependency.requires.element)
        {
            claim_ids.retain(|x| x != id);
        }
        if let Some(claim_ids) = self.claims_by_lease.get_mut(&claim.lease_id) {
            claim_ids.retain(|x| x != id);
        }
        Some(claim)
    }

    /// Returns original lease, and a Vec of all claims created
    /// as a result of this call.
    fn acquire(
        &mut self,
        element: &ElementID,
        level: &PowerLevel,
    ) -> Result<(Lease, Vec<Claim>), Error> {
        // TODO: Add lease validation and control.
        let lease = Lease::new(&element, level);
        self.leases.insert(lease.id.clone(), lease.clone());
        // Create claims for all of the transitive dependencies.
        // TODO(b/302381778): Do this in the proper order.
        let mut claims = Vec::new();
        let element_level = ElementLevel { element: element.clone(), level: level.clone() };
        for dependency in self.topology.get_all_deps(&element_level) {
            // TODO: Make sure this is permitted by Limiters (once we have them).
            let dep_lease = Claim::new(dependency, &lease.id);
            claims.push(dep_lease);
        }
        for claim in claims.iter() {
            tracing::debug!("adding claim: {:?}", &claim);
            self.add_claim(claim.clone());
        }
        Ok((lease, claims))
    }

    /// Returns original lease, and a Vec of all claims dropped
    /// as a result of this call.
    fn drop(&mut self, lease_id: &LeaseID) -> Result<(Lease, Vec<Claim>), Error> {
        let lease = self.leases.remove(lease_id).ok_or(anyhow!("{lease_id} not found"))?;
        let claim_ids = self
            .claims_by_lease
            .get(&lease.id)
            .ok_or(anyhow!("{} not found by originator id", lease_id))?
            .clone();
        tracing::debug!("claim_ids: {:?}", &claim_ids);
        // Drop all claims created for the transitive dependencies.
        // TODO(b/302381778): Do this in the proper order.
        let mut claims = Vec::new();
        for id in claim_ids.into_iter() {
            let res = self.remove_claim(&id);
            if let Some(removed) = res {
                tracing::debug!("removing claim: {:?}", &removed);
                claims.push(removed)
            } else {
                tracing::error!("remove_claim not found: {}", id);
            }
        }
        Ok((lease, claims))
    }
}

// Holds PowerLevels for each element and publishes updates to subscribers.
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

#[cfg(test)]
mod tests {
    use super::*;
    use fidl_fuchsia_power_broker::{BinaryPowerLevel, PowerLevel};

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
    fn test_catalog_acquire_drop_direct() {
        // Create a topology of a child element with two direct dependencies.
        // P1 <- C -> P2
        let mut catalog = Catalog::new();
        let child: ElementID = catalog.topology.add_element("C");
        let parent1: ElementID = catalog.topology.add_element("P1");
        let parent2: ElementID = catalog.topology.add_element("P2");
        catalog
            .topology
            .add_direct_dep(&Dependency {
                level: ElementLevel {
                    element: child.clone(),
                    level: PowerLevel::Binary(BinaryPowerLevel::On),
                },
                requires: ElementLevel {
                    element: parent1.clone(),
                    level: PowerLevel::Binary(BinaryPowerLevel::On),
                },
            })
            .expect("add_direct_dep failed");
        catalog
            .topology
            .add_direct_dep(&Dependency {
                level: ElementLevel {
                    element: child.clone(),
                    level: PowerLevel::Binary(BinaryPowerLevel::On),
                },
                requires: ElementLevel {
                    element: parent2.clone(),
                    level: PowerLevel::Binary(BinaryPowerLevel::On),
                },
            })
            .expect("add_direct_dep failed");
        assert_eq!(
            catalog.calc_min_level(&parent1.clone()),
            PowerLevel::Binary(BinaryPowerLevel::Off),
            "Parent 1 should start with min level OFF"
        );
        assert_eq!(
            catalog.calc_min_level(&parent2.clone()),
            PowerLevel::Binary(BinaryPowerLevel::Off),
            "Parent 2 should start with min level OFF"
        );

        // Acquire the lease, which should result in two claims, one
        // for each dependency.
        let (lease, claims) = catalog
            .acquire(&child, &PowerLevel::Binary(BinaryPowerLevel::On))
            .expect("acquire failed");
        assert_eq!(claims.len(), 2);
        assert_ne!(lease.id, "");
        assert_eq!(lease.element, child.clone());
        assert_eq!(lease.level, PowerLevel::Binary(BinaryPowerLevel::On));
        let claims_by_required_element: HashMap<ElementID, Claim> =
            claims.into_iter().map(|c| (c.dependency.requires.element.clone(), c)).collect();
        for parent in [&parent1, &parent2] {
            let claim = claims_by_required_element
                .get(&parent)
                .unwrap_or_else(|| panic!("missing claim for {:?}", parent));
            assert_ne!(claim.id, "");
            assert_eq!(claim.lease_id, lease.id);
            assert_eq!(claim.dependency.level.element, child.clone());
            assert_eq!(claim.dependency.level.level, PowerLevel::Binary(BinaryPowerLevel::On));
            assert_eq!(claim.dependency.requires.element, parent.clone());
            assert_eq!(claim.dependency.requires.level, PowerLevel::Binary(BinaryPowerLevel::On));
        }
        assert_eq!(
            catalog.calc_min_level(&parent1.clone()),
            PowerLevel::Binary(BinaryPowerLevel::On),
            "Parent 1 should now have min level ON from direct claim"
        );
        assert_eq!(
            catalog.calc_min_level(&parent2.clone()),
            PowerLevel::Binary(BinaryPowerLevel::On),
            "Parent 2 should now have min level ON from direct claim"
        );

        // Now drop the lease and verify both claims are also dropped.
        let (dropped, dropped_claims) = catalog.drop(&lease.id).expect("drop failed");
        assert_eq!(dropped_claims.len(), 2);
        assert_eq!(dropped.id, lease.id);
        assert_eq!(dropped.element, lease.element);
        assert_eq!(dropped.level, lease.level);
        let dropped_claims_by_required_element: HashMap<ElementID, Claim> = dropped_claims
            .into_iter()
            .map(|c| (c.dependency.requires.element.clone(), c))
            .collect();
        for parent in [&parent1, &parent2] {
            let claim = dropped_claims_by_required_element
                .get(&parent)
                .unwrap_or_else(|| panic!("missing claim for {:?}", parent));
            assert_eq!(claim, claims_by_required_element.get(&parent).unwrap());
        }
        assert_eq!(
            catalog.calc_min_level(&parent1.clone()),
            PowerLevel::Binary(BinaryPowerLevel::Off),
            "Parent 1 should now have min level OFF from dropped claim"
        );
        assert_eq!(
            catalog.calc_min_level(&parent2.clone()),
            PowerLevel::Binary(BinaryPowerLevel::Off),
            "Parent 2 should now have min level OFF from dropped claim"
        );

        // Try dropping the lease one more time, which should result in an error.
        let extra_drop = catalog.drop(&lease.id);
        assert!(extra_drop.is_err());
    }

    #[fuchsia::test]
    fn test_catalog_acquire_drop_transitive() {
        // Create a topology of a child element with two chained transitive
        // dependencies.
        // C -> P -> GP
        let mut catalog = Catalog::new();
        let child: ElementID = catalog.topology.add_element("C");
        let parent: ElementID = catalog.topology.add_element("P");
        let grandparent: ElementID = catalog.topology.add_element("GP");
        catalog
            .topology
            .add_direct_dep(&Dependency {
                level: ElementLevel {
                    element: child.clone(),
                    level: PowerLevel::Binary(BinaryPowerLevel::On),
                },
                requires: ElementLevel {
                    element: parent.clone(),
                    level: PowerLevel::Binary(BinaryPowerLevel::On),
                },
            })
            .expect("add_direct_dep failed");
        catalog
            .topology
            .add_direct_dep(&Dependency {
                level: ElementLevel {
                    element: parent.clone(),
                    level: PowerLevel::Binary(BinaryPowerLevel::On),
                },
                requires: ElementLevel {
                    element: grandparent.clone(),
                    level: PowerLevel::Binary(BinaryPowerLevel::On),
                },
            })
            .expect("add_direct_dep failed");
        assert_eq!(
            catalog.calc_min_level(&parent.clone()),
            PowerLevel::Binary(BinaryPowerLevel::Off),
            "Parent should start with min level OFF"
        );
        assert_eq!(
            catalog.calc_min_level(&grandparent.clone()),
            PowerLevel::Binary(BinaryPowerLevel::Off),
            "Grandparent should start with min level OFF"
        );

        // Acquire the lease, which should result in two claims, one
        // for the direct parent dependency, and one for the transitive
        // grandparent dependency.
        let (lease, claims) = catalog
            .acquire(&child.clone(), &PowerLevel::Binary(BinaryPowerLevel::On))
            .expect("acquire failed");
        assert_eq!(claims.len(), 2);
        let claims_by_required_element: HashMap<ElementID, Claim> =
            claims.into_iter().map(|c| (c.dependency.requires.element.clone(), c)).collect();
        let parent_claim = claims_by_required_element
            .get(&parent)
            .unwrap_or_else(|| panic!("missing claim for {:?}", parent));
        assert_ne!(parent_claim.id, "");
        assert_eq!(parent_claim.lease_id, lease.id);
        assert_eq!(parent_claim.dependency.level.element, child.clone());
        assert_eq!(parent_claim.dependency.level.level, PowerLevel::Binary(BinaryPowerLevel::On));
        assert_eq!(parent_claim.dependency.requires.element, parent.clone());
        assert_eq!(
            parent_claim.dependency.requires.level,
            PowerLevel::Binary(BinaryPowerLevel::On)
        );
        let grandparent_claim = claims_by_required_element
            .get(&grandparent)
            .unwrap_or_else(|| panic!("missing claim for {:?}", grandparent));
        assert_ne!(grandparent_claim.id, "");
        assert_eq!(grandparent_claim.lease_id, lease.id);
        assert_eq!(grandparent_claim.dependency.level.element, parent.clone());
        assert_eq!(
            grandparent_claim.dependency.level.level,
            PowerLevel::Binary(BinaryPowerLevel::On)
        );
        assert_eq!(grandparent_claim.dependency.requires.element, grandparent.clone());
        assert_eq!(
            grandparent_claim.dependency.requires.level,
            PowerLevel::Binary(BinaryPowerLevel::On)
        );
        assert_eq!(
            catalog.calc_min_level(&parent.clone()),
            PowerLevel::Binary(BinaryPowerLevel::On),
            "Parent should now have min level ON from direct claim"
        );
        assert_eq!(
            catalog.calc_min_level(&grandparent.clone()),
            PowerLevel::Binary(BinaryPowerLevel::On),
            "Grandparent should now have min level ON from transitive claim"
        );

        // Now drop the lease and verify both claims are also dropped.
        let (dropped, dropped_claims) = catalog.drop(&lease.id).expect("drop failed");
        assert_eq!(dropped.id, lease.id);
        assert_eq!(dropped_claims.len(), 2);
        let dropped_claims_by_required_element: HashMap<ElementID, Claim> = dropped_claims
            .into_iter()
            .map(|c| (c.dependency.requires.element.clone(), c))
            .collect();
        let dropped_parent_claim = dropped_claims_by_required_element
            .get(&parent)
            .unwrap_or_else(|| panic!("missing claim for {:?}", parent));
        assert_eq!(dropped_parent_claim, parent_claim);
        let dropped_grandparent_claim = dropped_claims_by_required_element
            .get(&grandparent)
            .unwrap_or_else(|| panic!("missing claim for {:?}", grandparent));
        assert_eq!(dropped_grandparent_claim, grandparent_claim);
        assert_eq!(
            catalog.calc_min_level(&parent.clone()),
            PowerLevel::Binary(BinaryPowerLevel::Off),
            "Parent should now have min level OFF after lease drop"
        );
        assert_eq!(
            catalog.calc_min_level(&grandparent.clone()),
            PowerLevel::Binary(BinaryPowerLevel::Off),
            "Grandparent should now have min level OFF after lease drop"
        );
    }

    #[fuchsia::test]
    fn test_catalog_acquire_drop_shared() {
        // Create a topology of two child elements with a shared
        // parent and grandparent
        // C1 \
        //     > P -> GP
        // C2 /
        let mut catalog = Catalog::new();
        let child1: ElementID = catalog.topology.add_element("C1");
        let child2: ElementID = catalog.topology.add_element("C2");
        let parent: ElementID = catalog.topology.add_element("P");
        let grandparent: ElementID = catalog.topology.add_element("GP");
        catalog
            .topology
            .add_direct_dep(&Dependency {
                level: ElementLevel {
                    element: child1.clone(),
                    level: PowerLevel::Binary(BinaryPowerLevel::On),
                },
                requires: ElementLevel {
                    element: parent.clone(),
                    level: PowerLevel::Binary(BinaryPowerLevel::On),
                },
            })
            .expect("add_direct_dep failed");
        catalog
            .topology
            .add_direct_dep(&Dependency {
                level: ElementLevel {
                    element: child2.clone(),
                    level: PowerLevel::Binary(BinaryPowerLevel::On),
                },
                requires: ElementLevel {
                    element: parent.clone(),
                    level: PowerLevel::Binary(BinaryPowerLevel::On),
                },
            })
            .expect("add_direct_dep failed");
        catalog
            .topology
            .add_direct_dep(&Dependency {
                level: ElementLevel {
                    element: parent.clone(),
                    level: PowerLevel::Binary(BinaryPowerLevel::On),
                },
                requires: ElementLevel {
                    element: grandparent.clone(),
                    level: PowerLevel::Binary(BinaryPowerLevel::On),
                },
            })
            .expect("add_direct_dep failed");
        assert_eq!(
            catalog.calc_min_level(&parent.clone()),
            PowerLevel::Binary(BinaryPowerLevel::Off),
            "Parent should start with min level OFF"
        );
        assert_eq!(
            catalog.calc_min_level(&grandparent.clone()),
            PowerLevel::Binary(BinaryPowerLevel::Off),
            "Grandparent should start with min level OFF"
        );

        // Acquire a lease for the first child.
        let (lease1, claims1) = catalog
            .acquire(&child1, &PowerLevel::Binary(BinaryPowerLevel::On))
            .expect("acquire failed");
        assert_eq!(claims1.len(), 2);
        assert_ne!(lease1.id, "");
        assert_eq!(lease1.element, child1.clone());
        assert_eq!(lease1.level, PowerLevel::Binary(BinaryPowerLevel::On));

        let claims1_by_required_element: HashMap<ElementID, &Claim> =
            claims1.iter().map(|c| (c.dependency.requires.element.clone(), c)).collect();
        let parent_claim1 = claims1_by_required_element
            .get(&parent)
            .unwrap_or_else(|| panic!("missing claim for {:?}", parent));
        assert_ne!(parent_claim1.id, "");
        assert_eq!(parent_claim1.lease_id, lease1.id);
        assert_eq!(parent_claim1.dependency.level.element, child1.clone());
        assert_eq!(parent_claim1.dependency.level.level, PowerLevel::Binary(BinaryPowerLevel::On));
        assert_eq!(parent_claim1.dependency.requires.element, parent.clone());
        assert_eq!(
            parent_claim1.dependency.requires.level,
            PowerLevel::Binary(BinaryPowerLevel::On)
        );
        let grandparent_claim1 = claims1_by_required_element
            .get(&parent)
            .unwrap_or_else(|| panic!("missing claim for {:?}", parent));
        assert_ne!(grandparent_claim1.id, "");
        assert_eq!(grandparent_claim1.lease_id, lease1.id);
        assert_eq!(grandparent_claim1.dependency.level.element, child1.clone());
        assert_eq!(
            grandparent_claim1.dependency.level.level,
            PowerLevel::Binary(BinaryPowerLevel::On)
        );
        assert_eq!(grandparent_claim1.dependency.requires.element, parent.clone());
        assert_eq!(
            grandparent_claim1.dependency.requires.level,
            PowerLevel::Binary(BinaryPowerLevel::On)
        );
        assert_eq!(
            catalog.calc_min_level(&parent.clone()),
            PowerLevel::Binary(BinaryPowerLevel::On),
            "Parent should now have min level ON"
        );
        assert_eq!(
            catalog.calc_min_level(&grandparent.clone()),
            PowerLevel::Binary(BinaryPowerLevel::On),
            "Grandparent should now have min level ON"
        );

        // Acquire a lease for the second child.
        let (lease2, claims2) = catalog
            .acquire(&child2, &PowerLevel::Binary(BinaryPowerLevel::On))
            .expect("acquire failed");
        assert_eq!(claims2.len(), 2);
        assert_ne!(lease2.id, "");
        assert_eq!(lease2.element, child2.clone());
        assert_eq!(lease2.level, PowerLevel::Binary(BinaryPowerLevel::On));
        let claims2_by_required_element: HashMap<ElementID, &Claim> =
            claims2.iter().map(|c| (c.dependency.requires.element.clone(), c)).collect();
        let parent_claim2 = claims2_by_required_element
            .get(&parent)
            .unwrap_or_else(|| panic!("missing claim for {:?}", parent));
        assert_ne!(parent_claim2.id, "");
        assert_eq!(parent_claim2.lease_id, lease2.id);
        assert_eq!(parent_claim2.dependency.level.element, child2.clone());
        assert_eq!(parent_claim2.dependency.level.level, PowerLevel::Binary(BinaryPowerLevel::On));
        assert_eq!(parent_claim2.dependency.requires.element, parent.clone());
        assert_eq!(
            parent_claim2.dependency.requires.level,
            PowerLevel::Binary(BinaryPowerLevel::On)
        );
        let grandparent_claim2 = claims2_by_required_element
            .get(&parent)
            .unwrap_or_else(|| panic!("missing claim for {:?}", parent));
        assert_ne!(grandparent_claim2.id, "");
        assert_eq!(grandparent_claim2.lease_id, lease2.id);
        assert_eq!(grandparent_claim2.dependency.level.element, child2.clone());
        assert_eq!(
            grandparent_claim2.dependency.level.level,
            PowerLevel::Binary(BinaryPowerLevel::On)
        );
        assert_eq!(grandparent_claim2.dependency.requires.element, parent.clone());
        assert_eq!(
            grandparent_claim2.dependency.requires.level,
            PowerLevel::Binary(BinaryPowerLevel::On)
        );
        assert_eq!(
            catalog.calc_min_level(&parent.clone()),
            PowerLevel::Binary(BinaryPowerLevel::On),
            "Parent should still have min level ON"
        );
        assert_eq!(
            catalog.calc_min_level(&grandparent.clone()),
            PowerLevel::Binary(BinaryPowerLevel::On),
            "Grandparent should still have min level ON"
        );

        // Now drop the first lease. Parent should still have min level ON.
        let (dropped1, dropped_claims1) = catalog.drop(&lease1.id).expect("drop failed");
        assert_eq!(dropped_claims1.len(), 2);
        assert_eq!(dropped1.id, lease1.id);
        assert_eq!(dropped1.element, lease1.element);
        assert_eq!(dropped1.level, lease1.level);
        assert_eq!(dropped_claims1, claims1);
        assert_eq!(
            catalog.calc_min_level(&parent.clone()),
            PowerLevel::Binary(BinaryPowerLevel::On),
            "Parent should still have min level ON from the second claim"
        );
        assert_eq!(
            catalog.calc_min_level(&grandparent.clone()),
            PowerLevel::Binary(BinaryPowerLevel::On),
            "Grandparent should still have min level ON from the second claim"
        );

        // Now drop the second lease. Parent should now have min level OFF.
        let (dropped2, dropped_claims2) = catalog.drop(&lease2.id).expect("drop failed");
        assert_eq!(dropped_claims2.len(), 2);
        assert_eq!(dropped2.id, lease2.id);
        assert_eq!(dropped2.element, lease2.element);
        assert_eq!(dropped2.level, lease2.level);
        assert_eq!(dropped_claims2, claims2);
        assert_eq!(
            catalog.calc_min_level(&parent.clone()),
            PowerLevel::Binary(BinaryPowerLevel::Off),
            "Parent should now have min level OFF"
        );
        assert_eq!(
            catalog.calc_min_level(&grandparent.clone()),
            PowerLevel::Binary(BinaryPowerLevel::Off),
            "Grandparent should now have min level OFF"
        );
    }
}
