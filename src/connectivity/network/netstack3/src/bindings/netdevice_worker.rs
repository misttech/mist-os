// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::{
    collections::HashMap,
    convert::{TryFrom as _, TryInto as _},
    sync::Arc,
};

use assert_matches::assert_matches;
use fidl_fuchsia_hardware_network as fhardware_network;
use fidl_fuchsia_net as fnet;
use fidl_fuchsia_net_interfaces as fnet_interfaces;

use futures::{lock::Mutex, FutureExt as _, TryStreamExt as _};
use net_types::ip::{Ip, Ipv4, Ipv6, Ipv6Addr, Subnet};
use netstack3_core::{
    context::RngContext as _,
    device::{
        ethernet, update_ipv4_configuration, update_ipv6_configuration, EthernetWeakDeviceId,
    },
    ip::{
        device::{
            slaac::{
                SlaacConfiguration, TemporarySlaacAddressConfiguration, STABLE_IID_SECRET_KEY_BYTES,
            },
            IpDeviceConfigurationUpdate, Ipv4DeviceConfigurationUpdate,
            Ipv6DeviceConfigurationUpdate,
        },
        types::RawMetric,
    },
};
use rand::Rng as _;

use crate::bindings::{
    devices, interfaces_admin, trace_duration, BindingId, BindingsNonSyncCtxImpl, Ctx, DeviceId,
    Ipv6DeviceConfiguration, Netstack, NonSyncContext, SyncCtx, DEFAULT_INTERFACE_METRIC,
};

#[derive(Clone)]
struct Inner {
    device: netdevice_client::Client,
    session: netdevice_client::Session,
    state: Arc<Mutex<netdevice_client::PortSlab<EthernetWeakDeviceId<BindingsNonSyncCtxImpl>>>>,
}

/// The worker that receives messages from the ethernet device, and passes them
/// on to the main event loop.
pub(crate) struct NetdeviceWorker {
    ctx: Ctx,
    task: netdevice_client::Task,
    inner: Inner,
}

#[derive(thiserror::Error, Debug)]
pub(crate) enum Error {
    #[error("failed to create system resources: {0}")]
    SystemResource(fidl::Error),
    #[error("client error: {0}")]
    Client(#[from] netdevice_client::Error),
    #[error("port {0:?} already installed")]
    AlreadyInstalled(netdevice_client::Port),
    #[error("failed to connect to port: {0}")]
    CantConnectToPort(fidl::Error),
    #[error("port closed")]
    PortClosed,
    #[error("invalid port info: {0}")]
    InvalidPortInfo(netdevice_client::client::PortInfoValidationError),
    #[error("unsupported configuration")]
    ConfigurationNotSupported,
    #[error("mac {mac} on port {port:?} is not a valid unicast address")]
    MacNotUnicast { mac: net_types::ethernet::Mac, port: netdevice_client::Port },
    #[error("interface named {0} already exists")]
    DuplicateName(String),
}

const DEFAULT_BUFFER_LENGTH: usize = 2048;

// TODO(https://fxbug.dev/101303): Decorate *all* logging with human-readable
// device debug information to disambiguate.
impl NetdeviceWorker {
    pub async fn new(
        ctx: Ctx,
        device: fidl::endpoints::ClientEnd<fhardware_network::DeviceMarker>,
    ) -> Result<Self, Error> {
        let device =
            netdevice_client::Client::new(device.into_proxy().expect("must be in executor"));
        let (session, task) = device
            .primary_session("netstack3", DEFAULT_BUFFER_LENGTH)
            .await
            .map_err(Error::Client)?;
        Ok(Self { ctx, inner: Inner { device, session, state: Default::default() }, task })
    }

    pub fn new_handler(&self) -> DeviceHandler {
        DeviceHandler { inner: self.inner.clone() }
    }

    pub async fn run(self) -> Result<std::convert::Infallible, Error> {
        let Self { ctx, inner: Inner { device: _, session, state }, task } = self;
        // Allow buffer shuttling to happen in other threads.
        let mut task = fuchsia_async::Task::spawn(task).fuse();

        let mut buff = [0u8; DEFAULT_BUFFER_LENGTH];
        loop {
            // Extract result into an enum to avoid too much code in  macro.
            let rx: netdevice_client::Buffer<_> = futures::select! {
                r = session.recv().fuse() => r.map_err(Error::Client)?,
                r = task => match r {
                    Ok(()) => panic!("task should never end cleanly"),
                    Err(e) => return Err(Error::Client(e))
                }
            };
            let port = rx.port();
            let id = if let Some(id) = state.lock().await.get(&port) {
                id.clone()
            } else {
                tracing::debug!("dropping frame for port {:?}, no device mapping available", port);
                continue;
            };

            trace_duration!("netdevice::recv");

            // TODO(https://fxbug.dev/100873): pass strongly owned buffers down
            // to the stack instead of copying it out.
            rx.read_at(0, &mut buff[..rx.len()]).map_err(|e| {
                tracing::error!("failed to read from buffer {:?}", e);
                Error::Client(e)
            })?;

            let mut ctx = ctx.clone();
            let Ctx { sync_ctx, non_sync_ctx } = &mut ctx;

            let Some(id) = id.upgrade() else {
                // This is okay because we hold a weak reference; the device may
                // be removed under us. Note that when the device removal has
                // completed, the interface's `PortHandler` will be uninstalled
                // from the port slab (table of ports for this network device).
                tracing::debug!("received frame for device after it has been removed; device_id={:?}", id);
                // We continue because even though we got frames for a removed
                // device, this network device may have other ports that will
                // receive and handle frames.
                continue;
            };

            netstack3_core::device::receive_frame(
                sync_ctx,
                non_sync_ctx,
                &id,
                packet::Buf::new(&mut buff[..rx.len()], ..),
            )
        }
    }
}

pub(crate) struct InterfaceOptions {
    pub name: Option<String>,
    pub metric: Option<u32>,
}

pub(crate) struct DeviceHandler {
    inner: Inner,
}

impl DeviceHandler {
    pub(crate) async fn add_port(
        &self,
        ns: &Netstack,
        InterfaceOptions { name, metric }: InterfaceOptions,
        port: fhardware_network::PortId,
        control_hook: futures::channel::mpsc::Sender<interfaces_admin::OwnedControlHandle>,
    ) -> Result<
        (
            BindingId,
            impl futures::Stream<Item = netdevice_client::Result<netdevice_client::PortStatus>>,
        ),
        Error,
    > {
        let port = netdevice_client::Port::try_from(port)?;

        let DeviceHandler { inner: Inner { state, device, session: _ } } = self;
        let port_proxy = device.connect_port(port)?;
        let netdevice_client::client::PortInfo {
            id: _,
            base_info:
                netdevice_client::client::PortBaseInfo { port_class: device_class, rx_types, tx_types },
        } = port_proxy
            .get_info()
            .await
            .map_err(Error::CantConnectToPort)?
            .try_into()
            .map_err(Error::InvalidPortInfo)?;

        let mut status_stream =
            netdevice_client::client::new_port_status_stream(&port_proxy, None)?;

        // TODO(https://fxbug.dev/100871): support non-ethernet devices.
        let supports_ethernet_on_rx =
            rx_types.iter().any(|f| *f == fhardware_network::FrameType::Ethernet);
        let supports_ethernet_on_tx = tx_types.iter().any(
            |fhardware_network::FrameTypeSupport { type_, features: _, supported_flags: _ }| {
                *type_ == fhardware_network::FrameType::Ethernet
            },
        );
        if !(supports_ethernet_on_rx && supports_ethernet_on_tx) {
            return Err(Error::ConfigurationNotSupported);
        }

        let netdevice_client::client::PortStatus { flags, mtu: max_eth_frame_size } =
            status_stream.try_next().await?.ok_or_else(|| Error::PortClosed)?;
        let phy_up = flags.contains(fhardware_network::StatusFlags::ONLINE);

        let (mac_proxy, mac_server) =
            fidl::endpoints::create_proxy::<fhardware_network::MacAddressingMarker>()
                .map_err(Error::SystemResource)?;
        let () = port_proxy.get_mac(mac_server).map_err(Error::CantConnectToPort)?;

        let mac_addr = {
            let fnet::MacAddress { octets } =
                mac_proxy.get_unicast_address().await.map_err(|e| {
                    // TODO(https://fxbug.dev/100871): support non-ethernet
                    // devices.
                    tracing::warn!("failed to get unicast address, sending not supported: {:?}", e);
                    Error::ConfigurationNotSupported
                })?;
            let mac = net_types::ethernet::Mac::new(octets);
            net_types::UnicastAddr::new(mac).ok_or_else(|| {
                tracing::warn!("{} is not a valid unicast address", mac);
                Error::MacNotUnicast { mac, port }
            })?
        };

        // Always set the interface to multicast promiscuous mode because we
        // don't really plumb through multicast filtering.
        // TODO(https://fxbug.dev/58919): Remove this when multicast filtering
        // is available.
        fuchsia_zircon::Status::ok(
            mac_proxy
                .set_mode(fhardware_network::MacFilterMode::MulticastPromiscuous)
                .await
                .map_err(Error::CantConnectToPort)?,
        )
        .unwrap_or_else(|e| {
            tracing::warn!("failed to set multicast promiscuous for new interface: {:?}", e)
        });

        let mut state = state.lock().await;
        let state_entry = match state.entry(port) {
            netdevice_client::port_slab::Entry::Occupied(occupied) => {
                tracing::warn!(
                    "attempted to install port {:?} which is already installed for {:?}",
                    port,
                    occupied.get()
                );
                return Err(Error::AlreadyInstalled(port));
            }
            netdevice_client::port_slab::Entry::SaltMismatch(stale) => {
                tracing::warn!(
                    "attempted to install port {:?} which is already has a stale entry: {:?}",
                    port,
                    stale
                );
                return Err(Error::AlreadyInstalled(port));
            }
            netdevice_client::port_slab::Entry::Vacant(e) => e,
        };
        let mut ctx = &mut ns.ctx.clone();
        let Ctx { sync_ctx, non_sync_ctx } = &mut ctx;

        // Check if there already exists an interface with this name.
        // Interface names are unique.
        let name = name
            .map(|name| {
                if let Some(_device_info) = non_sync_ctx.devices.get_device_by_name(&name) {
                    return Err(Error::DuplicateName(name));
                };
                Ok(name)
            })
            .transpose()?;

        let max_frame_size = ethernet::MaxFrameSize::new(max_eth_frame_size)
            .ok_or(Error::ConfigurationNotSupported)?;
        let core_id = netstack3_core::device::add_ethernet_device_with_state(
            sync_ctx,
            mac_addr,
            max_frame_size,
            RawMetric(metric.unwrap_or(DEFAULT_INTERFACE_METRIC)),
            || {
                let binding_id = non_sync_ctx.devices.alloc_new_id();

                let name = name.unwrap_or_else(|| format!("eth{}", binding_id));
                devices::NetdeviceInfo {
                    handler: PortHandler {
                        id: binding_id,
                        port_id: port,
                        inner: self.inner.clone(),
                        _mac_proxy: mac_proxy,
                    },
                    mac: mac_addr,
                    dynamic: devices::DynamicNetdeviceInfo {
                        phy_up,
                        common_info: devices::DynamicCommonInfo {
                            mtu: max_frame_size.as_mtu(),
                            admin_enabled: false,
                            events: ns.create_interface_event_producer(
                                binding_id,
                                crate::bindings::InterfaceProperties {
                                    name: name.clone(),
                                    device_class: fnet_interfaces::DeviceClass::Device(
                                        device_class,
                                    ),
                                },
                            ),
                            control_hook: control_hook,
                            addresses: HashMap::new(),
                        },
                    }
                    .into(),
                    static_common_info: devices::StaticCommonInfo { binding_id, name },
                }
                .into()
            },
        );

        let devices::NetdeviceInfo {
            static_common_info: devices::StaticCommonInfo { binding_id, .. },
            handler: _,
            mac: _,
            dynamic: _,
        } = core_id.external_state();
        let binding_id = *binding_id;
        state_entry.insert(core_id.downgrade());

        let core_id = core_id.into();
        add_initial_routes(sync_ctx, non_sync_ctx, &core_id).expect("failed to add default routes");

        // TODO(https://fxbug.dev/69644): Use a different secret key (not this
        // one) to generate stable opaque interface identifiers.
        let mut secret_key = [0; STABLE_IID_SECRET_KEY_BYTES];
        non_sync_ctx.rng().fill(&mut secret_key);

        let ip_config = Some(IpDeviceConfigurationUpdate {
            ip_enabled: Some(false),
            forwarding_enabled: Some(false),
            gmp_enabled: Some(true),
        });

        let _: Ipv6DeviceConfigurationUpdate = update_ipv6_configuration(
            sync_ctx,
            non_sync_ctx,
            &core_id,
            Ipv6DeviceConfigurationUpdate {
                dad_transmits: Some(Some(
                    Ipv6DeviceConfiguration::DEFAULT_DUPLICATE_ADDRESS_DETECTION_TRANSMITS,
                )),
                max_router_solicitations: Some(Some(
                    Ipv6DeviceConfiguration::DEFAULT_MAX_RTR_SOLICITATIONS,
                )),
                slaac_config: Some(SlaacConfiguration {
                    enable_stable_addresses: true,
                    temporary_address_configuration: Some(
                        TemporarySlaacAddressConfiguration::default_with_secret_key(secret_key),
                    ),
                }),
                ip_config,
            },
        )
        .unwrap();
        let _: Ipv4DeviceConfigurationUpdate = update_ipv4_configuration(
            sync_ctx,
            non_sync_ctx,
            &core_id,
            Ipv4DeviceConfigurationUpdate { ip_config },
        )
        .unwrap();

        non_sync_ctx.devices.add_device(binding_id, core_id.clone());

        Ok((binding_id, status_stream))
    }
}

/// Adds the IPv4 and IPv6 multicast subnet routes, the IPv6 link-local subnet
/// route, and the IPv4 limited broadcast subnet route.
///
/// Note that if an error is encountered while installing a route, any routes
/// that were successfully installed prior to the error will not be removed.
fn add_initial_routes<NonSyncCtx: NonSyncContext>(
    sync_ctx: &SyncCtx<NonSyncCtx>,
    non_sync_ctx: &mut NonSyncCtx,
    device: &DeviceId<NonSyncCtx>,
) -> Result<(), netstack3_core::ip::forwarding::AddRouteError> {
    use netstack3_core::ip::types::{AddableEntry, AddableEntryEither, AddableMetric};
    const LINK_LOCAL_SUBNET: Subnet<Ipv6Addr> = net_declare::net_subnet_v6!("fe80::/64");
    for entry in [
        AddableEntryEither::from(AddableEntry::without_gateway(
            LINK_LOCAL_SUBNET,
            device.clone(),
            AddableMetric::MetricTracksInterface,
        )),
        AddableEntryEither::from(AddableEntry::without_gateway(
            Ipv4::MULTICAST_SUBNET,
            device.clone(),
            AddableMetric::MetricTracksInterface,
        )),
        AddableEntryEither::from(AddableEntry::without_gateway(
            Ipv6::MULTICAST_SUBNET,
            device.clone(),
            AddableMetric::MetricTracksInterface,
        )),
        AddableEntryEither::from(AddableEntry::without_gateway(
            crate::bindings::IPV4_LIMITED_BROADCAST_SUBNET,
            device.clone(),
            AddableMetric::ExplicitMetric(RawMetric(crate::bindings::DEFAULT_LOW_PRIORITY_METRIC)),
        )),
    ] {
        netstack3_core::add_route(sync_ctx, non_sync_ctx, entry)?;
    }
    Ok(())
}

pub struct PortHandler {
    id: BindingId,
    port_id: netdevice_client::Port,
    inner: Inner,
    // We must keep the mac proxy alive to maintain our multicast filtering mode
    // selection set.
    _mac_proxy: fhardware_network::MacAddressingProxy,
}

#[derive(thiserror::Error, Debug)]
pub(crate) enum SendError {
    #[error("no buffers available")]
    NoTxBuffers,
    #[error("device error: {0}")]
    Device(#[from] netdevice_client::Error),
}

impl PortHandler {
    pub(crate) async fn attach(&self) -> Result<(), netdevice_client::Error> {
        let Self { id: _, port_id, inner: Inner { device: _, session, state: _ }, _mac_proxy: _ } =
            self;
        session.attach(*port_id, &[fhardware_network::FrameType::Ethernet]).await
    }

    pub(crate) async fn detach(&self) -> Result<(), netdevice_client::Error> {
        let Self { id: _, port_id, inner: Inner { device: _, session, state: _ }, _mac_proxy: _ } =
            self;
        session.detach(*port_id).await
    }

    pub(crate) fn send(&self, frame: &[u8]) -> Result<(), SendError> {
        trace_duration!("netdevice::send");

        let Self { id: _, port_id, inner: Inner { device: _, session, state: _ }, _mac_proxy: _ } =
            self;
        // NB: We currently send on a dispatcher, so we can't wait for new
        // buffers to become available. If that ends up being the long term way
        // of enqueuing outgoing buffers we might want to fix this impedance
        // mismatch here.
        let mut tx =
            session.alloc_tx_buffer(frame.len()).now_or_never().ok_or(SendError::NoTxBuffers)??;
        tx.set_port(*port_id);
        tx.set_frame_type(fhardware_network::FrameType::Ethernet);
        tx.write_at(0, frame)?;
        session.send(tx)?;
        Ok(())
    }

    pub(crate) async fn uninstall(self) -> Result<(), netdevice_client::Error> {
        let Self { id: _, port_id, inner: Inner { device: _, session, state }, _mac_proxy: _ } =
            self;
        let _: EthernetWeakDeviceId<_> = assert_matches!(
            state.lock().await.remove(&port_id),
            netdevice_client::port_slab::RemoveOutcome::Removed(core_id) => core_id
        );
        session.detach(port_id).await
    }

    pub(crate) fn connect_port(
        &self,
        port: fidl::endpoints::ServerEnd<fhardware_network::PortMarker>,
    ) -> Result<(), netdevice_client::Error> {
        let Self { id: _, port_id, inner: Inner { device, session: _, state: _ }, _mac_proxy: _ } =
            self;
        device.connect_port_server_end(*port_id, port)
    }
}

impl std::fmt::Debug for PortHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let Self { id, port_id, inner: _, _mac_proxy: _ } = self;
        f.debug_struct("PortHandler").field("id", id).field("port_id", port_id).finish()
    }
}
