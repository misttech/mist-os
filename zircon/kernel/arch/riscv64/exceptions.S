// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <asm.h>
#include <arch/regs.h>
#include <arch/riscv64.h>
#include <arch/riscv64/mp.h>
#include <lib/syscalls/zx-syscall-numbers.h>
#include <zircon/errors.h>

// RISC-V exception handlers, user space entry and syscall logic.
//
// Currently all exceptions are handled via a single entry point (riscv64_exception_entry).
// At the point of entry, all of the general purpose registers are 'live' in that they
// the state of the cpu at the time the exception fired. In order to tell which
// mode (user or supervisor) the cpu came from, use the sscratch register to either hold
// a pointer to the current kernel stack if coming from user mode or zero in case the cpu
// is currently running supervisor mode. A quick swap of the sp with sscratch will determine
// the mode and two paths can be taken.
//
// In the case of kernel mode, assume the SP is valid at time of exception and simply push
// the register state and enter the kernel. On the way out, restore mostly everything except
// registers that should still be valid, or registers that may have changed (gp).
//
// User mode is a bit more complicated. When entering user mode either for the first time or
// as a result of returning from an exception that originated in user mode, store the top of
// the kernel stack in sscratch and leave behind a few pieces of data to recover the
// supervisor state.
//
// Notably in the top 2 or 3 words (depending on usage of shadow call stack feature) just
// below the kernel SP:
// -8 = saved tp (thread pointer)
// -16 = saved s11 (current cpu pointer)
// -24 = saved gp (current shadow call stack pointer, if feature is enabled)
//
// When entering the kernel from user mode quickly recover this state before saving iframe
// state that would otherwise overlap these saved words (iframe->regs.t4 ... t6)

#define SAVERESTORE_REGS(n) (RISCV64_IFRAME_SIZE - REGOFF(n))
#define SAVERESTORE_TP SAVERESTORE_REGS(1)
#define SAVERESTORE_S11 SAVERESTORE_REGS(2)
#define SAVERESTORE_GP SAVERESTORE_REGS(3)

// Save all of the required registers on the stack, conditional logic based on if
// exception originates in user or kernel mode.
// On entry SP should be valid, and if coming from user mode sscratch will hold a copy
// of the user SP.
.macro save_regs, user
.if \user == 0
    // Save a zero at the bottom of the frame first, to probe for stack overflows.
    // This ensures that if the stack is overrun in the kernel it'll fault immediately here,
    // prior to pushing the stack down. There's no current way to detect this condition aside
    // from the cpu going into an exception loop, but at least the cpu wont walk through memory
    // decrementing the stack pointer and trying again.
    // No real reason to do this if coming from user mode and may actually be slightly
    // dangerous since sscratch currently holds the user sp instead of zero.
    sd     zero, (-RISCV64_IFRAME_SIZE)(sp)
.endif

    // move the SP down by the size of our iframe
    addi   sp, sp, -RISCV64_IFRAME_SIZE
.if \user == 1
    // start by saving tp to the iframe so we free up a register
    sd     tp, RISCV64_IFRAME_OFFSET_TP(sp)

    // use tp to save the user stack pointer which we had previously swapped into
    // sscratch register and then zero sscratch
    csrrw  tp, sscratch, zero
    sd     tp, RISCV64_IFRAME_OFFSET_SP(sp)

    // recover tp from the top word of the stack (we saved it here before)
    ld     tp, SAVERESTORE_TP(sp)

    // Recover s11 (percpu pointer) from the second from top word of the stack.
    sd     s11, RISCV64_IFRAME_OFFSET_S11(sp)
    ld     s11, SAVERESTORE_S11(sp)

    // Always save gp here to keep the logic simpler in the no shadow-call-stack case.
    sd     gp, RISCV64_IFRAME_OFFSET_GP(sp)
#if __has_feature(shadow_call_stack)
    // recover gp (the shadow call stack pointer) from the third from top word of the stack
    ld     gp, SAVERESTORE_GP(sp)
#endif
.endif

    // save the entire integer register set minus the ones we had already handled in the user
    // block above
    sd     ra, RISCV64_IFRAME_OFFSET_RA(sp)   // x1
.if \user == 0
    // TODO-rvbringup: consider saving pre-decremented sp for debugging purposes
    sd     sp, RISCV64_IFRAME_OFFSET_SP(sp)   // x2
    sd     gp, RISCV64_IFRAME_OFFSET_GP(sp)   // x3
#if __has_feature(shadow_call_stack)
    // TODO(fxbug.dev/124357): bump the shadow call stack forward one in case
    // we interrupted the cpu after it has saved a return address but before it
    // has had a chance to increment it
    addi   gp, gp, 8
#endif
    sd     tp, RISCV64_IFRAME_OFFSET_TP(sp)   // x4
.endif
    sd     t0, RISCV64_IFRAME_OFFSET_T0(sp)   // x5
    sd     t1, RISCV64_IFRAME_OFFSET_T1(sp)   // x6
    sd     t2, RISCV64_IFRAME_OFFSET_T2(sp)   // x7
    sd     s0, RISCV64_IFRAME_OFFSET_S0(sp)   // x8
    sd     s1, RISCV64_IFRAME_OFFSET_S1(sp)   // x9
    sd     a0, RISCV64_IFRAME_OFFSET_A0(sp)   // x10
    sd     a1, RISCV64_IFRAME_OFFSET_A1(sp)   // x11
    sd     a2, RISCV64_IFRAME_OFFSET_A2(sp)   // x12
    sd     a3, RISCV64_IFRAME_OFFSET_A3(sp)   // x13
    sd     a4, RISCV64_IFRAME_OFFSET_A4(sp)   // x14
    sd     a5, RISCV64_IFRAME_OFFSET_A5(sp)   // x15
    sd     a6, RISCV64_IFRAME_OFFSET_A6(sp)   // x16
    sd     a7, RISCV64_IFRAME_OFFSET_A7(sp)   // x17
    sd     s2, RISCV64_IFRAME_OFFSET_S2(sp)   // x18
    sd     s3, RISCV64_IFRAME_OFFSET_S3(sp)   // x19
    sd     s4, RISCV64_IFRAME_OFFSET_S4(sp)   // x20
    sd     s5, RISCV64_IFRAME_OFFSET_S5(sp)   // x21
    sd     s6, RISCV64_IFRAME_OFFSET_S6(sp)   // x22
    sd     s7, RISCV64_IFRAME_OFFSET_S7(sp)   // x23
    sd     s8, RISCV64_IFRAME_OFFSET_S8(sp)   // x24
    sd     s9, RISCV64_IFRAME_OFFSET_S9(sp)   // x25
    sd     s10, RISCV64_IFRAME_OFFSET_S10(sp) // x26
    // s11 (x27) is the percpu pointer, already saved above.
    sd     t3, RISCV64_IFRAME_OFFSET_T3(sp)   // x28
    sd     t4, RISCV64_IFRAME_OFFSET_T4(sp)   // x29
    sd     t5, RISCV64_IFRAME_OFFSET_T5(sp)   // x30
    sd     t6, RISCV64_IFRAME_OFFSET_T6(sp)   // x31

    csrr   t0, sepc
    sd     t0, RISCV64_IFRAME_OFFSET_PC(sp)
    csrr   t0, sstatus
    sd     t0, RISCV64_IFRAME_OFFSET_STATUS(sp)

    csrr   a0, scause
    mv     a1, sp
    // args are set up for a call into riscv64_exception_handler()
    // a0 = scause
    // a1 = sp
.endm

.macro restore_regs, user
    // put everything back
    ld     t0, RISCV64_IFRAME_OFFSET_STATUS(sp)
    csrw   sstatus, t0
    ld     t0, RISCV64_IFRAME_OFFSET_PC(sp)
    csrw   sepc, t0

    // restore most of the register state except the ones that need to be
    // specially handled in the user path below
    ld     ra, RISCV64_IFRAME_OFFSET_RA(sp)   // x1
    // the following registers will be handled in the user path below
    // or in a special way for kernel mode:
    // sp (x2) - bumped at the end of the macro
    // gp (x3) - restored here in kernel mode (shadow-call-stack pointer)
    // tp (x4) - restored here in kernel mode (thread pointer)
    // s11 (x27) - not restored in kernel mode (percpu pointer)
.if \user == 0
   // These two will be handled separately in the user path.
    ld     gp, RISCV64_IFRAME_OFFSET_GP(sp)   // x3
    ld     tp, RISCV64_IFRAME_OFFSET_TP(sp)   // x4
.endif
    ld     t0, RISCV64_IFRAME_OFFSET_T0(sp)   // x5
    ld     t1, RISCV64_IFRAME_OFFSET_T1(sp)   // x6
    ld     t2, RISCV64_IFRAME_OFFSET_T2(sp)   // x7
    ld     s0, RISCV64_IFRAME_OFFSET_S0(sp)   // x8
    ld     s1, RISCV64_IFRAME_OFFSET_S1(sp)   // x9
    ld     a0, RISCV64_IFRAME_OFFSET_A0(sp)   // x10
    ld     a1, RISCV64_IFRAME_OFFSET_A1(sp)   // x11
    ld     a2, RISCV64_IFRAME_OFFSET_A2(sp)   // x12
    ld     a3, RISCV64_IFRAME_OFFSET_A3(sp)   // x13
    ld     a4, RISCV64_IFRAME_OFFSET_A4(sp)   // x14
    ld     a5, RISCV64_IFRAME_OFFSET_A5(sp)   // x15
    ld     a6, RISCV64_IFRAME_OFFSET_A6(sp)   // x16
    ld     a7, RISCV64_IFRAME_OFFSET_A7(sp)   // x17
    ld     s2, RISCV64_IFRAME_OFFSET_S2(sp)   // x18
    ld     s3, RISCV64_IFRAME_OFFSET_S3(sp)   // x19
    ld     s4, RISCV64_IFRAME_OFFSET_S4(sp)   // x20
    ld     s5, RISCV64_IFRAME_OFFSET_S5(sp)   // x21
    ld     s6, RISCV64_IFRAME_OFFSET_S6(sp)   // x22
    ld     s7, RISCV64_IFRAME_OFFSET_S7(sp)   // x23
    ld     s8, RISCV64_IFRAME_OFFSET_S8(sp)   // x24
    ld     s9, RISCV64_IFRAME_OFFSET_S9(sp)   // x25
    ld     s10, RISCV64_IFRAME_OFFSET_S10(sp) // x26
    // s11 (x27) will be handled separately in the user path.
    ld     t3, RISCV64_IFRAME_OFFSET_T3(sp)   // x28
    ld     t4, RISCV64_IFRAME_OFFSET_T4(sp)   // x29
    ld     t5, RISCV64_IFRAME_OFFSET_T5(sp)   // x30
    ld     t6, RISCV64_IFRAME_OFFSET_T6(sp)   // x31

.if \user == 1
    // Before we run out of registers, save tp and s11 (percpu pointer) to the
    // top of the kernel stack and put the kernel stack in the scratch
    // register.  The registers at the tail end of the iframe have already been
    // loaded above, so we can use that part of the stack to transfer the last
    // few special registers.
#if (SAVERESTORE_TP <= RISCV64_IFRAME_OFFSET_S11 || \
     SAVERESTORE_S11 <= RISCV64_IFRAME_OFFSET_S11 || \
     SAVERESTORE_GP <= RISCV64_IFRAME_OFFSET_S11)
#error "iframe regs clobbered by regs saved at top of kernel stack while in user"
#endif

    sd     tp, SAVERESTORE_TP(sp)
    sd     s11, SAVERESTORE_S11(sp)
#if __has_feature(shadow_call_stack)
    // save the shadow call pointer in the third from top slot on the kernel stack
    sd     gp, SAVERESTORE_GP(sp)
#endif

    // save the top of the kernel stack into the sscratch register
    add    s11, sp, RISCV64_IFRAME_SIZE
    csrw   sscratch, s11

    // recover the last of the user registers from the iframe, stack pointer last
    ld     s11, RISCV64_IFRAME_OFFSET_S11(sp)
    ld     tp, RISCV64_IFRAME_OFFSET_TP(sp)
    ld     gp, RISCV64_IFRAME_OFFSET_GP(sp)
    ld     sp, RISCV64_IFRAME_OFFSET_SP(sp)

    // at this point we have all of the user registers loaded
.else
    // bump the stack to the previous value when returning to kernel mode
    addi   sp, sp, RISCV64_IFRAME_SIZE
.endif
.endm

// top level exception handler for riscv in non vectored mode
.balign 4
FUNCTION(riscv64_exception_entry)
    // Check to see if we came from user space.
    // If sscratch is not zero, it holds the kernel stack pointer and we will recover
    // it and other critical registers before continuing.
    // If sscratch is zero, we're interrupting kernel code so we can continue with
    // an assumption that SP and other critical registers are okay.
    csrrw   sp, sscratch, sp
    bnez    sp, user_exception_entry

    // put the stack back
    csrrw   sp, sscratch, sp

    // fall through...
END_FUNCTION(riscv64_exception_entry)

LOCAL_FUNCTION(kernel_exception_entry)
    // we came from kernel space so tp and s11 are okay
    save_regs 0

    // Clear the SUM bit in case we interrupted a section of the kernel with it cleared.
    // TODO-rvbringup: see if this can be moved prior to pushing state on the kernel stack
    li      t0, RISCV64_CSR_SSTATUS_SUM
    csrc    sstatus, t0

    // extern "C" void riscv64_exception_handler(long cause, struct iframe_t *frame);
    call    riscv64_exception_handler
    restore_regs 0

    sret
END_FUNCTION(kernel_exception_entry)

LOCAL_FUNCTION(user_exception_entry)
    // we came from user space, assume gp and tp have been trashed
    save_regs 1

    // extern "C" void riscv64_exception_handler(long cause, struct iframe_t *frame);
    call    riscv64_exception_handler
    restore_regs 1

    sret
END_FUNCTION(user_exception_entry)

// void riscv64_uspace_entry(iframe_t* iframe, void *sp, void *shadow_call_base) __NO_RETURN;
FUNCTION(riscv64_uspace_entry)
    // a0 == frame
    // a1 == top of kernel stack
    // a2 == shadow call base

#define SAVERESTORE_NO_IFRAME(reg) (SAVERESTORE_##reg - RISCV64_IFRAME_SIZE)

    // Save a few things at the top of the stack to recover on exception entry
    // Note: must match logic in the frame save/restore macro above
    sd     tp, SAVERESTORE_NO_IFRAME(TP)(a1) // thread pointer
    sd     s11, SAVERESTORE_NO_IFRAME(S11)(a1) // current cpu pointer
#if __has_feature(shadow_call_stack)
    sd     a2, SAVERESTORE_NO_IFRAME(GP)(a1) // shadow call stack pointer
#endif

    // Save the kernel stack pointer in the sscratch register
    csrw   sscratch, a1

    // Load the iframe
    ld     t0, RISCV64_IFRAME_OFFSET_PC(a0)
    csrw   sepc, t0
    ld     t0, RISCV64_IFRAME_OFFSET_STATUS(a0)
    csrw   sstatus, t0

    ld     ra, RISCV64_IFRAME_OFFSET_RA(a0)   // x1
    ld     sp, RISCV64_IFRAME_OFFSET_SP(a0)   // x2
    ld     gp, RISCV64_IFRAME_OFFSET_GP(a0)   // x3
    ld     tp, RISCV64_IFRAME_OFFSET_TP(a0)   // x4
    ld     t0, RISCV64_IFRAME_OFFSET_T0(a0)   // x5
    ld     t1, RISCV64_IFRAME_OFFSET_T1(a0)   // x6
    ld     t2, RISCV64_IFRAME_OFFSET_T2(a0)   // x7
    ld     s0, RISCV64_IFRAME_OFFSET_S0(a0)   // x8
    ld     s1, RISCV64_IFRAME_OFFSET_S1(a0)   // x9
    // a0 (x10) restored below
    ld     a1, RISCV64_IFRAME_OFFSET_A1(a0)   // x11
    ld     a2, RISCV64_IFRAME_OFFSET_A2(a0)   // x12
    ld     a3, RISCV64_IFRAME_OFFSET_A3(a0)   // x13
    ld     a4, RISCV64_IFRAME_OFFSET_A4(a0)   // x14
    ld     a5, RISCV64_IFRAME_OFFSET_A5(a0)   // x15
    ld     a6, RISCV64_IFRAME_OFFSET_A6(a0)   // x16
    ld     a7, RISCV64_IFRAME_OFFSET_A7(a0)   // x17
    ld     s2, RISCV64_IFRAME_OFFSET_S2(a0)   // x18
    ld     s3, RISCV64_IFRAME_OFFSET_S3(a0)   // x19
    ld     s4, RISCV64_IFRAME_OFFSET_S4(a0)   // x20
    ld     s5, RISCV64_IFRAME_OFFSET_S5(a0)   // x21
    ld     s6, RISCV64_IFRAME_OFFSET_S6(a0)   // x22
    ld     s7, RISCV64_IFRAME_OFFSET_S7(a0)   // x23
    ld     s8, RISCV64_IFRAME_OFFSET_S8(a0)   // x24
    ld     s9, RISCV64_IFRAME_OFFSET_S9(a0)   // x25
    ld     s10, RISCV64_IFRAME_OFFSET_S10(a0) // x26
    ld     s11, RISCV64_IFRAME_OFFSET_S11(a0) // x27
    ld     t3, RISCV64_IFRAME_OFFSET_T3(a0)   // x28
    ld     t4, RISCV64_IFRAME_OFFSET_T4(a0)   // x29
    ld     t5, RISCV64_IFRAME_OFFSET_T5(a0)   // x30
    ld     t6, RISCV64_IFRAME_OFFSET_T6(a0)   // x31

    // Load a0 last since it points to the iframe_t
    ld     a0, RISCV64_IFRAME_OFFSET_A0(a0)

    // Return to user space
    sret
END_FUNCTION(riscv64_uspace_entry)

#ifdef KERNEL_NO_USERABI
// If we're not building user abi there are no wrapper functions or
// proper unknown_syscall function for the dispatcher to dispatch to,
// so define a stub routine so the code at least links.
FUNCTION(unknown_syscall)
    lla    a0, unknown_syscall_panic_string
    j      panic
END_FUNCTION(unknown_syscall)

.data
LOCAL_DATA(unknown_syscall_panic_string)
.ascii "no syscall hooks to call!"
END_DATA(unknown_syscall_panic_string)
.text
#endif // KERNEL_NO_USERABI

// void riscv64_syscall_dispatcher(iframe_t* iframe);
// Registers in the iframe are parsed using the following convention:
//
//   a0-a7 - contains syscall arguments
//   t0    - contains syscall_num
//   pc    - contains the syscall instruction address
//
FUNCTION(riscv64_syscall_dispatcher)
    addi   sp, sp, -16
    // Store ra to the +8 slot, leaving 0(sp) free for potential syscall
    // table use below.
    sd     ra, 8(sp)

    // Check if we're issuing a syscall from restricted mode.
    // We do this after storing RA to make sure we don't lose the call chain.
    lw t3, PERCPU_IN_RESTRICTED_MODE(s11)
    bnez t3, .Lrestricted_syscall

    ld     t1, RISCV64_IFRAME_OFFSET_PC(a0)
    ld     t0, RISCV64_IFRAME_OFFSET_T0(a0)
    // Load a0 last since it points to the iframe.
    ld     a1, RISCV64_IFRAME_OFFSET_A1(a0)
    ld     a2, RISCV64_IFRAME_OFFSET_A2(a0)
    ld     a3, RISCV64_IFRAME_OFFSET_A3(a0)
    ld     a4, RISCV64_IFRAME_OFFSET_A4(a0)
    ld     a5, RISCV64_IFRAME_OFFSET_A5(a0)
    ld     a6, RISCV64_IFRAME_OFFSET_A6(a0)
    ld     a7, RISCV64_IFRAME_OFFSET_A7(a0)
    ld     a0, RISCV64_IFRAME_OFFSET_A0(a0)

    // Verify syscall number and call the unknown handler if bad.
    li     t2, ZX_SYS_COUNT
    bgeu   t0, t2, .Lunknown_syscall

    // Jump to the right syscall wrapper C++ function via assembly
    // trampolines.  The trampolines expect the user PC in t1 and the user's
    // arguments in the a0..a7 registers; they may write to the word at 0(sp).
    // Each trampoline marshalls some arguments and tail calls the routine
    // which causes the wrapper to return to .Lpost_syscall.  The syscall
    // table is an array of offsets to trampoline entry points defined by the
    // `syscall_dispatcher` macro (see below).
    lla    t2, syscall_dispatcher_table
    sll    t0, t0, 2   // Scale to 4 bytes per entry.
    add    t0, t2, t0  // Index into the table.
    lw     t2, (t0)    // Load 32-bit offset and sign-extend it.
    add    t2, t2, t0  // Materialize the full trampoline PC.
    jalr   t2

.Lpost_syscall:
    ld     ra, 8(sp)
    addi   sp, sp, 16
    ret

.Lunknown_syscall:
    mv     a0, t0 // move the syscall number into the 0 arg slot
    mv     a1, t1 // pc into arg 1
    call   unknown_syscall
    j      .Lpost_syscall

.Lrestricted_syscall:
    call syscall_from_restricted // This does not return.
    unimp

END_FUNCTION(riscv64_syscall_dispatcher)

// This establishes the label at the beginning of the section,
// before any syscall_dispatcher macro invocations add to it.
.pushsection .rodata.syscall_dispatcher_table, "a", %progbits
.balign 4
syscall_dispatcher_table:  // Use a non-.L label so it appears in disassembly.

//
// Syscall args are in a0-a7 already.
// pc is in t1 and needs to go in the next available register,
// or the stack if the regs are full.
//
.macro syscall_dispatcher nargs, syscall
    // Each trampoline goes into its own little .text section that the
    // linker can move around arbitrarily. Relaxation can change the size
    // of the code sequence at link time: `tail`, `call`, and `j` always
    // expand to a multi-instruction sequence that might get relaxed down
    // to a single instruction.
    .pushsection .text.syscall_dispatcher.\syscall, "ax", %progbits
    syscall_dispatcher.\syscall:  // This label will appear in disassembly.
.if \nargs == 8
    // store the pc in the slot we already left on the stack
    sd     t1, (sp)
.else
    mv     a\nargs, t1
.endif
#ifndef KERNEL_NO_USERABI
    tail   wrapper_\syscall
#else
    tail   unknown_syscall
#endif
    .popsection

    // Each trampoline's address goes into the table so it can be indexed as a
    // flat array of fixed-size pointers.  To keep the table smaller, rather
    // than full pointers, store 32-bit offsets from the location of the
    // pointer itself.  All the macro invocations are within the table's
    // section, so after the .popsection this adds the next entry to the end.
    .int syscall_dispatcher.\syscall - .
.endm

// One of these macros is invoked by kernel.inc for each syscall.
// These are the direct kernel entry points.
#define KERNEL_SYSCALL(name, type, attrs, nargs, arglist, prototype) \
  syscall_dispatcher nargs, name
#define INTERNAL_SYSCALL(...) KERNEL_SYSCALL(__VA_ARGS__)
#define BLOCKING_SYSCALL(...) KERNEL_SYSCALL(__VA_ARGS__)
// These don't have kernel entry points.
#define VDSO_SYSCALL(...)

#include <lib/syscalls/kernel.inc>

#undef KERNEL_SYSCALL
#undef INTERNAL_SYSCALL
#undef BLOCKING_SYSCALL
#undef VDSO_SYSCALL

.size syscall_dispatcher_table, . - syscall_dispatcher_table
.popsection
