// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! This file implements control group hierarchy.
//!
//! There is no support for actual resource constraints, or any operations outside of adding tasks
//! to a control group (for the duration of their lifetime).

use starnix_core::task::{CurrentTask, Task};
use starnix_core::vfs::buffers::InputBuffer;
use starnix_core::vfs::{
    fileops_impl_delegate_read_and_seek, fileops_impl_noop_sync, fs_node_impl_not_dir,
    AppendLockGuard, BytesFile, DirectoryEntryType, DynamicFile, DynamicFileBuf, DynamicFileSource,
    FileObject, FileOps, FileSystemHandle, FsNode, FsNodeHandle, FsNodeInfo, FsNodeOps, FsStr,
    FsString, VecDirectory, VecDirectoryEntry,
};
use starnix_logging::track_stub;
use starnix_sync::{FileOpsCore, Locked, Mutex};
use starnix_types::ownership::WeakRef;
use starnix_uapi::auth::FsCred;
use starnix_uapi::device_type::DeviceType;
use starnix_uapi::errors::Errno;
use starnix_uapi::file_mode::{mode, FileMode};
use starnix_uapi::open_flags::OpenFlags;
use starnix_uapi::{errno, error, pid_t};
use std::collections::btree_map::Entry;
use std::collections::BTreeMap;
use std::ops::Deref;
use std::sync::{Arc, OnceLock, Weak};

use crate::freezer::FreezerFile;

const CONTROLLERS_FILE: &str = "cgroup.controllers";
const PROCS_FILE: &str = "cgroup.procs";
const FREEZE_FILE: &str = "cgroup.freeze";

/// Common operations of all cgroups.
pub trait CgroupOps: Send + Sync + 'static {
    /// Add a task to a cgroup. Errors if the cgroup has been deleted.
    fn add_task(&self, task: WeakRef<Task>) -> Result<(), Errno>;

    /// Create a new sub-cgroup as a child of this cgroup. Errors if the cgroup is deleted, or a
    /// child with `name` already exists.
    fn new_child(
        &self,
        current_task: &CurrentTask,
        fs: &FileSystemHandle,
        name: &FsStr,
    ) -> Result<CgroupHandle, Errno>;

    /// Remove a child from this cgroup and return it, if found. Errors if cgroup is deleted, or a
    /// child with `name` is not found.
    fn remove_child(&self, name: &FsStr) -> Result<CgroupHandle, Errno>;

    /// Return a `VecDirectoryEntry` for each interface file and each child.
    fn get_entries(&self) -> Vec<VecDirectoryEntry>;

    /// Find a child or interface file with the given name and return its `node`, if exists. Errors
    /// if such a node was not found.
    fn get_node(&self, name: &FsStr) -> Result<FsNodeHandle, Errno>;

    /// Return all pids that belong to this cgroup.
    fn get_pids(&self) -> Vec<pid_t>;
}

/// `CgroupRoot` is the root of the cgroup hierarchy. The root cgroup is different from the rest of
/// the cgroups in a cgroup hierarchy (sub-cgroups of the root) in a few ways:
///
/// - The root contains all known processes on cgroup creation, and all new processes as they are
/// spawned. As such, the root cgroup reports processes belonging to it differently than its
/// sub-cgroups.
///
/// - The root does not contain resource controller interface files, as otherwise they would apply
/// to the whole system.
///
/// - The root does not own a `FsNode` as it is created and owned by the `FileSystem` instead.
#[derive(Default)]
pub struct CgroupRoot {
    /// Sub-cgroups of this cgroup.
    children: Mutex<CgroupChildren>,

    /// Interface nodes of the root cgroup. Lazily by `init()` and immutable after.
    interface_nodes: OnceLock<BTreeMap<FsString, FsNodeHandle>>,
}
impl CgroupRoot {
    /// Since `CgroupRoot` is part of the `FileSystem` (see `CgroupFsV1::new_fs` and
    /// `CgroupFsV2::new_fs`), initializing a `CgroupRoot` has two steps:
    ///
    /// - new() to create a `FileSystem`,
    /// - init() to use the newly created `FileSystem` to create the `FsNode`s of the `CgroupRoot`.
    pub fn new() -> Arc<CgroupRoot> {
        Arc::new(Self::default())
    }

    /// Populate `interface_nodes` with nodes of the cgroup root directory, then set
    /// `CgroupDirectoryHandle` to be the root node of the `FileSystem`. Can only be called once.
    pub fn init(self: &Arc<Self>, current_task: &CurrentTask, fs: &FileSystemHandle) {
        let cloned = self.clone();
        let weak_ops = Arc::downgrade(&(cloned as Arc<dyn CgroupOps>));
        self.interface_nodes
            .set(BTreeMap::from([
                (
                    PROCS_FILE.into(),
                    fs.create_node(
                        current_task,
                        ControlGroupNode::new(weak_ops.clone()),
                        FsNodeInfo::new_factory(mode!(IFREG, 0o644), FsCred::root()),
                    ),
                ),
                (
                    CONTROLLERS_FILE.into(),
                    fs.create_node(
                        current_task,
                        BytesFile::new_node(b"".to_vec()),
                        FsNodeInfo::new_factory(mode!(IFREG, 0o444), FsCred::root()),
                    ),
                ),
            ]))
            .expect("CgroupRoot is only initialized once");
        fs.set_root(CgroupDirectory::new(weak_ops));
    }
}

impl CgroupOps for CgroupRoot {
    fn add_task(&self, _task: WeakRef<Task>) -> Result<(), Errno> {
        track_stub!(TODO("https://fxbug.dev/377429221"), "add task to root cgroup");
        Ok(())
    }

    fn new_child(
        &self,
        current_task: &CurrentTask,
        fs: &FileSystemHandle,
        name: &FsStr,
    ) -> Result<CgroupHandle, Errno> {
        let mut children = self.children.lock();
        children.insert_child(name.into(), Cgroup::new(current_task, fs))
    }

    fn remove_child(&self, name: &FsStr) -> Result<CgroupHandle, Errno> {
        let mut children = self.children.lock();
        children.remove_child(name)
    }

    fn get_entries(&self) -> Vec<VecDirectoryEntry> {
        let entries = self.interface_nodes.get().expect("CgroupRoot is initialized").iter().map(
            |(name, child)| VecDirectoryEntry {
                entry_type: DirectoryEntryType::REG,
                name: name.clone(),
                inode: Some(child.info().ino),
            },
        );
        let children = self.children.lock();
        entries.chain(children.get_entries()).collect()
    }

    fn get_node(&self, name: &FsStr) -> Result<FsNodeHandle, Errno> {
        if let Some(node) = self.interface_nodes.get().expect("CgroupRoot is initialized").get(name)
        {
            Ok(node.clone())
        } else {
            let children = self.children.lock();
            children.get_node(name)
        }
    }

    fn get_pids(&self) -> Vec<pid_t> {
        track_stub!(TODO("https://fxbug.dev/377429221"), "get tasks from root cgroup");
        vec![]
    }
}

#[derive(Default)]
struct CgroupChildren(BTreeMap<FsString, CgroupHandle>);
impl CgroupChildren {
    fn insert_child(&mut self, name: FsString, child: CgroupHandle) -> Result<CgroupHandle, Errno> {
        let Entry::Vacant(child_entry) = self.0.entry(name) else {
            return error!(EEXIST);
        };
        Ok(child_entry.insert(child).clone())
    }

    fn remove_child(&mut self, name: &FsStr) -> Result<CgroupHandle, Errno> {
        let Entry::Occupied(child_entry) = self.0.entry(name.into()) else {
            return error!(ENOENT);
        };
        let child = child_entry.get();

        let mut child_state = child.state.lock();
        assert!(!child_state.deleted, "child cannot be deleted");

        if !child_state.tasks.is_empty() {
            // TODO(https://fxbug.dev/375677856): Should filter out tasks that are no longer around.
            return error!(EBUSY);
        }
        if !child_state.children.is_empty() {
            return error!(EBUSY);
        }

        child_state.deleted = true;
        drop(child_state);

        Ok(child_entry.remove())
    }

    fn get_entries(&self) -> impl IntoIterator<Item = VecDirectoryEntry> + '_ {
        self.0.iter().map(|(name, child)| VecDirectoryEntry {
            entry_type: DirectoryEntryType::DIR,
            name: name.clone(),
            inode: Some(child.directory_node.info().ino),
        })
    }

    fn get_node(&self, name: &FsStr) -> Result<FsNodeHandle, Errno> {
        self.0.get(name).map(|child| child.directory_node.clone()).ok_or_else(|| errno!(ENOENT))
    }
}

impl Deref for CgroupChildren {
    type Target = BTreeMap<FsString, CgroupHandle>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

#[derive(Default)]
struct CgroupState {
    /// Subgroups of this control group.
    children: CgroupChildren,

    /// The tasks that are part of this control group.
    tasks: Vec<WeakRef<Task>>,

    /// If true, can no longer add children or tasks.
    deleted: bool,
}

/// `Cgroup` is a non-root cgroup in a cgroup hierarchy, and can have other `Cgroup`s as children.
pub struct Cgroup {
    state: Mutex<CgroupState>,

    /// The directory node associated with this control group.
    directory_node: FsNodeHandle,

    /// The interface nodes associated with this control group.
    interface_nodes: BTreeMap<FsString, FsNodeHandle>,
}
pub type CgroupHandle = Arc<Cgroup>;

impl Cgroup {
    pub fn new(current_task: &CurrentTask, fs: &FileSystemHandle) -> CgroupHandle {
        Arc::new_cyclic(|weak| {
            let weak_ops = weak.clone() as Weak<dyn CgroupOps>;
            Self {
                state: Default::default(),
                directory_node: fs.create_node(
                    current_task,
                    CgroupDirectory::new(weak_ops.clone()),
                    FsNodeInfo::new_factory(mode!(IFDIR, 0o755), FsCred::root()),
                ),
                interface_nodes: BTreeMap::from([
                    (
                        PROCS_FILE.into(),
                        fs.create_node(
                            current_task,
                            ControlGroupNode::new(weak_ops.clone()),
                            FsNodeInfo::new_factory(mode!(IFREG, 0o644), FsCred::root()),
                        ),
                    ),
                    (
                        CONTROLLERS_FILE.into(),
                        fs.create_node(
                            current_task,
                            BytesFile::new_node(b"".to_vec()),
                            FsNodeInfo::new_factory(mode!(IFREG, 0o444), FsCred::root()),
                        ),
                    ),
                    (
                        FREEZE_FILE.into(),
                        fs.create_node(
                            current_task,
                            FreezerFile::new_node(),
                            FsNodeInfo::new_factory(mode!(IFREG, 0o644), FsCred::root()),
                        ),
                    ),
                ]),
            }
        })
    }
}

impl CgroupOps for Cgroup {
    fn add_task(&self, task: WeakRef<Task>) -> Result<(), Errno> {
        let mut state = self.state.lock();
        if state.deleted {
            return error!(ENOENT);
        }
        state.tasks.push(task);
        Ok(())
    }

    fn new_child(
        &self,
        current_task: &CurrentTask,
        fs: &FileSystemHandle,
        name: &FsStr,
    ) -> Result<CgroupHandle, Errno> {
        let mut state = self.state.lock();
        if state.deleted {
            return error!(ENOENT);
        }
        state.children.insert_child(name.into(), Cgroup::new(current_task, fs))
    }

    fn remove_child(&self, name: &FsStr) -> Result<CgroupHandle, Errno> {
        let mut state = self.state.lock();
        if state.deleted {
            return error!(ENOENT);
        }
        state.children.remove_child(name)
    }

    fn get_entries(&self) -> Vec<VecDirectoryEntry> {
        let entries = self.interface_nodes.iter().map(|(name, child)| VecDirectoryEntry {
            entry_type: DirectoryEntryType::REG,
            name: name.clone(),
            inode: Some(child.info().ino),
        });
        let state = self.state.lock();
        entries.chain(state.children.get_entries()).collect()
    }

    fn get_node(&self, name: &FsStr) -> Result<FsNodeHandle, Errno> {
        if let Some(node) = self.interface_nodes.get(name) {
            Ok(node.clone())
        } else {
            let state = self.state.lock();
            state.children.get_node(name)
        }
    }

    fn get_pids(&self) -> Vec<pid_t> {
        let mut pids: Vec<pid_t> = vec![];
        let mut state = self.state.lock();
        state.tasks.retain(|t| {
            if let Some(t) = t.upgrade() {
                pids.push(t.get_pid());
                true
            } else {
                // Filter out the tasks that have been dropped.
                false
            }
        });
        pids
    }
}

/// A `CgroupDirectoryNode` represents the node associated with a particular control group.
#[derive(Debug)]
pub struct CgroupDirectory {
    /// The associated cgroup.
    cgroup: Weak<dyn CgroupOps>,
}

impl CgroupDirectory {
    pub fn new(cgroup: Weak<dyn CgroupOps>) -> CgroupDirectoryHandle {
        CgroupDirectoryHandle(Arc::new(Self { cgroup }))
    }
}

/// `CgroupDirectoryHandle` is needed to implement a trait for an Arc.
#[derive(Debug, Clone)]
pub struct CgroupDirectoryHandle(Arc<CgroupDirectory>);
impl CgroupDirectoryHandle {
    fn cgroup(&self) -> Result<Arc<dyn CgroupOps>, Errno> {
        self.cgroup.upgrade().ok_or_else(|| errno!(ENODEV))
    }
}

impl Deref for CgroupDirectoryHandle {
    type Target = CgroupDirectory;

    fn deref(&self) -> &Self::Target {
        &self.0.deref()
    }
}

impl FsNodeOps for CgroupDirectoryHandle {
    fn create_file_ops(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _node: &FsNode,
        _current_task: &CurrentTask,
        _flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        Ok(VecDirectory::new_file(self.cgroup()?.get_entries()))
    }

    fn mkdir(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        node: &FsNode,
        current_task: &CurrentTask,
        name: &FsStr,
        _mode: FileMode,
        _owner: FsCred,
    ) -> Result<FsNodeHandle, Errno> {
        let child = self.cgroup()?.new_child(current_task, &node.fs(), name)?;
        node.update_info(|info| {
            info.link_count += 1;
        });
        Ok(child.directory_node.clone())
    }

    fn mknod(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _node: &FsNode,
        _current_task: &CurrentTask,
        _name: &FsStr,
        _mode: FileMode,
        _dev: DeviceType,
        _owner: FsCred,
    ) -> Result<FsNodeHandle, Errno> {
        error!(EACCES)
    }

    fn unlink(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        node: &FsNode,
        _current_task: &CurrentTask,
        name: &FsStr,
        child: &FsNodeHandle,
    ) -> Result<(), Errno> {
        let cgroup = self.cgroup()?;

        // Only cgroup directories can be removed. Cgroup interface files cannot be removed.
        let Some(child_dir) = child.downcast_ops::<CgroupDirectoryHandle>() else {
            return error!(EPERM);
        };
        let child_cgroup = child_dir.cgroup()?;

        let removed = cgroup.remove_child(name)?;
        assert!(Arc::ptr_eq(&(removed as Arc<dyn CgroupOps>), &child_cgroup));

        node.update_info(|info| {
            info.link_count -= 1;
        });

        Ok(())
    }

    fn create_symlink(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _node: &FsNode,
        _current_task: &CurrentTask,
        _name: &FsStr,
        _target: &FsStr,
        _owner: FsCred,
    ) -> Result<FsNodeHandle, Errno> {
        error!(EPERM)
    }

    fn lookup(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _node: &FsNode,
        _current_task: &CurrentTask,
        name: &FsStr,
    ) -> Result<FsNodeHandle, Errno> {
        self.cgroup()?.get_node(name)
    }
}

/// A `ControlGroupNode` backs the `cgroup.procs` file.
///
/// Opening and writing to this node will add tasks to the control group.
struct ControlGroupNode {
    cgroup: Weak<dyn CgroupOps>,
}

impl ControlGroupNode {
    fn new(cgroup: Weak<dyn CgroupOps>) -> Self {
        ControlGroupNode { cgroup }
    }
}

impl FsNodeOps for ControlGroupNode {
    fs_node_impl_not_dir!();

    fn create_file_ops(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _node: &FsNode,
        _current_task: &CurrentTask,
        _flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        Ok(Box::new(ControlGroupFile::new(self.cgroup.clone())))
    }

    fn truncate(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _guard: &AppendLockGuard<'_>,
        _node: &FsNode,
        _current_task: &CurrentTask,
        _length: u64,
    ) -> Result<(), Errno> {
        Ok(())
    }
}

struct ControlGroupFileSource {
    cgroup: Weak<dyn CgroupOps>,
}

impl ControlGroupFileSource {
    fn cgroup(&self) -> Result<Arc<dyn CgroupOps>, Errno> {
        self.cgroup.upgrade().ok_or_else(|| errno!(ENODEV))
    }
}

impl DynamicFileSource for ControlGroupFileSource {
    fn generate(&self, sink: &mut DynamicFileBuf) -> Result<(), Errno> {
        let cgroup = self.cgroup()?;
        for pid in cgroup.get_pids() {
            write!(sink, "{pid}")?;
        }
        Ok(())
    }
}

/// A `ControlGroupFile` currently represents the `cgroup.procs` file for the control group. Writing
/// to this file will add tasks to the control group.
pub struct ControlGroupFile {
    cgroup: Weak<dyn CgroupOps>,
    dynamic_file: DynamicFile<ControlGroupFileSource>,
}

impl ControlGroupFile {
    fn new(cgroup: Weak<dyn CgroupOps>) -> Self {
        Self {
            cgroup: cgroup.clone(),
            dynamic_file: DynamicFile::new(ControlGroupFileSource { cgroup: cgroup.clone() }),
        }
    }

    fn cgroup(&self) -> Result<Arc<dyn CgroupOps>, Errno> {
        self.cgroup.upgrade().ok_or_else(|| errno!(ENODEV))
    }
}

impl FileOps for ControlGroupFile {
    fileops_impl_delegate_read_and_seek!(self, self.dynamic_file);
    fileops_impl_noop_sync!();

    fn write(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        current_task: &CurrentTask,
        _offset: usize,
        data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        let cgroup = self.cgroup()?;
        let bytes = data.read_all()?;

        let pid_string = std::str::from_utf8(&bytes).map_err(|_| errno!(EINVAL))?;
        let pid = pid_string.trim().parse::<pid_t>().map_err(|_| errno!(ENOENT))?;
        let weak_task = current_task.get_task(pid);
        let task = weak_task.upgrade().ok_or_else(|| errno!(EINVAL))?;

        cgroup.add_task(WeakRef::from(&task))?;

        Ok(bytes.len())
    }
}
