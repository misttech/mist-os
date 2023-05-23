// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fuchsia_zircon as zx;

use crate::arch::registers::RegisterState;
use crate::mm::vmo::round_up_to_increment;
use crate::signals::*;
use crate::task::*;
use crate::types::*;

/// The size of the red zone.
// TODO(fxbug.dev/121659): Determine whether or not this is the correct red zone size for aarch64.
pub const RED_ZONE_SIZE: u64 = 128;

/// The size of the syscall instruction in bytes.
pub const SYSCALL_INSTRUCTION_SIZE_BYTES: u64 = 12;

/// The size, in bytes, of the signal stack frame.
pub const SIG_STACK_SIZE: usize = std::mem::size_of::<SignalStackFrame>();

/// A `SignalStackFrame` contains all the state that is stored on the stack prior to executing a
/// signal handler. The exact layout of this structure is part of the platform's ABI.
#[repr(C)]
pub struct SignalStackFrame {
    pub siginfo_bytes: [u8; std::mem::size_of::<siginfo_t>()],
    pub context: ucontext,
}

impl SignalStackFrame {
    pub fn new(
        task: &Task,
        registers: &mut RegisterState,
        signal_state: &SignalState,
        siginfo: &SignalInfo,
        _action: sigaction_t,
    ) -> SignalStackFrame {
        let mut regs = registers.r.to_vec();
        regs.push(registers.lr);
        let context = ucontext {
            uc_flags: 0,
            uc_link: std::ptr::null_mut(),
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
            uc_mcontext: sigcontext {
                regs: regs.try_into().unwrap(),
                sp: registers.sp,
                pc: registers.pc,
                pstate: registers.cpsr,
                // TODO(fxbug.dev/121659): Should actually contain the fault address for SIGBUS and
                // SIGSEGV.
                fault_address: 0,
                ..Default::default()
            },
            ..Default::default()
        };

        let vdso_sigreturn_offset = task.thread_group.kernel.vdso.sigreturn_offset.expect(
            "The offset for vDSO sigreturn implementation must be known on this architecture.",
        );
        let sigreturn_addr = {
            let mm_state = task.mm.state.read();
            mm_state.vdso_base.ptr() as u64 + vdso_sigreturn_offset
        };

        registers.lr = sigreturn_addr;

        SignalStackFrame { context, siginfo_bytes: siginfo.as_siginfo_bytes() }
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
        SigSet(value.sig[0])
    }
}

impl From<SigSet> for sigset_t {
    fn from(val: SigSet) -> Self {
        sigset_t { sig: [val.0] }
    }
}

pub fn restore_registers(current_task: &mut CurrentTask, signal_stack_frame: &SignalStackFrame) {
    let uctx = &signal_stack_frame.context.uc_mcontext;
    // `zx_thread_state_general_regs_t` stores the link register separately from the other general
    // purpose registers, but the uapi struct does not. Thus we just need to copy out the first 30
    // values to store in `r`, and then we read `lr` separately.
    const NUM_REGS_WITHOUT_LINK_REGISTER: usize = 30;
    let mut registers = [0; NUM_REGS_WITHOUT_LINK_REGISTER];
    registers.copy_from_slice(&uctx.regs[..NUM_REGS_WITHOUT_LINK_REGISTER]);

    // Restore the register state from before executing the signal handler.
    current_task.registers = zx::sys::zx_thread_state_general_regs_t {
        r: registers,
        lr: uctx.regs[NUM_REGS_WITHOUT_LINK_REGISTER],
        sp: uctx.sp,
        pc: uctx.pc,
        cpsr: uctx.pstate,
        tpidr: current_task.registers.tpidr,
    }
    .into();
}

pub fn align_stack_pointer(pointer: u64) -> u64 {
    round_up_to_increment(pointer as usize, 16).expect("Failed to round up stack pointer") as u64
}

pub fn update_register_state_for_restart(registers: &mut RegisterState, err: ErrnoCode) {
    if err == ERESTART_RESTARTBLOCK {
        // Update the register containing the syscall number to reference `restart_syscall`.
        registers.r[8] = __NR_restart_syscall as u64;
    }
    // Reset the x0 register value to what it was when the original syscall trap occurred. This
    // needs to be done because x0 may have been overwritten in the syscall dispatch loop.
    registers.r[0] = registers.orig_x0;
}
