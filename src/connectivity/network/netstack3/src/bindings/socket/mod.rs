// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Socket features exposed by netstack3.

pub(crate) mod datagram;
pub(crate) mod packet;
pub(crate) mod queue;
pub(crate) mod raw;
pub(crate) mod stream;
pub(crate) mod worker;

use std::{convert::Infallible as Never, num::NonZeroU64};

use const_unwrap::const_unwrap_option;
use either::Either;
use fidl_fuchsia_net as fnet;
use fidl_fuchsia_posix::Errno;
use fidl_fuchsia_posix_socket as psocket;
use fuchsia_zircon as zx;
use futures::{TryFutureExt as _, TryStreamExt as _};
use net_types::{
    ip::{Ip, IpAddress, Ipv4, Ipv4Addr, Ipv6, Ipv6Addr},
    ScopeableAddress, SpecifiedAddr, Witness, ZonedAddr,
};
use netstack3_core::{
    device::DeviceId,
    error::{LocalAddressError, NetstackError, RemoteAddressError, SocketError, ZonedAddressError},
    ip::{
        socket::{IpSockCreationError, IpSockSendError},
        ResolveRouteError,
    },
    socket::datagram::{ConnectListenerError, SetMulticastMembershipError, SockCreationError},
    transport::{tcp, udp},
};

use crate::bindings::{
    devices::{
        BindingId, DeviceSpecificInfo, Devices, DynamicCommonInfo, DynamicNetdeviceInfo,
        LoopbackInfo, NetdeviceInfo, StaticCommonInfo,
    },
    util::{DeviceNotFoundError, IntoCore as _, IntoFidl as _, TryIntoCoreWithContext},
    Ctx, DeviceIdExt as _,
};

const ZXSIO_SIGNAL_INCOMING: zx::Signals =
    const_unwrap_option(zx::Signals::from_bits(psocket::SIGNAL_DATAGRAM_INCOMING));
const ZXSIO_SIGNAL_OUTGOING: zx::Signals =
    const_unwrap_option(zx::Signals::from_bits(psocket::SIGNAL_DATAGRAM_OUTGOING));
const ZXSIO_SIGNAL_CONNECTED: zx::Signals =
    const_unwrap_option(zx::Signals::from_bits(psocket::SIGNAL_STREAM_CONNECTED));

/// Common properties for socket workers.
#[derive(Debug)]
pub(crate) struct SocketWorkerProperties {}

pub(crate) async fn serve(
    ctx: crate::bindings::Ctx,
    stream: psocket::ProviderRequestStream,
) -> Result<(), fidl::Error> {
    stream
        .try_fold(ctx, |ctx, req| async {
            match req {
                psocket::ProviderRequest::InterfaceIndexToName { index, responder } => {
                    let mut response = {
                        let ctx = ctx.clone();
                        let Ctx { sync_ctx: _, non_sync_ctx } = &ctx;
                        BindingId::new(index)
                            .ok_or(DeviceNotFoundError)
                            .and_then(|id| id.try_into_core_with_ctx(non_sync_ctx))
                            .map(|core_id: DeviceId<_>| {
                                core_id.external_state().static_common_info().name.clone()
                            })
                            .map_err(|DeviceNotFoundError| zx::Status::NOT_FOUND.into_raw())
                    };
                    responder_send!(responder, &mut response);
                }
                psocket::ProviderRequest::InterfaceNameToIndex { name, responder } => {
                    let response = {
                        let ctx = ctx.clone();
                        let Ctx { sync_ctx: _, non_sync_ctx } = &ctx;
                        let devices = AsRef::<Devices<_>>::as_ref(&non_sync_ctx);
                        let result = devices
                            .get_device_by_name(&name)
                            .map(|d| d.external_state().static_common_info().binding_id.get())
                            .ok_or(zx::Status::NOT_FOUND.into_raw());
                        result
                    };
                    responder_send!(responder, response);
                }
                psocket::ProviderRequest::InterfaceNameToFlags { name, responder } => {
                    responder_send!(responder, &mut get_interface_flags(&ctx, &name));
                }
                psocket::ProviderRequest::StreamSocket { domain, proto, responder } => {
                    let (client, request_stream) = create_request_stream();
                    stream::spawn_worker(domain, proto, ctx.clone(), request_stream);
                    responder_send!(responder, Ok(client));
                }
                psocket::ProviderRequest::DatagramSocketDeprecated { domain, proto, responder } => {
                    let response = (|| {
                        let (client, request_stream) = create_request_stream();
                        let () = datagram::spawn_worker(
                            domain,
                            proto,
                            ctx.clone(),
                            request_stream,
                            SocketWorkerProperties {},
                        )?;
                        Ok(client)
                    })();
                    responder_send!(responder, response);
                }
                psocket::ProviderRequest::DatagramSocket { domain, proto, responder } => {
                    let response = (|| {
                        let (client, request_stream) = create_request_stream();
                        let () = datagram::spawn_worker(
                            domain,
                            proto,
                            ctx.clone(),
                            request_stream,
                            SocketWorkerProperties {},
                        )?;
                        Ok(psocket::ProviderDatagramSocketResponse::SynchronousDatagramSocket(
                            client,
                        ))
                    })();
                    responder_send!(responder, response);
                }
                psocket::ProviderRequest::GetInterfaceAddresses { responder } => {
                    responder_send!(responder, &get_interface_addresses(&ctx));
                }
            }
            Ok(ctx)
        })
        .map_ok(|_: crate::bindings::Ctx| ())
        .await
}

pub(crate) fn create_request_stream<T: fidl::endpoints::ProtocolMarker>(
) -> (fidl::endpoints::ClientEnd<T>, T::RequestStream) {
    fidl::endpoints::create_request_stream().expect("can't create stream")
}

fn get_interface_addresses(ctx: &Ctx) -> Vec<psocket::InterfaceAddresses> {
    let Ctx { sync_ctx, non_sync_ctx } = ctx;
    non_sync_ctx.devices.with_devices(|devices| {
        devices
            .map(|d| {
                // Generally, calling into `netstack3_core` while operating
                // on the non-sync context is a recipe for deadlocks. That's
                // not an issue here since the non-sync context isn't being
                // passed into `get_all_ip_addr_subnets`.
                let addresses = netstack3_core::get_all_ip_addr_subnets(&*sync_ctx, d)
                    .into_iter()
                    .map(fidl_fuchsia_net_ext::FromExt::from_ext)
                    .collect();

                let info = d.external_state();
                let flags = flags_for_device(&info);
                let StaticCommonInfo { binding_id, name } = info.static_common_info();

                psocket::InterfaceAddresses {
                    id: Some(binding_id.get()),
                    name: Some(name.clone()),
                    addresses: Some(addresses),
                    interface_flags: Some(flags),
                    ..Default::default()
                }
            })
            .collect::<Vec<_>>()
    })
}

fn get_interface_flags(
    ctx: &Ctx,
    name: &str,
) -> Result<psocket::InterfaceFlags, zx::sys::zx_status_t> {
    let Ctx { sync_ctx: _, non_sync_ctx } = ctx;
    let device =
        non_sync_ctx.devices.get_device_by_name(name).ok_or(zx::Status::NOT_FOUND.into_raw())?;
    Ok(flags_for_device(&device.external_state()))
}

fn flags_for_device(info: &DeviceSpecificInfo<'_>) -> psocket::InterfaceFlags {
    let physical_up: bool;
    let loopback: bool;

    struct FromDynamicInfo {
        admin_enabled: bool,
    }

    impl<'a> From<&'a DynamicCommonInfo> for FromDynamicInfo {
        fn from(value: &'a DynamicCommonInfo) -> Self {
            let DynamicCommonInfo {
                mtu: _,
                admin_enabled,
                events: _,
                control_hook: _,
                addresses: _,
            } = value;
            FromDynamicInfo { admin_enabled: *admin_enabled }
        }
    }

    let FromDynamicInfo { admin_enabled } = match info {
        DeviceSpecificInfo::Netdevice(NetdeviceInfo {
            handler: _,
            mac: _,
            static_common_info: _,
            dynamic,
        }) => {
            let guard = dynamic.read().unwrap();
            let DynamicNetdeviceInfo { common_info, phy_up } = &*guard;
            physical_up = *phy_up;
            loopback = false;
            common_info.into()
        }
        DeviceSpecificInfo::Loopback(LoopbackInfo {
            static_common_info: _,
            dynamic_common_info,
            rx_notifier: _,
        }) => {
            physical_up = true;
            loopback = true;
            (&*dynamic_common_info.read().unwrap()).into()
        }
    };

    // Approximate that all interfaces support multicasting.
    // TODO(https://fxbug.dev/125492): Set this more precisely.
    let multicast = true;

    // Note that the interface flags are not all intuitively named. Quotes below
    // are from https://www.xml.com/ldd/chapter/book/ch14.html#INDEX-3,507.
    [
        // IFF_UP is "on when the interface is active and ready to transfer
        // packets".
        (physical_up, psocket::InterfaceFlags::UP),
        // IFF_LOOPBACK is "set only in the loopback interface".
        (loopback, psocket::InterfaceFlags::LOOPBACK),
        // IFF_RUNNING "indicates that the interface is up and running".
        (admin_enabled && physical_up, psocket::InterfaceFlags::RUNNING),
        // IFF_MULTICAST is set for "interfaces that are capable of multicast
        // transmission".
        (multicast, psocket::InterfaceFlags::MULTICAST),
    ]
    .into_iter()
    .fold(psocket::InterfaceFlags::empty(), |mut flags, (b, flag)| {
        flags.set(flag, b);
        flags
    })
}

/// A trait generalizing the data structures passed as arguments to POSIX socket
/// calls.
///
/// `SockAddr` implementers are typically passed to POSIX socket calls as a blob
/// of bytes. It represents a type that can be parsed from a C API `struct
/// sockaddr`, expressed as a stream of bytes.
pub(crate) trait SockAddr: std::fmt::Debug + Sized + Send {
    /// The concrete address type for this `SockAddr`.
    type AddrType: IpAddress + ScopeableAddress;
    /// The socket's domain.
    const DOMAIN: psocket::Domain;
    /// The type of a zone identifier for this `SockAddr`.
    type Zone;

    /// Creates a new `SockAddr` from the provided address and port.
    ///
    /// `addr` is either `Some(a)` where `a` holds a specified address an
    /// optional zone, or `None` for the unspecified address (which can't have a
    /// zone).
    fn new(addr: Option<ZonedAddr<Self::AddrType, Self::Zone>>, port: u16) -> Self;

    /// Gets this `SockAddr`'s address.
    fn addr(&self) -> Self::AddrType;

    /// Set this [`SockAddr`]'s address.
    fn set_addr(&mut self, addr: Self::AddrType);

    /// Gets this `SockAddr`'s port.
    fn port(&self) -> u16;

    /// Set this [`SockAddr`]'s port.
    fn set_port(&mut self, port: u16);

    /// Gets a `SpecifiedAddr` witness type for this `SockAddr`'s address.
    fn get_specified_addr(&self) -> Option<SpecifiedAddr<Self::AddrType>> {
        SpecifiedAddr::<Self::AddrType>::new(self.addr())
    }

    /// Gets this `SockAddr`'s zone identifier.
    fn zone(&self) -> Option<Self::Zone>;

    /// Converts this `SockAddr` into an [`fnet::SocketAddress`].
    fn into_sock_addr(self) -> fnet::SocketAddress;

    /// Converts an [`fnet::SocketAddress`] into a `SockAddr`.
    fn from_sock_addr(addr: fnet::SocketAddress) -> Result<Self, Errno>;
}

impl SockAddr for fnet::Ipv6SocketAddress {
    type AddrType = Ipv6Addr;
    const DOMAIN: psocket::Domain = psocket::Domain::Ipv6;
    type Zone = NonZeroU64;

    /// Creates a new `SockAddr6`.
    fn new(addr: Option<ZonedAddr<Ipv6Addr, NonZeroU64>>, port: u16) -> Self {
        let (addr, zone_index) = addr.map_or((Ipv6::UNSPECIFIED_ADDRESS, 0), |addr| {
            let (addr, zone) = ZonedAddr::into_addr_zone(addr);
            (addr.get(), zone.map_or(0, NonZeroU64::get))
        });
        fnet::Ipv6SocketAddress { address: addr.into_fidl(), port, zone_index }
    }

    fn addr(&self) -> Ipv6Addr {
        self.address.into_core()
    }

    fn set_addr(&mut self, addr: Ipv6Addr) {
        self.address = addr.into_fidl();
    }

    fn port(&self) -> u16 {
        self.port
    }

    fn set_port(&mut self, port: u16) {
        self.port = port
    }

    fn zone(&self) -> Option<Self::Zone> {
        NonZeroU64::new(self.zone_index)
    }

    fn into_sock_addr(self) -> fnet::SocketAddress {
        fnet::SocketAddress::Ipv6(self)
    }

    fn from_sock_addr(addr: fnet::SocketAddress) -> Result<Self, Errno> {
        match addr {
            fnet::SocketAddress::Ipv6(a) => Ok(a),
            fnet::SocketAddress::Ipv4(_) => Err(Errno::Eafnosupport),
        }
    }
}

impl SockAddr for fnet::Ipv4SocketAddress {
    type AddrType = Ipv4Addr;
    const DOMAIN: psocket::Domain = psocket::Domain::Ipv4;
    type Zone = Never;

    /// Creates a new `SockAddr4`.
    fn new(addr: Option<ZonedAddr<Ipv4Addr, Never>>, port: u16) -> Self {
        let addr = addr.map_or(Ipv4::UNSPECIFIED_ADDRESS, |zoned| match zoned.into_addr_zone() {
            (a, None) => a.get(),
            (_a, Some(n)) => match n {},
        });
        fnet::Ipv4SocketAddress { address: addr.into_fidl(), port }
    }

    fn addr(&self) -> Ipv4Addr {
        self.address.into_core()
    }

    fn set_addr(&mut self, addr: Ipv4Addr) {
        self.address = addr.into_fidl();
    }

    fn port(&self) -> u16 {
        self.port
    }

    fn set_port(&mut self, port: u16) {
        self.port = port
    }

    fn zone(&self) -> Option<Self::Zone> {
        None
    }

    fn into_sock_addr(self) -> fnet::SocketAddress {
        fnet::SocketAddress::Ipv4(self)
    }

    fn from_sock_addr(addr: fnet::SocketAddress) -> Result<Self, Errno> {
        match addr {
            fnet::SocketAddress::Ipv4(a) => Ok(a),
            fnet::SocketAddress::Ipv6(_) => Err(Errno::Eafnosupport),
        }
    }
}

/// Extension trait that associates a [`SockAddr`] and [`MulticastMembership`]
/// implementation to an IP version. We provide implementations for [`Ipv4`] and
/// [`Ipv6`].
pub(crate) trait IpSockAddrExt: Ip {
    type SocketAddress: SockAddr<AddrType = Self::Addr>;
}

impl IpSockAddrExt for Ipv4 {
    type SocketAddress = fnet::Ipv4SocketAddress;
}

impl IpSockAddrExt for Ipv6 {
    type SocketAddress = fnet::Ipv6SocketAddress;
}

pub(crate) enum IpMulticastMembership {
    V4(psocket::IpMulticastMembership),
    V6(psocket::Ipv6MulticastMembership),
}

impl From<psocket::IpMulticastMembership> for IpMulticastMembership {
    fn from(membership: psocket::IpMulticastMembership) -> Self {
        Self::V4(membership)
    }
}

impl From<psocket::Ipv6MulticastMembership> for IpMulticastMembership {
    fn from(membership: psocket::Ipv6MulticastMembership) -> Self {
        Self::V6(membership)
    }
}

#[cfg(test)]
mod testutil {
    use net_types::ip::{AddrSubnetEither, IpAddr};

    use super::*;

    /// A trait that exposes common test behavior to implementers of
    /// [`SockAddr`].
    pub(crate) trait TestSockAddr: SockAddr {
        /// A different domain.
        ///
        /// `Ipv4SocketAddress` defines it as `Ipv6SocketAddress` and
        /// vice-versa.
        type DifferentDomain: TestSockAddr;
        /// The local address used for tests.
        const LOCAL_ADDR: Self::AddrType;
        /// The remote address used for tests.
        const REMOTE_ADDR: Self::AddrType;
        /// An alternate remote address used for tests.
        const REMOTE_ADDR_2: Self::AddrType;
        /// An non-local address which is unreachable, used for tests.
        const UNREACHABLE_ADDR: Self::AddrType;

        /// The default subnet prefix used for tests.
        const DEFAULT_PREFIX: u8;

        /// Creates an [`fnet::SocketAddress`] with the given `addr` and `port`.
        fn create(addr: Self::AddrType, port: u16) -> fnet::SocketAddress {
            Self::new(SpecifiedAddr::new(addr).map(ZonedAddr::Unzoned), port).into_sock_addr()
        }

        /// Gets the local address and prefix configured for the test
        /// [`SockAddr`].
        fn config_addr_subnet() -> AddrSubnetEither {
            AddrSubnetEither::new(IpAddr::from(Self::LOCAL_ADDR), Self::DEFAULT_PREFIX).unwrap()
        }

        /// Gets the remote address and prefix to use for the test [`SockAddr`].
        fn config_addr_subnet_remote() -> AddrSubnetEither {
            AddrSubnetEither::new(IpAddr::from(Self::REMOTE_ADDR), Self::DEFAULT_PREFIX).unwrap()
        }
    }

    impl TestSockAddr for fnet::Ipv6SocketAddress {
        type DifferentDomain = fnet::Ipv4SocketAddress;

        const LOCAL_ADDR: Ipv6Addr =
            Ipv6Addr::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 192, 168, 0, 1]);
        const REMOTE_ADDR: Ipv6Addr =
            Ipv6Addr::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 192, 168, 0, 2]);
        const REMOTE_ADDR_2: Ipv6Addr =
            Ipv6Addr::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 192, 168, 0, 3]);
        const UNREACHABLE_ADDR: Ipv6Addr =
            Ipv6Addr::from_bytes([0, 0, 0, 0, 0, 0, 0, 42, 0, 0, 0, 0, 192, 168, 0, 1]);
        const DEFAULT_PREFIX: u8 = 64;
    }

    impl TestSockAddr for fnet::Ipv4SocketAddress {
        type DifferentDomain = fnet::Ipv6SocketAddress;

        const LOCAL_ADDR: Ipv4Addr = Ipv4Addr::new([192, 168, 0, 1]);
        const REMOTE_ADDR: Ipv4Addr = Ipv4Addr::new([192, 168, 0, 2]);
        const REMOTE_ADDR_2: Ipv4Addr = Ipv4Addr::new([192, 168, 0, 3]);
        const UNREACHABLE_ADDR: Ipv4Addr = Ipv4Addr::new([192, 168, 42, 1]);
        const DEFAULT_PREFIX: u8 = 24;
    }
}

/// Trait expressing the conversion of error types into
/// [`fidl_fuchsia_posix::Errno`] errors for the POSIX-lite wrappers.
pub(crate) trait IntoErrno {
    /// Returns the most equivalent POSIX error code for `self`.
    fn into_errno(self) -> Errno;
}

impl IntoErrno for Errno {
    fn into_errno(self) -> Errno {
        self
    }
}

impl<A: IntoErrno, B: IntoErrno> IntoErrno for Either<A, B> {
    fn into_errno(self) -> Errno {
        match self {
            Either::Left(a) => a.into_errno(),
            Either::Right(b) => b.into_errno(),
        }
    }
}

impl IntoErrno for LocalAddressError {
    fn into_errno(self) -> Errno {
        match self {
            LocalAddressError::CannotBindToAddress
            | LocalAddressError::FailedToAllocateLocalPort => Errno::Eaddrnotavail,
            LocalAddressError::AddressMismatch => Errno::Einval,
            LocalAddressError::AddressInUse => Errno::Eaddrinuse,
            LocalAddressError::Zone(e) => e.into_errno(),
        }
    }
}

impl IntoErrno for RemoteAddressError {
    fn into_errno(self) -> Errno {
        match self {
            RemoteAddressError::NoRoute => Errno::Enetunreach,
        }
    }
}

impl IntoErrno for SocketError {
    fn into_errno(self) -> Errno {
        match self {
            SocketError::Remote(e) => e.into_errno(),
            SocketError::Local(e) => e.into_errno(),
        }
    }
}

impl IntoErrno for ResolveRouteError {
    fn into_errno(self) -> Errno {
        match self {
            ResolveRouteError::NoSrcAddr => Errno::Eaddrnotavail,
            ResolveRouteError::Unreachable => Errno::Enetunreach,
        }
    }
}

impl IntoErrno for IpSockCreationError {
    fn into_errno(self) -> Errno {
        match self {
            IpSockCreationError::Route(e) => e.into_errno(),
        }
    }
}

impl IntoErrno for IpSockSendError {
    fn into_errno(self) -> Errno {
        match self {
            IpSockSendError::Mtu => Errno::Einval,
            IpSockSendError::Unroutable(e) => e.into_errno(),
        }
    }
}

impl IntoErrno for netstack3_core::ip::icmp::IcmpSockCreationError {
    fn into_errno(self) -> Errno {
        match self {
            netstack3_core::ip::icmp::IcmpSockCreationError::Ip(e) => e.into_errno(),
            netstack3_core::ip::icmp::IcmpSockCreationError::SockAddrConflict => Errno::Eaddrinuse,
        }
    }
}

impl IntoErrno for udp::SendToError {
    fn into_errno(self) -> Errno {
        match self {
            Self::NotWriteable => Errno::Epipe,
            Self::CreateSock(err) => err.into_errno(),
            Self::Zone(err) => err.into_errno(),
            Self::Mtu => Errno::Emsgsize,
        }
    }
}

impl IntoErrno for udp::SendError {
    fn into_errno(self) -> Errno {
        match self {
            Self::IpSock(err) => err.into_errno(),
            Self::NotWriteable => Errno::Epipe,
        }
    }
}

impl IntoErrno for SockCreationError {
    fn into_errno(self) -> Errno {
        match self {
            Self::Ip(err) => err.into_errno(),
            Self::Zone(err) => err.into_errno(),
            Self::CouldNotAllocateLocalPort => Errno::Eaddrnotavail,
            Self::SockAddrConflict => Errno::Eaddrinuse,
        }
    }
}

impl IntoErrno for ConnectListenerError {
    fn into_errno(self) -> Errno {
        match self {
            Self::Ip(err) => err.into_errno(),
            Self::Zone(err) => err.into_errno(),
            Self::AddressConflict => Errno::Eaddrinuse,
        }
    }
}

impl IntoErrno for SetMulticastMembershipError {
    fn into_errno(self) -> Errno {
        match self {
            Self::AddressNotAvailable
            | Self::DeviceDoesNotExist
            | Self::NoDeviceWithAddress
            | Self::NoDeviceAvailable => Errno::Enodev,
            Self::NoMembershipChange => Errno::Eaddrinuse,
            Self::WrongDevice => Errno::Einval,
        }
    }
}

impl IntoErrno for ZonedAddressError {
    fn into_errno(self) -> Errno {
        match self {
            Self::RequiredZoneNotProvided => Errno::Einval,
            Self::DeviceZoneMismatch => Errno::Einval,
        }
    }
}

impl IntoErrno for tcp::socket::SetDeviceError {
    fn into_errno(self) -> Errno {
        match self {
            Self::Conflict => Errno::Eaddrinuse,
            Self::Unroutable => Errno::Ehostunreach,
            Self::ZoneChange => Errno::Einval,
        }
    }
}

impl IntoErrno for NetstackError {
    fn into_errno(self) -> Errno {
        match self {
            NetstackError::Parse(_) => Errno::Einval,
            NetstackError::Exists => Errno::Ealready,
            NetstackError::NotFound => Errno::Efault,
            NetstackError::SendUdp(s) => s.into_errno(),
            NetstackError::Connect(c) => c.into_errno(),
            NetstackError::NoRoute => Errno::Ehostunreach,
            NetstackError::Mtu => Errno::Emsgsize,
        }
    }
}
