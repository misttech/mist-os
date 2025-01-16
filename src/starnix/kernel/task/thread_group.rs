// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::device::terminal::{Terminal, TerminalController};
use crate::mutable_state::{state_accessor, state_implementation};
use crate::security;
use crate::signals::syscalls::{read_siginfo, WaitingOptions};
use crate::signals::{
    action_for_signal, send_standard_signal, DeliveryAction, QueuedSignals, SignalActions,
    SignalDetail, SignalInfo,
};
use crate::task::interval_timer::IntervalTimerHandle;
use crate::task::{
    ptrace_detach, AtomicStopState, ControllingTerminal, CurrentTask, ExitStatus, Kernel, PidTable,
    ProcessGroup, PtraceAllowedPtracers, PtraceEvent, PtraceOptions, PtraceStatus, Session,
    StopState, Task, TaskFlags, TaskMutableState, TaskPersistentInfo, TaskPersistentInfoState,
    TimerTable, WaitQueue, ZombiePtraces,
};
use itertools::Itertools;
use macro_rules_attribute::apply;
use starnix_lifecycle::{AtomicU64Counter, DropNotifier};
use starnix_logging::{log_error, track_stub};
use starnix_sync::{LockBefore, Locked, Mutex, MutexGuard, ProcessGroupState, RwLock, Unlocked};
use starnix_types::ownership::{OwnedRef, Releasable, TempRef, WeakRef};
use starnix_types::stats::TaskTimeStats;
use starnix_types::time::{itimerspec_from_itimerval, timeval_from_duration};
use starnix_uapi::auth::{Credentials, CAP_SYS_ADMIN, CAP_SYS_RESOURCE};
use starnix_uapi::errors::Errno;
use starnix_uapi::personality::PersonalityFlags;
use starnix_uapi::resource_limits::{Resource, ResourceLimits};
use starnix_uapi::signals::{Signal, UncheckedSignal, SIGCHLD, SIGCONT, SIGHUP, SIGKILL, SIGTTOU};
use starnix_uapi::user_address::UserAddress;
use starnix_uapi::{
    errno, error, itimerval, pid_t, rlimit, uid_t, ITIMER_PROF, ITIMER_REAL, ITIMER_VIRTUAL,
    SIG_IGN, SI_TKILL, SI_USER,
};
use std::collections::BTreeMap;
use std::fmt;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use zx::AsHandleRef;

/// The mutable state of the ThreadGroup.
pub struct ThreadGroupMutableState {
    /// The parent thread group.
    ///
    /// The value needs to be writable so that it can be re-parent to the correct subreaper if the
    /// parent ends before the child.
    pub parent: Option<ThreadGroupParent>,

    /// The tasks in the thread group.
    ///
    /// The references to Task is weak to prevent cycles as Task have a Arc reference to their
    /// thread group.
    /// It is still expected that these weak references are always valid, as tasks must unregister
    /// themselves before they are deleted.
    tasks: BTreeMap<pid_t, TaskContainer>,

    /// The children of this thread group.
    ///
    /// The references to ThreadGroup is weak to prevent cycles as ThreadGroup have a Arc reference
    /// to their parent.
    /// It is still expected that these weak references are always valid, as thread groups must unregister
    /// themselves before they are deleted.
    pub children: BTreeMap<pid_t, WeakRef<ThreadGroup>>,

    /// Child tasks that have exited, but not yet been waited for.
    pub zombie_children: Vec<OwnedRef<ZombieProcess>>,

    /// ptracees of this process that have exited, but not yet been waited for.
    pub zombie_ptracees: ZombiePtraces,

    // Child tasks that have exited, but the zombie ptrace needs to be consumed
    // before they can be waited for.  (pid_t, pid_t) is the original tracer and
    // tracee, so the tracer can be updated with a reaper if this thread group
    // exits.
    pub deferred_zombie_ptracers: Vec<(pid_t, pid_t)>,

    /// WaitQueue for updates to the WaitResults of tasks in this group.
    pub child_status_waiters: WaitQueue,

    /// Whether this thread group will inherit from children of dying processes in its descendant
    /// tree.
    pub is_child_subreaper: bool,

    /// The IDs used to perform shell job control.
    pub process_group: Arc<ProcessGroup>,

    /// The timers for this thread group (from timer_create(), etc.).
    pub timers: TimerTable,

    pub did_exec: bool,

    /// Wait queue for updates to `stopped`.
    pub stopped_waiters: WaitQueue,

    /// A signal that indicates whether the process is going to become waitable
    /// via waitid and waitpid for either WSTOPPED or WCONTINUED, depending on
    /// the value of `stopped`. If not None, contains the SignalInfo to return.
    pub last_signal: Option<SignalInfo>,

    pub leader_exit_info: Option<ProcessExitInfo>,

    pub terminating: bool,

    /// Time statistics accumulated from the children.
    pub children_time_stats: TaskTimeStats,

    /// Personality flags set with `sys_personality()`.
    pub personality: PersonalityFlags,

    /// Thread groups allowed to trace tasks in this this thread group.
    pub allowed_ptracers: PtraceAllowedPtracers,
}

/// A collection of `Task` objects that roughly correspond to a "process".
///
/// Userspace programmers often think about "threads" and "process", but those concepts have no
/// clear analogs inside the kernel because tasks are typically created using `clone(2)`, which
/// takes a complex set of flags that describes how much state is shared between the original task
/// and the new task.
///
/// If a new task is created with the `CLONE_THREAD` flag, the new task will be placed in the same
/// `ThreadGroup` as the original task. Userspace typically uses this flag in conjunction with the
/// `CLONE_FILES`, `CLONE_VM`, and `CLONE_FS`, which corresponds to the userspace notion of a
/// "thread". For example, that's how `pthread_create` behaves. In that sense, a `ThreadGroup`
/// normally corresponds to the set of "threads" in a "process". However, this pattern is purely a
/// userspace convention, and nothing stops userspace from using `CLONE_THREAD` without
/// `CLONE_FILES`, for example.
///
/// In Starnix, a `ThreadGroup` corresponds to a Zircon process, which means we do not support the
/// `CLONE_THREAD` flag without the `CLONE_VM` flag. If we run into problems with this limitation,
/// we might need to revise this correspondence.
///
/// Each `Task` in a `ThreadGroup` has the same thread group ID (`tgid`). The task with the same
/// `pid` as the `tgid` is called the thread group leader.
///
/// Thread groups are destroyed when the last task in the group exits.
pub struct ThreadGroup {
    /// Weak reference to the `OwnedRef` of this `ThreadGroup`. This allows to retrieve the
    /// `TempRef` from a raw `ThreadGroup`.
    pub weak_thread_group: WeakRef<ThreadGroup>,

    /// The kernel to which this thread group belongs.
    pub kernel: Arc<Kernel>,

    /// A handle to the underlying Zircon process object.
    ///
    /// Currently, we have a 1-to-1 mapping between thread groups and zx::process
    /// objects. This approach might break down if/when we implement CLONE_VM
    /// without CLONE_THREAD because that creates a situation where two thread
    /// groups share an address space. To implement that situation, we might
    /// need to break the 1-to-1 mapping between thread groups and zx::process
    /// or teach zx::process to share address spaces.
    pub process: zx::Process,

    /// The lead task of this thread group.
    ///
    /// The lead task is typically the initial thread created in the thread group.
    pub leader: pid_t,

    /// The signal actions that are registered for this process.
    pub signal_actions: Arc<SignalActions>,

    /// A mechanism to be notified when this `ThreadGroup` is destroyed.
    pub drop_notifier: DropNotifier,

    /// Whether the process is currently stopped.
    ///
    /// Must only be set when the `mutable_state` write lock is held.
    stop_state: AtomicStopState,

    /// The mutable state of the ThreadGroup.
    mutable_state: RwLock<ThreadGroupMutableState>,

    /// The resource limits for this thread group.  This is outside mutable_state
    /// to avoid deadlocks where the thread_group lock is held when acquiring
    /// the task lock, and vice versa.
    pub limits: Mutex<ResourceLimits>,

    /// The next unique identifier for a seccomp filter.  These are required to be
    /// able to distinguish identical seccomp filters, which are treated differently
    /// for the purposes of SECCOMP_FILTER_FLAG_TSYNC.  Inherited across clone because
    /// seccomp filters are also inherited across clone.
    pub next_seccomp_filter_id: AtomicU64Counter,

    /// Tasks ptraced by this process
    pub ptracees: Mutex<BTreeMap<pid_t, TaskContainer>>,

    /// The signals that are currently pending for this thread group.
    pub pending_signals: Mutex<QueuedSignals>,

    /// The monotonic time at which the thread group started.
    pub start_time: zx::MonotonicInstant,
}

impl fmt::Debug for ThreadGroup {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}({})",
            self.process.get_name().unwrap_or(zx::Name::new_lossy("<unknown>")),
            self.leader
        )
    }
}

impl PartialEq for ThreadGroup {
    fn eq(&self, other: &Self) -> bool {
        self.leader == other.leader
    }
}

impl Releasable for ThreadGroup {
    type Context<'a: 'b, 'b> = ();

    fn release<'a: 'b, 'b>(mut self, _context: Self::Context<'a, 'b>) {
        let mut pids = self.kernel.pids.write();
        let state = self.mutable_state.get_mut();

        for zombie in state.zombie_children.drain(..) {
            zombie.release(&mut pids);
        }

        state.zombie_ptracees.release(&mut pids);
    }
}

#[cfg(any(test, debug_assertions))]
impl Drop for ThreadGroup {
    fn drop(&mut self) {
        let state = self.mutable_state.get_mut();
        assert!(state.tasks.is_empty());
        assert!(state.children.is_empty());
    }
}

/// A wrapper around a `WeakRef<ThreadGroup>` that expects the underlying `WeakRef` to always be
/// valid. The wrapper will check this at runtime during creation and upgrade.
pub struct ThreadGroupParent(WeakRef<ThreadGroup>);

impl ThreadGroupParent {
    pub fn new(t: WeakRef<ThreadGroup>) -> Self {
        debug_assert!(t.upgrade().is_some());
        Self(t)
    }

    pub fn upgrade<'a>(&'a self) -> TempRef<'a, ThreadGroup> {
        self.0.upgrade().expect("ThreadGroupParent references must always be valid")
    }
}

impl Clone for ThreadGroupParent {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<I: Into<WeakRef<ThreadGroup>>> From<I> for ThreadGroupParent {
    fn from(r: I) -> Self {
        Self::new(r.into())
    }
}

/// A selector that can match a process. Works as a representation of the pid argument to syscalls
/// like wait and kill.
#[derive(Debug, Clone, Copy)]
pub enum ProcessSelector {
    /// Matches any process at all.
    Any,
    /// Matches only the process with the specified pid
    Pid(pid_t),
    /// Matches all the processes in the given process group
    Pgid(pid_t),
}

impl ProcessSelector {
    pub fn do_match(&self, pid: pid_t, pid_table: &PidTable) -> bool {
        match *self {
            ProcessSelector::Pid(p) => pid == p,
            ProcessSelector::Any => true,
            ProcessSelector::Pgid(pgid) => {
                if let Some(task_ref) = pid_table.get_task(pid).upgrade() {
                    pid_table.get_process_group(pgid).as_ref()
                        == Some(&task_ref.thread_group.read().process_group)
                } else {
                    false
                }
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessExitInfo {
    pub status: ExitStatus,
    pub exit_signal: Option<Signal>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WaitResult {
    pub pid: pid_t,
    pub uid: uid_t,

    pub exit_info: ProcessExitInfo,

    /// Cumulative time stats for the process and its children.
    pub time_stats: TaskTimeStats,
}

impl WaitResult {
    // According to wait(2) man page, SignalInfo.signal needs to always be set to SIGCHLD
    pub fn as_signal_info(&self) -> SignalInfo {
        SignalInfo::new(
            SIGCHLD,
            self.exit_info.status.signal_info_code(),
            SignalDetail::SIGCHLD {
                pid: self.pid,
                uid: self.uid,
                status: self.exit_info.status.signal_info_status(),
            },
        )
    }
}

#[derive(Debug)]
pub struct ZombieProcess {
    pub pid: pid_t,
    pub pgid: pid_t,
    pub uid: uid_t,

    pub exit_info: ProcessExitInfo,

    /// Cumulative time stats for the process and its children.
    pub time_stats: TaskTimeStats,

    /// Whether dropping this ZombieProcess should imply removing the pid from
    /// the PidTable
    pub is_canonical: bool,
}

impl PartialEq for ZombieProcess {
    fn eq(&self, other: &Self) -> bool {
        // We assume only one set of ZombieProcess data per process, so this should cover it.
        self.pid == other.pid
            && self.pgid == other.pgid
            && self.uid == other.uid
            && self.is_canonical == other.is_canonical
    }
}

impl Eq for ZombieProcess {}

impl PartialOrd for ZombieProcess {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        // pid by itself seems good enough.
        self.pid.partial_cmp(&other.pid)
    }
}

impl Ord for ZombieProcess {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.pid.cmp(&other.pid)
    }
}

impl ZombieProcess {
    pub fn new(
        thread_group: ThreadGroupStateRef<'_>,
        credentials: &Credentials,
        exit_info: ProcessExitInfo,
    ) -> OwnedRef<Self> {
        let time_stats = thread_group.base.time_stats() + thread_group.children_time_stats;
        OwnedRef::new(ZombieProcess {
            pid: thread_group.base.leader,
            pgid: thread_group.process_group.leader,
            uid: credentials.uid,
            exit_info,
            time_stats,
            is_canonical: true,
        })
    }

    pub fn to_wait_result(&self) -> WaitResult {
        WaitResult {
            pid: self.pid,
            uid: self.uid,
            exit_info: self.exit_info.clone(),
            time_stats: self.time_stats,
        }
    }

    pub fn copy_for_key(&self) -> Self {
        ZombieProcess {
            pid: self.pid,
            pgid: self.pgid,
            uid: self.uid,
            exit_info: self.exit_info.clone(),
            time_stats: self.time_stats,
            is_canonical: false,
        }
    }
}

impl Releasable for ZombieProcess {
    type Context<'a: 'b, 'b> = &'a mut PidTable;

    fn release<'a: 'b, 'b>(self, pids: &mut PidTable) {
        if self.is_canonical {
            pids.remove_zombie(self.pid);
        }
    }
}

impl ThreadGroup {
    pub fn new<L>(
        locked: &mut Locked<'_, L>,
        kernel: Arc<Kernel>,
        process: zx::Process,
        parent: Option<ThreadGroupWriteGuard<'_>>,
        leader: pid_t,
        process_group: Arc<ProcessGroup>,
        signal_actions: Arc<SignalActions>,
    ) -> OwnedRef<ThreadGroup>
    where
        L: LockBefore<ProcessGroupState>,
    {
        OwnedRef::new_cyclic(|weak_thread_group| {
            let mut thread_group = ThreadGroup {
                weak_thread_group: weak_thread_group.clone(),
                kernel,
                process,
                leader,
                signal_actions,
                drop_notifier: Default::default(),
                // A child process created via fork(2) inherits its parent's
                // resource limits.  Resource limits are preserved across execve(2).
                limits: Mutex::new(
                    parent
                        .as_ref()
                        .map(|p| p.base.limits.lock().clone())
                        .unwrap_or(Default::default()),
                ),
                next_seccomp_filter_id: Default::default(),
                ptracees: Default::default(),
                stop_state: AtomicStopState::new(StopState::Awake),
                pending_signals: Default::default(),
                start_time: zx::MonotonicInstant::get(),
                mutable_state: RwLock::new(ThreadGroupMutableState {
                    parent: parent
                        .as_ref()
                        .map(|p| ThreadGroupParent::from(p.base.weak_thread_group.clone())),
                    tasks: BTreeMap::new(),
                    children: BTreeMap::new(),
                    zombie_children: vec![],
                    zombie_ptracees: ZombiePtraces::new(),
                    deferred_zombie_ptracers: vec![],
                    child_status_waiters: WaitQueue::default(),
                    is_child_subreaper: false,
                    process_group: Arc::clone(&process_group),
                    timers: Default::default(),
                    did_exec: false,
                    stopped_waiters: WaitQueue::default(),
                    last_signal: None,
                    leader_exit_info: None,
                    terminating: false,
                    children_time_stats: Default::default(),
                    personality: parent
                        .as_ref()
                        .map(|p| p.personality)
                        .unwrap_or(Default::default()),
                    allowed_ptracers: PtraceAllowedPtracers::None,
                }),
            };

            if let Some(mut parent) = parent {
                thread_group.next_seccomp_filter_id.reset(parent.base.next_seccomp_filter_id.get());
                parent.children.insert(leader, weak_thread_group);
                process_group.insert(locked, &thread_group);
            };
            thread_group
        })
    }

    state_accessor!(ThreadGroup, mutable_state);

    pub fn load_stopped(&self) -> StopState {
        self.stop_state.load(Ordering::Relaxed)
    }

    // Causes the thread group to exit.  If this is being called from a task
    // that is part of the current thread group, the caller should pass
    // `current_task`.  If ownership issues prevent passing `current_task`, then
    // callers should use CurrentTask::thread_group_exit instead.
    pub fn exit(
        &self,
        locked: &mut Locked<'_, Unlocked>,
        exit_status: ExitStatus,
        mut current_task: Option<&mut CurrentTask>,
    ) {
        if let Some(ref mut current_task) = current_task {
            current_task.ptrace_event(
                locked,
                PtraceOptions::TRACEEXIT,
                exit_status.signal_info_status() as u64,
            );
        }
        let mut pids = self.kernel.pids.write();
        let mut state = self.write();
        if state.terminating {
            // The thread group is already terminating and all threads in the thread group have
            // already been interrupted.
            return;
        }
        state.terminating = true;

        // Drop ptrace zombies
        state.zombie_ptracees.release(&mut pids);

        // Interrupt each task. Unlock the group because send_signal will lock the group in order
        // to call set_stopped.
        // SAFETY: tasks is kept on the stack. The static is required to ensure the lock on
        // ThreadGroup can be dropped.
        let tasks = state.tasks().map(TempRef::into_static).collect::<Vec<_>>();
        drop(state);

        // Detach from any ptraced tasks.
        let tracees = self.ptracees.lock().keys().cloned().collect::<Vec<_>>();
        for tracee in tracees {
            if let Some(task_ref) = pids.get_task(tracee).clone().upgrade() {
                let _ = ptrace_detach(self, task_ref.as_ref(), &UserAddress::NULL);
            }
        }

        for task in tasks {
            task.write().set_exit_status(exit_status.clone());
            send_standard_signal(&task, SignalInfo::default(SIGKILL));
        }
    }

    pub fn add(&self, task: &TempRef<'_, Task>) -> Result<(), Errno> {
        let mut state = self.write();
        if state.terminating {
            return error!(EINVAL);
        }
        state.tasks.insert(task.id, task.into());
        Ok(())
    }

    pub fn remove<L>(&self, locked: &mut Locked<'_, L>, task: &OwnedRef<Task>)
    where
        L: LockBefore<ProcessGroupState>,
    {
        let mut pids = self.kernel.pids.write();

        task.set_ptrace_zombie(&pids);
        pids.remove_task(task.id);

        let mut state = self.write();

        let persistent_info: TaskPersistentInfo =
            if let Some(container) = state.tasks.remove(&task.id) {
                container.into()
            } else {
                // The task has never been added. The only expected case is that this thread was
                // already terminating.
                debug_assert!(state.terminating);
                return;
            };

        if task.id == self.leader {
            let exit_status = task.exit_status().unwrap_or_else(|| {
                log_error!("Exiting without an exit code.");
                ExitStatus::Exit(u8::MAX)
            });
            state.leader_exit_info = Some(ProcessExitInfo {
                status: exit_status,
                exit_signal: *persistent_info.lock().exit_signal(),
            });
        }

        if state.tasks.is_empty() {
            state.terminating = true;

            // Replace PID table entry with a zombie.
            let exit_info =
                state.leader_exit_info.take().expect("Failed to capture leader exit status");
            let zombie =
                ZombieProcess::new(state.as_ref(), persistent_info.lock().creds(), exit_info);
            pids.kill_process(self.leader, OwnedRef::downgrade(&zombie));

            state.leave_process_group(locked, &mut pids);

            // I have no idea if dropping the lock here is correct, and I don't want to think about
            // it. If problems do turn up with another thread observing an intermediate state of
            // this exit operation, the solution is to unify locks. It should be sensible and
            // possible for there to be a single lock that protects all (or nearly all) of the
            // data accessed by both exit and wait. In gvisor and linux this is the lock on the
            // equivalent of the PidTable. This is made more difficult by rust locks being
            // containers that only lock the data they contain, but see
            // https://docs.google.com/document/d/1YHrhBqNhU1WcrsYgGAu3JwwlVmFXPlwWHTJLAbwRebY/edit
            // for an idea.
            std::mem::drop(state);

            // We will need the immediate parent and the reaper. Once we have them, we can make
            // sure to take the locks in the right order: parent before child.
            let parent = self.read().parent.clone();
            let reaper = self.find_reaper();

            {
                // Reparent the children.
                if let Some(reaper) = reaper {
                    let reaper = reaper.upgrade();
                    {
                        let mut reaper_state = reaper.write();
                        let mut state = self.write();
                        for (_pid, weak_child) in std::mem::take(&mut state.children) {
                            if let Some(child) = weak_child.upgrade() {
                                let mut child_state = child.write();
                                child_state.parent = Some(ThreadGroupParent::from(&reaper));
                                reaper_state.children.insert(child.leader, weak_child.clone());
                            }
                        }
                        reaper_state.zombie_children.append(&mut state.zombie_children);
                    }
                    ZombiePtraces::reparent(&pids, self, &reaper);
                } else {
                    // If we don't have a reaper then just drop the zombies.
                    let mut state = self.write();
                    for zombie in state.zombie_children.drain(..) {
                        zombie.release(&mut pids);
                    }
                }
            }

            if let Some(ref parent) = parent {
                let parent = parent.upgrade();
                let mut tracer_pid = None;
                if let Some(ref ptrace) = &task.read().ptrace {
                    tracer_pid = Some(ptrace.get_pid());
                }

                let mut maybe_zombie = Some(zombie);
                if let Some(tracer_pid) = tracer_pid {
                    if let Some(ref tracer) = pids.get_task(tracer_pid).upgrade() {
                        maybe_zombie = tracer.thread_group.maybe_notify_tracer(
                            task,
                            &mut pids,
                            &parent,
                            maybe_zombie.unwrap(),
                        );
                    }
                }
                if let Some(zombie) = maybe_zombie {
                    parent.do_zombie_notifications(zombie);
                }
            } else {
                zombie.release(&mut pids);
            }

            // TODO: Set the error_code on the Zircon process object. Currently missing a way
            // to do this in Zircon. Might be easier in the new execution model.

            // Once the last zircon thread stops, the zircon process will also stop executing.

            if let Some(parent) = parent {
                let parent = parent.upgrade();
                parent.check_orphans(locked);
            }
        }
    }

    pub fn do_zombie_notifications(&self, zombie: OwnedRef<ZombieProcess>) {
        let mut state = self.write();

        state.children.remove(&zombie.pid);
        state.deferred_zombie_ptracers.retain(|&(_, tracee)| tracee != zombie.pid);

        let exit_signal = zombie.exit_info.exit_signal;
        let mut signal_info = zombie.to_wait_result().as_signal_info();

        state.zombie_children.push(zombie);
        state.child_status_waiters.notify_all();

        // Send signals
        if let Some(exit_signal) = exit_signal {
            signal_info.signal = exit_signal;
            state.send_signal(signal_info);
        }
    }

    /// Notifies the tracer if appropriate.  Returns Some(zombie) if caller
    /// needs to notify the parent, None otherwise.  The caller should probably
    /// invoke parent.do_zombie_notifications(zombie) on the result.
    fn maybe_notify_tracer(
        &self,
        tracee: &Task,
        mut pids: &mut PidTable,
        parent: &ThreadGroup,
        zombie: OwnedRef<ZombieProcess>,
    ) -> Option<OwnedRef<ZombieProcess>> {
        if self.read().zombie_ptracees.has_match(&ProcessSelector::Pid(tracee.id), &pids) {
            if self == parent {
                // The tracer is the parent and has not consumed the
                // notification.  Don't bother with the ptracee stuff, and just
                // notify the parent.
                self.write().zombie_ptracees.retain(&mut |z: &ZombieProcess| z.pid != tracee.id);
                return Some(zombie);
            } else {
                // The tracer is not the parent and the tracer has not consumed
                // the notification.
                {
                    // Tell the parent to expect a notification later.
                    let mut parent_state = parent.write();
                    parent_state.deferred_zombie_ptracers.push((self.leader, tracee.id));
                    parent_state.children.remove(&tracee.thread_group.leader);
                }
                // Tell the tracer that there is a notification pending.
                let mut state = self.write();
                state.zombie_ptracees.set_parent_of(tracee.id, Some(zombie), parent);
                tracee.write().notify_ptracers();
                return None;
            }
        } else if self == parent {
            // The tracer is the parent and has already consumed the parent
            // notification.  No further action required.
            parent.write().children.remove(&tracee.id);
            zombie.release(&mut pids);
            return None;
        }
        // The tracer is not the parent and has already consumed the parent
        // notification.  Notify the parent.
        Some(zombie)
    }

    /// Find the task which will adopt our children after we die.
    fn find_reaper(&self) -> Option<ThreadGroupParent> {
        let mut weak_parent = self.read().parent.clone()?;
        loop {
            weak_parent = {
                let parent = weak_parent.upgrade();
                let parent_state = parent.read();
                if parent_state.is_child_subreaper {
                    break;
                }
                match parent_state.parent {
                    Some(ref next_parent) => next_parent.clone(),
                    None => break,
                }
            };
        }
        Some(weak_parent)
    }

    pub fn setsid<L>(&self, locked: &mut Locked<'_, L>) -> Result<(), Errno>
    where
        L: LockBefore<ProcessGroupState>,
    {
        {
            let mut pids = self.kernel.pids.write();
            if pids.get_process_group(self.leader).is_some() {
                return error!(EPERM);
            }
            let process_group = ProcessGroup::new(self.leader, None);
            pids.add_process_group(&process_group);
            self.write().set_process_group(locked, process_group, &mut pids);
        }
        self.check_orphans(locked);

        Ok(())
    }

    pub fn setpgid<L>(
        &self,
        locked: &mut Locked<'_, L>,
        target: &Task,
        pgid: pid_t,
    ) -> Result<(), Errno>
    where
        L: LockBefore<ProcessGroupState>,
    {
        {
            let mut pids = self.kernel.pids.write();

            let current_process_group = Arc::clone(&self.read().process_group);

            // The target process must be either the current process of a child of the current process
            let mut target_thread_group = target.thread_group.write();
            let is_target_current_process_child =
                target_thread_group.parent.as_ref().map(|tg| tg.upgrade().leader)
                    == Some(self.leader);
            if target_thread_group.leader() != self.leader && !is_target_current_process_child {
                return error!(ESRCH);
            }

            // If the target process is a child of the current task, it must not have executed one of the exec
            // function.
            if is_target_current_process_child && target_thread_group.did_exec {
                return error!(EACCES);
            }

            let new_process_group;
            {
                let target_process_group = &target_thread_group.process_group;

                // The target process must not be a session leader and must be in the same session as the current process.
                if target_thread_group.leader() == target_process_group.session.leader
                    || current_process_group.session != target_process_group.session
                {
                    return error!(EPERM);
                }

                let target_pgid = if pgid == 0 { target_thread_group.leader() } else { pgid };
                if target_pgid < 0 {
                    return error!(EINVAL);
                }

                if target_pgid == target_process_group.leader {
                    return Ok(());
                }

                // If pgid is not equal to the target process id, the associated process group must exist
                // and be in the same session as the target process.
                if target_pgid != target_thread_group.leader() {
                    new_process_group =
                        pids.get_process_group(target_pgid).ok_or_else(|| errno!(EPERM))?;
                    if new_process_group.session != target_process_group.session {
                        return error!(EPERM);
                    }
                } else {
                    // Create a new process group
                    new_process_group =
                        ProcessGroup::new(target_pgid, Some(target_process_group.session.clone()));
                    pids.add_process_group(&new_process_group);
                }
            }

            target_thread_group.set_process_group(locked, new_process_group, &mut pids);
        }
        target.thread_group.check_orphans(locked);

        Ok(())
    }

    fn itimer_real(&self) -> IntervalTimerHandle {
        self.write().timers.itimer_real()
    }

    pub fn set_itimer(
        &self,
        current_task: &CurrentTask,
        which: u32,
        value: itimerval,
    ) -> Result<itimerval, Errno> {
        if which == ITIMER_PROF || which == ITIMER_VIRTUAL {
            // We don't support setting these timers.
            // The gvisor test suite clears ITIMER_PROF as part of its test setup logic, so we support
            // clearing these values.
            if value.it_value.tv_sec == 0 && value.it_value.tv_usec == 0 {
                return Ok(itimerval::default());
            }
            track_stub!(TODO("https://fxbug.dev/322874521"), "Unsupported itimer type", which);
            return Err(errno!(ENOTSUP));
        }

        if which != ITIMER_REAL {
            return Err(errno!(EINVAL));
        }
        let itimer_real = self.itimer_real();
        let prev_remaining = itimer_real.time_remaining();
        if value.it_value.tv_sec != 0 || value.it_value.tv_usec != 0 {
            itimer_real.arm(current_task, itimerspec_from_itimerval(value), false)?;
        } else {
            itimer_real.disarm(current_task)?;
        }
        Ok(itimerval {
            it_value: timeval_from_duration(prev_remaining.remainder),
            it_interval: timeval_from_duration(prev_remaining.interval),
        })
    }

    pub fn get_itimer(&self, which: u32) -> Result<itimerval, Errno> {
        if which == ITIMER_PROF || which == ITIMER_VIRTUAL {
            // We don't support setting these timers, so we can accurately report that these are not set.
            return Ok(itimerval::default());
        }
        if which != ITIMER_REAL {
            return Err(errno!(EINVAL));
        }
        let remaining = self.itimer_real().time_remaining();
        Ok(itimerval {
            it_value: timeval_from_duration(remaining.remainder),
            it_interval: timeval_from_duration(remaining.interval),
        })
    }

    /// Check whether the stop state is compatible with `new_stopped`. If it is return it,
    /// otherwise, return None.
    fn check_stopped_state(
        &self,
        new_stopped: StopState,
        finalize_only: bool,
    ) -> Option<StopState> {
        let stopped = self.load_stopped();
        if finalize_only && !stopped.is_stopping_or_stopped() {
            return Some(stopped);
        }

        if stopped.is_illegal_transition(new_stopped) {
            return Some(stopped);
        }

        return None;
    }

    /// Set the stop status of the process.  If you pass |siginfo| of |None|,
    /// does not update the signal.  If |finalize_only| is set, will check that
    /// the set will be a finalize (Stopping -> Stopped or Stopped -> Stopped)
    /// before executing it.
    ///
    /// Returns the latest stop state after any changes.
    pub fn set_stopped(
        &self,
        new_stopped: StopState,
        siginfo: Option<SignalInfo>,
        finalize_only: bool,
    ) -> StopState {
        // Perform an early return check to see if we can avoid taking the lock.
        if let Some(stopped) = self.check_stopped_state(new_stopped, finalize_only) {
            return stopped;
        }

        self.write().set_stopped(new_stopped, siginfo, finalize_only)
    }

    /// Ensures |session| is the controlling session inside of |terminal_controller|, and returns a
    /// reference to the |TerminalController|.
    fn check_terminal_controller(
        session: &Arc<Session>,
        terminal_controller: &Option<TerminalController>,
    ) -> Result<(), Errno> {
        if let Some(terminal_controller) = terminal_controller {
            if let Some(terminal_session) = terminal_controller.session.upgrade() {
                if Arc::ptr_eq(session, &terminal_session) {
                    return Ok(());
                }
            }
        }
        error!(ENOTTY)
    }

    pub fn get_foreground_process_group(&self, terminal: &Arc<Terminal>) -> Result<pid_t, Errno> {
        let state = self.read();
        let process_group = &state.process_group;
        let terminal_state = terminal.read();

        // "When fd does not refer to the controlling terminal of the calling
        // process, -1 is returned" - tcgetpgrp(3)
        Self::check_terminal_controller(&process_group.session, &terminal_state.controller)?;
        let pid = process_group.session.read().get_foreground_process_group_leader();
        Ok(pid)
    }

    pub fn set_foreground_process_group<L>(
        &self,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
        terminal: &Arc<Terminal>,
        pgid: pid_t,
    ) -> Result<(), Errno>
    where
        L: LockBefore<ProcessGroupState>,
    {
        let process_group;
        let send_ttou;
        {
            // Keep locks to ensure atomicity.
            let pids = self.kernel.pids.read();
            let state = self.read();
            process_group = Arc::clone(&state.process_group);
            let terminal_state = terminal.read();
            Self::check_terminal_controller(&process_group.session, &terminal_state.controller)?;

            // pgid must be positive.
            if pgid < 0 {
                return error!(EINVAL);
            }

            let new_process_group = pids.get_process_group(pgid).ok_or_else(|| errno!(ESRCH))?;
            if new_process_group.session != process_group.session {
                return error!(EPERM);
            }

            let mut session_state = process_group.session.write();
            // If the calling process is a member of a background group and not ignoring SIGTTOU, a
            // SIGTTOU signal is sent to all members of this background process group.
            send_ttou = process_group.leader != session_state.get_foreground_process_group_leader()
                && !current_task.read().signal_mask().has_signal(SIGTTOU)
                && self.signal_actions.get(SIGTTOU).sa_handler != SIG_IGN;

            if !send_ttou {
                session_state.set_foreground_process_group(&new_process_group);
            }
        }

        // Locks must not be held when sending signals.
        if send_ttou {
            process_group.send_signals(locked, &[SIGTTOU]);
            return error!(EINTR);
        }

        Ok(())
    }

    pub fn set_controlling_terminal(
        &self,
        current_task: &CurrentTask,
        terminal: &Arc<Terminal>,
        is_main: bool,
        steal: bool,
        is_readable: bool,
    ) -> Result<(), Errno> {
        // Keep locks to ensure atomicity.
        let state = self.read();
        let process_group = &state.process_group;
        let mut terminal_state = terminal.write();
        let mut session_writer = process_group.session.write();

        // "The calling process must be a session leader and not have a
        // controlling terminal already." - tty_ioctl(4)
        if process_group.session.leader != self.leader
            || session_writer.controlling_terminal.is_some()
        {
            return error!(EINVAL);
        }

        let has_admin = current_task.creds().has_capability(CAP_SYS_ADMIN);

        // "If this terminal is already the controlling terminal of a different
        // session group, then the ioctl fails with EPERM, unless the caller
        // has the CAP_SYS_ADMIN capability and arg equals 1, in which case the
        // terminal is stolen, and all processes that had it as controlling
        // terminal lose it." - tty_ioctl(4)
        if let Some(other_session) =
            terminal_state.controller.as_ref().and_then(|cs| cs.session.upgrade())
        {
            if other_session != process_group.session {
                if !has_admin || !steal {
                    return error!(EPERM);
                }

                // Steal the TTY away. Unlike TIOCNOTTY, don't send signals.
                other_session.write().controlling_terminal = None;
            }
        }

        if !is_readable && !has_admin {
            return error!(EPERM);
        }

        session_writer.controlling_terminal =
            Some(ControllingTerminal::new(terminal.clone(), is_main));
        terminal_state.controller = TerminalController::new(&process_group.session);
        Ok(())
    }

    pub fn release_controlling_terminal<L>(
        &self,
        locked: &mut Locked<'_, L>,
        _current_task: &CurrentTask,
        terminal: &Arc<Terminal>,
        is_main: bool,
    ) -> Result<(), Errno>
    where
        L: LockBefore<ProcessGroupState>,
    {
        let process_group;
        {
            // Keep locks to ensure atomicity.
            let state = self.read();
            process_group = Arc::clone(&state.process_group);
            let mut terminal_state = terminal.write();
            let mut session_writer = process_group.session.write();

            // tty must be the controlling terminal.
            Self::check_terminal_controller(&process_group.session, &terminal_state.controller)?;
            if !session_writer
                .controlling_terminal
                .as_ref()
                .map_or(false, |ct| ct.matches(terminal, is_main))
            {
                return error!(ENOTTY);
            }

            // "If the process was session leader, then send SIGHUP and SIGCONT to the foreground
            // process group and all processes in the current session lose their controlling terminal."
            // - tty_ioctl(4)

            // Remove tty as the controlling tty for each process in the session, then
            // send them SIGHUP and SIGCONT.

            session_writer.controlling_terminal = None;
            terminal_state.controller = None;
        }

        if process_group.session.leader == self.leader {
            process_group.send_signals(locked, &[SIGHUP, SIGCONT]);
        }

        Ok(())
    }

    fn check_orphans<L>(&self, locked: &mut Locked<'_, L>)
    where
        L: LockBefore<ProcessGroupState>,
    {
        let mut thread_groups =
            self.read().children().map(TempRef::into_static).collect::<Vec<_>>();
        let this = self.weak_thread_group.upgrade().unwrap();
        thread_groups.push(this);
        let process_groups =
            thread_groups.iter().map(|tg| Arc::clone(&tg.read().process_group)).unique();
        for pg in process_groups {
            pg.check_orphaned(locked);
        }
    }

    pub fn get_rlimit(&self, resource: Resource) -> u64 {
        self.limits.lock().get(resource).rlim_cur
    }

    pub fn adjust_rlimits(
        &self,
        current_task: &CurrentTask,
        resource: Resource,
        maybe_new_limit: Option<rlimit>,
    ) -> Result<rlimit, Errno> {
        let can_increase_rlimit = current_task.creds().has_capability(CAP_SYS_RESOURCE);
        let mut limit_state = self.limits.lock();
        limit_state.get_and_set(resource, maybe_new_limit, can_increase_rlimit)
    }

    pub fn time_stats(&self) -> TaskTimeStats {
        let process: &zx::Process = if zx::AsHandleRef::as_handle_ref(&self.process).is_invalid() {
            // `process` must be valid for all tasks, except `kthreads`. In that case get the
            // stats from starnix process.
            assert_eq!(
                self as *const ThreadGroup,
                TempRef::as_ptr(&self.kernel.kthreads.system_thread_group())
            );
            &self.kernel.kthreads.starnix_process
        } else {
            &self.process
        };

        let info =
            zx::Task::get_runtime_info(process).expect("Failed to get starnix process stats");
        TaskTimeStats {
            user_time: zx::MonotonicDuration::from_nanos(info.cpu_time),
            // TODO(https://fxbug.dev/42078242): How can we calculate system time?
            system_time: zx::MonotonicDuration::default(),
        }
    }

    /// For each task traced by this thread_group that matches the given
    /// selector, acquire its TaskMutableState and ptracees lock and execute the
    /// given function.
    pub fn get_ptracees_and(
        &self,
        selector: ProcessSelector,
        pids: &PidTable,
        f: &mut dyn FnMut(WeakRef<Task>, &TaskMutableState),
    ) {
        for tracee in self
            .ptracees
            .lock()
            .keys()
            .filter(|tracee_pid| selector.do_match(**tracee_pid, &pids))
            .map(|tracee_pid| pids.get_task(*tracee_pid))
        {
            if let Some(task_ref) = tracee.clone().upgrade() {
                let task_state = task_ref.write();
                if task_state.ptrace.is_some() {
                    f(tracee, &task_state);
                }
            }
        }
    }

    /// Returns a tracee whose state has changed, so that waitpid can report on
    /// it. If this returns a value, and the pid is being traced, the tracer
    /// thread is deemed to have seen the tracee ptrace-stop for the purposes of
    /// PTRACE_LISTEN.
    pub fn get_waitable_ptracee(
        &self,
        selector: ProcessSelector,
        options: &WaitingOptions,
        pids: &mut PidTable,
    ) -> Option<WaitResult> {
        // This checks to see if the target is a zombie ptracee.
        let waitable_entry = self.write().zombie_ptracees.get_waitable_entry(selector, options);
        match waitable_entry {
            None => (),
            Some((zombie, None)) => return Some(zombie.to_wait_result()),
            Some((zombie, Some((tg, z)))) => {
                if let Some(tg) = tg.upgrade() {
                    if TempRef::as_ptr(&tg) != self as *const Self {
                        tg.do_zombie_notifications(z);
                    } else {
                        {
                            let mut state = tg.write();
                            state.children.remove(&z.pid);
                            state.deferred_zombie_ptracers.retain(|&(_, tracee)| tracee != z.pid);
                        }

                        z.release(pids);
                    };
                }
                return Some(zombie.to_wait_result());
            }
        }

        let mut tasks = vec![];

        // This checks to see if the target is a living ptracee
        self.get_ptracees_and(selector, pids, &mut |task: WeakRef<Task>, _| {
            tasks.push(task);
        });
        for task in tasks {
            let Some(task_ref) = task.upgrade() else {
                continue;
            };

            let process_state = &mut task_ref.thread_group.write();
            let mut task_state = task_ref.write();
            if task_state
                .ptrace
                .as_ref()
                .is_some_and(|ptrace| ptrace.is_waitable(task_ref.load_stopped(), options))
            {
                // We've identified a potential target.  Need to return either
                // the process's information (if we are in group-stop) or the
                // thread's information (if we are in a different stop).

                // The shared information:
                let mut pid: i32 = 0;
                let info = process_state.tasks.values().next().unwrap().info().clone();
                let uid = info.creds().uid;
                let mut exit_status = None;
                let exit_signal = info.exit_signal();
                let time_stats =
                    process_state.base.time_stats() + process_state.children_time_stats;
                let task_stopped = task_ref.load_stopped();

                #[derive(PartialEq)]
                enum ExitType {
                    None,
                    Cont,
                    Stop,
                    Kill,
                }
                if process_state.is_waitable() {
                    let ptrace = &mut task_state.ptrace;
                    // The information for processes, if we were in group stop.
                    let process_stopped = process_state.base.load_stopped();
                    let mut fn_type = ExitType::None;
                    if process_stopped == StopState::Awake && options.wait_for_continued {
                        fn_type = ExitType::Cont;
                    }
                    let mut event = ptrace
                        .as_ref()
                        .map_or(PtraceEvent::None, |ptrace| {
                            ptrace.event_data.as_ref().map_or(PtraceEvent::None, |data| data.event)
                        })
                        .clone();
                    // Tasks that are ptrace'd always get stop notifications.
                    if process_stopped == StopState::GroupStopped
                        && (options.wait_for_stopped || ptrace.is_some())
                    {
                        fn_type = ExitType::Stop;
                    }
                    if fn_type != ExitType::None {
                        let siginfo = if options.keep_waitable_state {
                            process_state.last_signal.clone()
                        } else {
                            process_state.last_signal.take()
                        };
                        if let Some(mut siginfo) = siginfo {
                            if task_ref.thread_group.load_stopped() == StopState::GroupStopped
                                && ptrace.as_ref().is_some_and(|ptrace| ptrace.is_seized())
                            {
                                if event == PtraceEvent::None {
                                    event = PtraceEvent::Stop;
                                }
                                siginfo.code |= (PtraceEvent::Stop as i32) << 8;
                            }
                            if siginfo.signal == SIGKILL {
                                fn_type = ExitType::Kill;
                            }
                            exit_status = match fn_type {
                                ExitType::Stop => Some(ExitStatus::Stop(siginfo, event)),
                                ExitType::Cont => Some(ExitStatus::Continue(siginfo, event)),
                                ExitType::Kill => Some(ExitStatus::Kill(siginfo)),
                                _ => None,
                            };
                        }
                        // Clear the wait status of the ptrace, because we're
                        // using the tg status instead.
                        ptrace
                            .as_mut()
                            .map(|ptrace| ptrace.get_last_signal(options.keep_waitable_state));
                    }
                    pid = process_state.base.leader;
                }
                if exit_status == None {
                    if let Some(ptrace) = task_state.ptrace.as_mut() {
                        // The information for the task, if we were in a non-group stop.
                        let mut fn_type = ExitType::None;
                        let event = ptrace
                            .event_data
                            .as_ref()
                            .map_or(PtraceEvent::None, |event| event.event);
                        if task_stopped == StopState::Awake {
                            fn_type = ExitType::Cont;
                        }
                        if task_stopped.is_stopping_or_stopped()
                            || ptrace.stop_status == PtraceStatus::Listening
                        {
                            fn_type = ExitType::Stop;
                        }
                        if fn_type != ExitType::None {
                            if let Some(siginfo) =
                                ptrace.get_last_signal(options.keep_waitable_state)
                            {
                                if siginfo.signal == SIGKILL {
                                    fn_type = ExitType::Kill;
                                }
                                exit_status = match fn_type {
                                    ExitType::Stop => Some(ExitStatus::Stop(siginfo, event)),
                                    ExitType::Cont => Some(ExitStatus::Continue(siginfo, event)),
                                    ExitType::Kill => Some(ExitStatus::Kill(siginfo)),
                                    _ => None,
                                };
                            }
                        }
                        pid = task_ref.get_tid();
                    }
                }
                if let Some(exit_status) = exit_status {
                    return Some(WaitResult {
                        pid,
                        uid,
                        exit_info: ProcessExitInfo {
                            status: exit_status,
                            exit_signal: *exit_signal,
                        },
                        time_stats,
                    });
                }
            }
        }
        None
    }

    /// Attempts to send an unchecked signal to this thread group.
    ///
    /// - `current_task`: The task that is sending the signal.
    /// - `unchecked_signal`: The signal that is to be sent. Unchecked, since `0` is a sentinel value
    /// where rights are to be checked but no signal is actually sent.
    ///
    /// # Returns
    /// Returns Ok(()) if the signal was sent, or the permission checks passed with a 0 signal, otherwise
    /// the error that was encountered.
    pub fn send_signal_unchecked(
        &self,
        current_task: &CurrentTask,
        unchecked_signal: UncheckedSignal,
    ) -> Result<(), Errno> {
        if let Some(signal) = self.check_signal_access(current_task, unchecked_signal)? {
            let signal_info = SignalInfo {
                code: SI_USER as i32,
                detail: SignalDetail::Kill {
                    pid: current_task.thread_group.leader,
                    uid: current_task.creds().uid,
                },
                ..SignalInfo::default(signal)
            };

            self.write().send_signal(signal_info);
        }

        Ok(())
    }

    /// Attempts to send an unchecked signal to this thread group, with info read from
    /// `siginfo_ref`.
    ///
    /// - `current_task`: The task that is sending the signal.
    /// - `unchecked_signal`: The signal that is to be sent. Unchecked, since `0` is a sentinel value
    /// where rights are to be checked but no signal is actually sent.
    /// - `siginfo_ref`: The siginfo that will be enqueued.
    ///
    /// # Returns
    /// Returns Ok(()) if the signal was sent, or the permission checks passed with a 0 signal, otherwise
    /// the error that was encountered.
    pub fn send_signal_unchecked_with_info(
        &self,
        current_task: &CurrentTask,
        unchecked_signal: UncheckedSignal,
        siginfo_ref: UserAddress,
    ) -> Result<(), Errno> {
        if let Some(signal) = self.check_signal_access(current_task, unchecked_signal)? {
            let signal_info = read_siginfo(current_task, signal, siginfo_ref)?;
            if self.leader != current_task.get_pid()
                && (signal_info.code >= 0 || signal_info.code == SI_TKILL)
            {
                return error!(EPERM);
            }

            self.write().send_signal(signal_info);
        }

        Ok(())
    }

    /// Checks whether or not `current_task` can signal this thread group with `unchecked_signal`.
    ///
    /// Returns:
    ///   - `Ok(Some(Signal))` if the signal passed checks and should be sent.
    ///   - `Ok(None)` if the signal passed checks, but should not be sent. This is used by
    ///   userspace for permission checks.
    ///   - `Err(_)` if the permission checks failed.
    fn check_signal_access(
        &self,
        current_task: &CurrentTask,
        unchecked_signal: UncheckedSignal,
    ) -> Result<Option<Signal>, Errno> {
        // Pick an arbitrary task in thread_group to check permissions.
        //
        // Tasks can technically have different credentials, but in practice they are kept in sync.
        let state = self.read();
        let target_task = state.get_live_task()?;
        if !current_task.can_signal(&target_task, unchecked_signal) {
            return error!(EPERM);
        }

        // 0 is a sentinel value used to do permission checks.
        if unchecked_signal.is_zero() {
            return Ok(None);
        }

        let signal = Signal::try_from(unchecked_signal)?;
        security::check_signal_access(current_task, &target_task, signal)?;

        Ok(Some(signal))
    }
}

pub enum WaitableChildResult {
    ReadyNow(WaitResult),
    ShouldWait,
    NoneFound,
}

#[apply(state_implementation!)]
impl ThreadGroupMutableState<Base = ThreadGroup> {
    pub fn leader(&self) -> pid_t {
        self.base.leader
    }

    pub fn children(&self) -> impl Iterator<Item = TempRef<'_, ThreadGroup>> + '_ {
        self.children.values().map(|v| {
            v.upgrade().expect("Weak references to processes in ThreadGroup must always be valid")
        })
    }

    pub fn tasks(&self) -> impl Iterator<Item = TempRef<'_, Task>> + '_ {
        self.tasks.values().flat_map(|t| t.upgrade())
    }

    pub fn task_ids(&self) -> impl Iterator<Item = &pid_t> {
        self.tasks.keys()
    }

    pub fn contains_task(&self, tid: pid_t) -> bool {
        self.tasks.contains_key(&tid)
    }

    pub fn get_task(&self, tid: pid_t) -> Option<TempRef<'_, Task>> {
        self.tasks.get(&tid).and_then(|t| t.upgrade())
    }

    pub fn tasks_count(&self) -> usize {
        self.tasks.len()
    }

    pub fn get_ppid(&self) -> pid_t {
        match &self.parent {
            Some(parent) => parent.upgrade().leader,
            None => 0,
        }
    }

    fn set_process_group<L>(
        &mut self,
        locked: &mut Locked<'_, L>,
        process_group: Arc<ProcessGroup>,
        pids: &mut PidTable,
    ) where
        L: LockBefore<ProcessGroupState>,
    {
        if self.process_group == process_group {
            return;
        }
        self.leave_process_group(locked, pids);
        self.process_group = process_group;
        self.process_group.insert(locked, self.base);
    }

    fn leave_process_group<L>(&mut self, locked: &mut Locked<'_, L>, pids: &mut PidTable)
    where
        L: LockBefore<ProcessGroupState>,
    {
        if self.process_group.remove(locked, self.base) {
            self.process_group.session.write().remove(self.process_group.leader);
            pids.remove_process_group(self.process_group.leader);
        }
    }

    /// Indicates whether the thread group is waitable via waitid and waitpid for
    /// either WSTOPPED or WCONTINUED.
    pub fn is_waitable(&self) -> bool {
        return self.last_signal.is_some() && !self.base.load_stopped().is_in_progress();
    }

    pub fn get_waitable_zombie(
        &mut self,
        zombie_list: &dyn Fn(&mut ThreadGroupMutableState) -> &mut Vec<OwnedRef<ZombieProcess>>,
        selector: ProcessSelector,
        options: &WaitingOptions,
        pids: &mut PidTable,
    ) -> Option<WaitResult> {
        // The zombies whose pid matches the pid selector queried.
        let zombie_matches_pid_selector = |zombie: &OwnedRef<ZombieProcess>| match selector {
            ProcessSelector::Any => true,
            ProcessSelector::Pid(pid) => zombie.pid == pid,
            ProcessSelector::Pgid(pgid) => zombie.pgid == pgid,
        };

        // The zombies whose exit signal matches the waiting options queried.
        let zombie_matches_wait_options: Box<dyn Fn(&OwnedRef<ZombieProcess>) -> bool> = if options
            .wait_for_all
        {
            Box::new(|_zombie: &OwnedRef<ZombieProcess>| true)
        } else {
            // A "clone" zombie is one which has delivered no signal, or a
            // signal other than SIGCHLD to its parent upon termination.
            Box::new(|zombie: &OwnedRef<ZombieProcess>| {
                Self::is_correct_exit_signal(options.wait_for_clone, zombie.exit_info.exit_signal)
            })
        };

        // We look for the last zombie in the vector that matches pid selector and waiting options
        let selected_zombie_position = zombie_list(self)
            .iter()
            .rev()
            .position(|zombie: &OwnedRef<ZombieProcess>| {
                zombie_matches_wait_options(zombie) && zombie_matches_pid_selector(zombie)
            })
            .map(|position_starting_from_the_back| {
                zombie_list(self).len() - 1 - position_starting_from_the_back
            });

        selected_zombie_position.map(|position| {
            if options.keep_waitable_state {
                zombie_list(self)[position].to_wait_result()
            } else {
                let zombie = zombie_list(self).remove(position);
                self.children_time_stats += zombie.time_stats;
                let result = zombie.to_wait_result();
                zombie.release(pids);
                result
            }
        })
    }

    pub fn is_correct_exit_signal(for_clone: bool, exit_code: Option<Signal>) -> bool {
        for_clone == (exit_code != Some(SIGCHLD))
    }

    fn get_waitable_running_children(
        &self,
        selector: ProcessSelector,
        options: &WaitingOptions,
        pids: &PidTable,
    ) -> WaitableChildResult {
        // The children whose pid matches the pid selector queried.
        let filter_children_by_pid_selector = |child: &ThreadGroup| match selector {
            ProcessSelector::Any => true,
            ProcessSelector::Pid(pid) => child.leader == pid,
            ProcessSelector::Pgid(pgid) => {
                pids.get_process_group(pgid).as_ref() == Some(&child.read().process_group)
            }
        };

        // The children whose exit signal matches the waiting options queried.
        let filter_children_by_waiting_options = |child: &ThreadGroup| {
            if options.wait_for_all {
                return true;
            }
            let child_state = child.read();
            if child_state.terminating {
                // Child is terminating.  In addition to its original location,
                // the leader may have exited, and its exit signal may be in the
                // leader_exit_info.
                if let Some(info) = &child_state.leader_exit_info {
                    if info.exit_signal.is_some() {
                        return Self::is_correct_exit_signal(
                            options.wait_for_clone,
                            info.exit_signal,
                        );
                    }
                }
            };

            child_state.tasks.values().any(|container| {
                let info = container.info();
                Self::is_correct_exit_signal(options.wait_for_clone, *info.exit_signal())
            })
        };

        // If wait_for_exited flag is disabled or no terminated children were found we look for living children.
        let mut selected_children = self
            .children
            .values()
            .map(|t| t.upgrade().unwrap())
            .filter(|tg| filter_children_by_pid_selector(&tg))
            .filter(|tg| filter_children_by_waiting_options(&tg))
            .peekable();
        if selected_children.peek().is_none() {
            // There still might be a process that ptrace hasn't looked at yet.
            if self.deferred_zombie_ptracers.iter().any(|&(_, tracee)| match selector {
                ProcessSelector::Any => true,
                ProcessSelector::Pid(pid) => tracee == pid,
                ProcessSelector::Pgid(pgid) => {
                    pids.get_process_group(pgid).as_ref() == pids.get_process_group(tracee).as_ref()
                }
            }) {
                return WaitableChildResult::ShouldWait;
            }

            return WaitableChildResult::NoneFound;
        }
        for child in selected_children {
            let child = child.write();
            if child.last_signal.is_some() {
                let build_wait_result = |mut child: ThreadGroupWriteGuard<'_>,
                                         exit_status: &dyn Fn(SignalInfo) -> ExitStatus|
                 -> WaitResult {
                    let siginfo = if options.keep_waitable_state {
                        child.last_signal.clone().unwrap()
                    } else {
                        child.last_signal.take().unwrap()
                    };
                    let exit_status = if siginfo.signal == SIGKILL {
                        // This overrides the stop/continue choice.
                        ExitStatus::Kill(siginfo)
                    } else {
                        exit_status(siginfo)
                    };
                    let info = child.tasks.values().next().unwrap().info();
                    WaitResult {
                        pid: child.base.leader,
                        uid: info.creds().uid,
                        exit_info: ProcessExitInfo {
                            status: exit_status,
                            exit_signal: *info.exit_signal(),
                        },
                        time_stats: child.base.time_stats() + child.children_time_stats,
                    }
                };
                let child_stopped = child.base.load_stopped();
                if child_stopped == StopState::Awake && options.wait_for_continued {
                    return WaitableChildResult::ReadyNow(build_wait_result(child, &|siginfo| {
                        ExitStatus::Continue(siginfo, PtraceEvent::None)
                    }));
                }
                if child_stopped == StopState::GroupStopped && options.wait_for_stopped {
                    return WaitableChildResult::ReadyNow(build_wait_result(child, &|siginfo| {
                        ExitStatus::Stop(siginfo, PtraceEvent::None)
                    }));
                }
            }
        }

        WaitableChildResult::ShouldWait
    }

    /// Returns any waitable child matching the given `selector` and `options`. Returns None if no
    /// child matching the selector is waitable. Returns ECHILD if no child matches the selector at
    /// all.
    ///
    /// Will remove the waitable status from the child depending on `options`.
    pub fn get_waitable_child(
        &mut self,
        selector: ProcessSelector,
        options: &WaitingOptions,
        pids: &mut PidTable,
    ) -> WaitableChildResult {
        if options.wait_for_exited {
            if let Some(waitable_zombie) = self.get_waitable_zombie(
                &|state: &mut ThreadGroupMutableState| &mut state.zombie_children,
                selector,
                options,
                pids,
            ) {
                return WaitableChildResult::ReadyNow(waitable_zombie);
            }
        }

        self.get_waitable_running_children(selector, options, pids)
    }

    /// Returns a task in the current thread group.
    pub fn get_live_task(&self) -> Result<TempRef<'_, Task>, Errno> {
        self.tasks
            .get(&self.leader())
            .and_then(|t| t.upgrade())
            .or_else(|| self.tasks().next())
            .ok_or_else(|| errno!(ESRCH))
    }

    /// Set the stop status of the process.  If you pass |siginfo| of |None|,
    /// does not update the signal.  If |finalize_only| is set, will check that
    /// the set will be a finalize (Stopping -> Stopped or Stopped -> Stopped)
    /// before executing it.
    ///
    /// Returns the latest stop state after any changes.
    pub fn set_stopped(
        mut self,
        new_stopped: StopState,
        siginfo: Option<SignalInfo>,
        finalize_only: bool,
    ) -> StopState {
        if let Some(stopped) = self.base.check_stopped_state(new_stopped, finalize_only) {
            return stopped;
        }

        // Thread groups don't transition to group stop if they are waking, because waking
        // means something told it to wake up (like a SIGCONT) but hasn't finished yet.
        if self.base.load_stopped() == StopState::Waking
            && (new_stopped == StopState::GroupStopping || new_stopped == StopState::GroupStopped)
        {
            return self.base.load_stopped();
        }

        // TODO(https://g-issues.fuchsia.dev/issues/306438676): When thread
        // group can be stopped inside user code, tasks/thread groups will
        // need to be either restarted or stopped here.
        self.store_stopped(new_stopped);
        if let Some(signal) = &siginfo {
            // We don't want waiters to think the process was unstopped
            // because of a sigkill.  They will get woken when the
            // process dies.
            if signal.signal != SIGKILL {
                self.last_signal = siginfo;
            }
        }
        if new_stopped == StopState::Waking || new_stopped == StopState::ForceWaking {
            self.stopped_waiters.notify_all();
        };

        let parent = (!new_stopped.is_in_progress()).then(|| self.parent.clone()).flatten();

        // Drop the lock before locking the parent.
        std::mem::drop(self);
        if let Some(parent) = parent {
            let parent = parent.upgrade();
            parent.write().child_status_waiters.notify_all();
        }

        new_stopped
    }

    fn store_stopped(&mut self, state: StopState) {
        // We don't actually use the guard but we require it to enforce that the
        // caller holds the thread group's mutable state lock (identified by
        // mutable access to the thread group's mutable state).

        self.base.stop_state.store(state, Ordering::Relaxed)
    }

    /// Sends the signal `signal_info` to this thread group.
    #[allow(unused_mut)]
    pub fn send_signal(mut self, signal_info: SignalInfo) {
        let sigaction = self.base.signal_actions.get(signal_info.signal);
        let action = action_for_signal(&signal_info, sigaction);

        self.base.pending_signals.lock().enqueue(signal_info.clone());
        let tasks: Vec<WeakRef<Task>> = self.tasks.values().map(|t| t.weak_clone()).collect();

        // Set state to waking before interrupting any tasks.
        if signal_info.signal == SIGKILL {
            self.set_stopped(StopState::ForceWaking, Some(signal_info.clone()), false);
        } else if signal_info.signal == SIGCONT {
            self.set_stopped(StopState::Waking, Some(signal_info.clone()), false);
        }

        let mut has_interrupted_task = false;
        for task in tasks.iter().flat_map(|t| t.upgrade()) {
            let mut task_state = task.write();

            if signal_info.signal == SIGKILL {
                task_state.set_stopped(StopState::ForceWaking, None, None, None);
            } else if signal_info.signal == SIGCONT {
                task_state.set_stopped(StopState::Waking, None, None, None);
            }

            let is_masked = task_state.is_signal_masked(signal_info.signal);
            let was_masked = task_state.is_signal_masked_by_saved_mask(signal_info.signal);

            let is_queued = action != DeliveryAction::Ignore
                || is_masked
                || was_masked
                || task_state.is_ptraced();

            if is_queued {
                task_state.notify_signal_waiters();
                task_state.set_flags(TaskFlags::SIGNALS_AVAILABLE, true);

                if !is_masked && action.must_interrupt(Some(sigaction)) && !has_interrupted_task {
                    // Only interrupt one task, and only interrupt if the signal was actually queued
                    // and the action must interrupt.
                    drop(task_state);
                    task.interrupt();
                    has_interrupted_task = true;
                }
            }
        }
    }
}

/// Container around a weak task and a strong `TaskPersistentInfo`. It is needed to keep the
/// information even when the task is not upgradable, because when the task is dropped, there is a
/// moment where the task is not yet released, yet the weak pointer is not upgradeable anymore.
/// During this time, it is still necessary to access the persistent info to compute the state of
/// the thread for the different wait syscalls.
pub struct TaskContainer(WeakRef<Task>, TaskPersistentInfo);

impl From<&TempRef<'_, Task>> for TaskContainer {
    fn from(task: &TempRef<'_, Task>) -> Self {
        Self(WeakRef::from(task), task.persistent_info.clone())
    }
}

impl From<TaskContainer> for TaskPersistentInfo {
    fn from(container: TaskContainer) -> TaskPersistentInfo {
        container.1
    }
}

impl TaskContainer {
    fn upgrade(&self) -> Option<TempRef<'_, Task>> {
        self.0.upgrade()
    }

    fn weak_clone(&self) -> WeakRef<Task> {
        self.0.clone()
    }

    fn info(&self) -> MutexGuard<'_, TaskPersistentInfoState> {
        self.1.lock()
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::testing::*;

    #[::fuchsia::test]
    async fn test_setsid() {
        fn get_process_group(task: &Task) -> Arc<ProcessGroup> {
            Arc::clone(&task.thread_group.read().process_group)
        }
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        assert_eq!(current_task.thread_group.setsid(&mut locked), error!(EPERM));

        let child_task = current_task.clone_task_for_test(&mut locked, 0, Some(SIGCHLD));
        assert_eq!(get_process_group(&current_task), get_process_group(&child_task));

        let old_process_group = child_task.thread_group.read().process_group.clone();
        assert_eq!(child_task.thread_group.setsid(&mut locked), Ok(()));
        assert_eq!(
            child_task.thread_group.read().process_group.session.leader,
            child_task.get_pid()
        );
        assert!(!old_process_group
            .read(&mut locked)
            .thread_groups()
            .contains(&OwnedRef::temp(&child_task.thread_group)));
    }

    #[::fuchsia::test]
    async fn test_exit_status() {
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let child = current_task.clone_task_for_test(&mut locked, 0, Some(SIGCHLD));
        child.thread_group.exit(&mut locked, ExitStatus::Exit(42), None);
        std::mem::drop(child);
        assert_eq!(
            current_task.thread_group.read().zombie_children[0].exit_info.status,
            ExitStatus::Exit(42)
        );
    }

    #[::fuchsia::test]
    async fn test_setgpid() {
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        assert_eq!(current_task.thread_group.setsid(&mut locked), error!(EPERM));

        let child_task1 = current_task.clone_task_for_test(&mut locked, 0, Some(SIGCHLD));
        let child_task2 = current_task.clone_task_for_test(&mut locked, 0, Some(SIGCHLD));
        let execd_child_task = current_task.clone_task_for_test(&mut locked, 0, Some(SIGCHLD));
        execd_child_task.thread_group.write().did_exec = true;
        let other_session_child_task =
            current_task.clone_task_for_test(&mut locked, 0, Some(SIGCHLD));
        assert_eq!(other_session_child_task.thread_group.setsid(&mut locked), Ok(()));

        assert_eq!(child_task1.thread_group.setpgid(&mut locked, &current_task, 0), error!(ESRCH));
        assert_eq!(
            current_task.thread_group.setpgid(&mut locked, &execd_child_task, 0),
            error!(EACCES)
        );
        assert_eq!(current_task.thread_group.setpgid(&mut locked, &current_task, 0), error!(EPERM));
        assert_eq!(
            current_task.thread_group.setpgid(&mut locked, &other_session_child_task, 0),
            error!(EPERM)
        );
        assert_eq!(
            current_task.thread_group.setpgid(&mut locked, &child_task1, -1),
            error!(EINVAL)
        );
        assert_eq!(
            current_task.thread_group.setpgid(&mut locked, &child_task1, 255),
            error!(EPERM)
        );
        assert_eq!(
            current_task.thread_group.setpgid(
                &mut locked,
                &child_task1,
                other_session_child_task.id
            ),
            error!(EPERM)
        );

        assert_eq!(child_task1.thread_group.setpgid(&mut locked, &child_task1, 0), Ok(()));
        assert_eq!(child_task1.thread_group.read().process_group.session.leader, current_task.id);
        assert_eq!(child_task1.thread_group.read().process_group.leader, child_task1.id);

        let old_process_group = child_task2.thread_group.read().process_group.clone();
        assert_eq!(
            current_task.thread_group.setpgid(&mut locked, &child_task2, child_task1.id),
            Ok(())
        );
        assert_eq!(child_task2.thread_group.read().process_group.leader, child_task1.id);
        assert!(!old_process_group
            .read(&mut locked)
            .thread_groups()
            .contains(&OwnedRef::temp(&child_task2.thread_group)));
    }

    #[::fuchsia::test]
    async fn test_adopt_children() {
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let task1 = current_task.clone_task_for_test(&mut locked, 0, None);
        let task2 = task1.clone_task_for_test(&mut locked, 0, None);
        let task3 = task2.clone_task_for_test(&mut locked, 0, None);

        assert_eq!(task3.thread_group.read().get_ppid(), task2.id);

        task2.thread_group.exit(&mut locked, ExitStatus::Exit(0), None);
        std::mem::drop(task2);

        // Task3 parent should be current_task.
        assert_eq!(task3.thread_group.read().get_ppid(), current_task.id);
    }
}
