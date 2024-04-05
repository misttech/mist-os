// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Neighbor API structs.

use core::{fmt::Display, marker::PhantomData};

use net_types::{
    ip::{Ip, IpAddress, IpVersionMarker, Ipv4, Ipv6},
    SpecifiedAddr, UnicastAddress as _, Witness as _,
};
use thiserror::Error;

use crate::{
    context::{ContextPair, EventContext as _, InstantContext as _},
    device::{link::LinkDevice, DeviceIdContext},
    error::NotFoundError,
    inspect::Inspector,
    ip::device::nud::{
        Delay, DynamicNeighborState, Entry, Event, Incomplete, LinkResolutionContext,
        LinkResolutionNotifier, LinkResolutionResult, NeighborState, NudBindingsContext,
        NudContext, NudHandler, NudState, Probe, Reachable, Stale, Unreachable,
    },
};

/// Error when a static neighbor entry cannot be inserted.
#[derive(Debug, PartialEq, Eq, Error)]
pub enum StaticNeighborInsertionError {
    /// The MAC address used for a static neighbor entry is not unicast.
    #[error("MAC address is not unicast")]
    MacAddressNotUnicast,

    /// The IP address is invalid as the address of a neighbor. A valid address
    /// is:
    /// - specified,
    /// - not multicast,
    /// - not loopback,
    /// - not an IPv4-mapped address, and
    /// - not the limited broadcast address of `255.255.255.255`.
    #[error("IP address is invalid")]
    IpAddressInvalid,
}

/// Error when a neighbor table entry cannot be removed.
#[derive(Debug, PartialEq, Eq, Error)]
pub enum NeighborRemovalError {
    /// The IP address is invalid as the address of a neighbor.
    #[error("IP address is invalid")]
    IpAddressInvalid,

    /// Entry cannot be found.
    #[error("{0}")]
    NotFound(#[from] NotFoundError),
}

// TODO(https://fxbug.dev/42083952): Use NeighborAddr to witness these properties.
fn validate_neighbor_addr<A: IpAddress>(addr: A) -> Option<SpecifiedAddr<A>> {
    let is_valid: bool = A::Version::map_ip(
        addr,
        |v4| {
            !Ipv4::LOOPBACK_SUBNET.contains(&v4)
                && !Ipv4::MULTICAST_SUBNET.contains(&v4)
                && v4 != Ipv4::LIMITED_BROADCAST_ADDRESS.get()
        },
        |v6| v6 != Ipv6::LOOPBACK_ADDRESS.get() && v6.to_ipv4_mapped().is_none() && v6.is_unicast(),
    );
    is_valid.then_some(()).and_then(|()| SpecifiedAddr::new(addr))
}

/// The neighbor API.
pub struct NeighborApi<I: Ip, D, C>(C, IpVersionMarker<I>, PhantomData<D>);

impl<I: Ip, D, C> NeighborApi<I, D, C> {
    pub(crate) fn new(ctx: C) -> Self {
        Self(ctx, IpVersionMarker::new(), PhantomData)
    }
}

impl<I, D, C> NeighborApi<I, D, C>
where
    I: Ip,
    D: LinkDevice,
    C: ContextPair,
    C::CoreContext: NudContext<I, D, C::BindingsContext>,
    C::BindingsContext: NudBindingsContext<I, D, <C::CoreContext as DeviceIdContext<D>>::DeviceId>,
{
    fn core_ctx(&mut self) -> &mut C::CoreContext {
        let Self(pair, IpVersionMarker { .. }, PhantomData) = self;
        pair.core_ctx()
    }

    fn contexts(&mut self) -> (&mut C::CoreContext, &mut C::BindingsContext) {
        let Self(pair, IpVersionMarker { .. }, PhantomData) = self;
        pair.contexts()
    }

    /// Resolve the link-address for a given device's neighbor.
    ///
    /// Lookup the given destination IP address in the neighbor table for given
    /// device, returning either the associated link-address if it is available,
    /// or an observer that can be used to wait for link address resolution to
    /// complete.
    pub fn resolve_link_addr(
        &mut self,
        device_id: &<C::CoreContext as DeviceIdContext<D>>::DeviceId,
    // TODO(https://fxbug.dev/42076887): Use IPv4 subnet information to
    // disallow subnet and subnet broadcast addresses.
    // TODO(https://fxbug.dev/42083952): Use NeighborAddr when available.
        dst: &SpecifiedAddr<I::Addr>,
    ) -> LinkResolutionResult<
        D::Address,
        <<C::BindingsContext as LinkResolutionContext<D>>::Notifier as LinkResolutionNotifier<
            D,
        >>::Observer,
    >{
        let (core_ctx, bindings_ctx) = self.contexts();
        let (result, do_multicast_solicit) = core_ctx.with_nud_state_mut(
            device_id,
            |NudState { neighbors, timer_heap, .. }, core_ctx| {
                match neighbors.entry(*dst) {
                    Entry::Vacant(entry) => {
                        // Initiate link resolution.
                        let (notifier, observer) =
                            <C::BindingsContext as LinkResolutionContext<D>>::Notifier::new();
                        let state = entry.insert(NeighborState::Dynamic(
                            DynamicNeighborState::Incomplete(Incomplete::new_with_notifier(
                                core_ctx,
                                bindings_ctx,
                                timer_heap,
                                *dst,
                                notifier,
                            )),
                        ));
                        bindings_ctx.on_event(Event::added(
                            device_id,
                            state.to_event_state(),
                            *dst,
                            bindings_ctx.now(),
                        ));
                        (LinkResolutionResult::Pending(observer), true)
                    }
                    Entry::Occupied(e) => match e.into_mut() {
                        NeighborState::Static(link_address) => {
                            (LinkResolutionResult::Resolved(*link_address), false)
                        }
                        NeighborState::Dynamic(e) => {
                            e.resolve_link_addr(core_ctx, bindings_ctx, timer_heap, device_id, *dst)
                        }
                    },
                }
            },
        );

        if do_multicast_solicit {
            core_ctx.send_neighbor_solicitation(
                bindings_ctx,
                &device_id,
                *dst,
                /* multicast */ None,
            );
        }

        result
    }

    /// Flush neighbor table entries.
    pub fn flush_table(&mut self, device: &<C::CoreContext as DeviceIdContext<D>>::DeviceId) {
        let (core_ctx, bindings_ctx) = self.contexts();
        NudHandler::<I, D, _>::flush(core_ctx, bindings_ctx, device)
    }

    /// Sets a static neighbor entry for the neighbor.
    ///
    /// If no entry exists, a new one may be created. If an entry already
    /// exists, it will be updated with the provided link address and set to be
    /// a static entry.
    ///
    /// Dynamic updates for the neighbor will be ignored for static entries.
    pub fn insert_static_entry(
        &mut self,
        device_id: &<C::CoreContext as DeviceIdContext<D>>::DeviceId,
        neighbor: I::Addr,
        // TODO(https://fxbug.dev/42076887): Use IPv4 subnet information to
        // disallow the address with all host bits equal to 0, and the
        // subnet broadcast addresses with all host bits equal to 1.
        // TODO(https://fxbug.dev/42083952): Use NeighborAddr when available.
        link_address: D::Address,
    ) -> Result<(), StaticNeighborInsertionError> {
        if !link_address.is_unicast() {
            return Err(StaticNeighborInsertionError::MacAddressNotUnicast);
        }
        let neighbor = validate_neighbor_addr(neighbor)
            .ok_or(StaticNeighborInsertionError::IpAddressInvalid)?;
        let (core_ctx, bindings_ctx) = self.contexts();

        core_ctx.with_nud_state_mut_and_sender_ctx(
            device_id,
            |NudState { neighbors, last_gc: _, timer_heap }, core_ctx| match neighbors
                .entry(neighbor)
            {
                Entry::Occupied(mut occupied) => {
                    let previous =
                        core::mem::replace(occupied.get_mut(), NeighborState::Static(link_address));
                    let event_state = occupied.get().to_event_state();
                    if event_state != previous.to_event_state() {
                        bindings_ctx.on_event(Event::changed(
                            device_id,
                            event_state,
                            neighbor,
                            bindings_ctx.now(),
                        ));
                    }
                    match previous {
                        NeighborState::Dynamic(entry) => {
                            entry.cancel_timer_and_complete_resolution(
                                core_ctx,
                                bindings_ctx,
                                timer_heap,
                                neighbor,
                                link_address,
                            );
                        }
                        NeighborState::Static(_) => {}
                    }
                }
                Entry::Vacant(vacant) => {
                    let state = vacant.insert(NeighborState::Static(link_address));
                    let event = Event::added(
                        device_id,
                        state.to_event_state(),
                        neighbor,
                        bindings_ctx.now(),
                    );
                    bindings_ctx.on_event(event);
                }
            },
        );
        Ok(())
    }

    /// Remove a static or dynamic neighbor table entry.
    pub fn remove_entry(
        &mut self,
        device_id: &<C::CoreContext as DeviceIdContext<D>>::DeviceId,
        // TODO(https://fxbug.dev/42076887): Use IPv4 subnet information to
        // disallow the address with all host bits equal to 0, and the
        // subnet broadcast addresses with all host bits equal to 1.
        // TODO(https://fxbug.dev/42083952): Use NeighborAddr when available.
        neighbor: I::Addr,
    ) -> Result<(), NeighborRemovalError> {
        let (core_ctx, bindings_ctx) = self.contexts();
        let neighbor =
            validate_neighbor_addr(neighbor).ok_or(NeighborRemovalError::IpAddressInvalid)?;

        core_ctx.with_nud_state_mut(
            device_id,
            |NudState { neighbors, last_gc: _, timer_heap }, _config| {
                match neighbors.remove(&neighbor).ok_or(NotFoundError)? {
                    NeighborState::Dynamic(mut entry) => {
                        entry.cancel_timer(bindings_ctx, timer_heap, neighbor);
                    }
                    NeighborState::Static(_) => {}
                }
                bindings_ctx.on_event(Event::removed(device_id, neighbor, bindings_ctx.now()));
                Ok(())
            },
        )
    }

    /// Writes `device`'s neighbor state information into `inspector`.
    pub fn inspect_neighbors<N: Inspector>(
        &mut self,
        device: &<C::CoreContext as DeviceIdContext<D>>::DeviceId,
        inspector: &mut N,
    ) where
        D::Address: Display,
    {
        self.core_ctx().with_nud_state(device, |nud| {
            nud.neighbors.iter().for_each(|(ip_address, state)| {
                let (state, link_address, last_confirmed_at) = match state {
                    NeighborState::Static(addr) => ("Static", Some(addr), None),
                    NeighborState::Dynamic(dynamic_state) => match dynamic_state {
                        DynamicNeighborState::Incomplete(Incomplete {
                            transmit_counter: _,
                            pending_frames: _,
                            notifiers: _,
                            _marker,
                        }) => ("Incomplete", None, None),
                        DynamicNeighborState::Reachable(Reachable {
                            link_address,
                            last_confirmed_at,
                        }) => ("Reachable", Some(link_address), Some(last_confirmed_at)),
                        DynamicNeighborState::Stale(Stale { link_address }) => {
                            ("Stale", Some(link_address), None)
                        }
                        DynamicNeighborState::Delay(Delay { link_address }) => {
                            ("Delay", Some(link_address), None)
                        }
                        DynamicNeighborState::Probe(Probe {
                            link_address,
                            transmit_counter: _,
                        }) => ("Probe", Some(link_address), None),
                        DynamicNeighborState::Unreachable(Unreachable {
                            link_address,
                            mode: _,
                        }) => ("Unreachable", Some(link_address), None),
                    },
                };
                inspector.record_unnamed_child(|inspector| {
                    inspector.record_str("State", state);
                    inspector.record_ip_addr("IpAddress", ip_address.get());
                    if let Some(link_address) = link_address {
                        inspector.record_display("LinkAddress", link_address);
                    };
                    if let Some(last_confirmed_at) = last_confirmed_at {
                        inspector.record_inspectable_value("LastConfirmedAt", last_confirmed_at);
                    }
                });
            })
        })
    }
}
