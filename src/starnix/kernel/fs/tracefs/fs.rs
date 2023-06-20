// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::tracing_directory::*;
use crate::fs::*;
use crate::task::*;
use crate::types::*;

use std::sync::Arc;

pub fn trace_fs(kernel: Arc<Kernel>, options: FileSystemOptions) -> FileSystemHandle {
    kernel.trace_fs.get_or_init(|| TraceFs::new_fs(&kernel, options)).clone()
}

pub struct TraceFs;

impl FileSystemOps for Arc<TraceFs> {
    fn statfs(&self, _fs: &FileSystem, _current_task: &CurrentTask) -> Result<statfs, Errno> {
        Ok(statfs::default(TRACEFS_MAGIC))
    }

    fn name(&self) -> &'static FsStr {
        b"tracefs"
    }
}

impl TraceFs {
    pub fn new_fs(kernel: &Arc<Kernel>, options: FileSystemOptions) -> FileSystemHandle {
        let fs = FileSystem::new(kernel, CacheMode::Uncached, Arc::new(TraceFs), options);
        fs.set_root(TracingDirectory::new(&fs));
        fs
    }
}
