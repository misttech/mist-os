// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::arch::registers::RegisterState;
use crate::signals::{SignalInfo, SignalState};
use crate::task::{CurrentTask, Task};
use extended_pstate::ExtendedPstateState;
use starnix_logging::{log_debug, track_stub};
use starnix_uapi::errors::Errno;
use starnix_uapi::math::round_up_to_increment;
use starnix_uapi::signals::{SIGBUS, SIGSEGV};
use starnix_uapi::user_address::UserAddress;
use starnix_uapi::{
    _aarch64_ctx, errno, error, esr_context, fpsimd_context, sigaction_t, sigaltstack, sigcontext,
    siginfo_t, ucontext, ESR_MAGIC, EXTRA_MAGIC, FPSIMD_MAGIC,
};
use zerocopy::{FromBytes, IntoBytes};

/// The size of the red zone.
pub const RED_ZONE_SIZE: u64 = 0;

/// The size of the syscall instruction in bytes.
pub const SYSCALL_INSTRUCTION_SIZE_BYTES: u64 = 4;

// TODO(https://fxbug.dev/380405833): Add in arm and thumb here to enable syscall_restart

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
        extended_pstate: &ExtendedPstateState,
        signal_state: &SignalState,
        siginfo: &SignalInfo,
        _action: sigaction_t,
        _stack_pointer: UserAddress,
    ) -> Result<SignalStackFrame, Errno> {
        let mut regs = registers.r.to_vec();
        // TODO(https://fxbug.dev/380405833) Do we need to capture r[14] here?
        regs.push(registers.lr);

        let fault_address = 0;
        if signal_state.has_queued(SIGBUS) || signal_state.has_queued(SIGSEGV) {
            track_stub!(TODO("https://fxbug.dev/322873483"), "arm64 signal fault address");
        }
        let context = ucontext {
            uc_flags: 0,
            uc_link: Default::default(),
            uc_stack: signal_state
                .alt_stack
                .map(|stack| sigaltstack {
                    ss_sp: stack.ss_sp.into(),
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
                fault_address,
                __reserved: get_sigcontext_data(extended_pstate),
                ..Default::default()
            },
            ..Default::default()
        };

        let vdso_sigreturn_offset = task.kernel().vdso.sigreturn_offset;
        let sigreturn_addr = task.mm().ok_or_else(|| errno!(EINVAL))?.state.read().vdso_base.ptr()
            as u64
            + vdso_sigreturn_offset;
        registers.lr = sigreturn_addr;

        Ok(SignalStackFrame { context, siginfo_bytes: siginfo.as_siginfo_bytes() })
    }

    pub fn as_bytes(&self) -> &[u8; SIG_STACK_SIZE] {
        unsafe { std::mem::transmute(self) }
    }

    pub fn from_bytes(bytes: [u8; SIG_STACK_SIZE]) -> SignalStackFrame {
        unsafe { std::mem::transmute(bytes) }
    }
}

pub fn restore_registers(
    current_task: &mut CurrentTask,
    signal_stack_frame: &SignalStackFrame,
    _stack_pointer: UserAddress,
) -> Result<(), Errno> {
    let uctx = &signal_stack_frame.context.uc_mcontext;
    // `zx_thread_state_general_regs_t` stores the link register separately from the other general
    // purpose registers, but the uapi struct does not. Thus we just need to copy out the first 30
    // values to store in `r`, and then we read `lr` separately.
    const NUM_REGS_WITHOUT_LINK_REGISTER: usize = 30;
    let mut registers = [0; NUM_REGS_WITHOUT_LINK_REGISTER];
    registers.copy_from_slice(&uctx.regs[..NUM_REGS_WITHOUT_LINK_REGISTER]);

    // Restore the register state from before executing the signal handler.
    let mut restored_regs = zx::sys::zx_thread_state_general_regs_t {
        r: registers,
        lr: uctx.regs[NUM_REGS_WITHOUT_LINK_REGISTER],
        sp: uctx.sp,
        pc: uctx.pc,
        cpsr: uctx.pstate,
        tpidr: current_task.thread_state.registers.tpidr,
    };
    // TODO(https://fxbug.dev/380405833) This feels very clunky
    if current_task.thread_state.arch_width.is_arch32() {
        restored_regs.r[13] = restored_regs.sp;
        restored_regs.r[14] = restored_regs.lr;
    }
    current_task.thread_state.registers = restored_regs.into();

    parse_sigcontext_data(&uctx.__reserved, &mut current_task.thread_state.extended_pstate)
}

pub fn align_stack_pointer(pointer: u64) -> u64 {
    round_up_to_increment(pointer, 16).expect("Failed to round up stack pointer")
}

// Size of `sigcontext::__reserved`.
const SIGCONTEXT_RESERVED_DATA_SIZE: usize = 4096;

// Returns the array to be saved in `sigcontext.__reserved`. It contains a sequence of sections
// each identified with a `_aarch64_ctx` header. The end is indicated with both fields in the
// header set to 0.
fn get_sigcontext_data(
    extended_pstate: &ExtendedPstateState,
) -> [u8; SIGCONTEXT_RESERVED_DATA_SIZE] {
    let mut result = [0u8; SIGCONTEXT_RESERVED_DATA_SIZE];

    let fpsimd = fpsimd_context {
        head: _aarch64_ctx {
            magic: FPSIMD_MAGIC,
            size: std::mem::size_of::<fpsimd_context>() as u32,
        },
        fpsr: extended_pstate.get_arm64_fpsr(),
        fpcr: extended_pstate.get_arm64_fpcr(),
        vregs: *extended_pstate.get_arm64_qregs(),
    };
    let _ = fpsimd.write_to_prefix(&mut result);

    // TODO(b/313465152): Save ESR with `esr_context` and `ESR_MAGIC`. The register is read-only,
    // but the signal handler may still need to read it from `sigcontext`.

    result
}

fn parse_sigcontext_data(
    data: &[u8; SIGCONTEXT_RESERVED_DATA_SIZE],
    extended_pstate: &mut ExtendedPstateState,
) -> Result<(), Errno> {
    const FPSIMD_CONTEXT_SIZE: u32 = std::mem::size_of::<fpsimd_context>() as u32;
    const ESR_CONTEXT_SIZE: u32 = std::mem::size_of::<esr_context>() as u32;

    let mut found_fpsimd = false;
    let mut offset: usize = 0;
    loop {
        match _aarch64_ctx::read_from_prefix(&data[offset..]) {
            Ok((_aarch64_ctx { magic: 0, size: 0 }, _)) => break,

            Ok((_aarch64_ctx { magic: FPSIMD_MAGIC, size: FPSIMD_CONTEXT_SIZE }, _))
                if found_fpsimd =>
            {
                log_debug!("Found duplicate `fpsimd_context` in `sigcontext`");
                return error!(EINVAL);
            }

            Ok((_aarch64_ctx { magic: FPSIMD_MAGIC, size: FPSIMD_CONTEXT_SIZE }, _)) => {
                found_fpsimd = true;

                // Set Q registers.
                let (fpsimd, _) = fpsimd_context::read_from_prefix(&data[offset..])
                    .expect("Failed to get fpsimd_context from array");
                extended_pstate.set_arm64_state(&fpsimd.vregs, fpsimd.fpsr, fpsimd.fpcr);

                offset += FPSIMD_CONTEXT_SIZE as usize;
            }

            Ok((_aarch64_ctx { magic: FPSIMD_MAGIC, size }, _)) => {
                log_debug!("Invalid size for `fpsimd_context` in `sigcontext`: {}", size);
                return error!(EINVAL);
            }

            Ok((_aarch64_ctx { magic: ESR_MAGIC, size: ESR_CONTEXT_SIZE }, _)) => {
                // ESR register is read-only so we can skip it.
                offset += ESR_CONTEXT_SIZE as usize;
            }

            Ok((_aarch64_ctx { magic: ESR_MAGIC, size }, _)) => {
                log_debug!("Invalid size for `fpsimd_context` in `sigcontext`: {}", size);
                return error!(EINVAL);
            }

            Ok((_aarch64_ctx { magic: EXTRA_MAGIC, size }, _)) => {
                if size as usize <= std::mem::size_of::<_aarch64_ctx>() {
                    log_debug!("Invalid size for `EXTRA_MAGIC` section in `sigcontext`");
                    return error!(EINVAL);
                }

                track_stub!(TODO("https://fxbug.dev/322873793"), "sigcontext EXTRA_MAGIC");
                offset += ESR_CONTEXT_SIZE as usize;
            }

            Ok((_aarch64_ctx { magic, size }, _)) => {
                log_debug!(
                    "Unrecognized sectionin `sigcontext` (magic: 0x{:x}. size: {})",
                    magic,
                    size
                );
                return error!(EINVAL);
            }

            Err(_) => return error!(EINVAL),
        };
    }

    if !found_fpsimd {
        log_debug!("Couldn't find `fpsimd_context` in `sigcontext`");
        return error!(EINVAL);
    }

    Ok(())
}
