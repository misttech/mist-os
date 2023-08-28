// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use starnix_sync::InterruptibleEvent;
use std::{collections::VecDeque, sync::Arc};
use zerocopy::{AsBytes, FromBytes, FromZeroes};

use crate::{
    lock::RwLock,
    task::{TimerId, WaitQueue, WaiterRef},
    types::*,
};

/// `SignalActions` contains a `sigaction_t` for each valid signal.
#[derive(Debug)]
pub struct SignalActions {
    actions: RwLock<[sigaction_t; Signal::NUM_SIGNALS as usize + 1]>,
}

impl SignalActions {
    /// Returns a collection of `sigaction_t`s that contains default values for each signal.
    pub fn default() -> Arc<SignalActions> {
        Arc::new(SignalActions {
            actions: RwLock::new([sigaction_t::default(); Signal::NUM_SIGNALS as usize + 1]),
        })
    }

    /// Returns the `sigaction_t` that is currently set for `signal`.
    pub fn get(&self, signal: Signal) -> sigaction_t {
        // This is safe, since the actions always contain a value for each signal.
        self.actions.read()[signal.number() as usize]
    }

    /// Update the action for `signal`. Returns the previously configured action.
    pub fn set(&self, signal: Signal, new_action: sigaction_t) -> sigaction_t {
        let mut actions = self.actions.write();
        let old_action = actions[signal.number() as usize];
        actions[signal.number() as usize] = new_action;
        old_action
    }

    pub fn fork(&self) -> Arc<SignalActions> {
        Arc::new(SignalActions { actions: RwLock::new(*self.actions.read()) })
    }

    pub fn reset_for_exec(&self) {
        for action in self.actions.write().iter_mut() {
            if action.sa_handler != SIG_DFL && action.sa_handler != SIG_IGN {
                action.sa_handler = SIG_DFL;
            }
        }
    }
}

/// Whether, and how, this task is blocked. This enum can be extended with new
/// variants to optimize different kinds of waiting.
#[derive(Debug, Clone)]
pub enum RunState {
    /// This task is not blocked.
    ///
    /// The task might be running in userspace or kernel.
    Running,

    /// This thread is blocked in a `Waiter`.
    Waiter(WaiterRef),

    /// This thread is blocked in an `InterruptibleEvent`.
    Event(Arc<InterruptibleEvent>),
}

impl Default for RunState {
    fn default() -> Self {
        RunState::Running
    }
}

impl RunState {
    /// Whether this task is blocked.
    ///
    /// If the task is blocked, you can break the task out of the wait using the `wake` function.
    pub fn is_blocked(&self) -> bool {
        match self {
            RunState::Running => false,
            RunState::Waiter(waiter) => waiter.is_valid(),
            RunState::Event(_) => true,
        }
    }

    /// Unblock the task by interrupting whatever wait the task is blocked upon.
    pub fn wake(&self) {
        match self {
            RunState::Running => (),
            RunState::Waiter(waiter) => waiter.interrupt(),
            RunState::Event(event) => event.interrupt(),
        }
    }
}

impl PartialEq<RunState> for RunState {
    fn eq(&self, other: &RunState) -> bool {
        match (self, other) {
            (RunState::Running, RunState::Running) => true,
            (RunState::Waiter(lhs), RunState::Waiter(rhs)) => lhs == rhs,
            (RunState::Event(lhs), RunState::Event(rhs)) => Arc::ptr_eq(lhs, rhs),
            _ => false,
        }
    }
}

/// Per-task signal handling state.
#[derive(Default)]
pub struct SignalState {
    // See https://man7.org/linux/man-pages/man2/sigaltstack.2.html
    pub alt_stack: Option<sigaltstack_t>,

    /// Wait queue for signalfd and sigtimedwait. Signaled whenever a signal is added to the queue.
    pub signal_wait: WaitQueue,

    /// A handle for interrupting this task, if any.
    pub run_state: RunState,

    /// The signal mask of the task.
    // See https://man7.org/linux/man-pages/man2/rt_sigprocmask.2.html
    mask: SigSet,

    /// The signal mask that should be restored by the signal handling machinery, after dequeuing
    /// a signal.
    ///
    /// Some syscalls apply a temporary signal mask by setting `SignalState.mask` during the wait.
    /// This means that the mask must be set to the temporary mask when the signal is dequeued,
    /// which is done by the syscall dispatch loop before returning to userspace. After the signal
    /// is dequeued `mask` can be reset to `saved_mask`.
    saved_mask: Option<SigSet>,

    /// The queue of signals for a given task.
    ///
    /// There may be more than one instance of a real-time signal in the queue, but for standard
    /// signals there is only ever one instance of any given signal.
    queue: VecDeque<SignalInfo>,
}

impl SignalState {
    pub fn with_mask(mask: SigSet) -> Self {
        Self { mask, ..Default::default() }
    }

    /// Sets the signal mask of the state, and returns the old signal mask.
    pub fn set_mask(&mut self, signal_mask: SigSet) -> SigSet {
        let old_mask = self.mask;
        self.mask = signal_mask.with_sigset_removed(UNBLOCKABLE_SIGNALS);
        old_mask
    }

    /// Sets the signal mask of the state temporarily, until the signal machinery has completed its
    /// next dequeue operation. This can be used by syscalls that want to change the signal mask
    /// during a wait, but want the signal mask to be reset before returning back to userspace after
    /// the wait.
    pub fn set_temporary_mask(&mut self, signal_mask: SigSet) {
        assert!(self.saved_mask.is_none());
        self.saved_mask = Some(self.mask);
        self.mask = signal_mask.with_sigset_removed(UNBLOCKABLE_SIGNALS);
    }

    /// Restores the signal mask to what it was before the previous call to `set_temporary_mask`.
    /// If there is no saved mask, the mask is left alone.
    pub fn restore_mask(&mut self) {
        if let Some(mask) = self.saved_mask {
            self.mask = mask;
            self.saved_mask = None;
        }
    }

    pub fn mask(&self) -> SigSet {
        self.mask
    }

    pub fn enqueue(&mut self, siginfo: SignalInfo) {
        if siginfo.signal.is_real_time() || !self.has_queued(siginfo.signal) {
            self.queue.push_back(siginfo);
            self.signal_wait.notify_all();
        }
    }

    /// Finds the next queued signal where the given function returns true, removes it from the
    /// queue, and returns it.
    pub fn take_next_where<F>(&mut self, predicate: F) -> Option<SignalInfo>
    where
        F: Fn(&SignalInfo) -> bool,
    {
        // Find the first non-blocked signal
        let index = self.queue.iter().position(predicate)?;
        self.queue.remove(index)
    }

    /// Returns whether any signals are pending (queued and not blocked).
    pub fn is_any_pending(&self) -> bool {
        self.is_any_allowed_by_mask(self.mask)
    }

    /// Returns whether any signals are queued and not blocked by the given mask.
    pub fn is_any_allowed_by_mask(&self, mask: SigSet) -> bool {
        self.queue.iter().any(|sig| !mask.has_signal(sig.signal))
    }

    /// Iterates over queued signals with the given number.
    fn iter_queued_by_number(&self, signal: Signal) -> impl Iterator<Item = &SignalInfo> {
        self.queue.iter().filter(move |info| info.signal == signal)
    }

    /// Tests whether a signal with the given number is in the queue.
    pub fn has_queued(&self, signal: Signal) -> bool {
        self.iter_queued_by_number(signal).next().is_some()
    }

    #[cfg(test)]
    pub fn queued_count(&self, signal: Signal) -> usize {
        self.iter_queued_by_number(signal).count()
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq, AsBytes, FromZeroes, FromBytes)]
#[repr(C)]
pub struct SignalInfoHeader {
    pub signo: u32,
    pub errno: i32,
    pub code: i32,
    pub _pad: i32,
}

pub const SI_HEADER_SIZE: usize = std::mem::size_of::<SignalInfoHeader>();

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignalInfo {
    pub signal: Signal,
    pub errno: i32,
    pub code: i32,
    pub detail: SignalDetail,
    pub force: bool,
}

impl SignalInfo {
    pub fn default(signal: Signal) -> Self {
        Self::new(signal, SI_KERNEL, SignalDetail::default())
    }

    pub fn new(signal: Signal, code: u32, detail: SignalDetail) -> Self {
        Self { signal, errno: 0, code: code as i32, detail, force: false }
    }

    // TODO(tbodt): Add a bound requiring siginfo_t to be FromBytes. This will help ensure the
    // Linux side won't get an invalid siginfo_t.
    pub fn as_siginfo_bytes(&self) -> [u8; std::mem::size_of::<siginfo_t>()] {
        macro_rules! make_siginfo {
            ($self:ident $(, $sifield:ident, $value:expr)?) => {
                struct_with_union_into_bytes!(siginfo_t {
                    __bindgen_anon_1.__bindgen_anon_1.si_signo: $self.signal.number() as i32,
                    __bindgen_anon_1.__bindgen_anon_1.si_errno: $self.errno,
                    __bindgen_anon_1.__bindgen_anon_1.si_code: $self.code,
                    $(
                        __bindgen_anon_1.__bindgen_anon_1._sifields.$sifield: $value,
                    )?
                })
            };
        }

        match self.detail {
            SignalDetail::None => make_siginfo!(self),
            SignalDetail::SigChld { pid, uid, status } => make_siginfo!(
                self,
                _sigchld,
                __sifields__bindgen_ty_4 {
                    _pid: pid,
                    _uid: uid,
                    _status: status,
                    ..Default::default()
                }
            ),
            SignalDetail::SigSys { call_addr, syscall, arch } => make_siginfo!(
                self,
                _sigsys,
                __sifields__bindgen_ty_7 {
                    _call_addr: call_addr.into(),
                    _syscall: syscall as c_int,
                    _arch: arch as c_uint,
                }
            ),
            SignalDetail::Timer { timer_id: tid, overrun, sigval } => {
                make_siginfo!(
                    self,
                    _timer,
                    __sifields__bindgen_ty_2 {
                        _tid: tid,
                        _overrun: overrun,
                        _sigval: sigval.into(),
                        ..Default::default()
                    }
                )
            }
            SignalDetail::Raw { data } => {
                let header = SignalInfoHeader {
                    signo: self.signal.number(),
                    errno: self.errno,
                    code: self.code,
                    _pad: 0,
                };
                let mut array: [u8; SI_MAX_SIZE as usize] = [0; SI_MAX_SIZE as usize];
                header.write_to(&mut array[..SI_HEADER_SIZE]);
                array[SI_HEADER_SIZE..SI_MAX_SIZE as usize].copy_from_slice(&data);
                array
            }
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SignalDetail {
    None,
    SigChld {
        pid: pid_t,
        uid: uid_t,
        status: i32,
    },
    SigSys {
        call_addr: UserAddress,
        syscall: i32,
        arch: u32,
    },
    /// POSIX timer
    Timer {
        timer_id: TimerId,
        /// For the timer referred to by the `timer_id, the number of timer expirations that
        /// occurred between the time when the signal was generated and when it was delivered or
        /// accepted.
        overrun: i32,
        /// Data passed with notification from asynchronous routines.
        sigval: SignalEventValue,
    },
    Raw {
        data: [u8; SI_MAX_SIZE as usize - SI_HEADER_SIZE],
    },
}

impl Default for SignalDetail {
    fn default() -> Self {
        Self::None
    }
}

#[derive(Debug)]
pub struct SignalEvent {
    pub value: Option<SignalEventValue>,
    pub signo: Option<Signal>,
    pub notify: SignalEventNotify,
}

impl SignalEvent {
    pub fn new(value: SignalEventValue, signo: Signal, notify: SignalEventNotify) -> Self {
        Self { value: Some(value), signo: Some(signo), notify }
    }

    pub fn none() -> Self {
        Self { value: None, signo: None, notify: SignalEventNotify::None }
    }
}

impl TryFrom<sigevent> for SignalEvent {
    type Error = Errno;

    fn try_from(value: sigevent) -> Result<Self, Self::Error> {
        // SAFETY: _sigev_un was created with FromBytes so it's safe to access any variant
        // because all variants must be valid with all bit patterns.
        let notify = match value.sigev_notify as u32 {
            SIGEV_SIGNAL => SignalEventNotify::Signal,
            SIGEV_NONE => SignalEventNotify::None,
            SIGEV_THREAD => unsafe {
                SignalEventNotify::Thread {
                    function: value._sigev_un._sigev_thread._function.into(),
                    attribute: value._sigev_un._sigev_thread._attribute.into(),
                }
            },
            SIGEV_THREAD_ID => SignalEventNotify::ThreadId(unsafe { value._sigev_un._tid }),
            _ => return error!(EINVAL),
        };

        Ok(match notify {
            SignalEventNotify::None => SignalEvent::none(),
            _ => SignalEvent::new(
                value.sigev_value.into(),
                UncheckedSignal::new(value.sigev_signo as u64).try_into()?,
                notify,
            ),
        })
    }
}

#[derive(Debug)]
/// Specifies how notification is to be performed.
pub enum SignalEventNotify {
    /// Notify the process by sending the signal specified in `SignalInfo::Signal`.
    Signal,
    /// Don't do anything when the event occurs.
    None,
    /// Notify the process by invoking the `function` as if it were the start function of
    /// a new thread.
    Thread { function: UserAddress, attribute: UserAddress },
    /// Similar to `SignalNotify::Signal`, but the signal is targeted at the thread ID.
    ThreadId(pid_t),
}

impl From<SignalEventNotify> for i32 {
    fn from(value: SignalEventNotify) -> Self {
        match value {
            SignalEventNotify::Signal => SIGEV_SIGNAL as i32,
            SignalEventNotify::None => SIGEV_NONE as i32,
            SignalEventNotify::Thread { .. } => SIGEV_THREAD as i32,
            SignalEventNotify::ThreadId(_) => SIGEV_THREAD_ID as i32,
        }
    }
}

/// Data passed with signal event notification.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct SignalEventValue(pub u64);

impl From<sigval_t> for SignalEventValue {
    fn from(value: sigval_t) -> Self {
        SignalEventValue(unsafe { value._bindgen_opaque_blob })
    }
}

impl From<SignalEventValue> for sigval_t {
    fn from(value: SignalEventValue) -> Self {
        Self { _bindgen_opaque_blob: value.0 }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use std::convert::TryFrom;

    #[::fuchsia::test]
    fn test_signal() {
        assert!(Signal::try_from(UncheckedSignal::from(0)).is_err());
        assert!(Signal::try_from(UncheckedSignal::from(1)).is_ok());
        assert!(Signal::try_from(UncheckedSignal::from(Signal::NUM_SIGNALS)).is_ok());
        assert!(Signal::try_from(UncheckedSignal::from(Signal::NUM_SIGNALS + 1)).is_err());
        assert!(!SIGCHLD.is_real_time());
        assert!(Signal::try_from(UncheckedSignal::from(uapi::SIGRTMIN + 12))
            .unwrap()
            .is_real_time());
        assert_eq!(format!("{SIGPWR}"), "signal 30: SIGPWR");
        assert_eq!(
            format!("{}", Signal::try_from(UncheckedSignal::from(uapi::SIGRTMIN + 10)).unwrap()),
            "signal 42: SIGRTMIN+10"
        );
    }

    #[::fuchsia::test]
    fn test_siginfo_bytes() {
        let mut sigchld_bytes =
            vec![17, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 123, 0, 0, 0, 200, 1, 0, 0, 2];
        sigchld_bytes.resize(std::mem::size_of::<siginfo_t>(), 0);
        assert_eq!(
            &SignalInfo::new(
                SIGCHLD,
                CLD_EXITED,
                SignalDetail::SigChld { pid: 123, uid: 456, status: 2 }
            )
            .as_siginfo_bytes(),
            sigchld_bytes.as_slice()
        );
    }
}
