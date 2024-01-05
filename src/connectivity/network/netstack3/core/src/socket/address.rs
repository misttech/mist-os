// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! A collection of types that represent the various parts of socket addresses.

use core::{
    fmt::{self, Debug, Display, Formatter},
    ops::Deref,
};

use derivative::Derivative;
use net_types::{
    ip::{GenericOverIp, Ip, IpAddress, Ipv4Addr, Ipv6Addr, Ipv6SourceAddr},
    NonMappedAddr, ScopeableAddress, SpecifiedAddr, UnicastAddr, Witness, ZonedAddr,
};

use crate::socket::{AddrVec, DualStackIpExt, SocketMapAddrSpec};

/// A [`ZonedAddr`] whose addr is witness to the properties required by sockets.
#[derive(Copy, Clone, Eq, GenericOverIp, Hash, PartialEq)]
#[generic_over_ip(A, IpAddress)]
pub struct SocketZonedIpAddr<A: IpAddress, Z>(ZonedAddr<SpecifiedAddr<A>, Z>);

impl<A: IpAddress, Z: Debug> Debug for SocketZonedIpAddr<A, Z> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        let Self(addr) = self;
        write!(f, "{:?}", addr)
    }
}

impl<A: IpAddress, Z> SocketZonedIpAddr<A, Z> {
    /// Convert self the inner [`ZonedAddr`]
    pub fn into_inner(self) -> ZonedAddr<SpecifiedAddr<A>, Z> {
        self.0
    }

    /// Creates from a specified IP address and an optional zone.
    ///
    /// If `addr` requires a zone, then `get_zone` will be called to provide
    /// the zone.
    ///
    /// # Panics
    /// This method panics if the `addr` wants a zone and `get_zone` will panic
    /// when called.
    pub fn new_with_zone(addr: SpecifiedAddr<A>, get_zone: impl FnOnce() -> Z) -> Self {
        if let Some(addr_and_zone) = crate::socket::try_into_null_zoned(&addr) {
            ZonedAddr::Zoned(addr_and_zone.map_zone(move |()| get_zone())).into()
        } else {
            ZonedAddr::Unzoned(addr).into()
        }
    }
}

impl<A: IpAddress, Z> Deref for SocketZonedIpAddr<A, Z> {
    type Target = ZonedAddr<SpecifiedAddr<A>, Z>;
    fn deref(&self) -> &Self::Target {
        let SocketZonedIpAddr(addr) = self;
        addr
    }
}

impl<A: IpAddress, Z> From<ZonedAddr<SpecifiedAddr<A>, Z>> for SocketZonedIpAddr<A, Z> {
    fn from(addr: ZonedAddr<SpecifiedAddr<A>, Z>) -> Self {
        SocketZonedIpAddr(addr)
    }
}

/// An IP address that witnesses all required properties of a socket address.
///
/// Requires `SpecifiedAddr` because most contexts do not permit unspecified
/// addresses; those that do can hold a `Option<SocketIpAddr>`.
///
/// Requires `NonMappedAddr` because mapped addresses (i.e. ipv4-mapped-ipv6
/// addresses) are converted from their original IP version to their target IP
/// version when entering the stack.
#[derive(Copy, Clone, Eq, Hash, PartialEq)]
pub(crate) struct SocketIpAddr<A: IpAddress>(NonMappedAddr<SpecifiedAddr<A>>);

impl<A: IpAddress> Display for SocketIpAddr<A> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        let Self(addr) = self;
        write!(f, "{}", addr)
    }
}

impl<A: IpAddress> Debug for SocketIpAddr<A> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        let Self(addr) = self;
        write!(f, "{:?}", addr)
    }
}

impl<A: IpAddress> SocketIpAddr<A> {
    #[cfg(test)]
    pub(crate) fn new(addr: A) -> Option<SocketIpAddr<A>> {
        Some(SocketIpAddr(NonMappedAddr::new(SpecifiedAddr::new(addr)?)?))
    }

    /// Callers must ensure that the addr is both `Specified` and `NonMapped`.
    #[cfg(test)]
    pub(crate) const unsafe fn new_unchecked(addr: A) -> SocketIpAddr<A> {
        SocketIpAddr(NonMappedAddr::new_unchecked(SpecifiedAddr::new_unchecked(addr)))
    }

    /// Callers must ensure that the addr is `NonMapped`.
    pub(crate) const unsafe fn new_from_specified_unchecked(
        addr: SpecifiedAddr<A>,
    ) -> SocketIpAddr<A> {
        SocketIpAddr(NonMappedAddr::new_unchecked(addr))
    }

    pub(crate) fn addr(self) -> A {
        let SocketIpAddr(addr) = self;
        **addr
    }

    /// Constructs a `SocktIpAddr` from an addr that is known to be specified.
    ///
    /// # Panics
    ///
    /// If the given addr is mapped.
    // TODO(https://fxbug.dev/132092): Remove this function and it's callsites
    // once `SocketIpAddr` is the defacto address type in NS3's socket layer.
    pub(crate) fn new_from_specified_or_panic(addr: SpecifiedAddr<A>) -> Self {
        SocketIpAddr(NonMappedAddr::new(addr).expect("should be called with a non-mapped addr"))
    }
}

impl SocketIpAddr<Ipv4Addr> {
    pub(crate) fn new_ipv4_specified(addr: SpecifiedAddr<Ipv4Addr>) -> Self {
        addr.try_into().unwrap_or_else(|AddrIsMappedError {}| {
            unreachable!("IPv4 addresses must be non-mapped")
        })
    }
}

impl SocketIpAddr<Ipv6Addr> {
    /// Optionally constructs a [`SocketIpAddr`] from the given
    /// [`Ipv6SourceAddr`], returning `None` if the given addr is `Unspecified`.
    pub(crate) fn new_from_ipv6_source(addr: Ipv6SourceAddr) -> Option<Self> {
        match addr {
            Ipv6SourceAddr::Unspecified => None,
            Ipv6SourceAddr::Unicast(addr) => {
                let addr: UnicastAddr<NonMappedAddr<_>> = addr.transpose();
                let addr: NonMappedAddr<SpecifiedAddr<_>> = addr.into_specified().transpose();
                Some(SocketIpAddr(addr))
            }
        }
    }
}

impl<A: IpAddress> From<SocketIpAddr<A>> for SpecifiedAddr<A> {
    fn from(addr: SocketIpAddr<A>) -> Self {
        let SocketIpAddr(addr) = addr;
        *addr
    }
}

impl<A: IpAddress> AsRef<SpecifiedAddr<A>> for SocketIpAddr<A> {
    fn as_ref(&self) -> &SpecifiedAddr<A> {
        let SocketIpAddr(addr) = self;
        addr.as_ref()
    }
}

/// The addr could not be converted to a `NonMappedAddr`.
///
/// Perhaps the address was an ipv4-mapped-ipv6 addresses.
#[derive(Debug)]
pub(crate) struct AddrIsMappedError {}

impl<A: IpAddress> TryFrom<SpecifiedAddr<A>> for SocketIpAddr<A> {
    type Error = AddrIsMappedError;
    fn try_from(addr: SpecifiedAddr<A>) -> Result<Self, Self::Error> {
        NonMappedAddr::new(addr).map(SocketIpAddr).ok_or(AddrIsMappedError {})
    }
}

/// Allows [`SocketIpAddr`] to be used inside of a [`ZonedAddr`].
impl<A: IpAddress> ScopeableAddress for SocketIpAddr<A> {
    type Scope = A::Scope;
    fn scope(&self) -> Self::Scope {
        let SocketIpAddr(addr) = self;
        addr.scope()
    }
}

/// The IP address and identifier (port) of a listening socket.
#[derive(Copy, Clone, Debug, Eq, GenericOverIp, Hash, PartialEq)]
#[generic_over_ip(A, IpAddress)]
pub struct ListenerIpAddr<A: IpAddress, LI> {
    /// The specific address being listened on, or `None` for all addresses.
    pub(crate) addr: Option<SocketIpAddr<A>>,
    /// The local identifier (i.e. port for TCP/UDP).
    pub(crate) identifier: LI,
}

/// The address of a listening socket.
#[derive(Copy, Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) struct ListenerAddr<A, D> {
    pub(crate) ip: A,
    pub(crate) device: Option<D>,
}

/// The IP address and identifier (port) of a connected socket.
#[derive(Copy, Clone, Debug, Eq, GenericOverIp, Hash, PartialEq)]
#[generic_over_ip(A, IpAddress)]
pub struct ConnIpAddr<A: IpAddress, LI, RI> {
    pub(crate) local: (SocketIpAddr<A>, LI),
    pub(crate) remote: (SocketIpAddr<A>, RI),
}

/// The address of a connected socket.
#[derive(Copy, Clone, Debug, Eq, GenericOverIp, Hash, PartialEq)]
#[generic_over_ip()]
pub struct ConnAddr<A, D> {
    pub(crate) ip: A,
    pub(crate) device: Option<D>,
}

/// The IP address and identifier (port) of a dual-stack listening socket.
#[derive(Copy, Clone, Debug, Eq, Hash, PartialEq)]
pub enum DualStackListenerIpAddr<A: IpAddress, LI>
where
    A::Version: DualStackIpExt,
{
    ThisStack(ListenerIpAddr<A, LI>),
    OtherStack(ListenerIpAddr<<<A::Version as DualStackIpExt>::OtherVersion as Ip>::Addr, LI>),
    // The socket is dual-stack enabled and bound to the IPv6 any address.
    BothStacks(LI),
}

impl<A: IpAddress, NewIp: DualStackIpExt, LI> GenericOverIp<NewIp>
    for DualStackListenerIpAddr<A, LI>
where
    A::Version: DualStackIpExt,
{
    type Type = DualStackListenerIpAddr<NewIp::Addr, LI>;
}

/// The IP address and identifiers (ports) of a dual-stack connected socket.
#[derive(Copy, Clone, Debug, Eq, Hash, PartialEq)]
pub enum DualStackConnIpAddr<A: IpAddress, LI, RI>
where
    A::Version: DualStackIpExt,
{
    ThisStack(ConnIpAddr<A, LI, RI>),
    OtherStack(ConnIpAddr<<<A::Version as DualStackIpExt>::OtherVersion as Ip>::Addr, LI, RI>),
}

impl<A: IpAddress, NewIp: DualStackIpExt, LI, RI> GenericOverIp<NewIp>
    for DualStackConnIpAddr<A, LI, RI>
where
    A::Version: DualStackIpExt,
{
    type Type = DualStackConnIpAddr<NewIp::Addr, LI, RI>;
}

impl<I: Ip, A: SocketMapAddrSpec> From<ListenerIpAddr<I::Addr, A::LocalIdentifier>>
    for IpAddrVec<I, A>
{
    fn from(listener: ListenerIpAddr<I::Addr, A::LocalIdentifier>) -> Self {
        IpAddrVec::Listener(listener)
    }
}

impl<I: Ip, A: SocketMapAddrSpec> From<ConnIpAddr<I::Addr, A::LocalIdentifier, A::RemoteIdentifier>>
    for IpAddrVec<I, A>
{
    fn from(conn: ConnIpAddr<I::Addr, A::LocalIdentifier, A::RemoteIdentifier>) -> Self {
        IpAddrVec::Connected(conn)
    }
}

impl<I: Ip, D, A: SocketMapAddrSpec>
    From<ListenerAddr<ListenerIpAddr<I::Addr, A::LocalIdentifier>, D>> for AddrVec<I, D, A>
{
    fn from(listener: ListenerAddr<ListenerIpAddr<I::Addr, A::LocalIdentifier>, D>) -> Self {
        AddrVec::Listen(listener)
    }
}

impl<I: Ip, D, A: SocketMapAddrSpec>
    From<ConnAddr<ConnIpAddr<I::Addr, A::LocalIdentifier, A::RemoteIdentifier>, D>>
    for AddrVec<I, D, A>
{
    fn from(
        conn: ConnAddr<ConnIpAddr<I::Addr, A::LocalIdentifier, A::RemoteIdentifier>, D>,
    ) -> Self {
        AddrVec::Conn(conn)
    }
}

/// An address vector containing the portions of a socket address that are
/// visible in an IP packet.
#[derive(Derivative)]
#[derivative(
    Debug(bound = ""),
    Clone(bound = ""),
    Eq(bound = ""),
    PartialEq(bound = ""),
    Hash(bound = "")
)]
pub(crate) enum IpAddrVec<I: Ip, A: SocketMapAddrSpec> {
    Listener(ListenerIpAddr<I::Addr, A::LocalIdentifier>),
    Connected(ConnIpAddr<I::Addr, A::LocalIdentifier, A::RemoteIdentifier>),
}

impl<I: Ip, A: SocketMapAddrSpec> IpAddrVec<I, A> {
    fn with_device<D>(self, device: Option<D>) -> AddrVec<I, D, A> {
        match self {
            IpAddrVec::Listener(ip) => AddrVec::Listen(ListenerAddr { ip, device }),
            IpAddrVec::Connected(ip) => AddrVec::Conn(ConnAddr { ip, device }),
        }
    }
}

impl<I: Ip, A: SocketMapAddrSpec> IpAddrVec<I, A> {
    /// Returns the next smallest address vector that would receive all the same
    /// packets as this one.
    ///
    /// Address vectors are ordered by their shadowing relationship, such that
    /// a "smaller" vector shadows a "larger" one. This function returns the
    /// smallest of the set of shadows of `self`.
    fn widen(self) -> Option<Self> {
        match self {
            IpAddrVec::Listener(ListenerIpAddr { addr: None, identifier }) => {
                let _: A::LocalIdentifier = identifier;
                None
            }
            IpAddrVec::Connected(ConnIpAddr { local: (local_ip, local_identifier), remote }) => {
                let _: (SocketIpAddr<I::Addr>, A::RemoteIdentifier) = remote;
                Some(ListenerIpAddr { addr: Some(local_ip), identifier: local_identifier })
            }
            IpAddrVec::Listener(ListenerIpAddr { addr: Some(addr), identifier }) => {
                let _: SocketIpAddr<I::Addr> = addr;
                Some(ListenerIpAddr { addr: None, identifier })
            }
        }
        .map(IpAddrVec::Listener)
    }
}

enum AddrVecIterInner<I: Ip, D, A: SocketMapAddrSpec> {
    WithDevice { device: D, emitted_device: bool, addr: IpAddrVec<I, A> },
    NoDevice { addr: IpAddrVec<I, A> },
    Done,
}

impl<I: Ip, D: Clone, A: SocketMapAddrSpec> Iterator for AddrVecIterInner<I, D, A> {
    type Item = AddrVec<I, D, A>;

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::Done => None,
            Self::WithDevice { device, emitted_device, addr } => {
                if !*emitted_device {
                    *emitted_device = true;
                    Some(addr.clone().with_device(Some(device.clone())))
                } else {
                    let r = addr.clone().with_device(None);
                    if let Some(next) = addr.clone().widen() {
                        *addr = next;
                        *emitted_device = false;
                    } else {
                        *self = Self::Done;
                    }
                    Some(r)
                }
            }
            Self::NoDevice { addr } => {
                let r = addr.clone().with_device(None);
                if let Some(next) = addr.clone().widen() {
                    *addr = next;
                } else {
                    *self = Self::Done
                }
                Some(r)
            }
        }
    }
}

/// An iterator over socket addresses.
///
/// The generated address vectors are ordered according to the following
/// rules (ordered by precedence):
///   - a connected address is preferred over a listening address,
///   - a listening address for a specific IP address is preferred over one
///     for all addresses,
///   - an address with a specific device is preferred over one for all
///     devices.
///
/// The first yielded address is always the one provided via
/// [`AddrVecIter::with_device`] or [`AddrVecIter::without_device`].
pub(crate) struct AddrVecIter<I: Ip, D, A: SocketMapAddrSpec>(AddrVecIterInner<I, D, A>);

impl<I: Ip, D, A: SocketMapAddrSpec> AddrVecIter<I, D, A> {
    pub(crate) fn with_device(addr: IpAddrVec<I, A>, device: D) -> Self {
        Self(AddrVecIterInner::WithDevice { device, emitted_device: false, addr })
    }

    pub(crate) fn without_device(addr: IpAddrVec<I, A>) -> Self {
        Self(AddrVecIterInner::NoDevice { addr })
    }
}

impl<I: Ip, D: Clone, A: SocketMapAddrSpec> Iterator for AddrVecIter<I, D, A> {
    type Item = AddrVec<I, D, A>;

    fn next(&mut self) -> Option<Self::Item> {
        let Self(it) = self;
        it.next()
    }
}

#[derive(GenericOverIp)]
#[generic_over_ip(I, Ip)]
pub(crate) enum TryUnmapResult<I: DualStackIpExt, D> {
    /// The address does not have an un-mapped representation.
    ///
    /// This spits back the input address unmodified.
    CannotBeUnmapped(ZonedAddr<SocketIpAddr<I::Addr>, D>),
    /// The address in the other stack that corresponds to the input.
    ///
    /// Since [`SocketZonedIpAddr`] is guaranteed to hold a specified address,
    /// this must hold an `Option<SocketZonedIpAddr>`. Since `::FFFF:0.0.0.0` is
    /// a legal IPv4-mapped IPv6 address, this allows us to represent it as the
    /// unspecified IPv4 address.
    Mapped(Option<ZonedAddr<SocketIpAddr<<I::OtherVersion as Ip>::Addr>, D>>),
}

/// Try to convert a specified address into the address that maps to it from
/// the other stack.
///
/// This is an IP-generic function that tries to invert the
/// IPv4-to-IPv4-mapped-IPv6 conversion that is performed by
/// [`Ipv4Addr::to_ipv6_mapped`].
///
/// The only inputs that will produce [`TryUnmapResult::Mapped`] are
/// IPv4-mapped IPv6 addresses. All other inputs will produce
/// [`TryUnmapResult::CannotBeUnmapped`].
pub(crate) fn try_unmap<A: IpAddress, D>(
    addr: SocketZonedIpAddr<A, D>,
) -> TryUnmapResult<A::Version, D>
where
    A::Version: DualStackIpExt,
{
    <A::Version as Ip>::map_ip(
        addr.into_inner(),
        |v4| {
            let addr = SocketIpAddr::new_ipv4_specified(v4.addr());
            TryUnmapResult::CannotBeUnmapped(ZonedAddr::Unzoned(addr))
        },
        |v6| match v6.addr().to_ipv4_mapped() {
            Some(v4) => {
                let addr = SpecifiedAddr::new(v4).map(SocketIpAddr::new_ipv4_specified);
                TryUnmapResult::Mapped(addr.map(ZonedAddr::Unzoned))
            }
            None => {
                let (addr, zone) = v6.into_addr_zone();
                let addr: SocketIpAddr<_> =
                    addr.try_into().unwrap_or_else(|AddrIsMappedError {}| {
                        unreachable!(
                            "addr cannot be mapped because `to_ipv4_mapped` returned `None`"
                        )
                    });
                TryUnmapResult::CannotBeUnmapped(ZonedAddr::new(addr, zone).unwrap_or_else(|| {
                    unreachable!("addr should still be scopeable after wrapping in `SocketIpAddr`")
                }))
            }
        },
    )
}

/// A remote IP address that's either in the current stack or the other stack.
pub(crate) enum DualStackRemoteIp<I: DualStackIpExt, D> {
    ThisStack(ZonedAddr<SocketIpAddr<I::Addr>, D>),
    OtherStack(ZonedAddr<SocketIpAddr<<I::OtherVersion as Ip>::Addr>, D>),
}

/// Returns the [`DualStackRemoteIp`] for the given remote_ip.
///
/// An IPv4-mapped-IPv6 address will be unmapped to the inner IPv4 address, and
/// an unspecified address will be populated with
/// [`crate::socket::specify_unspecified_remote()`].
pub(crate) fn dual_stack_remote_ip<I: DualStackIpExt, D>(
    remote_ip: Option<SocketZonedIpAddr<I::Addr, D>>,
) -> DualStackRemoteIp<I, D> {
    let remote_ip = crate::socket::specify_unspecified_remote::<I, _, _>(
        remote_ip.map(SocketZonedIpAddr::into_inner),
    );
    match try_unmap(remote_ip.into()) {
        TryUnmapResult::CannotBeUnmapped(remote_ip) => {
            DualStackRemoteIp::<I, _>::ThisStack(remote_ip)
        }
        TryUnmapResult::Mapped(remote_ip) => {
            // NB: Even though we ensured the address was specified above by
            // calling `specify_unspecified_remote`, it's possible that
            // unmapping the address made it unspecified (e.g. `::FFFF:0.0.0.0`
            // is a specified IPv6 addr but an unspecified IPv4 addr). Call
            // `specify_unspecified_remote` again to ensure the unmapped address
            // is specified.
            let remote_ip =
                crate::socket::specify_unspecified_remote::<I::OtherVersion, _, _>(remote_ip);
            DualStackRemoteIp::OtherStack(remote_ip)
        }
    }
}
