// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! General-purpose socket utilities common to device layer and IP layer
//! sockets.

pub(crate) mod address;
pub mod datagram;

use alloc::collections::HashMap;
use core::{
    convert::Infallible as Never, fmt::Debug, hash::Hash, marker::PhantomData, num::NonZeroUsize,
};

use derivative::Derivative;
use net_types::{
    ip::{Ip, IpAddress},
    AddrAndZone, SpecifiedAddr, Witness as _,
};

use crate::{
    data_structures::{
        id_map::{EntryKey, IdMap},
        id_map_collection::IdMapCollectionKey,
        socketmap::{
            Entry, IterShadows, OccupiedEntry as SocketMapOccupiedEntry, SocketMap, Tagged,
        },
    },
    device::{Id, StrongId, WeakId},
    error::{ExistsError, NotFoundError},
    socket::address::{AddrVecIter, ConnAddr, ListenerAddr, ListenerIpAddr},
};

/// Determines whether the provided address is underspecified by itself.
///
/// Some addresses are ambiguous and so must have a zone identifier in order
/// to be used in a socket address. This function returns true for IPv6
/// link-local addresses and false for all others.
pub(crate) fn must_have_zone<A: IpAddress>(addr: &SpecifiedAddr<A>) -> bool {
    try_into_null_zoned(addr).is_some()
}

/// Determines where a change in device is allowed given the local and remote
/// addresses.
pub(crate) fn can_device_change<A: IpAddress, W: WeakId<Strong = S>, S: StrongId>(
    local_ip: Option<&SpecifiedAddr<A>>,
    remote_ip: Option<&SpecifiedAddr<A>>,
    old_device: Option<&W>,
    new_device: Option<&S>,
) -> bool {
    let must_have_zone =
        local_ip.map_or(false, must_have_zone) || remote_ip.map_or(false, must_have_zone);

    if !must_have_zone {
        return true;
    }

    let old_device = old_device.as_ref().unwrap_or_else(|| {
        panic!("local_ip={:?} or remote_ip={:?} must have zone", local_ip, remote_ip)
    });

    new_device.as_ref().map_or(false, |new_device| old_device == new_device)
}

/// Converts into a [`AddrAndZone<A, ()>`] if the address requires a zone.
///
/// Otherwise returns `None`.
pub(crate) fn try_into_null_zoned<A: IpAddress>(
    addr: &SpecifiedAddr<A>,
) -> Option<AddrAndZone<A, ()>> {
    if addr.get().is_loopback() {
        return None;
    }
    AddrAndZone::new(addr.get(), ())
}

/// A bidirectional map between connection sockets and addresses.
///
/// A `ConnSocketMap` keeps addresses mapped by integer indexes, and allows for
/// constant-time mapping in either direction (though address -> index mappings
/// are via a hash map, and thus slower).
pub(crate) struct ConnSocketMap<A, S> {
    id_to_sock: IdMap<ConnSocketEntry<S, A>>,
    addr_to_id: HashMap<A, usize>,
}

/// An entry in a [`ConnSocketMap`].
#[derive(Debug, Eq, PartialEq)]
pub(crate) struct ConnSocketEntry<S, A> {
    pub(crate) sock: S,
    pub(crate) addr: A,
}

impl<A: Eq + Hash + Clone, S> ConnSocketMap<A, S> {
    pub(crate) fn insert(&mut self, addr: A, sock: S) -> usize {
        let id = self.id_to_sock.push(ConnSocketEntry { sock, addr: addr.clone() });
        assert_eq!(self.addr_to_id.insert(addr, id), None);
        id
    }
}

impl<A: Eq + Hash, S> ConnSocketMap<A, S> {
    pub(crate) fn get_id_by_addr(&self, addr: &A) -> Option<usize> {
        self.addr_to_id.get(addr).cloned()
    }

    pub(crate) fn get_sock_by_id(&self, id: usize) -> Option<&ConnSocketEntry<S, A>> {
        self.id_to_sock.get(id)
    }
}

impl<A: Eq + Hash, S> Default for ConnSocketMap<A, S> {
    fn default() -> ConnSocketMap<A, S> {
        ConnSocketMap { id_to_sock: IdMap::default(), addr_to_id: HashMap::default() }
    }
}

/// Specification for the identifiers in an [`AddrVec`].
///
/// This is a convenience trait for bundling together the local and remote
/// identifiers for a protocol.
pub(crate) trait SocketMapAddrSpec {
    /// The local identifier portion of a socket address.
    type LocalIdentifier: Clone + Debug + Hash + Eq;
    /// The remote identifier portion of a socket address.
    type RemoteIdentifier: Clone + Debug + Hash + Eq;
}

pub(crate) struct ListenerAddrInfo {
    pub(crate) has_device: bool,
    pub(crate) specified_addr: bool,
}

impl<A: IpAddress, D: Id, LI> ListenerAddr<A, D, LI> {
    pub(crate) fn info(&self) -> ListenerAddrInfo {
        let Self { device, ip: ListenerIpAddr { addr, identifier: _ } } = self;
        ListenerAddrInfo { has_device: device.is_some(), specified_addr: addr.is_some() }
    }
}

/// Specifies the types parameters for [`BoundSocketMap`] state as a single bundle.
pub(crate) trait SocketMapStateSpec {
    /// The tag value of a socket address vector entry.
    ///
    /// These values are derived from [`Self::ListenerAddrState`] and
    /// [`Self::ConnAddrState`].
    type AddrVecTag: Eq + Copy + Debug + 'static;

    fn listener_tag(info: ListenerAddrInfo, state: &Self::ListenerAddrState) -> Self::AddrVecTag;

    fn connected_tag(has_device: bool, state: &Self::ConnAddrState) -> Self::AddrVecTag;

    /// An identifier for a listening socket.
    type ListenerId: Clone + Debug;
    /// An identifier for a connected socket.
    type ConnId: Clone + Debug;

    /// The state stored for a listening socket that is used to determine
    /// whether sockets can share an address.
    type ListenerSharingState: Clone + Debug;

    /// The state stored for a connected socket that is used to determine
    /// whether sockets can share an address.
    type ConnSharingState: Clone + Debug;

    /// The state stored for a listener socket address.
    type ListenerAddrState: SocketMapAddrStateSpec<Id = Self::ListenerId, SharingState = Self::ListenerSharingState>
        + Debug;

    /// The state stored for a connected socket address.
    type ConnAddrState: SocketMapAddrStateSpec<Id = Self::ConnId, SharingState = Self::ConnSharingState>
        + Debug;
}

pub(crate) trait SocketStateSpec: SocketMapStateSpec {
    /// The state stored for a listening socket.
    type ListenerState: Debug;
    /// The state stored for a connected socket.
    type ConnState: Debug;
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct IncompatibleError;

pub(crate) trait Inserter<T> {
    /// Inserts the provided item and consumes `self`.
    ///
    /// Inserts a single item and consumes the inserter (thus preventing
    /// additional insertions).
    fn insert(self, item: T);
}

impl<'a, T, E: Extend<T>> Inserter<T> for &'a mut E {
    fn insert(self, item: T) {
        self.extend([item])
    }
}

impl<T> Inserter<T> for Never {
    fn insert(self, _: T) {
        match self {}
    }
}

/// Describes an entry in a [`SocketMap`] for a listener or connection address.
pub(crate) trait SocketMapAddrStateSpec {
    /// The type of ID that can be present at the address.
    type Id;

    /// The sharing state for the address.
    ///
    /// This can be used to determine whether a socket can be inserted at the
    /// address. Every socket has its own sharing state associated with it,
    /// though the sharing state is not necessarily stored in the address
    /// entry.
    type SharingState;

    /// The type of inserter returned by [`SocketMapAddrStateSpec::try_get_inserter`].
    type Inserter<'a>: Inserter<Self::Id> + 'a
    where
        Self: 'a,
        Self::Id: 'a;

    /// Creates a new `Self` holding the provided socket with the given new
    /// sharing state at the specified address.
    fn new(new_sharing_state: &Self::SharingState, id: Self::Id) -> Self;

    /// Looks up the ID in self, returning `true` if it is present.
    fn contains_id(&self, id: &Self::Id) -> bool;

    /// Enables insertion in `self` for a new socket with the provided sharing
    /// state.
    ///
    /// If the new state is incompatible with the existing socket(s),
    /// implementations of this function should return `Err(IncompatibleError)`.
    /// If `Ok(x)` is returned, calling `x.insert(y)` will insert `y` into
    /// `self`.
    fn try_get_inserter<'a, 'b>(
        &'b mut self,
        new_sharing_state: &'a Self::SharingState,
    ) -> Result<Self::Inserter<'b>, IncompatibleError>;

    /// Returns `Ok` if an entry with the given sharing state could be added
    /// to `self`.
    ///
    /// If this returns `Ok`, `try_get_dest` should succeed.
    fn could_insert(&self, new_sharing_state: &Self::SharingState)
        -> Result<(), IncompatibleError>;

    /// Removes the given socket from the existing state.
    ///
    /// Implementations should assume that `id` is contained in `self`.
    fn remove_by_id(&mut self, id: Self::Id) -> RemoveResult;
}

pub(crate) trait SocketMapAddrStateUpdateSharingSpec: SocketMapAddrStateSpec {
    fn try_update_sharing(
        &mut self,
        id: Self::Id,
        new_sharing_state: &Self::SharingState,
    ) -> Result<(), IncompatibleError>;
}

pub(crate) trait SocketMapConflictPolicy<Addr, SharingState, I: Ip, D: Id, A: SocketMapAddrSpec>:
    SocketMapStateSpec
{
    /// Checks whether a new socket with the provided state can be inserted at
    /// the given address in the existing socket map, returning an error
    /// otherwise.
    ///
    /// Implementations of this function should check for any potential
    /// conflicts that would arise when inserting a socket with state
    /// `new_sharing_state` into a new or existing entry at `addr` in
    /// `socketmap`.
    fn check_insert_conflicts(
        new_sharing_state: &SharingState,
        addr: &Addr,
        socketmap: &SocketMap<AddrVec<I, D, A>, Bound<Self>>,
    ) -> Result<(), InsertError>;
}

pub(crate) trait SocketMapUpdateSharingPolicy<Addr, SharingState, I: Ip, D: Id, A>:
    SocketMapConflictPolicy<Addr, SharingState, I, D, A>
where
    A: SocketMapAddrSpec,
{
    fn allows_sharing_update(
        socketmap: &SocketMap<AddrVec<I, D, A>, Bound<Self>>,
        addr: &Addr,
        old_sharing: &SharingState,
        new_sharing_state: &SharingState,
    ) -> Result<(), UpdateSharingError>;
}

#[derive(Derivative)]
#[derivative(Debug(bound = "S::ListenerAddrState: Debug, S::ConnAddrState: Debug"))]
pub(crate) enum Bound<S: SocketMapStateSpec + ?Sized> {
    Listen(S::ListenerAddrState),
    Conn(S::ConnAddrState),
}

#[derive(Derivative)]
#[derivative(Debug(bound = "D: Debug"))]
pub(crate) enum SocketState<I: Ip, D, A: SocketMapAddrSpec, S: SocketStateSpec> {
    Listener(
        (S::ListenerState, S::ListenerSharingState, ListenerAddr<I::Addr, D, A::LocalIdentifier>),
    ),
    Connected(
        (
            S::ConnState,
            S::ConnSharingState,
            ConnAddr<I::Addr, D, A::LocalIdentifier, A::RemoteIdentifier>,
        ),
    ),
}

/// An "address vector" type that can hold any address in a [`SocketMap`].
///
/// This is a "vector" in the mathematical sense, in that it denotes an address
/// in a space. Here, the space is the possible addresses to which a socket
/// receiving IP packets can be bound.
///
/// `AddrVec`s are used as keys for the `SocketMap` type. Since an incoming
/// packet can match more than one address, for each incoming packet there is a
/// set of possible `AddrVec` keys whose entries (sockets) in a `SocketMap`
/// might receive the packet.
///
/// This set of keys can be ordered by precedence as described in the
/// documentation for [`AddrVecIter`]. Calling [`IterShadows::iter_shadows`] on
/// an instance will produce the sequence of addresses it has precedence over.
#[derive(Derivative)]
#[derivative(
    Debug(bound = "D: Debug"),
    Clone(bound = "D: Clone"),
    Eq(bound = "D: Eq"),
    PartialEq(bound = "D: PartialEq"),
    Hash(bound = "D: Hash")
)]
pub(crate) enum AddrVec<I: Ip, D, A: SocketMapAddrSpec + ?Sized> {
    Listen(ListenerAddr<I::Addr, D, A::LocalIdentifier>),
    Conn(ConnAddr<I::Addr, D, A::LocalIdentifier, A::RemoteIdentifier>),
}

impl<I: Ip, D: Id, A: SocketMapAddrSpec, S: SocketMapStateSpec + ?Sized> Tagged<AddrVec<I, D, A>>
    for Bound<S>
{
    type Tag = S::AddrVecTag;
    fn tag(&self, address: &AddrVec<I, D, A>) -> Self::Tag {
        match (self, address) {
            (Bound::Listen(l), AddrVec::Listen(addr)) => S::listener_tag(addr.info(), l),
            (Bound::Conn(c), AddrVec::Conn(ConnAddr { device, ip: _ })) => {
                S::connected_tag(device.is_some(), c)
            }
            (Bound::Listen(_), AddrVec::Conn(_)) => {
                unreachable!("found listen state for conn addr")
            }
            (Bound::Conn(_), AddrVec::Listen(_)) => {
                unreachable!("found conn state for listen addr")
            }
        }
    }
}

impl<I: Ip, D: Id, A: SocketMapAddrSpec> IterShadows for AddrVec<I, D, A> {
    type IterShadows = AddrVecIter<I, D, A>;

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
        iter
    }
}

#[derive(Copy, Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) enum SocketAddrType {
    AnyListener,
    SpecificListener,
    Connected,
}

#[derive(Copy, Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) struct SocketAddrTypeTag<S> {
    pub(crate) has_device: bool,
    pub(crate) addr_type: SocketAddrType,
    pub(crate) sharing: S,
}

impl<'a, A: IpAddress, D, LI, S> From<(&'a ListenerAddr<A, D, LI>, S)> for SocketAddrTypeTag<S> {
    fn from((addr, sharing): (&'a ListenerAddr<A, D, LI>, S)) -> Self {
        let ListenerAddr { ip: ListenerIpAddr { addr, identifier: _ }, device } = addr;
        SocketAddrTypeTag {
            has_device: device.is_some(),
            addr_type: if addr.is_some() {
                SocketAddrType::SpecificListener
            } else {
                SocketAddrType::AnyListener
            },
            sharing,
        }
    }
}

impl<'a, A: IpAddress, D, LI, RI, S> From<(&'a ConnAddr<A, D, LI, RI>, S)>
    for SocketAddrTypeTag<S>
{
    fn from((addr, sharing): (&'a ConnAddr<A, D, LI, RI>, S)) -> Self {
        let ConnAddr { ip: _, device } = addr;
        SocketAddrTypeTag {
            has_device: device.is_some(),
            addr_type: SocketAddrType::Connected,
            sharing,
        }
    }
}

/// The result of attempting to remove a socket from a collection of sockets.
pub(crate) enum RemoveResult {
    /// The value was removed successfully.
    Success,
    /// The value is the last value in the collection so the entire collection
    /// should be removed.
    IsLast,
}

#[derive(Derivative)]
#[derivative(Clone(bound = "S::ListenerId: Clone, S::ConnId: Clone"), Debug(bound = ""))]
pub(crate) enum SocketId<S: SocketMapStateSpec> {
    Listener(S::ListenerId),
    Connection(S::ConnId),
}

impl<S: SocketMapStateSpec> SocketId<S> {
    const LISTENER_VARIANT: usize = 0;
    const CONNECTION_VARIANT: usize = 0;
}

// TODO(https://fxbug.dev/126141): Remove this when it is no longer used for
// TCP socket lookup.
impl<S: SocketMapStateSpec> IdMapCollectionKey for SocketId<S>
where
    S::ListenerId: EntryKey,
    S::ConnId: EntryKey,
{
    const VARIANT_COUNT: NonZeroUsize = const_unwrap::const_unwrap_option(NonZeroUsize::new(2));
    fn get_id(&self) -> usize {
        match self {
            Self::Listener(l) => l.get_key_index(),
            Self::Connection(c) => c.get_key_index(),
        }
    }
    fn get_variant(&self) -> usize {
        match self {
            Self::Listener(_) => Self::LISTENER_VARIANT,
            Self::Connection(_) => Self::CONNECTION_VARIANT,
        }
    }
}

/// A map from socket addresses to sockets.
///
/// The types of keys and IDs is determined by the [`SocketMapStateSpec`]
/// parameter. Each listener and connected socket stores additional state.
/// Listener and connected sockets are keyed independently, but share the same
/// address vector space. Conflicts are detected on attempted insertion of new
/// sockets.
///
/// Listener addresses map to listener-address-specific state, and likewise
/// with connected addresses. Depending on protocol (determined by the
/// `SocketMapStateSpec` protocol), these address states can hold one or more
/// socket identifiers (e.g. UDP sockets with `SO_REUSEPORT` set can share an
/// address).
#[derive(Derivative)]
#[derivative(Default(bound = ""))]
pub(crate) struct BoundSocketMap<I: Ip, D: Id, A: SocketMapAddrSpec, S: SocketMapStateSpec> {
    addr_to_state: SocketMap<AddrVec<I, D, A>, Bound<S>>,
}

/// Uninstantiable tag type for denoting listening sockets.
pub(crate) enum Listener {}
/// Uninstantiable tag type for denoting connected sockets.
pub(crate) enum Connection {}

/// View struct over one type of sockets in a [`BoundSocketMap`].
pub(crate) struct Sockets<AddrToStateMap, SocketType>(AddrToStateMap, PhantomData<SocketType>);

impl<
        'a,
        I: Ip,
        D: Id,
        SocketType: ConvertSocketMapState<I, D, A, S>,
        A: SocketMapAddrSpec,
        S: SocketMapStateSpec,
    > Sockets<&'a SocketMap<AddrVec<I, D, A>, Bound<S>>, SocketType>
where
    S: SocketMapConflictPolicy<SocketType::Addr, SocketType::SharingState, I, D, A>,
{
    /// Returns the state at an address, if there is any.
    pub(crate) fn get_by_addr(self, addr: &SocketType::Addr) -> Option<&'a SocketType::AddrState> {
        let Self(addr_to_state, _marker) = self;
        addr_to_state.get(&SocketType::to_addr_vec(addr)).map(|state| {
            SocketType::from_bound_ref(state)
                .unwrap_or_else(|| unreachable!("found {:?} for address {:?}", state, addr))
        })
    }

    /// Returns `Ok(())` if a socket could be inserted, otherwise an error.
    ///
    /// Goes through a dry run of inserting a socket at the given address and
    /// with the given sharing state, returning `Ok(())` if the insertion would
    /// succeed, otherwise the error that would be returned.
    pub(crate) fn could_insert(
        self,
        addr: &SocketType::Addr,
        sharing: &SocketType::SharingState,
    ) -> Result<(), InsertError> {
        let Self(addr_to_state, _) = self;
        match self.get_by_addr(addr) {
            Some(state) => {
                state.could_insert(sharing).map_err(|IncompatibleError| InsertError::Exists)
            }
            None => S::check_insert_conflicts(&sharing, &addr, &addr_to_state),
        }
    }
}

#[derive(Derivative)]
#[derivative(Debug(bound = ""))]
pub(crate) struct SocketStateEntry<
    'a,
    I: Ip,
    D: Id,
    A: SocketMapAddrSpec,
    S: SocketMapStateSpec,
    SocketType,
> {
    id: SocketId<S>,
    addr_entry: SocketMapOccupiedEntry<'a, AddrVec<I, D, A>, Bound<S>>,
    _marker: PhantomData<SocketType>,
}

impl<
        'a,
        I: Ip,
        D: Id,
        SocketType: ConvertSocketMapState<I, D, A, S>,
        A: SocketMapAddrSpec,
        S: SocketMapStateSpec
            + SocketMapConflictPolicy<SocketType::Addr, SocketType::SharingState, I, D, A>,
    > Sockets<&'a mut SocketMap<AddrVec<I, D, A>, Bound<S>>, SocketType>
where
    SocketType::SharingState: Clone,
    SocketType::Id: Clone,
{
    pub(crate) fn try_insert(
        self,
        socket_addr: SocketType::Addr,
        tag_state: SocketType::SharingState,
        id: SocketType::Id,
    ) -> Result<SocketStateEntry<'a, I, D, A, S, SocketType>, (InsertError, SocketType::SharingState)>
    {
        self.try_insert_with(socket_addr, tag_state, |_addr, _sharing| id)
    }

    // TODO(https://fxbug.dev/126141): remove this and inline its contents into
    // try_insert once TCP socket IDs are not being constructed inside the
    // make_id callback.
    pub(crate) fn try_insert_with(
        self,
        socket_addr: SocketType::Addr,
        tag_state: SocketType::SharingState,
        make_id: impl FnOnce(SocketType::Addr, SocketType::SharingState) -> SocketType::Id,
    ) -> Result<SocketStateEntry<'a, I, D, A, S, SocketType>, (InsertError, SocketType::SharingState)>
    {
        let Self(addr_to_state, _) = self;
        match S::check_insert_conflicts(&tag_state, &socket_addr, &addr_to_state) {
            Err(e) => return Err((e, tag_state)),
            Ok(()) => (),
        };

        let addr = SocketType::to_addr_vec(&socket_addr);

        match addr_to_state.entry(addr) {
            Entry::Occupied(mut o) => {
                let id = o.map_mut(|bound| {
                    let bound = match SocketType::from_bound_mut(bound) {
                        Some(bound) => bound,
                        None => unreachable!("found {:?} for address {:?}", bound, socket_addr),
                    };
                    match <SocketType::AddrState as SocketMapAddrStateSpec>::try_get_inserter(
                        bound, &tag_state,
                    ) {
                        Ok(v) => {
                            let id = make_id(socket_addr, tag_state);
                            v.insert(id.clone());
                            Ok(SocketType::to_socket_id(id))
                        }
                        Err(IncompatibleError) => Err((InsertError::Exists, tag_state)),
                    }
                })?;
                Ok(SocketStateEntry { id, addr_entry: o, _marker: Default::default() })
            }
            Entry::Vacant(v) => {
                let id = make_id(socket_addr, tag_state.clone());
                let addr_entry = v.insert(SocketType::to_bound(SocketType::AddrState::new(
                    &tag_state,
                    id.clone(),
                )));
                let id = SocketType::to_socket_id(id);
                Ok(SocketStateEntry { id, addr_entry, _marker: Default::default() })
            }
        }
    }

    pub(crate) fn entry(
        self,
        id: &SocketType::Id,
        addr: &SocketType::Addr,
    ) -> Option<SocketStateEntry<'a, I, D, A, S, SocketType>> {
        let Self(addr_to_state, _) = self;
        let addr_entry = match addr_to_state.entry(SocketType::to_addr_vec(addr)) {
            Entry::Vacant(_) => return None,
            Entry::Occupied(o) => o,
        };
        let state = SocketType::from_bound_ref(addr_entry.get())?;

        state.contains_id(id).then_some(SocketStateEntry {
            id: SocketType::to_socket_id(id.clone()),
            addr_entry,
            _marker: PhantomData::default(),
        })
    }

    pub(crate) fn remove(
        self,
        id: &SocketType::Id,
        addr: &SocketType::Addr,
    ) -> Result<(), NotFoundError> {
        self.entry(id, addr)
            .map(|entry| {
                entry.remove();
            })
            .ok_or(NotFoundError)
    }
}

#[derive(Debug)]
pub(crate) struct UpdateSharingError;

impl<
        'a,
        I: Ip,
        D: Id,
        SocketType: ConvertSocketMapState<I, D, A, S>,
        A: SocketMapAddrSpec,
        S: SocketMapStateSpec,
    > SocketStateEntry<'a, I, D, A, S, SocketType>
where
    SocketType::Id: Clone,
{
    pub(crate) fn get_addr(&self) -> &SocketType::Addr {
        let Self { id: _, addr_entry, _marker } = self;
        SocketType::from_addr_vec_ref(addr_entry.key())
    }

    pub(crate) fn id(&self) -> SocketType::Id {
        let Self { id, addr_entry: _, _marker } = self;
        SocketType::from_socket_id_ref(id).clone()
    }

    pub(crate) fn try_update_addr(
        self,
        new_addr: SocketType::Addr,
    ) -> Result<Self, (ExistsError, Self)> {
        let Self { id, addr_entry, _marker } = self;

        let new_addrvec = SocketType::to_addr_vec(&new_addr);
        let old_addr = addr_entry.key().clone();
        let (addr_state, addr_to_state) = addr_entry.remove_from_map();
        let addr_to_state = match addr_to_state.entry(new_addrvec) {
            Entry::Occupied(o) => o.into_map(),
            Entry::Vacant(v) => {
                if v.descendant_counts().len() != 0 {
                    v.into_map()
                } else {
                    let new_addr_entry = v.insert(addr_state);
                    return Ok(SocketStateEntry { id, addr_entry: new_addr_entry, _marker });
                }
            }
        };
        let to_restore = addr_state;
        // Restore the old state before returning an error.
        let addr_entry = match addr_to_state.entry(old_addr) {
            Entry::Occupied(_) => unreachable!("just-removed-from entry is occupied"),
            Entry::Vacant(v) => v.insert(to_restore),
        };
        return Err((ExistsError, SocketStateEntry { id, addr_entry, _marker }));
    }

    pub(crate) fn remove(self) {
        let Self { id, mut addr_entry, _marker } = self;
        let addr = addr_entry.key().clone();
        match addr_entry.map_mut(|value| {
            let value = match SocketType::from_bound_mut(value) {
                Some(value) => value,
                None => unreachable!("found {:?} for address {:?}", value, addr),
            };
            value.remove_by_id(SocketType::from_socket_id_ref(&id).clone())
        }) {
            RemoveResult::Success => (),
            RemoveResult::IsLast => {
                let _: Bound<S> = addr_entry.remove();
            }
        }
    }

    pub(crate) fn try_update_sharing(
        self,
        old_sharing_state: &SocketType::SharingState,
        new_sharing_state: SocketType::SharingState,
    ) -> Result<(), UpdateSharingError>
    where
        SocketType::AddrState: SocketMapAddrStateUpdateSharingSpec,
        S: SocketMapUpdateSharingPolicy<SocketType::Addr, SocketType::SharingState, I, D, A>,
    {
        let Self { id, mut addr_entry, _marker } = self;
        let addr = SocketType::from_addr_vec_ref(addr_entry.key());

        S::allows_sharing_update(
            addr_entry.get_map(),
            addr,
            old_sharing_state,
            &new_sharing_state,
        )?;

        addr_entry
            .map_mut(|value| {
                let value = match SocketType::from_bound_mut(value) {
                    Some(value) => value,
                    // We shouldn't ever be storing listener state in a bound
                    // address, or bound state in a listener address. Doing so means
                    // we've got a serious bug.
                    None => unreachable!("found invalid state {:?}", value),
                };

                value.try_update_sharing(
                    SocketType::from_socket_id_ref(&id).clone(),
                    &new_sharing_state,
                )
            })
            .map_err(|IncompatibleError| UpdateSharingError)
    }
}

impl<I: Ip, D: Id, A: SocketMapAddrSpec, S> BoundSocketMap<I, D, A, S>
where
    AddrVec<I, D, A>: IterShadows,
    S: SocketMapStateSpec,
{
    pub(crate) fn listeners(&self) -> Sockets<&SocketMap<AddrVec<I, D, A>, Bound<S>>, Listener>
    where
        S: SocketMapConflictPolicy<
            ListenerAddr<I::Addr, D, A::LocalIdentifier>,
            <S as SocketMapStateSpec>::ListenerSharingState,
            I,
            D,
            A,
        >,
        S::ListenerAddrState:
            SocketMapAddrStateSpec<Id = S::ListenerId, SharingState = S::ListenerSharingState>,
    {
        let Self { addr_to_state } = self;
        Sockets(addr_to_state, Default::default())
    }

    pub(crate) fn listeners_mut(
        &mut self,
    ) -> Sockets<&mut SocketMap<AddrVec<I, D, A>, Bound<S>>, Listener>
    where
        S: SocketMapConflictPolicy<
            ListenerAddr<I::Addr, D, A::LocalIdentifier>,
            <S as SocketMapStateSpec>::ListenerSharingState,
            I,
            D,
            A,
        >,
        S::ListenerAddrState:
            SocketMapAddrStateSpec<Id = S::ListenerId, SharingState = S::ListenerSharingState>,
    {
        let Self { addr_to_state } = self;
        Sockets(addr_to_state, Default::default())
    }

    pub(crate) fn conns(&self) -> Sockets<&SocketMap<AddrVec<I, D, A>, Bound<S>>, Connection>
    where
        S: SocketMapConflictPolicy<
            ConnAddr<I::Addr, D, A::LocalIdentifier, A::RemoteIdentifier>,
            <S as SocketMapStateSpec>::ConnSharingState,
            I,
            D,
            A,
        >,
        S::ConnAddrState:
            SocketMapAddrStateSpec<Id = S::ConnId, SharingState = S::ConnSharingState>,
    {
        let Self { addr_to_state } = self;
        Sockets(addr_to_state, Default::default())
    }

    pub(crate) fn conns_mut(
        &mut self,
    ) -> Sockets<&mut SocketMap<AddrVec<I, D, A>, Bound<S>>, Connection>
    where
        S: SocketMapConflictPolicy<
            ConnAddr<I::Addr, D, A::LocalIdentifier, A::RemoteIdentifier>,
            <S as SocketMapStateSpec>::ConnSharingState,
            I,
            D,
            A,
        >,
        S::ConnAddrState:
            SocketMapAddrStateSpec<Id = S::ConnId, SharingState = S::ConnSharingState>,
    {
        let Self { addr_to_state } = self;
        Sockets(addr_to_state, Default::default())
    }

    #[cfg(test)]
    pub(crate) fn iter_addrs(&self) -> impl Iterator<Item = &AddrVec<I, D, A>> {
        let Self { addr_to_state } = self;
        addr_to_state.iter().map(|(a, _v): (_, &Bound<S>)| a)
    }

    pub(crate) fn get_shadower_counts(&self, addr: &AddrVec<I, D, A>) -> usize {
        let Self { addr_to_state } = self;
        addr_to_state.descendant_counts(&addr).map(|(_sharing, size)| size.get()).sum()
    }
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum InsertError {
    ShadowAddrExists,
    Exists,
    ShadowerExists,
    IndirectConflict,
}

/// Helper trait for converting between [`AddrVec`] and [`Bound`] and their
/// variants.
pub(crate) trait ConvertSocketMapState<I: Ip, D, A: SocketMapAddrSpec, S: SocketMapStateSpec> {
    type Id;
    type SharingState;
    type Addr: Debug;
    type AddrState: SocketMapAddrStateSpec<Id = Self::Id, SharingState = Self::SharingState>;

    fn to_addr_vec(addr: &Self::Addr) -> AddrVec<I, D, A>;
    fn from_addr_vec_ref(addr: &AddrVec<I, D, A>) -> &Self::Addr;
    fn from_bound_ref(bound: &Bound<S>) -> Option<&Self::AddrState>;
    fn from_bound_mut(bound: &mut Bound<S>) -> Option<&mut Self::AddrState>;
    fn to_bound(state: Self::AddrState) -> Bound<S>;
    fn to_socket_id(id: Self::Id) -> SocketId<S>;
    fn from_socket_id_ref(id: &SocketId<S>) -> &Self::Id;
}

/// Helper trait for converting between [`SocketState`] and its variants.
pub(crate) trait ConvertSocketTypeState<I: Ip, D, A: SocketMapAddrSpec, S: SocketStateSpec>:
    ConvertSocketMapState<I, D, A, S>
{
    type State;
    fn from_socket_state_ref(
        socket_state: &SocketState<I, D, A, S>,
    ) -> &(Self::State, Self::SharingState, Self::Addr);
    fn from_socket_state_mut(
        socket_state: &mut SocketState<I, D, A, S>,
    ) -> &mut (Self::State, Self::SharingState, Self::Addr);
    fn to_socket_state(
        state: (Self::State, Self::SharingState, Self::Addr),
    ) -> SocketState<I, D, A, S>;
    fn from_socket_state(
        socket_state: SocketState<I, D, A, S>,
    ) -> (Self::State, Self::SharingState, Self::Addr);
}

impl<I: Ip, D: Id, A: SocketMapAddrSpec, S: SocketMapStateSpec> ConvertSocketMapState<I, D, A, S>
    for Listener
{
    type Id = S::ListenerId;
    type SharingState = S::ListenerSharingState;
    type Addr = ListenerAddr<I::Addr, D, A::LocalIdentifier>;
    type AddrState = S::ListenerAddrState;
    fn to_addr_vec(addr: &Self::Addr) -> AddrVec<I, D, A> {
        AddrVec::Listen(addr.clone())
    }

    fn from_addr_vec_ref(addr: &AddrVec<I, D, A>) -> &Self::Addr {
        match addr {
            AddrVec::Listen(l) => l,
            AddrVec::Conn(c) => unreachable!("conn addr for listener: {c:?}"),
        }
    }

    fn from_bound_ref(bound: &Bound<S>) -> Option<&S::ListenerAddrState> {
        match bound {
            Bound::Listen(l) => Some(l),
            Bound::Conn(_c) => None,
        }
    }

    fn from_bound_mut(bound: &mut Bound<S>) -> Option<&mut S::ListenerAddrState> {
        match bound {
            Bound::Listen(l) => Some(l),
            Bound::Conn(_c) => None,
        }
    }

    fn to_bound(state: S::ListenerAddrState) -> Bound<S> {
        Bound::Listen(state)
    }
    fn from_socket_id_ref(id: &SocketId<S>) -> &Self::Id {
        match id {
            SocketId::Listener(id) => id,
            SocketId::Connection(_) => unreachable!("connection ID for listener"),
        }
    }
    fn to_socket_id(id: Self::Id) -> SocketId<S> {
        SocketId::Listener(id)
    }
}

impl<I: Ip, D: Id, A: SocketMapAddrSpec, S: SocketStateSpec> ConvertSocketTypeState<I, D, A, S>
    for Listener
{
    type State = S::ListenerState;
    fn from_socket_state(
        socket_state: SocketState<I, D, A, S>,
    ) -> (Self::State, Self::SharingState, Self::Addr) {
        match socket_state {
            SocketState::Listener(state) => state,
            SocketState::Connected(state) => {
                unreachable!("connection state for listener: {state:?}")
            }
        }
    }

    fn from_socket_state_mut(
        socket_state: &mut SocketState<I, D, A, S>,
    ) -> &mut (Self::State, Self::SharingState, Self::Addr) {
        match socket_state {
            SocketState::Listener(state) => state,
            SocketState::Connected(state) => {
                unreachable!("connection state for listener: {state:?}")
            }
        }
    }

    fn from_socket_state_ref(
        socket_state: &SocketState<I, D, A, S>,
    ) -> &(Self::State, Self::SharingState, Self::Addr) {
        match socket_state {
            SocketState::Listener(state) => state,
            SocketState::Connected(state) => {
                unreachable!("connection state for listener: {state:?}")
            }
        }
    }

    fn to_socket_state(
        state: (Self::State, Self::SharingState, Self::Addr),
    ) -> SocketState<I, D, A, S> {
        SocketState::Listener(state)
    }
}

impl<I: Ip, D: Id, A: SocketMapAddrSpec, S: SocketMapStateSpec> ConvertSocketMapState<I, D, A, S>
    for Connection
{
    type Id = S::ConnId;
    type SharingState = S::ConnSharingState;
    type Addr = ConnAddr<I::Addr, D, A::LocalIdentifier, A::RemoteIdentifier>;
    type AddrState = S::ConnAddrState;
    fn to_addr_vec(addr: &Self::Addr) -> AddrVec<I, D, A> {
        AddrVec::Conn(addr.clone())
    }

    fn from_addr_vec_ref(addr: &AddrVec<I, D, A>) -> &Self::Addr {
        match addr {
            AddrVec::Conn(c) => c,
            AddrVec::Listen(l) => unreachable!("listener addr for conn: {l:?}"),
        }
    }

    fn from_bound_ref(bound: &Bound<S>) -> Option<&S::ConnAddrState> {
        match bound {
            Bound::Listen(_l) => None,
            Bound::Conn(c) => Some(c),
        }
    }

    fn from_bound_mut(bound: &mut Bound<S>) -> Option<&mut S::ConnAddrState> {
        match bound {
            Bound::Listen(_l) => None,
            Bound::Conn(c) => Some(c),
        }
    }

    fn to_bound(state: S::ConnAddrState) -> Bound<S> {
        Bound::Conn(state)
    }

    fn from_socket_id_ref(id: &SocketId<S>) -> &Self::Id {
        match id {
            SocketId::Connection(id) => id,
            SocketId::Listener(_) => unreachable!("listener ID for connection"),
        }
    }
    fn to_socket_id(id: Self::Id) -> SocketId<S> {
        SocketId::Connection(id)
    }
}

impl<I: Ip, D: Id, A: SocketMapAddrSpec, S: SocketStateSpec> ConvertSocketTypeState<I, D, A, S>
    for Connection
{
    type State = S::ConnState;
    fn from_socket_state(
        socket_state: SocketState<I, D, A, S>,
    ) -> (Self::State, Self::SharingState, Self::Addr) {
        match socket_state {
            SocketState::Connected(state) => state,
            SocketState::Listener(state) => {
                unreachable!("listener state for connection: {state:?}")
            }
        }
    }

    fn from_socket_state_mut(
        socket_state: &mut SocketState<I, D, A, S>,
    ) -> &mut (Self::State, Self::SharingState, Self::Addr) {
        match socket_state {
            SocketState::Connected(state) => state,
            SocketState::Listener(state) => {
                unreachable!("listener state for connection: {state:?}")
            }
        }
    }

    fn from_socket_state_ref(
        socket_state: &SocketState<I, D, A, S>,
    ) -> &(Self::State, Self::SharingState, Self::Addr) {
        match socket_state {
            SocketState::Connected(state) => state,
            SocketState::Listener(state) => {
                unreachable!("listener state for connection: {state:?}")
            }
        }
    }

    fn to_socket_state(
        state: (Self::State, Self::SharingState, Self::Addr),
    ) -> SocketState<I, D, A, S> {
        SocketState::Connected(state)
    }
}

#[cfg(test)]
mod tests {
    use alloc::{collections::HashSet, vec, vec::Vec};

    use assert_matches::assert_matches;
    use net_declare::{net_ip_v4, net_ip_v6};
    use net_types::{
        ip::{Ipv4, Ipv4Addr, Ipv6, Ipv6Addr},
        SpecifiedAddr,
    };
    use test_case::test_case;

    use crate::{
        device::testutil::{FakeDeviceId, FakeWeakDeviceId},
        socket::address::{ConnIpAddr, ListenerIpAddr},
        testutil::set_logger_for_test,
    };

    use super::*;

    #[test_case(net_ip_v4!("8.8.8.8"))]
    #[test_case(net_ip_v4!("127.0.0.1"))]
    #[test_case(net_ip_v4!("127.0.8.9"))]
    #[test_case(net_ip_v4!("224.1.2.3"))]
    fn must_never_have_zone_ipv4(addr: Ipv4Addr) {
        // No IPv4 addresses are allowed to have a zone.
        let addr = SpecifiedAddr::new(addr).unwrap();
        assert_eq!(must_have_zone(&addr), false);
    }

    #[test_case(net_ip_v6!("1::2:3"), false)]
    #[test_case(net_ip_v6!("::1"), false; "localhost")]
    #[test_case(net_ip_v6!("1::"), false)]
    #[test_case(net_ip_v6!("ff03:1:2:3::1"), false)]
    #[test_case(net_ip_v6!("ff02:1:2:3::1"), true)]
    #[test_case(Ipv6::ALL_NODES_LINK_LOCAL_MULTICAST_ADDRESS.get(), true)]
    #[test_case(net_ip_v6!("fe80::1"), true)]
    fn must_have_zone_ipv6(addr: Ipv6Addr, must_have: bool) {
        // Only link-local unicast and multicast addresses are allowed to have
        // zones.
        let addr = SpecifiedAddr::new(addr).unwrap();
        assert_eq!(must_have_zone(&addr), must_have);
    }

    #[test]
    fn try_into_null_zoned_ipv6() {
        assert_eq!(try_into_null_zoned(&Ipv6::LOOPBACK_ADDRESS), None);
        let zoned = Ipv6::ALL_NODES_LINK_LOCAL_MULTICAST_ADDRESS.into_specified();
        const ZONE: u32 = 5;
        assert_eq!(
            try_into_null_zoned(&zoned).map(|a| a.map_zone(|()| ZONE)),
            Some(AddrAndZone::new(zoned.get(), ZONE).unwrap())
        );
    }

    enum FakeSpec {}

    #[derive(Copy, Clone, Eq, PartialEq, Debug, Hash)]
    struct Listener(usize);

    #[derive(PartialEq, Eq, Debug)]
    struct Multiple<T>(char, Vec<T>);

    impl<T> Multiple<T> {
        fn tag(&self) -> char {
            let Multiple(c, _) = self;
            *c
        }
    }

    #[derive(Copy, Clone, Eq, PartialEq, Debug, Hash)]
    struct Conn(usize);

    enum FakeAddrSpec {}

    impl SocketMapAddrSpec for FakeAddrSpec {
        type LocalIdentifier = u16;
        type RemoteIdentifier = ();
    }

    impl SocketMapStateSpec for FakeSpec {
        type AddrVecTag = char;

        type ListenerId = Listener;
        type ConnId = Conn;

        type ListenerSharingState = char;
        type ConnSharingState = char;

        type ListenerAddrState = Multiple<Listener>;
        type ConnAddrState = Multiple<Conn>;

        fn listener_tag(_: ListenerAddrInfo, state: &Self::ListenerAddrState) -> Self::AddrVecTag {
            state.tag()
        }

        fn connected_tag(_has_device: bool, state: &Self::ConnAddrState) -> Self::AddrVecTag {
            state.tag()
        }
    }

    type FakeBoundSocketMap =
        BoundSocketMap<Ipv4, FakeWeakDeviceId<FakeDeviceId>, FakeAddrSpec, FakeSpec>;

    /// Generator for unique socket IDs that don't have any state.
    ///
    /// Calling [`FakeSocketIdGen::next`] returns a unique ID.
    #[derive(Default)]
    struct FakeSocketIdGen {
        next_id: usize,
    }

    impl FakeSocketIdGen {
        fn next(&mut self) -> usize {
            let next_next_id = self.next_id + 1;
            core::mem::replace(&mut self.next_id, next_next_id)
        }
    }

    impl<I: Eq> SocketMapAddrStateSpec for Multiple<I> {
        type Id = I;
        type SharingState = char;
        type Inserter<'a> = &'a mut Vec<I> where I: 'a;

        fn new(new_sharing_state: &char, id: I) -> Self {
            Self(*new_sharing_state, vec![id])
        }

        fn contains_id(&self, id: &Self::Id) -> bool {
            self.1.contains(id)
        }

        fn try_get_inserter<'a, 'b>(
            &'b mut self,
            new_state: &'a char,
        ) -> Result<Self::Inserter<'b>, IncompatibleError> {
            let Self(c, v) = self;
            (new_state == c).then_some(v).ok_or(IncompatibleError)
        }

        fn could_insert(
            &self,
            new_sharing_state: &Self::SharingState,
        ) -> Result<(), IncompatibleError> {
            let Self(c, _) = self;
            (new_sharing_state == c).then_some(()).ok_or(IncompatibleError)
        }

        fn remove_by_id(&mut self, id: I) -> RemoveResult {
            let Self(_, v) = self;
            let index = v.iter().position(|i| i == &id).expect("did not find id");
            let _: I = v.swap_remove(index);
            if v.is_empty() {
                RemoveResult::IsLast
            } else {
                RemoveResult::Success
            }
        }
    }

    impl<A: Into<AddrVec<Ipv4, FakeWeakDeviceId<FakeDeviceId>, FakeAddrSpec>> + Clone>
        SocketMapConflictPolicy<A, char, Ipv4, FakeWeakDeviceId<FakeDeviceId>, FakeAddrSpec>
        for FakeSpec
    {
        fn check_insert_conflicts(
            new_state: &char,
            addr: &A,
            socketmap: &SocketMap<
                AddrVec<Ipv4, FakeWeakDeviceId<FakeDeviceId>, FakeAddrSpec>,
                Bound<FakeSpec>,
            >,
        ) -> Result<(), InsertError> {
            let dest = addr.clone().into();
            if dest.iter_shadows().any(|a| socketmap.get(&a).is_some()) {
                return Err(InsertError::ShadowAddrExists);
            }
            match socketmap.get(&dest) {
                Some(Bound::Listen(Multiple(c, _))) | Some(Bound::Conn(Multiple(c, _))) => {
                    // Require that all sockets inserted in a `Multiple` entry
                    // have the same sharing state.
                    if c != new_state {
                        return Err(InsertError::Exists);
                    }
                }
                None => (),
            }
            if socketmap.descendant_counts(&dest).len() != 0 {
                Err(InsertError::ShadowerExists)
            } else {
                Ok(())
            }
        }
    }

    impl<I: Eq> SocketMapAddrStateUpdateSharingSpec for Multiple<I> {
        fn try_update_sharing(
            &mut self,
            id: Self::Id,
            new_sharing_state: &Self::SharingState,
        ) -> Result<(), IncompatibleError> {
            let Self(sharing, v) = self;
            if new_sharing_state == sharing {
                return Ok(());
            }

            // Preserve the invariant that all sockets inserted in a `Multiple`
            // entry have the same sharing state. That means we can't change
            // the sharing state of all the sockets at the address unless there
            // is exactly one!
            if v.len() != 1 {
                return Err(IncompatibleError);
            }
            assert!(v.contains(&id));
            *sharing = *new_sharing_state;
            Ok(())
        }
    }

    impl<A: Into<AddrVec<Ipv4, FakeWeakDeviceId<FakeDeviceId>, FakeAddrSpec>> + Clone>
        SocketMapUpdateSharingPolicy<A, char, Ipv4, FakeWeakDeviceId<FakeDeviceId>, FakeAddrSpec>
        for FakeSpec
    {
        fn allows_sharing_update(
            _socketmap: &SocketMap<
                AddrVec<Ipv4, FakeWeakDeviceId<FakeDeviceId>, FakeAddrSpec>,
                Bound<Self>,
            >,
            _addr: &A,
            _old_sharing: &char,
            _new_sharing_state: &char,
        ) -> Result<(), UpdateSharingError> {
            Ok(())
        }
    }

    const LISTENER_ADDR: ListenerAddr<Ipv4Addr, FakeWeakDeviceId<FakeDeviceId>, u16> =
        ListenerAddr {
            ip: ListenerIpAddr {
                addr: Some(unsafe { SpecifiedAddr::new_unchecked(net_ip_v4!("1.2.3.4")) }),
                identifier: 0,
            },
            device: None,
        };

    const CONN_ADDR: ConnAddr<Ipv4Addr, FakeWeakDeviceId<FakeDeviceId>, u16, ()> = ConnAddr {
        ip: unsafe {
            ConnIpAddr {
                local: (SpecifiedAddr::new_unchecked(net_ip_v4!("5.6.7.8")), 0),
                remote: (SpecifiedAddr::new_unchecked(net_ip_v4!("8.7.6.5")), ()),
            }
        },
        device: None,
    };

    #[test]
    fn bound_insert_get_remove_listener() {
        set_logger_for_test();
        let mut bound = FakeBoundSocketMap::default();
        let mut fake_id_gen = FakeSocketIdGen::default();

        let addr = LISTENER_ADDR;

        let id = {
            let entry =
                bound.listeners_mut().try_insert(addr, 'v', Listener(fake_id_gen.next())).unwrap();
            assert_eq!(entry.get_addr(), &addr);
            entry.id()
        };

        assert_eq!(bound.listeners().get_by_addr(&addr), Some(&Multiple('v', vec![id])));

        assert_eq!(bound.listeners_mut().remove(&id, &addr), Ok(()));
        assert_eq!(bound.listeners().get_by_addr(&addr), None);
    }

    #[test]
    fn bound_insert_get_remove_conn() {
        set_logger_for_test();
        let mut bound = FakeBoundSocketMap::default();
        let mut fake_id_gen = FakeSocketIdGen::default();

        let addr = CONN_ADDR;

        let id = {
            let entry = bound.conns_mut().try_insert(addr, 'v', Conn(fake_id_gen.next())).unwrap();
            assert_eq!(entry.get_addr(), &addr);
            entry.id()
        };

        assert_eq!(bound.conns().get_by_addr(&addr), Some(&Multiple('v', vec![id])));

        assert_eq!(bound.conns_mut().remove(&id, &addr), Ok(()));
        assert_eq!(bound.conns().get_by_addr(&addr), None);
    }

    #[test]
    fn bound_iter_addrs() {
        set_logger_for_test();
        let mut bound = FakeBoundSocketMap::default();
        let mut fake_id_gen = FakeSocketIdGen::default();

        let listener_addrs = [
            (Some(net_ip_v4!("1.1.1.1")), 1),
            (Some(net_ip_v4!("2.2.2.2")), 2),
            (Some(net_ip_v4!("1.1.1.1")), 3),
            (None, 4),
        ]
        .map(|(ip, identifier)| ListenerAddr {
            device: None,
            ip: ListenerIpAddr { addr: ip.map(|x| SpecifiedAddr::new(x).unwrap()), identifier },
        });
        let conn_addrs = [
            (net_ip_v4!("3.3.3.3"), 3, net_ip_v4!("4.4.4.4")),
            (net_ip_v4!("4.4.4.4"), 3, net_ip_v4!("3.3.3.3")),
        ]
        .map(|(local_ip, local_identifier, remote_ip)| ConnAddr {
            ip: ConnIpAddr {
                local: (SpecifiedAddr::new(local_ip).unwrap(), local_identifier),
                remote: (SpecifiedAddr::new(remote_ip).unwrap(), ()),
            },
            device: None,
        });

        for addr in listener_addrs.iter().cloned() {
            let _entry =
                bound.listeners_mut().try_insert(addr, 'a', Listener(fake_id_gen.next())).unwrap();
        }
        for addr in conn_addrs.iter().cloned() {
            let _entry = bound.conns_mut().try_insert(addr, 'a', Conn(fake_id_gen.next())).unwrap();
        }
        let expected_addrs = listener_addrs
            .into_iter()
            .map(Into::into)
            .chain(conn_addrs.into_iter().map(Into::into))
            .collect::<HashSet<_>>();

        assert_eq!(expected_addrs, bound.iter_addrs().cloned().collect());
    }

    #[test]
    fn try_insert_with_callback_not_called_on_error() {
        // TODO(https://fxbug.dev/126141): remove this test along with
        // try_insert_with.
        set_logger_for_test();
        let mut bound = FakeBoundSocketMap::default();
        let addr = LISTENER_ADDR;

        // Insert a listener so that future calls can conflict.
        let _: Listener = bound.listeners_mut().try_insert(addr, 'a', Listener(0)).unwrap().id();

        // All of the below try_insert_with calls should fail, but more
        // importantly, they should not call the `make_id` callback (because it
        // is only called once success is certain).
        fn is_never_called<A, B, T>(_: A, _: B) -> T {
            panic!("should never be called");
        }

        assert_matches!(
            bound.listeners_mut().try_insert_with(addr, 'b', is_never_called),
            Err((InsertError::Exists, _))
        );
        assert_matches!(
            bound.listeners_mut().try_insert_with(
                ListenerAddr { device: Some(FakeWeakDeviceId(FakeDeviceId)), ..addr },
                'b',
                is_never_called
            ),
            Err((InsertError::ShadowAddrExists, _))
        );
        assert_matches!(
            bound.conns_mut().try_insert_with(
                ConnAddr {
                    device: None,
                    ip: ConnIpAddr {
                        local: (addr.ip.addr.unwrap(), addr.ip.identifier),
                        remote: (SpecifiedAddr::new(net_ip_v4!("1.1.1.1")).unwrap(), ()),
                    },
                },
                'b',
                is_never_called,
            ),
            Err((InsertError::ShadowAddrExists, _))
        );
    }

    #[test]
    fn insert_listener_conflict_with_listener() {
        set_logger_for_test();
        let mut bound = FakeBoundSocketMap::default();
        let mut fake_id_gen = FakeSocketIdGen::default();
        let addr = LISTENER_ADDR;

        let _: Listener =
            bound.listeners_mut().try_insert(addr, 'a', Listener(fake_id_gen.next())).unwrap().id();
        assert_matches!(
            bound.listeners_mut().try_insert(addr, 'b', Listener(fake_id_gen.next())),
            Err((InsertError::Exists, 'b'))
        );
    }

    #[test]
    fn insert_listener_conflict_with_shadower() {
        set_logger_for_test();
        let mut bound = FakeBoundSocketMap::default();
        let mut fake_id_gen = FakeSocketIdGen::default();
        let addr = LISTENER_ADDR;
        let shadows_addr = {
            assert_eq!(addr.device, None);
            ListenerAddr { device: Some(FakeWeakDeviceId(FakeDeviceId)), ..addr }
        };

        let _: Listener =
            bound.listeners_mut().try_insert(addr, 'a', Listener(fake_id_gen.next())).unwrap().id();
        assert_matches!(
            bound.listeners_mut().try_insert(shadows_addr, 'b', Listener(fake_id_gen.next())),
            Err((InsertError::ShadowAddrExists, 'b'))
        );
    }

    #[test]
    fn insert_conn_conflict_with_listener() {
        set_logger_for_test();
        let mut bound = FakeBoundSocketMap::default();
        let mut fake_id_gen = FakeSocketIdGen::default();
        let addr = LISTENER_ADDR;
        let shadows_addr = ConnAddr {
            device: None,
            ip: ConnIpAddr {
                local: (addr.ip.addr.unwrap(), addr.ip.identifier),
                remote: (SpecifiedAddr::new(net_ip_v4!("1.1.1.1")).unwrap(), ()),
            },
        };

        let _: Listener =
            bound.listeners_mut().try_insert(addr, 'a', Listener(fake_id_gen.next())).unwrap().id();
        assert_matches!(
            bound.conns_mut().try_insert(shadows_addr, 'b', Conn(fake_id_gen.next())),
            Err((InsertError::ShadowAddrExists, 'b'))
        );
    }

    #[test]
    fn insert_and_remove_listener() {
        set_logger_for_test();
        let mut bound = FakeBoundSocketMap::default();
        let mut fake_id_gen = FakeSocketIdGen::default();
        let addr = LISTENER_ADDR;

        let a =
            bound.listeners_mut().try_insert(addr, 'x', Listener(fake_id_gen.next())).unwrap().id();
        let b =
            bound.listeners_mut().try_insert(addr, 'x', Listener(fake_id_gen.next())).unwrap().id();
        assert_ne!(a, b);

        assert_eq!(bound.listeners_mut().remove(&a, &addr), Ok(()));
        assert_eq!(bound.listeners().get_by_addr(&addr), Some(&Multiple('x', vec![b])));
    }

    #[test]
    fn insert_and_remove_conn() {
        set_logger_for_test();
        let mut bound = FakeBoundSocketMap::default();
        let mut fake_id_gen = FakeSocketIdGen::default();
        let addr = CONN_ADDR;

        let a = bound.conns_mut().try_insert(addr, 'x', Conn(fake_id_gen.next())).unwrap().id();
        let b = bound.conns_mut().try_insert(addr, 'x', Conn(fake_id_gen.next())).unwrap().id();
        assert_ne!(a, b);

        assert_eq!(bound.conns_mut().remove(&a, &addr), Ok(()));
        assert_eq!(bound.conns().get_by_addr(&addr), Some(&Multiple('x', vec![b])));
    }

    #[test]
    fn update_listener_to_shadowed_addr_fails() {
        let mut bound = FakeBoundSocketMap::default();
        let mut fake_id_gen = FakeSocketIdGen::default();

        let first_addr = LISTENER_ADDR;
        let second_addr = ListenerAddr {
            ip: ListenerIpAddr {
                addr: Some(SpecifiedAddr::new(net_ip_v4!("1.1.1.1")).unwrap()),
                ..LISTENER_ADDR.ip
            },
            ..LISTENER_ADDR
        };
        let both_shadow = ListenerAddr {
            ip: ListenerIpAddr { addr: None, identifier: first_addr.ip.identifier },
            device: None,
        };

        let first = bound
            .listeners_mut()
            .try_insert(first_addr, 'a', Listener(fake_id_gen.next()))
            .unwrap()
            .id();
        let second = bound
            .listeners_mut()
            .try_insert(second_addr, 'b', Listener(fake_id_gen.next()))
            .unwrap()
            .id();

        // Moving from (1, "aaa") to (1, None) should fail since it is shadowed
        // by (1, "yyy"), and vise versa.
        let (ExistsError, entry) = bound
            .listeners_mut()
            .entry(&second, &second_addr)
            .unwrap()
            .try_update_addr(both_shadow)
            .expect_err("update should fail");

        // The entry should correspond to `second`.
        assert_eq!(entry.id(), second);
        drop(entry);

        let (ExistsError, entry) = bound
            .listeners_mut()
            .entry(&first, &first_addr)
            .unwrap()
            .try_update_addr(both_shadow)
            .expect_err("update should fail");
        assert_eq!(entry.get_addr(), &first_addr);
    }

    #[test]
    fn nonexistent_conn_entry() {
        let mut map = FakeBoundSocketMap::default();
        let mut fake_id_gen = FakeSocketIdGen::default();
        let addr = CONN_ADDR;
        let conn_id = map
            .conns_mut()
            .try_insert(addr.clone(), 'a', Conn(fake_id_gen.next()))
            .expect("failed to insert")
            .id();
        assert_matches!(map.conns_mut().remove(&conn_id, &addr), Ok(()));

        assert!(map.conns_mut().entry(&conn_id, &addr).is_none());
    }

    #[test]
    fn update_conn_sharing() {
        let mut map = FakeBoundSocketMap::default();
        let mut fake_id_gen = FakeSocketIdGen::default();
        let addr = CONN_ADDR;
        let entry = map
            .conns_mut()
            .try_insert(addr.clone(), 'a', Conn(fake_id_gen.next()))
            .expect("failed to insert");

        entry.try_update_sharing(&'a', 'd').expect("worked");
        // Updating sharing is only allowed if there are no other occupants at
        // the address.
        let second_conn = map
            .conns_mut()
            .try_insert(addr.clone(), 'd', Conn(fake_id_gen.next()))
            .expect("can insert");
        assert_matches!(second_conn.try_update_sharing(&'d', 'e'), Err(UpdateSharingError));
    }
}
