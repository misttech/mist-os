// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! This file implements control group hierarchy.
//!
//! There is no support for actual resource constraints, or any operations outside of adding tasks
//! to a control group (for the duration of their lifetime).

use starnix_core::signals::{send_freeze_signal, SignalInfo};
use starnix_core::task::{Kernel, ThreadGroup, WaitQueue, Waiter};
use starnix_core::vfs::{FsStr, FsString, PathBuilder};
use starnix_logging::{log_warn, track_stub};
use starnix_sync::Mutex;
use starnix_types::ownership::{TempRef, WeakRef};
use starnix_uapi::errors::Errno;
use starnix_uapi::signals::SIGKILL;
use starnix_uapi::{errno, error, pid_t};
use std::collections::{btree_map, hash_map, BTreeMap, HashMap};
use std::ops::Deref;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Weak};

/// All cgroups of the kernel. There is a single cgroup v2 hierarchy, and one-or-more cgroup v1
/// hierarchies.
/// TODO(https://fxbug.dev/389748287): Add cgroup v1 hierarchies on the kernel.
pub struct Cgroups {
    pub cgroup2: Arc<CgroupRoot>,
}

impl Cgroups {
    pub fn new(kernel: Weak<Kernel>) -> Self {
        Self { cgroup2: CgroupRoot::new(kernel) }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum FreezerState {
    Thawed,
    Frozen,
}

impl Default for FreezerState {
    fn default() -> Self {
        FreezerState::Thawed
    }
}

impl std::fmt::Display for FreezerState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FreezerState::Frozen => write!(f, "1"),
            FreezerState::Thawed => write!(f, "0"),
        }
    }
}

#[derive(Default)]
pub struct CgroupFreezerState {
    /// Cgroups's own freezer state as set by the `cgroup.freeze` file.
    pub self_freezer_state: FreezerState,
    /// Considers both the cgroup's self freezer state as set by the `cgroup.freeze` file and
    /// the freezer state of its ancestors. A cgroup is considered frozen if either itself or any
    /// of its ancestors is frozen.
    pub effective_freezer_state: FreezerState,
}

/// Common operations of all cgroups.
pub trait CgroupOps: Send + Sync + 'static {
    /// Returns the unique ID of the cgroup. ID of root cgroup is 0.
    fn id(&self) -> u64;

    /// Add a process to a cgroup. Errors if the cgroup has been deleted.
    fn add_process(&self, pid: pid_t, thread_group: &TempRef<'_, ThreadGroup>)
        -> Result<(), Errno>;

    /// Create a new sub-cgroup as a child of this cgroup. Errors if the cgroup is deleted, or a
    /// child with `name` already exists.
    fn new_child(&self, name: &FsStr) -> Result<CgroupHandle, Errno>;

    /// Gets all children of this cgroup.
    fn get_children(&self) -> Result<Vec<CgroupHandle>, Errno>;

    /// Gets the child with `name`, errors if not found.
    fn get_child(&self, name: &FsStr) -> Result<CgroupHandle, Errno>;

    /// Remove a child from this cgroup and return it, if found. Errors if cgroup is deleted, or a
    /// child with `name` is not found.
    fn remove_child(&self, name: &FsStr) -> Result<CgroupHandle, Errno>;

    /// Return all pids that belong to this cgroup.
    fn get_pids(&self) -> Vec<pid_t>;

    /// Kills all processes in the cgroup and its descendants.
    fn kill(&self);

    /// Whether the cgroup or any of its descendants have any processes.
    fn is_populated(&self) -> bool;

    /// Get the freezer `self state` and `effective state`.
    fn get_freezer_state(&self) -> CgroupFreezerState;

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

    /// Weak reference to Kernel, used to get processes and tasks.
    kernel: Weak<Kernel>,

    /// Weak reference to self, used when creating child cgroups.
    weak_self: Weak<CgroupRoot>,

    /// Used to generate IDs for descendent Cgroups.
    next_id: AtomicU64,
}
impl CgroupRoot {
    pub fn new(kernel: Weak<Kernel>) -> Arc<CgroupRoot> {
        Arc::new_cyclic(|weak_self| Self {
            weak_self: weak_self.clone(),
            kernel,
            next_id: AtomicU64::new(1),
            ..Default::default()
        })
    }

    fn get_next_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    fn kernel(&self) -> Arc<Kernel> {
        self.kernel.upgrade().expect("kernel is available for cgroup operations")
    }

    pub fn get_cgroup(&self, pid: pid_t) -> Option<Weak<Cgroup>> {
        self.pid_table.lock().get(&pid).cloned()
    }

    pub fn inherit_cgroup(
        &self,
        parent_pid: pid_t,
        child_pid: pid_t,
        thread_group: &TempRef<'_, ThreadGroup>,
    ) {
        let mut pid_table = self.pid_table.lock();
        if let Some(cgroup) = pid_table.get(&parent_pid).cloned() {
            assert!(
                pid_table.insert(child_pid, cgroup.clone()).is_none(),
                "child pid should not exist when inheriting"
            );
            cgroup
                .upgrade()
                .expect("parent cgroup should not be deprecated")
                .state
                .lock()
                .add_process(child_pid, thread_group)
                .expect("cgroup should not be deleted");
        }
    }
}

impl CgroupOps for CgroupRoot {
    fn id(&self) -> u64 {
        0
    }

    fn add_process(
        &self,
        pid: pid_t,
        thread_group: &TempRef<'_, ThreadGroup>,
    ) -> Result<(), Errno> {
        let mut pid_table = self.pid_table.lock();
        match pid_table.entry(pid) {
            hash_map::Entry::Occupied(entry) => {
                // If pid is in a child cgroup, remove it.
                if let Some(cgroup) = entry.get().upgrade() {
                    cgroup.state.lock().remove_process(pid, thread_group)?;
                }
                entry.remove();
            }
            // If pid is not in a child cgroup, then it must be in the root cgroup already.
            // This does not throw an error on Linux, so just return success here.
            hash_map::Entry::Vacant(_) => {}
        }
        Ok(())
    }

    fn new_child(&self, name: &FsStr) -> Result<CgroupHandle, Errno> {
        let id = self.get_next_id();
        let new_child = Cgroup::new(id, name, &self.weak_self, None);
        let mut children = self.children.lock();
        children.insert_child(name.into(), new_child)
    }

    fn get_child(&self, name: &FsStr) -> Result<CgroupHandle, Errno> {
        let children = self.children.lock();
        children.get_child(name).ok_or_else(|| errno!(ENOENT))
    }

    fn remove_child(&self, name: &FsStr) -> Result<CgroupHandle, Errno> {
        let mut children = self.children.lock();
        children.remove_child(name)
    }

    fn get_children(&self) -> Result<Vec<CgroupHandle>, Errno> {
        let children = self.children.lock();
        Ok(children.get_children())
    }

    fn get_pids(&self) -> Vec<pid_t> {
        let kernel_pids = self.kernel().pids.read().process_ids();
        let controlled_pids = self.pid_table.lock();
        kernel_pids.into_iter().filter(|pid| !controlled_pids.contains_key(pid)).collect()
    }

    fn kill(&self) {
        unreachable!("Root cgroup cannot kill its processes.");
    }

    fn is_populated(&self) -> bool {
        false
    }

    fn get_freezer_state(&self) -> CgroupFreezerState {
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

    fn get_child(&self, name: &FsStr) -> Option<CgroupHandle> {
        self.0.get(name).cloned()
    }

    fn get_children(&self) -> Vec<CgroupHandle> {
        self.0.values().cloned().collect()
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
            let waiter = Waiter::new_ignoring_signals();
            self.wait_queue.wait_async(&waiter);
            send_freeze_signal(&task, waiter).expect("sending freeze signal should not fail");
        }
    }

    fn thaw_thread_group(&self, thread_group: &ThreadGroup) {
        // Create static-lifetime TempRefs of Tasks so that we avoid don't hold the ThreadGroup
        // lock while iterating and sending the signal.
        // SAFETY: static TempRefs are released after all signals are queued.
        let tasks = thread_group.read().tasks().map(TempRef::into_static).collect::<Vec<_>>();
        for task in tasks {
            task.write().thaw();
            task.interrupt();
        }
    }

    fn get_effective_freezer_state(&self) -> FreezerState {
        std::cmp::max(self.self_freezer_state, self.inherited_freezer_state)
    }

    fn add_process(
        &mut self,
        pid: pid_t,
        thread_group: &TempRef<'_, ThreadGroup>,
    ) -> Result<(), Errno> {
        if self.deleted {
            return error!(ENOENT);
        }
        self.processes.insert(pid, WeakRef::from(thread_group));

        if self.get_effective_freezer_state() == FreezerState::Frozen {
            self.freeze_thread_group(&thread_group);
        }
        Ok(())
    }

    fn remove_process(
        &mut self,
        pid: pid_t,
        thread_group: &TempRef<'_, ThreadGroup>,
    ) -> Result<(), Errno> {
        if self.deleted {
            return error!(ENOENT);
        }
        self.processes.remove(&pid);

        if self.get_effective_freezer_state() == FreezerState::Frozen {
            self.thaw_thread_group(thread_group);
        }
        Ok(())
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

    fn propagate_kill(&self) {
        for (_, thread_group) in self.processes.iter() {
            let Some(thread_group) = thread_group.upgrade() else {
                continue;
            };
            thread_group.write().send_signal(SignalInfo::default(SIGKILL));
        }

        // Recursively lock and kill children cgroups' processes.
        for child in self.children.get_children() {
            child.state.lock().propagate_kill();
        }
    }
}

/// `Cgroup` is a non-root cgroup in a cgroup hierarchy, and can have other `Cgroup`s as children.
pub struct Cgroup {
    root: Weak<CgroupRoot>,

    /// ID of the cgroup.
    id: u64,

    /// Name of the cgroup.
    name: FsString,

    /// Weak reference to its parent cgroup, `None` if direct descendent of the root cgroup.
    /// This field is useful in implementing features that only apply to non-root cgroups.
    parent: Option<Weak<Cgroup>>,

    /// Internal state of the Cgroup.
    state: Mutex<CgroupState>,

    weak_self: Weak<Cgroup>,
}
pub type CgroupHandle = Arc<Cgroup>;

/// Returns the path from the root to this `cgroup`.
pub fn path_from_root(weak_cgroup: Option<Weak<Cgroup>>) -> Result<FsString, Errno> {
    let cgroup = match weak_cgroup {
        Some(weak_cgroup) => Weak::upgrade(&weak_cgroup).ok_or_else(|| errno!(ENODEV))?,
        None => return Ok("/".into()),
    };
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
        id: u64,
        name: &FsStr,
        root: &Weak<CgroupRoot>,
        parent: Option<Weak<Cgroup>>,
    ) -> CgroupHandle {
        Arc::new_cyclic(|weak| Self {
            id,
            root: root.clone(),
            name: name.to_owned(),
            parent,
            state: Default::default(),
            weak_self: weak.clone(),
        })
    }

    pub fn name(&self) -> &FsStr {
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
}

impl CgroupOps for Cgroup {
    fn id(&self) -> u64 {
        self.id
    }

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
                    other_cgroup.state.lock().remove_process(pid, thread_group)?;
                }

                self.state.lock().add_process(pid, thread_group)?;
                entry.insert(self.weak_self.clone());
            }
            hash_map::Entry::Vacant(entry) => {
                self.state.lock().add_process(pid, thread_group)?;
                entry.insert(self.weak_self.clone());
            }
        }

        Ok(())
    }

    fn new_child(&self, name: &FsStr) -> Result<CgroupHandle, Errno> {
        let id = self.root()?.get_next_id();
        let new_child = Cgroup::new(id, name, &self.root, Some(self.weak_self.clone()));
        let mut state = self.state.lock();
        if state.deleted {
            return error!(ENOENT);
        }
        // New child should inherit the effective freezer state of the current cgroup.
        new_child.state.lock().inherited_freezer_state = state.get_effective_freezer_state();
        state.children.insert_child(name.into(), new_child)
    }

    fn get_child(&self, name: &FsStr) -> Result<CgroupHandle, Errno> {
        let state = self.state.lock();
        state.children.get_child(name).ok_or_else(|| errno!(ENOENT))
    }

    fn remove_child(&self, name: &FsStr) -> Result<CgroupHandle, Errno> {
        let mut state = self.state.lock();
        if state.deleted {
            return error!(ENOENT);
        }
        state.children.remove_child(name)
    }

    fn get_children(&self) -> Result<Vec<CgroupHandle>, Errno> {
        let state = self.state.lock();
        if state.deleted {
            return error!(ENOENT);
        }
        Ok(state.children.get_children())
    }

    fn get_pids(&self) -> Vec<pid_t> {
        let mut state = self.state.lock();
        state.update_processes();
        state.processes.keys().copied().collect()
    }

    fn kill(&self) {
        let state = self.state.lock();
        state.propagate_kill();
    }

    fn is_populated(&self) -> bool {
        let mut state = self.state.lock();
        if state.deleted {
            return false;
        }
        state.update_processes();
        if !state.processes.is_empty() {
            return true;
        }

        state.children.get_children().into_iter().any(|child| child.is_populated())
    }

    fn get_freezer_state(&self) -> CgroupFreezerState {
        let state = self.state.lock();
        CgroupFreezerState {
            self_freezer_state: state.self_freezer_state,
            effective_freezer_state: state.get_effective_freezer_state(),
        }
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
    use super::*;
    use starnix_core::testing::create_kernel_and_task;

    #[::fuchsia::test]
    async fn cgroup_path_from_root() {
        let (kernel, _current_task) = create_kernel_and_task();
        let root = CgroupRoot::new(Arc::downgrade(&kernel));

        let test_cgroup = root.new_child("test".into()).expect("new_child on root cgroup succeeds");
        let child_cgroup =
            test_cgroup.new_child("child".into()).expect("new_child on non-root cgroup succeeds");

        assert_eq!(path_from_root(Some(Arc::downgrade(&test_cgroup))), Ok("/test".into()));
        assert_eq!(path_from_root(Some(Arc::downgrade(&child_cgroup))), Ok("/test/child".into()));
    }
}
