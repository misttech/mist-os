// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use itertools::Itertools;
use regex::Regex;
use starnix_core::mm::{MemoryAccessor, MemoryAccessorExt, ProcMapsFile, ProcSmapsFile, PAGE_SIZE};
use starnix_core::security;
use starnix_core::task::{
    path_from_root, CurrentTask, Task, TaskPersistentInfo, TaskStateCode, ThreadGroup,
    ThreadGroupKey,
};
use starnix_core::vfs::buffers::{InputBuffer, OutputBuffer};
use starnix_core::vfs::pseudo::dynamic_file::{DynamicFile, DynamicFileBuf, DynamicFileSource};
use starnix_core::vfs::pseudo::simple_file::{
    parse_i32_file, parse_unsigned_file, serialize_for_file, BytesFile, BytesFileOps,
    SimpleFileNode,
};
use starnix_core::vfs::pseudo::static_directory::StaticDirectoryBuilder;
use starnix_core::vfs::pseudo::stub_empty_file::StubEmptyFile;
use starnix_core::vfs::pseudo::vec_directory::{VecDirectory, VecDirectoryEntry};
use starnix_core::vfs::{
    default_seek, emit_dotdot, fileops_impl_delegate_read_and_seek, fileops_impl_directory,
    fileops_impl_noop_sync, fileops_impl_seekable, fileops_impl_unbounded_seek,
    fs_node_impl_dir_readonly, CallbackSymlinkNode, DirectoryEntryType, DirentSink, FdNumber,
    FileObject, FileOps, FileSystemHandle, FsNode, FsNodeHandle, FsNodeInfo, FsNodeOps, FsStr,
    FsString, ProcMountinfoFile, ProcMountsFile, SeekTarget, SymlinkTarget,
};
use starnix_logging::{bug_ref, track_stub};
use starnix_sync::{FileOpsCore, Locked};
use starnix_types::ownership::{OwnedRef, TempRef, WeakRef};
use starnix_types::time::duration_to_scheduler_clock;
use starnix_uapi::auth::{CAP_SYS_NICE, CAP_SYS_RESOURCE};
use starnix_uapi::device_type::DeviceType;
use starnix_uapi::errors::Errno;
use starnix_uapi::file_mode::{mode, FileMode};
use starnix_uapi::open_flags::OpenFlags;
use starnix_uapi::resource_limits::Resource;
use starnix_uapi::user_address::UserAddress;
use starnix_uapi::{
    errno, error, ino_t, off_t, pid_t, uapi, OOM_ADJUST_MIN, OOM_DISABLE, OOM_SCORE_ADJ_MIN,
    RLIM_INFINITY,
};
use std::borrow::Cow;
use std::ffi::CString;
use std::ops::{Deref, Range};
use std::sync::{Arc, LazyLock};
/// Loads entries for the `scope` of a task.
fn task_entries(scope: TaskEntryScope) -> Vec<(FsString, FileMode)> {
    // NOTE: keep entries in sync with `TaskDirectory::lookup()`.
    let mut entries = vec![
        (b"cgroup".into(), mode!(IFREG, 0o444)),
        (b"cwd".into(), mode!(IFLNK, 0o777)),
        (b"exe".into(), mode!(IFLNK, 0o777)),
        (b"fd".into(), mode!(IFDIR, 0o777)),
        (b"fdinfo".into(), mode!(IFDIR, 0o777)),
        (b"io".into(), mode!(IFREG, 0o444)),
        (b"limits".into(), mode!(IFREG, 0o444)),
        (b"maps".into(), mode!(IFREG, 0o444)),
        (b"mem".into(), mode!(IFREG, 0o600)),
        (b"root".into(), mode!(IFLNK, 0o777)),
        (b"sched".into(), mode!(IFREG, 0o644)),
        (b"schedstat".into(), mode!(IFREG, 0o444)),
        (b"smaps".into(), mode!(IFREG, 0o444)),
        (b"stat".into(), mode!(IFREG, 0o444)),
        (b"statm".into(), mode!(IFREG, 0o444)),
        (b"status".into(), mode!(IFREG, 0o444)),
        (b"cmdline".into(), mode!(IFREG, 0o444)),
        (b"environ".into(), mode!(IFREG, 0o444)),
        (b"auxv".into(), mode!(IFREG, 0o444)),
        (b"comm".into(), mode!(IFREG, 0o644)),
        (b"attr".into(), mode!(IFDIR, 0o555)),
        (b"ns".into(), mode!(IFDIR, 0o777)),
        (b"mountinfo".into(), mode!(IFREG, 0o444)),
        (b"mounts".into(), mode!(IFREG, 0o444)),
        (b"oom_adj".into(), mode!(IFREG, 0o744)),
        (b"oom_score".into(), mode!(IFREG, 0o444)),
        (b"oom_score_adj".into(), mode!(IFREG, 0o744)),
        (b"timerslack_ns".into(), mode!(IFREG, 0o666)),
        (b"wchan".into(), mode!(IFREG, 0o444)),
        (b"clear_refs".into(), mode!(IFREG, 0o200)),
    ];

    if scope == TaskEntryScope::ThreadGroup {
        entries.push((b"task".into(), mode!(IFDIR, 0o777)));
    }

    entries
}

#[derive(Copy, Clone, Eq, PartialEq)]
pub enum TaskEntryScope {
    Task,
    ThreadGroup,
}

/// Represents a directory node for either `/proc/<pid>` or `/proc/<pid>/task/<tid>`.
///
/// This directory lazily creates its child entries to save memory.
///
/// It pre-allocates a range of inode numbers (`inode_range`) for all its child entries to mark
/// them as unchanged when re-accessed.
/// The `creds` stored within is applied to the directory node itself and child entries.
pub struct TaskDirectory {
    task_weak: WeakRef<Task>,
    scope: TaskEntryScope,
    inode_range: Range<ino_t>,
}

#[derive(Clone)]
struct TaskDirectoryNode(Arc<TaskDirectory>);

impl Deref for TaskDirectoryNode {
    type Target = TaskDirectory;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl TaskDirectory {
    fn new(fs: &FileSystemHandle, task: &TempRef<'_, Task>, scope: TaskEntryScope) -> FsNodeHandle {
        let creds = task.creds().euid_as_fscred();
        let task_weak = WeakRef::from(task);
        fs.create_node_and_allocate_node_id(
            TaskDirectoryNode(Arc::new(TaskDirectory {
                task_weak,
                scope,
                inode_range: fs.allocate_ino_range(task_entries(scope).len()),
            })),
            FsNodeInfo::new(mode!(IFDIR, 0o777), creds),
        )
    }
}

impl FsNodeOps for TaskDirectoryNode {
    fs_node_impl_dir_readonly!();

    fn create_file_ops(
        &self,
        _locked: &mut Locked<FileOpsCore>,
        _node: &FsNode,
        _current_task: &CurrentTask,
        _flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        Ok(Box::new(self.clone()))
    }

    fn lookup(
        &self,
        _locked: &mut Locked<FileOpsCore>,
        node: &FsNode,
        _current_task: &CurrentTask,
        name: &FsStr,
    ) -> Result<FsNodeHandle, Errno> {
        let task_weak = self.task_weak.clone();
        let creds = node.info().cred();
        let fs = node.fs();
        let (mode, ino) = task_entries(self.scope)
            .into_iter()
            .enumerate()
            .find_map(|(index, (n, mode))| {
                if name == *n {
                    Some((mode, self.inode_range.start + index as ino_t))
                } else {
                    None
                }
            })
            .ok_or_else(|| errno!(ENOENT))?;

        // NOTE: keep entries in sync with `task_entries()`.
        let ops: Box<dyn FsNodeOps> = match &**name {
            b"cgroup" => Box::new(CgroupFile::new_node(task_weak)),
            b"cwd" => Box::new(CallbackSymlinkNode::new({
                move || Ok(SymlinkTarget::Node(Task::from_weak(&task_weak)?.fs().cwd()))
            })),
            b"exe" => Box::new(CallbackSymlinkNode::new({
                move || {
                    let task = Task::from_weak(&task_weak)?;
                    if let Some(node) = task.mm().and_then(|mm| mm.executable_node()) {
                        Ok(SymlinkTarget::Node(node))
                    } else {
                        error!(ENOENT)
                    }
                }
            })),
            b"fd" => Box::new(FdDirectory::new(task_weak)),
            b"fdinfo" => Box::new(FdInfoDirectory::new(task_weak)),
            b"io" => Box::new(IoFile::new_node()),
            b"limits" => Box::new(LimitsFile::new_node(task_weak)),
            b"maps" => Box::new(ProcMapsFile::new_node(task_weak)),
            b"mem" => Box::new(MemFile::new_node(task_weak)),
            b"root" => Box::new(CallbackSymlinkNode::new({
                move || Ok(SymlinkTarget::Node(Task::from_weak(&task_weak)?.fs().root()))
            })),
            b"sched" => Box::new(StubEmptyFile::new_node(
                "/proc/pid/sched",
                bug_ref!("https://fxbug.dev/322893980"),
            )),
            b"schedstat" => Box::new(StubEmptyFile::new_node(
                "/proc/pid/schedstat",
                bug_ref!("https://fxbug.dev/322894256"),
            )),
            b"smaps" => Box::new(ProcSmapsFile::new_node(task_weak)),
            b"stat" => Box::new(StatFile::new_node(task_weak, self.scope)),
            b"statm" => Box::new(StatmFile::new_node(task_weak)),
            b"status" => Box::new(StatusFile::new_node(task_weak)),
            b"cmdline" => Box::new(CmdlineFile::new_node(task_weak)),
            b"environ" => Box::new(EnvironFile::new_node(task_weak)),
            b"auxv" => Box::new(AuxvFile::new_node(task_weak)),
            b"comm" => {
                let task = self.task_weak.upgrade().ok_or_else(|| errno!(ESRCH))?;
                Box::new(CommFile::new_node(task_weak, task.persistent_info.clone()))
            }
            b"attr" => {
                let mut subdir = StaticDirectoryBuilder::new(&fs);
                subdir.dir_creds(creds);
                for (attr, name) in [
                    (security::ProcAttr::Current, "current"),
                    (security::ProcAttr::Exec, "exec"),
                    (security::ProcAttr::FsCreate, "fscreate"),
                    (security::ProcAttr::KeyCreate, "keycreate"),
                    (security::ProcAttr::SockCreate, "sockcreate"),
                ] {
                    subdir.entry_etc(
                        name,
                        AttrNode::new(task_weak.clone(), attr),
                        mode!(IFREG, 0o666),
                        DeviceType::NONE,
                        creds,
                    );
                }
                subdir.entry_etc(
                    "prev",
                    AttrNode::new(task_weak, security::ProcAttr::Previous),
                    mode!(IFREG, 0o444),
                    DeviceType::NONE,
                    creds,
                );
                subdir.build_ops()
            }
            b"ns" => Box::new(NsDirectory { task: task_weak }),
            b"mountinfo" => Box::new(ProcMountinfoFile::new_node(task_weak)),
            b"mounts" => Box::new(ProcMountsFile::new_node(task_weak)),
            b"oom_adj" => Box::new(OomAdjFile::new_node(task_weak)),
            b"oom_score" => Box::new(OomScoreFile::new_node(task_weak)),
            b"oom_score_adj" => Box::new(OomScoreAdjFile::new_node(task_weak)),
            b"timerslack_ns" => Box::new(TimerslackNsFile::new_node(task_weak)),
            b"wchan" => Box::new(BytesFile::new_node(b"0".to_vec())),
            b"clear_refs" => Box::new(ClearRefsFile::new_node(task_weak)),
            b"task" => {
                let task = self.task_weak.upgrade().ok_or_else(|| errno!(ESRCH))?;
                Box::new(TaskListDirectory {
                    thread_group: OwnedRef::downgrade(&task.thread_group()),
                })
            }
            name => unreachable!(
                "entry \"{:?}\" should be supported to keep in sync with task_entries()",
                name
            ),
        };

        Ok(fs.create_node(ino, ops, FsNodeInfo::new(mode, creds)))
    }
}

impl FileOps for TaskDirectory {
    fileops_impl_directory!();
    fileops_impl_noop_sync!();
    fileops_impl_unbounded_seek!();

    fn readdir(
        &self,
        _locked: &mut Locked<FileOpsCore>,
        file: &FileObject,
        _current_task: &CurrentTask,
        sink: &mut dyn DirentSink,
    ) -> Result<(), Errno> {
        emit_dotdot(file, sink)?;

        // Skip through the entries until the current offset is reached.
        // Subtract 2 from the offset to account for `.` and `..`.
        for (index, (name, mode)) in
            task_entries(self.scope).into_iter().enumerate().skip(sink.offset() as usize - 2)
        {
            sink.add(
                self.inode_range.start + index as ino_t,
                sink.offset() + 1,
                DirectoryEntryType::from_mode(mode),
                name.as_ref(),
            )?;
        }
        Ok(())
    }

    fn as_thread_group_key(&self, _file: &FileObject) -> Result<ThreadGroupKey, Errno> {
        let task = self.task_weak.upgrade().ok_or_else(|| errno!(ESRCH))?;
        Ok(task.thread_group().into())
    }
}

/// Creates an [`FsNode`] that represents the `/proc/<pid>` directory for `task`.
pub fn pid_directory(
    current_task: &CurrentTask,
    fs: &FileSystemHandle,
    task: &TempRef<'_, Task>,
) -> FsNodeHandle {
    // proc(5): "The files inside each /proc/pid directory are normally
    // owned by the effective user and effective group ID of the process."
    let fs_node = TaskDirectory::new(fs, task, TaskEntryScope::ThreadGroup);

    security::task_to_fs_node(current_task, task, &fs_node);
    fs_node
}

/// Creates an [`FsNode`] that represents the `/proc/<pid>/task/<tid>` directory for `task`.
fn tid_directory(fs: &FileSystemHandle, task: &TempRef<'_, Task>) -> FsNodeHandle {
    TaskDirectory::new(fs, task, TaskEntryScope::Task)
}

/// `FdDirectory` implements the directory listing operations for a `proc/<pid>/fd` directory.
///
/// Reading the directory returns a list of all the currently open file descriptors for the
/// associated task.
struct FdDirectory {
    task: WeakRef<Task>,
}

impl FdDirectory {
    fn new(task: WeakRef<Task>) -> Self {
        Self { task }
    }
}

impl FsNodeOps for FdDirectory {
    fs_node_impl_dir_readonly!();

    fn create_file_ops(
        &self,
        _locked: &mut Locked<FileOpsCore>,
        _node: &FsNode,
        _current_task: &CurrentTask,
        _flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        Ok(VecDirectory::new_file(fds_to_directory_entries(
            Task::from_weak(&self.task)?.files.get_all_fds(),
        )))
    }

    fn lookup(
        &self,
        _locked: &mut Locked<FileOpsCore>,
        node: &FsNode,
        _current_task: &CurrentTask,
        name: &FsStr,
    ) -> Result<FsNodeHandle, Errno> {
        let fd = FdNumber::from_fs_str(name).map_err(|_| errno!(ENOENT))?;
        let task = Task::from_weak(&self.task)?;
        // Make sure that the file descriptor exists before creating the node.
        let _ = task.files.get_allowing_opath(fd).map_err(|_| errno!(ENOENT))?;
        let task_reference = self.task.clone();
        Ok(node.fs().create_node_and_allocate_node_id(
            CallbackSymlinkNode::new(move || {
                let task = Task::from_weak(&task_reference)?;
                let file = task.files.get_allowing_opath(fd).map_err(|_| errno!(ENOENT))?;
                Ok(SymlinkTarget::Node(file.name.to_passive()))
            }),
            FsNodeInfo::new(mode!(IFLNK, 0o777), task.as_fscred()),
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

/// /proc/<pid>/attr directory entry.
struct AttrNode {
    attr: security::ProcAttr,
    task: WeakRef<Task>,
}

impl AttrNode {
    fn new(task: WeakRef<Task>, attr: security::ProcAttr) -> impl FsNodeOps {
        SimpleFileNode::new(move || Ok(AttrNode { attr, task: task.clone() }))
    }
}

impl FileOps for AttrNode {
    fileops_impl_seekable!();
    fileops_impl_noop_sync!();

    fn writes_update_seek_offset(&self) -> bool {
        false
    }

    fn read(
        &self,
        _locked: &mut Locked<FileOpsCore>,
        _file: &FileObject,
        current_task: &CurrentTask,
        offset: usize,
        data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        let task = Task::from_weak(&self.task)?;
        let response = security::get_procattr(current_task, &task, self.attr)?;
        data.write(&response[offset..])
    }

    fn write(
        &self,
        _locked: &mut Locked<FileOpsCore>,
        _file: &FileObject,
        current_task: &CurrentTask,
        offset: usize,
        data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        let task = Task::from_weak(&self.task)?;

        // If the current task is not the target then writes are not allowed.
        if current_task.temp_task() != task {
            return error!(EPERM);
        }
        if offset != 0 {
            return error!(EINVAL);
        }

        let data = data.read_all()?;
        let data_len = data.len();
        security::set_procattr(current_task, self.attr, data.as_slice())?;
        Ok(data_len)
    }
}

/// /proc/[pid]/ns directory
struct NsDirectory {
    task: WeakRef<Task>,
}

impl FsNodeOps for NsDirectory {
    fs_node_impl_dir_readonly!();

    fn create_file_ops(
        &self,
        _locked: &mut Locked<FileOpsCore>,
        _node: &FsNode,
        _current_task: &CurrentTask,
        _flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        // For each namespace, this contains a link to the current identifier of the given namespace
        // for the current task.
        Ok(VecDirectory::new_file(
            NS_ENTRIES
                .iter()
                .map(|&name| VecDirectoryEntry {
                    entry_type: DirectoryEntryType::LNK,
                    name: FsString::from(name),
                    inode: None,
                })
                .collect(),
        ))
    }

    fn lookup(
        &self,
        _locked: &mut Locked<FileOpsCore>,
        node: &FsNode,
        current_task: &CurrentTask,
        name: &FsStr,
    ) -> Result<FsNodeHandle, Errno> {
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

        let task = Task::from_weak(&self.task)?;
        if let Some(id) = elements.next() {
            // The name starts with {namespace}:, check that it matches {namespace}:[id]
            static NS_IDENTIFIER_RE: LazyLock<Regex> =
                LazyLock::new(|| Regex::new("^\\[[0-9]+\\]$").unwrap());
            if !NS_IDENTIFIER_RE.is_match(id) {
                return error!(ENOENT);
            }
            let node_info = || FsNodeInfo::new(mode!(IFREG, 0o444), task.as_fscred());
            let fallback = || {
                node.fs().create_node_and_allocate_node_id(BytesFile::new_node(vec![]), node_info())
            };
            Ok(match ns {
                "cgroup" => {
                    track_stub!(TODO("https://fxbug.dev/297313673"), "cgroup namespaces");
                    fallback()
                }
                "ipc" => {
                    track_stub!(TODO("https://fxbug.dev/297313673"), "ipc namespaces");
                    fallback()
                }
                "mnt" => node.fs().create_node_and_allocate_node_id(
                    current_task.task.fs().namespace(),
                    node_info(),
                ),
                "net" => {
                    track_stub!(TODO("https://fxbug.dev/297313673"), "net namespaces");
                    fallback()
                }
                "pid" => {
                    track_stub!(TODO("https://fxbug.dev/297313673"), "pid namespaces");
                    fallback()
                }
                "pid_for_children" => {
                    track_stub!(TODO("https://fxbug.dev/297313673"), "pid_for_children namespaces");
                    fallback()
                }
                "time" => {
                    track_stub!(TODO("https://fxbug.dev/297313673"), "time namespaces");
                    fallback()
                }
                "time_for_children" => {
                    track_stub!(
                        TODO("https://fxbug.dev/297313673"),
                        "time_for_children namespaces"
                    );
                    fallback()
                }
                "user" => {
                    track_stub!(TODO("https://fxbug.dev/297313673"), "user namespaces");
                    fallback()
                }
                "uts" => {
                    track_stub!(TODO("https://fxbug.dev/297313673"), "uts namespaces");
                    fallback()
                }
                _ => return error!(ENOENT),
            })
        } else {
            // The name is {namespace}, link to the correct one of the current task.
            let id = current_task.task.fs().namespace().id;
            Ok(node.fs().create_node_and_allocate_node_id(
                CallbackSymlinkNode::new(move || {
                    Ok(SymlinkTarget::Path(format!("{name}:[{id}]").into()))
                }),
                FsNodeInfo::new(mode!(IFLNK, 0o7777), task.as_fscred()),
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
    task: WeakRef<Task>,
}

impl FdInfoDirectory {
    fn new(task: WeakRef<Task>) -> Self {
        Self { task }
    }
}

impl FsNodeOps for FdInfoDirectory {
    fs_node_impl_dir_readonly!();

    fn create_file_ops(
        &self,
        _locked: &mut Locked<FileOpsCore>,
        _node: &FsNode,
        _current_task: &CurrentTask,
        _flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        Ok(VecDirectory::new_file(fds_to_directory_entries(
            Task::from_weak(&self.task)?.files.get_all_fds(),
        )))
    }

    fn lookup(
        &self,
        _locked: &mut Locked<FileOpsCore>,
        node: &FsNode,
        _current_task: &CurrentTask,
        name: &FsStr,
    ) -> Result<FsNodeHandle, Errno> {
        let task = Task::from_weak(&self.task)?;
        let fd = FdNumber::from_fs_str(name).map_err(|_| errno!(ENOENT))?;
        let file = task.files.get_allowing_opath(fd).map_err(|_| errno!(ENOENT))?;
        let pos = *file.offset.lock();
        let flags = file.flags();
        let data = format!("pos:\t{}flags:\t0{:o}\n", pos, flags.bits()).into_bytes();
        Ok(node.fs().create_node_and_allocate_node_id(
            BytesFile::new_node(data),
            FsNodeInfo::new(mode!(IFREG, 0o444), task.as_fscred()),
        ))
    }
}

fn fds_to_directory_entries(fds: Vec<FdNumber>) -> Vec<VecDirectoryEntry> {
    fds.into_iter()
        .map(|fd| VecDirectoryEntry {
            entry_type: DirectoryEntryType::DIR,
            name: fd.raw().to_string().into(),
            inode: None,
        })
        .collect()
}

/// Directory that lists the task IDs (tid) in a process. Located at `/proc/<pid>/task/`.
struct TaskListDirectory {
    thread_group: WeakRef<ThreadGroup>,
}

impl TaskListDirectory {
    fn thread_group(&self) -> Result<TempRef<'_, ThreadGroup>, Errno> {
        self.thread_group.upgrade().ok_or_else(|| errno!(ESRCH))
    }
}

impl FsNodeOps for TaskListDirectory {
    fs_node_impl_dir_readonly!();

    fn create_file_ops(
        &self,
        _locked: &mut Locked<FileOpsCore>,
        _node: &FsNode,
        _current_task: &CurrentTask,
        _flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        Ok(VecDirectory::new_file(
            self.thread_group()?
                .read()
                .task_ids()
                .map(|tid| VecDirectoryEntry {
                    entry_type: DirectoryEntryType::DIR,
                    name: tid.to_string().into(),
                    inode: None,
                })
                .collect(),
        ))
    }

    fn lookup(
        &self,
        _locked: &mut Locked<FileOpsCore>,
        node: &FsNode,
        _current_task: &CurrentTask,
        name: &FsStr,
    ) -> Result<FsNodeHandle, Errno> {
        let thread_group = self.thread_group()?;
        let tid = std::str::from_utf8(name)
            .map_err(|_| errno!(ENOENT))?
            .parse::<pid_t>()
            .map_err(|_| errno!(ENOENT))?;
        // Make sure the tid belongs to this process.
        if !thread_group.read().contains_task(tid) {
            return error!(ENOENT);
        }

        let pid_state = thread_group.kernel.pids.read();
        let weak_task = pid_state.get_task(tid);
        let task = weak_task.upgrade().ok_or_else(|| errno!(ENOENT))?;
        std::mem::drop(pid_state);

        Ok(tid_directory(&node.fs(), &task))
    }
}

#[derive(Clone)]
struct CgroupFile(WeakRef<Task>);
impl CgroupFile {
    pub fn new_node(task: WeakRef<Task>) -> impl FsNodeOps {
        DynamicFile::new_node(Self(task))
    }
}
impl DynamicFileSource for CgroupFile {
    fn generate(&self, sink: &mut DynamicFileBuf) -> Result<(), Errno> {
        let task = Task::from_weak(&self.0)?;
        let cgroup = task.kernel().cgroups.cgroup2.get_cgroup(task.thread_group());
        let path = path_from_root(cgroup)?;
        sink.write(format!("0::{}\n", path).as_bytes());
        Ok(())
    }
}

fn fill_buf_from_addr_range(
    task: &Task,
    range_start: UserAddress,
    range_end: UserAddress,
    sink: &mut DynamicFileBuf,
) -> Result<(), Errno> {
    #[allow(clippy::manual_saturating_arithmetic)]
    let len = range_end.ptr().checked_sub(range_start.ptr()).unwrap_or(0);
    // NB: If this is exercised in a hot-path, we can plumb the reading task
    // (`CurrentTask`) here to perform a copy without going through the VMO when
    // unified aspaces is enabled.
    let buf = task.read_memory_partial_to_vec(range_start, len)?;
    sink.write(&buf[..]);
    Ok(())
}

/// `CmdlineFile` implements `proc/<pid>/cmdline` file.
#[derive(Clone)]
pub struct CmdlineFile(WeakRef<Task>);
impl CmdlineFile {
    pub fn new_node(task: WeakRef<Task>) -> impl FsNodeOps {
        DynamicFile::new_node(Self(task))
    }
}
impl DynamicFileSource for CmdlineFile {
    fn generate(&self, sink: &mut DynamicFileBuf) -> Result<(), Errno> {
        // Opened cmdline file should still be functional once the task is a zombie.
        let task = if let Some(task) = self.0.upgrade() {
            task
        } else {
            return Ok(());
        };
        // /proc/<pid>/cmdline doesn't contain anything for kthreads
        let Some(mm) = task.mm() else {
            return Ok(());
        };
        let (start, end) = {
            let mm_state = mm.state.read();
            (mm_state.argv_start, mm_state.argv_end)
        };
        fill_buf_from_addr_range(&task, start, end, sink)
    }
}

/// `EnvironFile` implements `proc/<pid>/environ` file.
#[derive(Clone)]
pub struct EnvironFile(WeakRef<Task>);
impl EnvironFile {
    pub fn new_node(task: WeakRef<Task>) -> impl FsNodeOps {
        DynamicFile::new_node(Self(task))
    }
}
impl DynamicFileSource for EnvironFile {
    fn generate(&self, sink: &mut DynamicFileBuf) -> Result<(), Errno> {
        let task = Task::from_weak(&self.0)?;
        // /proc/<pid>/environ doesn't contain anything for kthreads
        let Some(mm) = task.mm() else {
            return Ok(());
        };
        let (start, end) = {
            let mm_state = mm.state.read();
            (mm_state.environ_start, mm_state.environ_end)
        };
        fill_buf_from_addr_range(&task, start, end, sink)
    }
}

/// `AuxvFile` implements `proc/<pid>/auxv` file.
#[derive(Clone)]
pub struct AuxvFile(WeakRef<Task>);
impl AuxvFile {
    pub fn new_node(task: WeakRef<Task>) -> impl FsNodeOps {
        DynamicFile::new_node(Self(task))
    }
}
impl DynamicFileSource for AuxvFile {
    fn generate(&self, sink: &mut DynamicFileBuf) -> Result<(), Errno> {
        let task = Task::from_weak(&self.0)?;
        // /proc/<pid>/auxv doesn't contain anything for kthreads
        let Some(mm) = task.mm() else {
            return Ok(());
        };
        let (start, end) = {
            let mm_state = mm.state.read();
            (mm_state.auxv_start, mm_state.auxv_end)
        };
        fill_buf_from_addr_range(&task, start, end, sink)
    }
}

/// `CommFile` implements `proc/<pid>/comm` file.
pub struct CommFile {
    task: WeakRef<Task>,
    dynamic_file: DynamicFile<CommFileSource>,
}
impl CommFile {
    pub fn new_node(task: WeakRef<Task>, info: TaskPersistentInfo) -> impl FsNodeOps {
        SimpleFileNode::new(move || {
            Ok(CommFile {
                task: task.clone(),
                dynamic_file: DynamicFile::new(CommFileSource(info.clone())),
            })
        })
    }
}

impl FileOps for CommFile {
    fileops_impl_delegate_read_and_seek!(self, self.dynamic_file);
    fileops_impl_noop_sync!();

    fn write(
        &self,
        _locked: &mut Locked<FileOpsCore>,
        _file: &FileObject,
        current_task: &CurrentTask,
        _offset: usize,
        data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        let task = Task::from_weak(&self.task)?;
        if !OwnedRef::ptr_eq(&task.thread_group(), &current_task.thread_group()) {
            return error!(EINVAL);
        }
        // What happens if userspace writes to this file in multiple syscalls? We need more
        // detailed tests to see when the data is actually committed back to the task.
        let bytes = data.read_all()?;
        let command =
            CString::new(bytes.iter().copied().take_while(|c| *c != b'\0').collect::<Vec<_>>())
                .unwrap();
        task.set_command_name(command);
        Ok(bytes.len())
    }
}

#[derive(Clone)]
pub struct CommFileSource(TaskPersistentInfo);
impl DynamicFileSource for CommFileSource {
    fn generate(&self, sink: &mut DynamicFileBuf) -> Result<(), Errno> {
        sink.write(self.0.lock().command().as_bytes());
        sink.write(b"\n");
        Ok(())
    }
}

/// `IoFile` implements `proc/<pid>/io` file.
#[derive(Clone)]
pub struct IoFile {}
impl IoFile {
    pub fn new_node() -> impl FsNodeOps {
        DynamicFile::new_node(Self {})
    }
}
impl DynamicFileSource for IoFile {
    fn generate(&self, sink: &mut DynamicFileBuf) -> Result<(), Errno> {
        track_stub!(TODO("https://fxbug.dev/322874250"), "/proc/pid/io");
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
pub struct LimitsFile(WeakRef<Task>);
impl LimitsFile {
    pub fn new_node(task: WeakRef<Task>) -> impl FsNodeOps {
        DynamicFile::new_node(Self(task))
    }
}
impl DynamicFileSource for LimitsFile {
    fn generate_locked(
        &self,
        locked: &mut Locked<FileOpsCore>,
        sink: &mut DynamicFileBuf,
    ) -> Result<(), Errno> {
        let task = Task::from_weak(&self.0)?;
        let limits = task.thread_group().limits.lock(locked);

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
pub struct MemFile(WeakRef<Task>);
impl MemFile {
    pub fn new_node(task: WeakRef<Task>) -> impl FsNodeOps {
        SimpleFileNode::new(move || Ok(Self(task.clone())))
    }
}

impl FileOps for MemFile {
    fileops_impl_noop_sync!();

    fn is_seekable(&self) -> bool {
        true
    }

    fn seek(
        &self,
        _locked: &mut Locked<FileOpsCore>,
        _file: &FileObject,
        _current_task: &CurrentTask,
        current_offset: off_t,
        target: SeekTarget,
    ) -> Result<off_t, Errno> {
        default_seek(current_offset, target, || error!(EINVAL))
    }

    fn read(
        &self,
        _locked: &mut Locked<FileOpsCore>,
        _file: &FileObject,
        current_task: &CurrentTask,
        offset: usize,
        data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        let task = if let Some(task) = self.0.upgrade() {
            task
        } else {
            return Ok(0);
        };
        match task.state_code() {
            TaskStateCode::Zombie => Ok(0),
            TaskStateCode::Running | TaskStateCode::Sleeping | TaskStateCode::TracingStop => {
                let mut addr = UserAddress::from(offset as u64);
                data.write_each(&mut |bytes| {
                    let read_bytes = if current_task.has_same_address_space(&task) {
                        current_task.read_memory_partial(addr, bytes)
                    } else {
                        task.read_memory_partial(addr, bytes)
                    }
                    .map_err(|_| errno!(EIO))?;
                    let actual = read_bytes.len();
                    addr = (addr + actual)?;
                    Ok(actual)
                })
            }
        }
    }

    fn write(
        &self,
        _locked: &mut Locked<FileOpsCore>,
        _file: &FileObject,
        current_task: &CurrentTask,
        offset: usize,
        data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        let task = Task::from_weak(&self.0)?;
        match task.state_code() {
            TaskStateCode::Zombie => Ok(0),
            TaskStateCode::Running | TaskStateCode::Sleeping | TaskStateCode::TracingStop => {
                let addr = UserAddress::from(offset as u64);
                let mut written = 0;
                let result = data.peek_each(&mut |bytes| {
                    let actual = if current_task.has_same_address_space(&task) {
                        current_task.write_memory_partial((addr + written)?, bytes)
                    } else {
                        task.write_memory_partial((addr + written)?, bytes)
                    }
                    .map_err(|_| errno!(EIO))?;
                    written += actual;
                    Ok(actual)
                });
                data.advance(written)?;
                result
            }
        }
    }
}

#[derive(Clone)]
pub struct StatFile {
    task: WeakRef<Task>,
    scope: TaskEntryScope,
}

impl StatFile {
    pub fn new_node(task: WeakRef<Task>, scope: TaskEntryScope) -> impl FsNodeOps {
        DynamicFile::new_node(Self { task, scope })
    }
}
impl DynamicFileSource for StatFile {
    fn generate_locked(
        &self,
        locked: &mut Locked<FileOpsCore>,
        sink: &mut DynamicFileBuf,
    ) -> Result<(), Errno> {
        let task = Task::from_weak(&self.task)?;

        // All fields and their types as specified in the man page. Unimplemented fields are set to
        // 0 here.
        let pid: pid_t; // 1
        let comm: &str;
        let state: char;
        let ppid: pid_t;
        let pgrp: pid_t; // 5
        let session: pid_t;
        let tty_nr: i32;
        let tpgid: i32 = 0;
        let flags: u32 = 0;
        let minflt: u64 = 0; // 10
        let cminflt: u64 = 0;
        let majflt: u64 = 0;
        let cmajflt: u64 = 0;
        let utime: i64;
        let stime: i64; // 15
        let cutime: i64;
        let cstime: i64;
        let priority: i64 = 0;
        let nice: i64;
        let num_threads: i64; // 20
        let itrealvalue: i64 = 0;
        let mut starttime: u64 = 0;
        let mut vsize: usize = 0;
        let mut rss: usize = 0;
        let mut rsslim: u64 = 0; // 25
        let startcode: u64 = 0;
        let endcode: u64 = 0;
        let mut startstack: usize = 0;
        let kstkesp: u64 = 0;
        let kstkeip: u64 = 0; // 30
        let signal: u64 = 0;
        let blocked: u64 = 0;
        let siginore: u64 = 0;
        let sigcatch: u64 = 0;
        let wchan: u64 = 0; // 35
        let nswap: u64 = 0;
        let cnswap: u64 = 0;
        let exit_signal: i32 = 0;
        let processor: i32 = 0;
        let rt_priority: u32 = 0; // 40
        let policy: u32 = 0;
        let delayacct_blkio_ticks: u64 = 0;
        let guest_time: u64 = 0;
        let cguest_time: i64 = 0;
        let start_data: u64 = 0; // 45
        let end_data: u64 = 0;
        let start_brk: u64 = 0;
        let mut arg_start: usize = 0;
        let mut arg_end: usize = 0;
        let mut env_start: usize = 0; // 50
        let mut env_end: usize = 0;
        let exit_code: i32 = 0;

        pid = task.get_tid();
        let command = task.command();
        comm = command.as_c_str().to_str().unwrap_or("unknown");
        state = task.state_code().code_char();
        nice = task.read().scheduler_state.normal_priority().as_nice() as i64;

        {
            let thread_group = task.thread_group().read();
            ppid = thread_group.get_ppid();
            pgrp = thread_group.process_group.leader;
            session = thread_group.process_group.session.leader;

            // TTY device ID.
            {
                let session = thread_group.process_group.session.read();
                tty_nr = session
                    .controlling_terminal
                    .as_ref()
                    .map(|t| t.terminal.device().bits())
                    .unwrap_or(0) as i32;
            }

            cutime = duration_to_scheduler_clock(thread_group.children_time_stats.user_time);
            cstime = duration_to_scheduler_clock(thread_group.children_time_stats.system_time);

            num_threads = thread_group.tasks_count() as i64;
        }

        let time_stats = match self.scope {
            TaskEntryScope::Task => task.time_stats(),
            TaskEntryScope::ThreadGroup => task.thread_group().time_stats(),
        };
        utime = duration_to_scheduler_clock(time_stats.user_time);
        stime = duration_to_scheduler_clock(time_stats.system_time);

        if let Ok(info) = task.thread_group().process.info() {
            starttime = duration_to_scheduler_clock(
                zx::MonotonicInstant::from_nanos(info.start_time) - zx::MonotonicInstant::ZERO,
            ) as u64;
        }

        if let Some(mm) = task.mm() {
            let mem_stats = mm.get_stats();
            let page_size = *PAGE_SIZE as usize;
            vsize = mem_stats.vm_size;
            rss = mem_stats.vm_rss / page_size;
            rsslim = task.thread_group().limits.lock(locked).get(Resource::RSS).rlim_max;

            {
                let mm_state = mm.state.read();
                startstack = mm_state.stack_start.ptr();
                arg_start = mm_state.argv_start.ptr();
                arg_end = mm_state.argv_end.ptr();
                env_start = mm_state.environ_start.ptr();
                env_end = mm_state.environ_end.ptr();
            }
        }

        writeln!(
            sink,
            "{pid} ({comm}) {state} {ppid} {pgrp} {session} {tty_nr} {tpgid} {flags} {minflt} {cminflt} {majflt} {cmajflt} {utime} {stime} {cutime} {cstime} {priority} {nice} {num_threads} {itrealvalue} {starttime} {vsize} {rss} {rsslim} {startcode} {endcode} {startstack} {kstkesp} {kstkeip} {signal} {blocked} {siginore} {sigcatch} {wchan} {nswap} {cnswap} {exit_signal} {processor} {rt_priority} {policy} {delayacct_blkio_ticks} {guest_time} {cguest_time} {start_data} {end_data} {start_brk} {arg_start} {arg_end} {env_start} {env_end} {exit_code}"
        )?;

        Ok(())
    }
}

#[derive(Clone)]
pub struct StatmFile(WeakRef<Task>);
impl StatmFile {
    pub fn new_node(task: WeakRef<Task>) -> impl FsNodeOps {
        DynamicFile::new_node(Self(task))
    }
}
impl DynamicFileSource for StatmFile {
    fn generate(&self, sink: &mut DynamicFileBuf) -> Result<(), Errno> {
        let mem_stats = if let Some(mm) = Task::from_weak(&self.0)?.mm() {
            mm.get_stats()
        } else {
            Default::default()
        };
        let page_size = *PAGE_SIZE as usize;

        // 5th and 7th fields are deprecated and should be set to 0.
        writeln!(
            sink,
            "{} {} {} {} 0 {} 0",
            mem_stats.vm_size / page_size,
            mem_stats.vm_rss / page_size,
            mem_stats.rss_shared / page_size,
            mem_stats.vm_exe / page_size,
            (mem_stats.vm_data + mem_stats.vm_stack) / page_size
        )?;
        Ok(())
    }
}

#[derive(Clone)]
pub struct StatusFile(WeakRef<Task>);
impl StatusFile {
    pub fn new_node(task: WeakRef<Task>) -> impl FsNodeOps {
        DynamicFile::new_node(Self(task))
    }
}
impl DynamicFileSource for StatusFile {
    fn generate(&self, sink: &mut DynamicFileBuf) -> Result<(), Errno> {
        let task = &self.0.upgrade();
        let (tgid, pid, creds_string) = {
            if let Some(task) = task {
                track_stub!(TODO("https://fxbug.dev/297440106"), "/proc/pid/status zombies");
                // Collect everything stored in info in this block.  There is a lock ordering
                // issue with the task lock acquired below, and cloning info is
                // expensive.
                let info = task.persistent_info.lock();
                write!(sink, "Name:\t")?;
                sink.write(info.command().as_bytes());
                let creds = info.creds();
                (
                    Some(info.pid()),
                    Some(info.tid()),
                    Some(format!(
                        "Uid:\t{}\t{}\t{}\t{}\nGid:\t{}\t{}\t{}\t{}\nGroups:\t{}",
                        creds.uid,
                        creds.euid,
                        creds.saved_uid,
                        creds.fsuid,
                        creds.gid,
                        creds.egid,
                        creds.saved_gid,
                        creds.fsgid,
                        creds.groups.iter().map(|n| n.to_string()).join(" ")
                    )),
                )
            } else {
                (None, None, None)
            }
        };

        writeln!(sink)?;

        if let Some(task) = task {
            writeln!(sink, "Umask:\t0{:03o}", task.fs().umask().bits())?;
            let task_state = task.read();
            writeln!(sink, "SigBlk:\t{:x}", task_state.signal_mask().0)?;
            writeln!(sink, "SigPnd:\t{:x}", task_state.task_specific_pending_signals().0)?;
            writeln!(
                sink,
                "ShdPnd:\t{:x}",
                task.thread_group().pending_signals.lock().pending().0
            )?;
        }

        let state_code =
            if let Some(task) = task { task.state_code() } else { TaskStateCode::Zombie };
        writeln!(sink, "State:\t{} ({})", state_code.code_char(), state_code.name())?;

        if let Some(tgid) = tgid {
            writeln!(sink, "Tgid:\t{}", tgid)?;
        }
        if let Some(pid) = pid {
            writeln!(sink, "Pid:\t{}", pid)?;
        }
        let (ppid, threads, tracer_pid) = if let Some(task) = task {
            let tracer_pid = task.read().ptrace.as_ref().map_or(0, |p| p.get_pid());
            let task_group = task.thread_group().read();
            (task_group.get_ppid(), task_group.tasks_count(), tracer_pid)
        } else {
            (1, 1, 0)
        };
        writeln!(sink, "PPid:\t{}", ppid)?;
        writeln!(sink, "TracerPid:\t{}", tracer_pid)?;

        if let Some(creds_string) = creds_string {
            writeln!(sink, "{}", creds_string)?;
        }

        if let Some(task) = task {
            if let Some(mm) = task.mm() {
                let mem_stats = mm.get_stats();
                writeln!(sink, "VmSize:\t{} kB", mem_stats.vm_size / 1024)?;
                writeln!(sink, "VmLck:\t{} kB", mem_stats.vm_lck / 1024)?;
                writeln!(sink, "VmRSS:\t{} kB", mem_stats.vm_rss / 1024)?;
                writeln!(sink, "RssAnon:\t{} kB", mem_stats.rss_anonymous / 1024)?;
                writeln!(sink, "RssFile:\t{} kB", mem_stats.rss_file / 1024)?;
                writeln!(sink, "RssShmem:\t{} kB", mem_stats.rss_shared / 1024)?;
                writeln!(sink, "VmData:\t{} kB", mem_stats.vm_data / 1024)?;
                writeln!(sink, "VmStk:\t{} kB", mem_stats.vm_stack / 1024)?;
                writeln!(sink, "VmExe:\t{} kB", mem_stats.vm_exe / 1024)?;
                writeln!(sink, "VmSwap:\t{} kB", mem_stats.vm_swap / 1024)?;
                writeln!(sink, "VmHWM:\t{} kB", mem_stats.vm_rss_hwm / 1024)?;
            }
            // Report seccomp filter status.
            let seccomp = task.seccomp_filter_state.get() as u8;
            writeln!(sink, "Seccomp:\t{}", seccomp)?;
        }

        // There should be at least on thread in Zombie processes.
        writeln!(sink, "Threads:\t{}", std::cmp::max(1, threads))?;

        Ok(())
    }
}

struct OomScoreFile(WeakRef<Task>);

impl OomScoreFile {
    fn new_node(task: WeakRef<Task>) -> impl FsNodeOps {
        BytesFile::new_node(Self(task))
    }
}

impl BytesFileOps for OomScoreFile {
    fn read(&self, _current_task: &CurrentTask) -> Result<Cow<'_, [u8]>, Errno> {
        let _task = Task::from_weak(&self.0)?;
        track_stub!(TODO("https://fxbug.dev/322873459"), "/proc/pid/oom_score");
        Ok(serialize_for_file(0).into())
    }
}

// Redefine these constants as i32 to avoid conversions below.
const OOM_ADJUST_MAX: i32 = uapi::OOM_ADJUST_MAX as i32;
const OOM_SCORE_ADJ_MAX: i32 = uapi::OOM_SCORE_ADJ_MAX as i32;

struct OomAdjFile(WeakRef<Task>);
impl OomAdjFile {
    fn new_node(task: WeakRef<Task>) -> impl FsNodeOps {
        BytesFile::new_node(Self(task))
    }
}

impl BytesFileOps for OomAdjFile {
    fn write(&self, current_task: &CurrentTask, data: Vec<u8>) -> Result<(), Errno> {
        let value = parse_i32_file(&data)?;
        let oom_score_adj = if value == OOM_DISABLE {
            OOM_SCORE_ADJ_MIN
        } else {
            if !(OOM_ADJUST_MIN..=OOM_ADJUST_MAX).contains(&value) {
                return error!(EINVAL);
            }
            let fraction = (value - OOM_ADJUST_MIN) / (OOM_ADJUST_MAX - OOM_ADJUST_MIN);
            fraction * (OOM_SCORE_ADJ_MAX - OOM_SCORE_ADJ_MIN) + OOM_SCORE_ADJ_MIN
        };
        security::check_task_capable(current_task, CAP_SYS_RESOURCE)?;
        let task = Task::from_weak(&self.0)?;
        task.write().oom_score_adj = oom_score_adj;
        Ok(())
    }

    fn read(&self, _current_task: &CurrentTask) -> Result<Cow<'_, [u8]>, Errno> {
        let task = Task::from_weak(&self.0)?;
        let oom_score_adj = task.read().oom_score_adj;
        let oom_adj = if oom_score_adj == OOM_SCORE_ADJ_MIN {
            OOM_DISABLE
        } else {
            let fraction =
                (oom_score_adj - OOM_SCORE_ADJ_MIN) / (OOM_SCORE_ADJ_MAX - OOM_SCORE_ADJ_MIN);
            fraction * (OOM_ADJUST_MAX - OOM_ADJUST_MIN) + OOM_ADJUST_MIN
        };
        Ok(serialize_for_file(oom_adj).into())
    }
}

struct OomScoreAdjFile(WeakRef<Task>);

impl OomScoreAdjFile {
    fn new_node(task: WeakRef<Task>) -> impl FsNodeOps {
        BytesFile::new_node(Self(task))
    }
}

impl BytesFileOps for OomScoreAdjFile {
    fn write(&self, current_task: &CurrentTask, data: Vec<u8>) -> Result<(), Errno> {
        let value = parse_i32_file(&data)?;
        if !(OOM_SCORE_ADJ_MIN..=OOM_SCORE_ADJ_MAX).contains(&value) {
            return error!(EINVAL);
        }
        security::check_task_capable(current_task, CAP_SYS_RESOURCE)?;
        let task = Task::from_weak(&self.0)?;
        task.write().oom_score_adj = value;
        Ok(())
    }

    fn read(&self, _current_task: &CurrentTask) -> Result<Cow<'_, [u8]>, Errno> {
        let task = Task::from_weak(&self.0)?;
        let oom_score_adj = task.read().oom_score_adj;
        Ok(serialize_for_file(oom_score_adj).into())
    }
}

struct TimerslackNsFile(WeakRef<Task>);

impl TimerslackNsFile {
    fn new_node(task: WeakRef<Task>) -> impl FsNodeOps {
        BytesFile::new_node(Self(task))
    }
}

impl BytesFileOps for TimerslackNsFile {
    fn write(&self, current_task: &CurrentTask, data: Vec<u8>) -> Result<(), Errno> {
        let target_task = Task::from_weak(&self.0)?;
        let same_task =
            current_task.task.thread_group().leader == target_task.thread_group().leader;
        if !same_task {
            security::check_task_capable(current_task, CAP_SYS_NICE)?;
            security::check_setsched_access(current_task, &target_task)?;
        };

        let value = parse_unsigned_file(&data)?;
        target_task.write().set_timerslack_ns(value);
        Ok(())
    }

    fn read(&self, current_task: &CurrentTask) -> Result<Cow<'_, [u8]>, Errno> {
        let target_task = Task::from_weak(&self.0)?;
        let same_task =
            current_task.task.thread_group().leader == target_task.thread_group().leader;
        if !same_task {
            security::check_task_capable(current_task, CAP_SYS_NICE)?;
            security::check_getsched_access(current_task, &target_task)?;
        };

        let timerslack_ns = target_task.read().timerslack_ns;
        Ok(serialize_for_file(timerslack_ns).into())
    }
}

struct ClearRefsFile(WeakRef<Task>);

impl ClearRefsFile {
    fn new_node(task: WeakRef<Task>) -> impl FsNodeOps {
        BytesFile::new_node(Self(task))
    }
}

impl BytesFileOps for ClearRefsFile {
    fn write(&self, _current_task: &CurrentTask, _data: Vec<u8>) -> Result<(), Errno> {
        let _task = Task::from_weak(&self.0)?;
        track_stub!(TODO("https://fxbug.dev/396221597"), "/proc/pid/clear_refs");
        Ok(())
    }
}
