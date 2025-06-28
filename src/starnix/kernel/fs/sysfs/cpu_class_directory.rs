// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

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
    dir.subdir("vulnerabilities", 0o755, |dir| {
        for (name, contents) in VULNERABILITIES {
            let contents = contents.to_string();
            dir.entry(name, BytesFile::new_node(contents.into_bytes()), mode!(IFREG, 0o444));
        }
    });
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

const VULNERABILITIES: &[(&str, &str)] = &[
    ("gather_data_sampling", "Not affected\n"),
    ("itlb_multihit", "Not affected\n"),
    ("l1tf", "Not affected\n"),
    ("mds", "Not affected\n"),
    ("meltdown", "Not affected\n"),
    ("mmio_stale_data", "Not affected\n"),
    ("retbleed", "Not affected\n"),
    ("spec_rstack_overflow", "Not affected\n"),
    ("spec_store_bypass", "Not affected\n"),
    ("spectre_v1", "Not affected\n"),
    ("spectre_v2", "Not affected\n"),
    ("srbds", "Not affected\n"),
    ("tsx_async_abort", "Not affected\n"),
];

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
