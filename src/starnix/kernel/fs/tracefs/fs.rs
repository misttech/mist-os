// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::tracing_directory::TraceMarkerFile;
use crate::{
    fs::{
        CacheMode, ConstFile, FileSystem, FileSystemHandle, FileSystemOps, FileSystemOptions,
        FsNodeInfo, FsStr, StaticDirectoryBuilder,
    },
    task::CurrentTask,
};
use starnix_uapi::{auth::FsCred, errors::Errno, file_mode::mode, statfs, TRACEFS_MAGIC};
use std::sync::Arc;

pub fn trace_fs(current_task: &CurrentTask, options: FileSystemOptions) -> &FileSystemHandle {
    current_task.kernel().trace_fs.get_or_init(|| TraceFs::new_fs(current_task, options))
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
    pub fn new_fs(current_task: &CurrentTask, options: FileSystemOptions) -> FileSystemHandle {
        let kernel = current_task.kernel();
        let fs = FileSystem::new(kernel, CacheMode::Uncached, Arc::new(TraceFs), options);
        let mut dir = StaticDirectoryBuilder::new(&fs);

        dir.node(
            b"trace",
            fs.create_node(
                current_task,
                ConstFile::new_node(vec![]),
                FsNodeInfo::new_factory(mode!(IFREG, 0o755), FsCred::root()),
            ),
        );
        // The remaining contents of the fs are a minimal set of files that we want to exist so
        // that Perfetto's ftrace controller will not error out. None of them provide any real
        // functionality.
        dir.subdir(current_task, b"per_cpu", 0o755, |dir| {
            for cpu in 0..fuchsia_zircon::system_get_num_cpus() {
                let dir_name = format!("cpu{}", cpu);
                dir.subdir(
                    current_task,
                    Box::leak(dir_name.into_boxed_str()).as_bytes(),
                    0o755,
                    |dir| {
                        dir.node(
                            b"trace_pipe_raw",
                            fs.create_node(
                                current_task,
                                ConstFile::new_node(vec![]),
                                FsNodeInfo::new_factory(mode!(IFREG, 0o755), FsCred::root()),
                            ),
                        );
                    },
                );
            }
        });
        dir.node(
            b"tracing_on",
            fs.create_node(
                current_task,
                ConstFile::new_node(b"0".to_vec()),
                FsNodeInfo::new_factory(mode!(IFREG, 0o755), FsCred::root()),
            ),
        );
        dir.node(
            b"trace_marker",
            fs.create_node(
                current_task,
                TraceMarkerFile::new_node(),
                FsNodeInfo::new_factory(mode!(IFREG, 0o755), FsCred::root()),
            ),
        );
        dir.build_root();

        fs
    }
}
