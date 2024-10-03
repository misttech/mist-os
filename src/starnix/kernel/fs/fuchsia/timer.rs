// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::power::OnWakeOps;
use crate::task::{CurrentTask, HandleWaitCanceler, TargetTime, WaitCanceler};
use crate::vfs::timer::TimerOps;
use starnix_uapi::errors::Errno;
use starnix_uapi::from_status_like_fdio;
use std::sync::{Arc, Weak};
use zx::{self as zx, AsHandleRef, HandleRef};

pub struct ZxTimer {
    timer: Arc<zx::Timer>,
}

impl ZxTimer {
    pub fn new() -> Self {
        // TODO(https://fxbug.dev/361583841) rationalize with boot time
        Self { timer: Arc::new(zx::MonotonicTimer::create()) }
    }
}

impl TimerOps for ZxTimer {
    fn start(
        &self,
        _currnet_task: &CurrentTask,
        _source: Option<Weak<dyn OnWakeOps>>,
        deadline: TargetTime,
    ) -> Result<(), Errno> {
        self.timer
            .set(deadline.estimate_monotonic(), zx::Duration::default())
            .map_err(|status| from_status_like_fdio!(status))?;
        Ok(())
    }

    fn stop(&self, _current_task: &CurrentTask) -> Result<(), Errno> {
        self.timer.cancel().map_err(|status| from_status_like_fdio!(status))
    }

    fn wait_canceler(&self, canceler: HandleWaitCanceler) -> WaitCanceler {
        WaitCanceler::new_timer(Arc::downgrade(&self.timer), canceler)
    }

    fn as_handle_ref(&self) -> HandleRef<'_> {
        self.timer.as_handle_ref()
    }
}
