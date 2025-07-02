// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::fmt;
use termion::{color, style};

pub use daemon_manager::{DaemonManager, DefaultDaemonManager};
pub use recorder::{DoctorRecorder, Recorder};

mod daemon_manager;
mod recorder;

#[derive(Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(rename_all = "snake_case")]
/// Message from a doctor check run by a plugin
pub struct DoctorCheck {
    // The name of the check to run
    pub name: String,
    // Message returned by the check
    pub message: String,
    // Did the check pass
    pub passed: bool,
}

impl fmt::Display for DoctorCheck {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let check = match self.passed {
            true => format!("[{}✓{}]", color::Fg(color::Green), style::Reset),
            false => format!("[{}✗{}]", color::Fg(color::Red), style::Reset),
        };
        write!(f, "{} {}: {}", check, self.name, self.message)
    }
}
