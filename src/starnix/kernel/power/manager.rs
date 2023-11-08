// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    logging::not_implemented,
    power::{SuspendState, SuspendStats},
    types::{error, Errno},
};
use starnix_lock::Mutex;
use std::{collections::HashSet, sync::Arc};

#[derive(Default)]
pub struct PowerManager {
    suspend_stats: Arc<Mutex<SuspendStats>>,
}

impl PowerManager {
    pub fn suspend_stats(&self) -> SuspendStats {
        self.suspend_stats.lock().clone()
    }

    pub fn suspend_states(&self) -> HashSet<SuspendState> {
        // TODO(b/303507442): Gets the real supported states via SPLA Control fidl api.
        HashSet::from([SuspendState::Ram, SuspendState::Idle])
    }

    pub fn suspend(&self, _state: SuspendState) -> Result<(), Errno> {
        // TODO(b/303507442): Execute ops of suspend state transition via SPLA Suspend fidl api.
        not_implemented!("PowerManager::suspend");
        error!(ENOTSUP)
    }
}
