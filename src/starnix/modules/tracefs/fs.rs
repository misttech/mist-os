// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::event::TraceEventQueue;
use super::tracing_directory::TraceMarkerFile;
use starnix_core::task::CurrentTask;
use starnix_core::vfs::{
    BytesFile, BytesFileOps, CacheMode, ConstFile, FileObject, FileOps, FileSystem,
    FileSystemHandle, FileSystemOps, FileSystemOptions, FsNodeInfo, FsNodeOps, FsStr, OutputBuffer,
    SimpleFileNode, StaticDirectoryBuilder,
};
use starnix_core::{fileops_impl_nonseekable, fileops_impl_noop_sync};
use starnix_logging::track_stub;
use starnix_sync::{FileOpsCore, Locked, Mutex, Unlocked};
use starnix_types::vfs::default_statfs;
use starnix_types::PAGE_SIZE;
use starnix_uapi::auth::FsCred;
use starnix_uapi::errors::Errno;
use starnix_uapi::file_mode::mode;
use starnix_uapi::{errno, error, statfs, TRACEFS_MAGIC};
use std::borrow::Cow;
use std::sync::{Arc, LazyLock};

pub fn trace_fs(
    _locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    options: FileSystemOptions,
) -> Result<FileSystemHandle, Errno> {
    struct TraceFsHandle(FileSystemHandle);

    let handle = current_task.kernel().expando.get_or_init(|| {
        TraceFsHandle(
            TraceFs::new_fs(current_task, options).expect("tracefs constructed with valid options"),
        )
    });
    Ok(handle.0.clone())
}

pub struct TraceFs;

impl FileSystemOps for TraceFs {
    fn statfs(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _fs: &FileSystem,
        _current_task: &CurrentTask,
    ) -> Result<statfs, Errno> {
        Ok(default_statfs(TRACEFS_MAGIC))
    }

    fn name(&self) -> &'static FsStr {
        "tracefs".into()
    }
}

impl TraceFs {
    pub fn new_fs(
        current_task: &CurrentTask,
        options: FileSystemOptions,
    ) -> Result<FileSystemHandle, Errno> {
        let kernel = current_task.kernel();
        let trace_event_queue = Arc::new(TraceEventQueue::new(&kernel.inspect_node)?);
        let fs = FileSystem::new(&kernel, CacheMode::Uncached, TraceFs, options)?;
        let mut dir = StaticDirectoryBuilder::new(&fs);

        dir.node(
            "trace",
            fs.create_node(
                current_task,
                TraceFile::new_node(),
                FsNodeInfo::new_factory(mode!(IFREG, 0o755), FsCred::root()),
            ),
        );
        dir.subdir(current_task, "per_cpu", 0o755, |dir| {
            /// A name for each cpu directory, cached to provide a 'static lifetime.
            static CPU_DIR_NAMES: LazyLock<Vec<String>> = LazyLock::new(|| {
                (0..zx::system_get_num_cpus()).map(|cpu| format!("cpu{}", cpu)).collect()
            });
            for dir_name in CPU_DIR_NAMES.iter().map(|s| s.as_str()) {
                dir.subdir(current_task, dir_name, 0o755, |dir| {
                    // We're not able to detect which cpu events are coming from, so we push them
                    // all into the first cpu.
                    let ops: Box<dyn FsNodeOps> = if dir_name == "cpu0" {
                        Box::new(TraceRawFile::new_node(trace_event_queue.clone()))
                    } else {
                        track_stub!(
                            TODO("https://fxbug.dev/357665908"),
                            "/sys/kernel/tracing/per_cpu/cpuX/trace_pipe_raw"
                        );
                        Box::new(EmptyFile::new_node())
                    };
                    dir.node(
                        "trace_pipe_raw",
                        fs.create_node(
                            current_task,
                            ops,
                            FsNodeInfo::new_factory(mode!(IFREG, 0o444), FsCred::root()),
                        ),
                    );
                    dir.entry(current_task, "trace", TraceFile::new_node(), mode!(IFREG, 0o444));
                });
            }
        });
        dir.subdir(current_task, "events", 0o755, |dir| {
            dir.entry(
                current_task,
                "header_page",
                EventsHeaderPage::new_node(),
                mode!(IFREG, 0o444),
            );
            dir.subdir(current_task, "ftrace", 0o755, |dir| {
                dir.subdir(current_task, "print", 0o755, |dir| {
                    dir.entry(
                        current_task,
                        "format",
                        FtracePrintFormatFile::new_node(),
                        mode!(IFREG, 0o444),
                    );
                });
            });
            dir.node(
                "enable",
                fs.create_node(
                    current_task,
                    TraceBytesFile::new_node(),
                    FsNodeInfo::new_factory(mode!(IFREG, 0o755), FsCred::root()),
                ),
            );
        });
        dir.subdir(current_task, "options", 0o755, |dir| {
            dir.entry(current_task, "overwrite", TraceBytesFile::new_node(), mode!(IFREG, 0o444));
        });
        dir.node(
            "tracing_on",
            fs.create_node(
                current_task,
                TracingOnFile::new_node(trace_event_queue.clone()),
                FsNodeInfo::new_factory(mode!(IFREG, 0o755), FsCred::root()),
            ),
        );
        dir.node(
            "current_tracer",
            fs.create_node(
                current_task,
                ConstFile::new_node("nop".into()),
                FsNodeInfo::new_factory(mode!(IFREG, 0o755), FsCred::root()),
            ),
        );
        dir.node(
            "trace_marker",
            fs.create_node(
                current_task,
                TraceMarkerFile::new_node(trace_event_queue.clone()),
                FsNodeInfo::new_factory(mode!(IFREG, 0o755), FsCred::root()),
            ),
        );
        dir.node(
            "printk_formats",
            fs.create_node(
                current_task,
                TraceBytesFile::new_node(),
                FsNodeInfo::new_factory(mode!(IFREG, 0o755), FsCred::root()),
            ),
        );
        dir.node(
            "trace_clock",
            fs.create_node(
                current_task,
                ConstFile::new_node(
                    "[local] global counter uptime perf mono mono_raw boot tai x86-tsc ".into(),
                ),
                FsNodeInfo::new_factory(mode!(IFREG, 0o755), FsCred::root()),
            ),
        );
        dir.node(
            "buffer_size_kb",
            fs.create_node(
                current_task,
                ConstFile::new_node("7".into()),
                FsNodeInfo::new_factory(mode!(IFREG, 0o755), FsCred::root()),
            ),
        );
        dir.build_root();

        Ok(fs)
    }
}

#[derive(Default)]
struct TraceBytesFile {
    data: Mutex<Vec<u8>>,
}

impl TraceBytesFile {
    #[track_caller]
    fn new_node() -> impl FsNodeOps {
        SimpleFileNode::new(move || Ok(BytesFile::new(Self::default())))
    }
}

impl BytesFileOps for TraceBytesFile {
    fn write(&self, _current_task: &CurrentTask, data: Vec<u8>) -> Result<(), Errno> {
        *self.data.lock() = data;
        Ok(())
    }
    fn read(&self, _current_task: &CurrentTask) -> Result<Cow<'_, [u8]>, Errno> {
        let data = self.data.lock().clone();
        Ok(data.into())
    }
}

#[derive(Clone)]
struct TraceRawFile {
    queue: Arc<TraceEventQueue>,
}

impl TraceRawFile {
    pub fn new_node(queue: Arc<TraceEventQueue>) -> impl FsNodeOps {
        SimpleFileNode::new(move || Ok(Self { queue: queue.clone() }))
    }
}

impl FileOps for TraceRawFile {
    fileops_impl_nonseekable!();
    fileops_impl_noop_sync!();

    fn read(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _offset: usize,
        data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        assert!(data.available() as u64 == *PAGE_SIZE);
        self.queue.read(data)
    }

    fn write(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _offset: usize,
        _data: &mut dyn starnix_core::vfs::InputBuffer,
    ) -> Result<usize, Errno> {
        error!(ENOSYS)
    }
}

#[derive(Default)]
struct EmptyFile {}

impl EmptyFile {
    #[track_caller]
    fn new_node() -> impl FsNodeOps {
        SimpleFileNode::new(move || Ok(BytesFile::new(Self::default())))
    }
}

impl BytesFileOps for EmptyFile {
    fn write(&self, _current_task: &CurrentTask, _data: Vec<u8>) -> Result<(), Errno> {
        error!(ENOTSUP)
    }
    fn read(&self, _current_task: &CurrentTask) -> Result<Cow<'_, [u8]>, Errno> {
        error!(EAGAIN)
    }
}

#[derive(Clone)]
struct TracingOnFile {
    queue: Arc<TraceEventQueue>,
}

impl TracingOnFile {
    pub fn new_node(queue: Arc<TraceEventQueue>) -> impl FsNodeOps {
        BytesFile::new_node(Self { queue })
    }
}

impl BytesFileOps for TracingOnFile {
    fn write(&self, _current_task: &CurrentTask, data: Vec<u8>) -> Result<(), Errno> {
        let state_str = std::str::from_utf8(&data).map_err(|_| errno!(EINVAL))?;
        let clean_state_str = state_str.split('\n').next().unwrap_or("");
        match clean_state_str {
            "0" => self.queue.disable(),
            "1" => self.queue.enable(),
            _ => error!(EINVAL),
        }
    }

    fn read(&self, _current_task: &CurrentTask) -> Result<Cow<'_, [u8]>, Errno> {
        if self.queue.is_enabled() {
            Ok(Cow::Borrowed(&b"1\n"[..]))
        } else {
            Ok(Cow::Borrowed(&b"0\n"[..]))
        }
    }
}

#[derive(Clone)]
struct EventsHeaderPage;
impl EventsHeaderPage {
    fn new_node() -> impl FsNodeOps {
        ConstFile::new_node(
            "\tfield: u64 timestamp;\toffset:0;\tsize:8;\tsigned:0\n\
        \tfield: local_t commit;\toffset:8;\tsize:8;\tsigned:1\n\
        \tfield: int overwrite;\toffset:8;\tsize:1;\tsigned:1\n\
        \tfield: char data;\toffset:16;\tsize:4080;\tsigned:0\n"
                .into(),
        )
    }
}

#[derive(Clone)]
struct FtracePrintFormatFile;
impl FtracePrintFormatFile {
    fn new_node() -> impl FsNodeOps {
        ConstFile::new_node(
            "name: print\n\
        ID: 5\n\
        format:\n\
        \tfield:unsigned short common_type;\toffset:0;\tsize:2;\tsigned:0;\n\
        \tfield:unsigned char common_flags;\toffset:2;\tsize:1;\tsigned:0;\n\
        \tfield:unsigned char common_preempt_count;\toffset:3;\tsize:1;\tsigned:0;\n\
        \tfield:int common_pid;\toffset:4;\tsize:4;\tsigned:1;\n\
        \n\
        \tfield:unsigned long ip;\toffset:8;\tsize:8;\tsigned:0;\n\
        \tfield:char buf[];\toffset:16;\tsize:0;\tsigned:0;\n\
        \n\
        print fmt: \"%ps: %s\", (void *)REC->ip, REC- >buf\n"
                .into(),
        )
    }
}

#[derive(Clone)]
struct TraceFile;
impl TraceFile {
    fn new_node() -> impl FsNodeOps {
        track_stub!(TODO("https://fxbug.dev/357665908"), "/sys/kernel/tracing/trace");
        ConstFile::new_node("# tracer: nop\n".into())
    }
}
