// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::arch::execution::new_syscall_from_state;
use crate::arch::LongPtr;
use crate::mm::{DumpPolicy, IOVecPtr, MemoryAccessor, MemoryAccessorExt};
use crate::security;
use crate::signals::syscalls::WaitingOptions;
use crate::signals::{
    send_signal_first, send_standard_signal, SignalDetail, SignalInfo, SignalInfoHeader,
    SI_HEADER_SIZE,
};
use crate::task::waiter::WaitQueue;
use crate::task::{
    CurrentTask, Kernel, PidTable, ProcessSelector, StopState, Task, TaskMutableState, ThreadGroup,
    ThreadState, ZombieProcess,
};
use crate::vfs::pseudo::simple_file::parse_unsigned_file;
use bitflags::bitflags;
use starnix_logging::track_stub;
use starnix_sync::{LockBefore, Locked, MmDumpable, ThreadGroupLimits, Unlocked};
use starnix_syscalls::decls::SyscallDecl;
use starnix_syscalls::SyscallResult;
use starnix_types::ownership::{OwnedRef, Releasable, ReleaseGuard, TempRef, WeakRef};
use starnix_uapi::auth::{CAP_SYS_PTRACE, PTRACE_MODE_ATTACH_REALCREDS};
use starnix_uapi::elf::ElfNoteType;
use starnix_uapi::errors::Errno;
use starnix_uapi::signals::{SigSet, Signal, UncheckedSignal, SIGKILL, SIGSTOP, SIGTRAP};
#[allow(unused_imports)]
use starnix_uapi::user_address::ArchSpecific;
use starnix_uapi::user_address::{MultiArchUserRef, UserAddress, UserRef};
use starnix_uapi::{
    clone_args, errno, error, pid_t, ptrace_syscall_info, tid_t, uapi, PTRACE_CONT, PTRACE_DETACH,
    PTRACE_EVENT_CLONE, PTRACE_EVENT_EXEC, PTRACE_EVENT_EXIT, PTRACE_EVENT_FORK,
    PTRACE_EVENT_SECCOMP, PTRACE_EVENT_STOP, PTRACE_EVENT_VFORK, PTRACE_EVENT_VFORK_DONE,
    PTRACE_GETEVENTMSG, PTRACE_GETREGSET, PTRACE_GETSIGINFO, PTRACE_GETSIGMASK,
    PTRACE_GET_SYSCALL_INFO, PTRACE_INTERRUPT, PTRACE_KILL, PTRACE_LISTEN, PTRACE_O_EXITKILL,
    PTRACE_O_TRACECLONE, PTRACE_O_TRACEEXEC, PTRACE_O_TRACEEXIT, PTRACE_O_TRACEFORK,
    PTRACE_O_TRACESYSGOOD, PTRACE_O_TRACEVFORK, PTRACE_O_TRACEVFORKDONE, PTRACE_PEEKDATA,
    PTRACE_PEEKTEXT, PTRACE_PEEKUSR, PTRACE_POKEDATA, PTRACE_POKETEXT, PTRACE_POKEUSR,
    PTRACE_SETOPTIONS, PTRACE_SETSIGINFO, PTRACE_SETSIGMASK, PTRACE_SYSCALL,
    PTRACE_SYSCALL_INFO_ENTRY, PTRACE_SYSCALL_INFO_EXIT, PTRACE_SYSCALL_INFO_NONE, SI_MAX_SIZE,
};

use std::collections::BTreeMap;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use zerocopy::FromBytes;

#[cfg(any(target_arch = "x86_64"))]
use starnix_uapi::{user, PTRACE_GETREGS};

type UserRegsStructPtr =
    MultiArchUserRef<starnix_uapi::user_regs_struct, starnix_uapi::arch32::user_regs_struct>;

uapi::check_arch_independent_layout! {
    ptrace_syscall_info {
        op,
        arch,
        instruction_pointer,
        stack_pointer,
        __bindgen_anon_1,
    }

    ptrace_syscall_info__bindgen_ty_1 {
        entry,
        exit,
        seccomp,
    }

    ptrace_syscall_info__bindgen_ty_1__bindgen_ty_1 {
        nr,
        args,
    }

    ptrace_syscall_info__bindgen_ty_1__bindgen_ty_2 {
        rval,
        is_error,
    }

    ptrace_syscall_info__bindgen_ty_1__bindgen_ty_3 {
        nr,
        args,
        ret_data,
    }
}

/// For most of the time, for the purposes of ptrace, a tracee is either "going"
/// or "stopped".  However, after certain ptrace calls, there are special rules
/// to be followed.
#[derive(Clone, Default, PartialEq)]
pub enum PtraceStatus {
    /// Proceed as otherwise indicated by the task's stop status.
    #[default]
    Default,
    /// Resuming after a ptrace_cont with a signal, so do not stop for signal-delivery-stop
    Continuing,
    /// "The state of the tracee after PTRACE_LISTEN is somewhat of a
    /// gray area: it is not in any ptrace-stop (ptrace commands won't work on it,
    /// and it will deliver waitpid(2) notifications), but it also may be considered
    /// "stopped" because it is not executing instructions (is not scheduled), and
    /// if it was in group-stop before PTRACE_LISTEN, it will not respond to signals
    /// until SIGCONT is received."
    Listening,
}

impl PtraceStatus {
    pub fn is_continuing(&self) -> bool {
        *self == PtraceStatus::Continuing
    }
}

/// Indicates the way that ptrace attached to the task.
#[derive(Copy, Clone, PartialEq)]
pub enum PtraceAttachType {
    /// Attached with PTRACE_ATTACH
    Attach,
    /// Attached with PTRACE_SEIZE
    Seize,
}

bitflags! {
    #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
    #[repr(transparent)]
    pub struct PtraceOptions: u32 {
        const EXITKILL = starnix_uapi::PTRACE_O_EXITKILL;
        const TRACECLONE = starnix_uapi::PTRACE_O_TRACECLONE;
        const TRACEEXEC = starnix_uapi::PTRACE_O_TRACEEXEC;
        const TRACEEXIT = starnix_uapi::PTRACE_O_TRACEEXIT;
        const TRACEFORK = starnix_uapi::PTRACE_O_TRACEFORK;
        const TRACESYSGOOD = starnix_uapi::PTRACE_O_TRACESYSGOOD;
        const TRACEVFORK = starnix_uapi::PTRACE_O_TRACEVFORK;
        const TRACEVFORKDONE = starnix_uapi::PTRACE_O_TRACEVFORKDONE;
        const TRACESECCOMP = starnix_uapi::PTRACE_O_TRACESECCOMP;
        const SUSPEND_SECCOMP = starnix_uapi::PTRACE_O_SUSPEND_SECCOMP;
    }
}

#[repr(u32)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum PtraceEvent {
    #[default]
    None = 0,
    Stop = PTRACE_EVENT_STOP,
    Clone = PTRACE_EVENT_CLONE,
    Fork = PTRACE_EVENT_FORK,
    Vfork = PTRACE_EVENT_VFORK,
    VforkDone = PTRACE_EVENT_VFORK_DONE,
    Exec = PTRACE_EVENT_EXEC,
    Exit = PTRACE_EVENT_EXIT,
    Seccomp = PTRACE_EVENT_SECCOMP,
}

impl PtraceEvent {
    pub fn from_option(option: &PtraceOptions) -> Self {
        match *option {
            PtraceOptions::TRACECLONE => PtraceEvent::Clone,
            PtraceOptions::TRACEFORK => PtraceEvent::Fork,
            PtraceOptions::TRACEVFORK => PtraceEvent::Vfork,
            PtraceOptions::TRACEVFORKDONE => PtraceEvent::VforkDone,
            PtraceOptions::TRACEEXEC => PtraceEvent::Exec,
            PtraceOptions::TRACEEXIT => PtraceEvent::Exit,
            PtraceOptions::TRACESECCOMP => PtraceEvent::Seccomp,
            _ => unreachable!("Bad ptrace event specified"),
        }
    }
}

/// Information about what caused a ptrace-event-stop.
pub struct PtraceEventData {
    /// The event that caused the task to stop (e.g., PTRACE_EVENT_TRACEFORK or PTRACE_EVENT_EXIT).
    pub event: PtraceEvent,

    /// The message associated with the event (e.g., tid, exit status)..
    pub msg: u64,
}

impl PtraceEventData {
    pub fn new(option: PtraceOptions, msg: u64) -> Self {
        Self { event: PtraceEvent::from_option(&option), msg }
    }
    pub fn new_from_event(event: PtraceEvent, msg: u64) -> Self {
        Self { event, msg }
    }
}

/// The ptrace state that a new task needs to connect to the same tracer as the
/// task that clones it.
#[derive(Clone)]
pub struct PtraceCoreState {
    /// The pid of the tracer
    pub pid: pid_t,

    /// Whether the attach was a seize or an attach.  There are a few subtle
    /// differences in behavior of the different attach types - see ptrace(2).
    pub attach_type: PtraceAttachType,

    /// The options set by PTRACE_SETOPTIONS
    pub options: PtraceOptions,

    /// The tracer waits on this WaitQueue to find out if the tracee has done
    /// something worth being notified about.
    pub tracer_waiters: Arc<WaitQueue>,
}

impl PtraceCoreState {
    pub fn has_option(&self, option: PtraceOptions) -> bool {
        self.options.contains(option)
    }
}

/// Per-task ptrace-related state
pub struct PtraceState {
    /// The core state of the tracer, which can be shared between processes
    pub core_state: PtraceCoreState,

    /// The tracee waits on this WaitQueue to find out when it should stop or wake
    /// for ptrace-related shenanigans.
    pub tracee_waiters: WaitQueue,

    /// The signal that caused the task to enter the given state (for
    /// signal-delivery-stop)
    pub last_signal: Option<SignalInfo>,

    /// Whether waitpid() will return the last signal.  The presence of last_signal
    /// can't be used for that, because that needs to be saved for GETSIGINFO.
    pub last_signal_waitable: bool,

    /// Data about the PTRACE_EVENT that caused the most recent stop (if any).
    pub event_data: Option<PtraceEventData>,

    /// Indicates whether the last ptrace call put this thread into a state with
    /// special semantics for stopping behavior.
    pub stop_status: PtraceStatus,

    /// For SYSCALL_INFO_EXIT
    pub last_syscall_was_error: bool,
}

impl PtraceState {
    pub fn new(pid: pid_t, attach_type: PtraceAttachType, options: PtraceOptions) -> Self {
        PtraceState {
            core_state: PtraceCoreState {
                pid,
                attach_type,
                options,
                tracer_waiters: Arc::new(WaitQueue::default()),
            },
            tracee_waiters: WaitQueue::default(),
            last_signal: None,
            last_signal_waitable: false,
            event_data: None,
            stop_status: PtraceStatus::default(),
            last_syscall_was_error: false,
        }
    }

    pub fn get_pid(&self) -> pid_t {
        self.core_state.pid
    }

    pub fn set_pid(&mut self, pid: pid_t) {
        self.core_state.pid = pid;
    }

    pub fn is_seized(&self) -> bool {
        self.core_state.attach_type == PtraceAttachType::Seize
    }

    pub fn get_attach_type(&self) -> PtraceAttachType {
        self.core_state.attach_type
    }

    pub fn is_waitable(&self, stop: StopState, options: &WaitingOptions) -> bool {
        if self.stop_status == PtraceStatus::Listening {
            // Waiting for any change of state
            return self.last_signal_waitable;
        }
        if !options.wait_for_continued && !stop.is_stopping_or_stopped() {
            // Only waiting for stops, but is not stopped.
            return false;
        }
        self.last_signal_waitable && !stop.is_in_progress()
    }

    pub fn set_last_signal(&mut self, mut signal: Option<SignalInfo>) {
        if let Some(ref mut siginfo) = signal {
            // We don't want waiters to think the process was unstopped because
            // of a sigkill. They will get woken when the process dies.
            if siginfo.signal == SIGKILL {
                return;
            }
            self.last_signal_waitable = true;
            self.last_signal = signal;
        }
    }

    pub fn set_last_event(&mut self, event: Option<PtraceEventData>) {
        if event.is_some() {
            self.event_data = event;
        }
    }

    // Gets the last signal, and optionally clears the wait state of the ptrace.
    pub fn get_last_signal(&mut self, keep_signal_waitable: bool) -> Option<SignalInfo> {
        self.last_signal_waitable = keep_signal_waitable;
        self.last_signal.clone()
    }

    pub fn has_option(&self, option: PtraceOptions) -> bool {
        self.core_state.has_option(option)
    }

    pub fn set_options_from_bits(&mut self, option: u32) -> Result<(), Errno> {
        if let Some(options) = PtraceOptions::from_bits(option) {
            self.core_state.options = options;
            Ok(())
        } else {
            error!(EINVAL)
        }
    }

    pub fn get_options(&self) -> PtraceOptions {
        self.core_state.options
    }

    /// Returns enough of the ptrace state to propagate it to a fork / clone / vforked task.
    pub fn get_core_state(&self) -> PtraceCoreState {
        self.core_state.clone()
    }

    pub fn tracer_waiters(&self) -> &Arc<WaitQueue> {
        &self.core_state.tracer_waiters
    }

    /// Returns an (i32, ptrace_syscall_info) pair.  The ptrace_syscall_info is
    /// the info associated with the syscall that the target task is currently
    /// blocked on, The i32 is (per ptrace(2)) "the number of bytes available to
    /// be written by the kernel.  If the size of the data to be written by the
    /// kernel exceeds the size specified by the addr argument, the output data
    /// is truncated."; ptrace(PTRACE_GET_SYSCALL_INFO) returns that value"
    pub fn get_target_syscall(
        &self,
        target: &Task,
        state: &TaskMutableState,
    ) -> Result<(i32, ptrace_syscall_info), Errno> {
        #[cfg(target_arch = "x86_64")]
        let arch = starnix_uapi::AUDIT_ARCH_X86_64;
        #[cfg(target_arch = "aarch64")]
        let arch = starnix_uapi::AUDIT_ARCH_AARCH64;
        #[cfg(target_arch = "riscv64")]
        let arch = starnix_uapi::AUDIT_ARCH_RISCV64;

        let mut info = ptrace_syscall_info { arch, ..Default::default() };
        let mut info_len = memoffset::offset_of!(ptrace_syscall_info, __bindgen_anon_1);

        match &state.captured_thread_state {
            Some(captured) => {
                let registers = captured.thread_state.registers;
                info.instruction_pointer = registers.instruction_pointer_register();
                info.stack_pointer = registers.stack_pointer_register();
                #[cfg(feature = "arch32")]
                if captured.thread_state.arch_width.is_arch32() {
                    // If any additional arch32 archs are added, just use a cfg
                    // macro here.
                    info.arch = starnix_uapi::AUDIT_ARCH_ARM;
                }
                match target.load_stopped() {
                    StopState::SyscallEnterStopped => {
                        let syscall_decl = SyscallDecl::from_number(
                            registers.syscall_register(),
                            captured.thread_state.arch_width,
                        );
                        let syscall = new_syscall_from_state(syscall_decl, &captured.thread_state);
                        info.op = PTRACE_SYSCALL_INFO_ENTRY as u8;
                        let entry = linux_uapi::ptrace_syscall_info__bindgen_ty_1__bindgen_ty_1 {
                            nr: syscall.decl.number,
                            args: [
                                syscall.arg0.raw(),
                                syscall.arg1.raw(),
                                syscall.arg2.raw(),
                                syscall.arg3.raw(),
                                syscall.arg4.raw(),
                                syscall.arg5.raw(),
                            ],
                        };
                        info_len += memoffset::offset_of!(
                            linux_uapi::ptrace_syscall_info__bindgen_ty_1__bindgen_ty_1,
                            args
                        ) + std::mem::size_of_val(&entry.args);
                        info.__bindgen_anon_1.entry = entry;
                    }
                    StopState::SyscallExitStopped => {
                        info.op = PTRACE_SYSCALL_INFO_EXIT as u8;
                        let exit = linux_uapi::ptrace_syscall_info__bindgen_ty_1__bindgen_ty_2 {
                            rval: registers.return_register() as i64,
                            is_error: state
                                .ptrace
                                .as_ref()
                                .map_or(0, |ptrace| ptrace.last_syscall_was_error as u8),
                            ..Default::default()
                        };
                        info_len += memoffset::offset_of!(
                            linux_uapi::ptrace_syscall_info__bindgen_ty_1__bindgen_ty_2,
                            is_error
                        ) + std::mem::size_of_val(&exit.is_error);
                        info.__bindgen_anon_1.exit = exit;
                    }
                    _ => {
                        info.op = PTRACE_SYSCALL_INFO_NONE as u8;
                    }
                };
            }
            _ => (),
        }
        Ok((info_len as i32, info))
    }

    /// Gets the core state for this ptrace if the options set on this ptrace
    /// match |trace_kind|.  Returns a pair: the trace option you *should* use
    /// (sometimes this is different from the one that the caller thinks it
    /// should use), and the core state.
    pub fn get_core_state_for_clone(
        &self,
        clone_args: &clone_args,
    ) -> (PtraceOptions, Option<PtraceCoreState>) {
        // ptrace(2): If the tracee calls clone(2) with the CLONE_VFORK flag,
        // PTRACE_EVENT_VFORK will be delivered instead if PTRACE_O_TRACEVFORK
        // is set, otherwise if the tracee calls clone(2) with the exit signal
        // set to SIGCHLD, PTRACE_EVENT_FORK will be delivered if
        // PTRACE_O_TRACEFORK is set.
        let trace_type = if clone_args.flags & (starnix_uapi::CLONE_UNTRACED as u64) != 0 {
            PtraceOptions::empty()
        } else {
            if clone_args.flags & (starnix_uapi::CLONE_VFORK as u64) != 0 {
                PtraceOptions::TRACEVFORK
            } else if clone_args.exit_signal != (starnix_uapi::SIGCHLD as u64) {
                PtraceOptions::TRACECLONE
            } else {
                PtraceOptions::TRACEFORK
            }
        };

        if !self.has_option(trace_type)
            && (clone_args.flags & (starnix_uapi::CLONE_PTRACE as u64) == 0)
        {
            return (PtraceOptions::empty(), None);
        }

        (trace_type, Some(self.get_core_state()))
    }
}

/// A zombie that must delivered to a tracer process for a traced process.
struct TracedZombie {
    /// An artificial zombie that must be delivered to the tracer program.
    artificial_zombie: ZombieProcess,

    /// An optional real zombie to be sent to the given ThreadGroup after the zomboe has been
    /// delivered to the tracer.
    delegate: Option<(WeakRef<ThreadGroup>, OwnedRef<ZombieProcess>)>,
}

impl Releasable for TracedZombie {
    type Context<'a: 'b, 'b> = &'a mut PidTable;

    fn release<'a: 'b, 'b>(self, pids: &mut PidTable) {
        self.artificial_zombie.release(pids);
        if let Some((_, z)) = self.delegate {
            z.release(pids);
        }
    }
}

impl TracedZombie {
    fn new(artificial_zombie: ZombieProcess) -> ReleaseGuard<Self> {
        ReleaseGuard::from(Self { artificial_zombie, delegate: None })
    }

    fn new_with_delegate(
        artificial_zombie: ZombieProcess,
        delegate: (WeakRef<ThreadGroup>, OwnedRef<ZombieProcess>),
    ) -> ReleaseGuard<Self> {
        ReleaseGuard::from(Self { artificial_zombie, delegate: Some(delegate) })
    }

    fn set_parent(
        &mut self,
        new_zombie: Option<OwnedRef<ZombieProcess>>,
        new_parent: &ThreadGroup,
    ) {
        if let Some(new_zombie) = new_zombie {
            self.delegate = Some((new_parent.weak_self.clone(), new_zombie));
        } else {
            self.delegate = self.delegate.take().map(|(_, z)| (new_parent.weak_self.clone(), z));
        }
    }
}

/// A list of zombie processes that were traced by a given tracer, but which
/// have not yet notified that tracer of their exit.  Once the tracer is
/// notified, the original parent will be notified.
#[derive(Default)]
pub struct ZombiePtraces {
    /// A list of zombies that have to be delivered to the ptracer.  The key is
    /// the tid of the traced process.
    zombies: BTreeMap<tid_t, ReleaseGuard<TracedZombie>>,
}

impl ZombiePtraces {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a zombie tracee to the list, but does not provide a parent task to
    /// notify when the tracer is done.
    pub fn add(&mut self, pids: &mut PidTable, tid: tid_t, zombie: ZombieProcess) {
        if let std::collections::btree_map::Entry::Vacant(entry) = self.zombies.entry(tid) {
            entry.insert(TracedZombie::new(zombie));
        } else {
            zombie.release(pids);
        }
    }

    /// Delete any zombie ptracees for the given tid.
    pub fn remove(&mut self, pids: &mut PidTable, tid: tid_t) {
        self.zombies.remove(&tid).release(pids);
    }

    /// Provide a parent task and a zombie to notify when the tracer has been
    /// notified.
    pub fn set_parent_of(
        &mut self,
        tracee: tid_t,
        new_zombie: Option<OwnedRef<ZombieProcess>>,
        new_parent: &ThreadGroup,
    ) {
        match self.zombies.entry(tracee) {
            std::collections::btree_map::Entry::Vacant(entry) => {
                if let Some(new_zombie) = new_zombie {
                    entry.insert(TracedZombie::new_with_delegate(
                        new_zombie.as_artificial(),
                        (new_parent.weak_self.clone(), new_zombie),
                    ));
                }
            }
            std::collections::btree_map::Entry::Occupied(mut entry) => {
                entry.get_mut().set_parent(new_zombie, new_parent);
            }
        }
    }

    /// When a parent dies without having been notified, replace it with a given
    /// new parent.
    pub fn reparent(old_parent: &ThreadGroup, new_parent: &ThreadGroup) {
        let mut lockless_list = old_parent.read().deferred_zombie_ptracers.clone();

        for deferred_zombie_ptracer in &lockless_list {
            if let Some(tg) = deferred_zombie_ptracer.tracer_thread_group_key.upgrade() {
                tg.write().zombie_ptracees.set_parent_of(
                    deferred_zombie_ptracer.tracee_tid,
                    None,
                    new_parent,
                );
            }
        }
        let mut new_state = new_parent.write();
        new_state.deferred_zombie_ptracers.append(&mut lockless_list);
    }

    /// Empty the table and notify all of the remaining parents.  Used if the
    /// tracer terminates or detaches without acknowledging all pending tracees.
    pub fn release(&mut self, pids: &mut PidTable) {
        let mut entry = self.zombies.pop_first();
        while let Some((_, mut zombie)) = entry {
            if let Some((tg, z)) = zombie.delegate.take() {
                if let Some(tg) = tg.upgrade() {
                    tg.do_zombie_notifications(z);
                }
            }
            zombie.release(pids);

            entry = self.zombies.pop_first();
        }
    }

    /// Returns true iff there is a zombie waiting to be delivered to the tracers matching the
    /// given selector.
    pub fn has_zombie_matching(&self, selector: &ProcessSelector) -> bool {
        self.zombies.values().any(|z| z.artificial_zombie.matches_selector(selector))
    }

    /// Returns true iff the given `tid` is a traced thread that needs to deliver a zombie to the
    /// tracer.
    pub fn has_tracee(&self, tid: tid_t) -> bool {
        self.zombies.contains_key(&tid)
    }

    /// Returns a zombie matching the given selector and options, and
    /// (optionally) a thread group to notify after the caller has consumed that
    /// zombie.
    pub fn get_waitable_entry(
        &mut self,
        selector: &ProcessSelector,
        options: &WaitingOptions,
    ) -> Option<(ZombieProcess, Option<(WeakRef<ThreadGroup>, OwnedRef<ZombieProcess>)>)> {
        // We look for the last zombie in the vector that matches pid
        // selector and waiting options
        let Some((t, found_zombie)) = self
            .zombies
            .iter()
            .map(|(t, z)| (*t, &z.artificial_zombie))
            .rfind(|(_, zombie)| zombie.matches_selector_and_waiting_option(selector, options))
        else {
            return None;
        };

        let result;
        if !options.keep_waitable_state {
            // Maybe notify child waiters.
            result = self.zombies.remove(&t).map(|traced_zombie| {
                let traced_zombie = ReleaseGuard::take(traced_zombie);
                (traced_zombie.artificial_zombie, traced_zombie.delegate)
            });
        } else {
            result = Some((found_zombie.as_artificial(), None));
        }

        result
    }
}

/// Scope definitions for Yama.  For full details, see ptrace(2).
/// 1 means tracer needs to have CAP_SYS_PTRACE or be a parent / child
/// process. This is the default.
const RESTRICTED_SCOPE: u8 = 1;
/// 2 means tracer needs to have CAP_SYS_PTRACE
const ADMIN_ONLY_SCOPE: u8 = 2;
/// 3 means no process can attach.
const NO_ATTACH_SCOPE: u8 = 3;

// PR_SET_PTRACER_ANY is defined as ((unsigned long) -1),
// which is not understood by bindgen.
pub const PR_SET_PTRACER_ANY: i32 = -1;

/// Indicates processes specifically allowed to trace a given process if using
/// RESTRICTED_SCOPE.  Used by prctl(PR_SET_PTRACER).
#[derive(Copy, Clone, Default, PartialEq)]
pub enum PtraceAllowedPtracers {
    #[default]
    None,
    Some(pid_t),
    Any,
}

/// Continues the target thread, optionally detaching from it.
/// |data| is treated as it is in PTRACE_CONT.
/// |new_status| is the PtraceStatus to set for this trace.
/// |detach| will cause the tracer to detach from the tracee.
fn ptrace_cont<L>(
    locked: &mut Locked<L>,
    tracee: &Task,
    data: &UserAddress,
    detach: bool,
) -> Result<(), Errno>
where
    L: LockBefore<ThreadGroupLimits>,
{
    let data = data.ptr() as u64;
    let new_state;
    let mut siginfo = if data != 0 {
        let signal = Signal::try_from(UncheckedSignal::new(data))?;
        Some(SignalInfo::default(signal))
    } else {
        None
    };

    let mut state = tracee.write();
    let is_listen = state.is_ptrace_listening();

    if tracee.load_stopped().is_waking_or_awake() && !is_listen {
        if detach {
            state.set_ptrace(None)?;
        }
        return error!(EIO);
    }

    if !state.can_accept_ptrace_commands() && !detach {
        return error!(ESRCH);
    }

    if let Some(ref mut ptrace) = &mut state.ptrace {
        if data != 0 {
            new_state = PtraceStatus::Continuing;
            if let Some(ref mut last_signal) = &mut ptrace.last_signal {
                if let Some(si) = siginfo {
                    let new_signal = si.signal;
                    last_signal.signal = new_signal;
                }
                siginfo = Some(last_signal.clone());
            }
        } else {
            new_state = PtraceStatus::Default;
            ptrace.last_signal = None;
            ptrace.event_data = None;
        }
        ptrace.stop_status = new_state;

        if is_listen {
            state.notify_ptracees();
        }
    }

    if let Some(siginfo) = siginfo {
        // This will wake up the task for us, and also release state
        send_signal_first(locked, &tracee, state, siginfo);
    } else {
        state.set_stopped(StopState::Waking, None, None, None);
        drop(state);
        tracee.thread_group().set_stopped(StopState::Waking, None, false);
    }
    if detach {
        tracee.write().set_ptrace(None)?;
    }
    Ok(())
}

fn ptrace_interrupt(tracee: &Task) -> Result<(), Errno> {
    let mut state = tracee.write();
    if let Some(ref mut ptrace) = &mut state.ptrace {
        if !ptrace.is_seized() {
            return error!(EIO);
        }
        let status = ptrace.stop_status.clone();
        ptrace.stop_status = PtraceStatus::Default;
        let event_data = Some(PtraceEventData::new_from_event(PtraceEvent::Stop, 0));
        if status == PtraceStatus::Listening {
            let signal = ptrace.last_signal.clone();
            // "If the tracee was already stopped by a signal and PTRACE_LISTEN
            // was sent to it, the tracee stops with PTRACE_EVENT_STOP and
            // WSTOPSIG(status) returns the stop signal"
            state.set_stopped(StopState::PtraceEventStopped, signal, None, event_data);
        } else {
            state.set_stopped(
                StopState::PtraceEventStopping,
                Some(SignalInfo::default(SIGTRAP)),
                None,
                event_data,
            );
            drop(state);
            tracee.interrupt();
        }
    }
    Ok(())
}

fn ptrace_listen(tracee: &Task) -> Result<(), Errno> {
    let mut state = tracee.write();
    if let Some(ref mut ptrace) = &mut state.ptrace {
        if !ptrace.is_seized()
            || (ptrace.last_signal_waitable
                && ptrace
                    .event_data
                    .as_ref()
                    .is_some_and(|event_data| event_data.event != PtraceEvent::Stop))
        {
            return error!(EIO);
        }
        ptrace.stop_status = PtraceStatus::Listening;
    }
    Ok(())
}

pub fn ptrace_detach<L>(
    locked: &mut Locked<L>,
    pids: &mut PidTable,
    thread_group: &ThreadGroup,
    tracee: &Task,
    data: &UserAddress,
) -> Result<(), Errno>
where
    L: LockBefore<ThreadGroupLimits>,
{
    if let Err(x) = ptrace_cont(locked, &tracee, &data, true) {
        return Err(x);
    }
    let tid = tracee.get_tid();
    thread_group.ptracees.lock().remove(&tid);
    thread_group.write().zombie_ptracees.remove(pids, tid);
    Ok(())
}

/// For all ptrace requests that require an attached tracee
pub fn ptrace_dispatch<L>(
    locked: &mut Locked<L>,
    current_task: &mut CurrentTask,
    request: u32,
    pid: pid_t,
    addr: UserAddress,
    data: UserAddress,
) -> Result<SyscallResult, Errno>
where
    L: LockBefore<ThreadGroupLimits>,
{
    let weak_task = current_task.kernel().pids.read().get_task(pid);
    let tracee = weak_task.upgrade().ok_or_else(|| errno!(ESRCH))?;

    if let Some(ptrace) = &tracee.read().ptrace {
        if ptrace.get_pid() != current_task.get_pid() {
            return error!(ESRCH);
        }
    }

    // These requests may be run without the thread in a stop state, or
    // check the stop state themselves.
    match request {
        PTRACE_KILL => {
            let mut siginfo = SignalInfo::default(SIGKILL);
            siginfo.code = (linux_uapi::SIGTRAP | PTRACE_KILL << 8) as i32;
            send_standard_signal(locked, &tracee, siginfo);
            return Ok(starnix_syscalls::SUCCESS);
        }
        PTRACE_INTERRUPT => {
            ptrace_interrupt(tracee.as_ref())?;
            return Ok(starnix_syscalls::SUCCESS);
        }
        PTRACE_LISTEN => {
            ptrace_listen(&tracee)?;
            return Ok(starnix_syscalls::SUCCESS);
        }
        PTRACE_CONT => {
            ptrace_cont(locked, &tracee, &data, false)?;
            return Ok(starnix_syscalls::SUCCESS);
        }
        PTRACE_SYSCALL => {
            tracee.trace_syscalls.store(true, std::sync::atomic::Ordering::Relaxed);
            ptrace_cont(locked, &tracee, &data, false)?;
            return Ok(starnix_syscalls::SUCCESS);
        }
        PTRACE_DETACH => {
            let mut pids = current_task.kernel().pids.write();
            ptrace_detach(locked, &mut pids, current_task.thread_group(), tracee.as_ref(), &data)?;
            return Ok(starnix_syscalls::SUCCESS);
        }
        _ => {}
    }

    // The remaining requests (to be added) require the thread to be stopped.
    let mut state = tracee.write();
    if !state.can_accept_ptrace_commands() {
        return error!(ESRCH);
    }

    match request {
        PTRACE_PEEKDATA | PTRACE_PEEKTEXT => {
            let Some(ref mut captured) = &mut state.captured_thread_state else {
                return error!(ESRCH);
            };

            // NB: The behavior of the syscall is different from the behavior in ptrace(2),
            // which is provided by libc.
            let src = LongPtr::new(captured, addr);
            let val = tracee.read_multi_arch_object(src)?;

            let dst = LongPtr::new(&src, data);
            current_task.write_multi_arch_object(dst, val)?;
            Ok(starnix_syscalls::SUCCESS)
        }
        PTRACE_POKEDATA | PTRACE_POKETEXT => {
            let Some(ref mut captured) = &mut state.captured_thread_state else {
                return error!(ESRCH);
            };

            let src = LongPtr::new(captured, addr);
            let val = data.ptr() as u64;
            tracee.write_multi_arch_object(src, val)?;
            Ok(starnix_syscalls::SUCCESS)
        }
        PTRACE_PEEKUSR => {
            let Some(ref mut captured) = &mut state.captured_thread_state else {
                return error!(ESRCH);
            };

            let dst = LongPtr::new(captured, data);
            let val = ptrace_peekuser(&mut captured.thread_state, addr.ptr() as usize)?;
            current_task.write_multi_arch_object(dst, val as u64)?;
            return Ok(starnix_syscalls::SUCCESS);
        }
        PTRACE_POKEUSR => {
            ptrace_pokeuser(&mut *state, data.ptr() as usize, addr.ptr() as usize)?;
            return Ok(starnix_syscalls::SUCCESS);
        }
        PTRACE_GETREGSET => {
            if let Some(ref mut captured) = state.captured_thread_state {
                let uiv = IOVecPtr::new(current_task, data);
                let mut iv = current_task.read_multi_arch_object(uiv)?;
                let base = iv.iov_base.addr;
                let mut len = iv.iov_len as usize;
                ptrace_getregset(
                    current_task,
                    &mut captured.thread_state,
                    ElfNoteType::try_from(addr.ptr() as usize)?,
                    base,
                    &mut len,
                )?;
                iv.iov_len = len as u64;
                current_task.write_multi_arch_object(uiv, iv)?;
                return Ok(starnix_syscalls::SUCCESS);
            }
            error!(ESRCH)
        }
        #[cfg(target_arch = "x86_64")]
        PTRACE_GETREGS => {
            if let Some(ref mut captured) = &mut state.captured_thread_state {
                let mut len = usize::MAX;
                ptrace_getregset(
                    current_task,
                    &mut captured.thread_state,
                    ElfNoteType::PrStatus,
                    data.ptr() as u64,
                    &mut len,
                )?;
                return Ok(starnix_syscalls::SUCCESS);
            }
            error!(ESRCH)
        }
        PTRACE_SETSIGMASK => {
            // addr is the size of the buffer pointed to
            // by data, but has to be sizeof(sigset_t).
            if addr.ptr() != std::mem::size_of::<SigSet>() {
                return error!(EINVAL);
            }
            // sigset comes from *data.
            let src: UserRef<SigSet> = UserRef::from(data);
            let val = current_task.read_object(src)?;
            state.set_signal_mask(val);

            Ok(starnix_syscalls::SUCCESS)
        }
        PTRACE_GETSIGMASK => {
            // addr is the size of the buffer pointed to
            // by data, but has to be sizeof(sigset_t).
            if addr.ptr() != std::mem::size_of::<SigSet>() {
                return error!(EINVAL);
            }
            // sigset goes in *data.
            let dst: UserRef<SigSet> = UserRef::from(data);
            let val = state.signal_mask();
            current_task.write_object(dst, &val)?;
            Ok(starnix_syscalls::SUCCESS)
        }
        PTRACE_GETSIGINFO => {
            if let Some(ptrace) = &state.ptrace {
                if let Some(signal) = ptrace.last_signal.as_ref() {
                    let dst = MultiArchUserRef::<uapi::siginfo_t, uapi::arch32::siginfo_t>::new(
                        current_task,
                        data,
                    );
                    signal.write(current_task, dst)?;
                } else {
                    return error!(EINVAL);
                }
            }
            Ok(starnix_syscalls::SUCCESS)
        }
        PTRACE_SETSIGINFO => {
            // Rust will let us do this cast in a const assignment but not in a
            // const generic constraint.
            const SI_MAX_SIZE_AS_USIZE: usize = SI_MAX_SIZE as usize;

            let siginfo_mem = current_task.read_memory_to_array::<SI_MAX_SIZE_AS_USIZE>(data)?;
            let header = SignalInfoHeader::read_from_bytes(&siginfo_mem[..SI_HEADER_SIZE]).unwrap();

            let mut bytes = [0u8; SI_MAX_SIZE as usize - SI_HEADER_SIZE];
            bytes.copy_from_slice(&siginfo_mem[SI_HEADER_SIZE..SI_MAX_SIZE as usize]);
            let details = SignalDetail::Raw { data: bytes };
            let unchecked_signal = UncheckedSignal::new(header.signo as u64);
            let signal = Signal::try_from(unchecked_signal)?;

            let siginfo = SignalInfo {
                signal,
                errno: header.errno,
                code: header.code,
                detail: details,
                force: false,
            };
            if let Some(ref mut ptrace) = &mut state.ptrace {
                ptrace.last_signal = Some(siginfo);
            }
            Ok(starnix_syscalls::SUCCESS)
        }
        PTRACE_GET_SYSCALL_INFO => {
            if let Some(ptrace) = &state.ptrace {
                let (size, info) = ptrace.get_target_syscall(&tracee, &state)?;
                let dst: UserRef<ptrace_syscall_info> = UserRef::from(data);
                let len = std::cmp::min(std::mem::size_of::<ptrace_syscall_info>(), addr.ptr());
                // SAFETY: ptrace_syscall_info does not implement FromBytes/IntoBytes,
                // so this has to happen manually.
                let src = unsafe {
                    std::slice::from_raw_parts(
                        &info as *const ptrace_syscall_info as *const u8,
                        len as usize,
                    )
                };
                current_task.write_memory(dst.addr(), src)?;
                Ok(size.into())
            } else {
                error!(ESRCH)
            }
        }
        PTRACE_SETOPTIONS => {
            let mask = data.ptr() as u32;
            // This is what we currently support.
            if mask != 0
                && (mask
                    & !(PTRACE_O_TRACESYSGOOD
                        | PTRACE_O_TRACECLONE
                        | PTRACE_O_TRACEFORK
                        | PTRACE_O_TRACEVFORK
                        | PTRACE_O_TRACEVFORKDONE
                        | PTRACE_O_TRACEEXEC
                        | PTRACE_O_TRACEEXIT
                        | PTRACE_O_EXITKILL)
                    != 0)
            {
                track_stub!(TODO("https://fxbug.dev/322874463"), "ptrace(PTRACE_SETOPTIONS)", mask);
                return error!(ENOSYS);
            }
            if let Some(ref mut ptrace) = &mut state.ptrace {
                ptrace.set_options_from_bits(mask)?;
            }
            Ok(starnix_syscalls::SUCCESS)
        }
        PTRACE_GETEVENTMSG => {
            if let Some(ptrace) = &state.ptrace {
                if let Some(event_data) = &ptrace.event_data {
                    let dst = LongPtr::new(current_task, data);
                    current_task.write_multi_arch_object(dst, event_data.msg)?;
                    return Ok(starnix_syscalls::SUCCESS);
                }
            }
            error!(EIO)
        }
        _ => {
            track_stub!(TODO("https://fxbug.dev/322874463"), "ptrace", request);
            error!(ENOSYS)
        }
    }
}

/// Makes the given thread group trace the given task.
fn do_attach(
    thread_group: &ThreadGroup,
    task: WeakRef<Task>,
    attach_type: PtraceAttachType,
    options: PtraceOptions,
) -> Result<(), Errno> {
    if let Some(task_ref) = task.upgrade() {
        thread_group.ptracees.lock().insert(task_ref.get_tid(), (&task_ref).into());
        {
            let process_state = &mut task_ref.thread_group().write();
            let mut state = task_ref.write();
            state.set_ptrace(Some(PtraceState::new(thread_group.leader, attach_type, options)))?;
            // If the tracee is already stopped, make sure that the tracer can
            // identify that right away.
            if process_state.is_waitable()
                && process_state.base.load_stopped() == StopState::GroupStopped
                && task_ref.load_stopped() == StopState::GroupStopped
            {
                if let Some(ref mut ptrace) = &mut state.ptrace {
                    ptrace.last_signal_waitable = true;
                }
            }
        }
        return Ok(());
    }
    // The tracee is either the current thread, or there is a live ref to it outside
    // this function.
    unreachable!("Tracee thread not found");
}

fn check_caps_for_attach(ptrace_scope: u8, current_task: &CurrentTask) -> Result<(), Errno> {
    if ptrace_scope == ADMIN_ONLY_SCOPE {
        // Admin only use of ptrace
        security::check_task_capable(current_task, CAP_SYS_PTRACE)?;
    }
    if ptrace_scope == NO_ATTACH_SCOPE {
        // No use of ptrace
        return error!(EPERM);
    }
    Ok(())
}

/// Uses the given core ptrace state (including tracer, attach type, etc) to
/// attach to another task, given by `tracee_task`.  Also sends a signal to stop
/// tracee_task.  Typical for when inheriting ptrace state from another task.
pub fn ptrace_attach_from_state<L>(
    locked: &mut Locked<L>,
    tracee_task: &OwnedRef<Task>,
    ptrace_state: PtraceCoreState,
) -> Result<(), Errno>
where
    L: LockBefore<ThreadGroupLimits>,
{
    {
        let weak_tg = tracee_task
            .thread_group()
            .kernel
            .pids
            .read()
            .get_thread_group(ptrace_state.pid)
            .map(TempRef::into_static);
        let tracer_tg = weak_tg.ok_or_else(|| errno!(ESRCH))?;
        do_attach(
            &tracer_tg,
            WeakRef::from(tracee_task),
            ptrace_state.attach_type,
            ptrace_state.options,
        )?;
    }
    let mut state = tracee_task.write();
    if let Some(ref mut ptrace) = &mut state.ptrace {
        ptrace.core_state.tracer_waiters = Arc::clone(&ptrace_state.tracer_waiters);
    }

    // The newly started tracee starts with a signal that depends on the attach type.
    let signal = if ptrace_state.attach_type == PtraceAttachType::Seize {
        if let Some(ref mut ptrace) = &mut state.ptrace {
            ptrace.set_last_event(Some(PtraceEventData::new_from_event(PtraceEvent::Stop, 0)));
        }
        SignalInfo::default(SIGTRAP)
    } else {
        SignalInfo::default(SIGSTOP)
    };
    send_signal_first(locked, tracee_task, state, signal);

    Ok(())
}

pub fn ptrace_traceme(current_task: &mut CurrentTask) -> Result<SyscallResult, Errno> {
    let ptrace_scope = current_task.kernel().ptrace_scope.load(Ordering::Relaxed);
    check_caps_for_attach(ptrace_scope, current_task)?;

    let parent = current_task.thread_group().read().parent.clone();
    if let Some(parent) = parent {
        let parent = parent.upgrade();
        // TODO: Move this check into `do_attach()` so that there is a single `ptrace_access_check(tracer, tracee)`?
        {
            let pids = current_task.kernel().pids.read();
            let parent_task = pids.get_task(parent.leader);
            security::ptrace_traceme(
                current_task,
                parent_task.upgrade().ok_or_else(|| errno!(EINVAL))?.as_ref(),
            )?;
        }

        let task_ref = OwnedRef::temp(&current_task.task);
        do_attach(&parent, (&task_ref).into(), PtraceAttachType::Attach, PtraceOptions::empty())?;
        Ok(starnix_syscalls::SUCCESS)
    } else {
        error!(EPERM)
    }
}

pub fn ptrace_attach<L>(
    locked: &mut Locked<L>,
    current_task: &mut CurrentTask,
    pid: pid_t,
    attach_type: PtraceAttachType,
    data: UserAddress,
) -> Result<SyscallResult, Errno>
where
    L: LockBefore<MmDumpable>,
{
    let ptrace_scope = current_task.kernel().ptrace_scope.load(Ordering::Relaxed);
    check_caps_for_attach(ptrace_scope, current_task)?;

    let weak_task = current_task.kernel().pids.read().get_task(pid);
    let tracee = weak_task.upgrade().ok_or_else(|| errno!(ESRCH))?;
    security::ptrace_access_check(&current_task, &tracee)?;

    if tracee.thread_group == current_task.thread_group {
        return error!(EPERM);
    }

    if *tracee.mm().ok_or_else(|| errno!(EINVAL))?.dumpable.lock(locked) == DumpPolicy::Disable {
        return error!(EPERM);
    }

    if ptrace_scope == RESTRICTED_SCOPE {
        // This only allows us to attach to descendants and tasks that have
        // explicitly allowlisted us with PR_SET_PTRACER.
        let mut ttg = tracee.thread_group().read().parent.clone();
        let mut is_parent = false;
        let my_pid = current_task.thread_group().leader;
        while let Some(target) = ttg {
            let target = target.upgrade();
            if target.as_ref().leader == my_pid {
                is_parent = true;
                break;
            }
            ttg = target.read().parent.clone();
        }
        if !is_parent {
            match tracee.thread_group().read().allowed_ptracers {
                PtraceAllowedPtracers::None => return error!(EPERM),
                PtraceAllowedPtracers::Some(pid) => {
                    if my_pid != pid {
                        return error!(EPERM);
                    }
                }
                PtraceAllowedPtracers::Any => {}
            }
        }
    }

    current_task.check_ptrace_access_mode(locked, PTRACE_MODE_ATTACH_REALCREDS, &tracee)?;
    do_attach(current_task.thread_group(), weak_task.clone(), attach_type, PtraceOptions::empty())?;
    if attach_type == PtraceAttachType::Attach {
        send_standard_signal(
            &mut locked.cast_locked::<MmDumpable>(),
            &tracee,
            SignalInfo::default(SIGSTOP),
        );
    } else if attach_type == PtraceAttachType::Seize {
        // When seizing, |data| should be used as the options bitmask.
        if let Some(task_ref) = weak_task.upgrade() {
            let mut state = task_ref.write();
            if let Some(ref mut ptrace) = &mut state.ptrace {
                ptrace.set_options_from_bits(data.ptr() as u32)?;
            }
        }
    }
    Ok(starnix_syscalls::SUCCESS)
}

/// Implementation of ptrace(PTRACE_PEEKUSER).  The user struct holds the
/// registers and other information about the process.  See ptrace(2) and
/// sys/user.h for full details.
pub fn ptrace_peekuser(thread_state: &mut ThreadState, offset: usize) -> Result<usize, Errno> {
    #[cfg(any(target_arch = "x86_64"))]
    if offset >= std::mem::size_of::<user>() {
        return error!(EIO);
    }
    if offset < UserRegsStructPtr::size_of_object_for(thread_state) {
        let result = thread_state.get_user_register(offset)?;
        return Ok(result);
    }
    error!(EIO)
}

pub fn ptrace_pokeuser(
    state: &mut TaskMutableState,
    value: usize,
    offset: usize,
) -> Result<(), Errno> {
    if let Some(ref mut thread_state) = state.captured_thread_state {
        thread_state.dirty = true;

        #[cfg(any(target_arch = "x86_64"))]
        if offset >= std::mem::size_of::<user>() {
            return error!(EIO);
        }
        if offset < UserRegsStructPtr::size_of_object_for(thread_state) {
            return thread_state.thread_state.set_user_register(offset, value);
        }
    }
    error!(EIO)
}

pub fn ptrace_getregset(
    current_task: &CurrentTask,
    thread_state: &mut ThreadState,
    regset_type: ElfNoteType,
    base: u64,
    len: &mut usize,
) -> Result<(), Errno> {
    match regset_type {
        ElfNoteType::PrStatus => {
            let user_regs_struct_len = UserRegsStructPtr::size_of_object_for(thread_state);
            if *len < user_regs_struct_len {
                return error!(EINVAL);
            }
            *len = std::cmp::min(*len, user_regs_struct_len);
            let mut i: usize = 0;
            let mut reg_ptr = LongPtr::new(thread_state, base);
            while i < *len {
                let mut val = None;
                thread_state
                    .registers
                    .apply_user_register(i, &mut |register| val = Some(*register as usize))?;
                if let Some(val) = val {
                    current_task.write_multi_arch_object(reg_ptr, val as u64)?;
                }
                i += reg_ptr.size_of_object();
                reg_ptr = reg_ptr.next()?;
            }
            Ok(())
        }
        _ => {
            error!(EINVAL)
        }
    }
}

pub fn ptrace_set_scope(kernel: &Kernel, data: &[u8]) -> Result<(), Errno> {
    loop {
        let ptrace_scope = kernel.ptrace_scope.load(Ordering::Relaxed);
        if let Ok(val) = parse_unsigned_file::<u8>(data) {
            // Legal values are 0<=val<=3, unless scope is NO_ATTACH - see Yama
            // documentation.
            if ptrace_scope == NO_ATTACH_SCOPE && val != NO_ATTACH_SCOPE {
                return error!(EINVAL);
            }
            if val <= NO_ATTACH_SCOPE {
                match kernel.ptrace_scope.compare_exchange(
                    ptrace_scope,
                    val,
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => return Ok(()),
                    Err(_) => continue,
                }
            }
        }
        return error!(EINVAL);
    }
}

pub fn ptrace_get_scope(kernel: &Kernel) -> Vec<u8> {
    let mut scope = kernel.ptrace_scope.load(Ordering::Relaxed).to_string();
    scope.push('\n');
    scope.into_bytes()
}

#[inline(never)]
pub fn ptrace_syscall_enter(locked: &mut Locked<Unlocked>, current_task: &mut CurrentTask) {
    let block = {
        let mut state = current_task.write();
        if state.ptrace.is_some() {
            current_task.trace_syscalls.store(false, Ordering::Relaxed);
            let mut sig = SignalInfo::default(SIGTRAP);
            sig.code = (linux_uapi::SIGTRAP | 0x80) as i32;
            if state
                .ptrace
                .as_ref()
                .is_some_and(|ptrace| ptrace.has_option(PtraceOptions::TRACESYSGOOD))
            {
                sig.signal.set_ptrace_syscall_bit();
            }
            state.set_stopped(StopState::SyscallEnterStopping, Some(sig), None, None);
            true
        } else {
            false
        }
    };
    if block {
        current_task.block_while_stopped(locked);
    }
}

#[inline(never)]
pub fn ptrace_syscall_exit(
    locked: &mut Locked<Unlocked>,
    current_task: &mut CurrentTask,
    is_error: bool,
) {
    let block = {
        let mut state = current_task.write();
        current_task.trace_syscalls.store(false, Ordering::Relaxed);
        if state.ptrace.is_some() {
            let mut sig = SignalInfo::default(SIGTRAP);
            sig.code = (linux_uapi::SIGTRAP | 0x80) as i32;
            if state
                .ptrace
                .as_ref()
                .is_some_and(|ptrace| ptrace.has_option(PtraceOptions::TRACESYSGOOD))
            {
                sig.signal.set_ptrace_syscall_bit();
            }

            state.set_stopped(StopState::SyscallExitStopping, Some(sig), None, None);
            if let Some(ref mut ptrace) = &mut state.ptrace {
                ptrace.last_syscall_was_error = is_error;
            }
            true
        } else {
            false
        }
    };
    if block {
        current_task.block_while_stopped(locked);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task::syscalls::sys_prctl;
    use crate::testing::{create_kernel_task_and_unlocked, create_task};
    use starnix_uapi::PR_SET_PTRACER;

    #[::fuchsia::test]
    async fn test_set_ptracer() {
        let (kernel, mut tracee, mut locked) = create_kernel_task_and_unlocked();
        let mut tracer = create_task(&mut locked, &kernel, "tracer");
        kernel.ptrace_scope.store(RESTRICTED_SCOPE, Ordering::Relaxed);
        assert_eq!(
            sys_prctl(&mut locked, &mut tracee, PR_SET_PTRACER, 0xFFF, 0, 0, 0),
            error!(EINVAL)
        );

        assert_eq!(
            ptrace_attach(
                &mut locked,
                &mut tracer,
                tracee.as_ref().task.tid,
                PtraceAttachType::Attach,
                UserAddress::NULL,
            ),
            error!(EPERM)
        );

        assert!(sys_prctl(
            &mut locked,
            &mut tracee,
            PR_SET_PTRACER,
            tracer.thread_group().leader as u64,
            0,
            0,
            0
        )
        .is_ok());

        let mut not_tracer = create_task(&mut locked, &kernel, "not-tracer");
        assert_eq!(
            ptrace_attach(
                &mut locked,
                &mut not_tracer,
                tracee.as_ref().task.tid,
                PtraceAttachType::Attach,
                UserAddress::NULL,
            ),
            error!(EPERM)
        );

        assert!(ptrace_attach(
            &mut locked,
            &mut tracer,
            tracee.as_ref().task.tid,
            PtraceAttachType::Attach,
            UserAddress::NULL,
        )
        .is_ok());
    }

    #[::fuchsia::test]
    async fn test_set_ptracer_any() {
        let (kernel, mut tracee, mut locked) = create_kernel_task_and_unlocked();
        let mut tracer = create_task(&mut locked, &kernel, "tracer");
        kernel.ptrace_scope.store(RESTRICTED_SCOPE, Ordering::Relaxed);
        assert_eq!(
            sys_prctl(&mut locked, &mut tracee, PR_SET_PTRACER, 0xFFF, 0, 0, 0),
            error!(EINVAL)
        );

        assert_eq!(
            ptrace_attach(
                &mut locked,
                &mut tracer,
                tracee.as_ref().task.tid,
                PtraceAttachType::Attach,
                UserAddress::NULL,
            ),
            error!(EPERM)
        );

        assert!(sys_prctl(
            &mut locked,
            &mut tracee,
            PR_SET_PTRACER,
            PR_SET_PTRACER_ANY as u64,
            0,
            0,
            0
        )
        .is_ok());

        assert!(ptrace_attach(
            &mut locked,
            &mut tracer,
            tracee.as_ref().task.tid,
            PtraceAttachType::Attach,
            UserAddress::NULL,
        )
        .is_ok());
    }
}
