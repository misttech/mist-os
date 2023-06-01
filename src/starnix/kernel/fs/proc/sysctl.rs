// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::auth::FsCred;
use crate::fs::*;
use crate::lock::Mutex;
use crate::task::*;
use crate::types::*;
use std::borrow::Cow;

pub fn sysctl_directory(fs: &FileSystemHandle) -> FsNodeHandle {
    let mode = mode!(IFREG, 0o644);
    let mut dir = StaticDirectoryBuilder::new(fs);
    dir.subdir(b"kernel", 0o555, |dir| {
        dir.entry(b"unprivileged_bpf_disable", StubSysctl::new_node(), mode);
        dir.entry(b"kptr_restrict", StubSysctl::new_node(), mode);
        dir.node(
            b"overflowuid",
            fs.create_node(
                BytesFile::new_node(b"65534".to_vec()),
                mode!(IFREG, 0o644),
                FsCred::root(),
            ),
        );
        dir.node(
            b"overflowgid",
            fs.create_node(
                BytesFile::new_node(b"65534".to_vec()),
                mode!(IFREG, 0o644),
                FsCred::root(),
            ),
        );
        dir.node(
            b"pid_max",
            fs.create_node(
                BytesFile::new_node(b"4194304".to_vec()),
                mode!(IFREG, 0o644),
                FsCred::root(),
            ),
        );
    });
    dir.subdir(b"net", 0o555, |dir| {
        dir.subdir(b"core", 0o555, |dir| {
            dir.entry(b"bpf_jit_enable", StubSysctl::new_node(), mode);
            dir.entry(b"bpf_jit_kallsyms", StubSysctl::new_node(), mode);
        });
    });
    dir.subdir(b"vm", 0o555, |dir| {
        dir.entry(b"mmap_rnd_bits", StubSysctl::new_node(), mode);
        dir.entry(b"mmap_rnd_compat_bits", StubSysctl::new_node(), mode);
    });
    dir.build()
}

struct StubSysctl {
    data: Mutex<Vec<u8>>,
}

impl StubSysctl {
    fn new_node() -> impl FsNodeOps {
        BytesFile::new_node(Self { data: Mutex::default() })
    }
}

impl BytesFileOps for StubSysctl {
    fn write(&self, _current_task: &CurrentTask, data: Vec<u8>) -> Result<(), Errno> {
        *self.data.lock() = data;
        Ok(())
    }
    fn read(&self, _current_task: &CurrentTask) -> Result<Cow<'_, [u8]>, Errno> {
        Ok(self.data.lock().clone().into())
    }
}

pub fn net_directory(fs: &FileSystemHandle) -> FsNodeHandle {
    let mut dir = StaticDirectoryBuilder::new(fs);
    dir.subdir(b"xt_quota", 0o555, |dir| {
        dir.entry(b"globalAlert", StubSysctl::new_node(), mode!(IFREG, 0o444));
    });
    dir.build()
}
