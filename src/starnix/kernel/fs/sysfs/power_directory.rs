// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    power::{PowerStateFile, PowerSyncOnSuspendFile, PowerWakeupCountFile},
    task::CurrentTask,
    vfs::{create_bytes_file_with_handler, StaticDirectoryBuilder, StubEmptyFile},
};
use starnix_logging::bug_ref;
use starnix_uapi::file_mode::mode;
use std::sync::Arc;

pub fn sysfs_power_directory(current_task: &CurrentTask, dir: &mut StaticDirectoryBuilder<'_>) {
    let kernel = current_task.kernel();
    dir.subdir(current_task, "power", 0o755, |dir| {
        dir.entry(
            current_task,
            "wakeup_count",
            PowerWakeupCountFile::new_node(),
            mode!(IFREG, 0o644),
        );
        dir.entry(
            current_task,
            "wake_lock",
            StubEmptyFile::new_node(
                "/sys/power/wake_lock",
                bug_ref!("https://fxbug.dev/322893982"),
            ),
            mode!(IFREG, 0o644),
        );
        dir.entry(
            current_task,
            "wake_unlock",
            StubEmptyFile::new_node(
                "/sys/power/wake_unlock",
                bug_ref!("https://fxbug.dev/322894043"),
            ),
            mode!(IFREG, 0o644),
        );
        dir.entry(current_task, "state", PowerStateFile::new_node(), mode!(IFREG, 0o644));
        dir.entry(
            current_task,
            "sync_on_suspend",
            PowerSyncOnSuspendFile::new_node(),
            mode!(IFREG, 0o644),
        );
        dir.subdir(current_task, "suspend_stats", 0o755, |dir| {
            let read_only_file_mode = mode!(IFREG, 0o444);
            dir.entry(
                current_task,
                "success",
                create_bytes_file_with_handler(Arc::downgrade(kernel), |kernel| {
                    kernel.suspend_resume_manager.suspend_stats().success_count.to_string()
                }),
                read_only_file_mode,
            );
            dir.entry(
                current_task,
                "fail",
                create_bytes_file_with_handler(Arc::downgrade(kernel), |kernel| {
                    kernel.suspend_resume_manager.suspend_stats().fail_count.to_string()
                }),
                read_only_file_mode,
            );
            dir.entry(
                current_task,
                "last_failed_dev",
                create_bytes_file_with_handler(Arc::downgrade(kernel), |kernel| {
                    kernel
                        .suspend_resume_manager
                        .suspend_stats()
                        .last_failed_device
                        .unwrap_or_default()
                }),
                read_only_file_mode,
            );
            dir.entry(
                current_task,
                "last_failed_errno",
                create_bytes_file_with_handler(Arc::downgrade(kernel), |kernel| {
                    kernel
                        .suspend_resume_manager
                        .suspend_stats()
                        .last_failed_errno
                        .map(|e| format!("-{}", e.code.error_code().to_string()))
                        .unwrap_or_default()
                }),
                read_only_file_mode,
            );
        });
    });
}
