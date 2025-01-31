// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use loom::sync::Arc;
use netstack3_base::socket::{SendBufferFullError, SendBufferTracking, SocketWritableListener};
use netstack3_base::PositiveIsize;

use super::{loom_model, loom_spawn, low_preemption_bound_model};

struct Listener {
    writable: bool,
}

impl Default for Listener {
    fn default() -> Self {
        Self { writable: true }
    }
}

impl SocketWritableListener for Listener {
    fn on_writable_changed(&mut self, writable: bool) {
        let old = core::mem::replace(&mut self.writable, writable);
        // Avoid double notification.
        assert_ne!(old, writable);
    }
}

#[test]
fn race_sndbuffer_writable() {
    loom_model(low_preemption_bound_model(), || {
        const SNDBUF: PositiveIsize = PositiveIsize::new(4).unwrap();
        const ACQ: PositiveIsize = PositiveIsize::new(3).unwrap();
        let tracking = Arc::new(SendBufferTracking::new(SNDBUF, Listener::default()));
        let snd = tracking.acquire(ACQ).expect("acquire");
        let ta = {
            let tracking = Arc::clone(&tracking);
            loom_spawn(move || {
                tracking.release(snd);
            })
        };
        let tb = {
            let tracking = Arc::clone(&tracking);
            loom_spawn(move || tracking.acquire(ACQ).expect("acquire"))
        };
        ta.join().expect("join a");
        let outstanding = tb.join().expect("join b");
        tracking.with_listener(|Listener { writable }| {
            assert_eq!(*writable, true);
        });
        tracking.release(outstanding);
    })
}

#[test]
fn race_sndbuffer_not_writable() {
    loom_model(low_preemption_bound_model(), || {
        const SNDBUF: PositiveIsize = PositiveIsize::new(4).unwrap();
        let tracking = Arc::new(SendBufferTracking::new(SNDBUF, Listener::default()));
        // Should have no more space left.
        let snd = tracking.acquire(SNDBUF).expect("acquire");
        tracking.with_listener(|Listener { writable }| {
            assert_eq!(*writable, false);
        });
        let ta = {
            let tracking = Arc::clone(&tracking);
            loom_spawn(move || {
                tracking.release(snd);
            })
        };
        let tb = {
            let tracking = Arc::clone(&tracking);
            loom_spawn(move || tracking.acquire(SNDBUF))
        };
        ta.join().expect("join a");
        let acquire_result = tb.join().expect("join b");
        match acquire_result {
            Ok(outstanding) => {
                tracking.with_listener(|Listener { writable }| {
                    assert_eq!(*writable, false);
                });
                tracking.release(outstanding);
                tracking.with_listener(|Listener { writable }| {
                    assert_eq!(*writable, true);
                });
            }
            Err(SendBufferFullError) => {
                // Lost the race, but socket should be writable now.
                tracking.with_listener(|Listener { writable }| {
                    assert_eq!(*writable, true);
                });
                let space = tracking.acquire(SNDBUF).expect("acquire");
                space.acknowledge_drop();
            }
        }
    })
}
