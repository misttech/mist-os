// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use once_cell::sync::Lazy;
use regex::Regex;
use std::sync::Arc;

use crate::fs::buffers::{InputBuffer, OutputBuffer};
use crate::fs::*;
use crate::mm::{
    MemoryAccessor, ProcMapsFile, ProcSmapsFile, ProcStatFile, ProcStatmFile, ProcStatusFile,
};
use crate::selinux::selinux_proc_attrs;
use crate::task::{CurrentTask, Task, TaskStateCode, ThreadGroup};
use crate::types::*;

/// Creates an [`FsNode`] that represents the `/proc/<pid>` directory for `task`.
pub fn pid_directory(fs: &FileSystemHandle, task: &Arc<Task>) -> Arc<FsNode> {
    let mut dir = static_directory_builder_with_common_task_entries(fs, task);
    dir.entry_creds(task.as_fscred());
    dir.entry(
        b"task",
        TaskListDirectory { thread_group: task.thread_group.clone() },
        mode!(IFDIR, 0o777),
    );
    dir.build()
}

/// Creates an [`FsNode`] that represents the `/proc/<pid>/task/<tid>` directory for `task`.
fn tid_directory(fs: &FileSystemHandle, task: &Arc<Task>) -> Arc<FsNode> {
    static_directory_builder_with_common_task_entries(fs, task).build()
}

/// Creates a [`StaticDirectoryBuilder`] and pre-populates it with files that are present in both
/// `/pid/<pid>` and `/pid/<pid>/task/<tid>`.
fn static_directory_builder_with_common_task_entries<'a>(
    fs: &'a FileSystemHandle,
    task: &Arc<Task>,
) -> StaticDirectoryBuilder<'a> {
    let mut dir = StaticDirectoryBuilder::new(fs);
    dir.entry_creds(task.as_fscred());
    let local_task = task.clone();
    dir.entry(
        b"cwd",
        CallbackSymlinkNode::new(move || Ok(SymlinkTarget::Node(local_task.fs().cwd()))),
        mode!(IFLNK, 0o777),
    );
    let local_task = task.clone();
    dir.entry(
        b"exe",
        CallbackSymlinkNode::new(move || {
            if let Some(node) = local_task.mm.executable_node() {
                Ok(SymlinkTarget::Node(node))
            } else {
                error!(ENOENT)
            }
        }),
        mode!(IFLNK, 0o777),
    );
    dir.entry(b"fd", FdDirectory::new(task), mode!(IFDIR, 0o777));
    dir.entry(b"fdinfo", FdInfoDirectory::new(task), mode!(IFDIR, 0o777));
    dir.entry(b"io", IoFile::new_node(task), mode!(IFREG, 0o444));
    dir.entry(b"limits", LimitsFile::new_node(task), mode!(IFREG, 0o444));
    dir.entry(b"maps", ProcMapsFile::new_node(task), mode!(IFREG, 0o444));
    dir.entry(b"mem", MemFile::new_node(task), mode!(IFREG, 0o600));
    let local_task = task.clone();
    dir.entry(
        b"root",
        CallbackSymlinkNode::new(move || Ok(SymlinkTarget::Node(local_task.fs().root()))),
        mode!(IFLNK, 0o777),
    );
    dir.entry(b"smaps", ProcSmapsFile::new_node(task), mode!(IFREG, 0o444));
    dir.entry(b"stat", ProcStatFile::new_node(task), mode!(IFREG, 0o444));
    dir.entry(b"statm", ProcStatmFile::new_node(task), mode!(IFREG, 0o444));
    dir.entry(b"status", ProcStatusFile::new_node(task), mode!(IFREG, 0o444));
    dir.entry(b"cmdline", CmdlineFile::new_node(task), mode!(IFREG, 0o444));
    dir.entry(b"environ", EnvironFile::new_node(task), mode!(IFREG, 0o444));
    dir.entry(b"auxv", AuxvFile::new_node(task), mode!(IFREG, 0o444));
    dir.entry(b"comm", CommFile::new_node(task), mode!(IFREG, 0o444));
    dir.subdir(b"attr", 0o555, |dir| {
        dir.entry_creds(task.as_fscred());
        dir.dir_creds(task.as_fscred());
        selinux_proc_attrs(task, dir);
    });
    dir.entry(b"ns", NsDirectory { task: task.clone() }, mode!(IFDIR, 0o777));
    dir.entry(b"mountinfo", ProcMountinfoFile::new_node(task), mode!(IFREG, 0o444));
    dir.entry(b"mounts", ProcMountsFile::new_node(task), mode!(IFREG, 0o444));
    dir.dir_creds(task.as_fscred());
    dir
}

/// `FdDirectory` implements the directory listing operations for a `proc/<pid>/fd` directory.
///
/// Reading the directory returns a list of all the currently open file descriptors for the
/// associated task.
struct FdDirectory {
    task: Arc<Task>,
}

impl FdDirectory {
    fn new(task: &Arc<Task>) -> Self {
        Self { task: Arc::clone(task) }
    }
}

impl FsNodeOps for FdDirectory {
    fs_node_impl_dir_readonly!();

    fn create_file_ops(
        &self,
        _node: &FsNode,
        _flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        Ok(VecDirectory::new_file(fds_to_directory_entries(self.task.files.get_all_fds())))
    }

    fn lookup(
        &self,
        node: &FsNode,
        _current_task: &CurrentTask,
        name: &FsStr,
    ) -> Result<Arc<FsNode>, Errno> {
        let fd = FdNumber::from_fs_str(name).map_err(|_| errno!(ENOENT))?;
        // Make sure that the file descriptor exists before creating the node.
        let _ = self.task.files.get(fd).map_err(|_| errno!(ENOENT))?;
        let local_task = self.task.clone();
        Ok(node.fs().create_node(
            CallbackSymlinkNode::new(move || {
                let file = local_task.files.get(fd).map_err(|_| errno!(ENOENT))?;
                Ok(SymlinkTarget::Node(file.name.clone()))
            }),
            mode!(IFLNK, 0o777),
            self.task.as_fscred(),
        ))
    }
}

const NS_ENTRIES: &[&str] = &[
    "cgroup",
    "ipc",
    "mnt",
    "net",
    "pid",
    "pid_for_children",
    "time",
    "time_for_children",
    "user",
    "uts",
];

/// /proc/[pid]/ns directory
struct NsDirectory {
    task: Arc<Task>,
}

impl FsNodeOps for NsDirectory {
    fs_node_impl_dir_readonly!();

    fn create_file_ops(
        &self,
        _node: &FsNode,
        _flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        // For each namespace, this contains a link to the current identifier of the given namespace
        // for the current task.
        Ok(VecDirectory::new_file(
            NS_ENTRIES
                .iter()
                .map(|name| VecDirectoryEntry {
                    entry_type: DirectoryEntryType::LNK,
                    name: name.as_bytes().to_vec(),
                    inode: None,
                })
                .collect(),
        ))
    }

    fn lookup(
        &self,
        node: &FsNode,
        _current_task: &CurrentTask,
        name: &FsStr,
    ) -> Result<Arc<FsNode>, Errno> {
        // If name is a given namespace, link to the current identifier of the that namespace for
        // the current task.
        // If name is {namespace}:[id], get a file descriptor for the given namespace.

        let name = String::from_utf8(name.to_vec()).map_err(|_| errno!(ENOENT))?;
        let mut elements = name.split(':');
        let ns = elements.next().expect("name must not be empty");
        // The name doesn't starts with a known namespace.
        if !NS_ENTRIES.contains(&ns) {
            return error!(ENOENT);
        }
        if let Some(id) = elements.next() {
            // The name starts with {namespace}:, check that it matches {namespace}:[id]
            static NS_IDENTIFIER_RE: Lazy<Regex> =
                Lazy::new(|| Regex::new("^\\[[0-9]+\\]$").unwrap());
            if NS_IDENTIFIER_RE.is_match(id) {
                // TODO(qsr): For now, returns an empty file. In the future, this should create a
                // reference to to correct namespace, and ensures it keeps it alive.
                Ok(node.fs().create_node(
                    BytesFile::new_node(vec![]),
                    mode!(IFREG, 0o444),
                    self.task.as_fscred(),
                ))
            } else {
                error!(ENOENT)
            }
        } else {
            // The name is {namespace}, link to the correct one of the current task.
            Ok(node.fs().create_node(
                CallbackSymlinkNode::new(move || {
                    // For now, all namespace have the identifier 1.
                    Ok(SymlinkTarget::Path(format!("{}:[1]", name).as_bytes().to_vec()))
                }),
                mode!(IFLNK, 0o7777),
                self.task.as_fscred(),
            ))
        }
    }
}

/// `FdInfoDirectory` implements the directory listing operations for a `proc/<pid>/fdinfo`
/// directory.
///
/// Reading the directory returns a list of all the currently open file descriptors for the
/// associated task.
struct FdInfoDirectory {
    task: Arc<Task>,
}

impl FdInfoDirectory {
    fn new(task: &Arc<Task>) -> Self {
        Self { task: Arc::clone(task) }
    }
}

impl FsNodeOps for FdInfoDirectory {
    fn create_file_ops(
        &self,
        _node: &FsNode,
        _flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        Ok(VecDirectory::new_file(fds_to_directory_entries(self.task.files.get_all_fds())))
    }

    fn lookup(
        &self,
        node: &FsNode,
        _current_task: &CurrentTask,
        name: &FsStr,
    ) -> Result<FsNodeHandle, Errno> {
        let fd = FdNumber::from_fs_str(name).map_err(|_| errno!(ENOENT))?;
        let file = self.task.files.get(fd).map_err(|_| errno!(ENOENT))?;
        let pos = *file.offset.lock();
        let flags = file.flags();
        let data = format!("pos:\t{}flags:\t0{:o}\n", pos, flags.bits()).into_bytes();
        Ok(node.fs().create_node(
            BytesFile::new_node(data),
            mode!(IFREG, 0o444),
            self.task.as_fscred(),
        ))
    }
}

fn fds_to_directory_entries(fds: Vec<FdNumber>) -> Vec<VecDirectoryEntry> {
    fds.into_iter()
        .map(|fd| VecDirectoryEntry {
            entry_type: DirectoryEntryType::DIR,
            name: fd.raw().to_string().into_bytes(),
            inode: None,
        })
        .collect()
}

/// Directory that lists the task IDs (tid) in a process. Located at `/proc/<pid>/task/`.
struct TaskListDirectory {
    thread_group: Arc<ThreadGroup>,
}

impl FsNodeOps for TaskListDirectory {
    fn create_file_ops(
        &self,
        _node: &FsNode,
        _flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        Ok(VecDirectory::new_file(
            self.thread_group
                .read()
                .tasks
                .keys()
                .map(|tid| VecDirectoryEntry {
                    entry_type: DirectoryEntryType::DIR,
                    name: tid.to_string().into_bytes(),
                    inode: None,
                })
                .collect(),
        ))
    }

    fn lookup(
        &self,
        node: &FsNode,
        _current_task: &CurrentTask,
        name: &FsStr,
    ) -> Result<Arc<FsNode>, Errno> {
        let tid = std::str::from_utf8(name)
            .map_err(|_| errno!(ENOENT))?
            .parse::<pid_t>()
            .map_err(|_| errno!(ENOENT))?;
        // Make sure the tid belongs to this process.
        if !self.thread_group.read().tasks.contains_key(&tid) {
            return error!(ENOENT);
        }
        let task =
            self.thread_group.kernel.pids.read().get_task(tid).ok_or_else(|| errno!(ENOENT))?;
        Ok(tid_directory(&node.fs(), &task))
    }
}

fn fill_buf_from_addr_range(
    task: &Arc<Task>,
    range_start: UserAddress,
    range_end: UserAddress,
    sink: &mut DynamicFileBuf,
) -> Result<(), Errno> {
    #[allow(clippy::manual_saturating_arithmetic)]
    let len = range_end.ptr().checked_sub(range_start.ptr()).unwrap_or(0);
    let mut buf = vec![0u8; len];
    let len = task.mm.read_memory_partial(range_start, &mut buf)?;
    sink.write(&buf[..len]);
    Ok(())
}

/// `CmdlineFile` implements `proc/<pid>/cmdline` file.
#[derive(Clone)]
pub struct CmdlineFile(Arc<Task>);
impl CmdlineFile {
    pub fn new_node(task: &Arc<Task>) -> impl FsNodeOps {
        DynamicFile::new_node(Self(task.clone()))
    }
}
impl DynamicFileSource for CmdlineFile {
    fn generate(&self, sink: &mut DynamicFileBuf) -> Result<(), Errno> {
        let (start, end) = {
            let mm_state = self.0.mm.state.read();
            (mm_state.argv_start, mm_state.argv_end)
        };
        fill_buf_from_addr_range(&self.0, start, end, sink)
    }
}

/// `EnvironFile` implements `proc/<pid>/environ` file.
#[derive(Clone)]
pub struct EnvironFile(Arc<Task>);
impl EnvironFile {
    pub fn new_node(task: &Arc<Task>) -> impl FsNodeOps {
        DynamicFile::new_node(Self(task.clone()))
    }
}
impl DynamicFileSource for EnvironFile {
    fn generate(&self, sink: &mut DynamicFileBuf) -> Result<(), Errno> {
        let (start, end) = {
            let mm_state = self.0.mm.state.read();
            (mm_state.environ_start, mm_state.environ_end)
        };
        fill_buf_from_addr_range(&self.0, start, end, sink)
    }
}

/// `AuxvFile` implements `proc/<pid>/auxv` file.
#[derive(Clone)]
pub struct AuxvFile(Arc<Task>);
impl AuxvFile {
    pub fn new_node(task: &Arc<Task>) -> impl FsNodeOps {
        DynamicFile::new_node(Self(task.clone()))
    }
}
impl DynamicFileSource for AuxvFile {
    fn generate(&self, sink: &mut DynamicFileBuf) -> Result<(), Errno> {
        let (start, end) = {
            let mm_state = self.0.mm.state.read();
            (mm_state.auxv_start, mm_state.auxv_end)
        };
        fill_buf_from_addr_range(&self.0, start, end, sink)
    }
}

/// `CommFile` implements `proc/<pid>/comm` file.
#[derive(Clone)]
pub struct CommFile(Arc<Task>);
impl CommFile {
    pub fn new_node(task: &Arc<Task>) -> impl FsNodeOps {
        DynamicFile::new_node(Self(task.clone()))
    }
}
impl DynamicFileSource for CommFile {
    fn generate(&self, sink: &mut DynamicFileBuf) -> Result<(), Errno> {
        sink.write(self.0.command().as_bytes());
        sink.write(b"\n");
        Ok(())
    }
}

/// `IoFile` implements `proc/<pid>/io` file.
#[derive(Clone)]
pub struct IoFile(Arc<Task>);
impl IoFile {
    pub fn new_node(task: &Arc<Task>) -> impl FsNodeOps {
        DynamicFile::new_node(Self(task.clone()))
    }
}
impl DynamicFileSource for IoFile {
    fn generate(&self, sink: &mut DynamicFileBuf) -> Result<(), Errno> {
        // TODO: Keep track of these stats and report them in this file.
        sink.write(b"rchar: 0\n");
        sink.write(b"wchar: 0\n");
        sink.write(b"syscr: 0\n");
        sink.write(b"syscw: 0\n");
        sink.write(b"read_bytes: 0\n");
        sink.write(b"write_bytes: 0\n");
        sink.write(b"cancelled_write_bytes: 0\n");
        Ok(())
    }
}

/// `LimitsFile` implements `proc/<pid>/limits` file.
#[derive(Clone)]
pub struct LimitsFile(Arc<Task>);
impl LimitsFile {
    pub fn new_node(task: &Arc<Task>) -> impl FsNodeOps {
        DynamicFile::new_node(Self(task.clone()))
    }
}
impl DynamicFileSource for LimitsFile {
    fn generate(&self, sink: &mut DynamicFileBuf) -> Result<(), Errno> {
        let state = self.0.thread_group.read();
        let limits = &state.limits;

        let write_limit = |sink: &mut DynamicFileBuf, value| {
            if value == RLIM_INFINITY as u64 {
                sink.write(format!("{:<20}", "unlimited").as_bytes());
            } else {
                sink.write(format!("{:<20}", value).as_bytes());
            }
        };
        sink.write(
            format!("{:<25}{:<20}{:<20}{:<10}\n", "Limit", "Soft Limit", "Hard Limit", "Units")
                .as_bytes(),
        );
        for resource in Resource::ALL {
            let desc = resource.desc();
            let limit = limits.get(resource);
            sink.write(format!("{:<25}", desc.name).as_bytes());
            write_limit(sink, limit.rlim_cur);
            write_limit(sink, limit.rlim_max);
            if !desc.unit.is_empty() {
                sink.write(format!("{:<10}", desc.unit).as_bytes());
            }
            sink.write(b"\n");
        }
        Ok(())
    }
}

/// `MemFile` implements `proc/<pid>/mem` file.
#[derive(Clone)]
pub struct MemFile(Arc<Task>);
impl MemFile {
    pub fn new_node(task: &Arc<Task>) -> impl FsNodeOps {
        let task = task.clone();
        SimpleFileNode::new(move || Ok(Self(task.clone())))
    }
}

impl FileOps for MemFile {
    fileops_impl_seekable_read!();
    fileops_impl_seekable_write!();

    fn seek(
        &self,
        file: &FileObject,
        current_task: &CurrentTask,
        offset: off_t,
        whence: SeekOrigin,
    ) -> Result<off_t, Errno> {
        match whence {
            SeekOrigin::End => error!(EINVAL),
            _ => default_seek(file, current_task, offset, whence),
        }
    }

    fn read_at(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        offset: usize,
        data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        let task = &self.0;
        match task.state_code() {
            TaskStateCode::Zombie => Ok(0),
            TaskStateCode::Running | TaskStateCode::Sleeping => {
                let mut addr = UserAddress::default() + offset;
                data.write_each(&mut |bytes| {
                    let actual =
                        task.mm.read_memory_partial(addr, bytes).map_err(|_| errno!(EIO))?;
                    addr += actual;
                    Ok(actual)
                })
            }
        }
    }

    fn write_at(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        offset: usize,
        data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        let task = &self.0;
        match task.state_code() {
            TaskStateCode::Zombie => Ok(0),
            TaskStateCode::Running | TaskStateCode::Sleeping => {
                let mut addr = UserAddress::default() + offset;
                data.read_each(&mut |bytes| {
                    let actual =
                        task.mm.write_memory_partial(addr, bytes).map_err(|_| errno!(EIO))?;
                    addr += actual;
                    Ok(actual)
                })
            }
        }
    }
}
