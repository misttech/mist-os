// Copyright 2023 The Fuchsia Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{lock::Mutex, logging::log_warn, vdso::vdso_loader::MemoryMappedVvar};
use fuchsia_runtime::duplicate_utc_clock_handle;
use fuchsia_zircon::{self as zx, AsHandleRef, ClockTransformation};
use once_cell::sync::Lazy;

#[derive(Debug)]
enum UtcClockSource {
    MonotonicWithOffset(zx::Duration),
    Clock(zx::Clock),
}

impl UtcClockSource {
    pub const fn new() -> Self {
        Self::MonotonicWithOffset(zx::Duration::from_nanos(0))
    }

    pub fn now(&self) -> zx::Time {
        match self {
            UtcClockSource::MonotonicWithOffset(offset) => zx::Time::get_monotonic() + *offset,
            UtcClockSource::Clock(clock) => clock.read().unwrap(),
        }
    }

    pub fn estimate_monotonic_deadline(&self, utc: zx::Time) -> zx::Time {
        match self {
            UtcClockSource::MonotonicWithOffset(offset) => utc - *offset,
            UtcClockSource::Clock(clock) => {
                clock.get_details().unwrap().mono_to_synthetic.apply_inverse(utc)
            }
        }
    }

    fn get_transform(&self) -> ClockTransformation {
        match self {
            UtcClockSource::MonotonicWithOffset(offset) => ClockTransformation {
                reference_offset: 0,
                synthetic_offset: (*offset).into_nanos(),
                rate: zx::sys::zx_clock_rate_t { synthetic_ticks: 1, reference_ticks: 1 },
            },
            UtcClockSource::Clock(clock) => clock.get_details().unwrap().mono_to_synthetic,
        }
    }

    pub fn write_vvar_data_transform_to(&self, dest: &MemoryMappedVvar) {
        let new_transform = self.get_transform();
        dest.update_utc_data_transform(&new_transform);
    }
}

static UTC_CLOCK_SOURCE: Lazy<Mutex<UtcClockSource>> =
    Lazy::new(|| Mutex::new(UtcClockSource::new()));

pub fn utc_write_vvar_data_transform_to(dest: &MemoryMappedVvar) {
    (*UTC_CLOCK_SOURCE).lock().write_vvar_data_transform_to(dest);
}

pub fn utc_now() -> zx::Time {
    #[cfg(test)]
    {
        if let Some(test_time) = UTC_CLOCK_OVERRIDE_FOR_TESTING
            .with(|cell| cell.borrow().as_ref().map(|test_clock| test_clock.read().unwrap()))
        {
            return test_time;
        }
    }
    (*UTC_CLOCK_SOURCE).lock().now()
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
    (*UTC_CLOCK_SOURCE).lock().estimate_monotonic_deadline(utc)
}

pub async fn start_utc_clock() {
    let real_utc_clock = duplicate_utc_clock_handle(zx::Rights::SAME_RIGHTS).unwrap();
    // Poll the clock first to see if CLOCK_STARTED is already asserted.
    // If it is, continue silently. Otherwise we'll log that we are waiting.
    match real_utc_clock.wait_handle(zx::Signals::CLOCK_STARTED, zx::Time::INFINITE_PAST) {
        Ok(e) if e.contains(zx::Signals::CLOCK_STARTED) => {
            *(*UTC_CLOCK_SOURCE).lock() = UtcClockSource::Clock(real_utc_clock);
            return;
        }
        Ok(_) | Err(zx::Status::TIMED_OUT) => {}
        Err(e) => {
            log_warn!("Error fetching initial UTC clock value: {:?}", e);
        }
    }

    log_warn!("Waiting for real UTC clock to start, using synthetic clock in the meantime.");
    // Pick an initial offset so that UTC times appear to start at the backstop time and advance
    // forward.  Once the real UTC clock starts we expect it to start a time newer than the backstop
    // time so the clock will jump forward. It could jump backwards if we started running close to
    // the backstop time and our monotonic clock runs much faster than the external UTC reference.
    let offset = real_utc_clock.get_details().unwrap().backstop - zx::Time::get_monotonic();
    *(*UTC_CLOCK_SOURCE).lock() = UtcClockSource::MonotonicWithOffset(offset);
    fuchsia_async::Task::spawn(async move {
        let _ = fuchsia_async::OnSignals::new(&real_utc_clock, zx::Signals::CLOCK_STARTED)
            .await
            .expect("wait should always succeed");
        log_warn!("Real UTC clock has started, replacing synthetic clock.");
        *(*UTC_CLOCK_SOURCE).lock() = UtcClockSource::Clock(real_utc_clock);
    })
    .detach();
}

#[cfg(test)]
thread_local! {
    static UTC_CLOCK_OVERRIDE_FOR_TESTING: std::cell::RefCell<Option<zx::Clock>> =
        std::cell::RefCell::new(None);
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
