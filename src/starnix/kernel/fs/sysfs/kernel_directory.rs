// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::task::Kernel;
use crate::vfs::pseudo::simple_directory::SimpleDirectoryMutator;
use crate::vfs::pseudo::simple_file::create_bytes_file_with_handler;
use crate::vfs::pseudo::stub_empty_file::StubEmptyFile;
use starnix_logging::bug_ref;
use starnix_uapi::file_mode::mode;

/// This directory contains various files and subdirectories that provide information about
/// the running kernel.
pub fn build_kernel_directory(kernel: &Kernel, dir: &SimpleDirectoryMutator) {
    dir.subdir("tracing", 0o755, |_| ());
    dir.subdir("mm", 0o755, |dir| {
        dir.subdir("transparent_hugepage", 0o755, |dir| {
            dir.entry(
                "enabled",
                StubEmptyFile::new_node(
                    "/sys/kernel/mm/transparent_hugepage/enabled",
                    bug_ref!("https://fxbug.dev/322894184"),
                ),
                mode!(IFREG, 0o644),
            );
        });
    });
    dir.subdir("wakeup_reasons", 0o755, |dir| {
        let read_only_file_mode = mode!(IFREG, 0o444);
        dir.entry(
            "last_resume_reason",
            create_bytes_file_with_handler(kernel.weak_self.clone(), |kernel| {
                kernel.suspend_resume_manager.suspend_stats().last_resume_reason.unwrap_or_default()
            }),
            read_only_file_mode,
        );
        dir.entry(
            "last_suspend_time",
            create_bytes_file_with_handler(kernel.weak_self.clone(), |kernel| {
                let suspend_stats = kernel.suspend_resume_manager.suspend_stats();
                // First number is the time spent in suspend and resume processes.
                // Second number is the time spent in sleep state.
                format!(
                    "{} {}",
                    suspend_stats.last_time_in_suspend_operations.into_seconds_f64(),
                    suspend_stats.last_time_in_sleep.into_seconds_f64()
                )
            }),
            read_only_file_mode,
        );
    });
}
