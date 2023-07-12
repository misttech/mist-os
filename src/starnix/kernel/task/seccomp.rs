// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::Arc;
use ubpf::{
    converter::{bpf_addressing_mode, bpf_class},
    program::EbpfProgram,
};

use crate::{
    fs::buffers::{InputBuffer, OutputBuffer},
    fs::{fileops_impl_nonseekable, Anon, FdEvents, FdFlags, FdNumber, FileObject, FileOps},
    lock::Mutex,
    logging::log_warn,
    mm::MemoryAccessorExt,
    signals::{send_signal, SignalDetail, SignalInfo},
    syscalls::{decls::Syscall, SyscallArg, SyscallResult},
    task::{CurrentTask, EventHandler, ExitStatus, Task, WaitCanceler, WaitQueue, Waiter},
    types::*,
};

pub struct SeccompFilter {
    /// The BPF program associated with this filter.
    program: EbpfProgram,

    /// The unique-to-this-process id of this filter.  SECCOMP_FILTER_FLAG_TSYNC only works if all
    /// threads in this process have filters that are a prefix of the filters of the thread
    /// attempting to do the TSYNC. Identical filters attached in separate seccomp calls are treated
    /// as different from each other for this purpose, so we need a way of distinguishing them.
    unique_id: u64,

    /// The next cookie (unique id for this syscall), as used by SECCOMP_RET_USER_NOTIF
    cookie: AtomicU64,
}

/// The result of running a set of seccomp filters.
pub struct SeccompFilterResult {
    /// The action indicated by the seccomp filter with the highest priority result.
    action: u32,

    /// The filter that returned the highest priority result, as used by SECCOMP_RET_USER_NOTIF,
    /// which has to have access to its cookie value
    filter: Option<Arc<SeccompFilter>>,
}

impl SeccompFilter {
    /// Creates a SeccompFilter object from the given sock_filter.  Associates the user-provided
    /// id with it, which is intended to be unique to this process.
    pub fn from_cbpf(code: &Vec<sock_filter>, maybe_unique_id: u64) -> Result<Self, Errno> {
        // If an instruction loads from / stores to an absolute address, that address has to be
        // 32-bit aligned and inside the struct seccomp_data passed in.
        for insn in code {
            if (bpf_class(insn) == BPF_LD || bpf_class(insn) == BPF_ST)
                && (bpf_addressing_mode(insn) == BPF_ABS)
                && (insn.k & 0x3 != 0 || std::mem::size_of::<seccomp_data>() < insn.k as usize)
            {
                return error!(EINVAL);
            }
        }

        match EbpfProgram::from_cbpf(code) {
            Ok(program) => {
                Ok(SeccompFilter { program, unique_id: maybe_unique_id, cookie: AtomicU64::new(0) })
            }
            Err(errmsg) => {
                log_warn!("{}", errmsg);
                error!(EINVAL)
            }
        }
    }

    pub fn run(&self, data: &mut seccomp_data) -> u32 {
        if let Ok(r) = self.program.run(data) {
            return r as u32;
        }
        SECCOMP_RET_ALLOW
    }
}

const SECCOMP_MAX_INSNS_PER_PATH: u16 = 32768;

/// A list of seccomp filters, intended to be associated with a specific process.
#[derive(Default)]
pub struct SeccompFilterContainer {
    /// List of currently installed seccomp_filters; most recently added is last.
    pub filters: Vec<Arc<SeccompFilter>>,

    // The total length of the provided seccomp filters, which cannot
    // exceed SECCOMP_MAX_INSNS_PER_PATH - 4 * the number of filters.  This is stored
    // instead of computed because we store seccomp filters in an
    // expanded form, and it is impossible to get the original length.
    pub provided_instructions: u16,

    // Data needed by SECCOMP_RET_USER_NOTIF
    pub notifier: Option<SeccompNotifierHandle>,
}

impl Clone for SeccompFilterContainer {
    fn clone(&self) -> Self {
        if let Some(n) = &self.notifier {
            n.lock().add_thread();
        }
        SeccompFilterContainer {
            filters: self.filters.clone(),
            provided_instructions: self.provided_instructions,
            notifier: self.notifier.clone(),
        }
    }
}

impl Drop for SeccompFilterContainer {
    fn drop(&mut self) {
        if let Some(n) = &self.notifier {
            // Notifier needs to send threads a HUP when there is no one left
            // referencing it.
            n.lock().remove_thread();
        }
    }
}

fn make_seccomp_data(syscall: &Syscall, ip: u64) -> seccomp_data {
    #[cfg(target_arch = "x86_64")]
    let arch_val = AUDIT_ARCH_X86_64;
    #[cfg(target_arch = "aarch64")]
    let arch_val = AUDIT_ARCH_AARCH64;
    #[cfg(target_arch = "riscv64")]
    let arch_val = AUDIT_ARCH_RISCV64;
    seccomp_data {
        nr: syscall.decl.number as i32,
        arch: arch_val,
        instruction_pointer: ip,
        args: [
            syscall.arg0.raw(),
            syscall.arg1.raw(),
            syscall.arg2.raw(),
            syscall.arg3.raw(),
            syscall.arg4.raw(),
            syscall.arg5.raw(),
        ],
    }
}

impl SeccompFilterContainer {
    /// Ensures that this set of seccomp filters can be "synced to" the given set.
    /// This means that our filters are a prefix of the given set of filters.
    pub fn can_sync_to(&self, source: &SeccompFilterContainer) -> bool {
        if source.filters.len() < self.filters.len() {
            return false;
        }
        for (filter, other_filter) in self.filters.iter().zip(source.filters.iter()) {
            if other_filter.unique_id != filter.unique_id {
                return false;
            }
        }
        true
    }

    /// Adds the given filter to this list.  The original_length parameter is the length of
    /// the originally provided BPF (i.e., the number of sock_filter instructions), used
    /// to ensure the total length does not exceed SECCOMP_MAX_INSNS_PER_PATH
    pub fn add_filter(
        &mut self,
        filter: Arc<SeccompFilter>,
        original_length: u16,
    ) -> Result<(), Errno> {
        let maybe_new_length = self.provided_instructions + original_length + 4;
        if maybe_new_length > SECCOMP_MAX_INSNS_PER_PATH {
            return Err(errno!(ENOMEM));
        }

        self.provided_instructions = maybe_new_length;
        self.filters.push(filter);
        Ok(())
    }

    /// Runs all of the seccomp filters in this container, most-to-least recent.  Returns the
    /// highest priority result (which contains a reference to the filter that generated it)
    pub fn run_all(&self, current_task: &CurrentTask, syscall: &Syscall) -> SeccompFilterResult {
        let mut r = SeccompFilterResult { action: SECCOMP_RET_ALLOW, filter: None };

        // VDSO calls can't be caught by seccomp, so most seccomp filters forget to declare them.
        // But our VDSO implementation is incomplete, and most of the calls forward to the actual
        // syscalls. So seccomp should ignore them until they're implemented correctly in the VDSO.
        #[cfg(target_arch = "x86_64")] // The set of VDSO calls is arch dependent.
        #[allow(non_upper_case_globals)]
        if let __NR_clock_gettime | __NR_getcpu | __NR_gettimeofday | __NR_time =
            syscall.decl.number as u32
        {
            return r;
        }
        #[cfg(target_arch = "aarch64")]
        #[allow(non_upper_case_globals)]
        if let __NR_clock_gettime | __NR_clock_getres | __NR_gettimeofday =
            syscall.decl.number as u32
        {
            return r;
        }

        // Filters are executed in reverse order of addition
        for filter in self.filters.iter().rev() {
            let mut data =
                make_seccomp_data(syscall, current_task.registers.instruction_pointer_register());

            let new_result = filter.run(&mut data);
            if ((new_result & SECCOMP_RET_ACTION_FULL) as i32)
                < ((r.action & SECCOMP_RET_ACTION_FULL) as i32)
            {
                r = SeccompFilterResult { action: new_result, filter: Some(filter.clone()) };
            }
        }
        r
    }

    /// Creates a new listener for use by SECCOMP_RET_USER_NOTIF.  Returns its fd.
    pub fn create_listener(&mut self, current_task: &CurrentTask) -> Result<FdNumber, Errno> {
        if self.notifier.is_some() {
            return Err(errno!(EBUSY));
        }

        let the_notifier = SeccompNotifier::new();

        let handle = Anon::new_file(
            current_task,
            Box::new(SeccompNotifierFileObject { notifier: the_notifier.clone() }),
            OpenFlags::RDWR,
        );
        let fd = current_task.add_file(handle, FdFlags::CLOEXEC)?;

        {
            let mut state = the_notifier.lock();
            state.add_thread();
        }
        self.notifier = Some(the_notifier);
        Ok(fd)
    }
}

/// Possible values for the current status of the seccomp filters for
/// this process.
#[repr(u8)]
#[derive(Clone, Copy, PartialEq)]
pub enum SeccompStateValue {
    None = 0,
    UserDefined = 1,
    Strict = 2,
}

/// Per-process state that cannot be stored in the container (e.g., whether there is a container).
#[derive(Default)]
pub struct SeccompState {
    // This AtomicU8 corresponds to a SeccompStateValue.
    filter_state: AtomicU8,
}

impl SeccompState {
    pub fn from(state: &SeccompState) -> SeccompState {
        SeccompState { filter_state: AtomicU8::new(state.filter_state.load(Ordering::Acquire)) }
    }

    fn from_u8(value: u8) -> SeccompStateValue {
        match value {
            0 => SeccompStateValue::None,
            1 => SeccompStateValue::UserDefined,
            2 => SeccompStateValue::Strict,
            _ => unreachable!(),
        }
    }

    pub fn get(&self) -> SeccompStateValue {
        Self::from_u8(self.filter_state.load(Ordering::Acquire))
    }

    pub fn set(&self, state: &SeccompStateValue) -> Result<(), Errno> {
        loop {
            let seccomp_filter_status = self.get();
            if seccomp_filter_status == *state {
                return Ok(());
            }
            if seccomp_filter_status != SeccompStateValue::None {
                return Err(errno!(EINVAL));
            }

            if self
                .filter_state
                .compare_exchange(
                    seccomp_filter_status as u8,
                    *state as u8,
                    Ordering::Release,
                    Ordering::Acquire,
                )
                .is_ok()
            {
                return Ok(());
            }
        }
    }

    /// Check to see if this syscall is allowed in STRICT mode, and, if not,
    /// send the current task a SIGKILL.
    pub fn do_strict(task: &Task, syscall: &Syscall) -> Option<Result<SyscallResult, Errno>> {
        if syscall.decl.number as u32 != __NR_exit
            && syscall.decl.number as u32 != __NR_read
            && syscall.decl.number as u32 != __NR_write
        {
            send_signal(task, SignalInfo::default(SIGKILL));
            return Some(Err(errno_from_code!(0)));
        }
        None
    }

    /// Take the given |action| on the given |task|.  The action is one of the SECCOMP_RET values
    /// (ALLOW, LOG, KILL, KILL_PROCESS, TRAP, ERRNO, USER_NOTIF, TRACE).  |task| is the thread that
    /// invoked the syscall, and |syscall| is the syscall that was invoked.
    /// Returns the result that the syscall will be forced to return by this
    /// filter, or None, if the syscall should return its actual return value.
    // NB: Allow warning below so that it is clear what we are doing on KILL_PROCESS
    #[allow(clippy::wildcard_in_or_patterns)]
    pub fn do_user_defined(
        result: SeccompFilterResult,
        current_task: &mut CurrentTask,
        syscall: &Syscall,
    ) -> Option<Result<SyscallResult, Errno>> {
        let action = result.action;
        match action & !SECCOMP_RET_DATA {
            SECCOMP_RET_ALLOW => None,
            SECCOMP_RET_ERRNO => {
                // Linux kernel compatibility: if errno exceeds 0xfff, it is capped at 0xfff.
                Some(Err(errno_from_code!(std::cmp::min(action & 0xffff, 0xfff) as i16)))
            }
            SECCOMP_RET_KILL_THREAD => {
                let siginfo = SignalInfo::default(SIGSYS);

                let is_last_thread = current_task.thread_group.read().tasks_count() == 1;
                let mut task_state = current_task.write();

                if is_last_thread {
                    task_state.dump_on_exit = true;
                    task_state.exit_status.get_or_insert(ExitStatus::CoreDump(siginfo));
                } else {
                    task_state.exit_status.get_or_insert(ExitStatus::Kill(siginfo));
                }
                Some(Err(errno_from_code!(0)))
            }
            SECCOMP_RET_LOG => {
                let creds = current_task.creds();
                let uid = creds.uid;
                let gid = creds.gid;
                let comm_r = current_task.command();
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
                    current_task.thread_group.leader,
                    comm,
                    syscall.decl.number,
                    current_task.registers.instruction_pointer_register(),
                    arch,
                    syscall.decl.name
                );
                None
            }
            SECCOMP_RET_TRACE => {
                // TODO(fxbug.dev/76810): Because there is no ptrace support, this returns ENOSYS
                Some(Err(errno!(ENOSYS)))
            }
            SECCOMP_RET_TRAP => {
                #[cfg(target_arch = "x86_64")]
                let arch_val = AUDIT_ARCH_X86_64;
                #[cfg(target_arch = "aarch64")]
                let arch_val = AUDIT_ARCH_AARCH64;
                #[cfg(target_arch = "riscv64")]
                let arch_val = AUDIT_ARCH_RISCV64;

                let siginfo = SignalInfo {
                    signal: SIGSYS,
                    errno: (action & SECCOMP_RET_DATA) as i32,
                    code: SYS_SECCOMP as i32,
                    detail: SignalDetail::SigSys {
                        call_addr: current_task.registers.instruction_pointer_register().into(),
                        syscall: syscall.decl.number as i32,
                        arch: arch_val,
                    },
                    force: true,
                };

                send_signal(current_task, siginfo);
                Some(Err(errno_from_code!(-(syscall.decl.number as i16))))
            }
            SECCOMP_RET_USER_NOTIF => {
                if let Some(notifier) = current_task.get_seccomp_notifier() {
                    let cookie = result.filter.unwrap().cookie.fetch_add(1, Ordering::Relaxed);
                    let msg = seccomp_notif {
                        id: cookie,
                        pid: current_task.id as u32,
                        flags: 0,
                        data: make_seccomp_data(
                            syscall,
                            current_task.registers.instruction_pointer_register(),
                        ),
                    };
                    // First, add a pending notification, and wake up the supervisor waiting for it.
                    let waiter = Waiter::new();
                    {
                        let mut notifier = notifier.lock();
                        if notifier.is_closed {
                            // Someone explicitly close()d the fd with the notifier, which does not
                            // clear the thread-local notifier.  Do it now.
                            drop(notifier);
                            current_task.set_seccomp_notifier(None);
                            return Some(Err(errno!(ENOSYS)));
                        }
                        notifier.create_notification(cookie, msg);
                        notifier.waiters.wait_async_value(&waiter, cookie);
                    }

                    // Next, wait for a response from the supervisor
                    if let Err(e) = waiter.wait(current_task) {
                        return Some(Err(e));
                    }

                    // Fetch the response.
                    let resp: Option<seccomp_notif_resp>;
                    {
                        let mut notifier = notifier.lock();
                        resp = notifier.get_response(cookie);
                        notifier.delete_notification(cookie);
                    }

                    // The response indicates what you are supposed to do with this syscall.
                    if let Some(response) = resp {
                        if response.val != 0 {
                            return Some(Ok(response.val.into()));
                        }
                        if response.error != 0 {
                            if response.error > 0 {
                                return Some(Ok(response.error.into()));
                            } else {
                                return Some(Err(errno_from_code!(-response.error as i16)));
                            }
                        }
                        if response.flags & SECCOMP_USER_NOTIF_FLAG_CONTINUE != 0 {
                            return None;
                        }
                    }
                    Some(Ok(0.into()))
                } else {
                    Some(Err(errno!(ENOSYS)))
                }
            }
            SECCOMP_RET_KILL_PROCESS | _ => {
                current_task.thread_group.exit(ExitStatus::CoreDump(SignalInfo::default(SIGSYS)));
                Some(Err(errno_from_code!(0)))
            }
        }
    }

    pub fn is_action_available(action: u32) -> Result<SyscallResult, Errno> {
        match action & !SECCOMP_RET_DATA {
            SECCOMP_RET_ALLOW
            | SECCOMP_RET_ERRNO
            | SECCOMP_RET_KILL_PROCESS
            | SECCOMP_RET_KILL_THREAD
            | SECCOMP_RET_LOG
            | SECCOMP_RET_TRAP
            | SECCOMP_RET_USER_NOTIF => Ok(().into()),
            // TODO(fxbug.dev/126644): Implement this.
            SECCOMP_RET_TRACE => {
                error!(EOPNOTSUPP)
            }
            _ => {
                error!(EOPNOTSUPP)
            }
        }
    }
}

/// This struct contains data that needs to be shuttled back and forth between the thread doing
/// a USER_NOTIF and the supervisor thread responding to it.
#[derive(Default)]
struct SeccompNotification {
    /// notif is the notification set by the filter.  When this is set, the associated fd will
    /// be set to POLLIN.
    notif: seccomp_notif,

    /// Consumed indicates whether a supervisor process has read this notification (and so it
    /// can no longer be consumed by any other SECCOMP_IOCTL_NOTIF_RECV ioctl).  When the notif
    /// is consumed, the associated fd will be set to POLLOUT, indicating that it is ready to
    /// receive a response.
    consumed: bool,

    /// resp is the response that the supervisor sends.  When this is set, an event will be sent
    /// to SeccompNotifiers::waiters corresponding to the unique id of the notification.  This
    /// will wake up the filter that is waiting for this particular response.
    resp: Option<seccomp_notif_resp>,
}

impl SeccompNotification {
    fn new(data: seccomp_notif) -> SeccompNotification {
        SeccompNotification { notif: data, resp: None, consumed: false }
    }
}

/// The underlying implementation of the file descriptor that connects a process that triggers a
/// SECCOMP_RET_USER_NOTIF with the monitoring process. This support seccomp's ability to notify a
/// user-space process on specific syscall triggers. See seccomp_unotify(2) for the semantics.
pub struct SeccompNotifier {
    waiters: WaitQueue,

    pending_notifications: HashMap<u64, SeccompNotification>,

    // This keeps track of the number of threads using this notifier as a filter.  If that hits
    // zero, the listeners need to receive a HUP.
    num_active_threads: u64,

    // notifiers are referenced both by fds and in SeccompFilterContainer. If the file no longer
    // has fds referring to it, it will be closed, and the SeccompFilterContainers should stop
    // using it.
    pub is_closed: bool,
}

pub type SeccompNotifierHandle = Arc<Mutex<SeccompNotifier>>;

impl SeccompNotifier {
    pub fn new() -> SeccompNotifierHandle {
        Arc::new(Mutex::new(SeccompNotifier {
            waiters: WaitQueue::default(),
            pending_notifications: HashMap::default(),
            num_active_threads: 0,
            is_closed: false,
        }))
    }

    fn add_thread(&mut self) {
        self.num_active_threads += 1;
    }

    fn remove_thread(&mut self) {
        self.num_active_threads -= 1;
        if self.num_active_threads == 0 {
            self.waiters.notify_fd_events(FdEvents::POLLHUP);
        }
    }

    // Creates a pending notification for communication between the
    // target thread and a supervisor, and notifies readers there is
    // an opportunity to read.
    fn create_notification(&mut self, cookie: u64, notif: seccomp_notif) {
        self.pending_notifications.insert(cookie, SeccompNotification::new(notif));
        self.waiters.notify_fd_events(FdEvents::POLLIN | FdEvents::POLLRDNORM);
    }

    // Gets a notification that needs to be handled by a supervisor,
    // and notifies waiters that there is an opportunity to write.
    fn consume_some_notification(&mut self) -> Option<seccomp_notif> {
        for (_, notif) in self.pending_notifications.iter_mut() {
            if !notif.consumed {
                notif.consumed = true;
                self.waiters.notify_fd_events(FdEvents::POLLOUT | FdEvents::POLLWRNORM);
                return Some(notif.notif);
            }
        }
        None
    }

    // In case something goes wrong after we consume the notification.
    fn unconsume(&mut self, cookie: u64) {
        if let Some(n) = self.pending_notifications.get_mut(&cookie).as_mut() {
            n.consumed = false;
        }
    }

    // Returns the appropriate notifications if someone is waiting with poll/epoll/select.
    fn get_fd_notifications(&self) -> FdEvents {
        let mut events = FdEvents::empty();

        for (_, notification) in self.pending_notifications.iter() {
            if !notification.consumed {
                events |= FdEvents::POLLIN | FdEvents::POLLRDNORM;
            } else if notification.resp.is_none() {
                events |= FdEvents::POLLOUT | FdEvents::POLLWRNORM;
            }
        }

        if self.num_active_threads == 0 {
            events |= FdEvents::POLLHUP;
        }
        events
    }

    // Sets the value read by the target in response to this notification.  Intended for use by the
    // supervisor.  Notifies the filter there is a response to this request.
    fn set_response(&mut self, cookie: u64, resp: seccomp_notif_resp) -> Option<Errno> {
        if let Some(entry) = self.pending_notifications.get_mut(&cookie) {
            if entry.resp.is_some() {
                return Some(errno!(EINPROGRESS));
            }
            entry.resp = Some(resp);
            self.waiters.notify_value_event(resp.id);
            None
        } else {
            Some(errno!(EINVAL))
        }
    }

    // Gets the value set by the supervisor for the target to read.
    fn get_response(&self, cookie: u64) -> Option<seccomp_notif_resp> {
        if let Some(value) = self.pending_notifications.get(&cookie) {
            return value.resp;
        }
        None
    }

    // Returns whether the cookie represents an active notification.
    fn notification_pending(&self, cookie: u64) -> bool {
        self.pending_notifications.contains_key(&cookie)
    }

    // Deletes the notification, when the target is done processing it.
    fn delete_notification(&mut self, cookie: u64) {
        let _ = self.pending_notifications.remove(&cookie);
    }
}

struct SeccompNotifierFileObject {
    notifier: SeccompNotifierHandle,
}

impl FileOps for SeccompNotifierFileObject {
    fileops_impl_nonseekable!();

    fn close(&self, _file: &FileObject) {
        let mut state = self.notifier.lock();

        for (cookie, notification) in state.pending_notifications.iter() {
            if !notification.consumed {
                state.waiters.notify_value_event(*cookie);
                state.waiters.notify_fd_events(FdEvents::POLLIN | FdEvents::POLLRDNORM);
            } else if notification.resp.is_none() {
                state.waiters.notify_fd_events(FdEvents::POLLOUT | FdEvents::POLLWRNORM);
            }
        }
        state.waiters.notify_fd_events(FdEvents::POLLHUP);

        state.pending_notifications.clear();

        state.is_closed = true;
    }

    fn read(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _offset: usize,
        _usize: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        Err(errno!(EINVAL))
    }

    fn write(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _offset: usize,
        _buffer: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        Err(errno!(EINVAL))
    }

    fn ioctl(
        &self,
        _file: &FileObject,
        current_task: &CurrentTask,
        request: u32,
        arg: SyscallArg,
    ) -> Result<SyscallResult, Errno> {
        let user_addr = UserAddress::from(arg);
        match request {
            SECCOMP_IOCTL_NOTIF_RECV => {
                if let Ok(notif) = current_task
                    .mm
                    .read_memory_to_vec(user_addr, std::mem::size_of::<seccomp_notif>())
                {
                    for value in notif.iter() {
                        if *value != 0 {
                            return error!(EINVAL);
                        }
                    }
                }
                // A RECV reads a notification, optionally waiting for one to become available.
                let mut notif: Option<seccomp_notif>;
                loop {
                    // Grab a notification or wait for one to become readable.
                    let waiter = Waiter::new();
                    {
                        let mut notifier = self.notifier.lock();
                        notif = notifier.consume_some_notification();
                        if notif.is_some() {
                            break;
                        }
                        notifier.waiters.wait_async_events(
                            &waiter,
                            FdEvents::POLLIN | FdEvents::POLLHUP,
                            Box::new(|_: FdEvents| {}),
                        );
                    }
                    waiter.wait(current_task)?;
                }
                if let Some(notif) = notif {
                    if let Err(e) = current_task
                        .mm
                        .write_object(UserRef::<seccomp_notif>::new(user_addr), &notif)
                    {
                        self.notifier.lock().unconsume(notif.id);
                        return Err(e);
                    }
                }

                Ok(0.into())
            }
            SECCOMP_IOCTL_NOTIF_SEND => {
                // A SEND sends a response to a previously received notification.
                let resp: seccomp_notif_resp =
                    current_task.mm.read_object(UserRef::new(user_addr))?;
                if resp.flags & !SECCOMP_USER_NOTIF_FLAG_CONTINUE != 0 {
                    return error!(EINVAL);
                }
                if resp.flags & SECCOMP_USER_NOTIF_FLAG_CONTINUE != 0
                    && (resp.error != 0 || resp.val != 0)
                {
                    return error!(EINVAL);
                }
                {
                    let mut notifier = self.notifier.lock();
                    if let Some(err) = notifier.set_response(resp.id, resp) {
                        return Err(err);
                    }
                }
                Ok(0.into())
            }
            SECCOMP_IOCTL_NOTIF_ID_VALID => {
                // An ID_VALID indicates that the notification is still in progress.
                let cookie: u64 = current_task.mm.read_object(UserRef::new(user_addr))?;
                {
                    let notifier = self.notifier.lock();
                    if notifier.notification_pending(cookie) {
                        Ok(0.into())
                    } else {
                        error!(ENOENT)
                    }
                }
            }
            SECCOMP_IOCTL_NOTIF_ADDFD => error!(EINVAL),
            _ => error!(EINVAL),
        }
    }

    fn wait_async(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        waiter: &Waiter,
        events: FdEvents,
        handler: EventHandler,
    ) -> Option<WaitCanceler> {
        let notifier = self.notifier.lock();
        Some(notifier.waiters.wait_async_events(waiter, events, handler))
    }

    fn query_events(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
    ) -> Result<FdEvents, Errno> {
        Ok(self.notifier.lock().get_fd_notifications())
    }
}
