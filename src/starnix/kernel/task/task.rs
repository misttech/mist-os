// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fuchsia_zircon::{self as zx, sys::zx_thread_state_general_regs_t, AsHandleRef, Signals};
use once_cell::sync::OnceCell;
use std::cmp;
use std::convert::TryFrom;
use std::ffi::CString;
use std::fmt;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;
use ubpf::converter::{bpf_addressing_mode, bpf_class};
use ubpf::program::EbpfProgram;

use crate::arch::{registers::RegisterState, task::decode_page_fault_exception_report};
use crate::auth::*;
use crate::execution::*;
use crate::fs::*;
use crate::loader::*;
use crate::lock::{Mutex, RwLock, RwLockReadGuard, RwLockWriteGuard};
use crate::logging::{self, *};
use crate::mm::{MemoryAccessorExt, MemoryManager};
use crate::signals::{send_signal, types::*, SignalInfo};
use crate::syscalls::{decls::Syscall, SyscallResult};
use crate::task::*;
use crate::types::*;

// In user space, priority (niceness) is an integer from -20..19 (inclusive)
// with the default being 0.
//
// In the kernel it is represented as a range from 1..40 (inclusive).
// The conversion is done by the formula: user_nice = 20 - kernel_nice.
//
// See https://man7.org/linux/man-pages/man2/setpriority.2.html#NOTES
const DEFAULT_TASK_PRIORITY: u8 = 20;

const SECCOMP_MAX_INSNS_PER_PATH: u16 = 32768;

pub struct CurrentTask {
    pub task: Arc<Task>,

    /// A copy of the registers associated with the Zircon thread. Up-to-date values can be read
    /// from `self.handle.read_state_general_regs()`. To write these values back to the thread, call
    /// `self.handle.write_state_general_regs(self.registers.into())`.
    pub registers: RegisterState,

    /// The address of the DT_DEBUG entry in the process' ELF file.
    ///
    /// The value of the DT_DEBUG entry is a pointer to the `r_debug` symbol in the dynamic linker.
    /// This struct contains a link map, which the debug agent can use to determine which shared
    /// objects have been loaded.
    ///
    /// The lifecycle of this value is as follows (assuming the tag is present):
    ///   1. Starnix finds the address of the DT_DEBUG entry and sets `debug_address`.
    ///   2. The dynamic linker sets the value of the DT_DEBUG entry to the address of its `r_debug`
    ///      symbol.
    ///   3. Starnix reads the address from the DT_DEBUG entry, writes the address to the process'
    ///      debug address property, and sets `debug_address` to `None`.
    pub dt_debug_address: Option<UserAddress>,

    /// A custom function to resume a syscall that has been interrupted by SIGSTOP.
    /// To use, call set_syscall_restart_func and return ERESTART_RESTARTBLOCK. sys_restart_syscall
    /// will eventually call it.
    pub syscall_restart_func: Option<Box<SyscallRestartFunc>>,
}

type SyscallRestartFunc =
    dyn FnOnce(&mut CurrentTask) -> Result<SyscallResult, Errno> + Send + Sync;

impl std::ops::Drop for CurrentTask {
    fn drop(&mut self) {
        self.task.destroy_do_not_use_outside_of_drop_if_possible();
    }
}

impl std::ops::Deref for CurrentTask {
    type Target = Task;
    fn deref(&self) -> &Self::Target {
        &self.task
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ExitStatus {
    Exit(u8),
    Kill(SignalInfo),
    CoreDump(SignalInfo),
    Stop(SignalInfo),
    Continue(SignalInfo),
}
impl ExitStatus {
    /// Converts the given exit status to a status code suitable for returning from wait syscalls.
    pub fn wait_status(&self) -> i32 {
        match self {
            ExitStatus::Exit(status) => (*status as i32) << 8,
            ExitStatus::Kill(siginfo) => siginfo.signal.number() as i32,
            ExitStatus::CoreDump(siginfo) => (siginfo.signal.number() as i32) | 0x80,
            ExitStatus::Continue(_) => 0xffff,
            ExitStatus::Stop(siginfo) => 0x7f + ((siginfo.signal.number() as i32) << 8),
        }
    }

    pub fn signal_info_code(&self) -> u32 {
        match self {
            ExitStatus::Exit(_) => CLD_EXITED,
            ExitStatus::Kill(_) => CLD_KILLED,
            ExitStatus::CoreDump(_) => CLD_DUMPED,
            ExitStatus::Stop(_) => CLD_STOPPED,
            ExitStatus::Continue(_) => CLD_CONTINUED,
        }
    }

    pub fn signal_info_status(&self) -> i32 {
        match self {
            ExitStatus::Exit(status) => *status as i32,
            ExitStatus::Kill(siginfo)
            | ExitStatus::CoreDump(siginfo)
            | ExitStatus::Continue(siginfo)
            | ExitStatus::Stop(siginfo) => siginfo.signal.number() as i32,
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub enum SchedulerPolicy {
    #[default]
    Normal,
    Batch,
    Idle,
    Fifo {
        priority: u32,
    },
    RoundRobin {
        priority: u32,
    },
}

impl SchedulerPolicy {
    pub fn from_raw(policy: u32, params: sched_param, rlimit: u64) -> Result<Self, Errno> {
        let valid_priorities =
            min_priority_for_sched_policy(policy)?..=max_priority_for_sched_policy(policy)?;
        if !valid_priorities.contains(&params.sched_priority) {
            return error!(EINVAL);
        }
        // ok to cast i32->u64, above range excludes negatives
        match (policy, params.sched_priority as u64) {
            (SCHED_NORMAL, 0) => Ok(Self::Normal),
            (SCHED_BATCH, 0) => Ok(Self::Batch),
            (SCHED_IDLE, 0) => Ok(Self::Idle),
            (SCHED_FIFO, priority) => {
                Ok(Self::Fifo { priority: std::cmp::min(priority, rlimit) as u32 })
            }
            (SCHED_RR, priority) => {
                Ok(Self::RoundRobin { priority: std::cmp::min(priority, rlimit) as u32 })
            }
            _ => error!(EINVAL),
        }
    }

    pub fn raw_policy(&self) -> u32 {
        match self {
            Self::Normal => SCHED_NORMAL,
            Self::Batch => SCHED_BATCH,
            Self::Idle => SCHED_IDLE,
            Self::Fifo { .. } => SCHED_FIFO,
            Self::RoundRobin { .. } => SCHED_RR,
        }
    }

    pub fn raw_params(&self) -> sched_param {
        match self {
            Self::Normal | Self::Batch | Self::Idle => sched_param { sched_priority: 0 },
            Self::Fifo { priority } | Self::RoundRobin { priority } => {
                sched_param { sched_priority: *priority as i32 }
            }
        }
    }
}

pub fn min_priority_for_sched_policy(policy: u32) -> Result<i32, Errno> {
    match policy {
        SCHED_NORMAL | SCHED_BATCH | SCHED_IDLE => Ok(0),
        SCHED_FIFO | SCHED_RR => Ok(1),
        _ => error!(EINVAL),
    }
}

pub fn max_priority_for_sched_policy(policy: u32) -> Result<i32, Errno> {
    match policy {
        SCHED_NORMAL | SCHED_BATCH | SCHED_IDLE => Ok(0),
        SCHED_FIFO | SCHED_RR => Ok(99),
        _ => error!(EINVAL),
    }
}

pub struct TaskMutableState {
    // See https://man7.org/linux/man-pages/man2/set_tid_address.2.html
    pub clear_child_tid: UserRef<pid_t>,

    /// Signal handler related state. This is grouped together for when atomicity is needed during
    /// signal sending and delivery.
    pub signals: SignalState,

    /// The exit status that this task exited with.
    pub exit_status: Option<ExitStatus>,

    /// Whether the executor should dump the stack of this task when it exits. Currently used to
    /// implement ExitStatus::CoreDump.
    pub dump_on_exit: bool,

    /// The priority of the current task, a value between 1 and 40 (inclusive). Higher value means
    /// higher priority. Defaults to 20.
    ///
    /// In POSIX, priority is a per-process setting, but in Linux it is per-thread.
    /// See https://man7.org/linux/man-pages/man2/setpriority.2.html#BUGS
    pub priority: u8,

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

    /// The address and size of the mapped restricted state VMO.
    pub restricted_state_addr_and_size: Option<(usize, usize)>,

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

    /// List of currently installed seccomp_filters; most recently added is last.
    pub seccomp_filters: Vec<SeccompFilter>,

    // The total length of the provided seccomp filters, which cannot
    // exceed SECCOMP_MAX_INSNS_PER_PATH - 4 * the number of filters.  This is stored
    // instead of computed because we store seccomp filters in an
    // expanded form, and it is impossible to get the original length.
    pub seccomp_provided_instructions: u16,
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
}

pub enum ExceptionResult {
    /// The exception was handled and no further action is required.
    Handled,

    // The exception generated a signal that should be delivered.
    Signal(SignalInfo),

    // The exception was not understood or could not be handled.
    Unhandled,
}

pub enum TaskStateCode {
    // Task is being executed.
    Running,

    // Task is waiting for an event.
    Sleeping,

    // Task has exited.
    Zombie,
}

impl TaskStateCode {
    pub fn code_char(&self) -> char {
        match self {
            TaskStateCode::Running => 'R',
            TaskStateCode::Sleeping => 'S',
            TaskStateCode::Zombie => 'Z',
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            TaskStateCode::Running => "running",
            TaskStateCode::Sleeping => "sleeping",
            TaskStateCode::Zombie => "zombie",
        }
    }
}

pub struct Task {
    pub id: pid_t,

    /// The thread group to which this task belongs.
    pub thread_group: Arc<ThreadGroup>,

    /// A handle to the underlying Zircon thread object.
    pub thread: RwLock<Option<zx::Thread>>,

    /// The file descriptor table for this task.
    pub files: FdTable,

    /// The memory manager for this task.
    pub mm: Arc<MemoryManager>,

    /// The file system for this task.
    ///
    /// The only case when this is not set is for the initial task while the FsContext is built.
    fs: OnceCell<Arc<FsContext>>,

    /// The namespace for abstract AF_UNIX sockets for this task.
    pub abstract_socket_namespace: Arc<AbstractUnixSocketNamespace>,

    /// The namespace for AF_VSOCK for this task.
    pub abstract_vsock_namespace: Arc<AbstractVsockSocketNamespace>,

    /// The signal this task generates on exit.
    pub exit_signal: Option<Signal>,

    /// The address of the signal trampoline in the task, or nullptr if no trampoline has been mapped.
    ///
    /// The signal trampoline consists of a call to sys_rt_sigreturn, which allows the kernel to restore
    /// the task state after a signal handler has run.
    // TODO(fxbug.dev/121659): Remove this onec the vDSO contains a signal trampoline.
    pub signal_trampoline: Mutex<UserAddress>,

    /// The mutable state of the Task.
    mutable_state: RwLock<TaskMutableState>,

    /// The command of this task.
    ///
    /// This is guarded by its own lock because it is use to display the task description, and
    /// other locks may be taken at the time. No other lock may be held while this lock is taken.
    command: RwLock<CString>,

    /// The security credentials for this task.
    ///
    /// This is necessary because credentials need to be read from operations on Node, and other
    /// locks may be taken at the time. No other lock may be held while this lock is taken.
    creds: RwLock<Credentials>,

    /// For vfork and clone() with CLONE_VFORK, this is set when the task exits or calls execve().
    /// It allows the calling task to block until the fork has been completed. Only populated
    /// when created with the CLONE_VFORK flag.
    vfork_event: Option<zx::Event>,

    /// Whether the restricted mode executor should ignore exceptions associated with this Task's thread
    // TODO(https://fxbug.dev/124427): Remove this mechanism once exceptions are handled inline.
    pub ignore_exceptions: std::sync::atomic::AtomicBool,

    /// Variable that can tell you whether there are currently seccomp
    /// filters without holding a lock
    pub has_seccomp_filters: AtomicU8,

    /// Used to ensure that all logs related to this task carry the same metadata about the task.
    logging_span: logging::Span,
}

/// The decoded cross-platform parts we care about for page fault exception reports.
pub struct PageFaultExceptionReport {
    pub faulting_address: u64,
    pub not_present: bool, // Set when the page fault was due to a not-present page.
    pub is_write: bool,    // Set when the triggering memory operation was a write.
}

impl Task {
    /// Internal function for creating a Task object. Useful when you need to specify the value of
    /// every field. create_process and create_thread are more likely to be what you want.
    ///
    /// Any fields that should be initialized fresh for every task, even if the task was created
    /// with fork, are initialized to their defaults inside this function. All other fields are
    /// passed as parameters.
    #[allow(clippy::let_and_return)]
    fn new(
        id: pid_t,
        command: CString,
        thread_group: Arc<ThreadGroup>,
        thread: Option<zx::Thread>,
        files: FdTable,
        mm: Arc<MemoryManager>,
        // The only case where fs should be None if when building the initial task that is the
        // used to build the initial FsContext.
        fs: Option<Arc<FsContext>>,
        creds: Credentials,
        abstract_socket_namespace: Arc<AbstractUnixSocketNamespace>,
        abstract_vsock_namespace: Arc<AbstractVsockSocketNamespace>,
        exit_signal: Option<Signal>,
        vfork_event: Option<zx::Event>,
        priority: u8,
        uts_ns: UtsNamespaceHandle,
        no_new_privs: bool,
        has_seccomp_filters: AtomicU8,
        seccomp_filters: Vec<SeccompFilter>,
        seccomp_provided_instructions: u16,
    ) -> Self {
        let fs = {
            let result = OnceCell::new();
            if let Some(fs) = fs {
                result.get_or_init(|| fs);
            }
            result
        };
        let logging_span = logging::Span::new(thread_group.leader, id, &command);
        let task = Task {
            id,
            thread_group,
            thread: RwLock::new(thread),
            files,
            mm,
            fs,
            abstract_socket_namespace,
            abstract_vsock_namespace,
            exit_signal,
            command: RwLock::new(command),
            creds: RwLock::new(creds),
            vfork_event,
            signal_trampoline: Default::default(),
            mutable_state: RwLock::new(TaskMutableState {
                clear_child_tid: UserRef::default(),
                signals: Default::default(),
                exit_status: None,
                dump_on_exit: false,
                priority,
                scheduler_policy: Default::default(),
                uts_ns,
                restricted_state_addr_and_size: None,
                no_new_privs,
                seccomp_filters,
                seccomp_provided_instructions,
            }),
            ignore_exceptions: std::sync::atomic::AtomicBool::new(false),
            has_seccomp_filters,
            logging_span,
        };
        #[cfg(any(test, debug_assertions))]
        {
            let _l1 = task.read();
            let _l2 = task.command.read();
            let _l3 = task.creds.read();
        }
        task
    }

    /// Access mutable state with a read lock.
    pub fn read(&self) -> RwLockReadGuard<'_, TaskMutableState> {
        self.mutable_state.read()
    }

    /// Access mutable state with a write lock.
    pub fn write(&self) -> RwLockWriteGuard<'_, TaskMutableState> {
        self.mutable_state.write()
    }

    pub fn add_file(&self, file: FileHandle, flags: FdFlags) -> Result<FdNumber, Errno> {
        self.files.add_with_flags(self, file, flags)
    }

    pub fn creds(&self) -> Credentials {
        self.creds.read().clone()
    }

    pub fn set_creds(&self, creds: Credentials) {
        *self.creds.write() = creds;
    }

    pub fn fs(&self) -> &Arc<FsContext> {
        self.fs.get().unwrap()
    }

    pub fn set_fs(&self, fs: Arc<FsContext>) {
        self.fs.set(fs).map_err(|_| "Cannot set fs multiple times").unwrap();
    }

    pub fn create_init_child_process(
        kernel: &Arc<Kernel>,
        binary_path: &CString,
    ) -> Result<CurrentTask, Errno> {
        let init_task = kernel.pids.read().get_task(1).ok_or_else(|| errno!(EINVAL))?;
        let task = Self::create_process_without_parent(
            kernel,
            binary_path.clone(),
            Some(init_task.fs().fork()),
        )?;
        {
            let mut init_writer = init_task.thread_group.write();
            let mut new_process_writer = task.thread_group.write();
            new_process_writer.parent = Some(init_task.thread_group.clone());
            init_writer.children.insert(task.id, Arc::downgrade(&task.thread_group));
        }
        Ok(task)
    }

    /// Create a task that is the leader of a new thread group.
    ///
    /// This function creates an underlying Zircon process to host the new
    /// task.
    ///
    /// root_fs should only be None for the init task, and set_fs should be called as soon as the
    /// FsContext is build.
    pub fn create_process_without_parent(
        kernel: &Arc<Kernel>,
        initial_name: CString,
        root_fs: Option<Arc<FsContext>>,
    ) -> Result<CurrentTask, Errno> {
        let mut pids = kernel.pids.write();
        let pid = pids.allocate_pid();

        let process_group = ProcessGroup::new(pid, None);
        pids.add_process_group(&process_group);

        let TaskInfo { thread, thread_group, memory_manager } = create_zircon_process(
            kernel,
            None,
            pid,
            process_group.clone(),
            SignalActions::default(),
            initial_name.as_bytes(),
        )?;
        process_group.insert(&thread_group);

        let current_task = CurrentTask::new(Self::new(
            pid,
            initial_name,
            thread_group,
            thread,
            FdTable::default(),
            memory_manager,
            root_fs,
            Credentials::root(),
            Arc::clone(&kernel.default_abstract_socket_namespace),
            Arc::clone(&kernel.default_abstract_vsock_namespace),
            None,
            None,
            DEFAULT_TASK_PRIORITY,
            kernel.root_uts_ns.clone(),
            false,
            std::sync::atomic::AtomicU8::new(0),
            vec![],
            0,
        ));
        current_task.thread_group.add(&current_task.task)?;

        pids.add_task(&current_task.task);
        pids.add_thread_group(&current_task.thread_group);
        Ok(current_task)
    }

    /// Clone this task.
    ///
    /// Creates a new task object that shares some state with this task
    /// according to the given flags.
    ///
    /// Used by the clone() syscall to create both processes and threads.
    ///
    /// The exit signal is broken out from the flags parameter like clone3() rather than being
    /// bitwise-ORed like clone().
    pub fn clone_task(
        &self,
        flags: u64,
        child_exit_signal: Option<Signal>,
        user_parent_tid: UserRef<pid_t>,
        user_child_tid: UserRef<pid_t>,
    ) -> Result<CurrentTask, Errno> {
        // TODO: Implement more flags.
        const IMPLEMENTED_FLAGS: u64 = (CLONE_VM
            | CLONE_FS
            | CLONE_FILES
            | CLONE_SIGHAND
            | CLONE_THREAD
            | CLONE_SYSVSEM
            | CLONE_SETTLS
            | CLONE_PARENT_SETTID
            | CLONE_CHILD_CLEARTID
            | CLONE_CHILD_SETTID
            | CLONE_VFORK) as u64;

        // CLONE_SETTLS is implemented by sys_clone.

        let clone_thread = flags & (CLONE_THREAD as u64) != 0;
        let clone_vm = flags & (CLONE_VM as u64) != 0;
        let clone_sighand = flags & (CLONE_SIGHAND as u64) != 0;
        let clone_vfork = flags & (CLONE_VFORK as u64) != 0;

        let new_uts = flags & (CLONE_NEWUTS as u64) != 0;

        if clone_sighand && !clone_vm {
            return error!(EINVAL);
        }
        if clone_thread && !clone_sighand {
            return error!(EINVAL);
        }

        if clone_vm && !clone_thread {
            // TODO(fxbug.dev/114813) Implement CLONE_VM for child processes (not just child
            // threads). Currently this executes CLONE_VM (explicitly passed to clone() or as
            // used by vfork()) as a fork (the VM in the child is copy-on-write) which is almost
            // always OK.
            //
            // CLONE_VM is primarily as an optimization to avoid making a copy-on-write version of a
            // process' VM that will be immediately replaced with a call to exec(). The main users
            // (libc and language runtimes) don't actually rely on the memory being shared between
            // the two processes. And the vfork() man page explicitly allows vfork() to be
            // implemented as fork() which is what we do here.
            if !clone_vfork {
                log_warn!("CLONE_VM set without CLONE_THREAD. Ignoring CLONE_VM (doing a fork).");
            }
        } else if clone_thread && !clone_vm {
            not_implemented!("CLONE_THREAD without CLONE_VM is not implemented");
            return error!(ENOSYS);
        }

        if flags & !IMPLEMENTED_FLAGS != 0 {
            not_implemented!("clone does not implement flags: 0x{:x}", flags & !IMPLEMENTED_FLAGS);
            return error!(ENOSYS);
        }

        let fs = if flags & (CLONE_FS as u64) != 0 { self.fs().clone() } else { self.fs().fork() };
        let files =
            if flags & (CLONE_FILES as u64) != 0 { self.files.clone() } else { self.files.fork() };

        let kernel = &self.thread_group.kernel;
        let mut pids = kernel.pids.write();

        let pid;
        let command;
        let creds;
        let priority;
        let uts_ns;
        let no_new_privs;
        let seccomp_filters;
        let seccomp_provided_instructions;
        {
            let state = self.read();

            no_new_privs = state.no_new_privs;
            seccomp_filters = state.seccomp_filters.clone();
            seccomp_provided_instructions = state.seccomp_provided_instructions;
        }
        let TaskInfo { thread, thread_group, memory_manager } = {
            // Make sure to drop these locks ASAP to avoid inversion
            let thread_group_state = self.thread_group.write();
            let state = self.read();

            pid = pids.allocate_pid();
            command = self.command();
            creds = self.creds();
            priority = state.priority;

            uts_ns = if new_uts {
                if !self.creds().has_capability(CAP_SYS_ADMIN) {
                    return error!(EPERM);
                }

                // Fork the UTS namespace of the existing task.
                let new_uts_ns = state.uts_ns.read().clone();
                Arc::new(RwLock::new(new_uts_ns))
            } else {
                // Inherit the UTS of the existing task.
                state.uts_ns.clone()
            };

            if clone_thread {
                create_zircon_thread(self)?
            } else {
                // Drop the lock on this task before entering `create_zircon_process`, because it will
                // take a lock on the new thread group, and locks on thread groups have a higher
                // priority than locks on the task in the thread group.
                std::mem::drop(state);
                let signal_actions = if clone_sighand {
                    self.thread_group.signal_actions.clone()
                } else {
                    self.thread_group.signal_actions.fork()
                };
                let process_group = thread_group_state.process_group.clone();
                create_zircon_process(
                    kernel,
                    Some(thread_group_state),
                    pid,
                    process_group,
                    signal_actions,
                    command.as_bytes(),
                )?
            }
        };

        // Only create the vfork event when the caller requested CLONE_VFORK.
        let vfork_event =
            if flags & (CLONE_VFORK as u64) != 0 { Some(zx::Event::create()) } else { None };

        let child = CurrentTask::new(Self::new(
            pid,
            command,
            thread_group,
            thread,
            files,
            memory_manager,
            Some(fs),
            creds,
            self.abstract_socket_namespace.clone(),
            self.abstract_vsock_namespace.clone(),
            child_exit_signal,
            vfork_event,
            priority,
            uts_ns,
            no_new_privs,
            AtomicU8::new(self.has_seccomp_filters.load(Ordering::Acquire)),
            seccomp_filters,
            seccomp_provided_instructions,
        ));

        // Drop the pids lock as soon as possible after creating the child. Destroying the child
        // and removing it from the pids table itself requires the pids lock, so if an early exit
        // takes place we have a self deadlock.
        pids.add_task(&child.task);
        if !clone_thread {
            pids.add_thread_group(&child.thread_group);
        }
        std::mem::drop(pids);

        // Child lock must be taken before this lock. Drop the lock on the task, take a writable
        // lock on the child and take the current state back.

        #[cfg(any(test, debug_assertions))]
        {
            // Take the lock on the thread group and its child in the correct order to ensure any wrong ordering
            // will trigger the tracing-mutex at the right call site.
            if !clone_thread {
                let _l1 = self.thread_group.read();
                let _l2 = child.thread_group.read();
            }
        }

        if clone_thread {
            self.thread_group.add(&child.task)?;
        } else {
            child.thread_group.add(&child.task)?;
            let mut child_state = child.write();
            let state = self.read();
            child_state.signals.alt_stack = state.signals.alt_stack;
            child_state.signals.set_mask(state.signals.mask());
            self.mm.snapshot_to(&child.mm)?;
            copy_process_debug_addr(&self.thread_group.process, &child.thread_group.process)?;
        }

        if flags & (CLONE_PARENT_SETTID as u64) != 0 {
            self.mm.write_object(user_parent_tid, &child.id)?;
        }

        if flags & (CLONE_CHILD_CLEARTID as u64) != 0 {
            child.write().clear_child_tid = user_child_tid;
        }

        if flags & (CLONE_CHILD_SETTID as u64) != 0 {
            child.mm.write_object(user_child_tid, &child.id)?;
        }

        Ok(child)
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
    /// vfork() and clone(... CLONE_VFORK, ...). The task musy have created with CLONE_EXECVE.
    pub fn wait_for_execve(&self) -> Result<(), Errno> {
        self.vfork_event
            .as_ref()
            .unwrap()
            .wait_handle(zx::Signals::USER_0, zx::Time::INFINITE)
            .map_err(|status| from_status_like_fdio!(status))?;
        Ok(())
    }

    /// The flags indicates only the flags as in clone3(), and does not use the low 8 bits for the
    /// exit signal as in clone().
    #[cfg(test)]
    pub fn clone_task_for_test(&self, flags: u64, exit_signal: Option<Signal>) -> CurrentTask {
        let result = self
            .clone_task(flags, exit_signal, UserRef::default(), UserRef::default())
            .expect("failed to create task in test");

        // Take the lock on thread group and task in the correct order to ensure any wrong ordering
        // will trigger the tracing-mutex at the right call site.
        {
            let _l1 = result.thread_group.read();
            let _l2 = result.read();
        }

        result
    }

    /// If needed, clear the child tid for this task.
    ///
    /// Userspace can ask us to clear the child tid and issue a futex wake at
    /// the child tid address when we tear down a task. For example, bionic
    /// uses this mechanism to implement pthread_join. The thread that calls
    /// pthread_join sleeps using FUTEX_WAIT on the child tid address. We wake
    /// them up here to let them know the thread is done.
    fn clear_child_tid_if_needed(&self) -> Result<(), Errno> {
        let mut state = self.write();
        let user_tid = state.clear_child_tid;
        if !user_tid.is_null() {
            let zero: pid_t = 0;
            self.mm.write_object(user_tid, &zero)?;
            self.thread_group.kernel.shared_futexes.wake(
                self,
                user_tid.addr(),
                usize::MAX,
                FUTEX_BITSET_MATCH_ANY,
            )?;
            state.clear_child_tid = UserRef::default();
        }
        Ok(())
    }

    /// Called by the Drop trait on CurrentTask and from handle_exceptions() in the restricted executor.
    // TODO(https://fxbug.dev/117302): Move to CurrentTask and restrict visibility if possible.
    pub fn destroy_do_not_use_outside_of_drop_if_possible(self: &Arc<Self>) {
        let _ignored = self.clear_child_tid_if_needed();
        self.thread_group.remove(self);

        // If the task has a mapping for its restricted state VMO, unmap it.
        {
            let task_state = self.write();
            if let Some((addr, size)) = task_state.restricted_state_addr_and_size {
                unsafe {
                    fuchsia_runtime::vmar_root_self().unmap(addr, size).expect("unmap");
                }
            }
        }

        // Release the fd table.
        // TODO(fxb/122600) This will be unneeded once the live state of a task is deleted as soon
        // as the task dies, instead of relying on Drop.
        self.files.drop_local();
    }

    pub fn get_task(&self, pid: pid_t) -> Option<Arc<Task>> {
        self.thread_group.kernel.pids.read().get_task(pid)
    }

    pub fn get_pid(&self) -> pid_t {
        self.thread_group.leader
    }

    pub fn get_tid(&self) -> pid_t {
        self.id
    }

    pub fn as_ucred(&self) -> ucred {
        let creds = self.creds();
        ucred { pid: self.get_pid(), uid: creds.uid, gid: creds.gid }
    }

    pub fn as_fscred(&self) -> FsCred {
        self.creds().as_fscred()
    }

    pub fn can_signal(&self, target: &Task, unchecked_signal: &UncheckedSignal) -> bool {
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

        // TODO(lindkvist): This check should also verify that the sessions are the same.
        if Signal::try_from(unchecked_signal) == Ok(SIGCONT) {
            return true;
        }

        false
    }

    /// Interrupts the current task.
    ///
    /// This will interrupt any blocking syscalls if the task is blocked on one.
    /// The signal_state of the task must not be locked.
    ///
    /// TODO(qsr): This should also interrupt any running code.
    pub fn interrupt(&self) {
        self.read().signals.waiter.access(|waiter| {
            if let Some(waiter) = waiter {
                waiter.interrupt()
            }
        })
    }

    pub fn command(&self) -> CString {
        self.command.read().clone()
    }

    pub fn set_command_name(&self, name: CString) {
        // Truncate to 16 bytes, including null byte.
        let bytes = name.to_bytes();
        let name = if bytes.len() > 15 {
            // SAFETY: Substring of a CString will contain no null bytes.
            CString::new(&bytes[..15]).unwrap()
        } else {
            name
        };
        self.logging_span.update(self.thread_group.leader, self.id, &name);
        *self.command.write() = name;
    }

    /// Processes a Zircon exception associated with this task.
    ///
    /// If the exception is fully handled, returns Ok(None)
    /// If the exception produces a signal, returns Ok(Some(SigInfo)).
    /// If the exception could not be handled returns Err(())
    // TODO(https://fxbug.dev/117302): Move to CurrentTask when the restricted executor's flow allows the exception handler to
    // access CurrentTask. It does not make sense to handle a Zircon exception for another task.
    pub fn process_exception(
        &self,
        info: &zx::sys::zx_exception_info_t,
        exception: &zx::Exception,
        report: &zx::sys::zx_exception_report_t,
    ) -> ExceptionResult {
        if info.type_ == zx::sys::ZX_EXCP_FATAL_PAGE_FAULT {
            // A page fault may be resolved by extending a growsdown mapping to cover the faulting
            // address. Ask the memory manager if it can extend a mapping to cover the faulting
            // address and if says that it's found a mapping that exists or that can be extended to
            // cover this address mark the exception as handled so that the instruction can try
            // again. Otherwise let the regular handling proceed.

            // We should only attempt growth on a not-present fault and we should only extend if the
            // access type matches the protection on the GROWSDOWN mapping.
            let decoded = decode_page_fault_exception_report(report);
            if decoded.not_present {
                match self.mm.extend_growsdown_mapping_to_address(
                    UserAddress::from(decoded.faulting_address),
                    decoded.is_write,
                ) {
                    Ok(true) => {
                        exception
                            .set_exception_state(&zx::sys::ZX_EXCEPTION_STATE_HANDLED)
                            .unwrap();
                        return ExceptionResult::Handled;
                    }
                    Err(e) => {
                        log_warn!("Error handling page fault: {e}")
                    }
                    _ => {}
                }
            }
        }
        match info.type_ {
            zx::sys::ZX_EXCP_FATAL_PAGE_FAULT => {
                ExceptionResult::Signal(SignalInfo::default(SIGSEGV))
            }
            zx::sys::ZX_EXCP_UNDEFINED_INSTRUCTION => {
                ExceptionResult::Signal(SignalInfo::default(SIGILL))
            }
            zx::sys::ZX_EXCP_SW_BREAKPOINT => ExceptionResult::Signal(SignalInfo::default(SIGTRAP)),
            _ => ExceptionResult::Unhandled,
        }
    }

    pub fn set_seccomp_state(&self, state: SeccompFilterState) -> Result<(), Errno> {
        let ustate: u8 = state as u8;
        loop {
            let seccomp_filter_status = self.has_seccomp_filters.load(Ordering::Acquire);
            if seccomp_filter_status == ustate {
                return Ok(());
            }
            if seccomp_filter_status != SeccompFilterState::None as u8 {
                return Err(errno!(EINVAL));
            }
            if self
                .has_seccomp_filters
                .compare_exchange(
                    seccomp_filter_status,
                    ustate,
                    Ordering::Release,
                    Ordering::Acquire,
                )
                .is_ok()
            {
                return Ok(());
            }
        }
    }

    pub fn logging_span(&self) -> logging::Span {
        self.logging_span.clone()
    }

    pub fn state_code(&self) -> TaskStateCode {
        let status = self.read();
        if status.exit_status.is_some() {
            TaskStateCode::Zombie
        } else if status.signals.waiter.is_valid() {
            TaskStateCode::Sleeping
        } else {
            TaskStateCode::Running
        }
    }
}

impl CurrentTask {
    fn new(task: Task) -> CurrentTask {
        CurrentTask {
            task: Arc::new(task),
            dt_debug_address: None,
            registers: RegisterState::default(),
            syscall_restart_func: None,
        }
    }

    pub fn kernel(&self) -> &Arc<Kernel> {
        &self.thread_group.kernel
    }

    pub fn task_arc_clone(&self) -> Arc<Task> {
        Arc::clone(&self.task)
    }

    pub fn set_syscall_restart_func<R: Into<SyscallResult>>(
        &mut self,
        f: impl FnOnce(&mut CurrentTask) -> Result<R, Errno> + Send + Sync + 'static,
    ) {
        self.syscall_restart_func = Some(Box::new(|current_task| Ok(f(current_task)?.into())));
    }

    /// Sets the task's signal mask to `signal_mask` and runs `wait_function`.
    ///
    /// Signals are dequeued prior to the original signal mask being restored. This is done by the
    /// signal machinery in the syscall dispatch loop.
    ///
    /// The returned result is the result returned from the wait function.
    pub fn wait_with_temporary_mask<F, T>(
        &mut self,
        signal_mask: SigSet,
        wait_function: F,
    ) -> Result<T, Errno>
    where
        F: FnOnce(&CurrentTask) -> Result<T, Errno>,
    {
        self.write().signals.set_temporary_mask(signal_mask);
        wait_function(self)
    }

    /// Determine namespace node indicated by the dir_fd.
    ///
    /// Returns the namespace node and the path to use relative to that node.
    pub fn resolve_dir_fd<'a>(
        &self,
        dir_fd: FdNumber,
        mut path: &'a FsStr,
    ) -> Result<(NamespaceNode, &'a FsStr), Errno> {
        let dir = if !path.is_empty() && path[0] == b'/' {
            path = &path[1..];
            self.fs().root()
        } else if dir_fd == FdNumber::AT_FDCWD {
            self.fs().cwd()
        } else {
            let file = self.files.get(dir_fd)?;
            file.name.clone()
        };
        if !path.is_empty() {
            if !dir.entry.node.is_dir() {
                return error!(ENOTDIR);
            }
            dir.entry.node.check_access(self, Access::EXEC)?;
        }
        Ok((dir, path))
    }

    /// A convenient wrapper for opening files relative to FdNumber::AT_FDCWD.
    ///
    /// Returns a FileHandle but does not install the FileHandle in the FdTable
    /// for this task.
    pub fn open_file(&self, path: &FsStr, flags: OpenFlags) -> Result<FileHandle, Errno> {
        if flags.contains(OpenFlags::CREAT) {
            // In order to support OpenFlags::CREAT we would need to take a
            // FileMode argument.
            return error!(EINVAL);
        }
        self.open_file_at(FdNumber::AT_FDCWD, path, flags, FileMode::default())
    }

    /// Resolves a path for open.
    ///
    /// If the final path component points to a symlink, the symlink is followed (as long as
    /// the symlink traversal limit has not been reached).
    ///
    /// If the final path component (after following any symlinks, if enabled) does not exist,
    /// and `flags` contains `OpenFlags::CREAT`, a new node is created at the location of the
    /// final path component.
    ///
    /// This returns the resolved node, and a boolean indicating whether the node has been created.
    fn resolve_open_path(
        &self,
        context: &mut LookupContext,
        dir: NamespaceNode,
        path: &FsStr,
        mode: FileMode,
        flags: OpenFlags,
    ) -> Result<(NamespaceNode, bool), Errno> {
        context.update_for_path(path);
        let mut parent_content = context.with(SymlinkMode::Follow);
        let (parent, basename) = self.lookup_parent(&mut parent_content, dir, path)?;
        context.remaining_follows = parent_content.remaining_follows;

        let must_create = flags.contains(OpenFlags::CREAT) && flags.contains(OpenFlags::EXCL);

        // Lookup the child, without following a symlink or expecting it to be a directory.
        let mut child_context = context.with(SymlinkMode::NoFollow);
        child_context.must_be_directory = false;

        match parent.lookup_child(self, &mut child_context, basename) {
            Ok(name) => {
                if name.entry.node.is_lnk() {
                    if context.symlink_mode == SymlinkMode::NoFollow
                        || context.remaining_follows == 0
                    {
                        if must_create {
                            // Since `must_create` is set, and a node was found, this returns EEXIST
                            // instead of ELOOP.
                            return error!(EEXIST);
                        }
                        // A symlink was found, but too many symlink traversals have been
                        // attempted.
                        return error!(ELOOP);
                    }

                    context.remaining_follows -= 1;
                    match name.readlink(self)? {
                        SymlinkTarget::Path(path) => {
                            let dir = if path[0] == b'/' { self.fs().root() } else { parent };
                            self.resolve_open_path(context, dir, &path, mode, flags)
                        }
                        SymlinkTarget::Node(node) => Ok((node, false)),
                    }
                } else {
                    if must_create {
                        return error!(EEXIST);
                    }
                    Ok((name, false))
                }
            }
            Err(e) if e == errno!(ENOENT) && flags.contains(OpenFlags::CREAT) => {
                if context.must_be_directory {
                    return error!(EISDIR);
                }
                Ok((
                    parent.open_create_node(
                        self,
                        basename,
                        mode.with_type(FileMode::IFREG),
                        DeviceType::NONE,
                        flags,
                    )?,
                    true,
                ))
            }
            Err(e) => Err(e),
        }
    }

    /// The primary entry point for opening files relative to a task.
    ///
    /// Absolute paths are resolve relative to the root of the FsContext for
    /// this task. Relative paths are resolve relative to dir_fd. To resolve
    /// relative to the current working directory, pass FdNumber::AT_FDCWD for
    /// dir_fd.
    ///
    /// Returns a FileHandle but does not install the FileHandle in the FdTable
    /// for this task.
    pub fn open_file_at(
        &self,
        dir_fd: FdNumber,
        path: &FsStr,
        flags: OpenFlags,
        mode: FileMode,
    ) -> Result<FileHandle, Errno> {
        if path.is_empty() {
            return error!(ENOENT);
        }

        let (dir, path) = self.resolve_dir_fd(dir_fd, path)?;
        self.open_namespace_node_at(dir, path, flags, mode)
    }

    pub fn open_namespace_node_at(
        &self,
        dir: NamespaceNode,
        path: &FsStr,
        flags: OpenFlags,
        mode: FileMode,
    ) -> Result<FileHandle, Errno> {
        let mut flags = flags;
        if flags.contains(OpenFlags::PATH) {
            // When O_PATH is specified in flags, flag bits other than O_CLOEXEC,
            // O_DIRECTORY, and O_NOFOLLOW are ignored.
            const ALLOWED_FLAGS: OpenFlags = OpenFlags::from_bits_truncate(
                OpenFlags::PATH.bits()
                    | OpenFlags::CLOEXEC.bits()
                    | OpenFlags::DIRECTORY.bits()
                    | OpenFlags::NOFOLLOW.bits(),
            );
            flags &= ALLOWED_FLAGS;
        }

        let nofollow = flags.contains(OpenFlags::NOFOLLOW);
        let must_create = flags.contains(OpenFlags::CREAT) && flags.contains(OpenFlags::EXCL);

        let symlink_mode =
            if nofollow || must_create { SymlinkMode::NoFollow } else { SymlinkMode::Follow };

        let mut context = LookupContext::new(symlink_mode);
        context.must_be_directory = flags.contains(OpenFlags::DIRECTORY);
        let (name, created) = self.resolve_open_path(&mut context, dir, path, mode, flags)?;

        // Be sure not to reference the mode argument after this point.
        let mode = name.entry.node.info().mode;
        if nofollow && mode.is_lnk() {
            return error!(ELOOP);
        }

        if mode.is_dir() {
            if flags.can_write()
                || flags.contains(OpenFlags::CREAT)
                || flags.contains(OpenFlags::TRUNC)
            {
                return error!(EISDIR);
            }
            if flags.contains(OpenFlags::DIRECT) {
                return error!(EINVAL);
            }
        } else if context.must_be_directory {
            return error!(ENOTDIR);
        }

        if flags.contains(OpenFlags::TRUNC) && mode.is_reg() && !created {
            // You might think we should check file.can_write() at this
            // point, which is what the docs suggest, but apparently we
            // are supposed to truncate the file if this task can write
            // to the underlying node, even if we are opening the file
            // as read-only. See OpenTest.CanTruncateReadOnly.
            name.entry.node.truncate(self, 0)?;
        }

        // If the node has been created, the open operation should not verify access right:
        // From <https://man7.org/linux/man-pages/man2/open.2.html>
        //
        // > Note that mode applies only to future accesses of the newly created file; the
        // > open() call that creates a read-only file may well return a  read/write  file
        // > descriptor.

        name.open(self, flags, !created)
    }

    /// A wrapper for FsContext::lookup_parent_at that resolves the given
    /// dir_fd to a NamespaceNode.
    ///
    /// Absolute paths are resolve relative to the root of the FsContext for
    /// this task. Relative paths are resolve relative to dir_fd. To resolve
    /// relative to the current working directory, pass FdNumber::AT_FDCWD for
    /// dir_fd.
    pub fn lookup_parent_at<'a>(
        &self,
        context: &mut LookupContext,
        dir_fd: FdNumber,
        path: &'a FsStr,
    ) -> Result<(NamespaceNode, &'a FsStr), Errno> {
        let (dir, path) = self.resolve_dir_fd(dir_fd, path)?;
        self.lookup_parent(context, dir, path)
    }

    /// Lookup the parent of a namespace node.
    ///
    /// Consider using Task::open_file_at or Task::lookup_parent_at rather than
    /// calling this function directly.
    ///
    /// This function resolves all but the last component of the given path.
    /// The function returns the parent directory of the last component as well
    /// as the last component.
    ///
    /// If path is empty, this function returns dir and an empty path.
    /// Similarly, if path ends with "." or "..", these components will be
    /// returned along with the parent.
    ///
    /// The returned parent might not be a directory.
    pub fn lookup_parent<'a>(
        &self,
        context: &mut LookupContext,
        dir: NamespaceNode,
        path: &'a FsStr,
    ) -> Result<(NamespaceNode, &'a FsStr), Errno> {
        context.update_for_path(path);

        let mut current_node = dir;
        let mut it = path.split(|c| *c == b'/').filter(|p| !p.is_empty());
        let mut current_path_component = it.next().unwrap_or(b"");
        for next_path_component in it {
            current_node = current_node.lookup_child(self, context, current_path_component)?;
            current_path_component = next_path_component;
        }
        Ok((current_node, current_path_component))
    }

    /// Lookup a namespace node.
    ///
    /// Consider using Task::open_file_at or Task::lookup_parent_at rather than
    /// calling this function directly.
    ///
    /// This function resolves the component of the given path.
    pub fn lookup_path(
        &self,
        context: &mut LookupContext,
        dir: NamespaceNode,
        path: &FsStr,
    ) -> Result<NamespaceNode, Errno> {
        let (parent, basename) = self.lookup_parent(context, dir, path)?;

        // The child must resolve to a directory. This is because a trailing slash
        // was found in the path. If the child is a symlink, we should follow it.
        // See https://pubs.opengroup.org/onlinepubs/9699919799/xrat/V4_xbd_chap03.html#tag_21_03_00_75
        if context.must_be_directory {
            *context = context.with(SymlinkMode::Follow);
        }

        parent.lookup_child(self, context, basename)
    }

    /// Lookup a namespace node starting at the root directory.
    ///
    /// Resolves symlinks.
    pub fn lookup_path_from_root(&self, path: &FsStr) -> Result<NamespaceNode, Errno> {
        let mut context = LookupContext::default();
        self.lookup_path(&mut context, self.fs().root(), path)
    }

    pub fn exec(
        &mut self,
        executable: FileHandle,
        path: CString,
        argv: Vec<CString>,
        environ: Vec<CString>,
    ) -> Result<(), Errno> {
        // Executable must be a regular file
        if !executable.name.entry.node.is_reg() {
            return error!(EACCES);
        }

        // File node must have EXEC mode permissions.
        // Note that the ability to execute a file is unrelated to the flags
        // used in the `open` call.
        executable.node().check_access(self, Access::EXEC)?;

        let resolved_elf = resolve_executable(self, executable, path.clone(), argv, environ)?;
        if let Err(err) = self.finish_exec(path, resolved_elf) {
            // TODO(tbodt): Replace this panic with a log and force a SIGSEGV.
            panic!("{self:?} unrecoverable error in exec: {err:?}");
        }

        self.signal_vfork();

        Ok(())
    }

    /// After the memory is unmapped, any failure in exec is unrecoverable and results in the
    /// process crashing. This function is for that second half; any error returned from this
    /// function will be considered unrecoverable.
    fn finish_exec(&mut self, path: CString, resolved_elf: ResolvedElf) -> Result<(), Errno> {
        self.mm
            .exec(resolved_elf.file.name.clone())
            .map_err(|status| from_status_like_fdio!(status))?;
        let start_info = load_executable(self, resolved_elf, &path)?;
        self.dt_debug_address = start_info.dt_debug_address;
        let regs: zx_thread_state_general_regs_t = start_info.into();
        self.registers = regs.into();

        {
            let mut state = self.write();
            let mut creds = self.creds.write();
            state.signals.alt_stack = None;

            // TODO(tbodt): Check whether capability xattrs are set on the file, and grant/limit
            // capabilities accordingly.
            creds.exec();
        }

        self.thread_group.signal_actions.reset_for_exec();

        // TODO: The termination signal is reset to SIGCHLD.

        // TODO: All threads other than the calling thread are destroyed.

        // TODO: The file descriptor table is unshared, undoing the effect of
        //       the CLONE_FILES flag of clone(2).
        //
        // To make this work, we can put the files in an RwLock and then cache
        // a reference to the files on the CurrentTask. That will let
        // functions that have CurrentTask access the FdTable without
        // needing to grab the read-lock.
        //
        // For now, we do not implement that behavior.
        self.files.exec();

        // TODO: POSIX timers are not preserved.

        self.thread_group.write().did_exec = true;

        // Get the basename of the path, which will be used as the name displayed with
        // `prctl(PR_GET_NAME)` and `/proc/self/stat`
        let basename = if let Some(idx) = memchr::memrchr(b'/', path.to_bytes()) {
            // SAFETY: Substring of a CString will contain no null bytes.
            CString::new(&path.to_bytes()[idx + 1..]).unwrap()
        } else {
            path
        };
        set_zx_name(&fuchsia_runtime::thread_self(), basename.as_bytes());
        set_zx_name(&self.thread_group.process, basename.as_bytes());
        self.set_command_name(basename);
        Ok(())
    }

    fn seccomp_tsync_error(id: i32, flags: u32) -> Result<SyscallResult, Errno> {
        // By default, TSYNC indicates failure state by returning the first thread
        // id not to be able to sync, rather than by returning -1 and setting
        // errno.  However, if TSYNC_ESRCH is set, it returns ESRCH.  This
        // prevents conflicts with fact that SECCOMP_FILTER_FLAG_NEW_LISTENER
        // makes seccomp return an fd.
        if flags & SECCOMP_FILTER_FLAG_TSYNC_ESRCH != 0 {
            Err(errno!(ESRCH))
        } else {
            Ok(id.into())
        }
    }

    pub fn add_seccomp_filter(
        &mut self,
        bpf_filter: UserAddress,
        flags: u32,
    ) -> Result<SyscallResult, Errno> {
        let fprog: sock_fprog = self.mm.read_object(UserRef::new(bpf_filter))?;

        if u32::from(fprog.len) > BPF_MAXINSNS || fprog.len == 0 {
            return Err(errno!(EINVAL));
        }

        let mut code: Vec<sock_filter> = vec![Default::default(); fprog.len as usize];
        self.read_objects(UserRef::new(UserAddress::from_ptr(fprog.filter)), code.as_mut_slice())?;

        // If an instruction loads from / stores to an absolute address, that address has to be
        // 32-bit aligned and inside the struct seccomp_data passed in.
        for insn in &code {
            if (bpf_class(insn) == BPF_LD || bpf_class(insn) == BPF_ST)
                && (bpf_addressing_mode(insn) == BPF_ABS)
                && (insn.k & 0x3 != 0 || std::mem::size_of::<seccomp_data>() < insn.k as usize)
            {
                return Err(errno!(EINVAL));
            }
        }

        match EbpfProgram::from_cbpf(&code) {
            Ok(program) => {
                let state = self.thread_group.write();
                if flags & SECCOMP_FILTER_FLAG_TSYNC != 0 {
                    // TSYNC synchronizes all filters for all threads in the current process to
                    // the current thread's

                    // We collect the filters for the current task upfront to save us acquiring
                    // the task's lock a lot of times below.
                    let mut filters: Vec<SeccompFilter> = self.read().seccomp_filters.clone();

                    // For TSYNC to work, all of the other thread filters in this process have to
                    // be a prefix of this thread's filters, and none of them can be in
                    // strict mode.
                    let tasks = state.tasks().collect::<Vec<_>>();
                    for task in &tasks {
                        let other_task_state = task.mutable_state.read();
                        let maybe_new_length =
                            other_task_state.seccomp_provided_instructions + fprog.len + 4;

                        if maybe_new_length > SECCOMP_MAX_INSNS_PER_PATH {
                            return Self::seccomp_tsync_error(task.id, flags);
                        }

                        // All threads need to do the length check, but only other threads need
                        // the prefix check.
                        if task.id == self.id {
                            continue;
                        }

                        // Target threads cannot be in SECCOMP_MODE_STRICT
                        if task.has_seccomp_filters.load(Ordering::Acquire)
                            & SeccompFilterState::Strict as u8
                            != 0
                        {
                            return Self::seccomp_tsync_error(task.id, flags);
                        }

                        // Check that target thread's filters are a prefix of this thread's
                        // filters.
                        if other_task_state.seccomp_filters.len() > filters.len() {
                            return Self::seccomp_tsync_error(task.id, flags);
                        }
                        for (filter, other_filter) in
                            filters.iter().zip(other_task_state.seccomp_filters.iter())
                        {
                            if other_filter.unique_id != filter.unique_id {
                                return Self::seccomp_tsync_error(task.id, flags);
                            }
                        }
                    }
                    // Now that we're sure we're allowed to do so, add the filter to all threads.
                    let new_length = self.read().seccomp_provided_instructions + fprog.len + 4;
                    filters.push(SeccompFilter {
                        program,
                        unique_id: self
                            .thread_group
                            .next_seccomp_filter_id
                            .fetch_add(1, Ordering::SeqCst),
                    });

                    for task in &tasks {
                        let mut other_task_state = task.mutable_state.write();

                        other_task_state.enable_no_new_privs();
                        other_task_state.seccomp_provided_instructions = new_length;
                        other_task_state.seccomp_filters = filters.clone();
                        task.set_seccomp_state(SeccompFilterState::UserDefined)?;
                    }
                } else {
                    let mut task_state = self.mutable_state.write();
                    let maybe_new_length = task_state.seccomp_provided_instructions + fprog.len + 4;
                    if maybe_new_length > SECCOMP_MAX_INSNS_PER_PATH {
                        return Err(errno!(ENOMEM));
                    }

                    task_state.seccomp_provided_instructions = maybe_new_length;
                    task_state.seccomp_filters.push(SeccompFilter {
                        program,
                        unique_id: self
                            .thread_group
                            .next_seccomp_filter_id
                            .fetch_add(1, Ordering::SeqCst),
                    });
                    self.set_seccomp_state(SeccompFilterState::UserDefined)?;
                }

                self.set_seccomp_state(SeccompFilterState::UserDefined)?;
            }
            Err(errmsg) => log_warn!("{}", errmsg),
        }

        Ok(().into())
    }

    // NB: Allow warning below so that it is clear what we are doing on KILL_PROCESS
    #[allow(clippy::wildcard_in_or_patterns)]
    pub fn run_seccomp_filters(&self, syscall: &Syscall) -> Option<Errno> {
        // Implementation of SECCOMP_FILTER_STRICT, which has slightly different semantics
        // from user-defined seccomp filters.
        if self.has_seccomp_filters.load(Ordering::Acquire) & SeccompFilterState::Strict as u8 != 0
            && syscall.decl.number as u32 != __NR_exit
            && syscall.decl.number as u32 != __NR_read
            && syscall.decl.number as u32 != __NR_write
        {
            send_signal(self, SignalInfo::default(SIGKILL));
            return Some(errno_from_code!(0));
        }

        // After that is done, execute any other seccomp filters.
        #[cfg(target_arch = "x86_64")]
        let arch_val = AUDIT_ARCH_X86_64;
        #[cfg(target_arch = "aarch64")]
        let arch_val = AUDIT_ARCH_AARCH64;

        // VDSO calls can't be caught by seccomp, so most seccomp filters forget to declare them.
        // But our VDSO implementation is incomplete, and most of the calls forward to the actual
        // syscalls. So seccomp should ignore them until they're implemented correctly in the VDSO.
        #[cfg(target_arch = "x86_64")] // The set of VDSO calls is arch dependent.
        #[allow(non_upper_case_globals)]
        if let __NR_clock_gettime | __NR_getcpu | __NR_gettimeofday | __NR_time =
            syscall.decl.number as u32
        {
            return None;
        }
        #[cfg(target_arch = "aarch64")]
        #[allow(non_upper_case_globals)]
        if let __NR_clock_gettime | __NR_clock_getres | __NR_gettimeofday =
            syscall.decl.number as u32
        {
            return None;
        }

        let mut data = seccomp_data {
            nr: syscall.decl.number as i32,
            arch: arch_val,
            instruction_pointer: self.registers.instruction_pointer_register(),
            args: [
                syscall.arg0,
                syscall.arg1,
                syscall.arg2,
                syscall.arg3,
                syscall.arg4,
                syscall.arg5,
            ],
        };

        let mut result = SECCOMP_RET_ALLOW;
        {
            let filters = &self.mutable_state.read().seccomp_filters;

            // Filters are executed in reverse order of addition
            for filter in filters.iter().rev() {
                let mut new_result = SECCOMP_RET_ALLOW;
                if let Ok(r) = filter.program.run(&mut data) {
                    new_result = r as u32
                }
                if ((new_result & SECCOMP_RET_ACTION_FULL) as i32)
                    < ((result & SECCOMP_RET_ACTION_FULL) as i32)
                {
                    result = new_result;
                }
            }
        }

        match result & !SECCOMP_RET_DATA {
            SECCOMP_RET_ALLOW => None,
            SECCOMP_RET_ERRNO => {
                // Linux kernel compatibility: if errno exceeds 0xfff, it is capped at 0xfff.
                Some(errno_from_code!(std::cmp::min(result & 0xffff, 0xfff) as i16))
            }

            SECCOMP_RET_KILL_THREAD => {
                let siginfo = SignalInfo::default(SIGSYS);

                let is_last_thread = self.thread_group.read().tasks.len() == 1;
                let mut task_state = self.write();

                if is_last_thread {
                    task_state.dump_on_exit = true;
                    task_state.exit_status.get_or_insert(ExitStatus::CoreDump(siginfo));
                } else {
                    task_state.exit_status.get_or_insert(ExitStatus::Kill(siginfo));
                }
                Some(errno_from_code!(0))
            }
            SECCOMP_RET_LOG => {
                let creds = self.creds();
                let uid = creds.uid;
                let gid = creds.gid;
                let comm_r = self.command();
                let comm = if let Ok(c) = comm_r.to_str() { c } else { "???" };

                let arch = if cfg!(target_arch = "x86_64") {
                    "x86_64"
                } else if cfg!(target_arch = "aarch64") {
                    "aarch64"
                } else {
                    "unknown"
                };
                crate::logging::log_info!(
                    "uid={} gid={} pid={} comm={} syscall={} ip={} ARCH={} SYSCALL={}",
                    uid,
                    gid,
                    self.thread_group.leader,
                    comm,
                    syscall.decl.number,
                    self.registers.instruction_pointer_register(),
                    arch,
                    syscall.decl.name
                );
                None
            }
            SECCOMP_RET_TRACE => {
                // TODO(fxbug.dev/76810): Because there is no ptrace support, this returns ENOSYS
                Some(errno!(ENOSYS))
            }
            SECCOMP_RET_TRAP => {
                let siginfo = SignalInfo {
                    signal: SIGSYS,
                    errno: (result & SECCOMP_RET_DATA) as i32,
                    code: SYS_SECCOMP as i32,
                    detail: SignalDetail::SigSys {
                        call_addr: self.registers.instruction_pointer_register() as usize,
                        syscall: syscall.decl.number as i32,
                        arch: arch_val,
                    },
                    force: true,
                };

                send_signal(self, siginfo);
                Some(errno_from_code!(-(syscall.decl.number as i16)))
            }
            SECCOMP_RET_KILL_PROCESS | _ => {
                // from seccomp(2): If an action value other than one of the above is specified,
                // then the filter action is treated as [...] SECCOMP_RET_KILL_PROCESS.
                self.thread_group.exit(ExitStatus::CoreDump(SignalInfo::default(SIGSYS)));
                Some(errno_from_code!(0))
            }
        }
    }
}

/// Possible values for the current status of the seccomp filters for
/// this process.
#[repr(u8)]
pub enum SeccompFilterState {
    None = 0,
    UserDefined = 1,
    Strict = 2,
}

#[derive(Clone)]
pub struct SeccompFilter {
    /// The BPF program associated with this filter.
    program: EbpfProgram,

    /// The unique-to-this-process id of this filter.  SECCOMP_FILTER_FLAG_TSYNC only works if all
    /// threads in this process have filters that are a prefix of the filters of the thread
    /// attempting to do the TSYNC. Identical filters attached in separate seccomp calls are treated
    /// as different from each other for this purpose, so we need a way of distinguishing them.
    unique_id: u64,
}

impl fmt::Debug for Task {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}:{}[{}]",
            self.thread_group.leader,
            self.id,
            self.command.read().to_string_lossy()
        )
    }
}

impl fmt::Debug for CurrentTask {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.task.fmt(f)
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

    #[::fuchsia::test]
    fn test_tid_allocation() {
        let (kernel, current_task) = create_kernel_and_task();

        assert_eq!(current_task.get_tid(), 1);
        let another_current = create_task(&kernel, "another-task");
        assert_eq!(another_current.get_tid(), 2);

        let pids = kernel.pids.read();
        assert_eq!(pids.get_task(1).unwrap().get_tid(), 1);
        assert_eq!(pids.get_task(2).unwrap().get_tid(), 2);
        assert!(pids.get_task(3).is_none());
    }

    #[::fuchsia::test]
    fn test_clone_pid_and_parent_pid() {
        let (_kernel, current_task) = create_kernel_and_task();
        let thread = current_task
            .clone_task_for_test((CLONE_THREAD | CLONE_VM | CLONE_SIGHAND) as u64, Some(SIGCHLD));
        assert_eq!(current_task.get_pid(), thread.get_pid());
        assert_ne!(current_task.get_tid(), thread.get_tid());
        assert_eq!(current_task.thread_group.leader, thread.thread_group.leader);

        let child_task = current_task.clone_task_for_test(0, Some(SIGCHLD));
        assert_ne!(current_task.get_pid(), child_task.get_pid());
        assert_ne!(current_task.get_tid(), child_task.get_tid());
        assert_eq!(current_task.get_pid(), child_task.thread_group.read().get_ppid());
    }

    #[::fuchsia::test]
    fn test_root_capabilities() {
        let (_kernel, current_task) = create_kernel_and_task();
        assert!(current_task.creds().has_capability(CAP_SYS_ADMIN));
        current_task.set_creds(Credentials::with_ids(1, 1));
        assert!(!current_task.creds().has_capability(CAP_SYS_ADMIN));
    }
}
