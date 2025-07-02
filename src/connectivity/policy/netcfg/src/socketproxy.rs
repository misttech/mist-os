// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    fidl_fuchsia_net_interfaces_ext as fnet_interfaces_ext,
    fidl_fuchsia_net_policy_socketproxy as fnp_socketproxy,
    fidl_fuchsia_posix_socket as fposix_socket,
};

use socket_proxy::{NetworkConversionError, NetworkExt, NetworkRegistryError};
use std::collections::hash_map::Entry;
use std::collections::HashMap;
use thiserror::Error;
use todo_unused::todo_unused;

use crate::InterfaceId;

#[derive(Debug)]
pub struct SocketProxyState {
    fuchsia_networks: fnp_socketproxy::FuchsiaNetworksProxy,
    default_id: Option<InterfaceId>,
    networks: HashMap<InterfaceId, fnp_socketproxy::Network>,
}

// Fuchsia Networks should only be added to the socketproxy when the link has a default v4 and/or
// v6 route. The functions that propagate calls to the socketproxy prioritize maintaining a correct
// version of local state and logging an error if the socketproxy state is not aligned.
impl SocketProxyState {
    #[todo_unused("https://fxbug.dev/385368910")]
    pub fn new(fuchsia_networks: fnp_socketproxy::FuchsiaNetworksProxy) -> Self {
        Self { fuchsia_networks, default_id: None, networks: HashMap::new() }
    }

    #[todo_unused("https://fxbug.dev/385368910")]
    /// Set or unset an existing Fuchsia network as the default in the socket proxy registry.
    ///
    /// # Errors
    /// The network does not exist
    pub(crate) async fn handle_default_network(
        &mut self,
        network_id: Option<InterfaceId>,
    ) -> Result<(), SocketProxyError> {
        // Default id is already the same, no reason to change the default network.
        if self.default_id == network_id {
            return Ok(());
        }

        let socketproxy_network_id = match network_id {
            Some(id) => {
                if !self.networks.contains_key(&id) {
                    return Err(SocketProxyError::SetDefaultNonexistentNetwork(id));
                }

                // We expect interface ids to safely fit in the range of u32 values.
                let id_u32: u32 = match id.get().try_into() {
                    Err(_) => {
                        return Err(SocketProxyError::InvalidInterfaceId(id));
                    }
                    Ok(id) => id,
                };

                fposix_socket::OptionalUint32::Value(id_u32)
            }
            None => fposix_socket::OptionalUint32::Unset(fposix_socket::Empty),
        };

        self.default_id = network_id;

        Ok(self.fuchsia_networks.set_default(&socketproxy_network_id).await??)
    }

    #[todo_unused("https://fxbug.dev/385368910")]
    /// Handle the removal of the current default network. When other Fuchsia networks exist,
    /// fallback to one of them instead, prioritizing the network with the lowest id. If no other
    /// networks exist, the Fuchsia default network will be set to None.
    pub(crate) async fn handle_default_network_removal(
        &mut self,
    ) -> Result<Option<InterfaceId>, SocketProxyError> {
        if let None = self.default_id {
            // Do nothing. There is no default network.
            return Ok(None);
        }
        let mut interface_ids = self
            .networks
            .keys()
            .filter(|network| Some(network.get()) != self.default_id.map(|id| id.get()))
            .cloned()
            .peekable();
        if interface_ids.peek().is_none() {
            // No need to fallback to another Fuchsia network if one doesn't
            // exist. Simply set the default to None.
            self.handle_default_network(None).await.map(|_| None)
        } else {
            // Fallback to the network with the lowest id.
            let new_id = interface_ids.into_iter().min();
            self.handle_default_network(new_id).await.map(|_| new_id)
        }
    }

    #[todo_unused("https://fxbug.dev/385368910")]
    /// Add a new Fuchsia network to the socket proxy registry.
    ///
    /// # Errors
    /// The network already exists
    pub(crate) async fn handle_add_network(
        &mut self,
        properties: &fnet_interfaces_ext::Properties<fnet_interfaces_ext::DefaultInterest>,
    ) -> Result<(), SocketProxyError> {
        // TODO(https://fxrev.dev/385368910): Include DNS servers
        let network = fnp_socketproxy::Network::from_watcher_properties(properties)?;
        match self.networks.entry(InterfaceId(properties.id)) {
            Entry::Vacant(entry) => {
                let _ = entry.insert(network.clone());
            }
            Entry::Occupied(_entry) => {
                return Err(SocketProxyError::AddedExistingNetwork(network));
            }
        }

        Ok(self.fuchsia_networks.add(&network).await??)
    }

    #[todo_unused("https://fxbug.dev/385368910")]
    /// Remove an existing Fuchsia network in the socket proxy registry.
    ///
    /// # Errors
    /// The network does not exist or is the current default network
    pub(crate) async fn handle_remove_network(
        &mut self,
        network_id: InterfaceId,
    ) -> Result<(), SocketProxyError> {
        if !self.networks.contains_key(&network_id) {
            return Err(SocketProxyError::RemovedNonexistentNetwork(network_id));
        } else if self.default_id.map(|id| id == network_id).unwrap_or(false) {
            return Err(SocketProxyError::RemovedDefaultNetwork(network_id));
        }

        // We expect interface ids to safely fit in the range of u32 values.
        let id_u32: u32 = match network_id.get().try_into() {
            Err(_) => {
                return Err(SocketProxyError::InvalidInterfaceId(network_id));
            }
            Ok(id) => id,
        };
        let _ = self.networks.remove(&network_id);

        Ok(self.fuchsia_networks.remove(id_u32).await??)
    }
}

#[todo_unused("https://fxbug.dev/385368910")]
// Errors produced when maintaining registry state or communicating
// updates to the socket proxy.
#[derive(Clone, Debug, Error)]
pub enum SocketProxyError {
    #[error("Error adding network that already exists: {0:?}")]
    AddedExistingNetwork(fnp_socketproxy::Network),
    #[error("Error converting the watcher properties to a network: {0}")]
    ConversionError(#[from] NetworkConversionError),
    #[error("Error calling FIDL on socketproxy: {0:?}")]
    Fidl(#[from] fidl::Error),
    #[error("Error converting id to socketproxy network: {0}")]
    InvalidInterfaceId(InterfaceId),
    #[error("Network Registry error: {0:?}")]
    NetworkRegistry(#[from] NetworkRegistryError),
    #[error("Error removing a current default network with id: {0}")]
    RemovedDefaultNetwork(InterfaceId),
    #[error("Error removing network that does not exist with id: {0}")]
    RemovedNonexistentNetwork(InterfaceId),
    #[error("Error setting default network that does not exist with id: {0}")]
    SetDefaultNonexistentNetwork(InterfaceId),
}

impl From<fnp_socketproxy::NetworkRegistryAddError> for SocketProxyError {
    fn from(error: fnp_socketproxy::NetworkRegistryAddError) -> Self {
        SocketProxyError::NetworkRegistry(NetworkRegistryError::Add(error))
    }
}

impl From<fnp_socketproxy::NetworkRegistryRemoveError> for SocketProxyError {
    fn from(error: fnp_socketproxy::NetworkRegistryRemoveError) -> Self {
        SocketProxyError::NetworkRegistry(NetworkRegistryError::Remove(error))
    }
}

impl From<fnp_socketproxy::NetworkRegistrySetDefaultError> for SocketProxyError {
    fn from(error: fnp_socketproxy::NetworkRegistrySetDefaultError) -> Self {
        SocketProxyError::NetworkRegistry(NetworkRegistryError::SetDefault(error))
    }
}

#[cfg(test)]
mod tests {
    use socket_proxy_testing::respond_to_socketproxy;
    use std::num::NonZeroU64;

    use super::*;

    fn interface_properties_from_id(
        id: u64,
    ) -> fnet_interfaces_ext::Properties<fnet_interfaces_ext::DefaultInterest> {
        fnet_interfaces_ext::Properties {
            id: NonZeroU64::new(id).expect("this is a valid u64"),
            online: true,
            name: String::from("network"),
            has_default_ipv4_route: true,
            has_default_ipv6_route: true,
            addresses: vec![],
            port_class: fnet_interfaces_ext::PortClass::Ethernet,
        }
    }

    #[fuchsia::test]
    async fn test_set_default_network_id() -> Result<(), SocketProxyError> {
        let (fuchsia_networks, mut fuchsia_networks_req_stream) =
            fidl::endpoints::create_proxy_and_stream::<fnp_socketproxy::FuchsiaNetworksMarker>();
        let mut state = SocketProxyState::new(fuchsia_networks);
        const NETWORK_ID_U64: u64 = 1u64;
        const NETWORK_ID: InterfaceId = InterfaceId(NonZeroU64::new(NETWORK_ID_U64).unwrap());

        // Attempt to set a network as default when it isn't known
        // to the SocketProxyState.
        assert_matches::assert_matches!(
            state.handle_default_network(Some(NETWORK_ID)).await,
            Err(SocketProxyError::SetDefaultNonexistentNetwork(id))
            if id.get() == NETWORK_ID_U64
        );

        // Add a network without 'officially' adding it so we can
        // test `handle_default_network` in isolation.
        assert_matches::assert_matches!(
            state.networks.insert(
                NETWORK_ID,
                fnp_socketproxy::Network::from_watcher_properties(&interface_properties_from_id(
                    NETWORK_ID_U64
                ))?,
            ),
            None
        );

        // Set the default network as an existing network.
        let (set_default_network_result, ()) = futures::join!(
            state.handle_default_network(Some(NETWORK_ID)),
            respond_to_socketproxy(&mut fuchsia_networks_req_stream, Ok(()))
        );
        assert_matches::assert_matches!(set_default_network_result, Ok(()));

        // Unset the default network.
        let (set_default_network_result2, ()) = futures::join!(
            state.handle_default_network(None),
            respond_to_socketproxy(&mut fuchsia_networks_req_stream, Ok(()))
        );
        assert_matches::assert_matches!(set_default_network_result2, Ok(()));

        Ok(())
    }

    #[fuchsia::test]
    async fn test_add_network() {
        let (fuchsia_networks, mut fuchsia_networks_req_stream) =
            fidl::endpoints::create_proxy_and_stream::<fnp_socketproxy::FuchsiaNetworksMarker>();
        let mut state = SocketProxyState::new(fuchsia_networks);
        const NETWORK_ID1_U32: u32 = 1u32;
        const NETWORK_ID2_U64: u64 = 2u64;

        let network1 = interface_properties_from_id(NETWORK_ID1_U32.into());
        let (add_network_result, ()) = futures::join!(
            state.handle_add_network(&network1),
            respond_to_socketproxy(&mut fuchsia_networks_req_stream, Ok(()))
        );
        assert_matches::assert_matches!(add_network_result, Ok(()));

        // Ensure we cannot add a network with the same id twice.
        assert_matches::assert_matches!(
            state.handle_add_network(&network1).await,
            Err(SocketProxyError::AddedExistingNetwork(fnp_socketproxy::Network {
                network_id: Some(NETWORK_ID1_U32),
                ..
            }))
        );

        // Ensure we can add a network with a different id.
        let network2 = interface_properties_from_id(NETWORK_ID2_U64);
        let (add_network_result2, ()) = futures::join!(
            state.handle_add_network(&network2),
            respond_to_socketproxy(&mut fuchsia_networks_req_stream, Ok(()))
        );
        assert_matches::assert_matches!(add_network_result2, Ok(()));
    }

    #[fuchsia::test]
    async fn test_remove_network() -> Result<(), SocketProxyError> {
        let (fuchsia_networks, mut fuchsia_networks_req_stream) =
            fidl::endpoints::create_proxy_and_stream::<fnp_socketproxy::FuchsiaNetworksMarker>();
        let mut state = SocketProxyState::new(fuchsia_networks);
        const NETWORK_ID_U64: u64 = 1;
        const NETWORK_ID: InterfaceId = InterfaceId(NonZeroU64::new(NETWORK_ID_U64).unwrap());

        // Attempt to remove a network when it isn't known
        // to the SocketProxyState.
        assert_matches::assert_matches!(
            state.handle_remove_network(NETWORK_ID).await,
            Err(SocketProxyError::RemovedNonexistentNetwork(id))
            if id.get() == NETWORK_ID_U64
        );

        // Add a network and make it default without 'officially' adding or
        // setting it so we can test `handle_remove_network` in isolation.
        assert_matches::assert_matches!(
            state.networks.insert(
                NETWORK_ID,
                fnp_socketproxy::Network::from_watcher_properties(&interface_properties_from_id(
                    NETWORK_ID_U64
                ),)?,
            ),
            None
        );
        state.default_id = Some(NETWORK_ID);

        // Attempt to remove the network although it is the default network.
        assert_matches::assert_matches!(
            state.handle_remove_network(NETWORK_ID).await,
            Err(SocketProxyError::RemovedDefaultNetwork(id))
            if id.get() == NETWORK_ID_U64
        );

        // Directly unset the default network so we can test
        // `handle_remove_network` in isolation.
        state.default_id = None;

        // Attempt to remove the network. This should succeed since it is
        // not the current default network.
        let (remove_network_result, ()) = futures::join!(
            state.handle_remove_network(NETWORK_ID),
            respond_to_socketproxy(&mut fuchsia_networks_req_stream, Ok(()))
        );
        assert_matches::assert_matches!(remove_network_result, Ok(()));
        assert_matches::assert_matches!(state.networks.get(&NETWORK_ID), None);

        Ok(())
    }

    #[fuchsia::test]
    async fn test_default_network_removal() -> Result<(), SocketProxyError> {
        let (fuchsia_networks, mut fuchsia_networks_req_stream) =
            fidl::endpoints::create_proxy_and_stream::<fnp_socketproxy::FuchsiaNetworksMarker>();
        let mut state = SocketProxyState::new(fuchsia_networks);
        const NETWORK_ID1_U64: u64 = 1;
        const NETWORK_ID1: InterfaceId = InterfaceId(NonZeroU64::new(NETWORK_ID1_U64).unwrap());
        const NETWORK_ID2_U64: u64 = 2;
        const NETWORK_ID2: InterfaceId = InterfaceId(NonZeroU64::new(NETWORK_ID2_U64).unwrap());

        // Attempting to remove the default network when one does not exist
        // should result in Ok(None).
        assert_matches::assert_matches!(state.handle_default_network_removal().await, Ok(None));

        // Add two networks and set one as the default without 'officially'
        // adding or setting it so we can test `handle_default_network_removal`
        // in isolation.
        assert_matches::assert_matches!(
            state.networks.insert(
                NETWORK_ID1,
                fnp_socketproxy::Network::from_watcher_properties(&interface_properties_from_id(
                    NETWORK_ID1_U64
                ),)?,
            ),
            None
        );
        assert_matches::assert_matches!(
            state.networks.insert(
                NETWORK_ID2,
                fnp_socketproxy::Network::from_watcher_properties(&interface_properties_from_id(
                    NETWORK_ID2_U64
                ),)?,
            ),
            None
        );
        state.default_id = Some(NETWORK_ID1);

        // Remove the default network. Since NETWORK_ID1 is the current default
        // then NETWORK_ID2 should be the chosen fallback.
        let (default_network_removal_result, ()) = futures::join!(
            state.handle_default_network_removal(),
            respond_to_socketproxy(&mut fuchsia_networks_req_stream, Ok(()))
        );
        assert_matches::assert_matches!(default_network_removal_result, Ok(Some(NETWORK_ID2)));
        assert_matches::assert_matches!(state.default_id, Some(NETWORK_ID2));

        // `handle_default_network_removal` does not remove the network itself,
        // it performs the logic to fallback to another default network or
        // unset it if one does not exist. Remove NETWORK_ID1 manually.
        assert_matches::assert_matches!(
            state.networks.remove(&NETWORK_ID1),
            Some(network) if network.network_id.unwrap() == NETWORK_ID1_U64 as u32
        );

        // Remove the default network. Since there is no other network, then
        // the default network should be unset.
        let (default_network_removal_result2, ()) = futures::join!(
            state.handle_default_network_removal(),
            respond_to_socketproxy(&mut fuchsia_networks_req_stream, Ok(()))
        );
        assert_matches::assert_matches!(default_network_removal_result2, Ok(None));
        assert_matches::assert_matches!(state.default_id, None);

        Ok(())
    }

    #[fuchsia::test]
    async fn test_multiple_operations() {
        let (fuchsia_networks, mut fuchsia_networks_req_stream) =
            fidl::endpoints::create_proxy_and_stream::<fnp_socketproxy::FuchsiaNetworksMarker>();
        let mut state = SocketProxyState::new(fuchsia_networks);
        const NETWORK_ID1_U64: u64 = 1;
        const NETWORK_ID1: InterfaceId = InterfaceId(NonZeroU64::new(NETWORK_ID1_U64).unwrap());
        const NETWORK_ID2_U64: u64 = 2;
        const NETWORK_ID2: InterfaceId = InterfaceId(NonZeroU64::new(NETWORK_ID2_U64).unwrap());

        // Add a network.
        let network1 = interface_properties_from_id(NETWORK_ID1_U64);
        let (add_network_result, ()) = futures::join!(
            state.handle_add_network(&network1),
            respond_to_socketproxy(&mut fuchsia_networks_req_stream, Ok(()))
        );
        assert_matches::assert_matches!(add_network_result, Ok(()));

        // Set the default network.
        let (set_default_network_result, ()) = futures::join!(
            state.handle_default_network(Some(NETWORK_ID1)),
            respond_to_socketproxy(&mut fuchsia_networks_req_stream, Ok(()))
        );
        assert_matches::assert_matches!(set_default_network_result, Ok(()));

        // Add another network.
        let network2 = interface_properties_from_id(NETWORK_ID2_U64);
        let (add_network_result, ()) = futures::join!(
            state.handle_add_network(&network2),
            respond_to_socketproxy(&mut fuchsia_networks_req_stream, Ok(()))
        );
        assert_matches::assert_matches!(add_network_result, Ok(()));

        // Attempt to remove the first network. We should get an error
        // since network1 is the current default network.
        assert_matches::assert_matches!(
            state.handle_remove_network(NETWORK_ID1).await,
            Err(SocketProxyError::RemovedDefaultNetwork(id))
            if id.get() == NETWORK_ID1_U64
        );

        // Ensure the first network still exists.
        assert!(state.networks.get(&NETWORK_ID1).is_some());

        // Set the default network to the second network.
        let (default_network_removal_result, ()) = futures::join!(
            state.handle_default_network_removal(),
            respond_to_socketproxy(&mut fuchsia_networks_req_stream, Ok(()))
        );
        assert_matches::assert_matches!(default_network_removal_result, Ok(Some(NETWORK_ID2)));

        // Remove the first network as it is no longer the default network.
        assert_matches::assert_matches!(
            state.networks.remove(&NETWORK_ID1),
            Some(network) if network.network_id.unwrap() == NETWORK_ID1_U64 as u32
        );
    }
}
