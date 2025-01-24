// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Implementation of `cgroup.events` file.
//!
//! This file is read-only, and only exists on non-root cgroups. Reading cgroup.events produces
//! whether the cgroup or any of its descendants are populated, and whether the cgroup or any of its
//! ancestors are frozen (referred to as the effective freezer state).
//!
//! Full details at https://docs.kernel.org/admin-guide/cgroup-v2.html#core-interface-files

use std::borrow::Cow;
use std::sync::{Arc, Weak};

use starnix_core::task::CurrentTask;
use starnix_core::vfs::{BytesFile, BytesFileOps, FsNodeOps};
use starnix_logging::track_stub;
use starnix_uapi::errno;
use starnix_uapi::errors::Errno;

use crate::cgroup::{Cgroup, CgroupOps};

pub struct EventsFile {
    cgroup: Weak<Cgroup>,
}

impl EventsFile {
    pub fn new_node(cgroup: Weak<Cgroup>) -> impl FsNodeOps {
        BytesFile::new_node(Self { cgroup })
    }

    fn cgroup(&self) -> Result<Arc<Cgroup>, Errno> {
        self.cgroup.upgrade().ok_or_else(|| errno!(ENODEV))
    }
}

impl BytesFileOps for EventsFile {
    fn read(&self, _current_task: &CurrentTask) -> Result<Cow<'_, [u8]>, Errno> {
        track_stub!(
            TODO("https://fxbug.dev/377755814"),
            "cgroup.events does not check state of parent and children cgroup"
        );
        let cgroup = self.cgroup()?;
        let events_str = format!(
            "populated {}\nfrozen {}\n",
            cgroup.is_populated() as u8,
            cgroup.get_freezer_state().effective_freezer_state
        );
        Ok(events_str.as_bytes().to_owned().into())
    }
}
