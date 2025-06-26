// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::fs::sysfs::VulnerabilitiesClassDirectory;
use crate::task::CurrentTask;
use crate::vfs::pseudo::simple_directory::SimpleDirectoryMutator;
use crate::vfs::pseudo::simple_file::{BytesFile, BytesFileOps};
use crate::vfs::FsNodeOps;
use starnix_uapi::errors::Errno;
use starnix_uapi::file_mode::mode;

pub fn build_cpu_class_directory(dir: &SimpleDirectoryMutator) {
    let cpu_count = zx::system_get_num_cpus();

    dir.entry(
        "online",
        BytesFile::new_node(format!("0-{}\n", cpu_count - 1).into_bytes()),
        mode!(IFREG, 0o444),
    );
    dir.entry(
        "possible",
        BytesFile::new_node(format!("0-{}\n", cpu_count - 1).into_bytes()),
        mode!(IFREG, 0o444),
    );
    dir.entry("vulnerabilities", VulnerabilitiesClassDirectory::new_node(), mode!(IFDIR, 0o755));
    dir.subdir("cpufreq", 0o755, |dir| {
        dir.subdir("policy0", 0o755, |dir| {
            dir.subdir("stats", 0o755, |dir| {
                dir.entry("reset", CpuFreqStatsResetFile::new_node(), mode!(IFREG, 0o200));
            });
        });
    });
    for i in 0..cpu_count {
        let name = format!("cpu{}", i);
        dir.subdir(&name, 0o755, |_| {});
    }
}

struct CpuFreqStatsResetFile {}

impl CpuFreqStatsResetFile {
    pub fn new_node() -> impl FsNodeOps {
        BytesFile::new_node(Self {})
    }
}

impl BytesFileOps for CpuFreqStatsResetFile {
    // Currently a no-op. The value written to this node does not matter.
    fn write(&self, _current_task: &CurrentTask, _data: Vec<u8>) -> Result<(), Errno> {
        Ok(())
    }
}
