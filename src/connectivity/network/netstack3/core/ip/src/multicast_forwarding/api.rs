// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Declares the API for configuring multicast forwarding within the netstack.

use net_types::ip::{Ip, IpVersionMarker};
use netstack3_base::{ContextPair, IpExt};

use crate::internal::multicast_forwarding::state::{
    MulticastForwardingState, MulticastForwardingStateContext,
};

/// The multicast forwarding API.
pub struct MulticastForwardingApi<I: Ip, C> {
    ctx: C,
    _ip_mark: IpVersionMarker<I>,
}

impl<I: Ip, C> MulticastForwardingApi<I, C> {
    /// Constructs a new multicast forwarding API.
    pub fn new(ctx: C) -> Self {
        Self { ctx, _ip_mark: IpVersionMarker::new() }
    }
}

impl<I: IpExt, C> MulticastForwardingApi<I, C>
where
    C: ContextPair,
    C::CoreContext: MulticastForwardingStateContext<I>,
{
    fn core_ctx(&mut self) -> &mut C::CoreContext {
        let Self { ctx, _ip_mark } = self;
        ctx.core_ctx()
    }

    /// Enables multicast forwarding.
    ///
    /// Returns whether multicast forwarding was newly enabled.
    pub fn enable(&mut self) -> bool {
        self.core_ctx().with_state_mut(|state, _ctx| match state {
            MulticastForwardingState::Enabled(_) => false,
            MulticastForwardingState::Disabled => {
                *state = MulticastForwardingState::Enabled(Default::default());
                true
            }
        })
    }

    /// Disables multicast forwarding.
    ///
    /// Returns whether multicast forwarding was newly disabled.
    ///
    /// Upon being disabled, the multicast route table will be cleared,
    /// and all pending packets will be dropped.
    pub fn disable(&mut self) -> bool {
        self.core_ctx().with_state_mut(|state, _ctx| match state {
            MulticastForwardingState::Disabled => false,
            MulticastForwardingState::Enabled(_) => {
                *state = MulticastForwardingState::Disabled;
                true
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use core::cell::RefCell;
    use core::ops::Deref;

    use alloc::rc::Rc;
    use assert_matches::assert_matches;
    use derivative::Derivative;
    use ip_test_macro::ip_test;
    use netstack3_base::testutil::{FakeStrongDeviceId, MultipleDevicesId};
    use netstack3_base::CtxPair;

    use crate::multicast_forwarding::{
        MulticastForwardingEnabledState, MulticastForwardingPendingPackets,
        MulticastForwardingPendingPacketsContext, MulticastRouteTable, MulticastRouteTableContext,
    };

    #[derive(Derivative)]
    #[derivative(Default(bound = ""))]
    struct FakeCoreCtxState<I: IpExt, D: FakeStrongDeviceId> {
        // NB: Hold in an `Rc<RefCell<...>>` to switch to runtime borrow
        // checking. This allows us to borrow the multicast forwarding state at
        // the same time as the outer `FakeCoreCtx` is mutably borrowed.
        multicast_forwarding: Rc<RefCell<MulticastForwardingState<I, D>>>,
    }

    type FakeBindingsCtx = netstack3_base::testutil::FakeBindingsCtx<(), (), (), ()>;
    type FakeCoreCtx<I, D> = netstack3_base::testutil::FakeCoreCtx<FakeCoreCtxState<I, D>, (), D>;

    impl<I: IpExt, D: FakeStrongDeviceId> MulticastForwardingStateContext<I> for FakeCoreCtx<I, D> {
        type Ctx<'a> = FakeCoreCtx<I, D>;
        fn with_state<
            O,
            F: FnOnce(&MulticastForwardingState<I, Self::DeviceId>, &mut Self::Ctx<'_>) -> O,
        >(
            &mut self,
            cb: F,
        ) -> O {
            let state = self.state.multicast_forwarding.clone();
            let borrow = state.borrow();
            cb(&borrow, self)
        }
        fn with_state_mut<
            O,
            F: FnOnce(&mut MulticastForwardingState<I, Self::DeviceId>, &mut Self::Ctx<'_>) -> O,
        >(
            &mut self,
            cb: F,
        ) -> O {
            let state = self.state.multicast_forwarding.clone();
            let mut borrow = state.borrow_mut();
            cb(&mut borrow, self)
        }
    }

    impl<I: IpExt, D: FakeStrongDeviceId> MulticastRouteTableContext<I> for FakeCoreCtx<I, D> {
        type Ctx<'a> = FakeCoreCtx<I, D>;
        fn with_route_table<
            O,
            F: FnOnce(&MulticastRouteTable<I, Self::DeviceId>, &mut Self::Ctx<'_>) -> O,
        >(
            &mut self,
            state: &MulticastForwardingEnabledState<I, Self::DeviceId>,
            cb: F,
        ) -> O {
            let route_table = state.route_table().read();
            cb(&route_table, self)
        }
        fn with_route_table_mut<
            O,
            F: FnOnce(&mut MulticastRouteTable<I, Self::DeviceId>, &mut Self::Ctx<'_>) -> O,
        >(
            &mut self,
            state: &MulticastForwardingEnabledState<I, Self::DeviceId>,
            cb: F,
        ) -> O {
            let mut route_table = state.route_table().write();
            cb(&mut route_table, self)
        }
    }

    impl<I: IpExt, D: FakeStrongDeviceId> MulticastForwardingPendingPacketsContext<I>
        for FakeCoreCtx<I, D>
    {
        fn with_pending_table_mut<
            O,
            F: FnOnce(&mut MulticastForwardingPendingPackets<I, Self::DeviceId>) -> O,
        >(
            &mut self,
            state: &MulticastForwardingEnabledState<I, Self::DeviceId>,
            cb: F,
        ) -> O {
            let mut pending_table = state.pending_table().lock();
            cb(&mut pending_table)
        }
    }

    fn new_multicast_forwarding_api<I: IpExt>(
    ) -> MulticastForwardingApi<I, CtxPair<FakeCoreCtx<I, MultipleDevicesId>, FakeBindingsCtx>>
    {
        MulticastForwardingApi::new(CtxPair::with_core_ctx(FakeCoreCtx::with_state(
            Default::default(),
        )))
    }

    #[ip_test(I)]
    fn enable_disable<I: IpExt>() {
        let mut api = new_multicast_forwarding_api::<I>();

        assert_matches!(
            api.core_ctx().state.multicast_forwarding.borrow().deref(),
            &MulticastForwardingState::Disabled
        );
        assert!(api.enable());
        assert!(!api.enable());
        assert_matches!(
            api.core_ctx().state.multicast_forwarding.borrow().deref(),
            &MulticastForwardingState::Enabled(_)
        );
        assert!(api.disable());
        assert!(!api.disable());
        assert_matches!(
            api.core_ctx().state.multicast_forwarding.borrow().deref(),
            &MulticastForwardingState::Disabled
        );
    }
}
