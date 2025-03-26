// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use starnix_logging::log_warn;
use starnix_uapi::errors::Errno;
use starnix_uapi::{errno, from_status_like_fdio};
use std::sync::{Arc, Weak};

/// A high-priority memory profile that (as of writing) disables reclamation for any mappings it
/// contains.
const MEMORY_ROLE: &str = "fuchsia.starnix.mlock";

#[derive(Clone, Debug, Default)]
pub enum MlockPinFlavor {
    #[default]
    Noop,
    ShadowProcess,
    VmarAlwaysNeed,
}

impl MlockPinFlavor {
    pub fn parse(s: &str) -> Result<Self, anyhow::Error> {
        Ok(match s {
            "noop" => Self::Noop,
            "shadow_process" => Self::ShadowProcess,
            "vmar_always_need" => Self::VmarAlwaysNeed,
            _ => anyhow::bail!(
                "unknown mlock_flavor {s}, known flavors: noop, shadow_process, vmar_always_need"
            ),
        })
    }
}

pub struct MlockShadowProcess {
    // Keep the process alive but we're never going to start it.
    _process: zx::Process,
    vmar: Arc<zx::Vmar>,
}

impl MlockShadowProcess {
    pub fn new() -> Result<Self, Errno> {
        let (_process, vmar) = zx::Process::create(
            &fuchsia_runtime::job_default(),
            zx::Name::from_bytes_lossy(b"mlock_holder"),
            Default::default(),
        )
        .map_err(|_| errno!(EINVAL))?;

        if let Err(e) = fuchsia_scheduler::set_role_for_vmar(&vmar, MEMORY_ROLE) {
            log_warn!(e:%; "Unable to set role for mlock() vmar.");
        }

        Ok(Self { _process, vmar: Arc::new(vmar) })
    }

    pub fn lock_pages(
        &self,
        vmo: &zx::Vmo,
        offset: u64,
        length: usize,
    ) -> Result<MlockMapping, Errno> {
        let base = self
            .vmar
            .map(0, vmo, offset, length, zx::VmarFlags::PERM_READ)
            .map_err(|e| from_status_like_fdio!(e))?;
        Ok(MlockMapping { vmar: Arc::downgrade(&self.vmar), base, length })
    }
}

#[derive(Clone, Debug)]
pub struct MlockMapping {
    vmar: Weak<zx::Vmar>,
    base: usize,
    length: usize,
}

impl Drop for MlockMapping {
    fn drop(&mut self) {
        if let Some(vmar) = self.vmar.upgrade() {
            // SAFETY: this address is not observable outside this module and it is just a key into
            // the high priority VMAR for this module's purposes. No pointers or references have
            // been created pointing into this mapping which makes it sound to unmap.
            if let Err(e) = unsafe { vmar.unmap(self.base, self.length) } {
                log_warn!(e:%; "Failed to unmap mlock() pin mapping.");
            }
        }
    }
}

impl std::cmp::PartialEq for MlockMapping {
    fn eq(&self, rhs: &Self) -> bool {
        Weak::ptr_eq(&self.vmar, &rhs.vmar) && self.base == rhs.base && self.length == rhs.length
    }
}
impl std::cmp::Eq for MlockMapping {}
