// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::fs::pseudo_directory::PseudoDirectoryBuilder;
use crate::power::{
    PowerStateFile, PowerSyncOnSuspendFile, PowerWakeLockFile, PowerWakeUnlockFile,
    PowerWakeupCountFile,
};
use crate::task::Kernel;
use crate::vfs::create_bytes_file_with_handler;
use starnix_uapi::auth::FsCred;
use starnix_uapi::file_mode::mode;
use std::sync::Arc;

pub fn sysfs_power_directory(kernel: &Arc<Kernel>, dir: &mut PseudoDirectoryBuilder) {
    let weak_kernel = Arc::downgrade(kernel);
    dir.node(b"wakeup_count", mode!(IFREG, 0o644), FsCred::root(), || {
        PowerWakeupCountFile::new_node()
    });
    dir.node(b"wake_lock", mode!(IFREG, 0o660), FsCred::root(), || PowerWakeLockFile::new_node());
    dir.node(b"wake_unlock", mode!(IFREG, 0o660), FsCred::root(), || {
        PowerWakeUnlockFile::new_node()
    });
    dir.node(b"state", mode!(IFREG, 0o644), FsCred::root(), || PowerStateFile::new_node());
    dir.node(b"sync_on_suspend", mode!(IFREG, 0o644), FsCred::root(), || {
        PowerSyncOnSuspendFile::new_node()
    });
    {
        let dir = dir.subdir(b"suspend_stats");
        let k = weak_kernel.clone();
        dir.node(b"success", mode!(IFREG, 0o444), FsCred::root(), move || {
            create_bytes_file_with_handler(k.clone(), |kernel| {
                kernel.suspend_resume_manager.suspend_stats().success_count.to_string()
            })
        });
        let k = weak_kernel.clone();
        dir.node(b"fail", mode!(IFREG, 0o444), FsCred::root(), move || {
            create_bytes_file_with_handler(k.clone(), |kernel| {
                kernel.suspend_resume_manager.suspend_stats().fail_count.to_string()
            })
        });
        let k = weak_kernel.clone();
        dir.node(b"last_failed_dev", mode!(IFREG, 0o444), FsCred::root(), move || {
            create_bytes_file_with_handler(k.clone(), |kernel| {
                kernel.suspend_resume_manager.suspend_stats().last_failed_device.unwrap_or_default()
            })
        });
        let k = weak_kernel;
        dir.node(b"last_failed_errno", mode!(IFREG, 0o444), FsCred::root(), move || {
            create_bytes_file_with_handler(k.clone(), |kernel| {
                kernel
                    .suspend_resume_manager
                    .suspend_stats()
                    .last_failed_errno
                    .map(|e| format!("-{}", e.code.error_code()))
                    // This matches local linux behavior when no suspends have failed.
                    .unwrap_or_else(|| "0".to_string())
            })
        });
    }
}
