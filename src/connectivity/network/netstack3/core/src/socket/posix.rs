// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! An implementation of POSIX-style socket conflict detection semantics on top
//! of [`SocketMapSpec`] that can be used to implement multiple types of
//! sockets.

use alloc::{vec, vec::Vec};
use core::{fmt::Debug, hash::Hash};

use net_types::ip::IpAddress;

use crate::{
    data_structures::socketmap::{IterShadows, Tagged},
    socket::{
        address::{AddrVecIter, ConnAddr, ConnIpAddr, IpAddrVec, ListenerAddr, ListenerIpAddr},
        AddrVec, IncompatibleError, RemoveResult, SocketAddrType, SocketMapAddrSpec,
        SocketMapAddrStateSpec,
    },
};

impl<A: SocketMapAddrSpec> From<ListenerIpAddr<A::IpAddr, A::LocalIdentifier>> for IpAddrVec<A> {
    fn from(listener: ListenerIpAddr<A::IpAddr, A::LocalIdentifier>) -> Self {
        IpAddrVec::Listener(listener)
    }
}

impl<A: SocketMapAddrSpec> From<ConnIpAddr<A::IpAddr, A::LocalIdentifier, A::RemoteIdentifier>>
    for IpAddrVec<A>
{
    fn from(conn: ConnIpAddr<A::IpAddr, A::LocalIdentifier, A::RemoteIdentifier>) -> Self {
        IpAddrVec::Connected(conn)
    }
}

impl<A: SocketMapAddrSpec> From<ListenerAddr<A::IpAddr, A::WeakDeviceId, A::LocalIdentifier>>
    for AddrVec<A>
{
    fn from(listener: ListenerAddr<A::IpAddr, A::WeakDeviceId, A::LocalIdentifier>) -> Self {
        AddrVec::Listen(listener)
    }
}

impl<A: SocketMapAddrSpec>
    From<ConnAddr<A::IpAddr, A::WeakDeviceId, A::LocalIdentifier, A::RemoteIdentifier>>
    for AddrVec<A>
{
    fn from(
        conn: ConnAddr<A::IpAddr, A::WeakDeviceId, A::LocalIdentifier, A::RemoteIdentifier>,
    ) -> Self {
        AddrVec::Conn(conn)
    }
}
pub(crate) struct PosixAddrVecIter<A: SocketMapAddrSpec>(AddrVecIter<A>);

impl<A: SocketMapAddrSpec> Iterator for PosixAddrVecIter<A> {
    type Item = AddrVec<A>;

    fn next(&mut self) -> Option<Self::Item> {
        let Self(it) = self;
        it.next()
    }
}

impl<A: SocketMapAddrSpec> PosixAddrVecIter<A> {
    #[cfg(test)]
    pub(crate) fn with_device(addr: impl Into<IpAddrVec<A>>, device: A::WeakDeviceId) -> Self {
        Self(AddrVecIter::with_device(addr.into(), device))
    }
}

impl<A: SocketMapAddrSpec> IterShadows for AddrVec<A> {
    type IterShadows = PosixAddrVecIter<A>;

    fn iter_shadows(&self) -> Self::IterShadows {
        let (socket_ip_addr, device) = match self.clone() {
            AddrVec::Conn(ConnAddr { ip, device }) => (ip.into(), device),
            AddrVec::Listen(ListenerAddr { ip, device }) => (ip.into(), device),
        };
        let mut iter = match device {
            Some(device) => AddrVecIter::with_device(socket_ip_addr, device),
            None => AddrVecIter::without_device(socket_ip_addr),
        };
        // Skip the first element, which is always `*self`.
        assert_eq!(iter.next().as_ref(), Some(self));
        PosixAddrVecIter(iter)
    }
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum PosixAddrState<T> {
    Exclusive(T),
    ReusePort(Vec<T>),
}

impl<'a, A: IpAddress, LI> From<&'a ListenerIpAddr<A, LI>> for SocketAddrType {
    fn from(ListenerIpAddr { addr, identifier: _ }: &'a ListenerIpAddr<A, LI>) -> Self {
        match addr {
            Some(_) => SocketAddrType::SpecificListener,
            None => SocketAddrType::AnyListener,
        }
    }
}

impl<'a, A: IpAddress, LI, RI> From<&'a ConnIpAddr<A, LI, RI>> for SocketAddrType {
    fn from(_: &'a ConnIpAddr<A, LI, RI>) -> Self {
        SocketAddrType::Connected
    }
}

#[derive(Copy, Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) enum PosixSharingOptions {
    Exclusive,
    ReusePort,
}

impl Default for PosixSharingOptions {
    fn default() -> Self {
        Self::Exclusive
    }
}

impl PosixSharingOptions {
    pub(crate) fn is_shareable_with_new_state(&self, new_state: PosixSharingOptions) -> bool {
        match (self, new_state) {
            (PosixSharingOptions::Exclusive, PosixSharingOptions::Exclusive) => false,
            (PosixSharingOptions::Exclusive, PosixSharingOptions::ReusePort) => false,
            (PosixSharingOptions::ReusePort, PosixSharingOptions::Exclusive) => false,
            (PosixSharingOptions::ReusePort, PosixSharingOptions::ReusePort) => true,
        }
    }

    pub(crate) fn is_reuse_port(&self) -> bool {
        match self {
            PosixSharingOptions::Exclusive => false,
            PosixSharingOptions::ReusePort => true,
        }
    }
}

#[derive(Copy, Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) struct PosixAddrVecTag {
    pub(crate) has_device: bool,
    pub(crate) addr_type: SocketAddrType,
    pub(crate) sharing: PosixSharingOptions,
}

impl<T, A: IpAddress, D, LI> Tagged<ListenerAddr<A, D, LI>> for PosixAddrState<T> {
    type Tag = PosixAddrVecTag;

    fn tag(&self, address: &ListenerAddr<A, D, LI>) -> Self::Tag {
        let ListenerAddr { ip, device } = address;
        PosixAddrVecTag {
            has_device: device.is_some(),
            addr_type: ip.into(),
            sharing: self.to_sharing_options(),
        }
    }
}

impl<T, A: IpAddress, D, LI, RI> Tagged<ConnAddr<A, D, LI, RI>> for PosixAddrState<T> {
    type Tag = PosixAddrVecTag;

    fn tag(&self, address: &ConnAddr<A, D, LI, RI>) -> Self::Tag {
        let ConnAddr { ip, device } = address;
        PosixAddrVecTag {
            has_device: device.is_some(),
            addr_type: ip.into(),
            sharing: self.to_sharing_options(),
        }
    }
}

pub(crate) trait ToPosixSharingOptions {
    fn to_sharing_options(&self) -> PosixSharingOptions;
}

impl ToPosixSharingOptions for PosixAddrVecTag {
    fn to_sharing_options(&self) -> PosixSharingOptions {
        let PosixAddrVecTag { has_device: _, addr_type: _, sharing } = self;
        *sharing
    }
}

impl<T> ToPosixSharingOptions for PosixAddrState<T> {
    fn to_sharing_options(&self) -> PosixSharingOptions {
        match self {
            PosixAddrState::Exclusive(_) => PosixSharingOptions::Exclusive,
            PosixAddrState::ReusePort(_) => PosixSharingOptions::ReusePort,
        }
    }
}

impl<T> ToPosixSharingOptions for (T, PosixSharingOptions) {
    fn to_sharing_options(&self) -> PosixSharingOptions {
        let (_state, sharing) = self;
        *sharing
    }
}

impl<I: Debug + Eq> SocketMapAddrStateSpec for PosixAddrState<I> {
    type Id = I;
    type SharingState = PosixSharingOptions;
    type Inserter<'a> = &'a mut Vec<I> where I: 'a;

    fn new(new_sharing_state: &PosixSharingOptions, id: I) -> Self {
        match new_sharing_state {
            PosixSharingOptions::Exclusive => Self::Exclusive(id),
            PosixSharingOptions::ReusePort => Self::ReusePort(vec![id]),
        }
    }

    fn contains_id(&self, id: &Self::Id) -> bool {
        match self {
            Self::Exclusive(x) => id == x,
            Self::ReusePort(ids) => ids.contains(id),
        }
    }

    fn try_get_inserter<'a, 'b>(
        &'b mut self,
        new_sharing_state: &'a PosixSharingOptions,
    ) -> Result<&'b mut Vec<I>, IncompatibleError> {
        match (self, new_sharing_state) {
            (PosixAddrState::Exclusive(_), _)
            | (PosixAddrState::ReusePort(_), PosixSharingOptions::Exclusive) => {
                Err(IncompatibleError)
            }
            (PosixAddrState::ReusePort(ids), PosixSharingOptions::ReusePort) => Ok(ids),
        }
    }

    fn could_insert(
        &self,
        new_sharing_state: &Self::SharingState,
    ) -> Result<(), IncompatibleError> {
        match (self, new_sharing_state) {
            (PosixAddrState::Exclusive(_), _) | (_, PosixSharingOptions::Exclusive) => {
                Err(IncompatibleError)
            }
            (PosixAddrState::ReusePort(_), PosixSharingOptions::ReusePort) => Ok(()),
        }
    }

    fn remove_by_id(&mut self, id: I) -> RemoveResult {
        match self {
            PosixAddrState::Exclusive(_) => RemoveResult::IsLast,
            PosixAddrState::ReusePort(ids) => {
                let index = ids.iter().position(|i| i == &id).expect("couldn't find ID to remove");
                assert_eq!(ids.swap_remove(index), id);
                if ids.is_empty() {
                    RemoveResult::IsLast
                } else {
                    RemoveResult::Success
                }
            }
        }
    }
}
