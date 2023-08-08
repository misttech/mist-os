// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{lock::Mutex, power::SuspendStats};
use std::sync::Arc;

#[derive(Default)]
pub struct PowerManager {
    suspend_stats: Arc<Mutex<SuspendStats>>,
}

impl PowerManager {
    pub fn suspend_stats(&self) -> SuspendStats {
        self.suspend_stats.lock().clone()
    }
}
