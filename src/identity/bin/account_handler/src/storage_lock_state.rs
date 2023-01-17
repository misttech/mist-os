// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![allow(dead_code)]

use {
    crate::account::Account,
    std::{fmt, sync::Arc},
    storage_manager::StorageManager,
};

/// The states of an initialized AccountHandler.
pub enum StorageLockState<SM>
where
    SM: StorageManager,
{
    /// The account is storage-locked.
    Locked,

    /// The account is currently loaded and is available.
    Unlocked {
        account: Arc<Account<SM>>,
        // TODO(jbuckland): Add `idle_watcher` here.
    },
}

impl<SM> fmt::Debug for StorageLockState<SM>
where
    SM: StorageManager,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            StorageLockState::Locked => "Locked",
            StorageLockState::Unlocked { .. } => "Unlocked",
        };
        write!(f, "{name}")
    }
}
