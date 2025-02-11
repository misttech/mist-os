// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::convert::{TryFrom as _, TryInto as _};
use std::num::TryFromIntError;
use std::sync::Arc;

use assert_matches::assert_matches;
use fidl_fuchsia_hardware_network::{self as fhardware_network};
use futures::lock::Mutex;
use futures::{FutureExt as _, StreamExt, TryStreamExt as _};
use log::{debug, error, info, warn};
use net_types::ethernet::Mac;
use net_types::ip::{Ip, IpVersion, Ipv4, Ipv6, Ipv6Addr, Mtu, Subnet};
use net_types::UnicastAddr;
use netstack3_core::device::{
    EthernetCreationProperties, EthernetDeviceId, EthernetLinkDevice, EthernetWeakDeviceId,
    MaxEthernetFrameSize, PureIpDevice, PureIpDeviceCreationProperties, PureIpDeviceId,
    PureIpDeviceReceiveFrameMetadata, PureIpWeakDeviceId, RecvEthernetFrameMeta,
};
use netstack3_core::ip::{
    IidGenerationConfiguration, IpDeviceConfigurationUpdate, Ipv4DeviceConfigurationUpdate,
    Ipv6DeviceConfigurationUpdate, SlaacConfigurationUpdate, StableSlaacAddressConfiguration,
    TemporarySlaacAddressConfiguration,
};
use netstack3_core::routes::RawMetric;
use netstack3_core::sync::RwLock as CoreRwLock;
use {
    fidl_fuchsia_net as fnet, fidl_fuchsia_net_interfaces_ext as fnet_interfaces_ext,
    fuchsia_async as fasync,
};

use crate::bindings::devices::TxTask;
use crate::bindings::util::NeedsDataNotifier;
use crate::bindings::{
    devices, interfaces_admin, routes, trace_duration, BindingId, BindingsCtx, Ctx, DeviceId,
    Ipv6DeviceConfiguration, Netstack, DEFAULT_INTERFACE_METRIC,
};

/// Like [`DeviceId`], but restricted to netdevice devices.
enum NetdeviceId {
    Ethernet(EthernetDeviceId<BindingsCtx>),
    PureIp(PureIpDeviceId<BindingsCtx>),
}

/// Like [`WeakDeviceId`], but restricted to netdevice devices.
#[derive(Clone, Debug)]
enum WeakNetdeviceId {
    Ethernet(EthernetWeakDeviceId<BindingsCtx>),
    PureIp(PureIpWeakDeviceId<BindingsCtx>),
}

impl WeakNetdeviceId {
    fn upgrade(&self) -> Option<NetdeviceId> {
        match self {
            WeakNetdeviceId::Ethernet(eth) => eth.upgrade().map(NetdeviceId::Ethernet),
            WeakNetdeviceId::PureIp(ip) => ip.upgrade().map(NetdeviceId::PureIp),
        }
    }
}

#[derive(Clone)]
struct Inner {
    device: netdevice_client::Client,
    session: netdevice_client::Session,
    state: Arc<Mutex<netdevice_client::PortSlab<WeakNetdeviceId>>>,
}

/// The worker that receives messages from the ethernet device, and passes them
/// on to the main event loop.
pub(crate) struct NetdeviceWorker {
    ctx: Ctx,
    task: netdevice_client::Task,
    inner: Inner,
    watch_rx_leases: bool,
}

#[derive(thiserror::Error, Debug)]
pub(crate) enum Error {
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
    #[error("{port_class:?} port received unexpected frame type: {frame_type:?}")]
    MismatchedRxFrameType { port_class: PortWireFormat, frame_type: FrameType },
    #[error("invalid port class: {0}")]
    InvalidPortClass(fnet_interfaces_ext::UnknownHardwareNetworkPortClassError),
    #[error("received unsupported frame type: {0:?}")]
    UnsupportedFrameType(fhardware_network::FrameType),
}

const DEFAULT_BUFFER_LENGTH: usize = 2048;

#[derive(Debug)]
pub(crate) enum FrameType {
    Ethernet,
    Ipv4,
    Ipv6,
}

impl TryFrom<fhardware_network::FrameType> for FrameType {
    type Error = Error;
    fn try_from(value: fhardware_network::FrameType) -> Result<Self, Self::Error> {
        match value {
            fhardware_network::FrameType::Ethernet => Ok(Self::Ethernet),
            fhardware_network::FrameType::Ipv4 => Ok(Self::Ipv4),
            fhardware_network::FrameType::Ipv6 => Ok(Self::Ipv6),
            x @ fhardware_network::FrameType::__SourceBreaking { .. } => {
                Err(Error::UnsupportedFrameType(x))
            }
        }
    }
}

impl NetdeviceWorker {
    pub(crate) async fn new(
        ctx: Ctx,
        device: fidl::endpoints::ClientEnd<fhardware_network::DeviceMarker>,
    ) -> Result<Self, Error> {
        let device = netdevice_client::Client::new(device.into_proxy());
        // Enable rx lease watching when suspension is enabled.
        let watch_rx_leases = ctx.bindings_ctx().config.suspend_enabled;
        let (session, task) = device
            .new_session_with_derivable_config(
                "netstack3",
                netdevice_client::DerivableConfig {
                    default_buffer_length: DEFAULT_BUFFER_LENGTH,
                    primary: true,
                    watch_rx_leases,
                },
            )
            .await
            .map_err(Error::Client)?;
        Ok(Self {
            ctx,
            inner: Inner { device, session, state: Default::default() },
            task,
            watch_rx_leases,
        })
    }

    pub(crate) fn new_handler(&self) -> DeviceHandler {
        DeviceHandler { inner: self.inner.clone() }
    }

    pub(crate) async fn run(self) -> Result<std::convert::Infallible, Error> {
        let Self { mut ctx, inner: Inner { device: _, session, state }, task, watch_rx_leases } =
            self;
        // Allow buffer shuttling to happen in other threads.
        let mut task = fuchsia_async::Task::spawn(task).fuse();

        // Watch rx leases in a separate task if configured to do so.
        //
        // We need not poll this task to observe its completion, it suffices
        // that it goes away when we stop serving the device (executor polls the
        // future for us). Any leases held in the stream will be safely dropped,
        // but before netstack executes any logic on them.
        let _watch_rx_leases_task = watch_rx_leases.then(|| {
            let ctx = ctx.clone();
            let fut = session
                .watch_rx_leases()
                .map(move |r| {
                    let lease = match r {
                        Ok(l) => l,
                        Err(e) => {
                            error!("error watching rx leases: {e:?}");
                            return;
                        }
                    };
                    // NB: Increment the counter before dropping the lease so
                    // the side effects are synchronized. Useful for tests to
                    // synchronize on lease drop
                    debug!("dropping delegated rx power lease");
                    ctx.bindings_ctx().counters.power.dropped_rx_leases.increment();
                    std::mem::drop(lease);
                })
                .collect::<()>();
            fasync::Task::spawn(fut)
        });

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
                debug!("dropping frame for port {:?}, no device mapping available", port);
                continue;
            };

            trace_duration!(c"netdevice::recv");

            let frame_length = rx.len();
            // TODO(https://fxbug.dev/42051635): pass strongly owned buffers down
            // to the stack instead of copying it out.
            rx.read_at(0, &mut buff[..frame_length]).map_err(|e| {
                error!("failed to read from buffer {:?}", e);
                Error::Client(e)
            })?;

            let Some(id) = id.upgrade() else {
                // This is okay because we hold a weak reference; the device may
                // be removed under us. Note that when the device removal has
                // completed, the interface's `PortHandler` will be uninstalled
                // from the port slab (table of ports for this network device).
                debug!("received frame for device after it has been removed; device_id={id:?}");
                // We continue because even though we got frames for a removed
                // device, this network device may have other ports that will
                // receive and handle frames.
                continue;
            };

            let buf = packet::Buf::new(&mut buff[..frame_length], ..);
            let frame_type = rx.frame_type().map_err(Error::Client)?.try_into()?;
            match id {
                NetdeviceId::Ethernet(id) => {
                    match frame_type {
                        FrameType::Ethernet => {}
                        f @ FrameType::Ipv4 | f @ FrameType::Ipv6 => {
                            // NB: When the port was attached, `Ethernet` was
                            // the only permitted frame type; anything else here
                            // indicates a bug in `netdevice_client` or the core
                            // netdevice driver.
                            return Err(Error::MismatchedRxFrameType {
                                port_class: PortWireFormat::Ethernet,
                                frame_type: f,
                            });
                        }
                    }
                    ctx.api()
                        .device::<EthernetLinkDevice>()
                        .receive_frame(RecvEthernetFrameMeta { device_id: id.clone() }, buf)
                }
                NetdeviceId::PureIp(id) => {
                    let ip_version = match frame_type {
                        FrameType::Ipv4 => IpVersion::V4,
                        FrameType::Ipv6 => IpVersion::V6,
                        f @ FrameType::Ethernet => {
                            // NB: When the port was attached, `IPv4` & `Ipv6`
                            // were the only permitted frame types; anything
                            // else here indicates a bug in `netdevice_client` or
                            // the core netdevice driver.
                            return Err(Error::MismatchedRxFrameType {
                                port_class: PortWireFormat::Ip,
                                frame_type: f,
                            });
                        }
                    };
                    ctx.api().device::<PureIpDevice>().receive_frame(
                        PureIpDeviceReceiveFrameMetadata { device_id: id.clone(), ip_version },
                        buf,
                    )
                }
            }
        }
    }
}

pub(crate) struct InterfaceOptions {
    pub(crate) name: Option<String>,
    pub(crate) metric: Option<u32>,
}

pub(crate) struct DeviceHandler {
    inner: Inner,
}

/// The wire format for packets sent to and received on a port.
#[derive(Debug, Eq, PartialEq)]
pub(crate) enum PortWireFormat {
    /// The port supports sending/receiving Ethernet frames.
    Ethernet,
    /// The port supports sending/receiving IPv4 and IPv6 packets.
    Ip,
}

impl PortWireFormat {
    fn frame_types(&self) -> &[fhardware_network::FrameType] {
        const ETHERNET_FRAMES: [fhardware_network::FrameType; 1] =
            [fhardware_network::FrameType::Ethernet];
        const IP_FRAMES: [fhardware_network::FrameType; 2] =
            [fhardware_network::FrameType::Ipv4, fhardware_network::FrameType::Ipv6];
        match self {
            Self::Ethernet => &ETHERNET_FRAMES,
            Self::Ip => &IP_FRAMES,
        }
    }
}

/// Error returned for ports with unsupported wire formats.
#[derive(Debug)]
pub(crate) enum PortWireFormatError<'a> {
    InvalidRxFrameTypes { _frame_types: Vec<&'a fhardware_network::FrameType> },
    InvalidTxFrameTypes { _frame_types: Vec<&'a fhardware_network::FrameType> },
    MismatchedRxTx { _rx: PortWireFormat, _tx: PortWireFormat },
}

impl PortWireFormat {
    fn new_from_port_info(
        info: &netdevice_client::client::PortBaseInfo,
    ) -> Result<PortWireFormat, PortWireFormatError<'_>> {
        let netdevice_client::client::PortBaseInfo { port_class: _, rx_types, tx_types } = info;

        // Verify the wire format in a single direction (tx/rx).
        fn wire_format_from_frame_types<'a>(
            frame_types: impl Iterator<Item = &'a fhardware_network::FrameType> + Clone,
        ) -> Result<PortWireFormat, impl Iterator<Item = &'a fhardware_network::FrameType>>
        {
            struct SupportedFormats {
                ethernet: bool,
                ipv4: bool,
                ipv6: bool,
            }
            let SupportedFormats { ethernet, ipv4, ipv6 } = frame_types.clone().fold(
                SupportedFormats { ethernet: false, ipv4: false, ipv6: false },
                |mut sf, frame_type| {
                    match frame_type {
                        fhardware_network::FrameType::Ethernet => sf.ethernet = true,
                        fhardware_network::FrameType::Ipv4 => sf.ipv4 = true,
                        fhardware_network::FrameType::Ipv6 => sf.ipv6 = true,
                        fhardware_network::FrameType::__SourceBreaking { .. } => (),
                    }
                    sf
                },
            );
            // Disallow devices with mixed frame types, and require that IP
            // Devices support both IPv4 and IPv6.
            if ethernet && !ipv4 && !ipv6 {
                Ok(PortWireFormat::Ethernet)
            } else if !ethernet && ipv4 && ipv6 {
                Ok(PortWireFormat::Ip)
            } else {
                Err(frame_types)
            }
        }

        // Ignore the superfluous information included with the tx frame types.
        let tx_iterator = || {
            tx_types.iter().map(
                |fhardware_network::FrameTypeSupport {
                     type_: frame_type,
                     features: _,
                     supported_flags: _,
                 }| { frame_type },
            )
        };

        // Verify each direction independently, and then ensure the port is
        // symmetrical.
        let rx_wire_format = wire_format_from_frame_types(rx_types.iter()).map_err(|rx_types| {
            PortWireFormatError::InvalidRxFrameTypes { _frame_types: rx_types.collect() }
        })?;
        let tx_wire_format = wire_format_from_frame_types(tx_iterator()).map_err(|tx_types| {
            PortWireFormatError::InvalidTxFrameTypes { _frame_types: tx_types.collect() }
        })?;
        if rx_wire_format == tx_wire_format {
            Ok(rx_wire_format)
        } else {
            Err(PortWireFormatError::MismatchedRxTx { _rx: rx_wire_format, _tx: tx_wire_format })
        }
    }
}

impl DeviceHandler {
    pub(crate) async fn add_port(
        &self,
        ns: &mut Netstack,
        InterfaceOptions { name, metric }: InterfaceOptions,
        port: fhardware_network::PortId,
        control_hook: futures::channel::mpsc::Sender<interfaces_admin::OwnedControlHandle>,
    ) -> Result<
        (
            BindingId,
            impl futures::Stream<Item = netdevice_client::Result<netdevice_client::PortStatus>>,
            TxTask,
        ),
        Error,
    > {
        let port = netdevice_client::Port::try_from(port)?;

        let DeviceHandler { inner: Inner { state, device, session: _ } } = self;
        let port_proxy = device.connect_port(port)?;
        let netdevice_client::client::PortInfo { id: _, base_info } = port_proxy
            .get_info()
            .await
            .map_err(Error::CantConnectToPort)?
            .try_into()
            .map_err(Error::InvalidPortInfo)?;

        let mut status_stream =
            netdevice_client::client::new_port_status_stream(&port_proxy, None)?;

        let wire_format = PortWireFormat::new_from_port_info(&base_info).map_err(
            |e: PortWireFormatError<'_>| {
                warn!("not installing port with invalid wire format: {:?}", e);
                Error::ConfigurationNotSupported
            },
        )?;

        let netdevice_client::client::PortStatus { flags, mtu } =
            status_stream.try_next().await?.ok_or_else(|| Error::PortClosed)?;
        let phy_up = flags.contains(fhardware_network::StatusFlags::ONLINE);
        let netdevice_client::client::PortBaseInfo {
            port_class: hw_port_class,
            rx_types: _,
            tx_types: _,
        } = base_info;

        enum DeviceProperties {
            Ethernet {
                max_frame_size: MaxEthernetFrameSize,
                mac: UnicastAddr<Mac>,
                mac_proxy: fhardware_network::MacAddressingProxy,
            },
            Ip {
                max_frame_size: Mtu,
            },
        }
        let properties = match wire_format {
            PortWireFormat::Ethernet => {
                let max_frame_size =
                    MaxEthernetFrameSize::new(mtu).ok_or(Error::ConfigurationNotSupported)?;
                let (mac, mac_proxy) = get_mac(&port_proxy, &port).await?;
                DeviceProperties::Ethernet { max_frame_size, mac, mac_proxy }
            }
            PortWireFormat::Ip => DeviceProperties::Ip { max_frame_size: Mtu::new(mtu) },
        };

        let mut state = state.lock().await;
        let state_entry = match state.entry(port) {
            netdevice_client::port_slab::Entry::Occupied(occupied) => {
                warn!(
                    "attempted to install port {:?} which is already installed for {:?}",
                    port,
                    occupied.get()
                );
                return Err(Error::AlreadyInstalled(port));
            }
            netdevice_client::port_slab::Entry::SaltMismatch(stale) => {
                warn!(
                    "attempted to install port {:?} which is already has a stale entry: {:?}",
                    port, stale
                );
                return Err(Error::AlreadyInstalled(port));
            }
            netdevice_client::port_slab::Entry::Vacant(e) => e,
        };
        let Netstack { interfaces_event_sink, neighbor_event_sink, ctx } = ns;

        let max_frame_size =
            mtu.try_into().map_err(|TryFromIntError { .. }| Error::ConfigurationNotSupported)?;

        let port_class = fnet_interfaces_ext::PortClass::try_from(hw_port_class)
            .map_err(Error::InvalidPortClass)?;

        let (binding_id, binding_id_alloc, name) = match name {
            None => ctx.bindings_ctx().devices.generate_and_reserve_name_and_alloc_new_id(
                match wire_format {
                    PortWireFormat::Ethernet => "eth",
                    PortWireFormat::Ip => "ip",
                },
            ),
            Some(name) => {
                match ctx.bindings_ctx().devices.try_reserve_name_and_alloc_new_id(name.clone()) {
                    Err(devices::NameNotAvailableError) => return Err(Error::DuplicateName(name)),
                    Ok((id, alloc)) => (id, alloc, name),
                }
            }
        };

        let iid_generation = if ctx.bindings_ctx().config.default_opaque_iids {
            IidGenerationConfiguration::Opaque {
                idgen_retries: StableSlaacAddressConfiguration::DEFAULT_IDGEN_RETRIES,
            }
        } else {
            IidGenerationConfiguration::Eui64
        };

        // Do the rest of the work in a closure so we don't accidentally add
        // errors after the binding ID is already allocated. This part of the
        // function should be infallible.
        let finalize = move || async move {
            let tx_notifier = NeedsDataNotifier::default();
            let tx_watcher = tx_notifier.watcher();
            let static_netdevice_info = devices::StaticNetdeviceInfo {
                handler: PortHandler {
                    id: binding_id,
                    port_id: port,
                    inner: self.inner.clone(),
                    port_class: hw_port_class,
                    wire_format,
                    max_frame_size,
                },
                tx_notifier,
            };
            let dynamic_netdevice_info_builder = |mtu: Mtu| devices::DynamicNetdeviceInfo {
                phy_up,
                common_info: devices::DynamicCommonInfo::new(
                    mtu,
                    crate::bindings::create_interface_event_producer(
                        interfaces_event_sink,
                        binding_id,
                        crate::bindings::InterfaceProperties { name: name.clone(), port_class },
                    ),
                    control_hook,
                ),
            };

            let core_id = match properties {
                DeviceProperties::Ethernet { max_frame_size, mac, mac_proxy } => {
                    let info = devices::EthernetInfo {
                        mac,
                        _mac_proxy: mac_proxy,
                        netdevice: static_netdevice_info,
                        common_info: Default::default(),
                        dynamic_info: CoreRwLock::new(devices::DynamicEthernetInfo {
                            netdevice: dynamic_netdevice_info_builder(max_frame_size.as_mtu()),
                            neighbor_event_sink: neighbor_event_sink.clone(),
                        }),
                    }
                    .into();
                    let core_ethernet_id = ctx.api().device::<EthernetLinkDevice>().add_device(
                        devices::DeviceIdAndName { id: binding_id, name: name.clone() },
                        EthernetCreationProperties { mac, max_frame_size },
                        RawMetric(metric.unwrap_or(DEFAULT_INTERFACE_METRIC)),
                        info,
                    );
                    state_entry.insert(WeakNetdeviceId::Ethernet(core_ethernet_id.downgrade()));
                    DeviceId::from(core_ethernet_id)
                }
                DeviceProperties::Ip { max_frame_size } => {
                    let info = devices::PureIpDeviceInfo {
                        common_info: Default::default(),
                        netdevice: static_netdevice_info,
                        dynamic_info: CoreRwLock::new(dynamic_netdevice_info_builder(
                            max_frame_size,
                        )),
                    }
                    .into();
                    let core_pure_ip_id = ctx.api().device::<PureIpDevice>().add_device(
                        devices::DeviceIdAndName { id: binding_id, name: name.clone() },
                        PureIpDeviceCreationProperties { mtu: max_frame_size },
                        RawMetric(metric.unwrap_or(DEFAULT_INTERFACE_METRIC)),
                        info,
                    );
                    state_entry.insert(WeakNetdeviceId::PureIp(core_pure_ip_id.downgrade()));
                    DeviceId::from(core_pure_ip_id)
                }
            };

            let tx_task = TxTask::new(ctx.clone(), core_id.downgrade(), tx_watcher);
            match &core_id {
                DeviceId::PureIp(device) => {
                    ctx.api().transmit_queue::<PureIpDevice>().set_configuration(
                        device,
                        netstack3_core::device::TransmitQueueConfiguration::Fifo,
                    );
                }
                DeviceId::Ethernet(device) => {
                    ctx.api().transmit_queue::<EthernetLinkDevice>().set_configuration(
                        device,
                        netstack3_core::device::TransmitQueueConfiguration::Fifo,
                    );
                }
                DeviceId::Loopback(device) => {
                    unreachable!("loopback device should not be backed by netdevice: {device:?}")
                }
                DeviceId::Blackhole(device) => {
                    unreachable!("blackhole device should not be backed by netdevice: {device:?}")
                }
            }
            add_initial_routes(ctx.bindings_ctx(), &core_id).await;

            let ip_config = IpDeviceConfigurationUpdate {
                ip_enabled: Some(false),
                unicast_forwarding_enabled: Some(false),
                multicast_forwarding_enabled: Some(false),
                gmp_enabled: Some(true),
            };

            let _: Ipv6DeviceConfigurationUpdate = ctx
                .api()
                .device_ip::<Ipv6>()
                .update_configuration(
                    &core_id,
                    Ipv6DeviceConfigurationUpdate {
                        dad_transmits: Some(Some(
                            Ipv6DeviceConfiguration::DEFAULT_DUPLICATE_ADDRESS_DETECTION_TRANSMITS,
                        )),
                        max_router_solicitations: Some(Some(
                            Ipv6DeviceConfiguration::DEFAULT_MAX_RTR_SOLICITATIONS,
                        )),
                        slaac_config: SlaacConfigurationUpdate {
                            stable_address_configuration: Some(
                                StableSlaacAddressConfiguration::Enabled { iid_generation },
                            ),
                            temporary_address_configuration: Some(
                                TemporarySlaacAddressConfiguration::enabled_with_rfc_defaults(),
                            ),
                        },
                        ip_config,
                        mld_mode: None,
                    },
                )
                .unwrap();
            let _: Ipv4DeviceConfigurationUpdate = ctx
                .api()
                .device_ip::<Ipv4>()
                .update_configuration(
                    &core_id,
                    Ipv4DeviceConfigurationUpdate { ip_config, igmp_mode: None },
                )
                .unwrap();

            info!("created interface {:?}", core_id);
            ctx.bindings_ctx().devices.add_device(binding_id_alloc, core_id);

            (binding_id, tx_task)
        };
        let (binding_id, tx_task) = finalize().await;

        Ok((binding_id, status_stream, tx_task))
    }
}

/// Connect to the Port's `MacAddressingProxy`, and fetch the MAC address.
async fn get_mac(
    port_proxy: &fhardware_network::PortProxy,
    port: &netdevice_client::Port,
) -> Result<(UnicastAddr<Mac>, fhardware_network::MacAddressingProxy), Error> {
    let (mac_proxy, mac_server) =
        fidl::endpoints::create_proxy::<fhardware_network::MacAddressingMarker>();
    let () = port_proxy.get_mac(mac_server).map_err(Error::CantConnectToPort)?;

    let mac_addr = {
        let fnet::MacAddress { octets } = mac_proxy.get_unicast_address().await.map_err(|e| {
            warn!("failed to get unicast address, sending not supported: {:?}", e);
            Error::ConfigurationNotSupported
        })?;
        let mac = net_types::ethernet::Mac::new(octets);
        net_types::UnicastAddr::new(mac).ok_or_else(|| {
            warn!("{} is not a valid unicast address", mac);
            Error::MacNotUnicast { mac, port: *port }
        })?
    };
    // Always set the interface to multicast promiscuous mode because we
    // don't really plumb through multicast filtering.
    // TODO(https://fxbug.dev/42136929): Remove this when multicast filtering
    // is available.
    zx::Status::ok(
        mac_proxy
            .set_mode(fhardware_network::MacFilterMode::MulticastPromiscuous)
            .await
            .map_err(Error::CantConnectToPort)?,
    )
    .unwrap_or_else(|e| warn!("failed to set multicast promiscuous for new interface: {:?}", e));

    Ok((mac_addr, mac_proxy))
}

/// Adds the IPv4 and IPv6 multicast subnet routes and the IPv6 link-local
/// subnet route.
///
/// Note that if an error is encountered while installing a route, any routes
/// that were successfully installed prior to the error will not be removed.
async fn add_initial_routes(bindings_ctx: &BindingsCtx, device: &DeviceId<BindingsCtx>) {
    use netstack3_core::routes::{AddableEntry, AddableMetric};
    const LINK_LOCAL_SUBNET: Subnet<Ipv6Addr> = net_declare::net_subnet_v6!("fe80::/64");

    let v4_changes = std::iter::once(AddableEntry::without_gateway(
        Ipv4::MULTICAST_SUBNET,
        device.downgrade(),
        AddableMetric::MetricTracksInterface,
    ))
    .map(|entry| {
        routes::Change::<Ipv4>::RouteOp(
            routes::RouteOp::Add(entry),
            routes::SetMembership::InitialDeviceRoutes,
        )
    })
    .map(Into::into);

    let v6_changes = [
        AddableEntry::without_gateway(
            LINK_LOCAL_SUBNET,
            device.downgrade(),
            AddableMetric::MetricTracksInterface,
        ),
        AddableEntry::without_gateway(
            Ipv6::MULTICAST_SUBNET,
            device.downgrade(),
            AddableMetric::MetricTracksInterface,
        ),
    ]
    .into_iter()
    .map(|entry| {
        routes::Change::<Ipv6>::RouteOp(
            routes::RouteOp::Add(entry),
            routes::SetMembership::InitialDeviceRoutes,
        )
    })
    .map(Into::into);

    for change in v4_changes.chain(v6_changes) {
        bindings_ctx
            .apply_route_change_either(change)
            .await
            .map(|outcome| assert_matches!(outcome, routes::ChangeOutcome::Changed))
            .expect("adding initial routes should succeed");
    }
}

pub(crate) struct PortHandler {
    id: BindingId,
    port_id: netdevice_client::Port,
    inner: Inner,
    port_class: fhardware_network::PortClass,
    wire_format: PortWireFormat,
    max_frame_size: usize,
}

impl PortHandler {
    pub(crate) fn port_class(&self) -> fhardware_network::PortClass {
        self.port_class
    }

    pub(crate) async fn attach(&self) -> Result<(), netdevice_client::Error> {
        let Self { port_id, inner: Inner { session, .. }, wire_format, .. } = self;
        session.attach(*port_id, wire_format.frame_types()).await
    }

    pub(crate) async fn detach(&self) -> Result<(), netdevice_client::Error> {
        let Self { port_id, inner: Inner { session, .. }, .. } = self;
        session.detach(*port_id).await
    }

    /// Waits for at least one buffer be available and returns an iterator of
    /// available tx buffers.
    ///
    /// Buffers are allocated with the ports's maximum frame size.
    pub(crate) async fn alloc_tx_buffers(
        &self,
    ) -> Result<
        impl Iterator<Item = Result<netdevice_client::TxBuffer, netdevice_client::Error>> + '_,
        netdevice_client::Error,
    > {
        let Self { inner: Inner { session, .. }, max_frame_size, .. } = self;
        session.alloc_tx_buffers(*max_frame_size).await
    }

    /// Attempts to allocate a [`netdevice::TxBuffer`] synchronously.
    ///
    /// Returns `Ok(None)` if no buffers are available.
    ///
    /// The buffer is always allocated with the ports's maximum frame size.
    pub(crate) fn alloc_tx_buffer(
        &self,
    ) -> Result<Option<netdevice_client::TxBuffer>, netdevice_client::Error> {
        let Self { inner: Inner { session, .. }, max_frame_size, .. } = self;
        session.alloc_tx_buffer(*max_frame_size).now_or_never().transpose()
    }

    pub(crate) fn send(
        &self,
        frame: &[u8],
        frame_type: fhardware_network::FrameType,
        mut tx: netdevice_client::TxBuffer,
    ) -> Result<(), netdevice_client::Error> {
        trace_duration!(c"netdevice::send");
        let Self { port_id, inner: Inner { session, .. }, .. } = self;
        tx.set_port(*port_id);
        tx.set_frame_type(frame_type);
        tx.write_at(0, frame)?;
        session.send(tx)?;
        Ok(())
    }

    pub(crate) async fn uninstall(self) -> Result<(), netdevice_client::Error> {
        let Self { port_id, inner: Inner { session, state, .. }, .. } = self;
        let _: WeakNetdeviceId = assert_matches!(
            state.lock().await.remove(&port_id),
            netdevice_client::port_slab::RemoveOutcome::Removed(core_id) => core_id
        );
        session.detach(port_id).await
    }

    pub(crate) fn connect_port(
        &self,
        port: fidl::endpoints::ServerEnd<fhardware_network::PortMarker>,
    ) -> Result<(), netdevice_client::Error> {
        let Self { port_id, inner: Inner { device, .. }, .. } = self;
        device.connect_port_server_end(*port_id, port)
    }

    pub(crate) async fn wait_tx_done(&self) {
        let Self { inner: Inner { session, .. }, .. } = self;
        session.wait_tx_idle().await;
    }
}

impl std::fmt::Debug for PortHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let Self { id, port_id, inner: _, port_class, wire_format, max_frame_size } = self;
        f.debug_struct("PortHandler")
            .field("id", id)
            .field("port_id", port_id)
            .field("port_class", port_class)
            .field("wire_format", wire_format)
            .field("max_frame_size", max_frame_size)
            .finish()
    }
}
