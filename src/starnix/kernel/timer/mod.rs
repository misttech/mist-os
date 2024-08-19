// Copyright 2024 The Fuchsia Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::power::OnWakeOps;
use crate::task::{CurrentTask, HandleWaitCanceler, WaitCanceler};
use crate::time::utc;
use fuchsia_zircon::{self as zx, HandleRef};
use starnix_uapi::errors::Errno;
use std::sync::Weak;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Timeline {
    RealTime,
    Monotonic,
    BootTime,
}

impl Timeline {
    /// Returns the current time on this timeline.
    pub fn now(&self) -> zx::Time {
        match self {
            // TODO(https://fxbug.dev/328306129) handle boot and monotonic time separately
            Self::BootTime | Self::Monotonic => zx::Time::get_monotonic(),
            Self::RealTime => utc::utc_now(),
        }
    }
}

pub enum TimerWakeup {
    /// A regular timer that does not wake the system if it is suspended.
    Regular,
    /// An alarm timer that will wake the system if it is suspended.
    Alarm,
}

pub trait TimerOps: Send + Sync + 'static {
    /// Starts the timer with the specified `deadline`.
    ///
    /// This method should start the timer and schedule it to trigger at the specified `deadline`.
    /// The timer should be cancelled if it is already running.
    fn start(
        &self,
        current_task: &CurrentTask,
        source: Option<Weak<dyn OnWakeOps>>,
        deadline: zx::Time,
    ) -> Result<(), Errno>;

    /// Stops the timer.
    ///
    /// This method should stop the timer and prevent it from triggering.
    fn stop(&self, current_task: &CurrentTask) -> Result<(), Errno>;

    /// Creates a `WaitCanceler` that can be used to wait for the timer to be cancelled.
    fn wait_canceler(&self, canceler: HandleWaitCanceler) -> WaitCanceler;

    /// Returns a reference to the underlying Zircon handle.
    fn as_handle_ref(&self) -> HandleRef<'_>;
}
