// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! This file implements control group hierarchy.
//!
//! There is no support for actual resource constraints, or any operations outside of adding tasks
//! to a control group (for the duration of their lifetime).

use starnix_core::signals::send_freeze_signal;
use starnix_core::task::{CurrentTask, Kernel, ProcessEntryRef, ThreadGroup, WaitQueue, Waiter};
use starnix_core::vfs::buffers::InputBuffer;
use starnix_core::vfs::{
    fileops_impl_delegate_read_and_seek, fileops_impl_noop_sync, fs_node_impl_not_dir,
    AppendLockGuard, BytesFile, BytesFileOps, DirectoryEntryType, DynamicFile, DynamicFileBuf,
    DynamicFileSource, FileObject, FileOps, FileSystemHandle, FsNode, FsNodeHandle, FsNodeInfo,
    FsNodeOps, FsStr, FsString, PathBuilder, VecDirectory, VecDirectoryEntry,
};
use starnix_logging::{log_warn, track_stub};
use starnix_sync::{FileOpsCore, Locked, Mutex};
use starnix_types::ownership::{TempRef, WeakRef};
use starnix_uapi::auth::FsCred;
use starnix_uapi::device_type::DeviceType;
use starnix_uapi::errors::Errno;
use starnix_uapi::file_mode::{mode, FileMode};
use starnix_uapi::open_flags::OpenFlags;
use starnix_uapi::{errno, error, pid_t};
use std::borrow::Cow;
use std::collections::{btree_map, hash_map, BTreeMap, HashMap};
use std::ops::Deref;
use std::sync::{Arc, OnceLock, Weak};

use crate::freezer::{FreezerFile, FreezerState};

const CONTROLLERS_FILE: &str = "cgroup.controllers";
const PROCS_FILE: &str = "cgroup.procs";
const FREEZE_FILE: &str = "cgroup.freeze";
const EVENTS_FILE: &str = "cgroup.events";

/// Common operations of all cgroups.
pub trait CgroupOps: Send + Sync + 'static {
    /// Add a process to a cgroup. Errors if the cgroup has been deleted.
    fn add_process(&self, pid: pid_t, thread_group: &TempRef<'_, ThreadGroup>)
        -> Result<(), Errno>;

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

    /// Freeze all tasks in the cgroup.
    fn freeze(&self) -> Result<(), Errno>;

    /// Thaw all tasks in the cgroup.
    fn thaw(&self);
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
    /// Look up cgroup by pid. Must be locked before child states.
    pid_table: Mutex<HashMap<pid_t, Weak<Cgroup>>>,

    /// Sub-cgroups of this cgroup.
    children: Mutex<CgroupChildren>,

    /// Interface nodes of the root cgroup. Lazily by `init()` and immutable after.
    interface_nodes: OnceLock<BTreeMap<FsString, FsNodeHandle>>,

    /// Weak reference to Kernel, used to get processes and tasks.
    kernel: Weak<Kernel>,

    /// Weak reference to self, used when creating child cgroups.
    weak_self: Weak<CgroupRoot>,
}
impl CgroupRoot {
    /// Since `CgroupRoot` is part of the `FileSystem` (see `CgroupFsV1::new_fs` and
    /// `CgroupFsV2::new_fs`), initializing a `CgroupRoot` has two steps:
    ///
    /// - new() to create a `FileSystem`,
    /// - init() to use the newly created `FileSystem` to create the `FsNode`s of the `CgroupRoot`.
    pub fn new(kernel: Weak<Kernel>) -> Arc<CgroupRoot> {
        Arc::new_cyclic(|weak_self| Self {
            weak_self: weak_self.clone(),
            kernel,
            ..Default::default()
        })
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

    fn kernel(&self) -> Arc<Kernel> {
        self.kernel.upgrade().expect("kernel is available for cgroup operations")
    }
}

impl CgroupOps for CgroupRoot {
    fn add_process(
        &self,
        pid: pid_t,
        _thread_group: &TempRef<'_, ThreadGroup>,
    ) -> Result<(), Errno> {
        let mut pid_table = self.pid_table.lock();
        match pid_table.entry(pid) {
            hash_map::Entry::Occupied(entry) => {
                // If pid is in a child cgroup, remove it.
                if let Some(cgroup) = entry.get().upgrade() {
                    cgroup.remove_process_internal(pid);
                }
                entry.remove();
            }
            // If pid is not in a child cgroup, then it must be in the root cgroup already.
            // This does not throw an error on Linux, so just return success here.
            hash_map::Entry::Vacant(_) => {}
        }
        Ok(())
    }

    fn new_child(
        &self,
        current_task: &CurrentTask,
        fs: &FileSystemHandle,
        name: &FsStr,
    ) -> Result<CgroupHandle, Errno> {
        let mut children = self.children.lock();
        children
            .insert_child(name.into(), Cgroup::new(current_task, fs, name, &self.weak_self, None))
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
        let kernel_pids = self.kernel().pids.read().process_ids();
        let controlled_pids = self.pid_table.lock();
        kernel_pids.into_iter().filter(|pid| !controlled_pids.contains_key(pid)).collect()
    }

    fn freeze(&self) -> Result<(), Errno> {
        unreachable!("Root cgroup cannot freeze any processes.");
    }

    fn thaw(&self) {
        unreachable!("Root cgroup cannot thaw any processes.");
    }
}

#[derive(Default)]
struct CgroupChildren(BTreeMap<FsString, CgroupHandle>);
impl CgroupChildren {
    fn insert_child(&mut self, name: FsString, child: CgroupHandle) -> Result<CgroupHandle, Errno> {
        let btree_map::Entry::Vacant(child_entry) = self.0.entry(name) else {
            return error!(EEXIST);
        };
        Ok(child_entry.insert(child).clone())
    }

    fn remove_child(&mut self, name: &FsStr) -> Result<CgroupHandle, Errno> {
        let btree_map::Entry::Occupied(child_entry) = self.0.entry(name.into()) else {
            return error!(ENOENT);
        };
        let child = child_entry.get();

        let mut child_state = child.state.lock();
        assert!(!child_state.deleted, "child cannot be deleted");

        child_state.update_processes();
        if !child_state.processes.is_empty() {
            // TODO(https://fxbug.dev/384194637): Remove warning log
            log_warn!(
                "Cannot remove due to active processes: {:?}",
                child_state.processes.keys().copied().collect::<Vec<_>>()
            );
            return error!(EBUSY);
        }
        if !child_state.children.is_empty() {
            // TODO(https://fxbug.dev/384194637): Remove warning log
            log_warn!(
                "Cannot remove due to sub-cgroups: {:?}",
                child_state.children.keys().cloned().collect::<Vec<FsString>>()
            );
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

/// Status of the cgroup, used for populating `cgroup.events` file.
#[derive(Debug, Default)]
pub struct CgroupStatus {
    /// Whether the cgroup or any of its descendants have any processes.
    pub populated: bool,

    /// Freezer state of the cgroup.
    pub freezer_state: FreezerState,
}

#[derive(Default)]
struct CgroupState {
    /// Subgroups of this control group.
    children: CgroupChildren,

    /// The tasks that are part of this control group.
    processes: HashMap<pid_t, WeakRef<ThreadGroup>>,

    /// If true, can no longer add children or tasks.
    deleted: bool,

    /// Wait queue to thaw all blocked tasks in this cgroup.
    wait_queue: WaitQueue,

    /// State of the cgroup freezer.
    freezer_state: FreezerState,
}

impl CgroupState {
    // Goes through `processes` and remove processes that are no longer alive.
    fn update_processes(&mut self) {
        self.processes.retain(|_pid, thread_group| {
            let Some(thread_group) = thread_group.upgrade() else {
                return false;
            };
            let terminating = thread_group.read().terminating;
            !terminating
        });
    }

    fn freeze_thread_group(&self, thread_group: &ThreadGroup) -> Result<(), Errno> {
        // Create static-lifetime TempRefs of Tasks so that we avoid don't hold the ThreadGroup
        // lock while iterating and sending the signal.
        // SAFETY: static TempRefs are released after all signals are queued.
        let tasks = thread_group.read().tasks().map(TempRef::into_static).collect::<Vec<_>>();
        for task in tasks {
            let waiter = Waiter::new();
            let wait_canceler = self.wait_queue.wait_async(&waiter);
            send_freeze_signal(&task, waiter, wait_canceler)?;
        }

        Ok(())
    }
}

/// `Cgroup` is a non-root cgroup in a cgroup hierarchy, and can have other `Cgroup`s as children.
pub struct Cgroup {
    root: Weak<CgroupRoot>,

    /// Name of the cgroup.
    name: FsString,

    /// Weak reference to its parent cgroup, `None` if direct descendent of the root cgroup.
    /// This field is useful in implementing features that only apply to non-root cgroups.
    parent: Option<Weak<Cgroup>>,

    /// Internal state of the Cgroup.
    state: Mutex<CgroupState>,

    /// The directory node associated with this control group.
    directory_node: FsNodeHandle,

    /// The interface nodes associated with this control group.
    interface_nodes: BTreeMap<FsString, FsNodeHandle>,

    weak_self: Weak<Cgroup>,
}
pub type CgroupHandle = Arc<Cgroup>;

/// Returns the path from the root to this `cgroup`.
#[allow(dead_code)]
pub fn path_from_root(cgroup: CgroupHandle) -> Result<FsString, Errno> {
    let mut path = PathBuilder::new();
    let mut current = Some(cgroup);
    while let Some(cgroup) = current {
        path.prepend_element(cgroup.name());
        current = cgroup.parent()?;
    }
    Ok(path.build_absolute())
}

impl Cgroup {
    pub fn new(
        current_task: &CurrentTask,
        fs: &FileSystemHandle,
        name: &FsStr,
        root: &Weak<CgroupRoot>,
        parent: Option<Weak<Cgroup>>,
    ) -> CgroupHandle {
        Arc::new_cyclic(|weak| {
            let weak_ops = weak.clone() as Weak<dyn CgroupOps>;
            Self {
                root: root.clone(),
                name: name.to_owned(),
                parent,
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
                            FreezerFile::new_node(weak.clone() as Weak<Self>),
                            FsNodeInfo::new_factory(mode!(IFREG, 0o644), FsCred::root()),
                        ),
                    ),
                    (
                        EVENTS_FILE.into(),
                        fs.create_node(
                            current_task,
                            EventsFile::new_node(weak.clone() as Weak<Self>),
                            FsNodeInfo::new_factory(mode!(IFREG, 0o644), FsCred::root()),
                        ),
                    ),
                ]),
                weak_self: weak.clone(),
            }
        })
    }

    fn name(&self) -> &FsStr {
        self.name.as_ref()
    }

    fn root(&self) -> Result<Arc<CgroupRoot>, Errno> {
        self.root.upgrade().ok_or_else(|| errno!(ENODEV))
    }

    /// Returns the upgraded parent cgroup, or `Ok(None)` if cgroup is a direct desendent of root.
    /// Errors if parent node is no longer around.
    fn parent(&self) -> Result<Option<CgroupHandle>, Errno> {
        self.parent.as_ref().map(|weak| weak.upgrade().ok_or_else(|| errno!(ENODEV))).transpose()
    }

    fn add_process_internal(
        &self,
        pid: pid_t,
        thread_group: &TempRef<'_, ThreadGroup>,
    ) -> Result<(), Errno> {
        let mut state = self.state.lock();
        if state.deleted {
            return error!(ENOENT);
        }
        state.processes.insert(pid, WeakRef::from(thread_group));

        // Check if the cgroup is frozen. If so, freeze the new process.
        if state.freezer_state == FreezerState::Frozen {
            state.freeze_thread_group(&thread_group)?;
        }

        Ok(())
    }

    fn remove_process_internal(&self, pid: pid_t) {
        let mut state = self.state.lock();
        if !state.deleted {
            state.processes.remove(&pid);
        }
    }

    /// Gets the current status of the cgroup.
    pub fn get_status(&self) -> CgroupStatus {
        let mut state = self.state.lock();
        if state.deleted {
            return CgroupStatus::default();
        }
        state.update_processes();
        CgroupStatus { populated: !state.processes.is_empty(), freezer_state: state.freezer_state }
    }
}

impl CgroupOps for Cgroup {
    fn add_process(
        &self,
        pid: pid_t,
        thread_group: &TempRef<'_, ThreadGroup>,
    ) -> Result<(), Errno> {
        let root = self.root()?;
        let mut pid_table = root.pid_table.lock();
        match pid_table.entry(pid) {
            hash_map::Entry::Occupied(mut entry) => {
                // Check if pid is already in the current cgroup. Linux does not return an error if
                // it already exists.
                if std::ptr::eq(self, entry.get().as_ptr()) {
                    return Ok(());
                }

                // If pid is in another cgroup, we need to remove it first.
                track_stub!(TODO("https://fxbug.dev/383374687"), "check permissions");
                if let Some(other_cgroup) = entry.get().upgrade() {
                    other_cgroup.remove_process_internal(pid);
                }

                self.add_process_internal(pid, thread_group)?;
                entry.insert(self.weak_self.clone());
            }
            hash_map::Entry::Vacant(entry) => {
                self.add_process_internal(pid, thread_group)?;
                entry.insert(self.weak_self.clone());
            }
        }

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
        state.children.insert_child(
            name.into(),
            Cgroup::new(current_task, fs, name, &self.root, Some(self.weak_self.clone())),
        )
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
        let mut state = self.state.lock();
        state.update_processes();
        state.processes.keys().copied().collect()
    }

    fn freeze(&self) -> Result<(), Errno> {
        let mut state = self.state.lock();

        for (_, thread_group) in state.processes.iter() {
            let Some(thread_group) = thread_group.upgrade() else {
                continue;
            };
            state.freeze_thread_group(&thread_group)?;
        }
        state.freezer_state = FreezerState::Frozen;
        Ok(())
    }

    fn thaw(&self) {
        let mut state = self.state.lock();
        state.wait_queue.notify_all();
        state.freezer_state = FreezerState::Thawed;
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
            write!(sink, "{pid}\n")?;
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
        let bytes = data.read_all()?;
        let pid_string = std::str::from_utf8(&bytes).map_err(|_| errno!(EINVAL))?;
        let pid = pid_string.trim().parse::<pid_t>().map_err(|_| errno!(ENOENT))?;

        // Check if the pid is a valid task.
        let kernel_pids = current_task.kernel().pids.read();
        let Some(ProcessEntryRef::Process(ref thread_group)) = kernel_pids.get_process(pid) else {
            return error!(EINVAL);
        };

        self.cgroup()?.add_process(pid, thread_group)?;

        Ok(bytes.len())
    }
}

pub struct EventsFile {
    cgroup: Weak<Cgroup>,
}
impl EventsFile {
    fn new_node(cgroup: Weak<Cgroup>) -> impl FsNodeOps {
        BytesFile::new_node(Self { cgroup })
    }

    fn cgroup(&self) -> Result<Arc<Cgroup>, Errno> {
        self.cgroup.upgrade().ok_or_else(|| errno!(ENODEV))
    }
}

impl BytesFileOps for EventsFile {
    fn read(&self, _current_task: &CurrentTask) -> Result<Cow<'_, [u8]>, Errno> {
        track_stub!(
            TODO("https://fxbug.dev/377755814"),
            "cgroup.events does not check state of parent and children cgroup"
        );
        let status = self.cgroup()?.get_status();
        let events_str =
            format!("populated {}\nfrozen {}\n", status.populated as u8, status.freezer_state);
        Ok(events_str.as_bytes().to_owned().into())
    }
}

#[cfg(test)]
mod test {
    use crate::CgroupV2Fs;

    use super::*;
    use starnix_core::testing::create_kernel_task_and_unlocked;
    use starnix_core::vfs::fs_registry::FsRegistry;

    #[::fuchsia::test]
    async fn cgroup_path_from_root() {
        let (kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let registry = kernel.expando.get::<FsRegistry>();
        registry.register(b"cgroup2".into(), CgroupV2Fs::new_fs);
        let fs = current_task
            .create_filesystem(&mut locked, b"cgroup2".into(), Default::default())
            .expect("");
        let root = fs.downcast_ops::<CgroupV2Fs>().expect("").root.clone();

        let test_cgroup = root.new_child(&current_task, &fs, "test".into()).expect("");
        let child_cgroup = test_cgroup.new_child(&current_task, &fs, "child".into()).expect("");

        assert_eq!(path_from_root(test_cgroup), Ok("/test".into()));
        assert_eq!(path_from_root(child_cgroup), Ok("/test/child".into()));
    }
}
