// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use core::{convert::Infallible as Never, num::NonZeroU16};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Weak,
};

use fidl_fuchsia_net as fidl_net;
use fidl_fuchsia_net_interfaces as fnet_interfaces;
use fidl_fuchsia_net_routes as fnet_routes;
use fidl_fuchsia_net_routes_ext as fnet_routes_ext;
use fidl_fuchsia_net_stack as fidl_net_stack;
use fidl_fuchsia_posix as fposix;
use fidl_fuchsia_posix_socket as fposix_socket;
use futures::{
    stream::{FusedStream, Stream},
    task::AtomicWaker,
};
use net_types::{
    ethernet::Mac,
    ip::{
        AddrSubnetEither, AddrSubnetError, GenericOverIp, Ip, IpAddr, IpAddress,
        IpInvariant as IpInv, Ipv4Addr, Ipv6Addr, SubnetEither, SubnetError,
    },
    AddrAndZone, MulticastAddr, ZonedAddr,
};
use net_types::{SpecifiedAddr, Witness};
use netstack3_core::{
    device::{DeviceId, WeakDeviceId},
    error::{ExistsError, NetstackError, NotFoundError},
    ip::{
        forwarding::AddRouteError,
        types::{
            AddableEntry, AddableEntryEither, AddableMetric, Entry, EntryEither, Metric, RawMetric,
        },
    },
    socket::datagram::{MulticastInterfaceSelector, MulticastMembershipInterfaceSelector},
};

use crate::bindings::{
    devices::BindingId,
    socket::{IntoErrno, IpSockAddrExt, SockAddr},
    BindingsNonSyncCtxImpl,
};

/// The value used to specify that a `ForwardingEntry.metric` is unset, and the
/// entry's metric should track the interface's routing metric.
const UNSET_FORWARDING_ENTRY_METRIC: u32 = 0;

/// A signal used between Core and Bindings, whenever Bindings receive a
/// notification by the protocol (Core), it should kick the associated task
/// to do work.
#[derive(Debug)]
struct NeedsData {
    ready: AtomicBool,
    waker: AtomicWaker,
}

impl Default for NeedsData {
    fn default() -> NeedsData {
        NeedsData { ready: AtomicBool::new(false), waker: AtomicWaker::new() }
    }
}

impl NeedsData {
    fn poll_ready(&self, cx: &mut std::task::Context<'_>) -> std::task::Poll<()> {
        self.waker.register(cx.waker());
        match self.ready.compare_exchange(true, false, Ordering::AcqRel, Ordering::Relaxed) {
            Ok(_) => std::task::Poll::Ready(()),
            Err(_) => std::task::Poll::Pending,
        }
    }

    fn schedule(&self) {
        self.ready.store(true, Ordering::Release);
        self.waker.wake();
    }
}

impl Drop for NeedsData {
    fn drop(&mut self) {
        self.schedule()
    }
}

/// The notifier side of the underlying signal struct, it is meant to be held
/// by the Core side and schedule signals to be received by the Bindings.
#[derive(Default, Debug, Clone)]
pub struct NeedsDataNotifier {
    inner: Arc<NeedsData>,
}

impl NeedsDataNotifier {
    pub(crate) fn schedule(&self) {
        self.inner.schedule()
    }

    pub(crate) fn watcher(&self) -> NeedsDataWatcher {
        NeedsDataWatcher { inner: Arc::downgrade(&self.inner) }
    }
}

/// The receiver side of the underlying signal struct, it is meant to be held
/// by the Bindings side. It is a [`Stream`] of wakeups scheduled by the Core
/// and upon receiving those wakeups, Bindings should perform any blocked
/// work.
#[derive(Debug)]
pub(crate) struct NeedsDataWatcher {
    inner: Weak<NeedsData>,
}

impl Stream for NeedsDataWatcher {
    type Item = ();

    fn poll_next(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        let this = self.get_mut();
        match this.inner.upgrade() {
            None => std::task::Poll::Ready(None),
            Some(needs_data) => {
                std::task::Poll::Ready(Some(std::task::ready!(needs_data.poll_ready(cx))))
            }
        }
    }
}

/// The watcher terminates when the underlying has been dropped by Core.
impl FusedStream for NeedsDataWatcher {
    fn is_terminated(&self) -> bool {
        self.inner.strong_count() == 0
    }
}

/// A core type which can be fallibly converted from the FIDL type `F`.
///
/// For all `C: TryFromFidl<F>`, we provide a blanket impl of
/// [`F: TryIntoCore<C>`].
///
/// [`F: TryIntoCore<C>`]: TryIntoCore
pub(crate) trait TryFromFidl<F>: Sized {
    /// The type of error returned from [`try_from_fidl`].
    ///
    /// [`try_from_fidl`]: TryFromFidl::try_from_fidl
    type Error;

    /// Attempt to convert from `fidl` into an instance of `Self`.
    fn try_from_fidl(fidl: F) -> Result<Self, Self::Error>;
}

/// A core type which can be fallibly converted to the FIDL type `F`.
///
/// For all `C: TryIntoFidl<F>`, we provide a blanket impl of
/// [`F: TryFromCore<C>`].
///
/// [`F: TryFromCore<C>`]: TryFromCore
pub(crate) trait TryIntoFidl<F>: Sized {
    /// The type of error returned from [`try_into_fidl`].
    ///
    /// [`try_into_fidl`]: TryIntoFidl::try_into_fidl
    type Error;

    /// Attempt to convert `self` into an instance of `F`.
    fn try_into_fidl(self) -> Result<F, Self::Error>;
}

/// A core type which can be infallibly converted into the FIDL type `F`.
///
/// `IntoFidl<F>` extends [`TryIntoFidl<F, Error = Never>`], and provides the
/// infallible conversion method [`into_fidl`].
///
/// [`TryIntoFidl<F, Error = Never>`]: TryIntoFidl
/// [`into_fidl`]: IntoFidl::into_fidl
pub(crate) trait IntoFidl<F> {
    /// Infallibly convert `self` into an instance of `F`.
    fn into_fidl(self) -> F;
}

impl<C: TryIntoFidl<F, Error = Never>, F> IntoFidl<F> for C {
    fn into_fidl(self) -> F {
        match self.try_into_fidl() {
            Ok(f) => f,
            Err(never) => match never {},
        }
    }
}

/// A FIDL type which can be fallibly converted from the core type `C`.
///
/// `TryFromCore<C>` is automatically implemented for all `F` where
/// [`C: TryIntoFidl<F>`].
///
/// [`C: TryIntoFidl<F>`]: TryIntoFidl
pub(crate) trait TryFromCore<C>: Sized {
    /// The error type on conversion failure.
    type Error;

    /// Attempt to convert from `core` into an instance of `Self`.
    fn try_from_core(core: C) -> Result<Self, Self::Error>;
}

impl<F, C: TryIntoFidl<F>> TryFromCore<C> for F {
    type Error = C::Error;
    fn try_from_core(core: C) -> Result<Self, Self::Error> {
        core.try_into_fidl()
    }
}

/// A FIDL type which can be fallibly converted into the core type `C`.
///
/// `TryIntoCore<C>` is automatically implemented for all `F` where
/// [`C: TryFromFidl<F>`].
///
/// [`C: TryFromFidl<F>`]: TryFromFidl
pub(crate) trait TryIntoCore<C>: Sized {
    /// The error returned on conversion failure.
    type Error;

    /// Attempt to convert from `self` into an instance of `C`.
    ///
    /// This is equivalent to [`C::try_from_fidl(self)`].
    ///
    /// [`C::try_from_fidl(self)`]: TryFromFidl::try_from_fidl
    fn try_into_core(self) -> Result<C, Self::Error>;
}

impl<F, C: TryFromFidl<F>> TryIntoCore<C> for F {
    type Error = C::Error;
    fn try_into_core(self) -> Result<C, Self::Error> {
        C::try_from_fidl(self)
    }
}

/// A FIDL type which can be infallibly converted into the core type `C`.
///
/// `IntoCore<C>` extends [`TryIntoCore<C>`] where `<C as TryFromFidl<_>>::Error
/// = Never`, and provides the infallible conversion method [`into_core`].
///
/// [`TryIntoCore<C>`]: TryIntoCore
/// [`into_core`]: IntoCore::into_core
pub(crate) trait IntoCore<C> {
    /// Infallibly convert `self` into an instance of `C`.
    fn into_core(self) -> C;
}

impl<F, C: TryFromFidl<F, Error = Never>> IntoCore<C> for F {
    fn into_core(self) -> C {
        match self.try_into_core() {
            Ok(c) => c,
            Err(never) => match never {},
        }
    }
}

impl<T> TryIntoFidl<T> for Never {
    type Error = Never;

    fn try_into_fidl(self) -> Result<T, Never> {
        match self {}
    }
}

impl TryIntoFidl<fidl_net_stack::Error> for SubnetError {
    type Error = Never;

    fn try_into_fidl(self) -> Result<fidl_net_stack::Error, Never> {
        Ok(fidl_net_stack::Error::InvalidArgs)
    }
}

impl TryIntoFidl<fidl_net_stack::Error> for AddrSubnetError {
    type Error = Never;

    fn try_into_fidl(self) -> Result<fidl_net_stack::Error, Never> {
        Ok(fidl_net_stack::Error::InvalidArgs)
    }
}

impl TryIntoFidl<fidl_net_stack::Error> for ExistsError {
    type Error = Never;

    fn try_into_fidl(self) -> Result<fidl_net_stack::Error, Never> {
        Ok(fidl_net_stack::Error::AlreadyExists)
    }
}

impl TryIntoFidl<fidl_net_stack::Error> for NetstackError {
    type Error = Never;

    fn try_into_fidl(self) -> Result<fidl_net_stack::Error, Never> {
        match self {
            NetstackError::Exists => Ok(fidl_net_stack::Error::AlreadyExists),
            NetstackError::NotFound => Ok(fidl_net_stack::Error::NotFound),
            _ => Ok(fidl_net_stack::Error::Internal),
        }
    }
}

impl TryIntoFidl<fidl_net_stack::Error> for NotFoundError {
    type Error = Never;

    fn try_into_fidl(self) -> Result<fidl_net_stack::Error, Never> {
        Ok(fidl_net_stack::Error::NotFound)
    }
}

impl TryIntoFidl<fidl_net_stack::Error> for AddRouteError {
    type Error = Never;

    fn try_into_fidl(self) -> Result<fidl_net_stack::Error, Never> {
        match self {
            AddRouteError::AlreadyExists => Ok(fidl_net_stack::Error::AlreadyExists),
            AddRouteError::GatewayNotNeighbor => Ok(fidl_net_stack::Error::BadState),
        }
    }
}

impl TryFromFidl<fidl_net::IpAddress> for IpAddr {
    type Error = Never;

    fn try_from_fidl(addr: fidl_net::IpAddress) -> Result<IpAddr, Never> {
        match addr {
            fidl_net::IpAddress::Ipv4(v4) => Ok(IpAddr::V4(v4.into_core())),
            fidl_net::IpAddress::Ipv6(v6) => Ok(IpAddr::V6(v6.into_core())),
        }
    }
}

impl TryIntoFidl<fidl_net::IpAddress> for IpAddr {
    type Error = Never;

    fn try_into_fidl(self) -> Result<fidl_net::IpAddress, Never> {
        match self {
            IpAddr::V4(addr) => Ok(fidl_net::IpAddress::Ipv4(addr.into_fidl())),
            IpAddr::V6(addr) => Ok(fidl_net::IpAddress::Ipv6(addr.into_fidl())),
        }
    }
}

impl TryFromFidl<fidl_net::Ipv4Address> for Ipv4Addr {
    type Error = Never;

    fn try_from_fidl(addr: fidl_net::Ipv4Address) -> Result<Ipv4Addr, Never> {
        Ok(addr.addr.into())
    }
}

impl TryIntoFidl<fidl_net::Ipv4Address> for Ipv4Addr {
    type Error = Never;

    fn try_into_fidl(self) -> Result<fidl_net::Ipv4Address, Never> {
        Ok(fidl_net::Ipv4Address { addr: self.ipv4_bytes() })
    }
}

impl TryFromFidl<fidl_net::Ipv6Address> for Ipv6Addr {
    type Error = Never;

    fn try_from_fidl(addr: fidl_net::Ipv6Address) -> Result<Ipv6Addr, Never> {
        Ok(addr.addr.into())
    }
}

impl TryIntoFidl<fidl_net::Ipv6Address> for Ipv6Addr {
    type Error = Never;

    fn try_into_fidl(self) -> Result<fidl_net::Ipv6Address, Never> {
        Ok(fidl_net::Ipv6Address { addr: self.ipv6_bytes() })
    }
}

impl TryFromFidl<fidl_net::MacAddress> for Mac {
    type Error = Never;

    fn try_from_fidl(mac: fidl_net::MacAddress) -> Result<Mac, Never> {
        Ok(Mac::new(mac.octets))
    }
}

impl TryIntoFidl<fidl_net::MacAddress> for Mac {
    type Error = Never;

    fn try_into_fidl(self) -> Result<fidl_net::MacAddress, Never> {
        Ok(fidl_net::MacAddress { octets: self.bytes() })
    }
}

/// An error indicating that an address was a member of the wrong class (for
/// example, a unicast address used where a multicast address is required).
#[derive(Debug)]
pub struct AddrClassError;

// TODO(joshlf): Introduce a separate variant to `fidl_net_stack::Error` for
// `AddrClassError`?
impl TryIntoFidl<fidl_net_stack::Error> for AddrClassError {
    type Error = Never;

    fn try_into_fidl(self) -> Result<fidl_net_stack::Error, Never> {
        Ok(fidl_net_stack::Error::InvalidArgs)
    }
}

impl TryFromFidl<fidl_net::IpAddress> for SpecifiedAddr<IpAddr> {
    type Error = AddrClassError;

    fn try_from_fidl(fidl: fidl_net::IpAddress) -> Result<SpecifiedAddr<IpAddr>, AddrClassError> {
        SpecifiedAddr::new(fidl.into_core()).ok_or(AddrClassError)
    }
}

impl TryIntoFidl<fidl_net::IpAddress> for SpecifiedAddr<IpAddr> {
    type Error = Never;

    fn try_into_fidl(self) -> Result<fidl_net::IpAddress, Never> {
        Ok(self.get().into_fidl())
    }
}

impl TryFromFidl<fidl_net::Subnet> for AddrSubnetEither {
    type Error = AddrSubnetError;

    fn try_from_fidl(fidl: fidl_net::Subnet) -> Result<AddrSubnetEither, AddrSubnetError> {
        AddrSubnetEither::new(fidl.addr.into_core(), fidl.prefix_len)
    }
}

impl TryIntoFidl<fidl_net::Subnet> for AddrSubnetEither {
    type Error = Never;

    fn try_into_fidl(self) -> Result<fidl_net::Subnet, Never> {
        let (addr, prefix) = self.addr_prefix();
        Ok(fidl_net::Subnet { addr: addr.into_fidl(), prefix_len: prefix })
    }
}

impl TryFromFidl<fidl_net::Subnet> for SubnetEither {
    type Error = SubnetError;

    fn try_from_fidl(fidl: fidl_net::Subnet) -> Result<SubnetEither, SubnetError> {
        SubnetEither::new(fidl.addr.into_core(), fidl.prefix_len)
    }
}

impl TryIntoFidl<fidl_net::Subnet> for SubnetEither {
    type Error = Never;

    fn try_into_fidl(self) -> Result<fidl_net::Subnet, Never> {
        let (net, prefix) = self.net_prefix();
        Ok(fidl_net::Subnet { addr: net.into_fidl(), prefix_len: prefix })
    }
}

impl TryFromFidl<fposix_socket::OptionalUint8> for Option<u8> {
    type Error = Never;

    fn try_from_fidl(fidl: fposix_socket::OptionalUint8) -> Result<Self, Self::Error> {
        Ok(match fidl {
            fposix_socket::OptionalUint8::Unset(fposix_socket::Empty) => None,
            fposix_socket::OptionalUint8::Value(u) => Some(u),
        })
    }
}

impl TryIntoFidl<fposix_socket::OptionalUint8> for Option<u8> {
    type Error = Never;

    fn try_into_fidl(self) -> Result<fposix_socket::OptionalUint8, Self::Error> {
        Ok(self
            .map(fposix_socket::OptionalUint8::Value)
            .unwrap_or(fposix_socket::OptionalUint8::Unset(fposix_socket::Empty)))
    }
}

impl TryIntoFidl<fnet_interfaces::AddressAssignmentState>
    for netstack3_core::ip::device::IpAddressState
{
    type Error = Never;

    fn try_into_fidl(self) -> Result<fnet_interfaces::AddressAssignmentState, Never> {
        match self {
            netstack3_core::ip::device::IpAddressState::Unavailable => {
                Ok(fnet_interfaces::AddressAssignmentState::Unavailable)
            }
            netstack3_core::ip::device::IpAddressState::Assigned => {
                Ok(fnet_interfaces::AddressAssignmentState::Assigned)
            }
            netstack3_core::ip::device::IpAddressState::Tentative => {
                Ok(fnet_interfaces::AddressAssignmentState::Tentative)
            }
        }
    }
}

impl<A: IpAddress> TryIntoFidl<<A::Version as IpSockAddrExt>::SocketAddress>
    for (Option<SpecifiedAddr<A>>, NonZeroU16)
where
    A::Version: IpSockAddrExt,
{
    type Error = Never;

    fn try_into_fidl(self) -> Result<<A::Version as IpSockAddrExt>::SocketAddress, Self::Error> {
        let (addr, port) = self;
        Ok(SockAddr::new(addr.map(ZonedAddr::Unzoned), port.get()))
    }
}

impl TryIntoFidl<fposix_socket::OptionalUint32> for Option<u32> {
    type Error = Never;

    fn try_into_fidl(self) -> Result<fposix_socket::OptionalUint32, Self::Error> {
        Ok(match self {
            Some(value) => fposix_socket::OptionalUint32::Value(value),
            None => fposix_socket::OptionalUint32::Unset(fposix_socket::Empty),
        })
    }
}

impl TryFromFidl<fposix_socket::OptionalUint32> for Option<u32> {
    type Error = Never;

    fn try_from_fidl(fidl: fposix_socket::OptionalUint32) -> Result<Self, Self::Error> {
        Ok(match fidl {
            fposix_socket::OptionalUint32::Value(value) => Some(value),
            fposix_socket::OptionalUint32::Unset(fposix_socket::Empty) => None,
        })
    }
}

pub(crate) enum MulticastMembershipConversionError {
    AddrNotMulticast,
    WrongIpVersion,
}

impl<I: Ip> GenericOverIp<I> for MulticastMembershipConversionError {
    type Type = Self;
}

impl IntoErrno for MulticastMembershipConversionError {
    fn into_errno(self) -> fposix::Errno {
        match self {
            Self::AddrNotMulticast => fposix::Errno::Einval,
            Self::WrongIpVersion => fposix::Errno::Enoprotoopt,
        }
    }
}

impl<A: IpAddress> TryFromFidl<fposix_socket::IpMulticastMembership>
    for (MulticastAddr<A>, Option<MulticastInterfaceSelector<A, BindingId>>)
{
    type Error = MulticastMembershipConversionError;

    fn try_from_fidl(fidl: fposix_socket::IpMulticastMembership) -> Result<Self, Self::Error> {
        <A::Version as Ip>::map_ip(
            IpInv(fidl),
            |IpInv(fidl)| {
                let fposix_socket::IpMulticastMembership { iface, local_addr, mcast_addr } = fidl;
                let mcast_addr = MulticastAddr::new(mcast_addr.into_core())
                    .ok_or(Self::Error::AddrNotMulticast)?;
                // Match Linux behavior by ignoring the address if an interface
                // identifier is provided.
                let selector = BindingId::new(iface)
                    .map(MulticastInterfaceSelector::Interface)
                    .or_else(|| {
                        SpecifiedAddr::new(local_addr.into_core())
                            .map(MulticastInterfaceSelector::LocalAddress)
                    });
                Ok((mcast_addr, selector))
            },
            |IpInv(_fidl)| Err(Self::Error::WrongIpVersion),
        )
    }
}

impl<A: IpAddress> TryFromFidl<fposix_socket::Ipv6MulticastMembership>
    for (MulticastAddr<A>, Option<MulticastInterfaceSelector<A, BindingId>>)
{
    type Error = MulticastMembershipConversionError;

    fn try_from_fidl(fidl: fposix_socket::Ipv6MulticastMembership) -> Result<Self, Self::Error> {
        <A::Version as Ip>::map_ip(
            IpInv(fidl),
            |IpInv(_fidl)| Err(Self::Error::WrongIpVersion),
            |IpInv(fidl)| {
                let fposix_socket::Ipv6MulticastMembership { iface, mcast_addr } = fidl;
                let mcast_addr = MulticastAddr::new(mcast_addr.into_core())
                    .ok_or(Self::Error::AddrNotMulticast)?;
                let selector = BindingId::new(iface).map(MulticastInterfaceSelector::Interface);
                Ok((mcast_addr, selector))
            },
        )
    }
}

/// Provides a stateful context for operations that require state-keeping to be
/// completed.
///
/// `ConversionContext` is used by conversion functions in
/// [`TryFromFidlWithContext`] and [`TryFromCoreWithContext`].
pub(crate) trait ConversionContext {
    /// Converts a binding identifier (exposed in FIDL as `u64`) to a core
    /// identifier `DeviceId`.
    ///
    /// Returns `None` if there is no core mapping equivalent for `binding_id`.
    fn get_core_id(&self, binding_id: BindingId) -> Option<DeviceId<BindingsNonSyncCtxImpl>>;

    /// Converts a core identifier `DeviceId` to a FIDL-compatible [`BindingId`].
    fn get_binding_id(&self, core_id: DeviceId<BindingsNonSyncCtxImpl>) -> BindingId;
}

/// A core type which can be fallibly converted from the FIDL type `F` given a
/// context that implements [`ConversionContext`].
///
/// For all `C: TryFromFidlWithContext<F>`, we provide a blanket impl of
/// [`F: TryIntoCoreWithContext<C>`].
///
/// [`F: TryIntoCoreWithContext<C>`]: TryIntoCoreWithContext
pub(crate) trait TryFromFidlWithContext<F>: Sized {
    /// The type of error returned from [`try_from_fidl_with_ctx`].
    ///
    /// [`try_from_fidl_with_ctx`]: TryFromFidlWithContext::try_from_fidl_with_ctx
    type Error;

    /// Attempt to convert from `fidl` into an instance of `Self`.
    fn try_from_fidl_with_ctx<C: ConversionContext>(ctx: &C, fidl: F) -> Result<Self, Self::Error>;
}

/// A core type which can be fallibly converted to the FIDL type `F` given a
/// context that implements [`ConversionContext`].
///
/// For all `C: TryIntoFidlWithContext<F>`, we provide a blanket impl of
/// [`F: TryFromCoreWithContext<C>`].
///
/// [`F: TryFromCoreWithContext<C>`]: TryFromCoreWithContext
pub(crate) trait TryIntoFidlWithContext<F>: Sized {
    /// The type of error returned from [`try_into_fidl_with_ctx`].
    ///
    /// [`try_into_fidl_with_ctx`]: TryIntoFidlWithContext::try_into_fidl_with_ctx
    type Error;

    /// Attempt to convert from `self` into an instance of `F`.
    fn try_into_fidl_with_ctx<C: ConversionContext>(self, ctx: &C) -> Result<F, Self::Error>;
}

/// A FIDL type which can be fallibly converted from the core type `C` given a
/// context that implements [`ConversionContext`].
///
/// `TryFromCoreWithContext<C>` is automatically implemented for all `F` where
/// [`C: TryIntoFidlWithContext<F>`].
///
/// [`C: TryIntoFidlWithContext<F>`]: TryIntoFidlWithContext
pub(crate) trait TryFromCoreWithContext<C>: Sized {
    /// The type of error returned from [`try_from_core_with_ctx`].
    ///
    /// [`try_from_core_with_ctx`]: TryFromCoreWithContext::try_from_core_with_ctx
    type Error;

    /// Attempt to convert from `core` into an instance of `Self`.
    fn try_from_core_with_ctx<X: ConversionContext>(ctx: &X, core: C) -> Result<Self, Self::Error>;
}

impl<F, C: TryIntoFidlWithContext<F>> TryFromCoreWithContext<C> for F {
    type Error = C::Error;

    fn try_from_core_with_ctx<X: ConversionContext>(ctx: &X, core: C) -> Result<Self, Self::Error> {
        core.try_into_fidl_with_ctx(ctx)
    }
}

/// A FIDL type which can be fallibly converted into the core type `C` given a
/// context that implements [`ConversionContext`].
///
/// `TryIntoCoreWithContext<C>` is automatically implemented for all `F` where
/// [`C: TryFromFidlWithContext<F>`].
///
/// [`C: TryFromFidlWithContext<F>`]: TryFromFidlWithContext
pub(crate) trait TryIntoCoreWithContext<C>: Sized {
    /// The type of error returned from [`try_into_core_with_ctx`].
    ///
    /// [`try_into_core_with_ctx`]: TryIntoCoreWithContext::try_into_core_with_ctx
    type Error;

    /// Attempt to convert from `self` into an instance of `C`.
    fn try_into_core_with_ctx<X: ConversionContext>(self, ctx: &X) -> Result<C, Self::Error>;
}

impl<F, C: TryFromFidlWithContext<F>> TryIntoCoreWithContext<C> for F {
    type Error = C::Error;

    fn try_into_core_with_ctx<X: ConversionContext>(self, ctx: &X) -> Result<C, Self::Error> {
        C::try_from_fidl_with_ctx(ctx, self)
    }
}

#[derive(Debug, PartialEq)]
pub struct DeviceNotFoundError;

#[derive(Debug, PartialEq)]
pub enum SocketAddressError {
    Device(DeviceNotFoundError),
    UnexpectedZone,
}

impl IntoErrno for SocketAddressError {
    fn into_errno(self) -> fposix::Errno {
        match self {
            SocketAddressError::Device(d) => d.into_errno(),
            SocketAddressError::UnexpectedZone => todo!(),
        }
    }
}

impl<A: IpAddress, D> TryFromFidlWithContext<<A::Version as IpSockAddrExt>::SocketAddress>
    for (Option<ZonedAddr<A, D>>, u16)
where
    A::Version: IpSockAddrExt,
    D: TryFromFidlWithContext<
        <<A::Version as IpSockAddrExt>::SocketAddress as SockAddr>::Zone,
        Error = DeviceNotFoundError,
    >,
{
    type Error = SocketAddressError;

    fn try_from_fidl_with_ctx<C: ConversionContext>(
        ctx: &C,
        fidl: <A::Version as IpSockAddrExt>::SocketAddress,
    ) -> Result<Self, Self::Error> {
        let port = fidl.port();
        let specified = match fidl.get_specified_addr() {
            Some(addr) => addr,
            None => return Ok((None, port)),
        };

        let zoned = match fidl.zone() {
            Some(zone) => {
                let addr_and_zone = AddrAndZone::new(specified.get(), zone)
                    .ok_or(SocketAddressError::UnexpectedZone)?;

                addr_and_zone
                    .try_map_zone(|zone| {
                        TryFromFidlWithContext::try_from_fidl_with_ctx(ctx, zone)
                            .map_err(SocketAddressError::Device)
                    })
                    .map(Into::into)?
            }
            None => specified.into(),
        };
        Ok((Some(zoned), port))
    }
}

impl<A: IpAddress, D> TryIntoFidlWithContext<<A::Version as IpSockAddrExt>::SocketAddress>
    for (Option<ZonedAddr<A, D>>, u16)
where
    A::Version: IpSockAddrExt,
    D: TryIntoFidlWithContext<<<A::Version as IpSockAddrExt>::SocketAddress as SockAddr>::Zone>,
{
    type Error = D::Error;

    fn try_into_fidl_with_ctx<C: ConversionContext>(
        self,
        ctx: &C,
    ) -> Result<<A::Version as IpSockAddrExt>::SocketAddress, Self::Error> {
        let (addr, port) = self;
        let addr = addr
            .map(|addr| {
                Ok(match addr {
                    ZonedAddr::Unzoned(addr) => addr.into(),
                    ZonedAddr::Zoned(z) => z
                        .try_map_zone(|zone| {
                            TryIntoFidlWithContext::try_into_fidl_with_ctx(zone, ctx)
                        })?
                        .into(),
                })
            })
            .transpose()?;
        Ok(SockAddr::new(addr, port))
    }
}

impl<A: IpAddress, D> TryIntoFidlWithContext<<A::Version as IpSockAddrExt>::SocketAddress>
    for (ZonedAddr<A, D>, NonZeroU16)
where
    A::Version: IpSockAddrExt,
    D: TryIntoFidlWithContext<
        <<A::Version as IpSockAddrExt>::SocketAddress as SockAddr>::Zone,
        Error = DeviceNotFoundError,
    >,
{
    type Error = DeviceNotFoundError;

    fn try_into_fidl_with_ctx<C: ConversionContext>(
        self,
        ctx: &C,
    ) -> Result<<A::Version as IpSockAddrExt>::SocketAddress, Self::Error> {
        let (addr, port) = self;
        (Some(addr), port.get()).try_into_fidl_with_ctx(ctx)
    }
}

impl<A: IpAddress, D> TryIntoFidlWithContext<<A::Version as IpSockAddrExt>::SocketAddress>
    for (Option<ZonedAddr<A, D>>, NonZeroU16)
where
    A::Version: IpSockAddrExt,
    D: TryIntoFidlWithContext<
        <<A::Version as IpSockAddrExt>::SocketAddress as SockAddr>::Zone,
        Error = DeviceNotFoundError,
    >,
{
    type Error = DeviceNotFoundError;

    fn try_into_fidl_with_ctx<C: ConversionContext>(
        self,
        ctx: &C,
    ) -> Result<<A::Version as IpSockAddrExt>::SocketAddress, Self::Error> {
        let (addr, port) = self;
        (addr, port.get()).try_into_fidl_with_ctx(ctx)
    }
}

impl<A, D1, D2> TryFromFidlWithContext<MulticastMembershipInterfaceSelector<A, D1>>
    for MulticastMembershipInterfaceSelector<A, D2>
where
    A: IpAddress,
    D2: TryFromFidlWithContext<D1>,
{
    type Error = D2::Error;

    fn try_from_fidl_with_ctx<C: ConversionContext>(
        ctx: &C,
        selector: MulticastMembershipInterfaceSelector<A, D1>,
    ) -> Result<Self, Self::Error> {
        use MulticastMembershipInterfaceSelector::*;
        Ok(match selector {
            Specified(MulticastInterfaceSelector::Interface(id)) => {
                Specified(MulticastInterfaceSelector::Interface(id.try_into_core_with_ctx(ctx)?))
            }
            Specified(MulticastInterfaceSelector::LocalAddress(addr)) => {
                Specified(MulticastInterfaceSelector::LocalAddress(addr))
            }
            AnyInterfaceWithRoute => AnyInterfaceWithRoute,
        })
    }
}

impl TryFromFidlWithContext<Never> for DeviceId<BindingsNonSyncCtxImpl> {
    type Error = DeviceNotFoundError;

    fn try_from_fidl_with_ctx<C: ConversionContext>(
        _ctx: &C,
        fidl: Never,
    ) -> Result<Self, Self::Error> {
        match fidl {}
    }
}

impl TryIntoFidlWithContext<Never> for DeviceId<BindingsNonSyncCtxImpl> {
    type Error = DeviceNotFoundError;

    fn try_into_fidl_with_ctx<C: ConversionContext>(self, _ctx: &C) -> Result<Never, Self::Error> {
        Err(DeviceNotFoundError)
    }
}

impl TryFromFidlWithContext<BindingId> for DeviceId<BindingsNonSyncCtxImpl> {
    type Error = DeviceNotFoundError;

    fn try_from_fidl_with_ctx<C: ConversionContext>(
        ctx: &C,
        fidl: BindingId,
    ) -> Result<Self, Self::Error> {
        ctx.get_core_id(fidl).ok_or(DeviceNotFoundError)
    }
}

impl TryIntoFidlWithContext<BindingId> for DeviceId<BindingsNonSyncCtxImpl> {
    type Error = Never;

    fn try_into_fidl_with_ctx<C: ConversionContext>(self, ctx: &C) -> Result<BindingId, Never> {
        Ok(ctx.get_binding_id(self))
    }
}

impl TryIntoFidlWithContext<Never> for WeakDeviceId<BindingsNonSyncCtxImpl> {
    type Error = DeviceNotFoundError;

    fn try_into_fidl_with_ctx<C: ConversionContext>(self, _ctx: &C) -> Result<Never, Self::Error> {
        Err(DeviceNotFoundError)
    }
}

impl TryIntoFidlWithContext<BindingId> for WeakDeviceId<BindingsNonSyncCtxImpl> {
    type Error = DeviceNotFoundError;

    fn try_into_fidl_with_ctx<C: ConversionContext>(
        self,
        ctx: &C,
    ) -> Result<BindingId, DeviceNotFoundError> {
        self.upgrade().map(|d| ctx.get_binding_id(d)).ok_or(DeviceNotFoundError)
    }
}

impl IntoErrno for DeviceNotFoundError {
    fn into_errno(self) -> fposix::Errno {
        fposix::Errno::Enodev
    }
}

#[derive(Debug, Eq, PartialEq)]
pub enum ForwardingConversionError {
    DeviceNotFound,
    TypeMismatch,
    Subnet(SubnetError),
    AddrClassError,
}

impl From<DeviceNotFoundError> for ForwardingConversionError {
    fn from(_: DeviceNotFoundError) -> Self {
        ForwardingConversionError::DeviceNotFound
    }
}

impl From<SubnetError> for ForwardingConversionError {
    fn from(err: SubnetError) -> Self {
        ForwardingConversionError::Subnet(err)
    }
}

impl From<AddrClassError> for ForwardingConversionError {
    fn from(_: AddrClassError) -> Self {
        ForwardingConversionError::AddrClassError
    }
}

impl From<ForwardingConversionError> for fidl_net_stack::Error {
    fn from(fwd_error: ForwardingConversionError) -> Self {
        match fwd_error {
            ForwardingConversionError::DeviceNotFound => fidl_net_stack::Error::NotFound,
            ForwardingConversionError::TypeMismatch
            | ForwardingConversionError::Subnet(_)
            | ForwardingConversionError::AddrClassError => fidl_net_stack::Error::InvalidArgs,
        }
    }
}

impl TryFromFidlWithContext<fidl_net_stack::ForwardingEntry>
    for AddableEntryEither<DeviceId<BindingsNonSyncCtxImpl>>
{
    type Error = ForwardingConversionError;

    fn try_from_fidl_with_ctx<C: ConversionContext>(
        ctx: &C,
        fidl: fidl_net_stack::ForwardingEntry,
    ) -> Result<AddableEntryEither<DeviceId<BindingsNonSyncCtxImpl>>, ForwardingConversionError>
    {
        let fidl_net_stack::ForwardingEntry { subnet, device_id, next_hop, metric } = fidl;
        let subnet = subnet.try_into_core()?;
        let device =
            BindingId::new(device_id).map(|d| d.try_into_core_with_ctx(ctx)).transpose()?;
        let next_hop: Option<SpecifiedAddr<IpAddr>> =
            next_hop.map(|next_hop| (*next_hop).try_into_core()).transpose()?;
        let metric = if metric == UNSET_FORWARDING_ENTRY_METRIC {
            AddableMetric::MetricTracksInterface
        } else {
            AddableMetric::ExplicitMetric(RawMetric(metric))
        };

        Ok(match (subnet, device, next_hop.map(Into::into)) {
            (subnet, Some(device), None) => Self::without_gateway(subnet, device, metric),
            (SubnetEither::V4(subnet), device, Some(IpAddr::V4(gateway))) => {
                AddableEntry::with_gateway(subnet, device, gateway, metric).into()
            }
            (SubnetEither::V6(subnet), device, Some(IpAddr::V6(gateway))) => {
                AddableEntry::with_gateway(subnet, device, gateway, metric).into()
            }
            (SubnetEither::V4(_), _, Some(IpAddr::V6(_)))
            | (SubnetEither::V6(_), _, Some(IpAddr::V4(_)))
            | (_, None, None) => return Err(ForwardingConversionError::TypeMismatch),
        })
    }
}

impl TryIntoFidlWithContext<fidl_net_stack::ForwardingEntry>
    for EntryEither<DeviceId<BindingsNonSyncCtxImpl>>
{
    type Error = Never;

    fn try_into_fidl_with_ctx<C: ConversionContext>(
        self,
        ctx: &C,
    ) -> Result<fidl_net_stack::ForwardingEntry, Never> {
        let (subnet, device, gateway, metric): (
            SubnetEither,
            _,
            Option<IpAddr<SpecifiedAddr<Ipv4Addr>, SpecifiedAddr<Ipv6Addr>>>,
            _,
        ) = match self {
            EntryEither::V4(Entry { subnet, device, gateway, metric }) => {
                (subnet.into(), device, gateway.map(|gateway| gateway.into()), metric)
            }
            EntryEither::V6(Entry { subnet, device, gateway, metric }) => {
                (subnet.into(), device, gateway.map(|gateway| gateway.into()), metric)
            }
        };
        let RawMetric(metric) = metric.value();
        let device_id: BindingId = device.try_into_fidl_with_ctx(ctx)?;
        let next_hop = gateway.map(|next_hop| {
            let next_hop: SpecifiedAddr<IpAddr> = next_hop.into();
            Box::new(next_hop.into_fidl())
        });
        Ok(fidl_net_stack::ForwardingEntry {
            subnet: subnet.into_fidl(),
            device_id: device_id.get(),
            next_hop,
            metric: metric,
        })
    }
}

impl<I: Ip> TryIntoFidlWithContext<fnet_routes_ext::InstalledRoute<I>>
    for Entry<I::Addr, DeviceId<BindingsNonSyncCtxImpl>>
{
    type Error = Never;

    fn try_into_fidl_with_ctx<C: ConversionContext>(
        self,
        ctx: &C,
    ) -> Result<fnet_routes_ext::InstalledRoute<I>, Never> {
        let Entry { subnet, device, gateway, metric } = self;
        let device: BindingId = device.try_into_fidl_with_ctx(ctx)?;
        let specified_metric = match metric {
            Metric::ExplicitMetric(value) => {
                fnet_routes::SpecifiedMetric::ExplicitMetric(value.into())
            }
            Metric::MetricTracksInterface(_value) => {
                fnet_routes::SpecifiedMetric::InheritedFromInterface(fnet_routes::Empty)
            }
        };
        Ok(fnet_routes_ext::InstalledRoute {
            route: fnet_routes_ext::Route {
                destination: subnet,
                action: fnet_routes_ext::RouteAction::Forward(fnet_routes_ext::RouteTarget {
                    outbound_interface: device.get(),
                    next_hop: gateway,
                }),
                properties: fnet_routes_ext::RouteProperties {
                    specified_properties: fnet_routes_ext::SpecifiedRouteProperties {
                        metric: specified_metric,
                    },
                },
            },
            effective_properties: fnet_routes_ext::EffectiveRouteProperties {
                metric: metric.value().into(),
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::fmt::Debug;

    use fidl_fuchsia_net as fidl_net;
    use fidl_fuchsia_net_ext::IntoExt;
    use fidl_fuchsia_net_interfaces_ext as fnet_interfaces_ext;
    use net_declare::{net_ip_v4, net_ip_v6};
    use net_types::ip::{Ipv4Addr, Ipv6Addr};
    use test_case::test_case;

    use crate::bindings::{
        devices, interfaces_admin::OwnedControlHandle, Ctx, DeviceIdExt as _,
        InterfaceEventProducer, DEFAULT_INTERFACE_METRIC, DEFAULT_LOOPBACK_MTU,
    };

    use super::*;

    struct FakeConversionContext {
        binding: BindingId,
        core: DeviceId<BindingsNonSyncCtxImpl>,
        // We hold the netstack even though it is unused because we create
        // a device in the Netstack to get a device ID.
        //
        // Netstack requires that all strongly-referenced IDs are dropped before
        // the internal reference to the device is dropped. We include netstack
        // here (at the bottom of this struct) to make sure it is only dropped
        // after the device IDs are dropped.
        _netstack: Ctx,
    }

    impl FakeConversionContext {
        async fn new() -> Self {
            // We need a valid context to be able to create DeviceIds, so
            // we just create it, get the device id, and then destroy
            // everything.
            let ctx = Ctx::default();
            let binding = BindingId::MIN;
            let core = {
                let (_control_client_end, control_server_end) =
                    fnet_interfaces_ext::admin::Control::create_endpoints()
                        .expect("create control proxy");
                let (control_sender, _control_receiver) =
                    OwnedControlHandle::new_channel_with_owned_handle(control_server_end).await;
                let (event_sender, _event_receiver) = futures::channel::mpsc::unbounded();

                let state = ctx.clone();
                netstack3_core::device::add_loopback_device_with_state(
                    &state.sync_ctx,
                    DEFAULT_LOOPBACK_MTU,
                    RawMetric(DEFAULT_INTERFACE_METRIC),
                    || {
                        const LOOPBACK_NAME: &'static str = "lo";

                        devices::LoopbackInfo {
                            static_common_info: devices::StaticCommonInfo {
                                binding_id: binding,
                                name: LOOPBACK_NAME.to_string(),
                                tx_notifier: Default::default(),
                            },
                            dynamic_common_info: devices::DynamicCommonInfo {
                                mtu: DEFAULT_LOOPBACK_MTU,
                                admin_enabled: true,
                                events: InterfaceEventProducer::new(binding, event_sender),
                                control_hook: control_sender,
                                addresses: HashMap::new(),
                            }
                            .into(),
                            rx_notifier: Default::default(),
                        }
                    },
                )
                .expect("failed to add loopback to core")
                .into()
            };

            Self { binding, core, _netstack: ctx }
        }
    }

    impl ConversionContext for FakeConversionContext {
        fn get_core_id(&self, binding_id: BindingId) -> Option<DeviceId<BindingsNonSyncCtxImpl>> {
            if binding_id == self.binding {
                Some(self.core.clone())
            } else {
                None
            }
        }

        fn get_binding_id(&self, core_id: DeviceId<BindingsNonSyncCtxImpl>) -> BindingId {
            core_id.external_state().static_common_info().binding_id
        }
    }

    struct EmptyFakeConversionContext;
    impl ConversionContext for EmptyFakeConversionContext {
        fn get_core_id(&self, _binding_id: BindingId) -> Option<DeviceId<BindingsNonSyncCtxImpl>> {
            None
        }

        fn get_binding_id(&self, core_id: DeviceId<BindingsNonSyncCtxImpl>) -> BindingId {
            core_id.external_state().static_common_info().binding_id
        }
    }

    fn create_addr_v4(bytes: [u8; 4]) -> (IpAddr, fidl_net::IpAddress) {
        let core = IpAddr::V4(Ipv4Addr::from(bytes));
        let fidl = fidl_net::IpAddress::Ipv4(fidl_net::Ipv4Address { addr: bytes });
        (core, fidl)
    }

    fn create_addr_v6(bytes: [u8; 16]) -> (IpAddr, fidl_net::IpAddress) {
        let core = IpAddr::V6(Ipv6Addr::from(bytes));
        let fidl = fidl_net::IpAddress::Ipv6(fidl_net::Ipv6Address { addr: bytes });
        (core, fidl)
    }

    fn create_subnet(
        subnet: (IpAddr, fidl_net::IpAddress),
        prefix: u8,
    ) -> (SubnetEither, fidl_net::Subnet) {
        let (core, fidl) = subnet;
        (
            SubnetEither::new(core, prefix).unwrap(),
            fidl_net::Subnet { addr: fidl, prefix_len: prefix },
        )
    }

    fn create_addr_subnet(
        addr: (IpAddr, fidl_net::IpAddress),
        prefix: u8,
    ) -> (AddrSubnetEither, fidl_net::Subnet) {
        let (core, fidl) = addr;
        (
            AddrSubnetEither::new(core, prefix).unwrap(),
            fidl_net::Subnet { addr: fidl, prefix_len: prefix },
        )
    }

    #[test]
    fn addr_v4() {
        let bytes = [192, 168, 0, 1];
        let (core, fidl) = create_addr_v4(bytes);

        assert_eq!(core, fidl.into_core());
        assert_eq!(fidl, core.into_fidl());
    }

    #[test]
    fn addr_v6() {
        let bytes = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16];
        let (core, fidl) = create_addr_v6(bytes);

        assert_eq!(core, fidl.into_core());
        assert_eq!(fidl, core.into_fidl());
    }

    #[test]
    fn addr_subnet_v4() {
        let bytes = [192, 168, 0, 1];
        let prefix = 24;
        let (core, fidl) = create_addr_subnet(create_addr_v4(bytes), prefix);

        assert_eq!(fidl, core.into_fidl());
        assert_eq!(core, fidl.try_into_core().unwrap());
    }

    #[test]
    fn addr_subnet_v6() {
        let bytes = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16];
        let prefix = 64;
        let (core, fidl) = create_addr_subnet(create_addr_v6(bytes), prefix);

        assert_eq!(fidl, core.into_fidl());
        assert_eq!(core, fidl.try_into_core().unwrap());
    }

    #[test]
    fn subnet_v4() {
        let bytes = [192, 168, 0, 0];
        let prefix = 24;
        let (core, fidl) = create_subnet(create_addr_v4(bytes), prefix);

        assert_eq!(fidl, core.into_fidl());
        assert_eq!(core, fidl.try_into_core().unwrap());
    }

    #[test]
    fn subnet_v6() {
        let bytes = [1, 2, 3, 4, 5, 6, 7, 8, 0, 0, 0, 0, 0, 0, 0, 0];
        let prefix = 64;
        let (core, fidl) = create_subnet(create_addr_v6(bytes), prefix);

        assert_eq!(fidl, core.into_fidl());
        assert_eq!(core, fidl.try_into_core().unwrap());
    }

    #[test]
    fn ip_address_state() {
        use fnet_interfaces::AddressAssignmentState;
        use netstack3_core::ip::device::IpAddressState;
        assert_eq!(IpAddressState::Unavailable.into_fidl(), AddressAssignmentState::Unavailable);
        assert_eq!(IpAddressState::Tentative.into_fidl(), AddressAssignmentState::Tentative);
        assert_eq!(IpAddressState::Assigned.into_fidl(), AddressAssignmentState::Assigned);
    }

    #[test_case(
        fidl_net::Ipv6SocketAddress {
            address: net_ip_v6!("1:2:3:4::").into_ext(),
            port: 8080,
            zone_index: 1
        },
        SocketAddressError::UnexpectedZone;
        "IPv6 specified unexpected zone")]
    #[test_case(
        fidl_net::Ipv6SocketAddress {
            address: net_ip_v6!("fe80::1").into_ext(),
            port: 8080,
            zone_index: 2
        },
        SocketAddressError::Device(DeviceNotFoundError);
        "IPv6 specified invalid zone")]
    #[fuchsia_async::run_singlethreaded(test)]
    async fn sock_addr_into_core_err<A: SockAddr>(addr: A, expected: SocketAddressError)
    where
        (Option<ZonedAddr<A::AddrType, DeviceId<BindingsNonSyncCtxImpl>>>, u16):
            TryFromFidlWithContext<A, Error = SocketAddressError>,
        <A::AddrType as IpAddress>::Version: IpSockAddrExt<SocketAddress = A>,
        DeviceId<BindingsNonSyncCtxImpl>:
            TryFromFidlWithContext<A::Zone, Error = DeviceNotFoundError>,
    {
        let ctx = FakeConversionContext::new().await;

        let result: Result<(Option<_>, _), _> = addr.try_into_core_with_ctx(&ctx);
        assert_eq!(result.expect_err("should fail"), expected);
    }

    /// Placeholder for an ID that should be replaced with the real `DeviceId`
    /// from the `FakeConversionContext`.
    struct ReplaceWithCoreId;

    #[test_case(
        fidl_net::Ipv4SocketAddress {address: net_ip_v4!("192.168.0.0").into_ext(), port: 8080},
        (Some(SpecifiedAddr::new(net_ip_v4!("192.168.0.0")).unwrap().into()), 8080);
        "IPv4 specified")]
    #[test_case(
        fidl_net::Ipv4SocketAddress {address: net_ip_v4!("0.0.0.0").into_ext(), port: 8000},
        (None, 8000);
        "IPv4 unspecified")]
    #[test_case(
        fidl_net::Ipv6SocketAddress {
            address: net_ip_v6!("1:2:3:4::").into_ext(),
            port: 8080,
            zone_index: 0
        },
        (Some(SpecifiedAddr::new(net_ip_v6!("1:2:3:4::")).unwrap().into()), 8080);
        "IPv6 specified no zone")]
    #[test_case(
        fidl_net::Ipv6SocketAddress {
            address: net_ip_v6!("::").into_ext(),
            port: 8080,
            zone_index: 0,
        },
        (None, 8080);
        "IPv6 unspecified")]
    #[test_case(
        fidl_net::Ipv6SocketAddress {
            address: net_ip_v6!("fe80::1").into_ext(),
            port: 8080,
            zone_index: 1
        },
        (Some(
            AddrAndZone::new(net_ip_v6!("fe80::1"), ReplaceWithCoreId).unwrap().into()
        ), 8080);
        "IPv6 specified valid zone")]
    #[fuchsia_async::run_singlethreaded(test)]
    async fn sock_addr_conversion_reversible<A: SockAddr + Eq + Clone>(
        addr: A,
        (zoned, port): (Option<ZonedAddr<A::AddrType, ReplaceWithCoreId>>, u16),
    ) where
        (Option<ZonedAddr<A::AddrType, DeviceId<BindingsNonSyncCtxImpl>>>, u16):
            TryFromFidlWithContext<A, Error = SocketAddressError> + TryIntoFidlWithContext<A>,
        <(Option<ZonedAddr<A::AddrType, DeviceId<BindingsNonSyncCtxImpl>>>, u16) as
                 TryIntoFidlWithContext<A>>::Error: Debug,
        <A::AddrType as IpAddress>::Version: IpSockAddrExt<SocketAddress = A>,
        DeviceId<BindingsNonSyncCtxImpl>:
            TryFromFidlWithContext<A::Zone, Error = DeviceNotFoundError>,
    {
        let ctx = FakeConversionContext::new().await;
        let zoned = zoned.map(|z| match z {
            ZonedAddr::Unzoned(z) => z.into(),
            ZonedAddr::Zoned(z) => z.map_zone(|ReplaceWithCoreId| ctx.core.clone()).into(),
        });

        let result: (Option<ZonedAddr<_, _>>, _) =
            addr.clone().try_into_core_with_ctx(&ctx).expect("into core should succeed");
        assert_eq!(result, (zoned, port));

        let result = result.try_into_fidl_with_ctx(&ctx).expect("reverse should succeed");
        assert_eq!(result, addr)
    }

    #[test]
    fn test_unzoned_ip_port_into_fidl() {
        let ip = net_ip_v4!("1.7.2.4");
        let port = 3893;
        assert_eq!(
            (SpecifiedAddr::new(ip), NonZeroU16::new(port).unwrap()).into_fidl(),
            fidl_net::Ipv4SocketAddress { address: ip.into_ext(), port }
        );

        let ip = net_ip_v6!("1:2:3:4:5::");
        assert_eq!(
            (SpecifiedAddr::new(ip), NonZeroU16::new(port).unwrap()).into_fidl(),
            fidl_net::Ipv6SocketAddress { address: ip.into_ext(), port, zone_index: 0 }
        );
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn device_id_from_bindings_id() {
        let ctx = FakeConversionContext::new().await;

        let id = ctx.binding;
        let device_id: DeviceId<_> = id.try_into_core_with_ctx(&ctx).unwrap();
        assert_eq!(device_id, ctx.core);

        let bad_id = id.checked_add(1).unwrap();
        assert_eq!(bad_id.try_into_core_with_ctx(&ctx), Err::<DeviceId<_>, _>(DeviceNotFoundError));
    }

    #[test]
    fn optional_u8_conversion() {
        let empty = fposix_socket::OptionalUint8::Unset(fposix_socket::Empty);
        let empty_core: Option<u8> = empty.into_core();
        assert_eq!(empty_core, None);
        assert_eq!(empty_core.into_fidl(), empty);

        let value = fposix_socket::OptionalUint8::Value(46);
        let value_core: Option<u8> = value.into_core();
        assert_eq!(value_core, Some(46));
        assert_eq!(value_core.into_fidl(), value);
    }
}
