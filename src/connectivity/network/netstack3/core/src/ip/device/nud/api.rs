// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Neighbor API structs.

use core::marker::PhantomData;

use net_types::{
    ip::{Ip, IpAddress, IpVersionMarker, Ipv4, Ipv6},
    SpecifiedAddr, UnicastAddr, UnicastAddress as _, Witness as _,
};
use thiserror::Error;

use crate::{
    context::{ContextPair, InstantBindingsTypes},
    device::{link::LinkDevice, DeviceIdContext},
    error::NotFoundError,
    ip::device::nud::{
        LinkResolutionContext, LinkResolutionNotifier, LinkResolutionResult, NeighborStateInspect,
        NudBindingsContext, NudContext, NudHandler,
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

// TODO(https://fxbug.dev/134098): Use NeighborAddr to witness these properties.
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

/// Visitor for NUD state.
pub trait NeighborVisitor<A: IpAddress, LinkAddress, T> {
    /// Performs a user-defined operation over an iterator of neighbor state.
    fn visit_neighbors(
        &mut self,
        neighbors: impl Iterator<Item = NeighborStateInspect<A, LinkAddress, T>>,
    );
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
        device: &<C::CoreContext as DeviceIdContext<D>>::DeviceId,
        dst: &SpecifiedAddr<I::Addr>,
    ) -> LinkResolutionResult<
        D::Address,
        <<C::BindingsContext as LinkResolutionContext<D>>::Notifier as LinkResolutionNotifier<
            D,
        >>::Observer,
    >{
        let (core_ctx, bindings_ctx) = self.contexts();
        NudHandler::<I, D, _>::resolve_link_addr(core_ctx, bindings_ctx, device, dst)
    }

    /// Flush neighbor table entries.
    pub fn flush_table(&mut self, device: &<C::CoreContext as DeviceIdContext<D>>::DeviceId) {
        let (core_ctx, bindings_ctx) = self.contexts();
        NudHandler::<I, D, _>::flush(core_ctx, bindings_ctx, device)
    }

    /// Inserts a static neighbor entry for a neighbor.
    pub fn insert_static_entry(
        &mut self,
        device: &<C::CoreContext as DeviceIdContext<D>>::DeviceId,
        addr: I::Addr,
        link_addr: D::Address,
    ) -> Result<(), StaticNeighborInsertionError> {
        let link_addr = UnicastAddr::new(link_addr)
            .ok_or(StaticNeighborInsertionError::MacAddressNotUnicast)?;
        let addr =
            validate_neighbor_addr(addr).ok_or(StaticNeighborInsertionError::IpAddressInvalid)?;
        let (core_ctx, bindings_ctx) = self.contexts();
        Ok(NudHandler::<I, D, _>::set_static_neighbor(
            core_ctx,
            bindings_ctx,
            device,
            addr,
            *link_addr,
        ))
    }

    /// Remove a static or dynamic neighbor table entry.
    pub fn remove_entry(
        &mut self,
        device: &<C::CoreContext as DeviceIdContext<D>>::DeviceId,
        addr: I::Addr,
    ) -> Result<(), NeighborRemovalError> {
        let (core_ctx, bindings_ctx) = self.contexts();
        let addr = validate_neighbor_addr(addr).ok_or(NeighborRemovalError::IpAddressInvalid)?;
        NudHandler::<I, D, _>::delete_neighbor(core_ctx, bindings_ctx, device, addr)
            .map_err(Into::into)
    }

    /// Provides access to NUD state via a `visitor`.
    pub fn inspect_neighbors<V>(
        &mut self,
        device: &<C::CoreContext as DeviceIdContext<D>>::DeviceId,
        visitor: &mut V,
    ) where
        V: NeighborVisitor<
            I::Addr,
            D::Address,
            <C::BindingsContext as InstantBindingsTypes>::Instant,
        >,
    {
        self.core_ctx().with_nud_state(device, |nud| visitor.visit_neighbors(nud.state_iter()))
    }
}
