// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::{
    collections::HashMap,
    convert::{TryFrom as _, TryInto as _},
    sync::Arc,
};

use assert_matches::assert_matches;
use fidl_fuchsia_hardware_network::{self as fhardware_network, FrameType};
use fidl_fuchsia_net as fnet;
use fidl_fuchsia_net_interfaces as fnet_interfaces;
use fuchsia_async as fasync;
use fuchsia_zircon as zx;

use futures::{lock::Mutex, FutureExt as _, TryStreamExt as _};
use net_types::{
    ethernet::Mac,
    ip::{Ip, IpVersion, Ipv4, Ipv6, Ipv6Addr, Mtu, Subnet},
    UnicastAddr,
};
use netstack3_core::{
    device::{
        DeviceProvider, EthernetCreationProperties, EthernetDeviceId, EthernetLinkDevice,
        EthernetWeakDeviceId, MaxEthernetFrameSize, PureIpDevice, PureIpDeviceCreationProperties,
        PureIpDeviceId, PureIpDeviceReceiveFrameMetadata, PureIpWeakDeviceId,
        RecvEthernetFrameMeta,
    },
    ip::{
        IpDeviceConfigurationUpdate, Ipv4DeviceConfigurationUpdate, Ipv6DeviceConfigurationUpdate,
        SlaacConfiguration, TemporarySlaacAddressConfiguration, STABLE_IID_SECRET_KEY_BYTES,
    },
    routes::RawMetric,
    sync::RwLock as CoreRwLock,
};
use rand::Rng as _;

use crate::bindings::{
    devices, interfaces_admin, routes, trace_duration, BindingId, BindingsCtx, Ctx, DeviceId,
    DeviceIdExt as _, Ipv6DeviceConfiguration, Netstack, StaticNetdeviceInfo,
    DEFAULT_INTERFACE_METRIC,
};

/// Like [`DeviceId`], but restricted to netdevice devices.
enum NetdeviceId {
    Ethernet(EthernetDeviceId<BindingsCtx>),
    PureIp(PureIpDeviceId<BindingsCtx>),
}

impl NetdeviceId {
    fn netdevice_info(&self) -> &StaticNetdeviceInfo {
        match self {
            NetdeviceId::Ethernet(eth) => &eth.external_state().netdevice,
            NetdeviceId::PureIp(ip) => &ip.external_state().netdevice,
        }
    }
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
    #[error("{port_type:?} port received unexpected frame type: {frame_type:?}")]
    MismatchedRxFrameType { port_type: PortWireFormat, frame_type: fhardware_network::FrameType },
}

const DEFAULT_BUFFER_LENGTH: usize = 2048;

// TODO(https://fxbug.dev/42052114): Decorate *all* logging with human-readable
// device debug information to disambiguate.
impl NetdeviceWorker {
    pub(crate) async fn new(
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

    pub(crate) fn new_handler(&self) -> DeviceHandler {
        DeviceHandler { inner: self.inner.clone() }
    }

    pub(crate) async fn run(self) -> Result<std::convert::Infallible, Error> {
        let Self { mut ctx, inner: Inner { device: _, session, state }, task } = self;
        // Allow buffer shuttling to happen in other threads.
        let mut task = fuchsia_async::Task::spawn(task).fuse();

        let mut buff = [0u8; DEFAULT_BUFFER_LENGTH];
        let mut last_wifi_drop_log = fasync::Time::INFINITE_PAST;
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

            trace_duration!(c"netdevice::recv");

            let frame_length = rx.len();
            // TODO(https://fxbug.dev/42051635): pass strongly owned buffers down
            // to the stack instead of copying it out.
            rx.read_at(0, &mut buff[..frame_length]).map_err(|e| {
                tracing::error!("failed to read from buffer {:?}", e);
                Error::Client(e)
            })?;

            let Some(id) = id.upgrade() else {
                // This is okay because we hold a weak reference; the device may
                // be removed under us. Note that when the device removal has
                // completed, the interface's `PortHandler` will be uninstalled
                // from the port slab (table of ports for this network device).
                tracing::debug!(
                    "received frame for device after it has been removed; device_id={id:?}"
                );
                // We continue because even though we got frames for a removed
                // device, this network device may have other ports that will
                // receive and handle frames.
                continue;
            };

            match workaround_drop_ssh_over_wlan(
                &id.netdevice_info().handler.device_class,
                &buff[..frame_length],
            ) {
                FilterResult::Drop => {
                    // This being hardcoded in Netstack is possibly surprising.
                    // Log a loud warning with some throttling to warn users.
                    let now = fasync::Time::now();
                    if now - last_wifi_drop_log >= zx::Duration::from_seconds(5) {
                        tracing::warn!(
                            "Dropping frame destined to TCP port 22 on WiFi interface. \
                            See https://fxbug.dev/42084174."
                        );
                        last_wifi_drop_log = now;
                    }
                    continue;
                }
                FilterResult::Accept => (),
            }
            let buf = packet::Buf::new(&mut buff[..frame_length], ..);
            let frame_type = rx.frame_type().map_err(Error::Client)?;
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
                                port_type: PortWireFormat::Ethernet,
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
                                port_type: PortWireFormat::Ip,
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

#[derive(Debug, Eq, PartialEq)]
enum FilterResult {
    Accept,
    Drop,
}

const SSH_PORT: u16 = 22;

/// Implements a hardcoded filter to drop all incoming TCP frames with
/// destination port 22 on WiFi interfaces because we don't have a filtering
/// engine yet. This allows us to test Netstack3 on some devices without
/// incurring the security risk of remote access before we have the filtering
/// engine in place.
///
/// TODO(https://fxbug.dev/42084174): Remove this and replace with real filtering.
fn workaround_drop_ssh_over_wlan(
    device_class: &fhardware_network::DeviceClass,
    buffer: &[u8],
) -> FilterResult {
    use packet::ParsablePacket;
    use packet_formats::{
        error::{IpParseError, ParseError, ParseResult},
        ethernet::{EtherType, EthernetFrame, EthernetFrameLengthCheck},
        ip::{IpProto, Ipv4Proto, Ipv6Proto},
        ipv4::{Ipv4Header as _, Ipv4PacketRaw},
        ipv6::{ExtHdrParseError, Ipv6PacketRaw},
        tcp::TcpSegmentRaw,
    };

    match device_class {
        fhardware_network::DeviceClass::Wlan | fhardware_network::DeviceClass::WlanAp => {}
        fhardware_network::DeviceClass::Ppp
        | fhardware_network::DeviceClass::Bridge
        | fhardware_network::DeviceClass::Virtual
        | fhardware_network::DeviceClass::Ethernet => return FilterResult::Accept,
    }
    // Attempt to parse the buffer as loosely as we can, that means we're as
    // strict as possible here in dropping SSH. We just need to parse deep
    // enough to get to the TCP header.
    fn extract_tcp_ports(data: &[u8]) -> ParseResult<Option<(u16, u16)>> {
        let mut bv = &data[..];
        let ethernet = EthernetFrame::parse(&mut bv, EthernetFrameLengthCheck::NoCheck)?;
        let tcp_hdr = match ethernet.ethertype().ok_or(ParseError::Format)? {
            EtherType::Ipv4 => {
                let ipv4 = Ipv4PacketRaw::parse(&mut bv, ())
                    .map_err(|_: IpParseError<_>| ParseError::Format)?;

                match ipv4.proto() {
                    Ipv4Proto::Proto(IpProto::Tcp) => (),
                    _ => return Ok(None),
                };
                TcpSegmentRaw::parse(&mut ipv4.body().into_inner(), ())?.flow_header()
            }
            EtherType::Ipv6 => {
                let ipv6 = Ipv6PacketRaw::parse(&mut bv, ())
                    .map_err(|_: IpParseError<_>| ParseError::Format)?;
                let (body, proto) =
                    ipv6.body_proto().map_err(|_: ExtHdrParseError| ParseError::Format)?;

                match proto {
                    Ipv6Proto::Proto(IpProto::Tcp) => (),
                    _ => return Ok(None),
                };
                TcpSegmentRaw::parse(&mut body.into_inner(), ())?.flow_header()
            }
            EtherType::Arp | EtherType::Other(_) => return Ok(None),
        };
        Ok(Some(tcp_hdr.src_dst()))
    }

    match extract_tcp_ports(buffer) {
        Ok(Some((_src, dst))) => {
            if dst == SSH_PORT {
                FilterResult::Drop
            } else {
                FilterResult::Accept
            }
        }
        Ok(None) => FilterResult::Accept,
        Result::<_, ParseError>::Err(_) => {
            // Just pass packets that are not parsable in.
            FilterResult::Accept
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
            fuchsia_async::Task<()>,
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
                tracing::warn!("not installing port with invalid wire format: {:?}", e);
                Error::ConfigurationNotSupported
            },
        )?;

        let netdevice_client::client::PortStatus { flags, mtu } =
            status_stream.try_next().await?.ok_or_else(|| Error::PortClosed)?;
        let phy_up = flags.contains(fhardware_network::StatusFlags::ONLINE);
        let netdevice_client::client::PortBaseInfo {
            port_class: device_class,
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
        let Netstack { interfaces_event_sink, neighbor_event_sink, ctx } = ns;

        // Check if there already exists an interface with this name.
        // Interface names are unique.
        let name = name
            .map(|name| {
                if let Some(_device_info) = ctx.bindings_ctx().devices.get_device_by_name(&name) {
                    return Err(Error::DuplicateName(name));
                };
                Ok(name)
            })
            .transpose()?;

        let binding_id = ctx.bindings_ctx().devices.alloc_new_id();

        let name = name.unwrap_or_else(|| match wire_format {
            PortWireFormat::Ethernet => format!("eth{}", binding_id),
            PortWireFormat::Ip => format!("ip{}", binding_id),
        });

        let static_netdevice_info = devices::StaticNetdeviceInfo {
            handler: PortHandler {
                id: binding_id,
                port_id: port,
                inner: self.inner.clone(),
                device_class: device_class.clone(),
                wire_format,
            },
        };
        let dynamic_netdevice_info_builder = |mtu: Mtu| devices::DynamicNetdeviceInfo {
            phy_up,
            common_info: devices::DynamicCommonInfo {
                mtu,
                admin_enabled: false,
                events: crate::bindings::create_interface_event_producer(
                    interfaces_event_sink,
                    binding_id,
                    crate::bindings::InterfaceProperties {
                        name: name.clone(),
                        device_class: fnet_interfaces::DeviceClass::Device(device_class),
                    },
                ),
                control_hook: control_hook,
                addresses: HashMap::new(),
            },
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
                    dynamic_info: CoreRwLock::new(dynamic_netdevice_info_builder(max_frame_size)),
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

        let binding_id = core_id.bindings_id().id;
        let external_state = core_id.external_state();
        let devices::StaticCommonInfo { tx_notifier, authorization_token: _ } =
            external_state.static_common_info();
        let task =
            crate::bindings::devices::spawn_tx_task(&tx_notifier, ctx.clone(), core_id.clone());
        netstack3_core::for_any_device_id!(DeviceId, DeviceProvider, D, &core_id, device => {
            ctx.api().transmit_queue::<D>().set_configuration(
                device,
                netstack3_core::device::TransmitQueueConfiguration::Fifo,
            );
        });
        add_initial_routes(ctx.bindings_ctx(), &core_id).await;

        // TODO(https://fxbug.dev/42148800): Use a different secret key (not this
        // one) to generate stable opaque interface identifiers.
        let mut secret_key = [0; STABLE_IID_SECRET_KEY_BYTES];
        ctx.rng().fill(&mut secret_key);

        let ip_config = IpDeviceConfigurationUpdate {
            ip_enabled: Some(false),
            forwarding_enabled: Some(false),
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
        let _: Ipv4DeviceConfigurationUpdate = ctx
            .api()
            .device_ip::<Ipv4>()
            .update_configuration(&core_id, Ipv4DeviceConfigurationUpdate { ip_config })
            .unwrap();

        tracing::info!("created interface {:?}", core_id);
        ctx.bindings_ctx().devices.add_device(binding_id, core_id);

        Ok((binding_id, status_stream, task))
    }
}

/// Connect to the Port's `MacAddressingProxy`, and fetch the MAC address.
async fn get_mac(
    port_proxy: &fhardware_network::PortProxy,
    port: &netdevice_client::Port,
) -> Result<(UnicastAddr<Mac>, fhardware_network::MacAddressingProxy), Error> {
    let (mac_proxy, mac_server) =
        fidl::endpoints::create_proxy::<fhardware_network::MacAddressingMarker>()
            .map_err(Error::SystemResource)?;
    let () = port_proxy.get_mac(mac_server).map_err(Error::CantConnectToPort)?;

    let mac_addr = {
        let fnet::MacAddress { octets } = mac_proxy.get_unicast_address().await.map_err(|e| {
            tracing::warn!("failed to get unicast address, sending not supported: {:?}", e);
            Error::ConfigurationNotSupported
        })?;
        let mac = net_types::ethernet::Mac::new(octets);
        net_types::UnicastAddr::new(mac).ok_or_else(|| {
            tracing::warn!("{} is not a valid unicast address", mac);
            Error::MacNotUnicast { mac, port: *port }
        })?
    };
    // Always set the interface to multicast promiscuous mode because we
    // don't really plumb through multicast filtering.
    // TODO(https://fxbug.dev/42136929): Remove this when multicast filtering
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
        routes::Change::RouteOp(
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
        routes::Change::RouteOp(
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
    device_class: fhardware_network::DeviceClass,
    wire_format: PortWireFormat,
}

#[derive(thiserror::Error, Debug)]
pub(crate) enum SendError {
    #[error("no buffers available")]
    NoTxBuffers,
    #[error("device error: {0}")]
    Device(#[from] netdevice_client::Error),
}

impl PortHandler {
    pub(crate) fn device_class(&self) -> fhardware_network::DeviceClass {
        self.device_class
    }

    pub(crate) async fn attach(&self) -> Result<(), netdevice_client::Error> {
        let Self { port_id, inner: Inner { session, .. }, wire_format, .. } = self;
        session.attach(*port_id, wire_format.frame_types()).await
    }

    pub(crate) async fn detach(&self) -> Result<(), netdevice_client::Error> {
        let Self { port_id, inner: Inner { session, .. }, .. } = self;
        session.detach(*port_id).await
    }

    pub(crate) fn send(
        &self,
        frame: &[u8],
        frame_type: fhardware_network::FrameType,
    ) -> Result<(), SendError> {
        trace_duration!(c"netdevice::send");

        let Self { port_id, inner: Inner { session, .. }, .. } = self;
        // NB: We currently send on a dispatcher, so we can't wait for new
        // buffers to become available. If that ends up being the long term way
        // of enqueuing outgoing buffers we might want to fix this impedance
        // mismatch here.
        let mut tx =
            session.alloc_tx_buffer(frame.len()).now_or_never().ok_or(SendError::NoTxBuffers)??;
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
}

impl std::fmt::Debug for PortHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let Self { id, port_id, inner: _, device_class, wire_format } = self;
        f.debug_struct("PortHandler")
            .field("id", id)
            .field("port_id", port_id)
            .field("device_class", device_class)
            .field("wire_format", wire_format)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ip_test_macro::ip_test;
    use net_declare::net_mac;
    use net_types::Witness as _;
    use packet::{Buf, InnerPacketBuilder as _, Serializer as _};
    use packet_formats::{
        ethernet::EthernetFrameBuilder,
        ip::{IpExt, IpPacketBuilder as _, IpProto},
        tcp::TcpSegmentBuilder,
    };
    use std::num::NonZeroU16;

    #[ip_test]
    fn wlan_ssh_workaround<I: Ip + IpExt>() {
        fn make_packet<I: IpExt>(ip_proto: IpProto, dst: u16) -> Buf<Vec<u8>> {
            let addr = I::LOOPBACK_ADDRESS.get();
            (&[1u8, 2, 3, 4])
                .into_serializer()
                .encapsulate(TcpSegmentBuilder::new(
                    addr,
                    addr,
                    NonZeroU16::new(1234).unwrap(),
                    NonZeroU16::new(dst).unwrap(),
                    1,
                    None,
                    1024,
                ))
                .encapsulate(I::PacketBuilder::new(addr, addr, 1, ip_proto.into()))
                .encapsulate(EthernetFrameBuilder::new(
                    net_mac!("02:00:00:00:00:01"),
                    net_mac!("02:00:00:00:00:02"),
                    I::ETHER_TYPE,
                    0,
                ))
                .serialize_vec_outer()
                .unwrap()
                .unwrap_b()
        }

        // Check that we're only dropping for WLAN.
        for device_class in [
            fhardware_network::DeviceClass::Virtual,
            fhardware_network::DeviceClass::Ethernet,
            fhardware_network::DeviceClass::Ppp,
            fhardware_network::DeviceClass::Bridge,
        ] {
            assert_matches!(
                workaround_drop_ssh_over_wlan(
                    &device_class,
                    make_packet::<I>(IpProto::Tcp, SSH_PORT).as_ref(),
                ),
                FilterResult::Accept
            );
        }

        for (proto, dst, expect) in [
            (IpProto::Udp, 1234, FilterResult::Accept),
            (IpProto::Udp, SSH_PORT, FilterResult::Accept),
            (IpProto::Tcp, 1234, FilterResult::Accept),
            (IpProto::Tcp, SSH_PORT, FilterResult::Drop),
        ] {
            assert_eq!(
                workaround_drop_ssh_over_wlan(
                    &fhardware_network::DeviceClass::Wlan,
                    make_packet::<I>(proto, dst).as_ref()
                ),
                expect,
                "proto={proto:?}, dst={dst}"
            );
        }
    }
}
