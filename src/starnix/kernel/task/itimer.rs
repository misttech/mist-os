// Copyright 2023 The Fuchsia Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::lock::Mutex;
use crate::signals::{send_signal, SignalInfo};
use crate::time::utc;
use fuchsia_async as fasync;
use fuchsia_zircon as zx;
use std::sync::{Arc, Weak};

use super::ThreadGroup;

#[derive(Default)]
pub struct Itimer {
    state: Mutex<ItimerMutableState>,
}

#[derive(Default)]
pub struct TimerRemaining {
    pub remainder: zx::Duration,
    pub interval: zx::Duration,
}

impl Itimer {
    pub fn arm(
        self: &Arc<Self>,
        thread_group: Weak<ThreadGroup>,
        executor: &fasync::EHandle,
        target_utc: zx::Time,
        interval: zx::Duration,
    ) {
        {
            let mut guard = self.state.lock();
            guard.armed = true;
            guard.target_utc = target_utc;
            guard.interval = interval;
        }
        let self_ref = self.clone();
        fasync::Task::spawn_on(executor, async move {
            loop {
                loop {
                    // We may have to issue multiple sleeps if the target time in the timer is
                    // updated while we are sleeping or if our estimation of the target time
                    // relative to the monotonic clock is off.
                    let target_monotonic =
                        utc::estimate_monotonic_deadline_from_utc(self_ref.state.lock().target_utc);
                    if zx::Time::get_monotonic() >= target_monotonic {
                        break;
                    }
                    fuchsia_async::Timer::new(target_monotonic).await;
                }
                if !self_ref.state.lock().armed {
                    return;
                }
                if let Some(thread_group) = thread_group.upgrade() {
                    let signal_info = SignalInfo::default(crate::types::signals::SIGALRM);
                    let signal_target =
                        thread_group.read().get_signal_target(&signal_info.signal.into());
                    if let Some(task) = &signal_target {
                        send_signal(task, signal_info);
                    }
                    let mut guard = self_ref.state.lock();
                    if guard.interval != zx::Duration::default() {
                        guard.target_utc = utc::utc_now() + guard.interval;
                    } else {
                        guard.armed = false;
                        break;
                    }
                } else {
                    break;
                }
            }
        })
        .detach();
    }

    pub fn disarm(&self) {
        self.state.lock().armed = false;
    }

    pub fn time_remaining(&self) -> TimerRemaining {
        let guard = self.state.lock();
        if !guard.armed {
            return TimerRemaining::default();
        }

        TimerRemaining { remainder: guard.target_utc - utc::utc_now(), interval: guard.interval }
    }
}

#[derive(Default)]
struct ItimerMutableState {
    armed: bool,
    target_utc: zx::Time,
    interval: zx::Duration,
}
