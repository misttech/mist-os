// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::power::OnWakeOps;
use crate::task::{CurrentTask, HandleWaitCanceler, TargetTime, WaitCanceler};
use crate::time::utc::estimate_boot_deadline_from_utc;
use crate::vfs::timer::TimerOps;
use starnix_uapi::errors::Errno;
use starnix_uapi::{error, from_status_like_fdio};
use std::sync::{Arc, Weak};
use zx::{self as zx, AsHandleRef, HandleRef};

pub struct MonotonicZxTimer {
    timer: Arc<zx::MonotonicTimer>,
}

impl MonotonicZxTimer {
    pub fn new() -> Self {
        Self { timer: Arc::new(zx::MonotonicTimer::create()) }
    }
}

impl TimerOps for MonotonicZxTimer {
    fn start(
        &self,
        _currnet_task: &CurrentTask,
        _source: Option<Weak<dyn OnWakeOps>>,
        deadline: TargetTime,
    ) -> Result<(), Errno> {
        match deadline {
            TargetTime::Monotonic(t) => self
                .timer
                .set(t, zx::MonotonicDuration::default())
                .map_err(|status| from_status_like_fdio!(status))?,
            TargetTime::BootInstant(_) | TargetTime::RealTime(_) => return error!(EINVAL),
        };

        Ok(())
    }

    fn stop(&self, _current_task: &CurrentTask) -> Result<(), Errno> {
        self.timer.cancel().map_err(|status| from_status_like_fdio!(status))
    }

    fn wait_canceler(&self, canceler: HandleWaitCanceler) -> WaitCanceler {
        WaitCanceler::new_mono_timer(Arc::downgrade(&self.timer), canceler)
    }

    fn as_handle_ref(&self) -> HandleRef<'_> {
        self.timer.as_handle_ref()
    }
}

pub struct BootZxTimer {
    timer: Arc<zx::BootTimer>,
}

impl BootZxTimer {
    pub fn new() -> Self {
        Self { timer: Arc::new(zx::BootTimer::create()) }
    }
}

impl TimerOps for BootZxTimer {
    fn start(
        &self,
        _currnet_task: &CurrentTask,
        _source: Option<Weak<dyn OnWakeOps>>,
        deadline: TargetTime,
    ) -> Result<(), Errno> {
        match deadline {
            TargetTime::BootInstant(t) => self
                .timer
                .set(t, zx::Duration::default())
                .map_err(|status| from_status_like_fdio!(status))?,
            TargetTime::RealTime(t) => self
                .timer
                .set(estimate_boot_deadline_from_utc(t), zx::Duration::default())
                .map_err(|status| from_status_like_fdio!(status))?,
            TargetTime::Monotonic(_) => return error!(EINVAL),
        }
        Ok(())
    }

    fn stop(&self, _current_task: &CurrentTask) -> Result<(), Errno> {
        self.timer.cancel().map_err(|status| from_status_like_fdio!(status))
    }

    fn wait_canceler(&self, canceler: HandleWaitCanceler) -> WaitCanceler {
        WaitCanceler::new_boot_timer(Arc::downgrade(&self.timer), canceler)
    }

    fn as_handle_ref(&self) -> HandleRef<'_> {
        self.timer.as_handle_ref()
    }
}
