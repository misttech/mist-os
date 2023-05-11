// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Utilities for interacting with devices during integration tests.

use fidl_fuchsia_hardware_network as fhardware_network;
use fidl_fuchsia_net as fnet;
use fidl_fuchsia_net_interfaces_admin as fnet_interfaces_admin;
use fidl_fuchsia_net_tun as fnet_tun;

/// Create a Tun device, returning handles to the created
/// `fuchsia.net.tun/Device` and the underlying network device.
pub fn create_tun_device(
) -> (fnet_tun::DeviceProxy, fidl::endpoints::ClientEnd<fhardware_network::DeviceMarker>) {
    create_tun_device_with(fnet_tun::DeviceConfig::default())
}

/// Create a Tun device with the provided config and return its handles.
pub fn create_tun_device_with(
    device_config: fnet_tun::DeviceConfig,
) -> (fnet_tun::DeviceProxy, fidl::endpoints::ClientEnd<fhardware_network::DeviceMarker>) {
    let tun_ctl = fuchsia_component::client::connect_to_protocol::<fnet_tun::ControlMarker>()
        .expect("connect to protocol");
    let (tun_dev, tun_dev_server_end) =
        fidl::endpoints::create_proxy::<fnet_tun::DeviceMarker>().expect("create proxy");
    tun_ctl.create_device(&device_config, tun_dev_server_end).expect("create tun device");
    let (netdevice_client_end, netdevice_server_end) =
        fidl::endpoints::create_endpoints::<fhardware_network::DeviceMarker>();
    tun_dev.get_device(netdevice_server_end).expect("get device");
    (tun_dev, netdevice_client_end)
}

/// Install the given network device into the test realm's networking stack,
/// returning the created `fuchsia.net.interfaces.admin/DeviceControl` handle.
pub fn install_device(
    realm: &netemul::TestRealm<'_>,
    device: fidl::endpoints::ClientEnd<fhardware_network::DeviceMarker>,
) -> fnet_interfaces_admin::DeviceControlProxy {
    let (admin_device_control, server_end) =
        fidl::endpoints::create_proxy::<fnet_interfaces_admin::DeviceControlMarker>()
            .expect("create proxy");
    let installer = realm
        .connect_to_protocol::<fnet_interfaces_admin::InstallerMarker>()
        .expect("connect to protocol");
    installer.install_device(device, server_end).expect("install device");
    admin_device_control
}

/// Create a port on the given Tun device, returning handles to the created
/// `fuchsia.net.tun/Port` and the underlying network port.
pub async fn create_tun_port_with(
    tun_device: &fnet_tun::DeviceProxy,
    id: u8,
    frame_types: impl IntoIterator<Item = fhardware_network::FrameType>,
    mac: Option<fnet::MacAddress>,
) -> (fnet_tun::PortProxy, fhardware_network::PortProxy) {
    let (port, server_end) =
        fidl::endpoints::create_proxy::<fnet_tun::PortMarker>().expect("create proxy");
    let (rx_types, tx_types): (Vec<_>, Vec<_>) = frame_types
        .into_iter()
        .map(|frame_type| {
            (
                frame_type,
                fhardware_network::FrameTypeSupport {
                    type_: frame_type,
                    features: fhardware_network::FRAME_FEATURES_RAW,
                    supported_flags: fhardware_network::TxFlags::empty(),
                },
            )
        })
        .unzip();
    tun_device
        .add_port(
            &fnet_tun::DevicePortConfig {
                base: Some(fnet_tun::BasePortConfig {
                    id: Some(id),
                    rx_types: Some(rx_types),
                    tx_types: Some(tx_types),
                    mtu: Some(netemul::DEFAULT_MTU.into()),
                    ..Default::default()
                }),
                mac,
                ..Default::default()
            },
            server_end,
        )
        .expect("add port");

    let (network_port, server_end) =
        fidl::endpoints::create_proxy::<fhardware_network::PortMarker>().expect("create endpoints");
    port.get_port(server_end).expect("get port");

    (port, network_port)
}

/// Creates a port on the given Tun device that supports IPv4 and IPv6 frame
/// types.
pub async fn create_ip_tun_port(
    tun_device: &fnet_tun::DeviceProxy,
    id: u8,
) -> (fnet_tun::PortProxy, fhardware_network::PortProxy) {
    create_tun_port_with(
        tun_device,
        id,
        [fhardware_network::FrameType::Ipv4, fhardware_network::FrameType::Ipv6],
        None,
    )
    .await
}

/// Creates a port on the given Tun device that supports the Ethernet frame
/// type.
pub async fn create_eth_tun_port(
    tun_device: &fnet_tun::DeviceProxy,
    id: u8,
    mac: fnet::MacAddress,
) -> (fnet_tun::PortProxy, fhardware_network::PortProxy) {
    create_tun_port_with(tun_device, id, [fhardware_network::FrameType::Ethernet], Some(mac)).await
}
