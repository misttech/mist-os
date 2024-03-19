// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use core::fmt::Debug;

use net_types::ip::{Ipv4, Ipv6};
use packet_formats::ip::IpExt;

use crate::state::validation::ValidState;

/// Trait defining the `DeviceClass` type provided by bindings.
///
/// Allows rules that match on device class to be installed, storing the
/// [`FilterBindingsTypes::DeviceClass`] type at rest, while allowing Netstack3
/// Core to have Bindings provide the type since it is platform-specific.
pub trait FilterBindingsTypes {
    /// The device class type for devices installed in the netstack.
    type DeviceClass: Clone + Debug;
}

/// The IP version-specific execution context for packet filtering.
///
/// This trait exists to abstract over access to the filtering state. It is
/// useful to implement filtering logic in terms of this trait, as opposed to,
/// for example, [`crate::logic::FilterHandler`] methods taking the state
/// directly as an argument, because it allows Netstack3 Core to use lock
/// ordering types to enforce that filtering state is only acquired at or before
/// a given lock level, while keeping test code free of locking concerns.
pub trait FilterIpContext<I: IpExt, BT: FilterBindingsTypes> {
    /// Calls the function with a reference to filtering state.
    fn with_filter_state<O, F: FnOnce(&ValidState<I, BT::DeviceClass>) -> O>(&mut self, cb: F)
        -> O;
}

/// A context for mutably accessing all filtering state at once, to allow IPv4
/// and IPv6 filtering state to be modified atomically.
pub trait FilterContext<BT: FilterBindingsTypes> {
    /// Calls the function with a mutable reference to all filtering state.
    fn with_all_filter_state_mut<
        O,
        F: FnOnce(&mut ValidState<Ipv4, BT::DeviceClass>, &mut ValidState<Ipv6, BT::DeviceClass>) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O;
}

#[cfg(test)]
pub(crate) mod testutil {
    use super::*;
    use crate::state::{IpRoutines, State};

    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    pub enum FakeDeviceClass {
        Ethernet,
        Wlan,
    }

    pub enum FakeBindingsTypes {}

    impl FilterBindingsTypes for FakeBindingsTypes {
        type DeviceClass = FakeDeviceClass;
    }

    pub struct FakeCtx<I: IpExt>(ValidState<I, FakeDeviceClass>);

    impl<I: IpExt> FakeCtx<I> {
        pub fn with_ip_routines(routines: IpRoutines<I, FakeDeviceClass, ()>) -> Self {
            let state = ValidState::new(State { ip_routines: routines, ..Default::default() })
                .expect("invalid state");
            Self(state)
        }
    }

    impl<I: IpExt> FilterIpContext<I, FakeBindingsTypes> for FakeCtx<I> {
        fn with_filter_state<O, F: FnOnce(&ValidState<I, FakeDeviceClass>) -> O>(
            &mut self,
            cb: F,
        ) -> O {
            let Self(state) = self;
            cb(&*state)
        }
    }
}
