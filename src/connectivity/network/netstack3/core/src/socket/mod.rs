// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! General-purpose socket utilities common to device layer and IP layer
//! sockets.

pub(crate) mod address;
pub mod datagram;
pub(crate) mod posix;

use alloc::collections::HashMap;
use core::{convert::Infallible as Never, fmt::Debug, hash::Hash, marker::PhantomData};
use net_types::{
    ip::{Ip, IpAddress},
    AddrAndZone, SpecifiedAddr, Witness as _,
};

use derivative::Derivative;

use crate::{
    data_structures::{
        id_map::{EntryKey, IdMap},
        id_map_collection::IdMapCollectionKey,
        socketmap::{
            Entry, IterShadows, OccupiedEntry as SocketMapOccupiedEntry, SocketMap, Tagged,
        },
    },
    device::{StrongId, WeakId},
    error::{ExistsError, NotFoundError},
    ip::IpExt,
    socket::address::{ConnAddr, ListenerAddr, ListenerIpAddr},
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

pub(crate) trait SocketMapAddrSpec {
    /// The version of IP addresses in socket addresses.
    type IpVersion: Ip<Addr = Self::IpAddr> + IpExt;
    /// The type of IP addresses in the socket address.
    type IpAddr: IpAddress<Version = Self::IpVersion>;
    /// The type of the device component of a socket address.
    type WeakDeviceId: WeakId;
    /// The local identifier portion of a socket address.
    type LocalIdentifier: Clone + Debug + Hash + Eq;
    /// The remote identifier portion of a socket address.
    type RemoteIdentifier: Clone + Debug + Hash + Eq;
}

/// Specifies the types parameters for [`BoundSocketMap`] state as a single bundle.
pub(crate) trait SocketMapStateSpec {
    /// The tag value of a socket address vector entry.
    ///
    /// These values are derived from [`Self::ListenerAddrState`] and
    /// [`Self::ConnAddrState`].
    type AddrVecTag: Eq + Copy + Debug + 'static;

    /// An identifier for a listening socket.
    type ListenerId: Clone + EntryKey + From<usize> + Debug;
    /// An identifier for a connected socket.
    type ConnId: Clone + EntryKey + From<usize> + Debug;

    /// The state stored for a listening socket.
    type ListenerState: Debug;
    /// The state stored for a listening socket that is used to determine
    /// whether sockets can share an address.
    type ListenerSharingState: Clone + Debug;

    /// The state stored for a connected socket.
    type ConnState: Debug;
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

pub(crate) trait SocketMapAddrStateSpec {
    type Id;
    type SharingState;
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

pub(crate) trait SocketMapConflictPolicy<Addr, SharingState, A: SocketMapAddrSpec>:
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
        socketmap: &SocketMap<AddrVec<A>, Bound<Self>>,
    ) -> Result<(), InsertError>
    where
        Bound<Self>: Tagged<AddrVec<A>>;
}

pub(crate) trait SocketMapUpdateSharingPolicy<Addr, SharingState, A>:
    SocketMapConflictPolicy<Addr, SharingState, A>
where
    A: SocketMapAddrSpec,
{
    fn allows_sharing_update(
        socketmap: &SocketMap<AddrVec<A>, Bound<Self>>,
        addr: &Addr,
        old_sharing: &SharingState,
        new_sharing_state: &SharingState,
    ) -> Result<(), UpdateSharingError>
    where
        Bound<Self>: Tagged<AddrVec<A>>;
}

#[derive(Derivative)]
#[derivative(Debug(bound = "S::ListenerAddrState: Debug, S::ConnAddrState: Debug"))]
pub(crate) enum Bound<S: SocketMapStateSpec + ?Sized> {
    Listen(S::ListenerAddrState),
    Conn(S::ConnAddrState),
}

#[derive(Derivative)]
#[derivative(Debug(bound = ""))]
pub(crate) enum SocketState<A: SocketMapAddrSpec, S: SocketMapStateSpec> {
    Listener(
        (
            S::ListenerState,
            S::ListenerSharingState,
            ListenerAddr<A::IpAddr, A::WeakDeviceId, A::LocalIdentifier>,
        ),
    ),
    Connected(
        (
            S::ConnState,
            S::ConnSharingState,
            ConnAddr<A::IpAddr, A::WeakDeviceId, A::LocalIdentifier, A::RemoteIdentifier>,
        ),
    ),
}

#[derive(Derivative)]
#[derivative(
    Debug(bound = ""),
    Clone(bound = ""),
    Eq(bound = ""),
    PartialEq(bound = ""),
    Hash(bound = "")
)]
pub(crate) enum AddrVec<A: SocketMapAddrSpec + ?Sized> {
    Listen(ListenerAddr<A::IpAddr, A::WeakDeviceId, A::LocalIdentifier>),
    Conn(ConnAddr<A::IpAddr, A::WeakDeviceId, A::LocalIdentifier, A::RemoteIdentifier>),
}

impl<A: SocketMapAddrSpec, S: SocketMapStateSpec> Tagged<AddrVec<A>> for Bound<S>
where
    S::ListenerAddrState:
        Tagged<ListenerAddr<A::IpAddr, A::WeakDeviceId, A::LocalIdentifier>, Tag = S::AddrVecTag>,
    S::ConnAddrState: Tagged<
        ConnAddr<A::IpAddr, A::WeakDeviceId, A::LocalIdentifier, A::RemoteIdentifier>,
        Tag = S::AddrVecTag,
    >,
{
    type Tag = S::AddrVecTag;

    fn tag(&self, address: &AddrVec<A>) -> Self::Tag {
        match (self, address) {
            (Bound::Listen(l), AddrVec::Listen(addr)) => l.tag(addr),
            (Bound::Conn(c), AddrVec::Conn(addr)) => c.tag(addr),
            (Bound::Listen(_), AddrVec::Conn(_)) => {
                unreachable!("found listen state for conn addr")
            }
            (Bound::Conn(_), AddrVec::Listen(_)) => {
                unreachable!("found conn state for listen addr")
            }
        }
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

impl<S: SocketMapStateSpec> IdMapCollectionKey for SocketId<S> {
    const VARIANT_COUNT: usize = 2;
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

/// A bidirectional map between sockets and their state, keyed in one direction
/// by socket IDs, and in the other by socket addresses.
///
/// The types of keys and IDs is determined by the [`SocketMapStateSpec`]
/// parameter. Each listener and connected socket stores additional state.
/// Listener and connected sockets are keyed independently, but share the same
/// address vector space. Conflicts are detected on attempted insertion of new
/// sockets.
#[derive(Derivative)]
#[derivative(Default(bound = ""))]
pub(crate) struct BoundSocketMap<A: SocketMapAddrSpec, S: SocketMapStateSpec>
where
    Bound<S>: Tagged<AddrVec<A>>,
{
    addr_to_state: SocketMap<AddrVec<A>, Bound<S>>,
}

/// Uninstantiable tag type for denoting listening sockets.
pub(crate) enum Listener {}
/// Uninstantiable tag type for denoting connected sockets.
pub(crate) enum Connection {}

/// View struct over one type of sockets in a [`BoundSocketMap`].
pub(crate) struct Sockets<AddrToStateMap, SocketType>(AddrToStateMap, PhantomData<SocketType>);

/// Provides associated types for a specific kind of socket.
pub(crate) trait BoundSocketType<A, S> {
    type Id: From<usize>;
    type State;
    type SharingState;
    type Addr: Debug;
    type AddrState: SocketMapAddrStateSpec<Id = Self::Id, SharingState = Self::SharingState>;
}

impl<A: SocketMapAddrSpec, S: SocketMapStateSpec> BoundSocketType<A, S> for Listener {
    type Id = S::ListenerId;
    type State = S::ListenerState;
    type SharingState = S::ListenerSharingState;
    type Addr = ListenerAddr<A::IpAddr, A::WeakDeviceId, A::LocalIdentifier>;
    type AddrState = S::ListenerAddrState;
}

impl<A: SocketMapAddrSpec, S: SocketMapStateSpec> BoundSocketType<A, S> for Connection {
    type Id = S::ConnId;
    type State = S::ConnState;
    type SharingState = S::ConnSharingState;
    type Addr = ConnAddr<A::IpAddr, A::WeakDeviceId, A::LocalIdentifier, A::RemoteIdentifier>;
    type AddrState = S::ConnAddrState;
}

impl<
        'a,
        SocketType: BoundSocketType<A, S> + ConvertSocketTypeState<A, S>,
        A: SocketMapAddrSpec,
        S: SocketMapStateSpec,
    > Sockets<&'a SocketMap<AddrVec<A>, Bound<S>>, SocketType>
where
    SocketType::Id: Clone,
    Bound<S>: Tagged<AddrVec<A>>,
    S: SocketMapConflictPolicy<SocketType::Addr, SocketType::SharingState, A>,
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
pub(crate) struct SocketStateEntry<'a, A: SocketMapAddrSpec, S: SocketMapStateSpec, SocketType>
where
    Bound<S>: Tagged<AddrVec<A>>,
{
    id: SocketId<S>,
    addr_entry: SocketMapOccupiedEntry<'a, AddrVec<A>, Bound<S>>,
    _marker: PhantomData<SocketType>,
}

impl<
        'a,
        SocketType: BoundSocketType<A, S> + ConvertSocketTypeState<A, S>,
        A: SocketMapAddrSpec,
        S: SocketMapStateSpec + SocketMapConflictPolicy<SocketType::Addr, SocketType::SharingState, A>,
    > Sockets<&'a mut SocketMap<AddrVec<A>, Bound<S>>, SocketType>
where
    Bound<S>: Tagged<AddrVec<A>>,
    SocketType::SharingState: Clone,
    SocketType::Id: Clone + EntryKey,
{
    pub(crate) fn try_insert(
        self,
        socket_addr: SocketType::Addr,
        tag_state: SocketType::SharingState,
        make_id: impl FnOnce(SocketType::Addr, SocketType::SharingState) -> SocketType::Id,
    ) -> Result<SocketStateEntry<'a, A, S, SocketType>, (InsertError, SocketType::SharingState)>
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
    ) -> Option<SocketStateEntry<'a, A, S, SocketType>> {
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
        SocketType: BoundSocketType<A, S> + ConvertSocketTypeState<A, S>,
        A: SocketMapAddrSpec,
        S: SocketMapStateSpec,
    > SocketStateEntry<'a, A, S, SocketType>
where
    Bound<S>: Tagged<AddrVec<A>>,
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
        S: SocketMapUpdateSharingPolicy<SocketType::Addr, SocketType::SharingState, A>,
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

impl<A: SocketMapAddrSpec, S> BoundSocketMap<A, S>
where
    Bound<S>: Tagged<AddrVec<A>>,
    AddrVec<A>: IterShadows,
    S: SocketMapStateSpec,
{
    pub(crate) fn listeners(&self) -> Sockets<&SocketMap<AddrVec<A>, Bound<S>>, Listener>
    where
        S: SocketMapConflictPolicy<
            ListenerAddr<A::IpAddr, A::WeakDeviceId, A::LocalIdentifier>,
            <S as SocketMapStateSpec>::ListenerSharingState,
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
    ) -> Sockets<&mut SocketMap<AddrVec<A>, Bound<S>>, Listener>
    where
        S: SocketMapConflictPolicy<
            ListenerAddr<A::IpAddr, A::WeakDeviceId, A::LocalIdentifier>,
            <S as SocketMapStateSpec>::ListenerSharingState,
            A,
        >,
        S::ListenerAddrState:
            SocketMapAddrStateSpec<Id = S::ListenerId, SharingState = S::ListenerSharingState>,
    {
        let Self { addr_to_state } = self;
        Sockets(addr_to_state, Default::default())
    }

    pub(crate) fn conns(&self) -> Sockets<&SocketMap<AddrVec<A>, Bound<S>>, Connection>
    where
        S: SocketMapConflictPolicy<
            ConnAddr<A::IpAddr, A::WeakDeviceId, A::LocalIdentifier, A::RemoteIdentifier>,
            <S as SocketMapStateSpec>::ConnSharingState,
            A,
        >,
        S::ConnAddrState:
            SocketMapAddrStateSpec<Id = S::ConnId, SharingState = S::ConnSharingState>,
    {
        let Self { addr_to_state } = self;
        Sockets(addr_to_state, Default::default())
    }

    pub(crate) fn conns_mut(&mut self) -> Sockets<&mut SocketMap<AddrVec<A>, Bound<S>>, Connection>
    where
        S: SocketMapConflictPolicy<
            ConnAddr<A::IpAddr, A::WeakDeviceId, A::LocalIdentifier, A::RemoteIdentifier>,
            <S as SocketMapStateSpec>::ConnSharingState,
            A,
        >,
        S::ConnAddrState:
            SocketMapAddrStateSpec<Id = S::ConnId, SharingState = S::ConnSharingState>,
    {
        let Self { addr_to_state } = self;
        Sockets(addr_to_state, Default::default())
    }

    #[cfg(test)]
    pub(crate) fn iter_addrs(&self) -> impl Iterator<Item = &AddrVec<A>> {
        let Self { addr_to_state } = self;
        addr_to_state.iter().map(|(a, _v): (_, &Bound<S>)| a)
    }

    pub(crate) fn get_shadower_counts(&self, addr: &AddrVec<A>) -> usize {
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
pub(crate) trait ConvertSocketTypeState<A: SocketMapAddrSpec, S: SocketMapStateSpec>:
    BoundSocketType<A, S>
{
    fn to_addr_vec(addr: &Self::Addr) -> AddrVec<A>;
    fn from_addr_vec_ref(addr: &AddrVec<A>) -> &Self::Addr;
    fn from_bound_ref(bound: &Bound<S>) -> Option<&Self::AddrState>;
    fn from_bound_mut(bound: &mut Bound<S>) -> Option<&mut Self::AddrState>;
    fn to_bound(state: Self::AddrState) -> Bound<S>;
    fn from_socket_state_ref(
        socket_state: &SocketState<A, S>,
    ) -> &(Self::State, Self::SharingState, Self::Addr);
    fn from_socket_state_mut(
        socket_state: &mut SocketState<A, S>,
    ) -> &mut (Self::State, Self::SharingState, Self::Addr);
    fn to_socket_state(state: (Self::State, Self::SharingState, Self::Addr)) -> SocketState<A, S>;
    fn from_socket_state(
        socket_state: SocketState<A, S>,
    ) -> (Self::State, Self::SharingState, Self::Addr);
    fn to_socket_id(id: Self::Id) -> SocketId<S>;
    fn from_socket_id_ref(id: &SocketId<S>) -> &Self::Id;
}

impl<A: SocketMapAddrSpec, S: SocketMapStateSpec> ConvertSocketTypeState<A, S> for Listener
where
    Bound<S>: Tagged<AddrVec<A>>,
{
    fn to_addr_vec(addr: &Self::Addr) -> AddrVec<A> {
        AddrVec::Listen(addr.clone())
    }

    fn from_addr_vec_ref(addr: &AddrVec<A>) -> &Self::Addr {
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

    fn from_socket_state(
        socket_state: SocketState<A, S>,
    ) -> (Self::State, Self::SharingState, Self::Addr) {
        match socket_state {
            SocketState::Listener(state) => state,
            SocketState::Connected(state) => {
                unreachable!("connection state for listener: {state:?}")
            }
        }
    }

    fn from_socket_state_mut(
        socket_state: &mut SocketState<A, S>,
    ) -> &mut (Self::State, Self::SharingState, Self::Addr) {
        match socket_state {
            SocketState::Listener(state) => state,
            SocketState::Connected(state) => {
                unreachable!("connection state for listener: {state:?}")
            }
        }
    }

    fn from_socket_state_ref(
        socket_state: &SocketState<A, S>,
    ) -> &(Self::State, Self::SharingState, Self::Addr) {
        match socket_state {
            SocketState::Listener(state) => state,
            SocketState::Connected(state) => {
                unreachable!("connection state for listener: {state:?}")
            }
        }
    }

    fn to_socket_state(state: (Self::State, Self::SharingState, Self::Addr)) -> SocketState<A, S> {
        SocketState::Listener(state)
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

impl<A: SocketMapAddrSpec, S: SocketMapStateSpec> ConvertSocketTypeState<A, S> for Connection
where
    Bound<S>: Tagged<AddrVec<A>>,
{
    fn to_addr_vec(addr: &Self::Addr) -> AddrVec<A> {
        AddrVec::Conn(addr.clone())
    }

    fn from_addr_vec_ref(addr: &AddrVec<A>) -> &Self::Addr {
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

    fn from_socket_state(
        socket_state: SocketState<A, S>,
    ) -> (Self::State, Self::SharingState, Self::Addr) {
        match socket_state {
            SocketState::Connected(state) => state,
            SocketState::Listener(state) => {
                unreachable!("listener state for connection: {state:?}")
            }
        }
    }

    fn from_socket_state_mut(
        socket_state: &mut SocketState<A, S>,
    ) -> &mut (Self::State, Self::SharingState, Self::Addr) {
        match socket_state {
            SocketState::Connected(state) => state,
            SocketState::Listener(state) => {
                unreachable!("listener state for connection: {state:?}")
            }
        }
    }

    fn from_socket_state_ref(
        socket_state: &SocketState<A, S>,
    ) -> &(Self::State, Self::SharingState, Self::Addr) {
        match socket_state {
            SocketState::Connected(state) => state,
            SocketState::Listener(state) => {
                unreachable!("listener state for connection: {state:?}")
            }
        }
    }

    fn to_socket_state(state: (Self::State, Self::SharingState, Self::Addr)) -> SocketState<A, S> {
        SocketState::Connected(state)
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

    impl EntryKey for Listener {
        fn get_key_index(&self) -> usize {
            let Listener(index) = self;
            *index
        }
    }

    impl From<usize> for Listener {
        fn from(index: usize) -> Listener {
            Listener(index)
        }
    }

    impl EntryKey for Conn {
        fn get_key_index(&self) -> usize {
            let Conn(index) = self;
            *index
        }
    }

    impl From<usize> for Conn {
        fn from(index: usize) -> Conn {
            Conn(index)
        }
    }

    #[derive(PartialEq, Eq, Debug)]
    struct Multiple<T>(char, Vec<T>);

    impl<T, A> Tagged<A> for Multiple<T> {
        type Tag = char;
        fn tag(&self, _: &A) -> Self::Tag {
            let Multiple(c, _) = self;
            *c
        }
    }

    #[derive(Copy, Clone, Eq, PartialEq, Debug, Hash)]
    struct Conn(usize);

    enum FakeAddrSpec {}

    impl SocketMapAddrSpec for FakeAddrSpec {
        type IpVersion = Ipv4;
        type IpAddr = Ipv4Addr;
        type WeakDeviceId = FakeWeakDeviceId<FakeDeviceId>;
        type LocalIdentifier = u16;
        type RemoteIdentifier = ();
    }

    impl SocketMapStateSpec for FakeSpec {
        type AddrVecTag = char;

        type ListenerId = Listener;
        type ConnId = Conn;

        type ListenerState = u8;
        type ListenerSharingState = char;
        type ConnState = u16;
        type ConnSharingState = char;

        type ListenerAddrState = Multiple<Listener>;
        type ConnAddrState = Multiple<Conn>;
    }

    type FakeBoundSocketMap = BoundSocketMap<FakeAddrSpec, FakeSpec>;

    /// Generator for unique socket IDs that don't have any state.
    ///
    /// Calling [`fakeSocketIdGen::id_maker`] returns a callable object that
    /// creates new socket IDs of the requested type that are all unique. The
    /// IDs can be stored in a [`BoundSocketMap`] but don't have any additional
    /// state associated with them.
    #[derive(Default)]
    struct FakeSocketIdGen {
        next_id: usize,
    }

    impl FakeSocketIdGen {
        fn id_maker<T: From<usize>, A, B>(&mut self) -> impl FnOnce(A, B) -> T + '_ {
            |_: A, _: B| {
                let id = self.next_id;
                self.next_id += 1;
                id.into()
            }
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

    impl<A: Into<AddrVec<FakeAddrSpec>> + Clone> SocketMapConflictPolicy<A, char, FakeAddrSpec>
        for FakeSpec
    {
        fn check_insert_conflicts(
            new_state: &char,
            addr: &A,
            socketmap: &SocketMap<AddrVec<FakeAddrSpec>, Bound<FakeSpec>>,
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

    impl<A: Into<AddrVec<FakeAddrSpec>> + Clone> SocketMapUpdateSharingPolicy<A, char, FakeAddrSpec>
        for FakeSpec
    {
        fn allows_sharing_update(
            _socketmap: &SocketMap<AddrVec<FakeAddrSpec>, Bound<Self>>,
            _addr: &A,
            _old_sharing: &char,
            _new_sharing_state: &char,
        ) -> Result<(), UpdateSharingError>
        where
            Bound<Self>: Tagged<AddrVec<FakeAddrSpec>>,
        {
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
                bound.listeners_mut().try_insert(addr, 'v', fake_id_gen.id_maker()).unwrap();
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
            let entry = bound.conns_mut().try_insert(addr, 'v', fake_id_gen.id_maker()).unwrap();
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
                bound.listeners_mut().try_insert(addr, 'a', fake_id_gen.id_maker()).unwrap();
        }
        for addr in conn_addrs.iter().cloned() {
            let _entry = bound.conns_mut().try_insert(addr, 'a', fake_id_gen.id_maker()).unwrap();
        }
        let expected_addrs = listener_addrs
            .into_iter()
            .map(Into::into)
            .chain(conn_addrs.into_iter().map(Into::into))
            .collect::<HashSet<_>>();

        assert_eq!(expected_addrs, bound.iter_addrs().cloned().collect());
    }

    #[test]
    fn insert_listener_conflict_with_listener() {
        set_logger_for_test();
        let mut bound = FakeBoundSocketMap::default();
        let mut fake_id_gen = FakeSocketIdGen::default();
        let addr = LISTENER_ADDR;

        let _: Listener =
            bound.listeners_mut().try_insert(addr, 'a', fake_id_gen.id_maker()).unwrap().id();
        assert_matches!(
            bound.listeners_mut().try_insert(addr, 'b', fake_id_gen.id_maker()),
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
            bound.listeners_mut().try_insert(addr, 'a', fake_id_gen.id_maker()).unwrap().id();
        assert_matches!(
            bound.listeners_mut().try_insert(shadows_addr, 'b', fake_id_gen.id_maker()),
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
            bound.listeners_mut().try_insert(addr, 'a', fake_id_gen.id_maker()).unwrap().id();
        assert_matches!(
            bound.conns_mut().try_insert(shadows_addr, 'b', fake_id_gen.id_maker()),
            Err((InsertError::ShadowAddrExists, 'b'))
        );
    }

    #[test]
    fn insert_and_remove_listener() {
        set_logger_for_test();
        let mut bound = FakeBoundSocketMap::default();
        let mut fake_id_gen = FakeSocketIdGen::default();
        let addr = LISTENER_ADDR;

        let a = bound.listeners_mut().try_insert(addr, 'x', fake_id_gen.id_maker()).unwrap().id();
        let b = bound.listeners_mut().try_insert(addr, 'x', fake_id_gen.id_maker()).unwrap().id();
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

        let a = bound.conns_mut().try_insert(addr, 'x', fake_id_gen.id_maker()).unwrap().id();
        let b = bound.conns_mut().try_insert(addr, 'x', fake_id_gen.id_maker()).unwrap().id();
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

        let first =
            bound.listeners_mut().try_insert(first_addr, 'a', fake_id_gen.id_maker()).unwrap().id();
        let second = bound
            .listeners_mut()
            .try_insert(second_addr, 'b', fake_id_gen.id_maker())
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
            .try_insert(addr.clone(), 'a', fake_id_gen.id_maker())
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
            .try_insert(addr.clone(), 'a', fake_id_gen.id_maker())
            .expect("failed to insert");

        entry.try_update_sharing(&'a', 'd').expect("worked");
        // Updating sharing is only allowed if there are no other occupants at
        // the address.
        let second_conn = map
            .conns_mut()
            .try_insert(addr.clone(), 'd', fake_id_gen.id_maker())
            .expect("can insert");
        assert_matches!(second_conn.try_update_sharing(&'d', 'e'), Err(UpdateSharingError));
    }
}
