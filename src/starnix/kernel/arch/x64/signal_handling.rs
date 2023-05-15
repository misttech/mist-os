// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fuchsia_zircon as zx;

use crate::arch::registers::RegisterState;
use crate::signals::*;
use crate::task::*;
use crate::types::*;

/// The size of the red zone.
///
/// From the AMD64 ABI:
///   > The 128-byte area beyond the location pointed to
///   > by %rsp is considered to be reserved and shall not be modified by signal or
///   > interrupt handlers. Therefore, functions may use this area for temporary
///   > data that is not needed across function calls. In particular, leaf functions
///   > may use this area for their entire stack frame, rather than adjusting the
///   > stack pointer in the prologue and epilogue. This area is known as the red
///   > zone.
pub const RED_ZONE_SIZE: u64 = 128;

/// The size of the syscall instruction in bytes.
pub const SYSCALL_INSTRUCTION_SIZE_BYTES: u64 = 2;

/// A `SignalStackFrame` contains all the state that is stored on the stack prior
/// to executing a signal handler.
///
/// The ordering of the fields is significant, as it is part of the syscall ABI. In particular,
/// restorer_address must be the first field, since that is where the signal handler will return
/// after it finishes executing.
#[repr(C)]
pub struct SignalStackFrame {
    /// The address of the signal handler function.
    ///
    /// Must be the first field, to be positioned to serve as the return address.
    restorer_address: u64,

    /// Information about the signal.
    pub siginfo_bytes: [u8; std::mem::size_of::<siginfo_t>()],

    /// The state of the thread at the time the signal was handled.
    pub context: ucontext,
    /// The FPU state. Must be immediately after the ucontext field, since Bionic considers this to
    /// be part of the ucontext for some reason.
    fpstate: _fpstate_64,
}

pub const SIG_STACK_SIZE: usize = std::mem::size_of::<SignalStackFrame>();

impl SignalStackFrame {
    pub fn new(
        _task: &Task,
        registers: &RegisterState,
        signal_state: &SignalState,
        siginfo: &SignalInfo,
        action: sigaction_t,
    ) -> SignalStackFrame {
        let context = ucontext {
            uc_mcontext: sigcontext {
                r8: registers.r8,
                r9: registers.r9,
                r10: registers.r10,
                r11: registers.r11,
                r12: registers.r12,
                r13: registers.r13,
                r14: registers.r14,
                r15: registers.r15,
                rdi: registers.rdi,
                rsi: registers.rsi,
                rbp: registers.rbp,
                rbx: registers.rbx,
                rdx: registers.rdx,
                rax: registers.rax,
                rcx: registers.rcx,
                rsp: registers.rsp,
                rip: registers.rip,
                eflags: registers.rflags,
                oldmask: signal_state.mask().into(),
                ..Default::default()
            },
            uc_stack: signal_state
                .alt_stack
                .map(|stack| sigaltstack {
                    ss_sp: stack.ss_sp.ptr(),
                    ss_flags: stack.ss_flags as i32,
                    ss_size: stack.ss_size as u64,
                    ..Default::default()
                })
                .unwrap_or_default(),
            uc_sigmask: signal_state.mask().into(),
            ..Default::default()
        };
        let restorer_address = action.sa_restorer.ptr() as u64;
        SignalStackFrame {
            context,
            siginfo_bytes: siginfo.as_siginfo_bytes(),
            restorer_address,
            fpstate: Default::default(),
        }
    }

    pub fn as_bytes(&self) -> &[u8; SIG_STACK_SIZE] {
        unsafe { std::mem::transmute(self) }
    }

    pub fn from_bytes(bytes: [u8; SIG_STACK_SIZE]) -> SignalStackFrame {
        unsafe { std::mem::transmute(bytes) }
    }
}

impl From<sigset_t> for SigSet {
    fn from(value: sigset_t) -> Self {
        SigSet(value)
    }
}

impl From<SigSet> for sigset_t {
    fn from(val: SigSet) -> Self {
        val.0
    }
}

/// Aligns the stack pointer to be 16 byte aligned, and then misaligns it by 8 bytes.
///
/// This is done because x86-64 functions expect the stack to be misaligned by 8 bytes,
/// as if the stack was 16 byte aligned and then someone used a call instruction. This
/// is due to alignment-requiring SSE instructions.
pub fn align_stack_pointer(pointer: u64) -> u64 {
    pointer - (pointer % 16 + 8)
}

pub fn restore_registers(current_task: &mut CurrentTask, signal_stack_frame: &SignalStackFrame) {
    let uctx = &signal_stack_frame.context.uc_mcontext;
    // Restore the register state from before executing the signal handler.
    current_task.registers = zx::sys::zx_thread_state_general_regs_t {
        r8: uctx.r8,
        r9: uctx.r9,
        r10: uctx.r10,
        r11: uctx.r11,
        r12: uctx.r12,
        r13: uctx.r13,
        r14: uctx.r14,
        r15: uctx.r15,
        rax: uctx.rax,
        rbx: uctx.rbx,
        rcx: uctx.rcx,
        rdx: uctx.rdx,
        rsi: uctx.rsi,
        rdi: uctx.rdi,
        rbp: uctx.rbp,
        rsp: uctx.rsp,
        rip: uctx.rip,
        rflags: uctx.eflags,
        fs_base: current_task.registers.fs_base,
        gs_base: current_task.registers.gs_base,
    }
    .into();
}

pub fn update_register_state_for_restart(registers: &mut RegisterState, err: ErrnoCode) {
    registers.rax = match err {
        // Custom restart, invoke restart_syscall instead of the original syscall.
        ERESTART_RESTARTBLOCK => __NR_restart_syscall as u64,
        // If the restart is not custom, simply replace `rax` with the value it had when the
        // original syscall trap occurred.
        _ => registers.orig_rax,
    };
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::mm::{DesiredAddress, MappingName, MappingOptions, ProtectionFlags};
    use crate::testing::*;

    const SYSCALL_INSTRUCTION_ADDRESS: UserAddress = UserAddress::from(100);
    const SYSCALL_NUMBER: u64 = 42;
    const SYSCALL_ARGS: (u64, u64, u64, u64, u64, u64) = (20, 21, 22, 23, 24, 25);
    const SA_RESTORER_ADDRESS: UserAddress = UserAddress::from(0xDEADBEEF);
    const SA_HANDLER_ADDRESS: UserAddress = UserAddress::from(0x00BADDAD);

    const SYSCALL2_INSTRUCTION_ADDRESS: UserAddress = UserAddress::from(200);
    const SYSCALL2_NUMBER: u64 = 84;
    const SYSCALL2_ARGS: (u64, u64, u64, u64, u64, u64) = (30, 31, 32, 33, 34, 35);
    const SA_HANDLER2_ADDRESS: UserAddress = UserAddress::from(0xBADDAD00);

    #[::fuchsia::test]
    fn syscall_restart_adjusts_instruction_pointer_and_rax() {
        let (_kernel, mut current_task) = create_kernel_and_task_with_stack();

        // Register the signal action.
        current_task.thread_group.signal_actions.set(
            SIGUSR1,
            sigaction_t {
                sa_flags: (SA_RESTORER | SA_RESTART | SA_SIGINFO) as u64,
                sa_handler: SA_HANDLER_ADDRESS,
                sa_restorer: SA_RESTORER_ADDRESS,
                ..sigaction_t::default()
            },
        );

        // Simulate a syscall that should be restarted by setting up the register state to what it
        // was after the interrupted syscall. `rax` should have the return value (-ERESTARTSYS);
        // `rdi`, `rsi`, `rdx`, `r10`, `r8`, `r9`, should be the syscall arguments;
        // `orig_rax` should hold the syscall number;
        // and the instruction pointer should be 2 bytes after the syscall instruction.
        current_task.registers.rax = ERESTARTSYS.return_value();
        current_task.registers.rdi = SYSCALL_ARGS.0;
        current_task.registers.rsi = SYSCALL_ARGS.1;
        current_task.registers.rdx = SYSCALL_ARGS.2;
        current_task.registers.r10 = SYSCALL_ARGS.3;
        current_task.registers.r8 = SYSCALL_ARGS.4;
        current_task.registers.r9 = SYSCALL_ARGS.5;
        current_task.registers.orig_rax = SYSCALL_NUMBER;
        current_task.registers.rip = (SYSCALL_INSTRUCTION_ADDRESS + 2u64).ptr() as u64;

        // Queue the signal that interrupted the syscall.
        current_task.write().signals.enqueue(SignalInfo::new(SIGUSR1, SI_USER, SignalDetail::None));

        // Process the signal.
        dequeue_signal(&mut current_task);

        // The instruction pointer should have changed to the signal handling address.
        assert_eq!(current_task.registers.rip, SA_HANDLER_ADDRESS.ptr() as u64);

        // The syscall arguments should be overwritten with signal handling args.
        assert_ne!(current_task.registers.rdi, SYSCALL_ARGS.0);
        assert_ne!(current_task.registers.rsi, SYSCALL_ARGS.1);
        assert_ne!(current_task.registers.rdx, SYSCALL_ARGS.2);

        // Now we assume that execution of the signal handler completed with a call to
        // `sys_rt_sigreturn`, which would set `rax` to that syscall number.
        current_task.registers.rax = __NR_rt_sigreturn as u64;
        current_task.registers.rsp += 8; // The stack was popped returning from the signal handler.

        restore_from_signal_handler(&mut current_task).expect("failed to restore state");

        // The state of the task is now such that when switching back to userspace, the instruction
        // pointer will point at the original syscall instruction, with the arguments correctly
        // restored into the registers.
        assert_eq!(current_task.registers.rax, SYSCALL_NUMBER);
        assert_eq!(current_task.registers.rdi, SYSCALL_ARGS.0);
        assert_eq!(current_task.registers.rsi, SYSCALL_ARGS.1);
        assert_eq!(current_task.registers.rdx, SYSCALL_ARGS.2);
        assert_eq!(current_task.registers.r10, SYSCALL_ARGS.3);
        assert_eq!(current_task.registers.r8, SYSCALL_ARGS.4);
        assert_eq!(current_task.registers.r9, SYSCALL_ARGS.5);
        assert_eq!(current_task.registers.rip, SYSCALL_INSTRUCTION_ADDRESS.ptr() as u64);
    }

    #[::fuchsia::test]
    fn syscall_nested_restart() {
        let (_kernel, mut current_task) = create_kernel_and_task_with_stack();

        // Register the signal actions.
        current_task.thread_group.signal_actions.set(
            SIGUSR1,
            sigaction_t {
                sa_flags: (SA_RESTORER | SA_RESTART | SA_SIGINFO) as u64,
                sa_handler: SA_HANDLER_ADDRESS,
                sa_restorer: SA_RESTORER_ADDRESS,
                ..sigaction_t::default()
            },
        );
        current_task.thread_group.signal_actions.set(
            SIGUSR2,
            sigaction_t {
                sa_flags: (SA_RESTORER | SA_RESTART | SA_SIGINFO) as u64,
                sa_handler: SA_HANDLER2_ADDRESS,
                sa_restorer: SA_RESTORER_ADDRESS,
                ..sigaction_t::default()
            },
        );

        // Simulate a syscall that should be restarted by setting up the register state to what it
        // was after the interrupted syscall. `rax` should have the return value (-ERESTARTSYS);
        // `rdi`, `rsi`, `rdx`, `r10`, `r8`, `r9`, should be the syscall arguments;
        // `orig_rax` should hold the syscall number;
        // and the instruction pointer should be 2 bytes after the syscall instruction.
        current_task.registers.rax = ERESTARTSYS.return_value();
        current_task.registers.rdi = SYSCALL_ARGS.0;
        current_task.registers.rsi = SYSCALL_ARGS.1;
        current_task.registers.rdx = SYSCALL_ARGS.2;
        current_task.registers.r10 = SYSCALL_ARGS.3;
        current_task.registers.r8 = SYSCALL_ARGS.4;
        current_task.registers.r9 = SYSCALL_ARGS.5;
        current_task.registers.orig_rax = SYSCALL_NUMBER;
        current_task.registers.rip = (SYSCALL_INSTRUCTION_ADDRESS + 2u64).ptr() as u64;

        // Queue the signal that interrupted the syscall.
        current_task.write().signals.enqueue(SignalInfo::new(SIGUSR1, SI_USER, SignalDetail::None));

        // Process the signal.
        dequeue_signal(&mut current_task);

        // The instruction pointer should have changed to the signal handling address.
        assert_eq!(current_task.registers.rip, SA_HANDLER_ADDRESS.ptr() as u64);

        // The syscall arguments should be overwritten with signal handling args.
        assert_ne!(current_task.registers.rdi, SYSCALL_ARGS.0);
        assert_ne!(current_task.registers.rsi, SYSCALL_ARGS.1);
        assert_ne!(current_task.registers.rdx, SYSCALL_ARGS.2);

        // Simulate another syscall being interrupted.
        current_task.registers.rax = ERESTARTSYS.return_value();
        current_task.registers.rdi = SYSCALL2_ARGS.0;
        current_task.registers.rsi = SYSCALL2_ARGS.1;
        current_task.registers.rdx = SYSCALL2_ARGS.2;
        current_task.registers.r10 = SYSCALL2_ARGS.3;
        current_task.registers.r8 = SYSCALL2_ARGS.4;
        current_task.registers.r9 = SYSCALL2_ARGS.5;
        current_task.registers.orig_rax = SYSCALL2_NUMBER;
        current_task.registers.rip = (SYSCALL2_INSTRUCTION_ADDRESS + 2u64).ptr() as u64;

        // Queue the signal that interrupted the syscall.
        current_task.write().signals.enqueue(SignalInfo::new(SIGUSR2, SI_USER, SignalDetail::None));

        // Process the signal.
        dequeue_signal(&mut current_task);

        // The instruction pointer should have changed to the signal handling address.
        assert_eq!(current_task.registers.rip, SA_HANDLER2_ADDRESS.ptr() as u64);

        // The syscall arguments should be overwritten with signal handling args.
        assert_ne!(current_task.registers.rdi, SYSCALL2_ARGS.0);
        assert_ne!(current_task.registers.rsi, SYSCALL2_ARGS.1);
        assert_ne!(current_task.registers.rdx, SYSCALL2_ARGS.2);

        // Now we assume that execution of the second signal handler completed with a call to
        // `sys_rt_sigreturn`, which would set `rax` to that syscall number.
        current_task.registers.rax = __NR_rt_sigreturn as u64;
        current_task.registers.rsp += 8; // The stack was popped returning from the signal handler.

        restore_from_signal_handler(&mut current_task).expect("failed to restore state");

        // The state of the task is now such that when switching back to userspace, the instruction
        // pointer will point at the original syscall instruction, with the arguments correctly
        // restored into the registers.
        assert_eq!(current_task.registers.rax, SYSCALL2_NUMBER);
        assert_eq!(current_task.registers.rdi, SYSCALL2_ARGS.0);
        assert_eq!(current_task.registers.rsi, SYSCALL2_ARGS.1);
        assert_eq!(current_task.registers.rdx, SYSCALL2_ARGS.2);
        assert_eq!(current_task.registers.r10, SYSCALL2_ARGS.3);
        assert_eq!(current_task.registers.r8, SYSCALL2_ARGS.4);
        assert_eq!(current_task.registers.r9, SYSCALL2_ARGS.5);
        assert_eq!(current_task.registers.rip, SYSCALL2_INSTRUCTION_ADDRESS.ptr() as u64);

        // Now we assume that execution of the first signal handler completed with a call to
        // `sys_rt_sigreturn`, which would set `rax` to that syscall number.
        current_task.registers.rax = __NR_rt_sigreturn as u64;
        current_task.registers.rsp += 8; // The stack was popped returning from the signal handler.

        restore_from_signal_handler(&mut current_task).expect("failed to restore state");

        // The state of the task is now such that when switching back to userspace, the instruction
        // pointer will point at the original syscall instruction, with the arguments correctly
        // restored into the registers.
        assert_eq!(current_task.registers.rax, SYSCALL_NUMBER);
        assert_eq!(current_task.registers.rdi, SYSCALL_ARGS.0);
        assert_eq!(current_task.registers.rsi, SYSCALL_ARGS.1);
        assert_eq!(current_task.registers.rdx, SYSCALL_ARGS.2);
        assert_eq!(current_task.registers.r10, SYSCALL_ARGS.3);
        assert_eq!(current_task.registers.r8, SYSCALL_ARGS.4);
        assert_eq!(current_task.registers.r9, SYSCALL_ARGS.5);
        assert_eq!(current_task.registers.rip, SYSCALL_INSTRUCTION_ADDRESS.ptr() as u64);
    }

    #[::fuchsia::test]
    fn syscall_does_not_restart_if_signal_action_has_no_sa_restart_flag() {
        let (_kernel, mut current_task) = create_kernel_and_task_with_stack();

        // Register the signal action.
        current_task.thread_group.signal_actions.set(
            SIGUSR1,
            sigaction_t {
                sa_flags: (SA_RESTORER | SA_SIGINFO) as u64,
                sa_handler: SA_HANDLER_ADDRESS,
                sa_restorer: SA_RESTORER_ADDRESS,
                ..sigaction_t::default()
            },
        );

        // Simulate a syscall that should be restarted by setting up the register state to what it
        // was after the interrupted syscall. `rax` should have the return value (-ERESTARTSYS);
        // `rdi`, `rsi`, `rdx`, `r10`, `r8`, `r9`, should be the syscall arguments;
        // `orig_rax` should hold the syscall number;
        // and the instruction pointer should be 2 bytes after the syscall instruction.
        current_task.registers.rax = ERESTARTSYS.return_value();
        current_task.registers.rdi = SYSCALL_ARGS.0;
        current_task.registers.rsi = SYSCALL_ARGS.1;
        current_task.registers.rdx = SYSCALL_ARGS.2;
        current_task.registers.r10 = SYSCALL_ARGS.3;
        current_task.registers.r8 = SYSCALL_ARGS.4;
        current_task.registers.r9 = SYSCALL_ARGS.5;
        current_task.registers.orig_rax = SYSCALL_NUMBER;
        current_task.registers.rip = (SYSCALL_INSTRUCTION_ADDRESS + 2u64).ptr() as u64;

        // Queue the signal that interrupted the syscall.
        current_task.write().signals.enqueue(SignalInfo::new(SIGUSR1, SI_USER, SignalDetail::None));

        // Process the signal.
        dequeue_signal(&mut current_task);

        // The instruction pointer should have changed to the signal handling address.
        assert_eq!(current_task.registers.rip, SA_HANDLER_ADDRESS.ptr() as u64);

        // The syscall arguments should be overwritten with signal handling args.
        assert_ne!(current_task.registers.rdi, SYSCALL_ARGS.0);
        assert_ne!(current_task.registers.rsi, SYSCALL_ARGS.1);
        assert_ne!(current_task.registers.rdx, SYSCALL_ARGS.2);

        // Now we assume that execution of the signal handler completed with a call to
        // `sys_rt_sigreturn`, which would set `rax` to that syscall number.
        current_task.registers.rax = __NR_rt_sigreturn as u64;
        current_task.registers.rsp += 8; // The stack was popped returning from the signal handler.

        restore_from_signal_handler(&mut current_task).expect("failed to restore state");

        // The state of the task is now such that when switching back to userspace, the instruction
        // pointer will point at the original syscall instruction, with the arguments correctly
        // restored into the registers.
        assert_eq!(current_task.registers.rax, EINTR.return_value());
        assert_eq!(current_task.registers.rip, (SYSCALL_INSTRUCTION_ADDRESS + 2u64).ptr() as u64);
    }

    /// Creates a kernel and initial task, giving the task a stack.
    fn create_kernel_and_task_with_stack() -> (Arc<Kernel>, CurrentTask) {
        let (kernel, mut current_task) = create_kernel_and_task();

        const STACK_SIZE: usize = 0x1000;

        // Give the task a stack.
        let prot_flags = ProtectionFlags::READ | ProtectionFlags::WRITE;
        let stack_base = current_task
            .mm
            .map(
                DesiredAddress::Hint(UserAddress::default()),
                Arc::new(zx::Vmo::create(STACK_SIZE as u64).expect("failed to create stack VMO")),
                0,
                STACK_SIZE,
                prot_flags,
                prot_flags.to_vmar_flags(),
                MappingOptions::empty(),
                MappingName::Stack,
            )
            .expect("failed to map stack VMO");
        current_task.registers.rsp = (stack_base + (STACK_SIZE - 8)).ptr() as u64;

        (kernel, current_task)
    }
}
