// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Testing-only code for persistent Timekeeper behavior changes around
//! real-time clock (RTC) handling.

use anyhow::Result;
use fidl_fuchsia_time_test as fftt;
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use std::borrow::BorrowMut;
use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex};
use tracing::{debug, error};

/// The path to the internal production persistent state file.
#[allow(dead_code)]
const PERSISTENT_STATE_PATH: &'static str = "/data/persistent_state.json";

/// A bool shareable across threads.
///
/// This is a less powerful version of "atomic bool", fitting the use case
/// in this code:
///
/// 1. Isolate locks to short non-async fns to ensure locks are only held
///    briefly and never across `.await`s.
/// 2. Keeps the mutable state sharing code in one place.
/// 3. Implements `Sync`, as its lifetime (but not use) crosses some `.await`
///    calls in the using code. (e.g. `Rc<Refcell<_>>`` would not do.)
#[derive(Debug, Clone)]
pub struct SharedBool(Arc<Mutex<bool>>);

impl SharedBool {
    // Create a new [Self] with the provided value.
    pub fn new(initial_value: bool) -> Self {
        Self(Arc::new(Mutex::new(initial_value)))
    }

    /// Set a new value.
    ///
    /// May block the current thread if a concurrent operation is in progress.
    /// May panic if the same thread attempts to block twice; or if any lock
    /// holder panics.  Neither of these should ever happen in normal operation.
    pub fn set(&mut self, new_value: bool) {
        // Refer to docs of std::sync::Mutex for error condition details.
        let mut g = self
            .0
            .lock()
            .expect("either other holder of this lock panicked, or same thread attempted to lock");
        *(*g.borrow_mut()) = new_value;
    }

    /// Get the current value. Blocks the current thread until done.
    ///
    /// May block the current thread if a concurrent operation is in progress.
    /// May panic if the same thread attempts to lock twice; or if any lock
    /// holder panics.  Neither of these should ever happen in normal operation.
    pub fn get(&self) -> bool {
        // Refer to docs of std::sync::Mutex for error condition details.
        *self
            .0
            .lock()
            .expect("either other holder of this lock panicked, or same thread attempted to lock")
    }
}

/// Serves `fuchsia.time.test/Rtc`.
///
/// Args:
/// - `persistent_enabled_bit`: the state bit to manage.
/// - `stream`: the request stream from the test fixture.
pub async fn serve(
    mut persist_enabled: SharedBool,
    mut stream: fftt::RtcRequestStream,
) -> Result<()> {
    debug!("rtc_testing::serve: entering serving loop");
    while let Some(request) = stream.next().await {
        match request {
            Ok(fftt::RtcRequest::PersistentEnable { responder, .. }) => {
                debug!("received: fuchsia.time.test/Rtc.PersistentEnable");
                persist_enabled.set(true);
                responder.send(Ok(()))?;
                write_state(&State::new(true));
            }
            Ok(fftt::RtcRequest::PersistentDisable { responder, .. }) => {
                debug!("received: fuchsia.time.test/Rtc.PersistentDisable");
                persist_enabled.set(false);
                responder.send(Ok(()))?;
                write_state(&State::new(false));
            }
            Err(e) => {
                error!("FIDL error: {:?}", e);
            }
        };
    }
    debug!("rtc_testing::serve: exited  serving loop");
    Ok(())
}

/// How many reboots should the setting persist for?
const REBOOT_COUNT: u8 = 1;

/// The persistent state of the RTC testing API.
#[derive(Serialize, Deserialize, Debug, Default, Clone)]
pub struct State {
    // This many reboots remain until we're allowed to write to RTC again. This
    // counter is decreased by Timekeeper on each restart, until zero is reached.
    num_reboots_until_rtc_write_allowed: u8,
}

impl State {
    pub fn new(update_rtc: bool) -> Self {
        Self {
            num_reboots_until_rtc_write_allowed: match update_rtc {
                true => 0,
                // Limit the validity of the setting, expressed in number of
                // reboots.
                false => REBOOT_COUNT,
            },
        }
    }

    /// Returns whether updating the RTC clock is allowed.
    pub fn may_update_rtc(&self) -> bool {
        self.num_reboots_until_rtc_write_allowed == 0
    }
}

/// Read the persistent state. Any errors are logged only, and defaults are returned.
pub fn read_and_update_state() -> State {
    read_and_update_state_internal(PERSISTENT_STATE_PATH)
}

pub fn read_and_update_state_internal<P: AsRef<Path>>(path: P) -> State {
    let path = path.as_ref();
    let state: State = fs::read_to_string(path)
        .as_ref()
        .map(|s| {
            serde_json::from_str(s)
                .map_err(|e| error!("while deserializing: {:?}: {:?}", path, e))
                .unwrap_or(Default::default())
        })
        .map_err(|e| debug!("while reading: {:?}", e))
        .unwrap_or(Default::default());
    debug!("read persistent state: {:?}", &state);

    // Change the reboot limit.
    let mut next = state.clone();
    next.num_reboots_until_rtc_write_allowed =
        state.num_reboots_until_rtc_write_allowed.saturating_sub(1);
    write_state_internal(path, &next);

    // Return the previous state.
    state
}

/// Write the persistent state to mutable persistent storage.
pub fn write_state(state: &State) {
    write_state_internal(PERSISTENT_STATE_PATH, state)
}

fn write_state_internal<P: AsRef<Path>>(path: P, state: &State) {
    let path = path.as_ref();
    debug!("writing persistent state: {:?} to {:?}", state, path);
    serde_json::to_string(state)
        .map(|s| {
            fs::write(path, s)
                .map_err(|e| error!("while writing persistent state: {:?}: {:?}", path, e))
                .map_err(|e| error!("while serializing state from: {:?}: {:?}", path, e))
                .unwrap_or(())
        })
        .unwrap_or(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_roundtrip() {
        let d = tempfile::TempDir::new().expect("tempdir created");
        let p = d.path().join("file.json");
        let s = State::new(false);
        write_state_internal(&p, &s);

        // First time around, we may not update the RTC.
        assert!(!read_and_update_state_internal(&p).may_update_rtc());
        // Second time around, we may.
        assert!(read_and_update_state_internal(&p).may_update_rtc());
    }
}
