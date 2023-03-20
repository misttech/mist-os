// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Netstack3 bindings.
//!
//! This module provides Fuchsia bindings for the [`netstack3_core`] crate.

#[macro_use]
mod macros;

#[cfg(test)]
mod integration_tests;

mod context;
mod debug_fidl_worker;
mod devices;
mod filter_worker;
mod interfaces_admin;
mod interfaces_watcher;
mod netdevice_worker;
mod socket;
mod stack_fidl_worker;
mod timers;
mod util;
mod verifier_worker;

use std::collections::HashMap;
use std::convert::TryFrom as _;
use std::future::Future;
use std::num::NonZeroU16;
use std::ops::{Deref as _, DerefMut as _};
use std::sync::Arc;
use std::time::Duration;

use fidl::endpoints::{DiscoverableProtocolMarker, RequestStream};
use fidl_fuchsia_net_interfaces_admin as fnet_interfaces_admin;
use fidl_fuchsia_net_stack as fidl_net_stack;
use fuchsia_async as fasync;
use fuchsia_component::server::{ServiceFs, ServiceFsDir};
use fuchsia_zircon as zx;
use futures::{
    channel::mpsc, lock::Mutex, FutureExt as _, SinkExt as _, StreamExt as _, TryStreamExt as _,
};
use log::{debug, error};
use packet::{Buf, BufferMut};
use packet_formats::icmp::{IcmpEchoReply, IcmpMessage, IcmpUnusedCode};
use rand::rngs::OsRng;
use util::{ConversionContext, IntoFidl as _};

use devices::{
    BindingId, DeviceSpecificInfo, Devices, DynamicCommonInfo, DynamicNetdeviceInfo, LoopbackInfo,
    StaticCommonInfo,
};
use interfaces_watcher::{InterfaceEventProducer, InterfaceProperties, InterfaceUpdate};
use timers::TimerDispatcher;

use net_declare::net_subnet_v4;
use net_types::{
    ip::{AddrSubnet, AddrSubnetEither, Ip, IpAddr, IpAddress, Ipv4, Ipv4Addr, Ipv6, Mtu, Subnet},
    SpecifiedAddr,
};
use netstack3_core::{
    add_ip_addr_subnet,
    context::{CounterContext, EventContext, InstantContext, RngContext, TimerContext},
    data_structures::id_map::IdMap,
    device::{
        loopback::LoopbackDeviceId, DeviceId, DeviceLayerEventDispatcher, DeviceSendFrameError,
        EthernetDeviceId,
    },
    error::NetstackError,
    handle_timer,
    ip::{
        device::{
            slaac::SlaacConfiguration,
            state::{IpDeviceConfiguration, Ipv4DeviceConfiguration, Ipv6DeviceConfiguration},
            IpDeviceEvent, RemovedReason,
        },
        icmp, IpExt,
    },
    transport::udp,
    Ctx, NonSyncContext, SyncCtx, TimerId,
};

const LOOPBACK_NAME: &'static str = "lo";

/// Default MTU for loopback.
///
/// This value is also the default value used on Linux. As of writing:
///
/// ```shell
/// $ ip link show dev lo
/// 1: lo: <LOOPBACK,UP,LOWER_UP> mtu 65536 qdisc noqueue state UNKNOWN mode DEFAULT group default qlen 1000
///     link/loopback 00:00:00:00:00:00 brd 00:00:00:00:00:00
/// ```
const DEFAULT_LOOPBACK_MTU: Mtu = Mtu::new(65536);

/// Subnet for the IPv4 Limited Broadcast Address.
const IPV4_LIMITED_BROADCAST_SUBNET: Subnet<Ipv4Addr> = net_subnet_v4!("255.255.255.255/32");

/// The default "Low Priority" metric to use for default routes.
///
/// The value is currently kept in sync with the Netstack2 implementation.
const DEFAULT_LOW_PRIORITY_METRIC: u32 = 99999;

type IcmpEchoSockets = socket::datagram::SocketCollectionPair<socket::datagram::IcmpEcho>;
type UdpSockets = socket::datagram::SocketCollectionPair<socket::datagram::Udp>;

/// Provides an implementation of [`NonSyncContext`].
#[derive(Default)]
pub(crate) struct BindingsNonSyncCtxImpl {
    rng: OsRng,
    timers: timers::TimerDispatcher<TimerId<BindingsNonSyncCtxImpl>>,
    devices: Devices<DeviceId<BindingsNonSyncCtxImpl>>,
    icmp_echo_sockets: IcmpEchoSockets,
    udp_sockets: UdpSockets,
    tcp_v4_listeners: IdMap<zx::Socket>,
    tcp_v6_listeners: IdMap<zx::Socket>,
}

impl AsRef<timers::TimerDispatcher<TimerId<BindingsNonSyncCtxImpl>>> for BindingsNonSyncCtxImpl {
    fn as_ref(&self) -> &TimerDispatcher<TimerId<BindingsNonSyncCtxImpl>> {
        &self.timers
    }
}

impl AsMut<timers::TimerDispatcher<TimerId<BindingsNonSyncCtxImpl>>> for BindingsNonSyncCtxImpl {
    fn as_mut(&mut self) -> &mut TimerDispatcher<TimerId<BindingsNonSyncCtxImpl>> {
        &mut self.timers
    }
}

impl AsRef<Devices<DeviceId<BindingsNonSyncCtxImpl>>> for BindingsNonSyncCtxImpl {
    fn as_ref(&self) -> &Devices<DeviceId<BindingsNonSyncCtxImpl>> {
        &self.devices
    }
}

impl AsMut<Devices<DeviceId<BindingsNonSyncCtxImpl>>> for BindingsNonSyncCtxImpl {
    fn as_mut(&mut self) -> &mut Devices<DeviceId<BindingsNonSyncCtxImpl>> {
        &mut self.devices
    }
}

impl<'a> context::Lockable<'a, Ctx<BindingsNonSyncCtxImpl>> for Netstack {
    type Guard = futures::lock::MutexGuard<'a, Ctx<BindingsNonSyncCtxImpl>>;
    type Fut = futures::lock::MutexLockFuture<'a, Ctx<BindingsNonSyncCtxImpl>>;
    fn lock(&'a self) -> Self::Fut {
        self.ctx.lock()
    }
}

impl AsRef<IcmpEchoSockets> for BindingsNonSyncCtxImpl {
    fn as_ref(&self) -> &IcmpEchoSockets {
        &self.icmp_echo_sockets
    }
}

impl AsMut<IcmpEchoSockets> for BindingsNonSyncCtxImpl {
    fn as_mut(&mut self) -> &mut IcmpEchoSockets {
        &mut self.icmp_echo_sockets
    }
}

impl AsRef<UdpSockets> for BindingsNonSyncCtxImpl {
    fn as_ref(&self) -> &UdpSockets {
        &self.udp_sockets
    }
}

impl AsMut<UdpSockets> for BindingsNonSyncCtxImpl {
    fn as_mut(&mut self) -> &mut UdpSockets {
        &mut self.udp_sockets
    }
}

impl timers::TimerHandler<TimerId<BindingsNonSyncCtxImpl>> for Ctx<BindingsNonSyncCtxImpl> {
    fn handle_expired_timer(&mut self, timer: TimerId<BindingsNonSyncCtxImpl>) {
        let Ctx { sync_ctx, non_sync_ctx } = self;
        handle_timer(sync_ctx, non_sync_ctx, timer)
    }

    fn get_timer_dispatcher(
        &mut self,
    ) -> &mut timers::TimerDispatcher<TimerId<BindingsNonSyncCtxImpl>> {
        self.non_sync_ctx.as_mut()
    }
}

impl timers::TimerContext<TimerId<BindingsNonSyncCtxImpl>> for Netstack {
    type Handler = Ctx<BindingsNonSyncCtxImpl>;
}

impl<D> ConversionContext for D
where
    D: AsRef<Devices<DeviceId<BindingsNonSyncCtxImpl>>>,
{
    fn get_core_id(&self, binding_id: u64) -> Option<DeviceId<BindingsNonSyncCtxImpl>> {
        self.as_ref().get_core_id(binding_id)
    }

    fn get_binding_id(&self, core_id: DeviceId<BindingsNonSyncCtxImpl>) -> u64 {
        core_id.external_state().static_common_info().binding_id
    }
}

/// A thin wrapper around `fuchsia_async::Time` that implements `core::Instant`.
#[derive(PartialEq, Eq, PartialOrd, Ord, Copy, Clone, Debug)]
pub(crate) struct StackTime(fasync::Time);

impl netstack3_core::Instant for StackTime {
    fn duration_since(&self, earlier: StackTime) -> Duration {
        assert!(self.0 >= earlier.0);
        // guaranteed not to panic because the assertion ensures that the
        // difference is non-negative, and all non-negative i64 values are also
        // valid u64 values
        Duration::from_nanos(u64::try_from(self.0.into_nanos() - earlier.0.into_nanos()).unwrap())
    }

    fn checked_add(&self, duration: Duration) -> Option<StackTime> {
        Some(StackTime(fasync::Time::from_nanos(
            self.0.into_nanos().checked_add(i64::try_from(duration.as_nanos()).ok()?)?,
        )))
    }

    fn checked_sub(&self, duration: Duration) -> Option<StackTime> {
        Some(StackTime(fasync::Time::from_nanos(
            self.0.into_nanos().checked_sub(i64::try_from(duration.as_nanos()).ok()?)?,
        )))
    }
}

impl InstantContext for BindingsNonSyncCtxImpl {
    type Instant = StackTime;

    fn now(&self) -> StackTime {
        StackTime(fasync::Time::now())
    }
}

impl CounterContext for BindingsNonSyncCtxImpl {}

impl RngContext for BindingsNonSyncCtxImpl {
    type Rng = OsRng;

    fn rng(&self) -> &OsRng {
        &self.rng
    }

    fn rng_mut(&mut self) -> &mut OsRng {
        &mut self.rng
    }
}

impl TimerContext<TimerId<BindingsNonSyncCtxImpl>> for BindingsNonSyncCtxImpl {
    fn schedule_timer_instant(
        &mut self,
        time: StackTime,
        id: TimerId<BindingsNonSyncCtxImpl>,
    ) -> Option<StackTime> {
        self.timers.schedule_timer(id, time)
    }

    fn cancel_timer(&mut self, id: TimerId<BindingsNonSyncCtxImpl>) -> Option<StackTime> {
        self.timers.cancel_timer(&id)
    }

    fn cancel_timers_with<F: FnMut(&TimerId<BindingsNonSyncCtxImpl>) -> bool>(&mut self, f: F) {
        self.timers.cancel_timers_with(f);
    }

    fn scheduled_instant(&self, id: TimerId<BindingsNonSyncCtxImpl>) -> Option<StackTime> {
        self.timers.scheduled_time(&id)
    }
}

impl DeviceLayerEventDispatcher for BindingsNonSyncCtxImpl {
    type DeviceState = DeviceSpecificInfo;

    fn wake_rx_task(&mut self, device: &LoopbackDeviceId<Self::Instant, Self::DeviceState>) {
        match device.external_state().deref() {
            DeviceSpecificInfo::Netdevice(_) => {
                unreachable!("only loopback supports RX queues")
            }
            DeviceSpecificInfo::Loopback(LoopbackInfo {
                static_common_info: _,
                dynamic_common_info: _,
                rx_notifier,
            }) => rx_notifier.schedule(),
        }
    }

    fn wake_tx_task(&mut self, device: &DeviceId<BindingsNonSyncCtxImpl>) {
        unimplemented!("TODO(https://fxbug.dev/105615): wake_tx_task(_, {})", device);
    }

    fn send_frame(
        &mut self,
        device: &EthernetDeviceId<Self::Instant, Self::DeviceState>,
        frame: Buf<Vec<u8>>,
    ) -> Result<(), DeviceSendFrameError<Buf<Vec<u8>>>> {
        match device.external_state().deref() {
            DeviceSpecificInfo::Netdevice(i) => {
                let enabled = i.with_dynamic_info(
                    |DynamicNetdeviceInfo {
                         phy_up,
                         common_info:
                             DynamicCommonInfo {
                                 admin_enabled,
                                 mtu: _,
                                 events: _,
                                 control_hook: _,
                                 addresses: _,
                             },
                     }| { *admin_enabled && *phy_up },
                );
                if enabled {
                    i.handler.send(frame.as_ref()).unwrap_or_else(|e| {
                        log::warn!("failed to send frame to {:?}: {:?}", i.handler, e)
                    })
                }
            }
            DeviceSpecificInfo::Loopback(LoopbackInfo { .. }) => {
                unreachable!("loopback must not send packets out of the node")
            }
        }

        Ok(())
    }
}

impl<I> icmp::IcmpContext<I> for BindingsNonSyncCtxImpl
where
    I: socket::datagram::SocketCollectionIpExt<socket::datagram::IcmpEcho> + icmp::IcmpIpExt,
{
    fn receive_icmp_error(&mut self, conn: icmp::IcmpConnId<I>, seq_num: u16, err: I::ErrorCode) {
        I::get_collection_mut(self).receive_icmp_error(conn, seq_num, err)
    }
}

impl<I, B: BufferMut> icmp::BufferIcmpContext<I, B> for BindingsNonSyncCtxImpl
where
    I: socket::datagram::SocketCollectionIpExt<socket::datagram::IcmpEcho> + icmp::IcmpIpExt,
    IcmpEchoReply: for<'a> IcmpMessage<I, &'a [u8], Code = IcmpUnusedCode>,
{
    fn receive_icmp_echo_reply(
        &mut self,
        conn: icmp::IcmpConnId<I>,
        src_ip: I::Addr,
        dst_ip: I::Addr,
        id: u16,
        seq_num: u16,
        data: B,
    ) {
        I::get_collection_mut(self).receive_icmp_echo_reply(conn, src_ip, dst_ip, id, seq_num, data)
    }
}

impl<I> udp::NonSyncContext<I> for BindingsNonSyncCtxImpl
where
    I: socket::datagram::SocketCollectionIpExt<socket::datagram::Udp> + icmp::IcmpIpExt,
{
    fn receive_icmp_error(&mut self, id: udp::BoundId<I>, err: I::ErrorCode) {
        I::get_collection_mut(self).receive_icmp_error(id, err)
    }
}

impl<I, B: BufferMut> udp::BufferNonSyncContext<I, B> for BindingsNonSyncCtxImpl
where
    I: socket::datagram::SocketCollectionIpExt<socket::datagram::Udp> + IpExt,
{
    fn receive_udp_from_conn(
        &mut self,
        conn: udp::ConnId<I>,
        src_ip: I::Addr,
        src_port: NonZeroU16,
        body: &B,
    ) {
        I::get_collection_mut(self).receive_udp_from_conn(conn, src_ip, src_port, body)
    }

    fn receive_udp_from_listen(
        &mut self,
        listener: udp::ListenerId<I>,
        src_ip: I::Addr,
        dst_ip: I::Addr,
        src_port: Option<NonZeroU16>,
        body: &B,
    ) {
        I::get_collection_mut(self)
            .receive_udp_from_listen(listener, src_ip, dst_ip, src_port, body)
    }
}

impl<I: Ip> EventContext<IpDeviceEvent<DeviceId<BindingsNonSyncCtxImpl>, I>>
    for BindingsNonSyncCtxImpl
{
    fn on_event(&mut self, event: IpDeviceEvent<DeviceId<BindingsNonSyncCtxImpl>, I>) {
        match event {
            IpDeviceEvent::AddressAdded { device, addr, state } => {
                self.notify_interface_update(
                    &device,
                    InterfaceUpdate::AddressAdded {
                        addr: addr.into(),
                        assignment_state: state,
                        valid_until: zx::Time::INFINITE,
                    },
                );
                self.notify_address_update(&device, addr.addr().into(), state);
            }
            IpDeviceEvent::AddressRemoved { device, addr, reason } => {
                self.notify_interface_update(
                    &device,
                    InterfaceUpdate::AddressRemoved(addr.to_ip_addr()),
                );
                match reason {
                    RemovedReason::Manual => (),
                    RemovedReason::DadFailed => self.notify_dad_failed(&device, addr.into()),
                }
            }
            IpDeviceEvent::AddressStateChanged { device, addr, state } => {
                self.notify_interface_update(
                    &device,
                    InterfaceUpdate::AddressAssignmentStateChanged {
                        addr: addr.to_ip_addr(),
                        new_state: state,
                    },
                );
                self.notify_address_update(&device, addr.into(), state);
            }
            IpDeviceEvent::EnabledChanged { device, ip_enabled } => {
                self.notify_interface_update(&device, InterfaceUpdate::OnlineChanged(ip_enabled))
            }
        };
    }
}

impl<I: Ip> EventContext<netstack3_core::ip::IpLayerEvent<DeviceId<BindingsNonSyncCtxImpl>, I>>
    for BindingsNonSyncCtxImpl
{
    fn on_event(
        &mut self,
        event: netstack3_core::ip::IpLayerEvent<DeviceId<BindingsNonSyncCtxImpl>, I>,
    ) {
        let (device, subnet, route_is_present) =
            match event {
                netstack3_core::ip::IpLayerEvent::RouteAdded(
                    netstack3_core::ip::types::Entry { device, subnet, gateway: _, metric: _ },
                ) => (device, subnet, true),
                netstack3_core::ip::IpLayerEvent::RouteRemoved(
                    netstack3_core::ip::types::Entry { device, subnet, gateway: _, metric: _ },
                ) => (device, subnet, false),
            };
        // Interfaces watchers only care about the default route.
        if subnet.prefix() != 0 || subnet.network() != I::UNSPECIFIED_ADDRESS {
            return;
        }
        self.notify_interface_update(
            &device,
            InterfaceUpdate::DefaultRouteChanged {
                version: I::VERSION,
                has_default_route: route_is_present,
            },
        );
    }
}

impl EventContext<netstack3_core::ip::device::dad::DadEvent<DeviceId<BindingsNonSyncCtxImpl>>>
    for BindingsNonSyncCtxImpl
{
    fn on_event(
        &mut self,
        event: netstack3_core::ip::device::dad::DadEvent<DeviceId<BindingsNonSyncCtxImpl>>,
    ) {
        match event {
            netstack3_core::ip::device::dad::DadEvent::AddressAssigned { device, addr } => self
                .on_event(
                    netstack3_core::ip::device::IpDeviceEvent::<_, Ipv6>::AddressStateChanged {
                        device,
                        addr: addr.into_specified(),
                        state: netstack3_core::ip::device::IpAddressState::Assigned,
                    },
                ),
        }
    }
}

impl
    EventContext<
        netstack3_core::ip::device::route_discovery::Ipv6RouteDiscoveryEvent<
            DeviceId<BindingsNonSyncCtxImpl>,
        >,
    > for BindingsNonSyncCtxImpl
{
    fn on_event(
        &mut self,
        _event: netstack3_core::ip::device::route_discovery::Ipv6RouteDiscoveryEvent<
            DeviceId<BindingsNonSyncCtxImpl>,
        >,
    ) {
        // TODO(https://fxbug.dev/97203): Update forwarding table in response to
        // the event.
    }
}

impl BindingsNonSyncCtxImpl {
    fn notify_interface_update(
        &self,
        device: &DeviceId<BindingsNonSyncCtxImpl>,
        event: InterfaceUpdate,
    ) {
        device
            .external_state()
            .with_common_info(|i| i.events.notify(event).expect("interfaces worker closed"));
    }

    /// Notify `AddressStateProvider.WatchAddressAssignmentState` watchers.
    fn notify_address_update(
        &self,
        device: &DeviceId<BindingsNonSyncCtxImpl>,
        address: SpecifiedAddr<IpAddr>,
        state: netstack3_core::ip::device::IpAddressState,
    ) {
        // Note that not all addresses have an associated watcher (e.g. loopback
        // address & autoconfigured SLAAC addresses).
        device.external_state().with_common_info(|i| {
            if let Some(address_info) = i.addresses.get(&address) {
                address_info
                    .assignment_state_sender
                    .unbounded_send(state.into_fidl())
                    .expect("assignment state receiver unexpectedly disconnected");
            }
        })
    }

    fn notify_dad_failed(
        &mut self,
        device: &DeviceId<BindingsNonSyncCtxImpl>,
        address: SpecifiedAddr<IpAddr>,
    ) {
        device.external_state().with_common_info_mut(|i| {
            if let Some(address_info) = i.addresses.get_mut(&address) {
                let devices::FidlWorkerInfo { worker: _, cancelation_sender } =
                    &mut address_info.address_state_provider;
                if let Some(sender) = cancelation_sender.take() {
                    sender
                        .send(fnet_interfaces_admin::AddressRemovalReason::DadFailed)
                        .expect("assignment state receiver unexpectedly disconnected");
                }
            }
        })
    }
}

fn set_interface_enabled(
    Ctx { sync_ctx, non_sync_ctx }: &mut Ctx<crate::bindings::BindingsNonSyncCtxImpl>,
    id: u64,
    should_enable: bool,
) -> Result<(), fidl_net_stack::Error> {
    let core_id = non_sync_ctx.devices.get_core_id(id).ok_or(fidl_net_stack::Error::NotFound)?;

    let dev_enabled = match core_id.external_state() {
        DeviceSpecificInfo::Netdevice(i) => i.with_dynamic_info(
            |DynamicNetdeviceInfo {
                 phy_up,
                 common_info:
                     DynamicCommonInfo {
                         admin_enabled,
                         mtu: _,
                         events: _,
                         control_hook: _,
                         addresses: _,
                     },
             }| *phy_up && *admin_enabled,
        ),
        DeviceSpecificInfo::Loopback(i) => i.with_dynamic_info(
            |DynamicCommonInfo {
                 admin_enabled,
                 mtu: _,
                 events: _,
                 control_hook: _,
                 addresses: _,
             }| { *admin_enabled },
        ),
    };

    if should_enable {
        // We want to enable the interface, but its device is considered
        // disabled so we do nothing further.
        //
        // This can happen when the interface was set to be administratively up
        // but the phy is down.
        if !dev_enabled {
            return Ok(());
        }
    } else {
        assert!(!dev_enabled, "caller attemped to disable an interface that is considered enabled");
    }

    netstack3_core::device::update_ipv4_configuration(sync_ctx, non_sync_ctx, &core_id, |config| {
        config.ip_config.ip_enabled = should_enable;
    });
    netstack3_core::device::update_ipv6_configuration(sync_ctx, non_sync_ctx, &core_id, |config| {
        config.ip_config.ip_enabled = should_enable;
    });

    Ok(())
}

fn add_loopback_ip_addrs<NonSyncCtx: NonSyncContext>(
    sync_ctx: &mut SyncCtx<NonSyncCtx>,
    non_sync_ctx: &mut NonSyncCtx,
    loopback: &DeviceId<NonSyncCtx>,
) -> Result<(), NetstackError> {
    for addr_subnet in [
        AddrSubnetEither::V4(
            AddrSubnet::from_witness(Ipv4::LOOPBACK_ADDRESS, Ipv4::LOOPBACK_SUBNET.prefix())
                .expect("error creating IPv4 loopback AddrSub"),
        ),
        AddrSubnetEither::V6(
            AddrSubnet::from_witness(Ipv6::LOOPBACK_ADDRESS, Ipv6::LOOPBACK_SUBNET.prefix())
                .expect("error creating IPv6 loopback AddrSub"),
        ),
    ] {
        add_ip_addr_subnet(sync_ctx, non_sync_ctx, loopback, addr_subnet)?
    }
    Ok(())
}

/// Adds the IPv4 and IPv6 Loopback and multicast subnet routes, and the IPv4
/// limited broadcast subnet route.
///
/// Note that if an error is encountered while installing a route, any routes
/// that were successfully installed prior to the error will not be removed.
fn add_loopback_routes<NonSyncCtx: NonSyncContext>(
    sync_ctx: &mut SyncCtx<NonSyncCtx>,
    non_sync_ctx: &mut NonSyncCtx,
    loopback: &DeviceId<NonSyncCtx>,
) -> Result<(), netstack3_core::ip::forwarding::AddRouteError> {
    use netstack3_core::ip::types::{AddableEntry, AddableEntryEither, AddableMetric, RawMetric};
    for entry in [
        AddableEntryEither::from(AddableEntry::without_gateway(
            Ipv4::LOOPBACK_SUBNET,
            loopback.clone(),
            AddableMetric::MetricTracksInterface,
        )),
        AddableEntryEither::from(AddableEntry::without_gateway(
            Ipv6::LOOPBACK_SUBNET,
            loopback.clone(),
            AddableMetric::MetricTracksInterface,
        )),
        AddableEntryEither::from(AddableEntry::without_gateway(
            Ipv4::MULTICAST_SUBNET,
            loopback.clone(),
            AddableMetric::MetricTracksInterface,
        )),
        AddableEntryEither::from(AddableEntry::without_gateway(
            Ipv6::MULTICAST_SUBNET,
            loopback.clone(),
            AddableMetric::MetricTracksInterface,
        )),
        AddableEntryEither::from(AddableEntry::without_gateway(
            IPV4_LIMITED_BROADCAST_SUBNET,
            loopback.clone(),
            AddableMetric::ExplicitMetric(RawMetric(DEFAULT_LOW_PRIORITY_METRIC)),
        )),
    ] {
        netstack3_core::add_route(sync_ctx, non_sync_ctx, entry)?;
    }
    Ok(())
}

type NetstackContext = Arc<Mutex<Ctx<BindingsNonSyncCtxImpl>>>;

/// The netstack.
///
/// Provides the entry point for creating a netstack to be served as a
/// component.
#[derive(Clone)]
pub struct Netstack {
    ctx: NetstackContext,
    interfaces_event_sink: interfaces_watcher::WorkerInterfaceSink,
}

/// Contains the information needed to start serving a network stack over FIDL.
pub struct NetstackSeed {
    netstack: Netstack,
    interfaces_worker: interfaces_watcher::Worker,
    interfaces_watcher_sink: interfaces_watcher::WorkerWatcherSink,
}

impl Default for NetstackSeed {
    fn default() -> Self {
        let (interfaces_worker, interfaces_watcher_sink, interfaces_event_sink) =
            interfaces_watcher::Worker::new();
        Self {
            netstack: Netstack { ctx: Default::default(), interfaces_event_sink },
            interfaces_worker,
            interfaces_watcher_sink,
        }
    }
}

impl Netstack {
    fn create_interface_event_producer(
        &self,
        id: BindingId,
        properties: InterfaceProperties,
    ) -> InterfaceEventProducer {
        self.interfaces_event_sink
            .add_interface(id, properties)
            .expect("interface worker not running")
    }

    fn spawn_interface_control(
        &self,
        id: BindingId,
        stop_receiver: futures::channel::oneshot::Receiver<
            fnet_interfaces_admin::InterfaceRemovedReason,
        >,
        control_receiver: futures::channel::mpsc::Receiver<interfaces_admin::OwnedControlHandle>,
        removable: bool,
    ) -> fuchsia_async::Task<()> {
        fuchsia_async::Task::spawn(
            interfaces_admin::run_interface_control(
                self.ctx.clone(),
                id,
                stop_receiver,
                control_receiver,
                removable,
            )
            .map(|f| f.map(|f| f()).unwrap_or(())),
        )
    }

    async fn add_loopback(
        &self,
    ) -> (
        futures::channel::oneshot::Sender<fnet_interfaces_admin::InterfaceRemovedReason>,
        BindingId,
        fasync::Task<()>,
    ) {
        let mut ctx = self.ctx.lock().await;
        let Ctx { sync_ctx, non_sync_ctx } = ctx.deref_mut();

        // Add and initialize the loopback interface with the IPv4 and IPv6
        // loopback addresses and on-link routes to the loopback subnets.
        let devices: &mut Devices<_> = non_sync_ctx.as_mut();
        let (control_sender, control_receiver) =
            interfaces_admin::OwnedControlHandle::new_channel();
        let loopback_rx_notifier = Default::default();

        let loopback = netstack3_core::device::add_loopback_device_with_state(
            sync_ctx,
            DEFAULT_LOOPBACK_MTU,
            || {
                let binding_id = devices.alloc_new_id();

                let events = self.create_interface_event_producer(
                    binding_id,
                    InterfaceProperties {
                        name: LOOPBACK_NAME.to_string(),
                        device_class: fidl_fuchsia_net_interfaces::DeviceClass::Loopback(
                            fidl_fuchsia_net_interfaces::Empty {},
                        ),
                    },
                );
                events
                    .notify(InterfaceUpdate::OnlineChanged(true))
                    .expect("interfaces worker not running");

                DeviceSpecificInfo::Loopback(LoopbackInfo {
                    static_common_info: StaticCommonInfo {
                        binding_id,
                        name: LOOPBACK_NAME.to_string(),
                    },
                    dynamic_common_info: DynamicCommonInfo {
                        mtu: DEFAULT_LOOPBACK_MTU,
                        admin_enabled: true,
                        events,
                        control_hook: control_sender,
                        addresses: HashMap::new(),
                    }
                    .into(),
                    rx_notifier: loopback_rx_notifier,
                })
                .into()
            },
        )
        .expect("error adding loopback device");

        let binding_id = match loopback.external_state() {
            DeviceSpecificInfo::Loopback(LoopbackInfo {
                static_common_info: StaticCommonInfo { binding_id, name: _ },
                dynamic_common_info: _,
                rx_notifier,
            }) => {
                crate::bindings::devices::spawn_rx_task(rx_notifier, self, &loopback);
                *binding_id
            }
            // TODO(https://fxbug.dev/123461): Make this more type-safe.
            e => unreachable!("added loopback device but got external_state={:?}", e),
        };
        let loopback: DeviceId<_> = loopback.into();
        devices.add_device(binding_id, loopback.clone());

        // Don't need DAD and IGMP/MLD on loopback.
        netstack3_core::device::update_ipv4_configuration(
            sync_ctx,
            non_sync_ctx,
            &loopback,
            |config| {
                *config = Ipv4DeviceConfiguration {
                    ip_config: IpDeviceConfiguration { ip_enabled: true, gmp_enabled: false },
                };
            },
        );
        netstack3_core::device::update_ipv6_configuration(
            sync_ctx,
            non_sync_ctx,
            &loopback,
            |config| {
                *config = Ipv6DeviceConfiguration {
                    dad_transmits: None,
                    max_router_solicitations: None,
                    slaac_config: SlaacConfiguration {
                        enable_stable_addresses: true,
                        temporary_address_configuration: None,
                    },
                    ip_config: IpDeviceConfiguration { ip_enabled: true, gmp_enabled: false },
                };
            },
        );
        add_loopback_ip_addrs(sync_ctx, non_sync_ctx, &loopback)
            .expect("error adding loopback addresses");
        add_loopback_routes(sync_ctx, non_sync_ctx, &loopback)
            .expect("error adding loopback routes");

        let (stop_sender, stop_receiver) = futures::channel::oneshot::channel();

        // Loopback interface can't be removed.
        let removable = false;
        let task =
            self.spawn_interface_control(binding_id, stop_receiver, control_receiver, removable);
        (stop_sender, binding_id, task)
    }
}

enum Service {
    Stack(fidl_fuchsia_net_stack::StackRequestStream),
    Socket(fidl_fuchsia_posix_socket::ProviderRequestStream),
    PacketSocket(fidl_fuchsia_posix_socket_packet::ProviderRequestStream),
    RawSocket(fidl_fuchsia_posix_socket_raw::ProviderRequestStream),
    Interfaces(fidl_fuchsia_net_interfaces::StateRequestStream),
    InterfacesAdmin(fidl_fuchsia_net_interfaces_admin::InstallerRequestStream),
    Filter(fidl_fuchsia_net_filter::FilterRequestStream),
    DebugInterfaces(fidl_fuchsia_net_debug::InterfacesRequestStream),
    DebugDiagnostics(fidl::endpoints::ServerEnd<fidl_fuchsia_net_debug::DiagnosticsMarker>),
    Verifier(fidl_fuchsia_update_verify::NetstackVerifierRequestStream),
}

enum WorkItem {
    Incoming(Service),
    Task(fasync::Task<()>),
}

trait RequestStreamExt: RequestStream {
    fn serve_with<F, Fut, E>(self, f: F) -> futures::future::Map<Fut, fn(Result<(), E>) -> ()>
    where
        E: std::error::Error,
        F: FnOnce(Self) -> Fut,
        Fut: Future<Output = Result<(), E>>;
}

impl<D: DiscoverableProtocolMarker, S: RequestStream<Protocol = D>> RequestStreamExt for S {
    fn serve_with<F, Fut, E>(self, f: F) -> futures::future::Map<Fut, fn(Result<(), E>) -> ()>
    where
        E: std::error::Error,
        F: FnOnce(Self) -> Fut,
        Fut: Future<Output = Result<(), E>>,
    {
        f(self).map(|res| res.unwrap_or_else(|err| error!("{} error: {}", D::PROTOCOL_NAME, err)))
    }
}

impl NetstackSeed {
    /// Consumes the netstack and starts serving all the FIDL services it
    /// implements to the outgoing service directory.
    pub async fn serve(self) -> Result<(), anyhow::Error> {
        use anyhow::Context as _;

        debug!("Serving netstack");

        let Self { netstack, interfaces_worker, interfaces_watcher_sink } = self;

        // Start servicing timers.
        netstack.ctx.lock().await.non_sync_ctx.timers.spawn(netstack.clone());

        // The Sender is unused because Loopback should never be canceled.
        let (_sender, _, loopback_interface_control_task): (
            futures::channel::oneshot::Sender<_>,
            BindingId,
            _,
        ) = netstack.add_loopback().await;

        let interfaces_worker_task = fuchsia_async::Task::spawn(async move {
            let result = interfaces_worker.run().await;
            // The worker is not expected to end for the lifetime of the stack.
            panic!("interfaces worker finished unexpectedly {:?}", result);
        });

        let mut fs = ServiceFs::new_local();
        let _: &mut ServiceFsDir<'_, _> = fs
            .dir("svc")
            .add_service_connector(Service::DebugDiagnostics)
            .add_fidl_service(Service::DebugInterfaces)
            .add_fidl_service(Service::Stack)
            .add_fidl_service(Service::Socket)
            .add_fidl_service(Service::PacketSocket)
            .add_fidl_service(Service::RawSocket)
            .add_fidl_service(Service::Interfaces)
            .add_fidl_service(Service::InterfacesAdmin)
            .add_fidl_service(Service::Filter)
            .add_fidl_service(Service::Verifier);

        let services = fs.take_and_serve_directory_handle().context("directory handle")?;

        // Buffer size doesn't matter much, we're just trying to reduce
        // allocations.
        const TASK_CHANNEL_BUFFER_SIZE: usize = 16;
        let (task_sink, task_stream) = mpsc::channel(TASK_CHANNEL_BUFFER_SIZE);
        let work_items = futures::stream::select(
            services.map(WorkItem::Incoming),
            task_stream.map(WorkItem::Task),
        );
        let diagnostics_handler = debug_fidl_worker::DiagnosticsHandler::default();
        let work_items_fut = work_items.for_each_concurrent(None, |wi| async {
            match wi {
                WorkItem::Incoming(Service::Stack(stack)) => {
                    stack
                        .serve_with(|rs| {
                            stack_fidl_worker::StackFidlWorker::serve(netstack.clone(), rs)
                        })
                        .await
                }
                WorkItem::Incoming(Service::Socket(socket)) => {
                    socket.serve_with(|rs| socket::serve(netstack.ctx.clone(), rs)).await
                }
                WorkItem::Incoming(Service::PacketSocket(socket)) => {
                    socket.serve_with(|rs| socket::packet::serve(rs)).await
                }
                WorkItem::Incoming(Service::RawSocket(socket)) => {
                    socket.serve_with(|rs| socket::raw::serve(rs)).await
                }
                WorkItem::Incoming(Service::Interfaces(interfaces)) => {
                    interfaces
                        .serve_with(|rs| {
                            interfaces_watcher::serve(rs, interfaces_watcher_sink.clone())
                        })
                        .await
                }
                WorkItem::Incoming(Service::InterfacesAdmin(installer)) => {
                    log::debug!(
                        "serving {}",
                        fidl_fuchsia_net_interfaces_admin::InstallerMarker::PROTOCOL_NAME
                    );
                    interfaces_admin::serve(netstack.clone(), installer)
                        .map_err(anyhow::Error::from)
                        .forward(task_sink.clone().sink_map_err(anyhow::Error::from))
                        .await
                        .unwrap_or_else(|e| {
                            log::warn!(
                                "error serving {}: {:?}",
                                fidl_fuchsia_net_interfaces_admin::InstallerMarker::PROTOCOL_NAME,
                                e
                            )
                        })
                }
                WorkItem::Incoming(Service::DebugInterfaces(debug_interfaces)) => {
                    debug_interfaces
                        .serve_with(|rs| debug_fidl_worker::serve_interfaces(netstack.clone(), rs))
                        .await
                }
                WorkItem::Incoming(Service::DebugDiagnostics(debug_diagnostics)) => {
                    diagnostics_handler.serve_diagnostics(debug_diagnostics).await
                }
                WorkItem::Incoming(Service::Filter(filter)) => {
                    filter.serve_with(|rs| filter_worker::serve(rs)).await
                }
                WorkItem::Incoming(Service::Verifier(verifier)) => {
                    verifier.serve_with(|rs| verifier_worker::serve(rs)).await
                }
                WorkItem::Task(task) => task.await,
            }
        });

        let ((), (), ()) =
            futures::join!(work_items_fut, interfaces_worker_task, loopback_interface_control_task);
        debug!("Services stream finished");
        Ok(())
    }
}
