// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::security;
use crate::task::CurrentTask;
use crate::vfs::{FileSystemHandle, FileSystemOptions, FsStr, FsString};
use starnix_sync::{Locked, Mutex, Unlocked};
use starnix_uapi::errors::Errno;
use std::collections::BTreeMap;
use std::sync::Arc;

type CreateFs = Arc<
    dyn Fn(
            &mut Locked<Unlocked>,
            &CurrentTask,
            FileSystemOptions,
        ) -> Result<FileSystemHandle, Errno>
        + Send
        + Sync
        + 'static,
>;

#[derive(Default)]
pub struct FsRegistry {
    registry: Mutex<BTreeMap<FsString, CreateFs>>,
}

impl FsRegistry {
    pub fn register<F>(&self, fs_type: &FsStr, create_fs: F)
    where
        F: Fn(
                &mut Locked<Unlocked>,
                &CurrentTask,
                FileSystemOptions,
            ) -> Result<FileSystemHandle, Errno>
            + Send
            + Sync
            + 'static,
    {
        let existing = self.registry.lock().insert(fs_type.into(), Arc::new(create_fs));
        assert!(existing.is_none());
    }

    pub fn create(
        &self,
        locked: &mut Locked<Unlocked>,
        current_task: &CurrentTask,
        fs_type: &FsStr,
        options: FileSystemOptions,
    ) -> Option<Result<FileSystemHandle, Errno>> {
        let create_fs = self.registry.lock().get(fs_type).map(Arc::clone)?;
        Some(create_fs(locked, current_task, options).and_then(|fs| {
            assert_eq!(fs_type, fs.name(), "FileSystem::name() must match the registered name.");
            security::file_system_resolve_security(locked, &current_task, &fs)?;
            Ok(fs)
        }))
    }
}
