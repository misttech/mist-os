// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Testing-only code for persistent Timekeeper behavior changes around
//! real-time clock (RTC) handling.

use log::{debug, error};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

/// The path to the internal production persistent state file.
const PERSISTENT_STATE_PATH: &'static str = "/data/persistent_state.json";

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
                .unwrap_or_else(|_| Default::default())
        })
        .map_err(|e| debug!("while reading: {:?}", e))
        .unwrap_or_else(|_| Default::default());
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
