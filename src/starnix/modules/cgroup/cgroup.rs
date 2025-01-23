// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! This file implements control group hierarchy.
//!
//! There is no support for actual resource constraints, or any operations outside of adding tasks
//! to a control group (for the duration of their lifetime).

use starnix_core::signals::send_freeze_signal;
use starnix_core::task::{CurrentTask, Kernel, ThreadGroup, WaitQueue, Waiter};
use starnix_core::vfs::{
    BytesFile, DirectoryEntryType, FileSystemHandle, FsNodeHandle, FsNodeInfo, FsStr, FsString,
    PathBuilder, VecDirectoryEntry,
};
use starnix_logging::{log_warn, track_stub};
use starnix_sync::Mutex;
use starnix_types::ownership::{TempRef, WeakRef};
use starnix_uapi::auth::FsCred;
use starnix_uapi::errors::Errno;
use starnix_uapi::file_mode::mode;
use starnix_uapi::{errno, error, pid_t};
use std::collections::{btree_map, hash_map, BTreeMap, HashMap};
use std::ops::Deref;
use std::sync::{Arc, OnceLock, Weak};

use crate::directory::CgroupDirectory;
use crate::events::EventsFile;
use crate::freeze::FreezeFile;
use crate::procs::ControlGroupNode;

const CONTROLLERS_FILE: &str = "cgroup.controllers";
const PROCS_FILE: &str = "cgroup.procs";
const FREEZE_FILE: &str = "cgroup.freeze";
const EVENTS_FILE: &str = "cgroup.events";

#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum FreezerState {
    Thawed,
    Frozen,
}

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

    /// Gets the current status of the cgroup.
    fn get_status(&self) -> CgroupStatus;

    /// Freeze all tasks in the cgroup.
    fn freeze(&self);

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
        let new_child = Cgroup::new(current_task, fs, name, &self.weak_self, None);
        let mut children = self.children.lock();
        children.insert_child(name.into(), new_child)
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

    fn get_status(&self) -> CgroupStatus {
        Default::default()
    }

    fn freeze(&self) {
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

    fn get_children(&self) -> impl IntoIterator<Item = &CgroupHandle> + '_ {
        self.0.values()
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

/// Status of the cgroup, used for populating `cgroup.events` and `cgroup.freeze` file.
#[derive(Debug, Default, Clone)]
pub struct CgroupStatus {
    /// Whether the cgroup or any of its descendants have any processes.
    pub populated: bool,

    /// Effective freezer state of this cgroup.
    ///
    /// This considers both the cgroup's own freezer state as set by the `cgroup.freeze` file and
    /// the freezer state of its ancestors. A cgroup is considered frozen if either itself
    /// or any of its ancestors is frozen.
    pub effective_freezer_state: FreezerState,

    /// The cgroup's own freezer state as set by the `cgroup.freeze` file.
    ///
    /// This does not reflect the state of its ancestors.
    pub self_freezer_state: FreezerState,
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

    /// The cgroup's own freezer state.
    self_freezer_state: FreezerState,

    /// Effective freezer state inherited from the parent cgroup.
    inherited_freezer_state: FreezerState,
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

    fn freeze_thread_group(&self, thread_group: &ThreadGroup) {
        // Create static-lifetime TempRefs of Tasks so that we avoid don't hold the ThreadGroup
        // lock while iterating and sending the signal.
        // SAFETY: static TempRefs are released after all signals are queued.
        let tasks = thread_group.read().tasks().map(TempRef::into_static).collect::<Vec<_>>();
        for task in tasks {
            let waiter = Waiter::new();
            let wait_canceler = self.wait_queue.wait_async(&waiter);
            send_freeze_signal(&task, waiter, wait_canceler)
                .expect("sending freeze signal should not fail");
        }
    }

    fn get_status(&mut self) -> CgroupStatus {
        if self.deleted {
            return CgroupStatus::default();
        }
        self.update_processes();
        CgroupStatus {
            populated: !self.processes.is_empty(),
            effective_freezer_state: self.get_effective_freezer_state(),
            self_freezer_state: self.self_freezer_state,
        }
    }

    fn get_effective_freezer_state(&self) -> FreezerState {
        std::cmp::max(self.self_freezer_state, self.inherited_freezer_state)
    }

    fn propagate_freeze(&mut self, inherited_freezer_state: FreezerState) {
        let prev_effective_freezer_state = self.get_effective_freezer_state();
        self.inherited_freezer_state = inherited_freezer_state;
        if prev_effective_freezer_state == FreezerState::Frozen {
            return;
        }

        for (_, thread_group) in self.processes.iter() {
            let Some(thread_group) = thread_group.upgrade() else {
                continue;
            };
            self.freeze_thread_group(&thread_group);
        }

        // Freeze all children cgroups while holding self state lock
        for child in self.children.get_children() {
            child.state.lock().propagate_freeze(FreezerState::Frozen);
        }
    }

    fn propagate_thaw(&mut self, inherited_freezer_state: FreezerState) {
        self.inherited_freezer_state = inherited_freezer_state;
        if self.get_effective_freezer_state() == FreezerState::Thawed {
            self.wait_queue.notify_all();
            for child in self.children.get_children() {
                child.state.lock().propagate_thaw(FreezerState::Thawed);
            }
        }
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
    pub directory_node: FsNodeHandle,

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
                            FreezeFile::new_node(weak.clone() as Weak<Self>),
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

        if state.get_effective_freezer_state() == FreezerState::Frozen {
            state.freeze_thread_group(&thread_group);
        }
        Ok(())
    }

    fn remove_process_internal(&self, pid: pid_t) {
        let mut state = self.state.lock();
        if !state.deleted {
            state.processes.remove(&pid);
        }
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
        let new_child =
            Cgroup::new(current_task, fs, name, &self.root, Some(self.weak_self.clone()));
        let mut state = self.state.lock();
        if state.deleted {
            return error!(ENOENT);
        }
        // New child should inherit the effective freezer state of the current cgroup.
        new_child.state.lock().inherited_freezer_state = state.get_effective_freezer_state();
        state.children.insert_child(name.into(), new_child)
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

    fn get_status(&self) -> CgroupStatus {
        self.state.lock().get_status()
    }

    fn freeze(&self) {
        let mut state = self.state.lock();
        let inherited_freezer_state = state.inherited_freezer_state;
        state.propagate_freeze(inherited_freezer_state);
        state.self_freezer_state = FreezerState::Frozen;
    }

    fn thaw(&self) {
        let mut state = self.state.lock();
        state.self_freezer_state = FreezerState::Thawed;
        let inherited_freezer_state = state.inherited_freezer_state;
        state.propagate_thaw(inherited_freezer_state);
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
