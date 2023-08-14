// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! A collection of types that represent the various parts of socket addresses.

use core::num::NonZeroU16;

use derivative::Derivative;
use net_types::{
    ip::{GenericOverIp, Ip, IpAddress},
    SpecifiedAddr,
};

use crate::socket::{AddrVec, SocketMapAddrSpec};

/// The IP address and identifier (port) of a listening socket.
#[derive(Copy, Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) struct ListenerIpAddr<A: IpAddress, LI> {
    /// The specific address being listened on, or `None` for all addresses.
    pub(crate) addr: Option<SpecifiedAddr<A>>,
    /// The local identifier (i.e. port for TCP/UDP).
    pub(crate) identifier: LI,
}

/// The address of a listening socket.
#[derive(Copy, Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) struct ListenerAddr<A, D> {
    pub(crate) ip: A,
    pub(crate) device: Option<D>,
}

// The IP address and identifier (port) of a connected socket.
#[derive(Copy, Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) struct ConnIpAddr<A: IpAddress, LI, RI> {
    pub(crate) local: (SpecifiedAddr<A>, LI),
    pub(crate) remote: (SpecifiedAddr<A>, RI),
}

/// The address of a connected socket.
#[derive(Copy, Clone, Debug, Eq, GenericOverIp, Hash, PartialEq)]
pub(crate) struct ConnAddr<A: IpAddress, D, LI, RI> {
    pub(crate) ip: ConnIpAddr<A, LI, RI>,
    pub(crate) device: Option<D>,
}

/// Uninstantiable type used to implement [`SocketMapAddrSpec`] for addresses
/// with IP addresses and 16-bit local and remote port identifiers.
pub(crate) enum IpPortSpec {}

impl SocketMapAddrSpec for IpPortSpec {
    type RemoteIdentifier = NonZeroU16;
    type LocalIdentifier = NonZeroU16;
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
    From<ConnAddr<I::Addr, D, A::LocalIdentifier, A::RemoteIdentifier>> for AddrVec<I, D, A>
{
    fn from(conn: ConnAddr<I::Addr, D, A::LocalIdentifier, A::RemoteIdentifier>) -> Self {
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
                let _: (SpecifiedAddr<I::Addr>, A::RemoteIdentifier) = remote;
                Some(ListenerIpAddr { addr: Some(local_ip), identifier: local_identifier })
            }
            IpAddrVec::Listener(ListenerIpAddr { addr: Some(addr), identifier }) => {
                let _: SpecifiedAddr<I::Addr> = addr;
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
