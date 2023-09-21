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
    lease_catalog: LeaseCatalog,
    // The current level for each element, as reported to the broker.
    current: Levels,
    // The level for each element required by the topology.
    required: Levels,
}

impl Broker {
    pub fn new(topology: Topology) -> Self {
        Broker {
            lease_catalog: LeaseCatalog::new(topology),
            current: Levels::new(),
            required: Levels::new(),
        }
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

    pub fn get_required_level(&self, id: &ElementID) -> PowerLevel {
        // TODO(b/299637587): Support different Power Levels
        self.required.get(id).unwrap_or(PowerLevel::Binary(BinaryPowerLevel::Off))
    }

    // TODO(b/299485602): Use this in PowerControl::GetRequiredLevelUpdate
    #[allow(dead_code)]
    pub fn subscribe_required_level(
        &mut self,
        id: &ElementID,
    ) -> UnboundedReceiver<Option<PowerLevel>> {
        self.required.subscribe(id)
    }

    pub fn acquire_lease(&mut self, dependency: Dependency) -> Result<Lease, Error> {
        let (original_lease, transitive_leases) = self.lease_catalog.acquire(dependency)?;
        self.update_required_levels(&vec![original_lease.clone()]);
        self.update_required_levels(&transitive_leases);
        Ok(original_lease)
    }

    pub fn drop_lease(&mut self, lease_id: &LeaseID) -> Result<(), Error> {
        let (original_lease, transitive_leases) = self.lease_catalog.drop(lease_id)?;
        self.update_required_levels(&vec![original_lease]);
        self.update_required_levels(&transitive_leases);
        Ok(())
    }

    fn update_required_levels(&mut self, leases: &Vec<Lease>) {
        for lease in leases {
            let new_min_lvl = self.lease_catalog.calc_min_level(&lease.dependency.requires.id);
            self.required.update(&lease.dependency.requires.id, &new_min_lvl);
        }
    }
}

type LeaseID = String;

#[derive(Clone, Debug, Eq, Hash, Ord, PartialOrd, PartialEq)]
pub struct Lease {
    pub id: LeaseID,
    pub dependency: Dependency,
    pub originator_lease: LeaseID,
}

impl Lease {
    fn new(dependency: Dependency, originator_lease: Option<&LeaseID>) -> Self {
        let id = LeaseID::from(Uuid::new_v4().as_simple().to_string());
        Lease {
            id: id.clone(),
            dependency,
            originator_lease: originator_lease.cloned().unwrap_or(id.clone()),
        }
    }
}

struct LeaseCatalog {
    topology: Topology,
    leases: HashMap<LeaseID, Lease>,
    ids_by_element: HashMap<ElementID, Vec<LeaseID>>,
    ids_by_originator_id: HashMap<LeaseID, Vec<LeaseID>>,
}

impl LeaseCatalog {
    fn new(topology: Topology) -> Self {
        LeaseCatalog {
            topology,
            leases: HashMap::new(),
            ids_by_element: HashMap::new(),
            ids_by_originator_id: HashMap::new(),
        }
    }

    fn calc_min_level(&self, element_id: &ElementID) -> PowerLevel {
        // TODO(b/299637587): support other power level types.
        self.ids_by_element
            .get(element_id)
            .unwrap_or(&Vec::new())
            .iter()
            .filter_map(|id| self.leases.get(id))
            .map(|x| x.dependency.requires.lvl)
            .max()
            .unwrap_or(PowerLevel::Binary(BinaryPowerLevel::Off))
    }

    fn add_internal(&mut self, lease: Lease) {
        self.ids_by_element
            .entry(lease.dependency.requires.id.clone())
            .or_insert(Vec::new())
            .push(lease.id.clone());
        self.ids_by_originator_id
            .entry(lease.originator_lease.clone())
            .or_insert(Vec::new())
            .push(lease.id.clone());
        self.leases.insert(lease.id.clone(), lease);
    }

    /// Returns original lease, and a Vec of all transitive leases created
    /// as a result of this call.
    fn acquire(&mut self, dependency: Dependency) -> Result<(Lease, Vec<Lease>), Error> {
        // TODO: Add lease validation and control.
        let lease = Lease::new(dependency, None);
        // Create internal leases for all of the transitive dependencies.
        let mut transitive_leases = Vec::new();
        for dependency in self
            .topology
            .get_all_deps(&lease.dependency.requires.id, &lease.dependency.requires.lvl)
        {
            // TODO: Make sure this is permitted by Limiters (once we have them).
            let dep_lease = Lease::new(dependency, Some(&lease.id));
            transitive_leases.push(dep_lease);
        }
        self.add_internal(lease.clone());
        for lease in transitive_leases.iter() {
            tracing::info!("adding lease: {:?}", &lease);
            self.add_internal(lease.clone());
        }
        Ok((lease, transitive_leases))
    }

    /// Returns original lease, and a Vec of all transitive leases dropped
    /// as a result of this call.
    fn drop(&mut self, lease_id: &LeaseID) -> Result<(Lease, Vec<Lease>), Error> {
        let lease = self.leases.remove(lease_id).ok_or(anyhow!("{lease_id} not found"))?;
        let transitive_lease_ids = self
            .ids_by_originator_id
            .get(&lease.id)
            .ok_or(anyhow!("{} not found by originator id", lease_id))?;
        tracing::info!("transitive_lease_ids: {:?}", &transitive_lease_ids);
        // Drop all internal leases created for the transitive dependencies.
        let mut transitive_leases = Vec::new();
        self.ids_by_element.entry(lease.dependency.requires.id.clone().into()).or_default();
        for id in transitive_lease_ids {
            // Remove from ids_by_element.
            if let Some(lease_ids) = self.ids_by_element.get_mut(&id.clone().into()) {
                lease_ids.retain(|x| x != id);
            }
            if id == &lease.id {
                // Already removed from leases above.
                continue;
            }
            let removed = self.leases.remove(id).ok_or(anyhow!("lease {id} not found"))?;
            tracing::info!("removing lease: {:?}", &removed);
            transitive_leases.push(removed);
        }
        self.ids_by_originator_id.remove(&lease.id);
        Ok((lease, transitive_leases))
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
        tracing::info!("update({:?}): {:?}", &id, &level);
        self.level_map.insert(id.clone(), level.clone());
        if let Some(senders) = self.channels.get(id) {
            for sender in senders.into_iter() {
                // TODO: Prune dead channels.
                tracing::info!("send: {:?}", level.clone());
                if let Err(err) = sender.unbounded_send(Some(level.clone())) {
                    tracing::error!("send failed: {:?}", err)
                }
            }
        }
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

        tracing::info!("subscribing A and B");
        let mut receiver_a = levels.subscribe(&"A".into());
        let mut receiver_b = levels.subscribe(&"B".into());

        levels.update(&"A".into(), &PowerLevel::Binary(BinaryPowerLevel::On));
        assert_eq!(levels.get(&"A".into()), Some(PowerLevel::Binary(BinaryPowerLevel::On)));
        assert_eq!(levels.get(&"B".into()), None);

        levels.update(&"A".into(), &PowerLevel::Binary(BinaryPowerLevel::Off));
        levels.update(&"B".into(), &PowerLevel::Binary(BinaryPowerLevel::On));
        assert_eq!(levels.get(&"A".into()), Some(PowerLevel::Binary(BinaryPowerLevel::Off)));
        assert_eq!(levels.get(&"B".into()), Some(PowerLevel::Binary(BinaryPowerLevel::On)));
        tracing::info!("updates done");

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
    fn test_lessor_acquire_drop() {
        let mut lessor = LeaseCatalog::new(Topology::new());

        let (lease, transitive_leases) = lessor
            .acquire(Dependency {
                level: ElementLevel {
                    id: "A".into(),
                    lvl: PowerLevel::Binary(BinaryPowerLevel::On),
                },
                requires: ElementLevel {
                    id: "B".into(),
                    lvl: PowerLevel::Binary(BinaryPowerLevel::On),
                },
            })
            .expect("acquire failed");
        assert_eq!(transitive_leases.len(), 0);
        assert_ne!(lease.id, "");
        assert_eq!(lease.dependency.level.id, "A".into());
        assert_eq!(lease.dependency.level.lvl, PowerLevel::Binary(BinaryPowerLevel::On));
        assert_eq!(lease.dependency.requires.id, "B".into());
        assert_eq!(lease.dependency.requires.lvl, PowerLevel::Binary(BinaryPowerLevel::On));

        let (dropped, transitive_leases) = lessor.drop(&lease.id).expect("drop failed");
        assert_eq!(transitive_leases.len(), 0);
        assert_eq!(dropped.id, lease.id);
        assert_eq!(dropped.dependency.level.id, lease.dependency.level.id);
        assert_eq!(dropped.dependency.level.lvl, lease.dependency.level.lvl);
        assert_eq!(dropped.dependency.requires.id, lease.dependency.requires.id);
        assert_eq!(dropped.dependency.requires.lvl, lease.dependency.requires.lvl);

        let extra_drop = lessor.drop(&lease.id);
        assert!(extra_drop.is_err());
    }

    #[fuchsia::test]
    fn test_lessor_drop() {
        let mut lessor = LeaseCatalog::new(Topology::new());

        let (lease, transitive_leases) = lessor
            .acquire(Dependency {
                level: ElementLevel {
                    id: "A".into(),
                    lvl: PowerLevel::Binary(BinaryPowerLevel::On),
                },
                requires: ElementLevel {
                    id: "B".into(),
                    lvl: PowerLevel::Binary(BinaryPowerLevel::On),
                },
            })
            .expect("acquire failed");
        assert_eq!(transitive_leases.len(), 0);
        assert_ne!(lease.id, "");
        assert_eq!(lease.dependency.level.id, "A".into());
        assert_eq!(lease.dependency.level.lvl, PowerLevel::Binary(BinaryPowerLevel::On));
        assert_eq!(lease.dependency.requires.id, "B".into());
        assert_eq!(lease.dependency.requires.lvl, PowerLevel::Binary(BinaryPowerLevel::On));
    }

    #[fuchsia::test]
    fn test_lessor_acquire_drop_transitive() {
        let mut topology = Topology::new();
        // A <- B <- C
        let ba = Dependency {
            level: ElementLevel { id: "B".into(), lvl: PowerLevel::Binary(BinaryPowerLevel::On) },
            requires: ElementLevel {
                id: "A".into(),
                lvl: PowerLevel::Binary(BinaryPowerLevel::On),
            },
        };
        topology.add_direct_dep(&ba);
        let cb = Dependency {
            level: ElementLevel { id: "C".into(), lvl: PowerLevel::Binary(BinaryPowerLevel::On) },
            requires: ElementLevel {
                id: "B".into(),
                lvl: PowerLevel::Binary(BinaryPowerLevel::On),
            },
        };
        topology.add_direct_dep(&cb);
        let mut lessor = LeaseCatalog::new(topology);
        assert_eq!(
            lessor.calc_min_level(&"B".into()),
            PowerLevel::Binary(BinaryPowerLevel::Off),
            "B should start with min lvl OFF"
        );
        assert_eq!(
            lessor.calc_min_level(&"A".into()),
            PowerLevel::Binary(BinaryPowerLevel::Off),
            "A should start with min lvl OFF"
        );

        let (original_lease, transitive_leases) = lessor
            .acquire(Dependency {
                level: ElementLevel {
                    id: "C".into(),
                    lvl: PowerLevel::Binary(BinaryPowerLevel::On),
                },
                requires: ElementLevel {
                    id: "B".into(),
                    lvl: PowerLevel::Binary(BinaryPowerLevel::On),
                },
            })
            .expect("acquire failed");
        assert_eq!(transitive_leases.len(), 1);
        assert_eq!(
            lessor.calc_min_level(&"B".into()),
            PowerLevel::Binary(BinaryPowerLevel::On),
            "B should now have min lvl ON"
        );
        assert_eq!(
            lessor.calc_min_level(&"A".into()),
            PowerLevel::Binary(BinaryPowerLevel::On),
            "A should now have min lvl ON from transitive lease"
        );

        let (dropped, dropped_transitive_leases) =
            lessor.drop(&original_lease.id).expect("drop failed");
        assert_eq!(dropped.id, original_lease.id);
        assert_eq!(dropped_transitive_leases.len(), 1);
        assert_eq!(
            lessor.calc_min_level(&"B".into()),
            PowerLevel::Binary(BinaryPowerLevel::Off),
            "B should have min lvl OFF after lease drop"
        );
        assert_eq!(
            lessor.calc_min_level(&"A".into()),
            PowerLevel::Binary(BinaryPowerLevel::Off),
            "A should have min lvl OFF after transitive lease drop of B on A"
        );
    }

    #[fuchsia::test]
    fn test_lessor_calc_min_level() {
        let mut lessor = LeaseCatalog::new(Topology::new());
        assert_eq!(lessor.calc_min_level(&"A".into()), PowerLevel::Binary(BinaryPowerLevel::Off));
        assert_eq!(lessor.calc_min_level(&"B".into()), PowerLevel::Binary(BinaryPowerLevel::Off));

        let (lease, transitive_leases) = lessor
            .acquire(Dependency {
                level: ElementLevel {
                    id: "A".into(),
                    lvl: PowerLevel::Binary(BinaryPowerLevel::On),
                },
                requires: ElementLevel {
                    id: "B".into(),
                    lvl: PowerLevel::Binary(BinaryPowerLevel::On),
                },
            })
            .expect("acquire failed");
        assert_eq!(transitive_leases.len(), 0);
        assert_eq!(lessor.calc_min_level(&"A".into()), PowerLevel::Binary(BinaryPowerLevel::Off));
        assert_eq!(lessor.calc_min_level(&"B".into()), PowerLevel::Binary(BinaryPowerLevel::On));

        lessor.drop(&lease.id).expect("drop failed");
        assert_eq!(lessor.calc_min_level(&"A".into()), PowerLevel::Binary(BinaryPowerLevel::Off));
        assert_eq!(lessor.calc_min_level(&"B".into()), PowerLevel::Binary(BinaryPowerLevel::Off));
    }
}
