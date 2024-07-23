// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Declares types and functionality related to multicast-forwarding state.

use core::marker::PhantomData;

use derivative::Derivative;
use lock_order::lock::{OrderedLockAccess, OrderedLockRef};
use netstack3_base::sync::{Mutex, RwLock};
use netstack3_base::{AnyDevice, DeviceIdContext, IpExt, StrongDeviceIdentifier};

/// Multicast forwarding state for an IP version `I`.
///
/// Multicast forwarding can be enabled/disabled for `I` globally. When disabled
/// no state is held.
#[derive(Debug, Derivative)]
#[derivative(Default(bound = ""))]
pub enum MulticastForwardingState<I: IpExt, D: StrongDeviceIdentifier> {
    /// Multicast forwarding is disabled.
    #[derivative(Default)]
    Disabled,
    /// Multicast forwarding is enabled.
    Enabled(MulticastForwardingEnabledState<I, D>),
}

/// State held by the netstack when multicast forwarding is enabled for `I`.
#[derive(Debug, Derivative)]
#[derivative(Default(bound = ""))]
pub struct MulticastForwardingEnabledState<I: IpExt, D: StrongDeviceIdentifier> {
    /// The stack's multicast route table.
    ///
    /// Keys here must not be present in `pending_table`.
    route_table: RwLock<MulticastRouteTable<I, D>>,
    /// The stack's table of pending multicast packets.
    ///
    /// Keys here must not be present in `route_table`.
    pending_table: Mutex<MulticastForwardingPendingPackets<I, D>>,
}

impl<I: IpExt, D: StrongDeviceIdentifier> MulticastForwardingEnabledState<I, D> {
    // Helper function to circumvent lock ordering, for tests.
    #[cfg(test)]
    pub(super) fn route_table(&self) -> &RwLock<MulticastRouteTable<I, D>> {
        &self.route_table
    }
    // Helper function to circumvent lock ordering, for tests.
    #[cfg(test)]
    pub(super) fn pending_table(&self) -> &Mutex<MulticastForwardingPendingPackets<I, D>> {
        &self.pending_table
    }
}

/// A table of multicast routes specifying how to forward multicast packets.
// TODO(https://fxbug.dev/353329136): Use a real table.
#[derive(Debug, Derivative)]
#[derivative(Default(bound = ""))]
pub struct MulticastRouteTable<I: IpExt, D>(PhantomData<(I, D)>);

/// A table of pending multicast packets that have not yet been forwarded.
///
/// Packets are placed in this table when, during forwarding, there is no route
/// in the [`MulticastRouteTable`] via which to forward them. If/when such a
/// route is installed, the packets stored here can be forwarded accordingly.
// TODO(https://fxbug.dev/353328975): Use a real table.
#[derive(Debug, Derivative)]
#[derivative(Default(bound = ""))]
pub struct MulticastForwardingPendingPackets<I: IpExt, D>(PhantomData<(I, D)>);

impl<I: IpExt, D: StrongDeviceIdentifier> OrderedLockAccess<MulticastRouteTable<I, D>>
    for MulticastForwardingEnabledState<I, D>
{
    type Lock = RwLock<MulticastRouteTable<I, D>>;
    fn ordered_lock_access(&self) -> OrderedLockRef<'_, Self::Lock> {
        OrderedLockRef::new(&self.route_table)
    }
}

impl<I: IpExt, D: StrongDeviceIdentifier> OrderedLockAccess<MulticastForwardingPendingPackets<I, D>>
    for MulticastForwardingEnabledState<I, D>
{
    type Lock = Mutex<MulticastForwardingPendingPackets<I, D>>;
    fn ordered_lock_access(&self) -> OrderedLockRef<'_, Self::Lock> {
        OrderedLockRef::new(&self.pending_table)
    }
}

/// A trait providing access to [`MulticastForwardingState`].
pub trait MulticastForwardingStateContext<I: IpExt>: DeviceIdContext<AnyDevice> {
    /// The context available after locking the multicast forwarding state.
    type Ctx<'a>: MulticastRouteTableContext<
        I,
        DeviceId = Self::DeviceId,
        WeakDeviceId = Self::WeakDeviceId,
    >;
    /// Provides immutable access to the state.
    // TODO(https://fxbug.dev/353329136): Use this.
    #[allow(unused)]
    fn with_state<
        O,
        F: FnOnce(&MulticastForwardingState<I, Self::DeviceId>, &mut Self::Ctx<'_>) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O;
    /// Provides mutable access to the state.
    fn with_state_mut<
        O,
        F: FnOnce(&mut MulticastForwardingState<I, Self::DeviceId>, &mut Self::Ctx<'_>) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O;
}

/// A trait providing access to [`MulticastRouteTable`].
pub trait MulticastRouteTableContext<I: IpExt>: DeviceIdContext<AnyDevice> {
    /// The context available after locking the multicast route table.
    type Ctx<'a>: MulticastForwardingPendingPacketsContext<
        I,
        DeviceId = Self::DeviceId,
        WeakDeviceId = Self::WeakDeviceId,
    >;
    /// Provides immutable access to the route table.
    // TODO(https://fxbug.dev/353329136): Use this.
    #[allow(unused)]
    fn with_route_table<
        O,
        F: FnOnce(&MulticastRouteTable<I, Self::DeviceId>, &mut Self::Ctx<'_>) -> O,
    >(
        &mut self,
        state: &MulticastForwardingEnabledState<I, Self::DeviceId>,
        cb: F,
    ) -> O;
    /// Provides mutable access to the route table.
    fn with_route_table_mut<
        O,
        F: FnOnce(&mut MulticastRouteTable<I, Self::DeviceId>, &mut Self::Ctx<'_>) -> O,
    >(
        &mut self,
        state: &MulticastForwardingEnabledState<I, Self::DeviceId>,
        cb: F,
    ) -> O;
}

/// A trait providing access to [`MulticastForwardingPendingPackets`].
pub trait MulticastForwardingPendingPacketsContext<I: IpExt>: DeviceIdContext<AnyDevice> {
    /// Provides mutable access to the table of pending packets.
    fn with_pending_table_mut<
        O,
        F: FnOnce(&mut MulticastForwardingPendingPackets<I, Self::DeviceId>) -> O,
    >(
        &mut self,
        state: &MulticastForwardingEnabledState<I, Self::DeviceId>,
        cb: F,
    ) -> O;
}

// TODO(https://fxbug.dev/353329136): Delete this function once we're actually
// accessing the route table / pending route table. For now it's a compile time
// check that the context trait schema allows the tables to be locked.
#[allow(unused)]
fn prove_lock_schema<I: IpExt, CC: MulticastForwardingStateContext<I>>(ctx: &mut CC) {
    ctx.with_state(|state, ctx| match state {
        MulticastForwardingState::Enabled(state) => ctx.with_route_table_mut(
            &state,
            |_route_table, ctx: &mut <CC::Ctx<'_> as MulticastRouteTableContext<I>>::Ctx<'_>| {
                ctx.with_pending_table_mut(&state, |_pending_table| {})
            },
        ),
        MulticastForwardingState::Disabled => {}
    })
}
