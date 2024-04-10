// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl_fuchsia_testing::{
    FakeClockControlMarker, FakeClockControlSynchronousProxy, FakeClockMarker,
    FakeClockSynchronousProxy, Increment,
};
use fuchsia_component::client::connect_to_protocol_sync;
use fuchsia_zircon as zx;

pub struct FakeClock {
    clock_control: FakeClockControlSynchronousProxy,
    clock: FakeClockSynchronousProxy,
}

impl FakeClock {
    pub fn new() -> Self {
        let clock = connect_to_protocol_sync::<FakeClockMarker>()
            .expect("failed to connect to FakeClockControl");
        let clock_control = connect_to_protocol_sync::<FakeClockControlMarker>()
            .expect("failed to connect to FakeClockControl");
        clock_control.pause(zx::Time::INFINITE).expect("pause fake clock");
        Self { clock, clock_control }
    }

    pub fn get(&self) -> zx::Time {
        zx::Time::from_nanos(self.clock.get(zx::Time::INFINITE).expect("get fake clock time"))
    }

    pub fn advance(&self, duration: zx::Duration) {
        self.clock_control
            .advance(&Increment::Determined(duration.into_nanos()), zx::Time::INFINITE)
            .expect("advance fake clock fidl call")
            .expect("advance fake clock succeeds");
    }
}
