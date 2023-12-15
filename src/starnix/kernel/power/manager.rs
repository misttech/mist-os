// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::power::{SuspendState, SuspendStats};
use fidl_fuchsia_kernel as fkernel;
use fuchsia_component::client::connect_to_protocol_sync;
use fuchsia_zircon as zx;
use once_cell::sync::Lazy;
use starnix_sync::Mutex;
use starnix_uapi::{errors::Errno, from_status_like_fdio};
use std::{collections::HashSet, sync::Arc};
use zx::AsHandleRef;

static CPU_RESOURCE: Lazy<zx::Resource> = Lazy::new(|| {
    connect_to_protocol_sync::<fkernel::CpuResourceMarker>()
        .expect("couldn't connect to fuchsia.kernel.CpuResource")
        .get(zx::Time::INFINITE)
        .expect("couldn't talk to fuchsia.kernel.CpuResource")
});

#[derive(Default)]
pub struct PowerManager {
    suspend_stats: Arc<Mutex<SuspendStats>>,
    pub enable_sync_on_suspend: Mutex<bool>,
}

impl PowerManager {
    pub fn suspend_stats(&self) -> SuspendStats {
        self.suspend_stats.lock().clone()
    }

    pub fn suspend_states(&self) -> HashSet<SuspendState> {
        // TODO(b/303507442): Gets the real supported states via SGA (System Activity Governor)
        // fidl api.
        HashSet::from([SuspendState::Ram, SuspendState::Idle])
    }

    pub fn suspend(&self, _state: SuspendState) -> Result<(), Errno> {
        // TODO(b/303507442): Execute ops of suspend state transition via SGA suspend fidl api.
        // Temporary hack to trigger system suspend directly.
        let resume_at = zx::Time::after(zx::Duration::from_seconds(5));
        zx::Status::ok(unsafe {
            zx::sys::zx_system_suspend_enter(CPU_RESOURCE.raw_handle(), resume_at.into_nanos())
        })
        .map_err(|status| from_status_like_fdio!(status))
    }
}
