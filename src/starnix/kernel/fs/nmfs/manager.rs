// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::fs::nmfs::NetworkMessage;
use bstr::BString;
use fuchsia_inspect_derive::{IValue, Inspect, Unit, WithInspect};
use once_cell::sync::OnceCell;
use starnix_logging::log_error;
use starnix_sync::{Mutex, MutexGuard};
use starnix_uapi::errno;
use starnix_uapi::errors::Errno;
use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::sync::Arc;

/// Manager for communicating network properties.
#[derive(Inspect, Default)]
pub(crate) struct NetworkManager {
    // TODO(https://fxbug.dev/348639637): Change this to a Mutex-wrapped
    // Option so that the proxy can be re-initialized.
    starnix_networks: OnceCell<()>,
    #[inspect(forward)]
    inner: Mutex<IValue<NetworkManagerInner>>,
}

#[derive(Unit, Default)]
struct NetworkManagerInner {
    // Keeps track of networks and their [`NetworkMessage`].
    #[inspect(skip)]
    default_id: Option<u32>,
    #[inspect(skip)]
    networks: HashMap<u32, Option<NetworkMessage>>,

    default_ids_set: SeenSentData,
    added_networks: SeenSentData,
    updated_networks: SeenSentData,
    removed_networks: SeenSentData,
}

#[derive(Unit, Default)]
struct SeenSentData {
    // The number of event occurrences witnessed
    // by the NetworkManager.
    seen: u64,
    // The number of event occurrences that have been
    // sent successfully to the socketproxy.
    sent: u64,
}

// This is an Arc so that the Kernel can hold a reference to the
// NetworkManager across all of its threads.
#[derive(Clone)]
pub struct NetworkManagerHandle(pub(crate) Arc<NetworkManager>);

impl NetworkManagerHandle {
    /// Create a NetworkManagerHandle with a `nmfs` inspect node.
    pub fn new_with_inspect(node: &fuchsia_inspect::Node) -> Self {
        Self(Arc::new(
            NetworkManager::default()
                .with_inspect(node, "nmfs")
                .expect("Failed to attach 'nmfs' node"),
        ))
    }

    /// Initialize the connection to the socketproxy.
    pub fn init(&self) -> Result<(), anyhow::Error> {
        self.0.init()
    }
}

impl NetworkManager {
    fn init(&self) -> Result<(), anyhow::Error> {
        // TODO(https://fxbug.dev/339448114): Connect to the socketproxy.
        self.starnix_networks.set(()).expect("Starnix Networks should be uninitialized");
        Ok(())
    }

    // Locks and returns the inner state of the manager.
    fn lock(&self) -> MutexGuard<'_, IValue<NetworkManagerInner>> {
        self.inner.lock()
    }

    pub(crate) fn get_default_network_id(&self) -> Option<u32> {
        self.lock().default_id
    }

    pub(crate) fn get_network(&self, network_id: &u32) -> Option<Option<NetworkMessage>> {
        self.lock().networks.get(network_id).cloned()
    }

    pub(crate) fn get_default_id_as_bytes(&self) -> BString {
        let default_id = match self.get_default_network_id() {
            Some(id) => id.to_string(),
            None => "".to_string(),
        };
        default_id.into_bytes().into()
    }

    pub(crate) fn get_network_by_id_as_bytes(&self, network_id: u32) -> BString {
        let network_info = match self.get_network(&network_id) {
            Some(Some(network)) => serde_json::to_string(&network).unwrap_or("{}".to_string()),
            // A network with that was created but hasn't yet
            // been populated with network properties.
            Some(None) | None => "{}".to_string(),
        };
        network_info.into_bytes().into()
    }

    // Set the default network identifier.
    pub(crate) fn set_default_network_id(&self, network_id: Option<u32>) {
        let mut inner_guard = self.lock();
        let mut inner = inner_guard.as_mut();
        inner.default_id = network_id;
        inner.default_ids_set.seen += 1;

        // TODO(https://fxbug.dev/339448114): Make the call in the socketproxy.
    }

    // Populate a new element in the Map.
    //
    // An error will be returned if a network with the id
    // exists in the local state.
    pub(crate) fn add_empty_network(&self, network_id: u32) -> Result<(), Errno> {
        let mut inner_guard = self.lock();
        match inner_guard.as_mut().networks.entry(network_id) {
            Entry::Occupied(_) => {
                log_error!(
                    "Failed to add empty network to HashMap, was present for id: {}",
                    network_id
                );
                return Err(errno!(EEXIST));
            }
            Entry::Vacant(entry) => entry.insert(None),
        };
        Ok(())
    }

    // Add a new network.
    //
    // An error will be returned if a network with the id
    // exists in the local state.
    pub(crate) fn add_network(&self, network: NetworkMessage) -> Result<(), Errno> {
        let mut inner_guard = self.lock();
        let mut inner = inner_guard.as_mut();
        match inner.networks.entry(network.netid) {
            Entry::Occupied(mut entry) => {
                // This is deliberately before any Map manipulation because we
                // should not modify the Map state if we return an error.
                if let Some(network) = entry.get() {
                    log_error!(
                        "Failed to add network with id {} to HashMap, already existed",
                        network.netid
                    );
                    return Err(errno!(EEXIST));
                }
                let _ = entry.insert(Some(network.clone()));
            }
            Entry::Vacant(entry) => {
                let _ = entry.insert(Some(network.clone()));
            }
        }
        inner.added_networks.seen += 1;

        // TODO(https://fxbug.dev/339448114): Make the call in the socketproxy.

        Ok(())
    }

    // Update an existing network.
    //
    // An error will be returned if a network with the id does not
    // exist in the local state.
    pub(crate) fn update_network(&self, network: NetworkMessage) -> Result<(), Errno> {
        // Ensure that there is a network already present at that netid
        // prior to modifying the map.
        let mut inner_guard = self.lock();
        let mut inner = inner_guard.as_mut();
        match inner.networks.entry(network.netid) {
            Entry::Occupied(mut entry) => {
                if let None = entry.get() {
                    return Err(errno!(ENOENT));
                }
                entry.insert(Some(network.clone()));
            }
            Entry::Vacant(_) => {
                return Err(errno!(ENOENT));
            }
        };

        inner.updated_networks.seen += 1;

        // TODO(https://fxbug.dev/339448114): Make the call in the socketproxy.

        Ok(())
    }

    // Remove an existing network.
    //
    // An error will be returned if a network with the id does not
    // exist in the local state.
    pub(crate) fn remove_network(&self, network_id: u32) -> Result<(), Errno> {
        // Surface an error if the network is the current default
        // network or if the network is not found.
        let default_network_id = self.get_default_network_id();
        if let Some(id) = default_network_id {
            if id == network_id {
                return Err(errno!(EPERM));
            }
        }
        let mut inner_guard = self.lock();
        let mut inner = inner_guard.as_mut();
        if let None = inner.networks.remove(&network_id) {
            return Err(errno!(ENOENT));
        }
        inner.removed_networks.seen += 1;

        // TODO(https://fxbug.dev/339448114): Make the call in the socketproxy.

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use assert_matches::assert_matches;
    use diagnostics_assertions::assert_data_tree;
    use starnix_uapi::{EEXIST, ENOENT, EPERM};

    fn test_network_message_from_id(netid: u32) -> NetworkMessage {
        NetworkMessage { netid, ..Default::default() }
    }

    #[::fuchsia::test]
    fn test_set_default_network_id() {
        let inspector = fuchsia_inspect::Inspector::default();
        let manager = &NetworkManagerHandle::new_with_inspect(inspector.root()).0;

        let default: Option<u32> = Some(1);
        manager.set_default_network_id(default);
        assert_eq!(manager.get_default_network_id(), default);
        assert_data_tree!(inspector, root: {
            nmfs: contains {
                default_ids_set: {
                    seen: 1u64,
                    sent: 0u64,
                },
            },
        });

        let default: Option<u32> = None;
        manager.set_default_network_id(default);
        assert_eq!(manager.get_default_network_id(), default);
        assert_data_tree!(inspector, root: {
            nmfs: contains {
                default_ids_set: {
                    seen: 2u64,
                    sent: 0u64,
                },
            },
        });
    }

    #[::fuchsia::test]
    fn test_add_network() {
        let inspector = fuchsia_inspect::Inspector::default();
        let manager = &NetworkManagerHandle::new_with_inspect(inspector.root()).0;

        let network1 = test_network_message_from_id(1);
        assert_matches!(manager.add_network(network1.clone()), Ok(()));
        assert_data_tree!(inspector, root: {
            nmfs: contains {
                added_networks: {
                    seen: 1u64,
                    sent: 0u64,
                },
            },
        });

        // Ensure we cannot add a network with the same id twice.
        assert_matches!(
            manager.add_network(network1),
            Err(errno) => errno.code.error_code() == EEXIST
        );

        // Ensure we can add a network with a different id.
        let network2 = test_network_message_from_id(2);
        assert_matches!(manager.add_network(network2.clone()), Ok(()));
        assert_data_tree!(inspector, root: {
            nmfs: contains {
                added_networks: {
                    seen: 2u64,
                    sent: 0u64,
                },
            },
        });
    }

    #[::fuchsia::test]
    fn test_update_network() {
        let inspector = fuchsia_inspect::Inspector::default();
        let manager = &NetworkManagerHandle::new_with_inspect(inspector.root()).0;

        let network_id = 1;
        let network_initial = test_network_message_from_id(network_id);
        // Ensure we cannot update a network that doesn't exist.
        assert_matches!(
            manager.update_network(network_initial.clone()),
            Err(errno) => errno.code.error_code() == ENOENT
        );

        // Insert the network manually and then update the network. The
        // retrieved network should have the same properties as the
        // network used to update the entry.
        {
            let mut inner_guard = manager.lock();
            let _ = inner_guard.as_mut().networks.insert(network_id, Some(network_initial.clone()));
        }
        let new_mark = 1;
        let mut network_new = network_initial.clone();
        network_new.mark = new_mark;
        assert_matches!(manager.update_network(network_new.clone()), Ok(()));
        assert_matches!(
            manager.get_network(&network_id),
            Some(Some(network_new)) => network_new.mark == new_mark
        );
        assert_data_tree!(inspector, root: {
            nmfs: contains {
                updated_networks: {
                    seen: 1u64,
                    sent: 0u64,
                },
            },
        });
    }

    #[::fuchsia::test]
    fn test_add_empty_network() {
        let inspector = fuchsia_inspect::Inspector::default();
        let manager = &NetworkManagerHandle::new_with_inspect(inspector.root()).0;

        let network_id = 1;
        assert_matches!(manager.add_empty_network(network_id), Ok(()));

        // Ensure we cannot add an empty network with the same id twice.
        assert_matches!(
            manager.add_empty_network(network_id),
            Err(errno) => errno.code.error_code() == EEXIST
        );
        assert_matches!(manager.get_network(&network_id), Some(None));
        // Empty networks don't get sent to the socketproxy, so they are
        // ignored in SeenSentData.
        assert_data_tree!(inspector, root: {
            nmfs: contains {
                added_networks: {
                    seen: 0u64,
                    sent: 0u64,
                },
            },
        });
    }

    #[::fuchsia::test]
    fn test_remove_network() {
        let inspector = fuchsia_inspect::Inspector::default();
        let manager = &NetworkManagerHandle::new_with_inspect(inspector.root()).0;

        let network_id = 1;
        let network = test_network_message_from_id(network_id);
        // Ensure we cannot remove a network that doesn't exist.
        assert_matches!(
            manager.remove_network(network_id),
            Err(errno) => errno.code.error_code() == ENOENT
        );

        // Insert the network manually and then remove it.
        {
            let mut inner_guard = manager.lock();
            let _ = inner_guard.as_mut().networks.insert(network_id, Some(network.clone()));
        }
        assert_matches!(manager.remove_network(network_id), Ok(()));

        // Ensure we cannot remove the network again.
        assert_matches!(
            manager.remove_network(network_id),
            Err(errno) => errno.code.error_code() == ENOENT
        );
        assert_data_tree!(inspector, root: {
            nmfs: contains {
                removed_networks: {
                    seen: 1u64,
                    sent: 0u64,
                },
            },
        });
    }

    #[::fuchsia::test]
    fn test_multiple_operations() {
        let inspector = fuchsia_inspect::Inspector::default();
        let manager = &NetworkManagerHandle::new_with_inspect(inspector.root()).0;

        // Add a network.
        let network1 = test_network_message_from_id(1);
        assert_matches!(manager.add_network(network1.clone()), Ok(()));
        assert_data_tree!(inspector, root: {
            nmfs: contains {
                added_networks: {
                    seen: 1u64,
                    sent: 0u64,
                },
            },
        });

        // Set the default network.
        manager.set_default_network_id(Some(1));
        assert_eq!(manager.get_default_network_id(), Some(1));
        assert_data_tree!(inspector, root: {
            nmfs: contains {
                default_ids_set: {
                    seen: 1u64,
                    sent: 0u64,
                },
            },
        });

        // Update the network.
        let mut network1_updated = network1.clone();
        network1_updated.mark = 1;
        assert_matches!(manager.update_network(network1_updated.clone()), Ok(()));
        assert_matches!(
            manager.get_network(&network1.netid),
            Some(Some(network)) => network.mark == 1
        );
        assert_data_tree!(inspector, root: {
            nmfs: contains {
                updated_networks: {
                    seen: 1u64,
                    sent: 0u64,
                },
            },
        });

        // Add another network.
        let network2 = test_network_message_from_id(2);
        assert_matches!(manager.add_network(network2.clone()), Ok(()));
        assert_data_tree!(inspector, root: {
            nmfs: contains {
                added_networks: {
                    seen: 2u64,
                    sent: 0u64,
                },
            },
        });

        // Attempt to remove the first network. We should get an error
        // since network1 is the current default network.
        assert_matches!(
            manager.remove_network(network1.netid),
            Err(errno) => errno.code.error_code() == EPERM
        );

        // Ensure the first network still exists.
        assert_eq!(manager.get_network(&network1.netid), Some(Some(network1_updated.clone())));

        // Set the default network to the second network.
        manager.set_default_network_id(Some(2));
        assert_eq!(manager.get_default_network_id(), Some(2));
        assert_data_tree!(inspector, root: {
            nmfs: contains {
                default_ids_set: {
                    seen: 2u64,
                    sent: 0u64,
                },
            },
        });

        // Remove the first network as it is no longer the default network.
        assert_matches!(manager.remove_network(network1.netid), Ok(()));
        assert_data_tree!(inspector, root: {
            nmfs: {
                default_ids_set: {
                    seen: 2u64,
                    sent: 0u64,
                },
                added_networks: {
                    seen: 2u64,
                    sent: 0u64,
                },
                updated_networks: {
                    seen: 1u64,
                    sent: 0u64,
                },
                removed_networks: {
                    seen: 1u64,
                    sent: 0u64,
                },
            },
        });
    }
}
