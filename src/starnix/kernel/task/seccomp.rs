// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::sync::atomic::{AtomicU8, Ordering};
use ubpf::{
    converter::{bpf_addressing_mode, bpf_class},
    program::EbpfProgram,
};

use crate::{
    logging::log_warn,
    signals::{send_signal, SignalDetail, SignalInfo},
    syscalls::{decls::Syscall, SyscallResult},
    task::{CurrentTask, ExitStatus, Task},
    types::*,
};

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
                return Err(errno!(EINVAL));
            }
        }

        match EbpfProgram::from_cbpf(code) {
            Ok(program) => Ok(SeccompFilter { program, unique_id: maybe_unique_id }),
            Err(errmsg) => {
                log_warn!("{}", errmsg);
                Err(errno!(EINVAL))
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
#[derive(Clone, Default)]
pub struct SeccompFilterContainer {
    /// List of currently installed seccomp_filters; most recently added is last.
    pub filters: Vec<SeccompFilter>,

    // The total length of the provided seccomp filters, which cannot
    // exceed SECCOMP_MAX_INSNS_PER_PATH - 4 * the number of filters.  This is stored
    // instead of computed because we store seccomp filters in an
    // expanded form, and it is impossible to get the original length.
    pub provided_instructions: u16,
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
    pub fn add_filter(&mut self, filter: SeccompFilter, original_length: u16) -> Result<(), Errno> {
        let maybe_new_length = self.provided_instructions + original_length + 4;
        if maybe_new_length > SECCOMP_MAX_INSNS_PER_PATH {
            return Err(errno!(ENOMEM));
        }

        self.provided_instructions = maybe_new_length;
        self.filters.push(filter);
        Ok(())
    }

    /// Runs all of the seccomp filters in this container, most-to-least recent.  Returns the
    /// highest priority result.
    pub fn run_all(&self, task: &CurrentTask, syscall: &Syscall) -> u32 {
        // VDSO calls can't be caught by seccomp, so most seccomp filters forget to declare them.
        // But our VDSO implementation is incomplete, and most of the calls forward to the actual
        // syscalls. So seccomp should ignore them until they're implemented correctly in the VDSO.
        #[cfg(target_arch = "x86_64")] // The set of VDSO calls is arch dependent.
        #[allow(non_upper_case_globals)]
        if let __NR_clock_gettime | __NR_getcpu | __NR_gettimeofday | __NR_time =
            syscall.decl.number as u32
        {
            return SECCOMP_RET_ALLOW;
        }
        #[cfg(target_arch = "aarch64")]
        #[allow(non_upper_case_globals)]
        if let __NR_clock_gettime | __NR_clock_getres | __NR_gettimeofday =
            syscall.decl.number as u32
        {
            return SECCOMP_RET_ALLOW;
        }

        #[cfg(target_arch = "x86_64")]
        let arch_val = AUDIT_ARCH_X86_64;
        #[cfg(target_arch = "aarch64")]
        let arch_val = AUDIT_ARCH_AARCH64;
        #[cfg(target_arch = "riscv64")]
        let arch_val = AUDIT_ARCH_RISCV64;

        let mut result = SECCOMP_RET_ALLOW;
        // Filters are executed in reverse order of addition
        for filter in self.filters.iter().rev() {
            let mut data = seccomp_data {
                nr: syscall.decl.number as i32,
                arch: arch_val,
                instruction_pointer: task.registers.instruction_pointer_register(),
                args: [
                    syscall.arg0.raw(),
                    syscall.arg1.raw(),
                    syscall.arg2.raw(),
                    syscall.arg3.raw(),
                    syscall.arg4.raw(),
                    syscall.arg5.raw(),
                ],
            };

            let new_result = filter.run(&mut data);
            if ((new_result & SECCOMP_RET_ACTION_FULL) as i32)
                < ((result & SECCOMP_RET_ACTION_FULL) as i32)
            {
                result = new_result;
            }
        }
        result
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
    pub fn do_strict(task: &Task, syscall: &Syscall) -> Option<Errno> {
        if syscall.decl.number as u32 != __NR_exit
            && syscall.decl.number as u32 != __NR_read
            && syscall.decl.number as u32 != __NR_write
        {
            send_signal(task, SignalInfo::default(SIGKILL));
            return Some(errno_from_code!(0));
        }
        None
    }

    /// Take the given |action| on the given |task|.  The action is one of the SECCOMP_RET values
    /// (ALLOW, LOG, KILL, KILL_PROCESS, TRAP, ERRNO, USER_NOTIF, TRACE).  |task| is the thread that
    /// invoked the syscall, and |syscall| is the syscall that was invoked.
    // NB: Allow warning below so that it is clear what we are doing on KILL_PROCESS
    #[allow(clippy::wildcard_in_or_patterns)]
    pub fn do_user_defined(action: u32, task: &CurrentTask, syscall: &Syscall) -> Option<Errno> {
        match action & !SECCOMP_RET_DATA {
            SECCOMP_RET_ALLOW => None,
            SECCOMP_RET_ERRNO => {
                // Linux kernel compatibility: if errno exceeds 0xfff, it is capped at 0xfff.
                Some(errno_from_code!(std::cmp::min(action & 0xffff, 0xfff) as i16))
            }
            SECCOMP_RET_KILL_THREAD => {
                let siginfo = SignalInfo::default(SIGSYS);

                let is_last_thread = task.thread_group.read().tasks.len() == 1;
                let mut task_state = task.write();

                if is_last_thread {
                    task_state.dump_on_exit = true;
                    task_state.exit_status.get_or_insert(ExitStatus::CoreDump(siginfo));
                } else {
                    task_state.exit_status.get_or_insert(ExitStatus::Kill(siginfo));
                }
                Some(errno_from_code!(0))
            }
            SECCOMP_RET_LOG => {
                let creds = task.creds();
                let uid = creds.uid;
                let gid = creds.gid;
                let comm_r = task.command();
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
                    task.thread_group.leader,
                    comm,
                    syscall.decl.number,
                    task.registers.instruction_pointer_register(),
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
                        call_addr: task.registers.instruction_pointer_register().into(),
                        syscall: syscall.decl.number as i32,
                        arch: arch_val,
                    },
                    force: true,
                };

                send_signal(task, siginfo);
                Some(errno_from_code!(-(syscall.decl.number as i16)))
            }
            SECCOMP_RET_KILL_PROCESS | _ => {
                task.thread_group.exit(ExitStatus::CoreDump(SignalInfo::default(SIGSYS)));
                Some(errno_from_code!(0))
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
            | SECCOMP_RET_TRAP => Ok(().into()),
            // TODO(fxbug.dev/126644): Implement these.
            SECCOMP_RET_TRACE | SECCOMP_RET_USER_NOTIF => {
                error!(EOPNOTSUPP)
            }
            _ => {
                error!(EOPNOTSUPP)
            }
        }
    }
}
