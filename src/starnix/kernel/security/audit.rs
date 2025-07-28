// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::fmt::Display;

use starnix_logging::log_warn;

pub struct AuditLogger;

impl AuditLogger {
    pub fn new() -> Self {
        Self
    }

    /// Audit logging function that prints the caller `component` and its `audit_message`
    pub fn audit_log(&self, component: &str, audit_message: &impl Display) {
        log_warn!("{component}: {audit_message}");
    }
}
