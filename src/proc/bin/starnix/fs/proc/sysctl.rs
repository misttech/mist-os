// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::fs::buffers::{InputBuffer, OutputBuffer};
use crate::fs::*;
use crate::task::*;
use crate::types::*;

pub fn sysctl_directory(fs: &FileSystemHandle) -> FsNodeHandle {
    let mode = mode!(IFREG, 0o644);
    StaticDirectoryBuilder::new(fs)
        .subdir(b"kernel", 0o555, |dir| {
            dir.entry(b"unprivileged_bpf_disable", StubSysctl::new_node(), mode)
        })
        .subdir(b"net", 0o555, |dir| {
            dir.subdir(b"core", 0o555, |dir| {
                dir.entry(b"bpf_jit_enable", StubSysctl::new_node(), mode).entry(
                    b"bpf_jit_kallsyms",
                    StubSysctl::new_node(),
                    mode,
                )
            })
        })
        .build()
}

struct StubSysctl;

impl StubSysctl {
    fn new_node() -> impl FsNodeOps {
        SimpleFileNode::new(|| Ok(Self))
    }
}

impl FileOps for StubSysctl {
    fileops_impl_seekable!();

    fn write_at(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        offset: usize,
        data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        if offset != 0 {
            return error!(EINVAL);
        }
        Ok(data.drain())
    }

    fn read_at(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _offset: usize,
        _buffer: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        Ok(0)
    }
}
