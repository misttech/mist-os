// Copyright 2023 The Fuchsia Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fuchsia_runtime::duplicate_utc_clock_handle;
use fuchsia_zircon::{self as zx, AsHandleRef};
use once_cell::sync::Lazy;

use crate::logging::log_warn;

static UTC_CLOCK: Lazy<zx::Clock> = Lazy::new(|| {
    duplicate_utc_clock_handle(zx::Rights::SAME_RIGHTS)
        .map_err(|status| panic!("Could not duplicate UTC clock handle: {status}"))
        .unwrap()
});

pub fn utc_now() -> zx::Time {
    #[cfg(test)]
    {
        if let Some(test_time) = UTC_CLOCK_OVERRIDE_FOR_TESTING
            .with(|cell| cell.borrow().as_ref().map(|test_clock| test_clock.read().unwrap()))
        {
            return test_time;
        }
    }
    UTC_CLOCK.read().unwrap()
}

pub fn estimate_monotonic_deadline_from_utc(utc: zx::Time) -> zx::Time {
    #[cfg(test)]
    {
        if let Some(test_time) = UTC_CLOCK_OVERRIDE_FOR_TESTING.with(|cell| {
            cell.borrow().as_ref().map(|test_clock| {
                test_clock.get_details().unwrap().mono_to_synthetic.apply_inverse(utc)
            })
        }) {
            return test_time;
        }
    }
    UTC_CLOCK.get_details().unwrap().mono_to_synthetic.apply_inverse(utc)
}

pub async fn wait_for_utc_clock_to_start() {
    // Poll the clock first to see if CLOCK_STARTED is already asserted.
    // If it is, continue silently. Otherwise we'll log that we are waiting.
    if UTC_CLOCK
        .wait_handle(zx::Signals::CLOCK_STARTED, zx::Time::INFINITE_PAST)
        .expect("UTC clock handle must be valid.")
        .contains(zx::Signals::CLOCK_STARTED)
    {
        return;
    }
    log_warn!("Waiting for UTC clock to start.");
    let _ = fuchsia_async::OnSignals::new(&*UTC_CLOCK, zx::Signals::CLOCK_STARTED)
        .await
        .expect("wait should always succeed");
    log_warn!("UTC clock has started.");
}

#[cfg(test)]
thread_local! {
    static UTC_CLOCK_OVERRIDE_FOR_TESTING: Lazy<std::cell::RefCell<Option<zx::Clock>>> =
        Lazy::new(|| std::cell::RefCell::new(None));
}

#[cfg(test)]
pub struct UtcClockOverrideGuard(());

#[cfg(test)]
impl UtcClockOverrideGuard {
    pub fn new(test_clock: zx::Clock) -> Self {
        UTC_CLOCK_OVERRIDE_FOR_TESTING.with(|cell| {
            assert_eq!(*cell.borrow(), None); // We don't expect a previously set clock override when using this type.
            *cell.borrow_mut() = Some(test_clock);
        });
        Self(())
    }
}

#[cfg(test)]
impl Drop for UtcClockOverrideGuard {
    fn drop(&mut self) {
        UTC_CLOCK_OVERRIDE_FOR_TESTING.with(|cell| {
            *cell.borrow_mut() = None;
        });
    }
}
