// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::vfs::{NamespaceNode, WhatToMount};
use starnix_logging::log_error;
use starnix_sync::Mutex;
use starnix_uapi::errors::Errno;
use starnix_uapi::mount_flags::MountFlags;
use starnix_uapi::ownership::ReleasableByRef;

/// A record of the mounts that should be unmounted.
///
/// When the record is dropped, the mounts are unmounted.
#[derive(Default)]
pub struct MountRecord {
    /// The mounts that should be unmounted.
    mounts: Mutex<Vec<NamespaceNode>>,
}

impl MountRecord {
    pub fn mount(
        &self,
        mount_point: NamespaceNode,
        what: WhatToMount,
        flags: MountFlags,
    ) -> Result<(), Errno> {
        let mut mounts = self.mounts.lock();
        mount_point.mount(what, flags)?;
        mounts.push(mount_point);
        Ok(())
    }

    pub fn adopt(&self, mount_record: MountRecord) {
        let mut additional_mounts: Vec<_> = mount_record.mounts.lock().drain(..).collect();
        let mut mounts = self.mounts.lock();
        mounts.append(&mut additional_mounts);
    }
}

impl ReleasableByRef for MountRecord {
    type Context<'a> = ();
    fn release(&self, _context: Self::Context<'_>) {
        let mut mounts = self.mounts.lock();
        while let Some(name) = mounts.pop() {
            match name.unmount() {
                Ok(()) => {}
                Err(e) => {
                    log_error!("failed to unmount {}: {:?}", name.path_escaping_chroot(), e);
                    break;
                }
            }
        }
        *mounts = Default::default();
    }
}
