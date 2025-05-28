// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::fs::pseudo_directory::PseudoDirectoryBuilder;
use crate::task::Kernel;
use crate::vfs::create_bytes_file_with_handler;
use crate::vfs::stub_empty_file::StubEmptyFile;
use starnix_logging::bug_ref;
use starnix_uapi::auth::FsCred;
use starnix_uapi::file_mode::mode;
use std::sync::Arc;

/// This directory contains various files and subdirectories that provide information about
/// the running kernel.
pub fn sysfs_kernel_directory(kernel: &Arc<Kernel>, dir: &mut PseudoDirectoryBuilder) {
    let weak_kernel = Arc::downgrade(kernel);
    dir.subdir(b"tracing");
    {
        let dir = dir.subdir(b"mm");
        {
            let dir = dir.subdir(b"transparent_hugepage");
            dir.node(b"enabled", mode!(IFREG, 0o644), FsCred::root(), || {
                StubEmptyFile::new_node(
                    "/sys/kernel/mm/transparent_hugepage/enabled",
                    bug_ref!("https://fxbug.dev/322894184"),
                )
            });
        }
    }
    {
        let dir = dir.subdir(b"wakeup_reasons");
        let k = weak_kernel.clone();
        dir.node(b"last_resume_reason", mode!(IFREG, 0o444), FsCred::root(), move || {
            create_bytes_file_with_handler(k.clone(), |kernel| {
                kernel.suspend_resume_manager.suspend_stats().last_resume_reason.unwrap_or_default()
            })
        });
        let k = weak_kernel;
        dir.node(b"last_suspend_time", mode!(IFREG, 0o444), FsCred::root(), move || {
            create_bytes_file_with_handler(k.clone(), |kernel| {
                let suspend_stats = kernel.suspend_resume_manager.suspend_stats();
                // First number is the time spent in suspend and resume processes.
                // Second number is the time spent in sleep state.
                format!(
                    "{} {}",
                    suspend_stats.last_time_in_suspend_operations.into_seconds_f64(),
                    suspend_stats.last_time_in_sleep.into_seconds_f64()
                )
            })
        });
    }
}
