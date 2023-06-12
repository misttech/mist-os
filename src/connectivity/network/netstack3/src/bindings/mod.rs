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

mod debug_fidl_worker;
mod devices;
mod filter_worker;
mod interfaces_admin;
mod interfaces_watcher;
mod netdevice_worker;
mod root_fidl_worker;
mod routes_fidl_worker;
mod socket;
mod stack_fidl_worker;
mod timers;
mod util;
mod verifier_worker;

use std::{
    borrow::Cow,
    collections::HashMap,
    convert::TryFrom as _,
    ffi::CStr,
    future::Future,
    num::NonZeroU16,
    ops::Deref,
    // TODO(https://fxbug.dev/125488): Use RC types exported from Core, after
    // we make sockets reference-backed.
    sync::{Arc, Weak},
    time::Duration,
};

use either::{self, Either};
use fidl::endpoints::{DiscoverableProtocolMarker, RequestStream};
use fidl_fuchsia_net_interfaces_admin as fnet_interfaces_admin;
use fidl_fuchsia_net_stack as fidl_net_stack;
use fuchsia_async as fasync;
use fuchsia_component::server::{ServiceFs, ServiceFsDir};
use fuchsia_zircon as zx;
use futures::{
    channel::mpsc, future::BoxFuture, lock::Mutex as AsyncMutex, FutureExt as _, SinkExt as _,
    StreamExt as _, TryStreamExt as _,
};
use packet::{Buf, BufferMut};
use rand::{rngs::OsRng, CryptoRng, RngCore};
use tracing::{debug, error, info};
use util::{ConversionContext, IntoFidl as _};

use devices::{
    BindingId, DeviceSpecificInfo, Devices, DynamicCommonInfo, DynamicNetdeviceInfo, LoopbackInfo,
    NetdeviceInfo, StaticCommonInfo,
};
use interfaces_watcher::{InterfaceEventProducer, InterfaceProperties, InterfaceUpdate};
use timers::TimerDispatcher;

use net_declare::net_subnet_v4;
use net_types::{
    ip::{
        AddrSubnet, AddrSubnetEither, Ip, IpAddr, IpAddress, IpVersion, Ipv4, Ipv4Addr, Ipv6, Mtu,
        Subnet,
    },
    SpecifiedAddr,
};
use netstack3_core::{
    add_ip_addr_subnet,
    context::{
        CounterContext, EventContext, InstantContext, RngContext, TimerContext, TracingContext,
    },
    data_structures::id_map::{EntryKey, IdMap},
    device::{
        loopback::LoopbackDeviceId, update_ipv4_configuration, update_ipv6_configuration, DeviceId,
        DeviceLayerEventDispatcher, DeviceLayerStateTypes, DeviceSendFrameError, EthernetDeviceId,
    },
    error::NetstackError,
    handle_timer,
    ip::{
        device::{
            slaac::SlaacConfiguration,
            state::{Ipv6DeviceConfiguration, Lifetime},
            IpDeviceConfigurationUpdate, IpDeviceEvent, Ipv4DeviceConfigurationUpdate,
            Ipv6DeviceConfigurationUpdate, RemovedReason,
        },
        icmp,
        types::RawMetric,
        IpExt,
    },
    sync::Mutex as CoreMutex,
    transport::tcp,
    transport::udp,
    NonSyncContext, SyncCtx, TimerId,
};

#[derive(Default, Clone)]
pub(crate) struct Ctx {
    // `non_sync_ctx` is the first member so all strongly-held references are
    // dropped before primary references held in `sync_ctx` are dropped. Note
    // that dropping a primary reference while holding onto strong references
    // will cause a panic. See `netstack3_core::sync::PrimaryRc` for more
    // details.
    pub non_sync_ctx: BindingsNonSyncCtxImpl,
    pub sync_ctx: Arc<SyncCtx<BindingsNonSyncCtxImpl>>,
}

use crate::bindings::{
    interfaces_watcher::AddressPropertiesUpdate, util::TryIntoFidlWithContext as _,
};

/// Extends the methods available to [`DeviceId`].
trait DeviceIdExt {
    /// Returns the state associated with devices.
    fn external_state(&self) -> DeviceSpecificInfo<'_>;
}

impl DeviceIdExt for DeviceId<BindingsNonSyncCtxImpl> {
    fn external_state(&self) -> DeviceSpecificInfo<'_> {
        match self {
            DeviceId::Ethernet(d) => DeviceSpecificInfo::Netdevice(d.external_state()),
            DeviceId::Loopback(d) => DeviceSpecificInfo::Loopback(d.external_state()),
        }
    }
}

/// Extends the methods available to [`Lifetime`].
trait LifetimeExt {
    /// Converts `self` to `zx::Time`.
    fn into_zx_time(self) -> zx::Time;
}

impl LifetimeExt for Lifetime<StackTime> {
    fn into_zx_time(self) -> zx::Time {
        match self {
            Lifetime::Finite(StackTime(time)) => time.into_zx(),
            Lifetime::Infinite => zx::Time::INFINITE,
        }
    }
}

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

/// Default routing metric for newly created interfaces, if unspecified.
///
/// The value is currently kept in sync with the Netstack2 implementation.
const DEFAULT_INTERFACE_METRIC: u32 = 100;

type UdpSockets = socket::datagram::SocketCollectionPair<socket::datagram::Udp>;

#[derive(Default)]
pub(crate) struct BindingsNonSyncCtxImplInner {
    rng: RngImpl,
    timers: timers::TimerDispatcher<TimerId<BindingsNonSyncCtxImpl>>,
    devices: Devices<DeviceId<BindingsNonSyncCtxImpl>>,
    udp_sockets: UdpSockets,
    tcp_v4_listeners: CoreMutex<IdMap<crate::bindings::socket::stream::ListenerState>>,
    tcp_v6_listeners: CoreMutex<IdMap<crate::bindings::socket::stream::ListenerState>>,
    tcp_v4_connections:
        CoreMutex<IdMap<(crate::bindings::socket::stream::ConnectionStatus, Weak<zx::Socket>)>>,
    tcp_v6_connections:
        CoreMutex<IdMap<(crate::bindings::socket::stream::ConnectionStatus, Weak<zx::Socket>)>>,
    route_update_dispatcher: CoreMutex<routes_fidl_worker::RouteUpdateDispatcher>,
}

/// Provides an implementation of [`NonSyncContext`].
#[derive(Clone, Default)]
pub(crate) struct BindingsNonSyncCtxImpl(Arc<BindingsNonSyncCtxImplInner>);

impl Deref for BindingsNonSyncCtxImpl {
    type Target = BindingsNonSyncCtxImplInner;

    fn deref(&self) -> &BindingsNonSyncCtxImplInner {
        let Self(this) = self;
        this.deref()
    }
}

impl AsRef<timers::TimerDispatcher<TimerId<BindingsNonSyncCtxImpl>>> for BindingsNonSyncCtxImpl {
    fn as_ref(&self) -> &TimerDispatcher<TimerId<BindingsNonSyncCtxImpl>> {
        &self.timers
    }
}

impl AsRef<Devices<DeviceId<BindingsNonSyncCtxImpl>>> for BindingsNonSyncCtxImpl {
    fn as_ref(&self) -> &Devices<DeviceId<BindingsNonSyncCtxImpl>> {
        &self.devices
    }
}

impl AsRef<UdpSockets> for BindingsNonSyncCtxImpl {
    fn as_ref(&self) -> &UdpSockets {
        &self.udp_sockets
    }
}

impl timers::TimerHandler<TimerId<BindingsNonSyncCtxImpl>> for Ctx {
    fn handle_expired_timer(&mut self, timer: TimerId<BindingsNonSyncCtxImpl>) {
        let Ctx { sync_ctx, non_sync_ctx } = self;
        handle_timer(sync_ctx, non_sync_ctx, timer)
    }

    fn get_timer_dispatcher(
        &mut self,
    ) -> &timers::TimerDispatcher<TimerId<BindingsNonSyncCtxImpl>> {
        self.non_sync_ctx.as_ref()
    }
}

impl timers::TimerContext<TimerId<BindingsNonSyncCtxImpl>> for Netstack {
    type Handler = Ctx;
    fn handler(&self) -> Ctx {
        self.ctx.clone()
    }
}

impl<D> ConversionContext for D
where
    D: AsRef<Devices<DeviceId<BindingsNonSyncCtxImpl>>>,
{
    fn get_core_id(&self, binding_id: BindingId) -> Option<DeviceId<BindingsNonSyncCtxImpl>> {
        self.as_ref().get_core_id(binding_id)
    }

    fn get_binding_id(&self, core_id: DeviceId<BindingsNonSyncCtxImpl>) -> BindingId {
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

impl TracingContext for BindingsNonSyncCtxImpl {
    type DurationScope = fuchsia_trace::DurationScope<'static>;

    fn duration(&self, name: &'static CStr) -> fuchsia_trace::DurationScope<'static> {
        fuchsia_trace::duration(cstr::cstr!("net"), name, &[])
    }
}

/// Convenience wrapper around the [`fuchsia_trace::duration`] macro that always
/// uses the "net" tracing category.
///
/// [`fuchsia_trace::duration`] uses RAII to begin and end the duration by tying
/// the scope of the duration to the lifetime of the object it returns. This
/// macro encapsulates that logic such that the trace duration will end when the
/// scope in which the macro is called ends.
macro_rules! trace_duration {
    ($name:expr) => {
        fuchsia_trace::duration!("net", $name);
    };
}

pub(crate) use trace_duration;

#[derive(Default)]
pub struct RngImpl(CoreMutex<OsRng>);

impl RngCore for &'_ RngImpl {
    fn next_u32(&mut self) -> u32 {
        let RngImpl(this) = self;
        this.lock().next_u32()
    }

    fn next_u64(&mut self) -> u64 {
        let RngImpl(this) = self;
        this.lock().next_u64()
    }

    fn fill_bytes(&mut self, dest: &mut [u8]) {
        let RngImpl(this) = self;
        this.lock().fill_bytes(dest)
    }

    fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), rand::Error> {
        let RngImpl(this) = self;
        this.lock().try_fill_bytes(dest)
    }
}

impl CryptoRng for &'_ RngImpl where OsRng: CryptoRng {}

impl RngContext for BindingsNonSyncCtxImpl {
    type Rng<'a> = &'a RngImpl where Self: 'a;

    fn rng(&mut self) -> &RngImpl {
        &self.rng
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

impl DeviceLayerStateTypes for BindingsNonSyncCtxImpl {
    type LoopbackDeviceState = LoopbackInfo;
    type EthernetDeviceState = NetdeviceInfo;
}

impl DeviceLayerEventDispatcher for BindingsNonSyncCtxImpl {
    fn wake_rx_task(&mut self, device: &LoopbackDeviceId<Self>) {
        let LoopbackInfo { static_common_info: _, dynamic_common_info: _, rx_notifier } =
            device.external_state();
        rx_notifier.schedule()
    }

    fn wake_tx_task(&mut self, device: &DeviceId<BindingsNonSyncCtxImpl>) {
        unimplemented!("TODO(https://fxbug.dev/105615): wake_tx_task(_, {})", device);
    }

    fn send_frame(
        &mut self,
        device: &EthernetDeviceId<Self>,
        frame: Buf<Vec<u8>>,
    ) -> Result<(), DeviceSendFrameError<Buf<Vec<u8>>>> {
        let state = device.external_state();
        let enabled = state.with_dynamic_info(
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
            state.handler.send(frame.as_ref()).unwrap_or_else(|e| {
                tracing::warn!("failed to send frame to {:?}: {:?}", state.handler, e)
            })
        }

        Ok(())
    }
}

impl<I: icmp::IcmpIpExt> icmp::IcmpContext<I> for BindingsNonSyncCtxImpl {
    fn receive_icmp_error(
        &mut self,
        _conn: icmp::IcmpConnId<I>,
        _seq_num: u16,
        _err: I::ErrorCode,
    ) {
        unimplemented!("TODO(https://fxbug.dev/125482): implement ICMP sockets")
    }
}

impl<I: icmp::IcmpIpExt, B: BufferMut> icmp::BufferIcmpContext<I, B> for BindingsNonSyncCtxImpl {
    fn receive_icmp_echo_reply(
        &mut self,
        _conn: icmp::IcmpConnId<I>,
        _src_ip: I::Addr,
        _dst_ip: I::Addr,
        _id: u16,
        _seq_num: u16,
        _data: B,
    ) {
        unimplemented!("TODO(https://fxbug.dev/125482): implement ICMP sockets")
    }
}

impl<I> udp::NonSyncContext<I> for BindingsNonSyncCtxImpl
where
    I: socket::datagram::SocketCollectionIpExt<socket::datagram::Udp> + icmp::IcmpIpExt,
{
    fn receive_icmp_error(&mut self, id: udp::SocketId<I>, err: I::ErrorCode) {
        I::with_collection_mut(self, |c| c.receive_icmp_error(id, err))
    }
}

impl<I, B: BufferMut> udp::BufferNonSyncContext<I, B> for BindingsNonSyncCtxImpl
where
    I: socket::datagram::SocketCollectionIpExt<socket::datagram::Udp> + IpExt,
{
    fn receive_udp(
        &mut self,
        id: udp::SocketId<I>,
        dst_ip: <I>::Addr,
        src_addr: (<I>::Addr, Option<NonZeroU16>),
        body: &B,
    ) {
        I::with_collection_mut(self, |c| c.receive_udp(id, dst_ip, src_addr, body))
    }
}

impl<I: Ip> EventContext<IpDeviceEvent<DeviceId<BindingsNonSyncCtxImpl>, I, StackTime>>
    for BindingsNonSyncCtxImpl
{
    fn on_event(&mut self, event: IpDeviceEvent<DeviceId<BindingsNonSyncCtxImpl>, I, StackTime>) {
        match event {
            IpDeviceEvent::AddressAdded { device, addr, state, valid_until } => {
                let valid_until = valid_until.into_zx_time();

                self.notify_interface_update(
                    &device,
                    InterfaceUpdate::AddressAdded {
                        addr: addr.into(),
                        assignment_state: state,
                        valid_until,
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
            IpDeviceEvent::AddressPropertiesChanged { device, addr, valid_until } => self
                .notify_interface_update(
                    &device,
                    InterfaceUpdate::AddressPropertiesChanged {
                        addr: addr.to_ip_addr(),
                        update: AddressPropertiesUpdate { valid_until: valid_until.into_zx_time() },
                    },
                ),
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
        let (entry, is_route_present, route_update_fn) = match event {
            netstack3_core::ip::IpLayerEvent::RouteAdded(entry) => {
                (entry, true, Either::Left(routes_fidl_worker::RoutingTableUpdate::<I>::RouteAdded))
            }
            netstack3_core::ip::IpLayerEvent::RouteRemoved(entry) => (
                entry,
                false,
                Either::Right(routes_fidl_worker::RoutingTableUpdate::<I>::RouteRemoved),
            ),
        };

        // Maybe publish the event to the interface watchers (which only care
        // about changes to the default route).
        let netstack3_core::ip::types::Entry { subnet, device, gateway: _, metric: _ } = &entry;
        if subnet.prefix() == 0 {
            self.notify_interface_update(
                device,
                InterfaceUpdate::DefaultRouteChanged {
                    version: I::VERSION,
                    has_default_route: is_route_present,
                },
            )
        }

        // Publish the event to the routes watchers.
        let installed_route =
            entry.try_into_fidl_with_ctx(self).expect("failed to convert route to FIDL");
        let route_update = either::for_both!(route_update_fn, f => f(installed_route));
        self.route_update_dispatcher
            .lock()
            .notify(route_update)
            .expect("failed to notify route update dispatcher");
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
    Ctx { sync_ctx, non_sync_ctx }: &mut Ctx,
    id: BindingId,
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

    let ip_config =
        Some(IpDeviceConfigurationUpdate { ip_enabled: Some(should_enable), ..Default::default() });

    let _: Ipv4DeviceConfigurationUpdate = netstack3_core::device::update_ipv4_configuration(
        sync_ctx,
        non_sync_ctx,
        &core_id,
        Ipv4DeviceConfigurationUpdate { ip_config, ..Default::default() },
    )
    .expect("changing ip_enabled should never fail");
    let _: Ipv6DeviceConfigurationUpdate = netstack3_core::device::update_ipv6_configuration(
        sync_ctx,
        non_sync_ctx,
        &core_id,
        Ipv6DeviceConfigurationUpdate { ip_config, ..Default::default() },
    )
    .expect("changing ip_enabled should never fail");

    Ok(())
}

fn add_loopback_ip_addrs<NonSyncCtx: NonSyncContext>(
    sync_ctx: &SyncCtx<NonSyncCtx>,
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
    sync_ctx: &SyncCtx<NonSyncCtx>,
    non_sync_ctx: &mut NonSyncCtx,
    loopback: &DeviceId<NonSyncCtx>,
) -> Result<(), netstack3_core::ip::forwarding::AddRouteError> {
    use netstack3_core::ip::types::{AddableEntry, AddableEntryEither, AddableMetric};
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

/// The netstack.
///
/// Provides the entry point for creating a netstack to be served as a
/// component.
#[derive(Clone)]
pub struct Netstack {
    ctx: Ctx,
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
                AsyncMutex::new(()),
            )
            .map(|f| f.map(|f| f()).unwrap_or(())),
        )
    }

    fn add_loopback(
        &self,
    ) -> (
        futures::channel::oneshot::Sender<fnet_interfaces_admin::InterfaceRemovedReason>,
        BindingId,
        fasync::Task<()>,
    ) {
        let mut ctx = self.ctx.clone();
        let Ctx { sync_ctx, non_sync_ctx } = &mut ctx;

        // Add and initialize the loopback interface with the IPv4 and IPv6
        // loopback addresses and on-link routes to the loopback subnets.
        let devices: &Devices<_> = non_sync_ctx.as_ref();
        let (control_sender, control_receiver) =
            interfaces_admin::OwnedControlHandle::new_channel();
        let loopback_rx_notifier = Default::default();

        let loopback = netstack3_core::device::add_loopback_device_with_state(
            sync_ctx,
            DEFAULT_LOOPBACK_MTU,
            RawMetric(DEFAULT_INTERFACE_METRIC),
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

                LoopbackInfo {
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
                }
            },
        )
        .expect("error adding loopback device");

        let LoopbackInfo {
            static_common_info: StaticCommonInfo { binding_id, name: _ },
            dynamic_common_info: _,
            rx_notifier,
        } = loopback.external_state();
        crate::bindings::devices::spawn_rx_task(rx_notifier, self, &loopback);
        let binding_id = *binding_id;
        let loopback: DeviceId<_> = loopback.into();
        devices.add_device(binding_id, loopback.clone());

        // Don't need DAD and IGMP/MLD on loopback.
        let ip_config = Some(IpDeviceConfigurationUpdate {
            ip_enabled: Some(true),
            forwarding_enabled: Some(false),
            gmp_enabled: Some(false),
        });
        let _: Ipv4DeviceConfigurationUpdate = update_ipv4_configuration(
            sync_ctx,
            non_sync_ctx,
            &loopback,
            Ipv4DeviceConfigurationUpdate { ip_config },
        )
        .unwrap();
        let _: Ipv6DeviceConfigurationUpdate = update_ipv6_configuration(
            sync_ctx,
            non_sync_ctx,
            &loopback,
            Ipv6DeviceConfigurationUpdate {
                dad_transmits: Some(None),
                max_router_solicitations: Some(None),
                slaac_config: Some(SlaacConfiguration {
                    enable_stable_addresses: true,
                    temporary_address_configuration: None,
                }),
                ip_config,
            },
        )
        .unwrap();
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

    fn socket_info_getter(
        &self,
    ) -> impl Fn() -> BoxFuture<'static, Result<fuchsia_inspect::Inspector, anyhow::Error>>
           + Clone
           + Sync
           + Send
           + 'static {
        /// Convert a [`tcp::socket::SocketId`] into a unique integer.
        ///
        /// Guarantees that no two unique `SocketId`s (even for different IP
        /// versions) will have have the same output value.
        fn transform_id<I: Ip>(id: tcp::socket::SocketId<I>) -> usize {
            let (index, variant) = match id {
                tcp::socket::SocketId::Unbound(id) => (id.get_key_index(), 0),
                tcp::socket::SocketId::Bound(id) => (id.get_key_index(), 1),
                tcp::socket::SocketId::Listener(id) => (id.get_key_index(), 2),
                tcp::socket::SocketId::Connection(id) => (id.get_key_index(), 3),
            };

            let unique_for_ip_version = index * 4 + variant;
            2 * unique_for_ip_version
                + match I::VERSION {
                    IpVersion::V4 => 0,
                    IpVersion::V6 => 1,
                }
        }

        struct Visitor(fuchsia_inspect::Inspector);
        impl tcp::socket::InfoVisitor for &'_ mut Visitor {
            type VisitResult = ();
            fn visit<I: Ip>(
                self,
                per_socket: impl Iterator<Item = tcp::socket::SocketStats<I>>,
            ) -> Self::VisitResult {
                let Visitor(inspector) = self;
                for socket in per_socket {
                    let tcp::socket::SocketStats { id, local, remote } = socket;
                    inspector.root().record_child(format!("{}", transform_id(id)), |node| {
                        node.record_string("TransportProtocol", "TCP");
                        node.record_string(
                            "NetworkProtocol",
                            match I::VERSION {
                                IpVersion::V4 => "IPv4",
                                IpVersion::V6 => "IPv6",
                            },
                        );
                        fn format_addr_port<'a, A: IpAddress, S: Deref<Target = A>>(
                            (addr, port): (S, NonZeroU16),
                        ) -> Cow<'a, str> {
                            Cow::Owned(format!("{}:{}", *addr, port))
                        }
                        node.record_string(
                            "LocalAddress",
                            local.map_or("[NOT BOUND]".into(), |(addr, port)| {
                                format_addr_port((
                                    &addr.map_or(I::UNSPECIFIED_ADDRESS, |a| *a),
                                    port,
                                ))
                            }),
                        );
                        node.record_string(
                            "RemoteAddress",
                            remote.map_or("[NOT CONNECTED]".into(), format_addr_port),
                        )
                    })
                }
            }
        }
        let ctx = self.ctx.clone();
        move || {
            let mut ctx = ctx.clone();
            Box::pin(async move {
                #[derive(thiserror::Error, Debug)]
                #[error("Netstack is not running")]
                struct NetstackNotRunningError;

                let Ctx { sync_ctx, non_sync_ctx: _ } = &mut ctx;

                let mut visitor = Visitor(fuchsia_inspect::Inspector::new(
                    fuchsia_inspect::InspectorConfig::default(),
                ));
                tcp::socket::with_info::<Ipv4, _, _>(sync_ctx, &mut visitor);
                tcp::socket::with_info::<Ipv6, _, _>(sync_ctx, &mut visitor);

                let Visitor(inspector) = visitor;
                Ok(inspector)
            })
        }
    }
}

enum Service {
    DebugDiagnostics(fidl::endpoints::ServerEnd<fidl_fuchsia_net_debug::DiagnosticsMarker>),
    DebugInterfaces(fidl_fuchsia_net_debug::InterfacesRequestStream),
    Filter(fidl_fuchsia_net_filter::FilterRequestStream),
    Interfaces(fidl_fuchsia_net_interfaces::StateRequestStream),
    InterfacesAdmin(fidl_fuchsia_net_interfaces_admin::InstallerRequestStream),
    PacketSocket(fidl_fuchsia_posix_socket_packet::ProviderRequestStream),
    RawSocket(fidl_fuchsia_posix_socket_raw::ProviderRequestStream),
    RootInterfaces(fidl_fuchsia_net_root::InterfacesRequestStream),
    RoutesState(fidl_fuchsia_net_routes::StateRequestStream),
    RoutesStateV4(fidl_fuchsia_net_routes::StateV4RequestStream),
    RoutesStateV6(fidl_fuchsia_net_routes::StateV6RequestStream),
    Socket(fidl_fuchsia_posix_socket::ProviderRequestStream),
    Stack(fidl_fuchsia_net_stack::StackRequestStream),
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

        info!("serving netstack with netstack3");

        let Self { netstack, interfaces_worker, interfaces_watcher_sink } = self;

        // Start servicing timers.
        netstack.ctx.clone().non_sync_ctx.timers.spawn(netstack.clone());

        // The Sender is unused because Loopback should never be canceled.
        let (_sender, _, loopback_interface_control_task): (
            futures::channel::oneshot::Sender<_>,
            BindingId,
            _,
        ) = netstack.add_loopback();

        let interfaces_worker_task = fuchsia_async::Task::spawn(async move {
            let result = interfaces_worker.run().await;
            // The worker is not expected to end for the lifetime of the stack.
            panic!("interfaces worker finished unexpectedly {:?}", result);
        });

        let mut fs = ServiceFs::new();
        let _: &mut ServiceFsDir<'_, _> = fs
            .dir("svc")
            .add_service_connector(Service::DebugDiagnostics)
            .add_fidl_service(Service::DebugInterfaces)
            .add_fidl_service(Service::Stack)
            .add_fidl_service(Service::Socket)
            .add_fidl_service(Service::PacketSocket)
            .add_fidl_service(Service::RawSocket)
            .add_fidl_service(Service::RootInterfaces)
            .add_fidl_service(Service::RoutesState)
            .add_fidl_service(Service::RoutesStateV4)
            .add_fidl_service(Service::RoutesStateV6)
            .add_fidl_service(Service::Interfaces)
            .add_fidl_service(Service::InterfacesAdmin)
            .add_fidl_service(Service::Filter)
            .add_fidl_service(Service::Verifier);

        let inspector = fuchsia_inspect::component::inspector();
        inspect_runtime::serve(inspector, &mut fs).expect("failed to serve inspect");
        let _socket_info =
            inspector.root().create_lazy_child("Socket Info", netstack.socket_info_getter());

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
        // It is unclear why we need to wrap the `for_each_concurrent` call with
        // `async move { ... }` but it seems like we do. Without this, the
        // `Future` returned by this function fails to implement `Send` with the
        // same issue reported in https://github.com/rust-lang/rust/issues/64552.
        //
        // TODO(https://github.com/rust-lang/rust/issues/64552): Remove this
        // workaround.
        let work_items_fut = async move {
            work_items
                .for_each_concurrent(None, |wi| async {
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
                            socket
                                .serve_with(|rs| socket::packet::serve(netstack.ctx.clone(), rs))
                                .await
                        }
                        WorkItem::Incoming(Service::RawSocket(socket)) => {
                            socket.serve_with(|rs| socket::raw::serve(rs)).await
                        }
                        WorkItem::Incoming(Service::RootInterfaces(root_interfaces)) => {
                            root_interfaces
                                .serve_with(|rs| {
                                    root_fidl_worker::serve_interfaces(netstack.clone(), rs)
                                })
                                .await
                        }
                        WorkItem::Incoming(Service::RoutesState(rs)) => {
                            routes_fidl_worker::serve_state(rs, netstack.clone()).await
                        }
                        WorkItem::Incoming(Service::RoutesStateV4(rs)) => {
                            routes_fidl_worker::serve_state_v4(rs, netstack.clone()).await
                        }
                        WorkItem::Incoming(Service::RoutesStateV6(rs)) => {
                            routes_fidl_worker::serve_state_v6(rs, netstack.clone()).await
                        }
                        WorkItem::Incoming(Service::Interfaces(interfaces)) => {
                            interfaces
                                .serve_with(|rs| {
                                    interfaces_watcher::serve(rs, interfaces_watcher_sink.clone())
                                })
                                .await
                        }
                        WorkItem::Incoming(Service::InterfacesAdmin(installer)) => {
                            tracing::debug!(
                                "serving {}",
                                fidl_fuchsia_net_interfaces_admin::InstallerMarker::PROTOCOL_NAME
                            );
                            interfaces_admin::serve(netstack.clone(), installer)
                                .map_err(anyhow::Error::from)
                                .forward(task_sink.clone().sink_map_err(anyhow::Error::from))
                                .await
                                .unwrap_or_else(|e| {
                                    tracing::warn!(
                                "error serving {}: {:?}",
                                fidl_fuchsia_net_interfaces_admin::InstallerMarker::PROTOCOL_NAME,
                                e
                            )
                                })
                        }
                        WorkItem::Incoming(Service::DebugInterfaces(debug_interfaces)) => {
                            debug_interfaces
                                .serve_with(|rs| {
                                    debug_fidl_worker::serve_interfaces(netstack.clone(), rs)
                                })
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
                })
                .await
        };

        let ((), (), ()) =
            futures::join!(work_items_fut, interfaces_worker_task, loopback_interface_control_task);
        debug!("Services stream finished");
        Ok(())
    }
}
