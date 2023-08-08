// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{
        auth::FsCred,
        fs::*,
        task::{CurrentTask, Kernel},
        types::*,
    },
    std::{
        borrow::Cow,
        sync::{Arc, Weak},
    },
};

impl<T> BytesFileOps for T
where
    T: Fn() -> Result<String, Errno> + Send + Sync + 'static,
{
    fn read(&self, _current_task: &CurrentTask) -> Result<Cow<'_, [u8]>, Errno> {
        self().map(|s| s.into_bytes().into())
    }
}

fn create_readonly_node<F>(
    fs: &Arc<FileSystem>,
    kernel: Weak<Kernel>,
    read_handler: F,
) -> Arc<FsNode>
where
    F: Fn(Arc<Kernel>) -> String + Send + Sync + 'static,
{
    fs.create_node(
        BytesFile::new_node(move || match kernel.upgrade() {
            Some(kernel) => Ok(read_handler(kernel) + "\n"),
            None => error!(ENOENT),
        }),
        FsNodeInfo::new_factory(mode!(IFREG, 0o444), FsCred::root()),
    )
}

pub fn sysfs_power_directory(
    dir: &mut StaticDirectoryBuilder<'_>,
    fs: &Arc<FileSystem>,
    kernel: Weak<Kernel>,
) {
    dir.subdir(b"power", 0o755, |dir| {
        dir.subdir(b"suspend_stats", 0o755, |dir| {
            dir.node(
                b"success",
                create_readonly_node(fs, kernel.clone(), |kernel| {
                    kernel.power_manager.suspend_stats().success_count.to_string()
                }),
            );
            dir.node(
                b"fail",
                create_readonly_node(fs, kernel.clone(), |kernel| {
                    kernel.power_manager.suspend_stats().fail_count.to_string()
                }),
            );
            dir.node(
                b"last_failed_dev",
                create_readonly_node(fs, kernel.clone(), |kernel| {
                    kernel.power_manager.suspend_stats().last_failed_device.unwrap_or_default()
                }),
            );
            dir.node(
                b"last_failed_errno",
                create_readonly_node(fs, kernel.clone(), |kernel| {
                    kernel
                        .power_manager
                        .suspend_stats()
                        .last_failed_errno
                        .map(|e| format!("-{}", e.code.error_code().to_string()))
                        .unwrap_or_default()
                }),
            );
        });
    });
}
