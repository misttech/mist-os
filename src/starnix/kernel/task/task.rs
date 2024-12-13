// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::mm::{DumpPolicy, MemoryAccessor, MemoryAccessorExt, MemoryManager, TaskMemoryAccessor};
use crate::mutable_state::{state_accessor, state_implementation};
use crate::security;
use crate::signals::{KernelSignal, RunState, SignalInfo, SignalState};
use crate::task::{
    set_thread_role, AbstractUnixSocketNamespace, AbstractVsockSocketNamespace, CurrentTask,
    EventHandler, Kernel, ProcessEntryRef, ProcessExitInfo, PtraceEvent, PtraceEventData,
    PtraceState, PtraceStatus, SchedulerPolicy, SeccompFilterContainer, SeccompState,
    SeccompStateValue, ThreadGroup, ThreadState, UtsNamespaceHandle, WaitCanceler, Waiter,
    ZombieProcess,
};
use crate::vfs::{FdFlags, FdNumber, FdTable, FileHandle, FsContext, FsNodeHandle, FsString};
use bitflags::bitflags;
use fuchsia_inspect_contrib::profile_duration;
use macro_rules_attribute::apply;
use starnix_logging::{log_debug, log_warn, set_current_task_info, set_zx_name};
use starnix_sync::{LockBefore, Locked, MmDumpable, Mutex, RwLock, TaskRelease};
use starnix_types::ownership::{
    OwnedRef, Releasable, ReleasableByRef, ReleaseGuard, TempRef, WeakRef,
};
use starnix_types::stats::TaskTimeStats;
use starnix_uapi::auth::{
    Credentials, FsCred, PtraceAccessMode, CAP_KILL, CAP_SYS_PTRACE, PTRACE_MODE_FSCREDS,
    PTRACE_MODE_REALCREDS,
};
use starnix_uapi::errors::Errno;
use starnix_uapi::signals::{
    sigaltstack_contains_pointer, SigSet, Signal, UncheckedSignal, SIGCONT,
};
use starnix_uapi::user_address::{UserAddress, UserRef};
use starnix_uapi::vfs::FdEvents;
use starnix_uapi::{
    errno, error, from_status_like_fdio, pid_t, robust_list_head, sigaction, sigaltstack, ucred,
    CLD_CONTINUED, CLD_DUMPED, CLD_EXITED, CLD_KILLED, CLD_STOPPED, FUTEX_BITSET_MATCH_ANY,
};
use std::collections::VecDeque;
use std::ffi::CString;
use std::mem::MaybeUninit;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::Arc;
use std::{cmp, fmt};
use zx::{
    AsHandleRef, Signals, Task as _, {self as zx},
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ExitStatus {
    Exit(u8),
    Kill(SignalInfo),
    CoreDump(SignalInfo),
    // The second field for Stop and Continue contains the type of ptrace stop
    // event that made it stop / continue, if applicable (PTRACE_EVENT_STOP,
    // PTRACE_EVENT_FORK, etc)
    Stop(SignalInfo, PtraceEvent),
    Continue(SignalInfo, PtraceEvent),
}
impl ExitStatus {
    /// Converts the given exit status to a status code suitable for returning from wait syscalls.
    pub fn wait_status(&self) -> i32 {
        match self {
            ExitStatus::Exit(status) => (*status as i32) << 8,
            ExitStatus::Kill(siginfo) => siginfo.signal.number() as i32,
            ExitStatus::CoreDump(siginfo) => (siginfo.signal.number() as i32) | 0x80,
            ExitStatus::Continue(siginfo, trace_event) => {
                let trace_event_val = *trace_event as u32;
                if trace_event_val != 0 {
                    (siginfo.signal.number() as i32) | (trace_event_val << 16) as i32
                } else {
                    0xffff
                }
            }
            ExitStatus::Stop(siginfo, trace_event) => {
                let trace_event_val = *trace_event as u32;
                (0x7f + ((siginfo.signal.number() as i32) << 8)) | (trace_event_val << 16) as i32
            }
        }
    }

    pub fn signal_info_code(&self) -> i32 {
        match self {
            ExitStatus::Exit(_) => CLD_EXITED as i32,
            ExitStatus::Kill(_) => CLD_KILLED as i32,
            ExitStatus::CoreDump(_) => CLD_DUMPED as i32,
            ExitStatus::Stop(_, _) => CLD_STOPPED as i32,
            ExitStatus::Continue(_, _) => CLD_CONTINUED as i32,
        }
    }

    pub fn signal_info_status(&self) -> i32 {
        match self {
            ExitStatus::Exit(status) => *status as i32,
            ExitStatus::Kill(siginfo)
            | ExitStatus::CoreDump(siginfo)
            | ExitStatus::Continue(siginfo, _)
            | ExitStatus::Stop(siginfo, _) => siginfo.signal.number() as i32,
        }
    }
}

pub struct AtomicStopState {
    inner: AtomicU8,
}

impl AtomicStopState {
    pub fn new(state: StopState) -> Self {
        Self { inner: AtomicU8::new(state as u8) }
    }

    pub fn load(&self, ordering: Ordering) -> StopState {
        let v = self.inner.load(ordering);
        // SAFETY: we only ever store to the atomic a value originating
        // from a valid `StopState`.
        unsafe { std::mem::transmute(v) }
    }

    pub fn store(&self, state: StopState, ordering: Ordering) {
        self.inner.store(state as u8, ordering)
    }
}

/// This enum describes the state that a task or thread group can be in when being stopped.
/// The names are taken from ptrace(2).
#[derive(Clone, Copy, Debug, PartialEq)]
#[repr(u8)]
pub enum StopState {
    /// In this state, the process has been told to wake up, but has not yet been woken.
    /// Individual threads may still be stopped.
    Waking,
    /// In this state, at least one thread is awake.
    Awake,
    /// Same as the above, but you are not allowed to make further transitions.  Used
    /// to kill the task / group.  These names are not in ptrace(2).
    ForceWaking,
    ForceAwake,

    /// In this state, the process has been told to stop via a signal, but has not yet stopped.
    GroupStopping,
    /// In this state, at least one thread of the process has stopped
    GroupStopped,
    /// In this state, the task has received a signal, and it is being traced, so it will
    /// stop at the next opportunity.
    SignalDeliveryStopping,
    /// Same as the last one, but has stopped.
    SignalDeliveryStopped,
    /// Stop for a ptrace event: a variety of events defined by ptrace and
    /// enabled with the use of various ptrace features, such as the
    /// PTRACE_O_TRACE_* options.  The parameter indicates the type of
    /// event. Examples include PTRACE_EVENT_FORK (the event is a fork),
    /// PTRACE_EVENT_EXEC (the event is exec), and other similar events.
    PtraceEventStopping,
    /// Same as the last one, but has stopped
    PtraceEventStopped,
    /// In this state, we have stopped before executing a syscall
    SyscallEnterStopping,
    SyscallEnterStopped,
    /// In this state, we have stopped after executing a syscall
    SyscallExitStopping,
    SyscallExitStopped,
}

impl StopState {
    /// This means a stop is either in progress or we've stopped.
    pub fn is_stopping_or_stopped(&self) -> bool {
        self.is_stopped() || self.is_stopping()
    }

    /// This means a stop is in progress.  Refers to any stop state ending in "ing".
    pub fn is_stopping(&self) -> bool {
        match *self {
            StopState::GroupStopping
            | StopState::SignalDeliveryStopping
            | StopState::PtraceEventStopping
            | StopState::SyscallEnterStopping
            | StopState::SyscallExitStopping => true,
            _ => false,
        }
    }

    /// This means task is stopped.
    pub fn is_stopped(&self) -> bool {
        match *self {
            StopState::GroupStopped
            | StopState::SignalDeliveryStopped
            | StopState::PtraceEventStopped
            | StopState::SyscallEnterStopped
            | StopState::SyscallExitStopped => true,
            _ => false,
        }
    }

    /// Returns the "ed" version of this StopState, if it is "ing".
    pub fn finalize(&self) -> Result<StopState, ()> {
        match *self {
            StopState::GroupStopping => Ok(StopState::GroupStopped),
            StopState::SignalDeliveryStopping => Ok(StopState::SignalDeliveryStopped),
            StopState::PtraceEventStopping => Ok(StopState::PtraceEventStopped),
            StopState::Waking => Ok(StopState::Awake),
            StopState::ForceWaking => Ok(StopState::ForceAwake),
            StopState::SyscallEnterStopping => Ok(StopState::SyscallEnterStopped),
            StopState::SyscallExitStopping => Ok(StopState::SyscallExitStopped),
            _ => Err(()),
        }
    }

    pub fn is_downgrade(&self, new_state: &StopState) -> bool {
        match *self {
            StopState::GroupStopped => *new_state == StopState::GroupStopping,
            StopState::SignalDeliveryStopped => *new_state == StopState::SignalDeliveryStopping,
            StopState::PtraceEventStopped => *new_state == StopState::PtraceEventStopping,
            StopState::SyscallEnterStopped => *new_state == StopState::SyscallEnterStopping,
            StopState::SyscallExitStopped => *new_state == StopState::SyscallExitStopping,
            StopState::Awake => *new_state == StopState::Waking,
            _ => false,
        }
    }

    pub fn is_waking_or_awake(&self) -> bool {
        *self == StopState::Waking
            || *self == StopState::Awake
            || *self == StopState::ForceWaking
            || *self == StopState::ForceAwake
    }

    /// Indicate if the transition to the stopped / awake state is not finished.  This
    /// function is typically used to determine when it is time to notify waiters.
    pub fn is_in_progress(&self) -> bool {
        *self == StopState::Waking
            || *self == StopState::ForceWaking
            || *self == StopState::GroupStopping
            || *self == StopState::SignalDeliveryStopping
            || *self == StopState::PtraceEventStopping
            || *self == StopState::SyscallEnterStopping
            || *self == StopState::SyscallExitStopping
    }

    pub fn ptrace_only(&self) -> bool {
        !self.is_waking_or_awake()
            && *self != StopState::GroupStopped
            && *self != StopState::GroupStopping
    }

    pub fn is_illegal_transition(&self, new_state: StopState) -> bool {
        *self == StopState::ForceAwake
            || (*self == StopState::ForceWaking && new_state != StopState::ForceAwake)
            || new_state == *self
            // Downgrades are generally a sign that something is screwed up, but
            // a SIGCONT can result in a downgrade from Awake to Waking, so we
            // allowlist it.
            || (self.is_downgrade(&new_state) && *self != StopState::Awake)
    }

    pub fn is_force(&self) -> bool {
        *self == StopState::ForceAwake || *self == StopState::ForceWaking
    }

    pub fn as_in_progress(&self) -> Result<StopState, ()> {
        match *self {
            StopState::GroupStopped => Ok(StopState::GroupStopping),
            StopState::SignalDeliveryStopped => Ok(StopState::SignalDeliveryStopping),
            StopState::PtraceEventStopped => Ok(StopState::PtraceEventStopping),
            StopState::Awake => Ok(StopState::Waking),
            StopState::ForceAwake => Ok(StopState::ForceWaking),
            StopState::SyscallEnterStopped => Ok(StopState::SyscallEnterStopping),
            StopState::SyscallExitStopped => Ok(StopState::SyscallExitStopping),
            _ => Ok(*self),
        }
    }
}

bitflags! {
    #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
    pub struct TaskFlags: u8 {
        const EXITED = 0x1;
        const SIGNALS_AVAILABLE = 0x2;
        const TEMPORARY_SIGNAL_MASK = 0x4;
        /// Whether the executor should dump the stack of this task when it exits.
        /// Currently used to implement ExitStatus::CoreDump.
        const DUMP_ON_EXIT = 0x8;
    }
}

pub struct AtomicTaskFlags {
    flags: AtomicU8,
}

impl AtomicTaskFlags {
    fn new(flags: TaskFlags) -> Self {
        Self { flags: AtomicU8::new(flags.bits()) }
    }

    fn load(&self, ordering: Ordering) -> TaskFlags {
        let flags = self.flags.load(ordering);
        // We only ever store values from a `TaskFlags`.
        TaskFlags::from_bits_retain(flags)
    }

    fn swap(&self, flags: TaskFlags, ordering: Ordering) -> TaskFlags {
        let flags = self.flags.swap(flags.bits(), ordering);
        // We only ever store values from a `TaskFlags`.
        TaskFlags::from_bits_retain(flags)
    }
}

/// This contains thread state that tracers can inspect and modify.  It is
/// captured when a thread stops, and optionally copied back (if dirty) when a
/// thread starts again.  An alternative implementation would involve the
/// tracers acting on thread state directly; however, this would involve sharing
/// CurrentTask structures across multiple threads, which goes against the
/// intent of the design of CurrentTask.
pub struct CapturedThreadState {
    /// The thread state of the traced task.  This is copied out when the thread
    /// stops.
    pub thread_state: ThreadState,

    /// Indicates that the last ptrace operation changed the thread state, so it
    /// should be written back to the original thread.
    pub dirty: bool,
}

pub struct TaskMutableState {
    // See https://man7.org/linux/man-pages/man2/set_tid_address.2.html
    pub clear_child_tid: UserRef<pid_t>,

    /// Signal handler related state. This is grouped together for when atomicity is needed during
    /// signal sending and delivery.
    signals: SignalState,

    /// Internal signals that have a higher priority than a regular signal.
    ///
    /// Storing in a separate queue outside of `SignalState` ensures the internal signals will
    /// never be ignored or masked when dequeuing. Higher priority ensures that no user signals
    /// will jump the queue, e.g. ptrace, which delays the delivery.
    ///
    /// This design is not about observable consequence, but about convenient implementation.
    kernel_signals: VecDeque<KernelSignal>,

    /// The exit status that this task exited with.
    exit_status: Option<ExitStatus>,

    /// Desired scheduler policy for the task.
    pub scheduler_policy: SchedulerPolicy,

    /// The UTS namespace assigned to this thread.
    ///
    /// This field is kept in the mutable state because the UTS namespace of a thread
    /// can be forked using `clone()` or `unshare()` syscalls.
    ///
    /// We use UtsNamespaceHandle because the UTS properties can be modified
    /// by any other thread that shares this namespace.
    pub uts_ns: UtsNamespaceHandle,

    /// Bit that determines whether a newly started program can have privileges its parent does
    /// not have.  See Documentation/prctl/no_new_privs.txt in the Linux kernel for details.
    /// Note that Starnix does not currently implement the relevant privileges (e.g.,
    /// setuid/setgid binaries).  So, you can set this, but it does nothing other than get
    /// propagated to children.
    ///
    /// The documentation indicates that this can only ever be set to
    /// true, and it cannot be reverted to false.  Accessor methods
    /// for this field ensure this property.
    no_new_privs: bool,

    /// Userspace hint about how to adjust the OOM score for this process.
    pub oom_score_adj: i32,

    /// List of currently installed seccomp_filters
    pub seccomp_filters: SeccompFilterContainer,

    /// A pointer to the head of the robust futex list of this thread in
    /// userspace. See get_robust_list(2)
    pub robust_list_head: UserRef<robust_list_head>,

    /// The timer slack used to group timer expirations for the calling thread.
    ///
    /// Timers may expire up to `timerslack_ns` late, but never early.
    ///
    /// If this value is 0, the task's default timerslack is used.
    pub timerslack_ns: u64,

    /// The default value for `timerslack_ns`. This value cannot change during the lifetime of a
    /// task.
    ///
    /// This value is set to the `timerslack_ns` of the creating thread, and thus is not constant
    /// across tasks.
    pub default_timerslack_ns: u64,

    /// Information that a tracer needs to communicate with this process, if it
    /// is being traced.
    pub ptrace: Option<PtraceState>,

    /// Information that a tracer needs to inspect this process.
    pub captured_thread_state: Option<CapturedThreadState>,

    /// Whether this process is frozen by the cgroup freezer.
    pub frozen: bool,
}

impl TaskMutableState {
    pub fn no_new_privs(&self) -> bool {
        self.no_new_privs
    }

    /// Sets the value of no_new_privs to true.  It is an error to set
    /// it to anything else.
    pub fn enable_no_new_privs(&mut self) {
        self.no_new_privs = true;
    }

    pub fn get_timerslack_ns(&self) -> u64 {
        self.timerslack_ns
    }

    /// Sets the current timerslack of the task to `ns`.
    ///
    /// If `ns` is zero, the current timerslack gets reset to the task's default timerslack.
    pub fn set_timerslack_ns(&mut self, ns: u64) {
        if ns == 0 {
            self.timerslack_ns = self.default_timerslack_ns;
        } else {
            self.timerslack_ns = ns;
        }
    }

    pub fn is_ptraced(&self) -> bool {
        self.ptrace.is_some()
    }

    pub fn is_ptrace_listening(&self) -> bool {
        self.ptrace.as_ref().is_some_and(|ptrace| ptrace.stop_status == PtraceStatus::Listening)
    }

    pub fn ptrace_on_signal_consume(&mut self) -> bool {
        self.ptrace.as_mut().is_some_and(|ptrace: &mut PtraceState| {
            if ptrace.stop_status.is_continuing() {
                ptrace.stop_status = PtraceStatus::Default;
                false
            } else {
                true
            }
        })
    }

    pub fn notify_ptracers(&mut self) {
        if let Some(ptrace) = &self.ptrace {
            ptrace.tracer_waiters().notify_all();
        }
    }

    pub fn wait_on_ptracer(&self, waiter: &Waiter) {
        if let Some(ptrace) = &self.ptrace {
            ptrace.tracee_waiters.wait_async(&waiter);
        }
    }

    pub fn notify_ptracees(&mut self) {
        if let Some(ptrace) = &self.ptrace {
            ptrace.tracee_waiters.notify_all();
        }
    }

    pub fn take_captured_state(&mut self) -> Option<CapturedThreadState> {
        if self.captured_thread_state.is_some() {
            let mut state = None;
            std::mem::swap(&mut state, &mut self.captured_thread_state);
            return state;
        }
        None
    }

    pub fn copy_state_from(&mut self, current_task: &CurrentTask) {
        self.captured_thread_state = Some(CapturedThreadState {
            thread_state: current_task.thread_state.extended_snapshot(),
            dirty: false,
        });
    }

    /// Returns the task's currently active signal mask.
    pub fn signal_mask(&self) -> SigSet {
        self.signals.mask()
    }

    /// Returns true if `signal` is currently blocked by this task's signal mask.
    pub fn is_signal_masked(&self, signal: Signal) -> bool {
        self.signals.mask().has_signal(signal)
    }

    /// Returns true if `signal` is blocked by the saved signal mask.
    ///
    /// Note that the current signal mask may still not be blocking the signal.
    pub fn is_signal_masked_by_saved_mask(&self, signal: Signal) -> bool {
        self.signals.saved_mask().is_some_and(|mask| mask.has_signal(signal))
    }

    /// Enqueues an internal signal at the back of the task's kernel signal queue.
    pub fn enqueue_kernel_signal(&mut self, signal: KernelSignal) {
        self.kernel_signals.push_back(signal);
    }

    /// Enqueues a signal at the back of the task's signal queue.
    pub fn enqueue_signal(&mut self, signal: SignalInfo) {
        self.signals.enqueue(signal);
    }

    /// Enqueues the signal, allowing the signal to skip straight to the front of the task's queue.
    ///
    /// `enqueue_signal` is the more common API to use.
    ///
    /// Note that this will not guarantee that the signal is dequeued before any process-directed
    /// signals.
    pub fn enqueue_signal_front(&mut self, signal: SignalInfo) {
        self.signals.enqueue(signal);
    }

    /// Sets the current signal mask of the task.
    pub fn set_signal_mask(&mut self, mask: SigSet) {
        self.signals.set_mask(mask);
    }

    /// Sets a temporary signal mask for the task.
    ///
    /// This mask should be removed by a matching call to `restore_signal_mask`.
    pub fn set_temporary_signal_mask(&mut self, mask: SigSet) {
        self.signals.set_temporary_mask(mask);
    }

    /// Removes the currently active, temporary, signal mask and restores the
    /// previously active signal mask.
    pub fn restore_signal_mask(&mut self) {
        self.signals.restore_mask();
    }

    /// Returns true if the task's current `RunState` is blocked.
    pub fn is_blocked(&self) -> bool {
        self.signals.run_state.is_blocked()
    }

    /// Sets the task's `RunState` to `run_state`.
    pub fn set_run_state(&mut self, run_state: RunState) {
        self.signals.run_state = run_state;
    }

    pub fn run_state(&self) -> RunState {
        self.signals.run_state.clone()
    }

    pub fn on_signal_stack(&self, stack_pointer_register: u64) -> bool {
        self.signals
            .alt_stack
            .map(|signal_stack| sigaltstack_contains_pointer(&signal_stack, stack_pointer_register))
            .unwrap_or(false)
    }

    pub fn set_sigaltstack(&mut self, stack: Option<sigaltstack>) {
        self.signals.alt_stack = stack;
    }

    pub fn sigaltstack(&self) -> Option<sigaltstack> {
        self.signals.alt_stack
    }

    pub fn wait_on_signal(&mut self, waiter: &Waiter) {
        self.signals.signal_wait.wait_async(waiter);
    }

    pub fn signals_mut(&mut self) -> &mut SignalState {
        &mut self.signals
    }

    pub fn wait_on_signal_fd_events(
        &self,
        waiter: &Waiter,
        events: FdEvents,
        handler: EventHandler,
    ) -> WaitCanceler {
        self.signals.signal_wait.wait_async_fd_events(waiter, events, handler)
    }

    pub fn notify_signal_waiters(&self) {
        self.signals.signal_wait.notify_all();
    }
}

#[apply(state_implementation!)]
impl TaskMutableState<Base = Task> {
    pub fn set_stopped(
        &mut self,
        stopped: StopState,
        siginfo: Option<SignalInfo>,
        current_task: Option<&CurrentTask>,
        event: Option<PtraceEventData>,
    ) {
        if stopped.ptrace_only() && self.ptrace.is_none() {
            return;
        }

        if self.base.load_stopped().is_illegal_transition(stopped) {
            return;
        }

        // TODO(https://g-issues.fuchsia.dev/issues/306438676): When task can be
        // stopped inside user code, task will need to be either restarted or
        // stopped here.
        self.store_stopped(stopped);
        if stopped.is_stopped() {
            if let Some(ref current_task) = current_task {
                self.copy_state_from(current_task);
            }
        }
        if let Some(ref mut ptrace) = &mut self.ptrace {
            ptrace.set_last_signal(siginfo);
            ptrace.set_last_event(event);
        }
        if stopped == StopState::Waking || stopped == StopState::ForceWaking {
            self.notify_ptracees();
        }
        if !stopped.is_in_progress() {
            self.notify_ptracers();
        }
    }

    pub fn set_ptrace(&mut self, tracer: Option<PtraceState>) -> Result<(), Errno> {
        if tracer.is_some() && self.ptrace.is_some() {
            return Err(errno!(EPERM));
        }

        if tracer.is_none() {
            if let Ok(tg_stop_state) = self.base.thread_group.load_stopped().as_in_progress() {
                self.set_stopped(tg_stop_state, None, None, None);
            }
        }
        self.ptrace = tracer;
        Ok(())
    }

    pub fn can_accept_ptrace_commands(&mut self) -> bool {
        !self.base.load_stopped().is_waking_or_awake()
            && self.is_ptraced()
            && !self.is_ptrace_listening()
    }

    fn store_stopped(&mut self, state: StopState) {
        // We don't actually use the guard but we require it to enforce that the
        // caller holds the thread group's mutable state lock (identified by
        // mutable access to the thread group's mutable state).

        self.base.stop_state.store(state, Ordering::Relaxed)
    }

    pub fn update_flags(&mut self, clear: TaskFlags, set: TaskFlags) {
        // We don't actually use the guard but we require it to enforce that the
        // caller holds the task's mutable state lock (identified by mutable
        // access to the task's mutable state).

        debug_assert_eq!(clear ^ set, clear | set);
        let observed = self.base.flags();
        let swapped = self.base.flags.swap((observed | set) & !clear, Ordering::Relaxed);
        debug_assert_eq!(swapped, observed);
    }

    pub fn set_flags(&mut self, flag: TaskFlags, v: bool) {
        let (clear, set) = if v { (TaskFlags::empty(), flag) } else { (flag, TaskFlags::empty()) };

        self.update_flags(clear, set);
    }

    pub fn set_exit_status(&mut self, status: ExitStatus) {
        self.set_flags(TaskFlags::EXITED, true);
        self.exit_status = Some(status);
    }

    pub fn set_exit_status_if_not_already(&mut self, status: ExitStatus) {
        self.set_flags(TaskFlags::EXITED, true);
        self.exit_status.get_or_insert(status);
    }

    /// Returns the number of pending signals for this task, without considering the signal mask.
    pub fn pending_signal_count(&self) -> usize {
        self.signals.num_queued() + self.base.thread_group.pending_signals.lock().num_queued()
    }

    /// Returns `true` if `signal` is pending for this task, without considering the signal mask.
    pub fn has_signal_pending(&self, signal: Signal) -> bool {
        self.signals.has_queued(signal)
            || self.base.thread_group.pending_signals.lock().has_queued(signal)
    }

    /// The set of pending signals for the task, including the signals pending for the thread
    /// group.
    pub fn pending_signals(&self) -> SigSet {
        self.signals.pending() | self.base.thread_group.pending_signals.lock().pending()
    }

    /// The set of pending signals for the task specifically, not including the signals pending
    /// for the thread group.
    pub fn task_specific_pending_signals(&self) -> SigSet {
        self.signals.pending()
    }

    /// Returns true if any currently pending signal is allowed by `mask`.
    pub fn is_any_signal_allowed_by_mask(&self, mask: SigSet) -> bool {
        self.signals.is_any_allowed_by_mask(mask)
            || self.base.thread_group.pending_signals.lock().is_any_allowed_by_mask(mask)
    }

    /// Returns whether or not a signal is pending for this task, taking the current
    /// signal mask into account.
    pub fn is_any_signal_pending(&self) -> bool {
        let mask = self.signal_mask();
        self.signals.is_any_pending()
            || self.base.thread_group.pending_signals.lock().is_any_allowed_by_mask(mask)
    }

    /// Returns the next pending signal that passes `predicate`.
    fn take_next_signal_where<F>(&mut self, predicate: F) -> Option<SignalInfo>
    where
        F: Fn(&SignalInfo) -> bool,
    {
        if let Some(signal) =
            self.base.thread_group.pending_signals.lock().take_next_where(&predicate)
        {
            Some(signal)
        } else {
            self.signals.take_next_where(&predicate)
        }
    }

    /// Removes and returns the next pending `signal` for this task.
    ///
    /// Returns `None` if `siginfo` is a blocked signal, or no such signal is pending.
    pub fn take_specific_signal(&mut self, siginfo: SignalInfo) -> Option<SignalInfo> {
        let signal_mask = self.signal_mask();
        if signal_mask.has_signal(siginfo.signal) {
            return None;
        }

        let predicate = |s: &SignalInfo| s.signal == siginfo.signal;
        self.take_next_signal_where(predicate)
    }

    /// Removes and returns a pending signal that is unblocked by the current signal mask.
    ///
    /// Returns `None` if there are no unblocked signals pending.
    pub fn take_any_signal(&mut self) -> Option<SignalInfo> {
        self.take_signal_with_mask(self.signal_mask())
    }

    /// Removes and returns a pending signal that is unblocked by `signal_mask`.
    ///
    /// Returns `None` if there are no signals pending that are unblocked by `signal_mask`.
    pub fn take_signal_with_mask(&mut self, signal_mask: SigSet) -> Option<SignalInfo> {
        let predicate = |s: &SignalInfo| !signal_mask.has_signal(s.signal) || s.force;
        self.take_next_signal_where(predicate)
    }

    /// Removes and returns a pending internal signal.
    ///
    /// Returns `None` if there are no signals pending.
    pub fn take_kernel_signal(&mut self) -> Option<KernelSignal> {
        self.kernel_signals.pop_front()
    }

    #[cfg(test)]
    pub fn queued_signal_count(&self, signal: Signal) -> usize {
        self.signals.queued_count(signal)
            + self.base.thread_group.pending_signals.lock().queued_count(signal)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskStateCode {
    // Task is being executed.
    Running,

    // Task is waiting for an event.
    Sleeping,

    // Tracing stop
    TracingStop,

    // Task has exited.
    Zombie,
}

impl TaskStateCode {
    pub fn code_char(&self) -> char {
        match self {
            TaskStateCode::Running => 'R',
            TaskStateCode::Sleeping => 'S',
            TaskStateCode::TracingStop => 't',
            TaskStateCode::Zombie => 'Z',
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            TaskStateCode::Running => "running",
            TaskStateCode::Sleeping => "sleeping",
            TaskStateCode::TracingStop => "tracing stop",
            TaskStateCode::Zombie => "zombie",
        }
    }
}

/// The information of the task that needs to be available to the `ThreadGroup` while computing
/// which process a wait can target. It is necessary to shared this data with the `ThreadGroup` so
/// that it is available while the task is being dropped and so is not accessible from a weak
/// pointer.
#[derive(Clone, Debug)]
pub struct TaskPersistentInfoState {
    /// Immutable information about the task
    tid: pid_t,
    pid: pid_t,

    /// The command of this task.
    command: CString,

    /// The security credentials for this task.
    creds: Credentials,

    /// The signal this task generates on exit.
    exit_signal: Option<Signal>,
}

impl TaskPersistentInfoState {
    fn new(
        tid: pid_t,
        pid: pid_t,
        command: CString,
        creds: Credentials,
        exit_signal: Option<Signal>,
    ) -> TaskPersistentInfo {
        Arc::new(Mutex::new(Self { tid, pid, command, creds, exit_signal }))
    }

    pub fn tid(&self) -> pid_t {
        self.tid
    }

    pub fn pid(&self) -> pid_t {
        self.pid
    }

    pub fn command(&self) -> &CString {
        &self.command
    }

    pub fn creds(&self) -> &Credentials {
        &self.creds
    }

    pub fn creds_mut(&mut self) -> &mut Credentials {
        &mut self.creds
    }

    pub fn exit_signal(&self) -> &Option<Signal> {
        &self.exit_signal
    }
}

pub type TaskPersistentInfo = Arc<Mutex<TaskPersistentInfoState>>;

/// A unit of execution.
///
/// A task is the primary unit of execution in the Starnix kernel. Most tasks are *user* tasks,
/// which have an associated Zircon thread. The Zircon thread switches between restricted mode,
/// in which the thread runs userspace code, and normal mode, in which the thread runs Starnix
/// code.
///
/// Tasks track the resources used by userspace by referencing various objects, such as an
/// `FdTable`, a `MemoryManager`, and an `FsContext`. Many tasks can share references to these
/// objects. In principle, which objects are shared between which tasks can be largely arbitrary,
/// but there are common patterns of sharing. For example, tasks created with `pthread_create`
/// will share the `FdTable`, `MemoryManager`, and `FsContext` and are often called "threads" by
/// userspace programmers. Tasks created by `posix_spawn` do not share these objects and are often
/// called "processes" by userspace programmers. However, inside the kernel, there is no clear
/// definition of a "thread" or a "process".
///
/// During boot, the kernel creates the first task, often called `init`. The vast majority of other
/// tasks are created as transitive clones (e.g., using `clone(2)`) of that task. Sometimes, the
/// kernel will create new tasks from whole cloth, either with a corresponding userspace component
/// or to represent some background work inside the kernel.
///
/// See also `CurrentTask`, which represents the task corresponding to the thread that is currently
/// executing.
pub struct Task {
    /// A unique identifier for this task.
    ///
    /// This value can be read in userspace using `gettid(2)`. In general, this value
    /// is different from the value return by `getpid(2)`, which returns the `id` of the leader
    /// of the `thread_group`.
    pub id: pid_t,

    /// The thread group to which this task belongs.
    ///
    /// The group of tasks in a thread group roughly corresponds to the userspace notion of a
    /// process.
    pub thread_group: OwnedRef<ThreadGroup>,

    /// A handle to the underlying Zircon thread object.
    ///
    /// Some tasks lack an underlying Zircon thread. These tasks are used internally by the
    /// Starnix kernel to track background work, typically on a `kthread`.
    pub thread: RwLock<Option<zx::Thread>>,

    /// The file descriptor table for this task.
    ///
    /// This table can be share by many tasks.
    pub files: FdTable,

    /// The memory manager for this task.
    mm: Option<Arc<MemoryManager>>,

    /// The file system for this task.
    fs: Option<RwLock<Arc<FsContext>>>,

    /// The namespace for abstract AF_UNIX sockets for this task.
    pub abstract_socket_namespace: Arc<AbstractUnixSocketNamespace>,

    /// The namespace for AF_VSOCK for this task.
    pub abstract_vsock_namespace: Arc<AbstractVsockSocketNamespace>,

    /// The stop state of the task, distinct from the stop state of the thread group.
    ///
    /// Must only be set when the `mutable_state` write lock is held.
    stop_state: AtomicStopState,

    /// The flags for the task.
    ///
    /// Must only be set the then `mutable_state` write lock is held.
    flags: AtomicTaskFlags,

    /// The mutable state of the Task.
    mutable_state: RwLock<TaskMutableState>,

    /// The information of the task that needs to be available to the `ThreadGroup` while computing
    /// which process a wait can target.
    /// Contains the command line, the task credentials and the exit signal.
    /// See `TaskPersistentInfo` for more information.
    pub persistent_info: TaskPersistentInfo,

    /// For vfork and clone() with CLONE_VFORK, this is set when the task exits or calls execve().
    /// It allows the calling task to block until the fork has been completed. Only populated
    /// when created with the CLONE_VFORK flag.
    vfork_event: Option<Arc<zx::Event>>,

    /// Variable that can tell you whether there are currently seccomp
    /// filters without holding a lock
    pub seccomp_filter_state: SeccompState,

    /// Tell you whether you are tracing syscall entry / exit without a lock.
    pub trace_syscalls: AtomicBool,

    // The pid directory, so it doesn't have to be generated and thrown away on every access.
    // See https://fxbug.dev/291962828 for details.
    pub proc_pid_directory_cache: Mutex<Option<FsNodeHandle>>,

    /// The Linux Security Modules state for this thread group.
    pub security_state: security::TaskState,
}

/// The decoded cross-platform parts we care about for page fault exception reports.
#[derive(Debug)]
pub struct PageFaultExceptionReport {
    pub faulting_address: u64,
    pub not_present: bool, // Set when the page fault was due to a not-present page.
    pub is_write: bool,    // Set when the triggering memory operation was a write.
}

impl Task {
    pub fn kernel(&self) -> &Arc<Kernel> {
        &self.thread_group.kernel
    }

    pub fn has_same_address_space(&self, other: &Self) -> bool {
        match (self.mm(), other.mm()) {
            (Some(this), Some(other)) => Arc::ptr_eq(this, other),
            (None, None) => true,
            _ => false,
        }
    }

    pub fn flags(&self) -> TaskFlags {
        self.flags.load(Ordering::Relaxed)
    }

    /// When the task exits, if there is a notification that needs to propagate
    /// to a ptracer, make sure it will propagate.
    pub fn set_ptrace_zombie(&self, pids: &crate::task::PidTable) {
        let pgid = self.thread_group.read().process_group.leader;
        let mut state = self.write();
        state.set_stopped(StopState::ForceAwake, None, None, None);
        if let Some(ref mut ptrace) = &mut state.ptrace {
            // Add a zombie that the ptracer will notice.
            ptrace.last_signal_waitable = true;
            let tracer_pid = ptrace.get_pid();
            let weak_init = pids.get_task(tracer_pid);
            if let Some(tracer_task) = weak_init.upgrade() {
                drop(state);
                let mut tracer_state = tracer_task.thread_group.write();

                let exit_status = self.exit_status().unwrap_or_else(|| {
                    starnix_logging::log_error!("Exiting without an exit code.");
                    ExitStatus::Exit(u8::MAX)
                });
                let (uid, exit_signal) = {
                    let persistent_state = self.persistent_info.lock();
                    (persistent_state.creds().uid, persistent_state.exit_signal().clone())
                };
                let exit_info = ProcessExitInfo { status: exit_status, exit_signal };
                let zombie = ZombieProcess {
                    pid: self.id,
                    pgid,
                    uid,
                    exit_info: exit_info,
                    // ptrace doesn't need this.
                    time_stats: TaskTimeStats::default(),
                    is_canonical: false,
                };

                tracer_state.zombie_ptracees.add(zombie);
            };
        }
    }

    /// Disconnects this task from the tracer, if the tracer is still running.
    pub fn ptrace_disconnect(&mut self) {
        let pids = self.thread_group.kernel.pids.read();
        let mut state = self.write();
        let ptracer_pid = state.ptrace.as_ref().map(|ptrace| ptrace.get_pid());
        if let Some(ptracer_pid) = ptracer_pid {
            let _ = state.set_ptrace(None);
            if let Some(ProcessEntryRef::Process(tg)) = pids.get_process(ptracer_pid) {
                let pid = self.get_pid();
                drop(state);
                tg.ptracees.lock().remove(&pid);
            }
        }
    }

    pub fn exit_status(&self) -> Option<ExitStatus> {
        self.is_exitted().then(|| self.read().exit_status.clone()).flatten()
    }

    pub fn is_exitted(&self) -> bool {
        self.flags().contains(TaskFlags::EXITED)
    }

    pub fn load_stopped(&self) -> StopState {
        self.stop_state.load(Ordering::Relaxed)
    }

    /// Upgrade a Reference to a Task, returning a ESRCH errno if the reference cannot be borrowed.
    pub fn from_weak(weak: &WeakRef<Task>) -> Result<TempRef<'_, Task>, Errno> {
        weak.upgrade().ok_or_else(|| errno!(ESRCH))
    }

    /// Internal function for creating a Task object. Useful when you need to specify the value of
    /// every field. create_process and create_thread are more likely to be what you want.
    ///
    /// Any fields that should be initialized fresh for every task, even if the task was created
    /// with fork, are initialized to their defaults inside this function. All other fields are
    /// passed as parameters.
    #[allow(clippy::let_and_return)]
    pub fn new(
        id: pid_t,
        command: CString,
        thread_group: OwnedRef<ThreadGroup>,
        thread: Option<zx::Thread>,
        files: FdTable,
        mm: Option<Arc<MemoryManager>>,
        // The only case where fs should be None if when building the initial task that is the
        // used to build the initial FsContext.
        fs: Arc<FsContext>,
        creds: Credentials,
        abstract_socket_namespace: Arc<AbstractUnixSocketNamespace>,
        abstract_vsock_namespace: Arc<AbstractVsockSocketNamespace>,
        exit_signal: Option<Signal>,
        signal_mask: SigSet,
        vfork_event: Option<Arc<zx::Event>>,
        scheduler_policy: SchedulerPolicy,
        uts_ns: UtsNamespaceHandle,
        no_new_privs: bool,
        seccomp_filter_state: SeccompState,
        seccomp_filters: SeccompFilterContainer,
        robust_list_head: UserRef<robust_list_head>,
        timerslack_ns: u64,
        security_state: security::TaskState,
    ) -> Self {
        let pid = thread_group.leader;
        let task = Task {
            id,
            thread_group,
            thread: RwLock::new(thread),
            files,
            mm,
            fs: Some(RwLock::new(fs)),
            abstract_socket_namespace,
            abstract_vsock_namespace,
            vfork_event,
            stop_state: AtomicStopState::new(StopState::Awake),
            flags: AtomicTaskFlags::new(TaskFlags::empty()),
            mutable_state: RwLock::new(TaskMutableState {
                clear_child_tid: UserRef::default(),
                signals: SignalState::with_mask(signal_mask),
                kernel_signals: Default::default(),
                exit_status: None,
                scheduler_policy,
                uts_ns,
                no_new_privs,
                oom_score_adj: Default::default(),
                seccomp_filters,
                robust_list_head,
                timerslack_ns,
                // The default timerslack is set to the current timerslack of the creating thread.
                default_timerslack_ns: timerslack_ns,
                ptrace: None,
                captured_thread_state: None,
                frozen: false,
            }),
            persistent_info: TaskPersistentInfoState::new(id, pid, command, creds, exit_signal),
            seccomp_filter_state,
            trace_syscalls: AtomicBool::new(false),
            proc_pid_directory_cache: Mutex::new(None),
            security_state,
        };
        #[cfg(any(test, debug_assertions))]
        {
            let _l1 = task.read();
            let _l2 = task.persistent_info.lock();
        }
        task
    }

    state_accessor!(Task, mutable_state);

    pub fn add_file(&self, file: FileHandle, flags: FdFlags) -> Result<FdNumber, Errno> {
        self.files.add_with_flags(self, file, flags)
    }

    pub fn creds(&self) -> Credentials {
        self.persistent_info.lock().creds.clone()
    }

    pub fn ptracer_task(&self) -> WeakRef<Task> {
        let ptracer = {
            let state = self.read();
            state.ptrace.as_ref().map(|p| p.core_state.pid)
        };

        let Some(ptracer) = ptracer else {
            return WeakRef::default();
        };

        self.get_task(ptracer)
    }

    pub fn exit_signal(&self) -> Option<Signal> {
        self.persistent_info.lock().exit_signal
    }

    pub fn fs(&self) -> Arc<FsContext> {
        self.fs.as_ref().expect("fs must be set").read().clone()
    }

    pub fn mm(&self) -> Option<&Arc<MemoryManager>> {
        self.mm.as_ref()
    }

    pub fn unshare_fs(&self) {
        let mut fs = self.fs.as_ref().expect("fs must be set").write();
        *fs = fs.fork();
    }

    /// Overwrite the existing scheduler policy with a new one and update the task's thread's role.
    pub fn set_scheduler_policy(&self, policy: SchedulerPolicy) -> Result<(), Errno> {
        self.update_sched_policy_then_role(|sched_policy| *sched_policy = policy)
    }

    /// Update the nice value of the scheduler policy and update the task's thread's role.
    pub fn update_scheduler_nice(&self, raw_priority: u8) -> Result<(), Errno> {
        self.update_sched_policy_then_role(|sched_policy| sched_policy.set_raw_nice(raw_priority))
    }

    /// Update the task's thread's role based on its current scheduler policy without making any
    /// changes to the policy.
    ///
    /// This should be called on tasks that have newly created threads, e.g. after cloning.
    pub fn sync_scheduler_policy_to_role(&self) -> Result<(), Errno> {
        self.update_sched_policy_then_role(|_| {})
    }

    fn update_sched_policy_then_role(
        &self,
        updater: impl FnOnce(&mut SchedulerPolicy),
    ) -> Result<(), Errno> {
        profile_duration!("UpdateTaskThreadRole");
        let new_scheduler_policy = {
            // Hold the task state lock as briefly as possible, it's not needed to update the role.
            let mut state = self.write();
            updater(&mut state.scheduler_policy);
            state.scheduler_policy
        };

        let Some(role_manager) = &self.thread_group.kernel.role_manager else {
            log_debug!("thread role update requested in kernel without ProfileProvider, skipping");
            return Ok(());
        };
        let thread = self.thread.read();
        let Some(thread) = thread.as_ref() else {
            log_debug!("thread role update requested for task without thread, skipping");
            return Ok(());
        };
        set_thread_role(role_manager, thread, new_scheduler_policy)?;
        Ok(())
    }

    // See "Ptrace access mode checking" in https://man7.org/linux/man-pages/man2/ptrace.2.html
    pub fn check_ptrace_access_mode<L>(
        &self,
        locked: &mut Locked<'_, L>,
        mode: PtraceAccessMode,
        target: &Task,
    ) -> Result<(), Errno>
    where
        L: LockBefore<MmDumpable>,
    {
        // (1)  If the calling thread and the target thread are in the same
        //      thread group, access is always allowed.
        if self.thread_group.leader == target.thread_group.leader {
            return Ok(());
        }

        // (2)  If the access mode specifies PTRACE_MODE_FSCREDS, then, for
        //      the check in the next step, employ the caller's filesystem
        //      UID and GID.  (As noted in credentials(7), the filesystem
        //      UID and GID almost always have the same values as the
        //      corresponding effective IDs.)
        //
        //      Otherwise, the access mode specifies PTRACE_MODE_REALCREDS,
        //      so use the caller's real UID and GID for the checks in the
        //      next step.  (Most APIs that check the caller's UID and GID
        //      use the effective IDs.  For historical reasons, the
        //      PTRACE_MODE_REALCREDS check uses the real IDs instead.)
        let creds = self.creds();
        let (uid, gid) = if mode.contains(PTRACE_MODE_FSCREDS) {
            let fscred = creds.as_fscred();
            (fscred.uid, fscred.gid)
        } else if mode.contains(PTRACE_MODE_REALCREDS) {
            (creds.uid, creds.gid)
        } else {
            unreachable!();
        };

        // (3)  Deny access if neither of the following is true:
        //
        //      -  The real, effective, and saved-set user IDs of the target
        //         match the caller's user ID, and the real, effective, and
        //         saved-set group IDs of the target match the caller's
        //         group ID.
        //
        //      -  The caller has the CAP_SYS_PTRACE capability in the user
        //         namespace of the target.
        let target_creds = target.creds();
        if !creds.has_capability(CAP_SYS_PTRACE)
            && !(target_creds.uid == uid
                && target_creds.euid == uid
                && target_creds.saved_uid == uid
                && target_creds.gid == gid
                && target_creds.egid == gid
                && target_creds.saved_gid == gid)
        {
            return error!(EPERM);
        }

        // (4)  Deny access if the target process "dumpable" attribute has a
        //      value other than 1 (SUID_DUMP_USER; see the discussion of
        //      PR_SET_DUMPABLE in prctl(2)), and the caller does not have
        //      the CAP_SYS_PTRACE capability in the user namespace of the
        //      target process.
        let dumpable = *target.mm().ok_or_else(|| errno!(EINVAL))?.dumpable.lock(locked);
        if dumpable != DumpPolicy::User && !creds.has_capability(CAP_SYS_PTRACE) {
            return error!(EPERM);
        }

        // TODO: Implement the LSM security_ptrace_access_check() interface.
        //
        // (5)  The kernel LSM security_ptrace_access_check() interface is
        //      invoked to see if ptrace access is permitted.

        // (6)  If access has not been denied by any of the preceding steps,
        //      then access is allowed.
        Ok(())
    }

    /// Signals the vfork event, if any, to unblock waiters.
    pub fn signal_vfork(&self) {
        if let Some(event) = &self.vfork_event {
            if let Err(status) = event.signal_handle(Signals::NONE, Signals::USER_0) {
                log_warn!("Failed to set vfork signal {status}");
            }
        };
    }

    /// Blocks the caller until the task has exited or executed execve(). This is used to implement
    /// vfork() and clone(... CLONE_VFORK, ...). The task must have created with CLONE_EXECVE.
    pub fn wait_for_execve(&self, task_to_wait: WeakRef<Task>) -> Result<(), Errno> {
        let event = task_to_wait.upgrade().and_then(|t| t.vfork_event.clone());
        if let Some(event) = event {
            event
                .wait_handle(zx::Signals::USER_0, zx::MonotonicInstant::INFINITE)
                .map_err(|status| from_status_like_fdio!(status))?;
        }
        Ok(())
    }

    /// If needed, clear the child tid for this task.
    ///
    /// Userspace can ask us to clear the child tid and issue a futex wake at
    /// the child tid address when we tear down a task. For example, bionic
    /// uses this mechanism to implement pthread_join. The thread that calls
    /// pthread_join sleeps using FUTEX_WAIT on the child tid address. We wake
    /// them up here to let them know the thread is done.
    pub fn clear_child_tid_if_needed(&self) -> Result<(), Errno> {
        let mut state = self.write();
        let user_tid = state.clear_child_tid;
        if !user_tid.is_null() {
            let zero: pid_t = 0;
            self.write_object(user_tid, &zero)?;
            self.kernel().shared_futexes.wake(
                self,
                user_tid.addr(),
                usize::MAX,
                FUTEX_BITSET_MATCH_ANY,
            )?;
            state.clear_child_tid = UserRef::default();
        }
        Ok(())
    }

    pub fn get_task(&self, pid: pid_t) -> WeakRef<Task> {
        self.kernel().pids.read().get_task(pid)
    }

    pub fn get_pid(&self) -> pid_t {
        self.thread_group.leader
    }

    pub fn get_tid(&self) -> pid_t {
        self.id
    }

    pub fn is_leader(&self) -> bool {
        self.get_pid() == self.get_tid()
    }

    pub fn read_argv(&self, max_len: usize) -> Result<Vec<FsString>, Errno> {
        // argv is empty for kthreads
        let Some(mm) = self.mm() else {
            return Ok(vec![]);
        };
        let (argv_start, argv_end) = {
            let mm_state = mm.state.read();
            (mm_state.argv_start, mm_state.argv_end)
        };

        let len_to_read = std::cmp::min(argv_end - argv_start, max_len);
        self.read_nul_delimited_c_string_list(argv_start, len_to_read)
    }

    pub fn read_env(&self, max_len: usize) -> Result<Vec<FsString>, Errno> {
        // environment is empty for kthreads
        let Some(mm) = self.mm() else { return Ok(vec![]) };
        let (env_start, env_end) = {
            let mm_state = mm.state.read();
            (mm_state.environ_start, mm_state.environ_end)
        };

        let len_to_read = std::cmp::min(env_end - env_start, max_len);
        self.read_nul_delimited_c_string_list(env_start, len_to_read)
    }

    pub fn thread_runtime_info(&self) -> Result<zx::TaskRuntimeInfo, Errno> {
        self.thread
            .read()
            .as_ref()
            .ok_or_else(|| errno!(EINVAL))?
            .get_runtime_info()
            .map_err(|status| from_status_like_fdio!(status))
    }

    pub fn as_ucred(&self) -> ucred {
        let creds = self.creds();
        ucred { pid: self.get_pid(), uid: creds.uid, gid: creds.gid }
    }

    pub fn as_fscred(&self) -> FsCred {
        self.creds().as_fscred()
    }

    pub fn can_signal(&self, target: &Task, unchecked_signal: UncheckedSignal) -> bool {
        // If both the tasks share a thread group the signal can be sent. This is not documented
        // in kill(2) because kill does not support task-level granularity in signal sending.
        if self.thread_group == target.thread_group {
            return true;
        }

        let self_creds = self.creds();

        if self_creds.has_capability(CAP_KILL) {
            return true;
        }

        if self_creds.has_same_uid(&target.creds()) {
            return true;
        }

        if Signal::try_from(unchecked_signal) == Ok(SIGCONT) {
            let target_session = target.thread_group.read().process_group.session.leader;
            let self_session = self.thread_group.read().process_group.session.leader;
            return target_session == self_session;
        }

        false
    }

    /// Interrupts the current task.
    ///
    /// This will interrupt any blocking syscalls if the task is blocked on one.
    /// The signal_state of the task must not be locked.
    pub fn interrupt(&self) {
        self.read().signals.run_state.wake();
        if let Some(thread) = self.thread.read().as_ref() {
            crate::execution::interrupt_thread(thread);
        }
    }

    pub fn command(&self) -> CString {
        self.persistent_info.lock().command.clone()
    }

    pub fn set_command_name(&self, name: CString) {
        // Set the name on the Linux thread.
        if let Some(thread) = self.thread.read().as_ref() {
            set_zx_name(thread, name.as_bytes());
        }
        // If this is the thread group leader, use this name for the process too.
        if self.is_leader() {
            set_zx_name(&self.thread_group.process, name.as_bytes());
            let _ = zx::Thread::raise_user_exception(
                zx::RaiseExceptionOptions::TARGET_JOB_DEBUGGER,
                zx::sys::ZX_EXCP_USER_CODE_PROCESS_NAME_CHANGED,
                0,
            );
        }

        set_current_task_info(&name, self.thread_group.leader, self.id);

        // Truncate to 16 bytes, including null byte.
        let bytes = name.to_bytes();

        self.persistent_info.lock().command = if bytes.len() > 15 {
            // SAFETY: Substring of a CString will contain no null bytes.
            CString::new(&bytes[..15]).unwrap()
        } else {
            name
        };
    }

    pub fn set_seccomp_state(&self, state: SeccompStateValue) -> Result<(), Errno> {
        self.seccomp_filter_state.set(&state)
    }

    pub fn state_code(&self) -> TaskStateCode {
        let status = self.read();
        if status.exit_status.is_some() {
            TaskStateCode::Zombie
        } else if status.signals.run_state.is_blocked() {
            let stop_state = self.load_stopped();
            if stop_state.ptrace_only() && stop_state.is_stopped() {
                TaskStateCode::TracingStop
            } else {
                TaskStateCode::Sleeping
            }
        } else {
            TaskStateCode::Running
        }
    }

    pub fn time_stats(&self) -> TaskTimeStats {
        let info = match &*self.thread.read() {
            Some(thread) => zx::Task::get_runtime_info(thread).expect("Failed to get thread stats"),
            None => return TaskTimeStats::default(),
        };

        TaskTimeStats {
            user_time: zx::MonotonicDuration::from_nanos(info.cpu_time),
            // TODO(https://fxbug.dev/42078242): How can we calculate system time?
            system_time: zx::MonotonicDuration::default(),
        }
    }

    pub fn get_signal_action(&self, signal: Signal) -> sigaction {
        self.thread_group.signal_actions.get(signal)
    }
}

impl Releasable for Task {
    type Context<'a: 'b, 'b> = (ThreadState, &'b mut Locked<'a, TaskRelease>);

    fn release<'a: 'b, 'b>(mut self, context: Self::Context<'a, 'b>) {
        let (thread_state, locked) = context;

        *self.proc_pid_directory_cache.get_mut() = None;

        self.ptrace_disconnect();

        // Release the fd table.
        self.files.release(());

        self.signal_vfork();

        // Drop fields that can end up owning a FsNode to ensure no FsNode are owned by this task.
        self.fs = None;
        self.mm = None;
        // Rebuild a temporary CurrentTask to run the release actions that requires a CurrentState.
        let current_task = CurrentTask::new(OwnedRef::new(self), thread_state);

        // Apply any delayed releasers left.
        current_task.trigger_delayed_releaser(locked);

        // Drop the task now that is has been released. This requires to take it fro the OwnedRef
        // and from the resulting ReleaseGuard.
        let CurrentTask { mut task, .. } = current_task;
        let task = OwnedRef::take(&mut task).expect("task should not have been re-owned");
        let task: Self = ReleaseGuard::take(task);

        // Release the ThreadGroup.
        task.thread_group.release(());
    }
}

impl MemoryAccessor for Task {
    fn read_memory<'a>(
        &self,
        addr: UserAddress,
        bytes: &'a mut [MaybeUninit<u8>],
    ) -> Result<&'a mut [u8], Errno> {
        // Using a `Task` to read memory generally indicates that the memory
        // is being read from a task different than the `CurrentTask`. When
        // this `Task` is not current, its address space is not mapped
        // so we need to go through the VMO.
        self.mm().ok_or_else(|| errno!(EINVAL))?.syscall_read_memory(addr, bytes)
    }

    fn read_memory_partial_until_null_byte<'a>(
        &self,
        addr: UserAddress,
        bytes: &'a mut [MaybeUninit<u8>],
    ) -> Result<&'a mut [u8], Errno> {
        // Using a `Task` to read memory generally indicates that the memory
        // is being read from a task different than the `CurrentTask`. When
        // this `Task` is not current, its address space is not mapped
        // so we need to go through the VMO.
        self.mm()
            .ok_or_else(|| errno!(EINVAL))?
            .syscall_read_memory_partial_until_null_byte(addr, bytes)
    }

    fn read_memory_partial<'a>(
        &self,
        addr: UserAddress,
        bytes: &'a mut [MaybeUninit<u8>],
    ) -> Result<&'a mut [u8], Errno> {
        // Using a `Task` to read memory generally indicates that the memory
        // is being read from a task different than the `CurrentTask`. When
        // this `Task` is not current, its address space is not mapped
        // so we need to go through the VMO.
        self.mm().ok_or_else(|| errno!(EINVAL))?.syscall_read_memory_partial(addr, bytes)
    }

    fn write_memory(&self, addr: UserAddress, bytes: &[u8]) -> Result<usize, Errno> {
        // Using a `Task` to write memory generally indicates that the memory
        // is being written to a task different than the `CurrentTask`. When
        // this `Task` is not current, its address space is not mapped
        // so we need to go through the VMO.
        self.mm().ok_or_else(|| errno!(EINVAL))?.syscall_write_memory(addr, bytes)
    }

    fn write_memory_partial(&self, addr: UserAddress, bytes: &[u8]) -> Result<usize, Errno> {
        // Using a `Task` to write memory generally indicates that the memory
        // is being written to a task different than the `CurrentTask`. When
        // this `Task` is not current, its address space is not mapped
        // so we need to go through the VMO.
        self.mm().ok_or_else(|| errno!(EINVAL))?.syscall_write_memory_partial(addr, bytes)
    }

    fn zero(&self, addr: UserAddress, length: usize) -> Result<usize, Errno> {
        // Using a `Task` to zero memory generally indicates that the memory
        // is being zeroed from a task different than the `CurrentTask`. When
        // this `Task` is not current, its address space is not mapped
        // so we need to go through the VMO.
        self.mm().ok_or_else(|| errno!(EINVAL))?.syscall_zero(addr, length)
    }
}

impl TaskMemoryAccessor for Task {
    fn maximum_valid_address(&self) -> Option<UserAddress> {
        self.mm().map(|mm| mm.maximum_valid_user_address)
    }
}

impl fmt::Debug for Task {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}:{}[{}]",
            self.thread_group.leader,
            self.id,
            self.persistent_info.lock().command.to_string_lossy()
        )
    }
}

impl cmp::PartialEq for Task {
    fn eq(&self, other: &Self) -> bool {
        let ptr: *const Task = self;
        let other_ptr: *const Task = other;
        ptr == other_ptr
    }
}

impl cmp::Eq for Task {}

impl From<&Task> for FsCred {
    fn from(t: &Task) -> FsCred {
        t.creds().into()
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::testing::*;
    use starnix_uapi::auth::{Capabilities, CAP_SYS_ADMIN};
    use starnix_uapi::resource_limits::Resource;
    use starnix_uapi::signals::SIGCHLD;
    use starnix_uapi::{rlimit, CLONE_SIGHAND, CLONE_THREAD, CLONE_VM};

    #[::fuchsia::test]
    async fn test_tid_allocation() {
        let (kernel, current_task, mut locked) = create_kernel_task_and_unlocked();

        assert_eq!(current_task.get_tid(), 1);
        let another_current = create_task(&mut locked, &kernel, "another-task");
        let another_tid = another_current.get_tid();
        assert!(another_tid >= 2);

        let pids = kernel.pids.read();
        assert_eq!(pids.get_task(1).upgrade().unwrap().get_tid(), 1);
        assert_eq!(pids.get_task(another_tid).upgrade().unwrap().get_tid(), another_tid);
    }

    #[::fuchsia::test]
    async fn test_clone_pid_and_parent_pid() {
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let thread = current_task.clone_task_for_test(
            &mut locked,
            (CLONE_THREAD | CLONE_VM | CLONE_SIGHAND) as u64,
            Some(SIGCHLD),
        );
        assert_eq!(current_task.get_pid(), thread.get_pid());
        assert_ne!(current_task.get_tid(), thread.get_tid());
        assert_eq!(current_task.thread_group.leader, thread.thread_group.leader);

        let child_task = current_task.clone_task_for_test(&mut locked, 0, Some(SIGCHLD));
        assert_ne!(current_task.get_pid(), child_task.get_pid());
        assert_ne!(current_task.get_tid(), child_task.get_tid());
        assert_eq!(current_task.get_pid(), child_task.thread_group.read().get_ppid());
    }

    #[::fuchsia::test]
    async fn test_root_capabilities() {
        let (_kernel, current_task) = create_kernel_and_task();
        assert!(current_task.creds().has_capability(CAP_SYS_ADMIN));
        assert_eq!(current_task.creds().cap_inheritable, Capabilities::empty());

        current_task.set_creds(Credentials::with_ids(1, 1));
        assert!(!current_task.creds().has_capability(CAP_SYS_ADMIN));
    }

    #[::fuchsia::test]
    async fn test_clone_rlimit() {
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let prev_fsize = current_task.thread_group.get_rlimit(Resource::FSIZE);
        assert_ne!(prev_fsize, 10);
        current_task
            .thread_group
            .limits
            .lock()
            .set(Resource::FSIZE, rlimit { rlim_cur: 10, rlim_max: 100 });
        let current_fsize = current_task.thread_group.get_rlimit(Resource::FSIZE);
        assert_eq!(current_fsize, 10);

        let child_task = current_task.clone_task_for_test(&mut locked, 0, Some(SIGCHLD));
        let child_fsize = child_task.thread_group.get_rlimit(Resource::FSIZE);
        assert_eq!(child_fsize, 10)
    }
}
