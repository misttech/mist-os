// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::fs::nmfs::NetworkMessage;
use crate::task::Kernel;
use bstr::BString;
use fuchsia_component::client::connect_to_protocol_sync;
use fuchsia_inspect_derive::{IValue, Inspect, Unit, WithInspect};
use futures::channel::{mpsc, oneshot};
use futures::{FutureExt as _, StreamExt as _};
use starnix_logging::{log_error, log_info};
use starnix_sync::{Mutex, MutexGuard};
use starnix_uapi::errno;
use starnix_uapi::errors::Errno;
use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use thiserror::Error;

use fidl_fuchsia_netpol_socketproxy as fnp_socketproxy;

/// Manager for communicating network properties.
#[derive(Inspect, Default)]
pub(crate) struct NetworkManager {
    starnix_networks: Mutex<Option<fnp_socketproxy::StarnixNetworksSynchronousProxy>>,
    // Timeout for thread-blocking calls to the socketproxy.
    proxy_timeout: zx::MonotonicDuration,
    // Sender for requests to initiate a socketproxy reconnect.
    initiate_reconnect_sender: OnceLock<mpsc::Sender<()>>,
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
    attempted_reconnects: u64,
    successful_reconnects: u64,
    no_events_to_replay: u64,
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
            NetworkManager {
                proxy_timeout: zx::MonotonicDuration::from_seconds(1),
                ..Default::default()
            }
            .with_inspect(node, "nmfs")
            .expect("Failed to attach 'nmfs' node"),
        ))
    }

    /// Initialize the connection to the socketproxy.
    pub fn init(&self, kernel: &Arc<Kernel>) -> Result<(), anyhow::Error> {
        self.0.init(kernel)
    }
}

// The functions that propagate calls to the socketproxy prioritize maintaining
// a correct version of local state and logging an error if the socketproxy
// state is not aligned. By maintaining a Map of network properties
// `NetworkManager` can replay the networks to the socketproxy in the case of
// the component restarting.
impl NetworkManager {
    fn init(&self, kernel: &Arc<Kernel>) -> Result<(), anyhow::Error> {
        let starnix_networks =
            connect_to_protocol_sync::<fnp_socketproxy::StarnixNetworksMarker>()?;
        let (sender, receiver) = mpsc::channel::<()>(1);

        let _ = self.starnix_networks.lock().replace(starnix_networks);
        self.initiate_reconnect_sender.set(sender).expect("init should only be called once");
        spawn_reconnection_thread(kernel, receiver);

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
            Some(Some(network)) => {
                serde_json::to_string(&network).unwrap_or_else(|_| "{}".to_string())
            }
            // A network with that was created but hasn't yet
            // been populated with network properties.
            Some(None) | None => "{}".to_string(),
        };
        network_info.into_bytes().into()
    }

    // The state between the Manager and the socketproxy is
    // inconsistent. Attempt a reconnect in order to reset the
    // socketproxy's network state.
    fn trigger_reconnect(&self) {
        match self.initiate_reconnect_sender.get() {
            Some(sender) => {
                let mut sender_mut = sender.clone();
                if let Err(e) = sender_mut.try_send(()) {
                    if e.is_full() {
                        log_info!(
                            "Ignoring reconnection request, \
                            reconnection already ongoing"
                        );
                    }

                    if e.is_disconnected() {
                        panic!(
                            "Channel disconnected; system is in \
                            unrecoverable state"
                        )
                    }
                }
            }
            None => {
                // Do nothing, the sender wasn't set. The socketproxy is not enabled.
                // Realistically, this only gets hit in tests because
                // `NetworkManager::ProxyNotInitialized` gets returned when the proxy does
                // not exist, not a socketproxy-specific error.
            }
        }
    }

    // Set the default network identifier. Propagate the change to the
    // socketproxy when present.
    pub(crate) fn set_default_network_id(&self, network_id: Option<u32>) {
        {
            let mut inner_guard = self.lock();
            let mut inner = inner_guard.as_mut();
            inner.default_id = network_id;
            inner.default_ids_set.seen += 1;
        }

        // Only log when there is an internal proxy error. The `ProxyNotInitialized`
        // variant likely means that the `network_manager` feature is disabled.
        match self.fidl_set_default_network_id(network_id) {
            Ok(()) => {
                log_info!(
                    "Successfully set network with id {network_id:?} as default in socketproxy",
                );
                let mut inner_guard = self.lock();
                inner_guard.as_mut().default_ids_set.sent += 1;
            }
            Err(NetworkManagerError::ProxyNotInitialized) => {}
            Err(e) => {
                log_error!(
                    "Failed to set network with id {network_id:?} as default in socketproxy; {e:?}"
                );
                self.trigger_reconnect();
            }
        }
    }

    // Populate a new element in the Map. This does not
    // propagate to the socketproxy.
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

    // Add a new network. Propagate the change to the
    // socketproxy when present.
    //
    // An error will be returned if a network with the id
    // exists in the local state.
    pub(crate) fn add_network(&self, network: NetworkMessage) -> Result<(), Errno> {
        {
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
        }

        // Only log when there is an internal proxy error. The `ProxyNotInitialized`
        // variant likely means that the `network_manager` feature is disabled.
        match self.fidl_add_network(&fnp_socketproxy::Network::from(&network)) {
            Ok(()) => {
                log_info!("Successfully added network with id {} to socketproxy", network.netid);
                let mut inner_guard = self.lock();
                inner_guard.as_mut().added_networks.sent += 1;
            }
            Err(NetworkManagerError::ProxyNotInitialized) => {}
            Err(e) => {
                log_error!(
                    "Failed to add network with id {:?} to socketproxy; {e:?}",
                    network.netid
                );
                self.trigger_reconnect();
            }
        }

        Ok(())
    }

    // Update an existing network. Propagate the change to the
    // socketproxy when present.
    //
    // An error will be returned if a network with the id does not
    // exist in the local state.
    pub(crate) fn update_network(&self, network: NetworkMessage) -> Result<(), Errno> {
        {
            let mut inner_guard = self.lock();
            let mut inner = inner_guard.as_mut();
            // Ensure that there is a network already present at that netid
            // prior to modifying the map.
            let _old_network = match inner.networks.entry(network.netid) {
                Entry::Occupied(mut entry) => {
                    if let None = entry.get() {
                        return Err(errno!(ENOENT));
                    }
                    entry.insert(Some(network.clone()))
                }
                Entry::Vacant(_) => {
                    return Err(errno!(ENOENT));
                }
            };
            inner.updated_networks.seen += 1;
        };

        // Only log when there is an internal proxy error. The `ProxyNotInitialized`
        // variant likely means that the `network_manager` feature is disabled.
        match self.fidl_update_network(&fnp_socketproxy::Network::from(&network)) {
            Ok(()) => {
                log_info!("Successfully updated network with id {} in socketproxy", network.netid);
                let mut inner_guard = self.lock();
                inner_guard.as_mut().updated_networks.sent += 1;
            }
            Err(NetworkManagerError::ProxyNotInitialized) => {}
            Err(e) => {
                log_error!(
                    "Failed to update network with id {} in socketproxy; {e:?}",
                    network.netid
                );
                self.trigger_reconnect();
            }
        }

        Ok(())
    }

    // Remove an existing network. Propagate the change to the
    // socketproxy when present.
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
        {
            let mut inner_guard = self.lock();
            let mut inner = inner_guard.as_mut();
            if let None = inner.networks.remove(&network_id) {
                return Err(errno!(ENOENT));
            }
            inner.removed_networks.seen += 1;
        }

        // Only log when there is an internal proxy error. The `ProxyNotInitialized`
        // variant likely means that the `network_manager` feature is disabled.
        match self.fidl_remove_network(&network_id) {
            Ok(()) => {
                log_info!("Successfully removed network with id {network_id} from socketproxy",);
                let mut inner_guard = self.lock();
                inner_guard.as_mut().removed_networks.sent += 1;
            }
            Err(NetworkManagerError::ProxyNotInitialized) => {}
            Err(e) => {
                log_error!("Failed to remove network with id {network_id} in socketproxy; {e:?}");
                self.trigger_reconnect();
            }
        }
        Ok(())
    }

    // Transmit the current network status to the socketproxy as a sequence of
    // events. The only events types sent are `add_network` and
    // `set_default_network_id`. If the default network is set,
    // `set_default_network_id` will be the last event transmitted. This
    // function holds the lock on `inner` so that all events can get sent in
    // a single batch to the socketproxy without interleaving.
    //
    // An error will be returned on the first event that is not
    // transmitted successfully.
    pub(crate) fn replay_network_events(&self) -> Result<usize, NetworkManagerError> {
        let mut inner_guard = self.lock();
        let mut inner = inner_guard.as_mut();

        let fidl_events: Vec<fnp_socketproxy::Network> = inner
            .networks
            .values()
            .filter_map(|network| match network {
                // Intentionally use a `match` instead of a `map`
                // to avoid the need to clone `network`.
                Some(network) => Some(network.into()),
                None => None,
            })
            .collect();
        let mut num_events = fidl_events.len();

        // This cannot be combined with the above statements due to
        // this block needing mutable access to `inner`.
        fidl_events.iter().try_for_each(|network| {
            let res = self.fidl_add_network(network);
            if res.is_ok() {
                inner.added_networks.sent += 1;
            }
            res
        })?;

        if let Some(id) = inner.default_id {
            num_events += 1;
            self.fidl_set_default_network_id(Some(id))?;
            inner.default_ids_set.sent += 1;
        }

        Ok(num_events)
    }

    // Call `set_default` on `StarnixNetworks`.
    fn fidl_set_default_network_id(
        &self,
        network_id: Option<u32>,
    ) -> Result<(), NetworkManagerError> {
        let binding = self.starnix_networks.lock();
        let starnix_networks = binding.as_ref().ok_or(NetworkManagerError::ProxyNotInitialized)?;
        let network_id = match network_id {
            Some(id) => fidl_fuchsia_posix_socket::OptionalUint32::Value(id),
            None => {
                fidl_fuchsia_posix_socket::OptionalUint32::Unset(fidl_fuchsia_posix_socket::Empty)
            }
        };
        Ok(starnix_networks
            .set_default(&network_id, zx::MonotonicInstant::after(self.proxy_timeout))??)
    }

    // Call `add` on `StarnixNetworks`.
    fn fidl_add_network(
        &self,
        network: &fnp_socketproxy::Network,
    ) -> Result<(), NetworkManagerError> {
        let binding = self.starnix_networks.lock();
        let starnix_networks = binding.as_ref().ok_or(NetworkManagerError::ProxyNotInitialized)?;
        Ok(starnix_networks.add(&network, zx::MonotonicInstant::after(self.proxy_timeout))??)
    }

    // Call `update` on `StarnixNetworks`.
    fn fidl_update_network(
        &self,
        network: &fnp_socketproxy::Network,
    ) -> Result<(), NetworkManagerError> {
        let binding = self.starnix_networks.lock();
        let starnix_networks = binding.as_ref().ok_or(NetworkManagerError::ProxyNotInitialized)?;
        Ok(starnix_networks.update(&network, zx::MonotonicInstant::after(self.proxy_timeout))??)
    }

    // Call `remove` on `StarnixNetworks`.
    fn fidl_remove_network(&self, network_id: &u32) -> Result<(), NetworkManagerError> {
        let binding = self.starnix_networks.lock();
        let starnix_networks = binding.as_ref().ok_or(NetworkManagerError::ProxyNotInitialized)?;
        Ok(starnix_networks
            .remove(*network_id, zx::MonotonicInstant::after(self.proxy_timeout))??)
    }
}

fn spawn_reconnection_thread(
    kernel: &Arc<Kernel>,
    initiate_reconnect_receiver: mpsc::Receiver<()>,
) {
    let handle = kernel.network_manager.clone();
    kernel.kthreads.spawn_future(async move {
        // Channel to handle requests to re-connect the socketproxy proxy.
        // The oneshot response will permit the reconnection loop to `replay_network_events`.
        let (sender, mut receiver) = mpsc::channel::<oneshot::Sender<bool>>(1);

        let mut reconnect_fut = std::pin::pin!(reconnect_to_proxy_loop(handle.clone(), sender, initiate_reconnect_receiver).fuse());
        loop {
            futures::select! {
                _ = reconnect_fut => unreachable!("Reconnect thread should never complete"),
                oneshot_sender = receiver.select_next_some() => {
                    let did_reset_proxy = match connect_to_protocol_sync::<fnp_socketproxy::StarnixNetworksMarker>() {
                        Ok(starnix_networks) => {
                            let _ = handle.0.starnix_networks.lock().replace(starnix_networks);
                            true
                        },
                        Err(e) => {
                            log_error!("Failed to reconnect to proxy: {e:?}");
                            false
                        }
                    };
                    match oneshot_sender.send(did_reset_proxy) {
                        Ok(()) => {},
                        Err(_) => log_error!("Receiver end was dropped"),
                    };
                }
            }
        }
    });
}

async fn reconnect_to_proxy_loop(
    handle: NetworkManagerHandle,
    mut sender: mpsc::Sender<oneshot::Sender<bool>>,
    mut initiate_reconnect_receiver: mpsc::Receiver<()>,
) {
    // The amount of time the thread should wait until it attempts to reconnect
    // to the socketproxy protocol and replay the network events.
    let mut reconnect_delay = zx::MonotonicDuration::from_seconds(0);
    // Each time this stream yields, attempt to reconnect to the proxy and replay events.
    let mut reconnect_futures = futures::stream::FuturesUnordered::new();
    loop {
        futures::select! {
            _ = initiate_reconnect_receiver.select_next_some() => {
                // Only act on the initiate request when there is not already an
                // ongoing attempt to reconnect to the socketproxy.
                if reconnect_futures.is_empty() {
                    log_info!("Attempting to reconnect to socketproxy");
                    reconnect_futures.push(fuchsia_async::Interval::new(reconnect_delay).into_future())
                }
            }
            _ = reconnect_futures.select_next_some() => {
                {
                    let mut inner_guard = handle.0.lock();
                    let mut inner = inner_guard.as_mut();
                    inner.attempted_reconnects += 1;
                }
                // Initiate a proxy reconnection to StarnixNetworks.
                let (reconnect_sender, reconnect_receiver) = oneshot::channel::<bool>();
                if let Err(e) = sender.try_send(reconnect_sender) {
                    log_error!("Failed to send message to initiate proxy reconnect: {e:?}");
                        reconnect_delay += zx::MonotonicDuration::from_seconds(1);
                        reconnect_futures.push(fuchsia_async::Interval::new(reconnect_delay).into_future());
                        continue;
                }

                match reconnect_receiver.await {
                    Ok(true) => {
                        // Proxy was reset successfully. Replay network events.
                    }
                    Ok(false) | Err(_) => {
                        log_error!("Unsucessfully re-connected proxy. Re-attempting \
                            in {reconnect_delay:?}...");
                        reconnect_delay += zx::MonotonicDuration::from_seconds(1);
                        reconnect_futures.push(fuchsia_async::Interval::new(reconnect_delay).into_future());
                        continue;
                    }
                }

                match handle.0.replay_network_events() {
                    Ok(0) => {
                        log_info!("There were no events to replay to the socketproxy.");
                        // Reset `reconnect_delay` to the initial delay period.
                        {
                            let mut inner_guard = handle.0.lock();
                            let mut inner = inner_guard.as_mut();
                            inner.no_events_to_replay += 1;
                        }
                    }
                    Ok(num_events) => {
                        log_info!("Successfully reconnected to socketproxy and \
                            replayed {num_events:?} events");
                        // On success, reset `reconnect_delay` to the initial delay period.
                        reconnect_delay = zx::MonotonicDuration::from_seconds(0);
                        {
                            let mut inner_guard = handle.0.lock();
                            let mut inner = inner_guard.as_mut();
                            inner.successful_reconnects += 1;
                        }
                    }
                    Err(e) => {
                        // On failure, re-attempt to connect to the socketproxy in
                        // `reconnect_delay` seconds.
                        match e {
                            NetworkManagerError::Fidl(fidl_error) => {
                                log_error!("On replay, received FIDL error: {fidl_error:?}. \
                                    Re-attempting in {reconnect_delay:?}...");
                            }
                            NetworkManagerError::Add(_) |
                            NetworkManagerError::SetDefault(_) => {
                                log_error!("On replay, socket proxy responded but states are \
                                    irreconcilable: {e:?}. Re-attempting in {reconnect_delay:?}...");
                            }
                            NetworkManagerError::ProxyNotInitialized |
                            NetworkManagerError::Remove(_) |
                            NetworkManagerError::Update(_) => {
                                unreachable!("responses not possible from `replay_network_events`");
                            }
                        }
                        reconnect_delay += zx::MonotonicDuration::from_seconds(1);
                        reconnect_futures.push(
                            fuchsia_async::Interval::new(reconnect_delay).into_future()
                        );
                    }
                }
            }
        }
    }
}

// Errors produced when communicating updates to
// the socket proxy.
#[derive(Clone, Debug, Error)]
pub(crate) enum NetworkManagerError {
    #[error("Error during socketproxy Add: {0:?}")]
    Add(fnp_socketproxy::NetworkRegistryAddError),
    #[error("Error calling FIDL on socketproxy: {0:?}")]
    Fidl(#[from] fidl::Error),
    #[error("Proxy was not initialized prior to use")]
    ProxyNotInitialized,
    #[error("Error during socketproxy Remove: {0:?}")]
    Remove(fnp_socketproxy::NetworkRegistryRemoveError),
    #[error("Error during socketproxy SetDefault: {0:?}")]
    SetDefault(fnp_socketproxy::NetworkRegistrySetDefaultError),
    #[error("Error during socketproxy Update: {0:?}")]
    Update(fnp_socketproxy::NetworkRegistryUpdateError),
}

impl From<fnp_socketproxy::NetworkRegistryAddError> for NetworkManagerError {
    fn from(error: fnp_socketproxy::NetworkRegistryAddError) -> Self {
        NetworkManagerError::Add(error)
    }
}

impl From<fnp_socketproxy::NetworkRegistryRemoveError> for NetworkManagerError {
    fn from(error: fnp_socketproxy::NetworkRegistryRemoveError) -> Self {
        NetworkManagerError::Remove(error)
    }
}

impl From<fnp_socketproxy::NetworkRegistrySetDefaultError> for NetworkManagerError {
    fn from(error: fnp_socketproxy::NetworkRegistrySetDefaultError) -> Self {
        NetworkManagerError::SetDefault(error)
    }
}

impl From<fnp_socketproxy::NetworkRegistryUpdateError> for NetworkManagerError {
    fn from(error: fnp_socketproxy::NetworkRegistryUpdateError) -> Self {
        NetworkManagerError::Update(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use assert_matches::assert_matches;
    use diagnostics_assertions::{assert_data_tree, tree_assertion};
    use fuchsia_inspect::DiagnosticsHierarchyGetter;
    use starnix_uapi::{EEXIST, ENOENT, EPERM};
    use test_case::test_case;

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
            nmfs: contains {
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

    #[::fuchsia::test]
    fn replay_network_events() {
        let inspector = fuchsia_inspect::Inspector::default();
        let manager = NetworkManagerHandle::new_with_inspect(inspector.root());

        // If there aren't any networks in the Manager, replay should succeed
        // even if the socketproxy isn't initialized.
        assert_matches!(manager.0.replay_network_events(), Ok(0));

        // Manually insert a network to simulate this network
        // already existing in the Manager.
        {
            let mut inner = manager.0.lock();
            let network_id = 1;
            let _ = inner
                .as_mut()
                .networks
                .insert(network_id, Some(test_network_message_from_id(network_id)));
        }

        assert_matches!(
            manager.0.replay_network_events(),
            Err(NetworkManagerError::ProxyNotInitialized)
        );
        assert_data_tree!(inspector, root: {
            nmfs: contains {
                added_networks: {
                    seen: 0u64,
                    sent: 0u64,
                },
            },
        });
    }

    // Create a NetworkManagerHandle for use with testing. Use an infinite
    // timeout because we know that the mock socketproxy is guaranteed to send
    // a response for every request.
    fn create_test_network_manager_handle(node: &fuchsia_inspect::Node) -> NetworkManagerHandle {
        NetworkManagerHandle(Arc::new(
            NetworkManager { proxy_timeout: zx::MonotonicDuration::INFINITE, ..Default::default() }
                .with_inspect(node, "nmfs")
                .expect("Failed to attach 'nmfs' node"),
        ))
    }

    // Set the `StarnixNetworksMarker` in the NetworkManager and mock out
    // the responses to `StarnixNetworksRequest`s with provided results.
    fn setup_proxy(
        inspect_node: &fuchsia_inspect::Node,
        results: Vec<Result<(), NetworkManagerError>>,
        initiate_reconnect_sender: Option<mpsc::Sender<()>>,
    ) -> NetworkManagerHandle {
        let manager = create_test_network_manager_handle(inspect_node);
        let (proxy, mut stream) = fidl::endpoints::create_sync_proxy_and_stream::<
            fnp_socketproxy::StarnixNetworksMarker,
        >();
        let _ = manager.0.starnix_networks.lock().replace(proxy);
        if let Some(sender) = initiate_reconnect_sender {
            manager
                .0
                .initiate_reconnect_sender
                .set(sender)
                .expect("setup_proxy should only be called once");
        }

        let mut results = results.into_iter();
        fuchsia_async::Task::spawn(async move {
            while let Some(item) = stream.next().await {
                let result = results
                    .next()
                    .expect("there should be an equivalent # of results and requests");
                match item.expect("receive request") {
                    fnp_socketproxy::StarnixNetworksRequest::SetDefault {
                        network_id: _,
                        responder,
                    } => {
                        let res = result.map_err(|e| match e {
                            NetworkManagerError::SetDefault(err) => err,
                            _ => unreachable!("should have been SetDefault error variant"),
                        });
                        responder.send(res).expect("respond to SetDefault");
                    }
                    fnp_socketproxy::StarnixNetworksRequest::Add { network: _, responder } => {
                        let res = result.map_err(|e| match e {
                            NetworkManagerError::Add(err) => err,
                            _ => unreachable!("should have been Add error variant"),
                        });
                        responder.send(res).expect("respond to Add");
                    }
                    fnp_socketproxy::StarnixNetworksRequest::Update { network: _, responder } => {
                        let res = result.map_err(|e| match e {
                            NetworkManagerError::Update(err) => err,
                            _ => unreachable!("should have been Update error variant"),
                        });
                        responder.send(res).expect("respond to Update");
                    }
                    fnp_socketproxy::StarnixNetworksRequest::Remove {
                        network_id: _,
                        responder,
                    } => {
                        let res = result.map_err(|e| match e {
                            NetworkManagerError::Remove(err) => err,
                            _ => unreachable!("should have been Remove error variant"),
                        });
                        responder.send(res).expect("respond to Remove");
                    }
                    fnp_socketproxy::StarnixNetworksRequest::CheckPresence { responder: _ } => {
                        unreachable!("not called in tests")
                    }
                }
            }
        })
        .detach();
        manager
    }

    // Mock of private `manager::SeenSentData` to use for
    // improved test case readability.
    struct SeenSentData {
        seen: u64,
        sent: u64,
    }

    #[test_case(vec![Ok(()), Ok(())], SeenSentData { seen: 2, sent: 2 }; "all_success")]
    #[test_case(
        vec![
            Ok(()),
            Err(NetworkManagerError::SetDefault(fnp_socketproxy::NetworkRegistrySetDefaultError::NotFound)),
        ],
        SeenSentData { seen: 2, sent: 1 };
        "one_error"
    )]
    #[test_case(
        vec![
            Err(NetworkManagerError::SetDefault(fnp_socketproxy::NetworkRegistrySetDefaultError::NotFound)),
            Err(NetworkManagerError::SetDefault(fnp_socketproxy::NetworkRegistrySetDefaultError::NotFound)),
        ],
        SeenSentData { seen: 2, sent: 0 };
        "all_errors"
    )]
    #[::fuchsia::test(threads = 2)]
    async fn test_set_default_network_id_with_proxy(
        results: Vec<Result<(), NetworkManagerError>>,
        expected_data: SeenSentData,
    ) {
        let inspector = fuchsia_inspect::Inspector::default();
        let manager = &setup_proxy(inspector.root(), results, None).0;

        manager.set_default_network_id(Some(1));
        assert_eq!(manager.get_default_network_id(), Some(1));

        manager.set_default_network_id(Some(2));
        assert_eq!(manager.get_default_network_id(), Some(2));

        assert_data_tree!(inspector, root: {
            nmfs: contains {
                default_ids_set: {
                    seen: expected_data.seen,
                    sent: expected_data.sent,
                },
                added_networks: {
                    seen: 0u64,
                    sent: 0u64,
                },
                updated_networks: {
                    seen: 0u64,
                    sent: 0u64,
                },
                removed_networks: {
                    seen: 0u64,
                    sent: 0u64,
                },
            },
        });
    }

    #[test_case(vec![Ok(()), Ok(())], SeenSentData { seen: 2, sent: 2 }; "all_success")]
    #[test_case(
        vec![
            Ok(()),
            Err(NetworkManagerError::Add(fnp_socketproxy::NetworkRegistryAddError::DuplicateNetworkId)),
        ],
        SeenSentData { seen: 2, sent: 1 };
        "one_error"
    )]
    #[test_case(
        vec![
            Err(NetworkManagerError::Add(fnp_socketproxy::NetworkRegistryAddError::MissingNetworkId)),
            Err(NetworkManagerError::Add(fnp_socketproxy::NetworkRegistryAddError::MissingNetworkInfo)),
        ],
        SeenSentData { seen: 2, sent: 0 };
        "all_errors"
    )]
    #[::fuchsia::test(threads = 2)]
    async fn test_add_network_with_proxy(
        results: Vec<Result<(), NetworkManagerError>>,
        expected_data: SeenSentData,
    ) {
        let inspector = fuchsia_inspect::Inspector::default();
        let manager = &setup_proxy(inspector.root(), results, None).0;

        // `add_network` returns Ok(()) as long as the network
        // addition is applied locally, regardless of if the call
        // is sent to the socketproxy successfully.
        let network1 = test_network_message_from_id(1);
        assert_matches!(manager.add_network(network1.clone()), Ok(()));

        let network2 = test_network_message_from_id(2);
        assert_matches!(manager.add_network(network2.clone()), Ok(()));

        assert_data_tree!(inspector, root: {
            nmfs: contains {
                default_ids_set: {
                    seen: 0u64,
                    sent: 0u64,
                },
                added_networks: {
                    seen: expected_data.seen,
                    sent: expected_data.sent,
                },
                updated_networks: {
                    seen: 0u64,
                    sent: 0u64,
                },
                removed_networks: {
                    seen: 0u64,
                    sent: 0u64,
                },
            },
        });
    }

    #[test_case(vec![Ok(()), Ok(())], SeenSentData { seen: 2, sent: 2 }; "all_success")]
    #[test_case(
        vec![
            Ok(()),
            Err(NetworkManagerError::Update(fnp_socketproxy::NetworkRegistryUpdateError::MissingNetworkId)),
        ],
        SeenSentData { seen: 2, sent: 1 };
        "one_error"
    )]
    #[test_case(
        vec![
            Err(NetworkManagerError::Update(fnp_socketproxy::NetworkRegistryUpdateError::NotFound)),
            Err(NetworkManagerError::Update(fnp_socketproxy::NetworkRegistryUpdateError::MissingNetworkInfo)),
        ],
        SeenSentData { seen: 2, sent: 0 };
        "all_errors"
    )]
    #[::fuchsia::test(threads = 2)]
    async fn test_update_network_with_proxy(
        results: Vec<Result<(), NetworkManagerError>>,
        expected_data: SeenSentData,
    ) {
        let inspector = fuchsia_inspect::Inspector::default();
        let manager = &setup_proxy(inspector.root(), results, None).0;

        let network_id = 1;
        let network = test_network_message_from_id(network_id);

        // Insert the network manually and then update the network.
        {
            let mut inner_guard = manager.lock();
            let _ = inner_guard.as_mut().networks.insert(network_id, Some(network.clone()));
        }

        // `update_network` returns Ok(()) as long as the network
        // update is applied locally, regardless of if the call
        // is sent to the socketproxy successfully. Use the same
        // network information -- another test verifies that the
        // change is applied successfully.
        assert_matches!(manager.update_network(network.clone()), Ok(()));
        assert_matches!(manager.update_network(network), Ok(()));

        assert_data_tree!(inspector, root: {
            nmfs: contains {
                default_ids_set: {
                    seen: 0u64,
                    sent: 0u64,
                },
                added_networks: {
                    seen: 0u64,
                    sent: 0u64,
                },
                updated_networks: {
                    seen: expected_data.seen,
                    sent: expected_data.sent,
                },
                removed_networks: {
                    seen: 0u64,
                    sent: 0u64,
                },
            },
        });
    }

    #[test_case(vec![Ok(())], SeenSentData { seen: 1, sent: 1 }; "success")]
    #[test_case(
        vec![
            Err(NetworkManagerError::Remove(fnp_socketproxy::NetworkRegistryRemoveError::NotFound)),
        ],
        SeenSentData { seen: 1, sent: 0 };
        "error"
    )]
    #[::fuchsia::test(threads = 2)]
    async fn test_remove_network_with_proxy(
        results: Vec<Result<(), NetworkManagerError>>,
        expected_data: SeenSentData,
    ) {
        let inspector = fuchsia_inspect::Inspector::default();
        let manager = &setup_proxy(inspector.root(), results, None).0;

        let network_id = 1;
        let network = test_network_message_from_id(network_id);

        // Insert the network manually and then remove it.
        {
            let mut inner_guard = manager.lock();
            let _ = inner_guard.as_mut().networks.insert(network_id, Some(network.clone()));
        }

        // `remove_network` returns Ok(()) as long as the network
        // removal is applied locally, regardless of if the call
        // is sent to the socketproxy successfully.
        assert_matches!(manager.remove_network(network_id), Ok(()));

        assert_data_tree!(inspector, root: {
            nmfs: contains {
                default_ids_set: {
                    seen: 0u64,
                    sent: 0u64,
                },
                added_networks: {
                    seen: 0u64,
                    sent: 0u64,
                },
                updated_networks: {
                    seen: 0u64,
                    sent: 0u64,
                },
                removed_networks: {
                    seen: expected_data.seen,
                    sent: expected_data.sent,
                },
            },
        });
    }

    #[::fuchsia::test(threads = 2)]
    async fn test_multiple_operations_with_proxy() {
        let inspector = fuchsia_inspect::Inspector::default();
        let results = vec![
            // Network added to Manager, but not added to socketproxy.
            Err(NetworkManagerError::Add(
                fnp_socketproxy::NetworkRegistryAddError::MissingNetworkId,
            )),
            // Network added to Manager and to socketproxy.
            Ok(()),
            // Network set as default in Manager, but not in socketproxy.
            Err(NetworkManagerError::SetDefault(
                fnp_socketproxy::NetworkRegistrySetDefaultError::NotFound,
            )),
            // Network set as default in Manager and in socketproxy.
            Ok(()),
            // Network updated in Manager, but not in socketproxy.
            Err(NetworkManagerError::Update(fnp_socketproxy::NetworkRegistryUpdateError::NotFound)),
            // Network updated in Manager and in socketproxy.
            Ok(()),
            // Unset the default network so the network is eligible to be removed.
            Ok(()),
            // Network removed from Manager, but not from socketproxy.
            Err(NetworkManagerError::Remove(fnp_socketproxy::NetworkRegistryRemoveError::NotFound)),
            // Network removed from Manager and from socketproxy.
            Ok(()),
        ];
        let manager = &setup_proxy(inspector.root(), results, None).0;

        // Add a network that doesn't get sent to the socketproxy.
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

        // Add a network that gets sent to the socketproxy.
        let network2 = test_network_message_from_id(2);
        assert_matches!(manager.add_network(network2.clone()), Ok(()));
        assert_data_tree!(inspector, root: {
            nmfs: contains {
                added_networks: {
                    seen: 2u64,
                    sent: 1u64,
                },
            },
        });

        // Set the default network that isn't known to the socketproxy.
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

        // Set the default network that is known to the socketproxy.
        manager.set_default_network_id(Some(2));
        assert_eq!(manager.get_default_network_id(), Some(2));
        assert_data_tree!(inspector, root: {
            nmfs: contains {
                default_ids_set: {
                    seen: 2u64,
                    sent: 1u64,
                },
            },
        });

        // Update a network not known to the socketproxy.
        let mut network1_updated = network1.clone();
        network1_updated.mark = 1;
        assert_matches!(manager.update_network(network1_updated.clone()), Ok(()));
        assert_data_tree!(inspector, root: {
            nmfs: contains {
                updated_networks: {
                    seen: 1u64,
                    sent: 0u64,
                },
            },
        });

        // Update a network that is known to the socketproxy.
        let mut network2_updated = network2.clone();
        network2_updated.mark = 2;
        assert_matches!(manager.update_network(network2_updated.clone()), Ok(()));
        assert_data_tree!(inspector, root: {
            nmfs: contains {
                updated_networks: {
                    seen: 2u64,
                    sent: 1u64,
                },
            },
        });

        // The default network must be unset first to remove this network.
        manager.set_default_network_id(None);
        assert_data_tree!(inspector, root: {
            nmfs: contains {
                default_ids_set: {
                    seen: 3u64,
                    sent: 2u64,
                },
            },
        });

        // Remove a network that doesn't get sent to the socketproxy.
        assert_matches!(manager.remove_network(1), Ok(()));
        assert_data_tree!(inspector, root: {
            nmfs: contains {
                removed_networks: {
                    seen: 1u64,
                    sent: 0u64,
                },
            },
        });

        // Remove a network that gets sent to the socketproxy.
        assert_matches!(manager.remove_network(2), Ok(()));
        assert_data_tree!(inspector, root: {
            nmfs: contains {
                default_ids_set: {
                    seen: 3u64,
                    sent: 2u64,
                },
                added_networks: {
                    seen: 2u64,
                    sent: 1u64,
                },
                updated_networks: {
                    seen: 2u64,
                    sent: 1u64,
                },
                removed_networks: {
                    seen: 2u64,
                    sent: 1u64,
                },
            },
        });
    }

    // Assumes that all networks get added successfully to the socketproxy. The
    // parameter toggles whether the default id gets successfully set
    // in the socketproxy.
    #[test_case(Ok(()), SeenSentData { seen: 0, sent: 1 }; "success")]
    #[test_case(
        Err(NetworkManagerError::SetDefault(fnp_socketproxy::NetworkRegistrySetDefaultError::NotFound)),
        SeenSentData { seen: 0, sent: 0 };
        "failed_setting_default"
    )]
    #[::fuchsia::test(threads = 2)]
    async fn replay_network_events_with_proxy(
        last_event_result: Result<(), NetworkManagerError>,
        default_id_data: SeenSentData,
    ) {
        let inspector = fuchsia_inspect::Inspector::default();
        let results = vec![Ok(()), Ok(()), last_event_result.clone()];
        let manager = &setup_proxy(inspector.root(), results, None).0;

        // If there aren't any networks in the Manager, replay should succeed.
        // This should trigger an increment to the `no_events_to_replay` node.
        assert_matches!(manager.replay_network_events(), Ok(0 /* number of updates sent */));

        // Manually insert the networks to simulate these networks
        // already existing in the Manager.
        {
            let mut inner = manager.lock();
            let mut inner = inner.as_mut();
            let network_id1 = 1;
            let _ =
                inner.networks.insert(network_id1, Some(test_network_message_from_id(network_id1)));
            let network_id2 = 2;
            let _ =
                inner.networks.insert(network_id2, Some(test_network_message_from_id(network_id2)));
            let _ = inner.default_id = Some(network_id2);
        }

        let replay_result = manager.replay_network_events();
        match last_event_result {
            Ok(_) => assert_matches!(replay_result, Ok(3 /* number of updates sent */)),
            Err(_) => assert_matches!(
                replay_result,
                Err(NetworkManagerError::SetDefault(
                    fnp_socketproxy::NetworkRegistrySetDefaultError::NotFound
                ))
            ),
        }
        // `added_networks.seen` remains 0, as no new networks are added
        // through the Manager, they are only sent to the socketproxy.
        assert_data_tree!(inspector, root: {
            nmfs: contains {
                added_networks: {
                    seen: 0u64,
                    sent: 2u64,
                },
                default_ids_set: {
                    seen: default_id_data.seen,
                    sent: default_id_data.sent,
                },
            },
        });
    }

    fn spawn_reconnection_thread_for_test(
        handle: NetworkManagerHandle,
        initiate_reconnect_receiver: mpsc::Receiver<()>,
    ) {
        fuchsia_async::Task::spawn(async move {
            // Channel to handle requests to re-connect the socketproxy proxy.
            // The oneshot response will permit the reconnection loop to `replay_network_events`.
            let (sender, mut receiver) = mpsc::channel::<oneshot::Sender<bool>>(1);

            let mut reconnect_fut = std::pin::pin!(reconnect_to_proxy_loop(
                handle.clone(),
                sender,
                initiate_reconnect_receiver
            )
            .fuse());
            loop {
                futures::select! {
                    _ = reconnect_fut => unreachable!("Reconnect thread should never complete"),
                    oneshot_sender = receiver.select_next_some() => {
                        // Respond `true` as if the proxy was replaced.
                        oneshot_sender.send(true).expect("Receiver end was dropped");
                    }
                }
            }
        })
        .detach();
    }

    // This test requires 3 threads. One for the main thread, one for mocking
    // the proxy responses, and one for handling the socketproxy reconnection.
    #[::fuchsia::test(threads = 3)]
    async fn replay_network_events_with_reconnect_thread() {
        let inspector = fuchsia_inspect::Inspector::default();
        let results: Vec<Result<(), NetworkManagerError>> = vec![
            // Network added to Manager and socketproxy.
            Ok(()),
            // Network set as default in Manager, but socketproxy responds
            // with an error, triggering a reconnect to reset the state.
            Err(NetworkManagerError::SetDefault(
                fnp_socketproxy::NetworkRegistrySetDefaultError::NotFound,
            )),
            // Network added to socketproxy via network replay.
            Ok(()),
            // Default network set in socketproxy via network replay.
            Ok(()),
        ];
        let (initiate_reconnect_sender, initiate_reconnect_receiver) = mpsc::channel(1);
        let manager = setup_proxy(inspector.root(), results, Some(initiate_reconnect_sender));
        spawn_reconnection_thread_for_test(manager.clone(), initiate_reconnect_receiver);
        let manager = &manager.0;

        // Add a network.
        let network_id = 1;
        let network = test_network_message_from_id(network_id);
        assert_matches!(manager.add_network(network.clone()), Ok(()));
        assert_data_tree!(inspector, root: {
            nmfs: contains {
                added_networks: {
                    seen: 1u64,
                    sent: 1u64,
                },
            },
        });

        // Set the default network and act like it isn't known to the socketproxy
        // because of the `NetworkRegistrySetDefaultError::NotFound`. This will
        // initiate a reconnect to the proxy and the state to be reset.
        manager.set_default_network_id(Some(network_id));

        // During this period `replay_network_events()` should be called from
        // another thread and the inspect values should be updated.
        // Use `TreeAssertion` instead of a macro to query the inspect tree in
        // a loop without triggering an error and failing the test.
        let assertion = tree_assertion!(root: {
            nmfs: contains {
                attempted_reconnects: 1u64,
                successful_reconnects: 1u64,
                no_events_to_replay: 0u64,
                added_networks: {
                    seen: 1u64,
                    sent: 2u64,
                },
                default_ids_set: {
                    seen: 1u64,
                    sent: 1u64,
                },
            },
        });

        while assertion.run(&inspector.get_diagnostics_hierarchy()).is_err() {
            // Loop until the other thread updates the inspect values
        }
    }

    // This test requires 3 threads. One for the main thread, one for mocking
    // the proxy responses, and one for handling the socketproxy reconnection.
    // Similar to `replay_network_events_with_reconnect_thread`, but
    // specifically tests the case where reconnection was attempted and no
    // networks are present to communicate to the socketproxy.
    #[::fuchsia::test(threads = 3)]
    async fn replay_network_events_with_reconnect_thread_no_events_to_replay() {
        let inspector = fuchsia_inspect::Inspector::default();
        let results: Vec<Result<(), NetworkManagerError>> = vec![
            // Network added to Manager and socketproxy.
            Ok(()),
            // Network set as default in Manager, but socketproxy responds
            // with an error, triggering a reconnect to reset the state.
            Err(NetworkManagerError::Remove(fnp_socketproxy::NetworkRegistryRemoveError::NotFound)),
        ];
        let (initiate_reconnect_sender, initiate_reconnect_receiver) = mpsc::channel(1);
        let manager = setup_proxy(inspector.root(), results, Some(initiate_reconnect_sender));
        spawn_reconnection_thread_for_test(manager.clone(), initiate_reconnect_receiver);
        let manager = &manager.0;

        // Add a network.
        let network_id = 1;
        let network = test_network_message_from_id(network_id);
        assert_matches!(manager.add_network(network.clone()), Ok(()));
        assert_data_tree!(inspector, root: {
            nmfs: contains {
                added_networks: {
                    seen: 1u64,
                    sent: 1u64,
                },
            },
        });

        // Remove the network and act like it isn't known to the socketproxy.
        // This will initiate a reconnect to the proxy and the state to be reset.
        let _removed_network = manager.remove_network(network_id);

        // During this period `replay_network_events()` should be called from
        // another thread and the inspect values should be updated.
        // Use `TreeAssertion` instead of a macro to query the inspect tree in
        // a loop without triggering an error and failing the test.
        let assertion = tree_assertion!(root: {
            nmfs: contains {
                no_events_to_replay: 1u64,
                added_networks: {
                    seen: 1u64,
                    sent: 1u64,
                },
                removed_networks: {
                    seen: 1u64,
                    sent: 0u64,
                },
            },
        });

        while assertion.run(&inspector.get_diagnostics_hierarchy()).is_err() {
            // Loop until the other thread updates the inspect values
        }
    }
}
