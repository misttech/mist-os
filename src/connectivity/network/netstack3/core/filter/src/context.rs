// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use core::fmt::Debug;

use net_types::ip::{Ipv4, Ipv6};
use netstack3_base::{InstantBindingsTypes, InstantContext};
use packet_formats::ip::IpExt;

use crate::state::State;

/// Trait defining required types for filtering provided by bindings.
///
/// Allows rules that match on device class to be installed, storing the
/// [`FilterBindingsTypes::DeviceClass`] type at rest, while allowing Netstack3
/// Core to have Bindings provide the type since it is platform-specific.
pub trait FilterBindingsTypes: InstantBindingsTypes {
    /// The device class type for devices installed in the netstack.
    type DeviceClass: Clone + Debug;
}

/// Trait aggregating functionality required from bindings.
pub trait FilterBindingsContext: InstantContext + FilterBindingsTypes {}
impl<BC: InstantContext + FilterBindingsTypes> FilterBindingsContext for BC {}

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
    fn with_filter_state<O, F: FnOnce(&State<I, BT>) -> O>(&mut self, cb: F) -> O;
}

/// A context for mutably accessing all filtering state at once, to allow IPv4
/// and IPv6 filtering state to be modified atomically.
pub trait FilterContext<BT: FilterBindingsTypes> {
    /// Calls the function with a mutable reference to all filtering state.
    fn with_all_filter_state_mut<O, F: FnOnce(&mut State<Ipv4, BT>, &mut State<Ipv6, BT>) -> O>(
        &mut self,
        cb: F,
    ) -> O;
}

#[cfg(test)]
pub(crate) mod testutil {
    use core::time::Duration;

    use netstack3_base::testutil::FakeInstant;

    use super::*;
    use crate::{
        conntrack,
        state::{validation::ValidRoutines, IpRoutines, Routines},
    };

    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    pub enum FakeDeviceClass {
        Ethernet,
        Wlan,
    }

    pub enum FakeBindingsTypes {}

    impl InstantBindingsTypes for FakeBindingsTypes {
        type Instant = FakeInstant;
    }

    impl FilterBindingsTypes for FakeBindingsTypes {
        type DeviceClass = FakeDeviceClass;
    }

    pub struct FakeCtx<I: IpExt, BT: FilterBindingsTypes>(State<I, BT>);

    impl<I: IpExt, BT: FilterBindingsTypes> FakeCtx<I, BT> {
        #[allow(dead_code)]
        pub fn with_ip_routines(routines: IpRoutines<I, BT::DeviceClass, ()>) -> Self {
            let (installed_routines, uninstalled_routines) =
                ValidRoutines::new(Routines { ip: routines, ..Default::default() })
                    .expect("invalid state");
            Self(State {
                installed_routines,
                uninstalled_routines,
                conntrack: conntrack::Table::new(),
            })
        }
    }

    impl<I: IpExt> FilterIpContext<I, FakeBindingsTypes> for FakeCtx<I, FakeBindingsTypes> {
        fn with_filter_state<O, F: FnOnce(&State<I, FakeBindingsTypes>) -> O>(
            &mut self,
            cb: F,
        ) -> O {
            let Self(state) = self;
            cb(&*state)
        }
    }

    #[derive(Debug)]
    pub struct FakeBindingsCtx {
        time_elapsed: Duration,
    }

    #[allow(dead_code)]
    impl FakeBindingsCtx {
        pub(crate) fn new() -> Self {
            Self { time_elapsed: Duration::from_secs(0) }
        }

        pub(crate) fn set_time_elapsed(&mut self, time_elapsed: Duration) {
            self.time_elapsed = time_elapsed;
        }
    }

    impl InstantBindingsTypes for FakeBindingsCtx {
        type Instant = FakeInstant;
    }

    impl FilterBindingsTypes for FakeBindingsCtx {
        type DeviceClass = FakeDeviceClass;
    }

    impl InstantContext for FakeBindingsCtx {
        fn now(&self) -> Self::Instant {
            Self::Instant { offset: self.time_elapsed }
        }
    }
}
