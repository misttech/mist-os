// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! A test that verifies the client interface, whose provisioning is delegated,
//! has been assigned a DHCP address.

#![cfg(test)]

use std::collections::HashMap;

use tracing::info;
use {
    delegated_provisioning_constants as constants, fidl_fuchsia_net as fnet,
    fidl_fuchsia_net_interfaces as fnet_interfaces,
    fidl_fuchsia_net_interfaces_ext as fnet_interfaces_ext,
};

#[fuchsia::test]
async fn delegated_provisioning_test() {
    info!("test starting...");

    let state_proxy =
        fuchsia_component::client::connect_to_protocol::<fnet_interfaces::StateMarker>()
            .expect("Failed to connect to State proxy");
    let event_stream = fnet_interfaces_ext::event_stream_from_state::<
        fnet_interfaces_ext::DefaultInterest,
    >(&state_proxy, fnet_interfaces_ext::IncludedAddresses::OnlyAssigned)
    .expect("failed to get interface event stream");

    info!(
        "waiting for interface ({}) to be assigned address ({:?})...",
        constants::CLIENT_IFACE_NAME,
        constants::DHCP_DYNAMIC_IP
    );

    let mut interfaces = HashMap::<u64, fnet_interfaces_ext::PropertiesAndState<(), _>>::new();
    fnet_interfaces_ext::wait_interface(event_stream, &mut interfaces, |interfaces| {
        info!("Observed change on interfaces watcher. Current State: {:?}", interfaces);
        interfaces
            .values()
            .any(
                |fnet_interfaces_ext::PropertiesAndState {
                     properties: fnet_interfaces_ext::Properties { name, addresses, .. },
                     state: _,
                 }| {
                    name == constants::CLIENT_IFACE_NAME
                        && addresses.iter().any(
                            |&fnet_interfaces_ext::Address {
                                 addr: fnet::Subnet { addr, prefix_len: _ },
                                 valid_until: _,
                                 assignment_state: _,
                                 preferred_lifetime_info: _,
                             }| match addr {
                                fnet::IpAddress::Ipv4(addr) => addr == constants::DHCP_DYNAMIC_IP,
                                fnet::IpAddress::Ipv6(_) => false,
                            },
                        )
                },
            )
            .then_some(())
    })
    .await
    .expect("interface event stream unexpectedly closed");

    info!(
        "observed interface ({}) with address ({:?})",
        constants::CLIENT_IFACE_NAME,
        constants::DHCP_DYNAMIC_IP
    );
}
