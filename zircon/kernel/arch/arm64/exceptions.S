// Copyright 2016 The Fuchsia Authors
// Copyright (c) 2014 Travis Geiselbrecht
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <arch/arch_thread.h>
#include <arch/arm64.h>
#include <arch/arm64/mp.h>
#include <arch/regs.h>
#include <lib/arch/arm64/exception-asm.h>
#include <lib/arch/asm.h>
#include <lib/syscalls/zx-syscall-numbers.h>
#include <lib/userabi/vdso-arm64.h>
#include <zircon/compiler.h>

.text

// Spill two registers into the iframe.  If the optional argument is !
// then it's setting up the frame by adjusting the SP by \offset.
.macro iframe.stp reg1, reg2, offset, wb=
  stp \reg1, \reg2, [sp, \offset]\wb
  .cfi.iframe.stp \reg1, \reg2, \offset, \wb
.endm
.macro .cfi.iframe.stp reg1, reg2, offset, wb=
  .if (\offset) % 16
    .error "iframe.stp \offset is not optimally aligned"
  .endif
  .ifc !,\wb
    .cfi_adjust_cfa_offset -(\offset)
  .endif
  // .cfi_rel_offset expresses an offset relative to SP, which is at the base
  // of the iframe.  The assembler will adjust this for offsets relative to the
  // CFA, which is at the end of the iframe.
  .cfi_rel_offset \reg1, \offset + 0
  .cfi_rel_offset \reg2, \offset + 8
.endm

// Reload two registers from the iframe.
.macro iframe.ldp reg1, reg2, offset
  .if (\offset) % 16
    .error "iframe.ldp \offset is not optimally aligned"
  .endif
  ldp \reg1, \reg2, [sp, \offset]
  .ifnc \reg1,xzr
    .cfi_same_value \reg1
  .endif
  .cfi_same_value \reg2
.endm

// Invoke one of those for each x<n> pair, optionally skipping x0, x1
// and optionally replacing x20 with another register (i.e. xzr).
.macro iframe.foreach.x op, x0=x0, x20=x20
  .ifc "x0","\x0"
    \op x0, x1, ARM64_IFRAME_OFFSET_R
  .else
    .ifnc "skip","\x0"
      .error "iframe.foreach.x optional x0= argument must be skip not x0=\x0"
    .endif
  .endif
  \op x2, x3, ARM64_IFRAME_OFFSET_R + (2 * 8)
  \op x4, x5, ARM64_IFRAME_OFFSET_R + (4 * 8)
  \op x6, x7, ARM64_IFRAME_OFFSET_R + (6 * 8)
  \op x8, x9, ARM64_IFRAME_OFFSET_R + (8 * 8)
  \op x10, x11, ARM64_IFRAME_OFFSET_R + (10 * 8)
  \op x12, x13, ARM64_IFRAME_OFFSET_R + (12 * 8)
  \op x14, x15, ARM64_IFRAME_OFFSET_R + (14 * 8)
  \op x16, x17, ARM64_IFRAME_OFFSET_R + (16 * 8)
  \op x18, x19, ARM64_IFRAME_OFFSET_R + (18 * 8)
  \op \x20, x21, ARM64_IFRAME_OFFSET_R + (20 * 8)
  \op x22, x23, ARM64_IFRAME_OFFSET_R + (22 * 8)
  \op x24, x25, ARM64_IFRAME_OFFSET_R + (24 * 8)
  \op x26, x27, ARM64_IFRAME_OFFSET_R + (26 * 8)
  \op x28, x29, ARM64_IFRAME_OFFSET_R + (28 * 8)
.endm

// Save the full register state on the stack.
// NOTE: Must stay in sync with the iframe_t definition in arch/regs.h
.macro iframe.exc.save, from_lower_el_64
  .ifnb \from_lower_el_64
    .ifnc lower_el_64,\from_lower_el_64
      .error "iframe.exc.save argument can only be `lower_el_64`"
    .endif
  .endif

  // The first push decrements the SP as a side effect. In the case of a stack
  // overflow will result in an immediate fault, instead of 'walking the stack'
  // downwards in an exception loop if the sp were decremented first.
  iframe.stp x0, x1, -ARM64_IFRAME_SIZE, !

  // Save x2..x29.
  iframe.foreach.x iframe.stp, x0=skip

  // Stay away from x0-x7, since they may be holding syscall arguments.
  mrs x9, sp_el0
  .cfi_register sp, x9

  // x10 (containing elr_el1, i.e. user's PC) is used in the syscall handler.
  mrs x10, elr_el1
  .cfi_register DW_REGNO_PC, x10

  // There is no DWARF number for SPSR.
  mrs x11, spsr_el1

  // Save x30 (aka LR) and SP.
  iframe.stp lr, x9, ARM64_IFRAME_OFFSET_LR

  // Save ELR_ELx, i.e. the interrupted PC, and SPSR.
  // There is no DWARF number for SPSR.
  stp x10, x11, [sp, ARM64_IFRAME_OFFSET_ELR]
  .cfi_rel_offset DW_REGNO_PC, ARM64_IFRAME_OFFSET_ELR

  .ifnb \from_lower_el_64
    // Save the user's mdscr value in the arch_thread struct. Because this
    // value is saved in the struct instead of on the stack, we must ensure
    // that we don't overwrite a previously saved value when we re-enter
    // the kernel. Only save/restore on user/kernel transitions.
    mrs x11, tpidr_el1
    mrs x12, mdscr_el1
    str x12, [x11, CURRENT_MDSCR_OFFSET]
  .endif
.endm

// Recreate the CFI state at the end of iframe.exc.save.
.macro .cfi.iframe.exc.save
  .cfi_def_cfa sp, ARM64_IFRAME_SIZE
  iframe.foreach.x .cfi.iframe.stp
  .cfi_rel_offset DW_REGNO_PC, ARM64_IFRAME_OFFSET_ELR
.endm

// Like `.function name`: here to `.end_function name` is the body of
// a function that's jumped to after iframe.exc.save sets up the iframe
// on the stack.  Recreate the right CFI conditions.
.macro .iframe.function name
  .function \name, local, cfi=custom, nosection=nosection
  .cfi.iframe.exc.save
.endm

// Reverse iframe.exc.save above, restoring the full register state from
// the iframe on the stack.
.macro iframe.exc.restore, from_lower_el_64
  .ifnb \from_lower_el_64
    .ifnc lower_el_64,\from_lower_el_64
      .error "iframe.exc.restore argument can only be `lower_el_64`"
    .endif
  .endif

  .ifnb \from_lower_el_64
    // See comment about mdscr in regsave macro for why we only restore if
    // we came from usermode.
    mrs x11, tpidr_el1
    ldr x12, [x11, CURRENT_MDSCR_OFFSET]
    msr mdscr_el1, x12
  .endif

  // Restore the rest of the registers from the stack.

  // First, x30 (aka LR) and use x9 for the interrupted SP.
  // These don't use iframe.ldp because the CFI for SP shouldn't
  // be .cfi_same_value until later.
  ldp lr, x9, [sp, #ARM64_IFRAME_OFFSET_LR]
  .cfi_same_value lr
  .cfi_register sp, x9

  // Next, use x10 and x11 for the interrupted PC and SPSR.
  ldp x10, x11, [sp, ARM64_IFRAME_OFFSET_ELR]
  .cfi_register DW_REGNO_PC, x10
  // There is no DWARF number for SPSR.

  // This doesn't update CFI for SP because SP is still SP_EL1 here.
  msr sp_el0, x9

  // The interrupted PC will be restored from ELR_EL1 by eret.
  msr elr_el1, x10
  .cfi_register DW_REGNO_PC, DW_REGNO_ELR_ELx

  // There is no DWARF number for SPSR.
  msr spsr_el1, x11

  // We're about to restore x9, so the interrupted SP will no longer be
  // accessible.  Actually, it's in SP_EL0, but there's no DWARF register
  // number for that to inform the debugger where to find it.
  .cfi_undefined sp

  // Reload x0..x29 from the iframe.
  .ifnb \from_lower_el_64
    iframe.foreach.x iframe.ldp
  .else
    // If restoring from EL1 -> EL1, leave x20 alone since it's holding the
    // current per cpu pointer. It may have changed since we originally
    // took the exception if we had been rescheduled to another cpu.
    iframe.foreach.x iframe.ldp, x20=xzr
  .endif

  // Finally, pop the iframe.
  .add.sp ARM64_IFRAME_SIZE
.endm


// All normal C code in the kernel expects the invariants that the fixed
// registers assigned to the percpu_ptr and the shadow-call-stack pointer have
// the correct values for the current CPU and kernel thread.  When an exception
// happens in the kernel, only percpu_ptr needs to be reloaded. (In fact, it
// would be disastrous to reload the shadow-call-stack pointer because the
// correct value to reflect the interrupted thread's kernel call stack exists
// only in the register!) But when an exception happens in a lower EL
// (i.e. user mode), these registers must be reloaded from the struct
// arch_thread accessible via TPIDR_EL1 before reaching any C functions.
.macro restore_fixed_regs temp
  mrs \temp, tpidr_el1
#if __has_feature(shadow_call_stack)
# if CURRENT_SCSP_OFFSET != CURRENT_PERCPU_PTR_OFFSET + 8
#  error "shadow_call_sp must follow current_percpu_ptr in struct arch_thread"
# endif
  ldp percpu_ptr, shadow_call_sp, [\temp, #CURRENT_PERCPU_PTR_OFFSET]
#else
  ldr percpu_ptr, [\temp, #CURRENT_PERCPU_PTR_OFFSET]
#endif
.endm

// Lighter version of restore_fixed_regs for EL1-to-EL1 exceptions. Strictly
// speaking not required, but in case of a firmware call via SMC or HVC
// accidentally trashing the register, preserve this fixed register by
// reloading it from the current thread pointer.
.macro restore_percpu_ptr temp
  mrs \temp, tpidr_el1
  ldr percpu_ptr, [\temp, #CURRENT_PERCPU_PTR_OFFSET]
.endm

// The shadow-call-stack pointer (x18) is saved/restored in struct arch_thread
// on context switch.  On entry from a lower EL (i.e. user mode), it gets
// reloaded from there via the restore_fixed_regs macro above.  So when
// returning to user mode, we must make sure to write back the current value
// (which should always be the base, since returning to user should be the base
// of the call stack) so that the next kernel entry reloads that instead of
// whatever was current last time this thread context-switched out.
.macro save_shadow_call_sp temp
#if __has_feature(shadow_call_stack)
  mrs \temp, tpidr_el1
  str shadow_call_sp, [\temp, #CURRENT_SCSP_OFFSET]
#endif
.endm

// Unhandled exception or irq. Save the full state and pass the which value
// through to the inner routine.
.macro iframe.invalid_exception, name, which
  // Save state in the iframe and restore kernel invariants.
  iframe.exc.save
  restore_fixed_regs x9

  // Call into the C++ code with the iframe and vector number as arguments.
  mov x0, sp
  mov x1, #\which
  bl arm64_invalid_exception

  // Spin rather than cascade exceptions if that ever returns.
  // Definitely don't restore state from the iframe after this!
  // The C++ code should have done a panic and not returned at all.
  b .
.endm

// Asynchronous exceptions (IRQ, SError).  Call into C++ with the iframe
// and a flag indicating user (EL0) vs kernel (EL1), and then restore from
// the iframe when C++ returns.
.macro iframe.async_exception name, call, from_lower_el_64
  .vbar_function \name

    // Save state in the iframe and restore kernel invariants.
    // x1 is the flag indicating a user-mode (EL0) exception.
    iframe.exc.save \from_lower_el_64
    .ifnb \from_lower_el_64
      restore_fixed_regs x9
      mov x1, #ARM64_EXCEPTION_FLAG_LOWER_EL
    .else
      restore_percpu_ptr x9
      mov x1, #0
    .endif

    // Call into the C++ code with the iframe and flags as arguments.
    mov x0, sp
    bl \call

    // Use the common return path to restore state and eret.
    .ifnb \from_lower_el_64
      b arm64_exc_shared_restore_lower_el
    .else
      b arm64_exc_shared_restore
    .endif

  .end_vbar_function
.endm

// TODO: find the appropriate place to reenable FIQs here when they're needed.

// Synchronous exceptions, i.e. everything expected that's not external.
.macro iframe.sync_exception name, from_lower_el_64
  .vbar_function \name

    // Save state in the iframe.
    iframe.exc.save \from_lower_el_64

    // Collect the Exception Syndrome Register that explains what this is.
    mrs x9, esr_el1

    .ifnb \from_lower_el_64
      // Restore kernel invariants.
      restore_fixed_regs x11

      // If this is a syscall, x0-x7 contain args and x16 contains syscall num.
      // x10 contains ELR_EL1.
      lsr x11, x9, #26              // Shift ESR right 26 bits to get EC.
      cmp x11, #0x15                // Check for 64-bit syscall...
      beq arm64_syscall_dispatcher  // ...and jump to syscall handler.
    .else
      // Restore kernel invariants just in case of clobberation.
      restore_percpu_ptr x11
    .endif

    // Prepare the default sync_exception args.
    mov w2, w9 // Third argument is ESR.
    .ifnb \from_lower_el_64
      // We've just run out of space to fit in the 0x80 bytes of the sync
      // exception vector.  Branch to another block of code later in the file
      // that will finish getting ready and call arm64_sync_exception.
      b arm64_sync_exception_lower_el
    .else
      // Call into the C++ code with iframe and flags (zero) as arguments.
      mov x0, sp
      mov x1, #0
      bl arm64_sync_exception
      b arm64_exc_shared_restore
    .endif

  .end_vbar_function
.endm


// Define the vector table.  See <lib/arch/arm64/exception-asm.h> for a
// more complete explanation.  Inside `.vbar_table` ... `.end_vbar_table`,
// each use of `.vbar_function` uses the exact name that's the name given
// to `.vbar_table` plus the exact suffix that describes the particular
// vector entry point in the terms they appear in the ARM manual under
// "Exception Vectors".  The `.vbar_function` entries must appear in the
// correct order, and an assembly-time error will be diagnosed if any is
// out of order (or too long).  Here it's the `iframe.*_exception` macros
// defined just above that use `.vbar_function`, but the full name of the
// `.vbar_function` entry point they'll define is spelled out in the macro
// invocations below and those symbol names will be visible in disassembly.
//
// iframe.invalid_exception provides the body for entry points with no
// individual `.vbar_function` below.  These are automatically filled in as
// needed when a vector that must be ordered later than an omitted one is
// defined, and at the end of the table.
.vbar_table arm64_el1_exception, global, iframe.invalid_exception

iframe.sync_exception arm64_el1_exception_sync_current_sp_elx

iframe.async_exception arm64_el1_exception_irq_current_sp_elx, arm64_irq

iframe.async_exception arm64_el1_exception_serror_current_sp_elx, \
                       arm64_serror_exception

iframe.sync_exception arm64_el1_exception_sync_lower_a64, lower_el_64

iframe.async_exception arm64_el1_exception_irq_lower_a64, \
                       arm64_irq, lower_el_64

iframe.async_exception arm64_el1_exception_serror_lower_a64, \
                       arm64_serror_exception, lower_el_64

.end_vbar_table


// Start the rest of the code on its own cache line.
.balign 64


// All the iframe.sync_exception ..., lower_el_64 cases jump here.
// The iframe is all set up on the stack.
.iframe.function arm64_sync_exception_lower_el
  // Call into C++ with the iframe and flags as arguments.
  mov x0, sp
  mov x1, #ARM64_EXCEPTION_FLAG_LOWER_EL
  bl arm64_sync_exception

  // Jump to the shared path to restore from the iframe.
  b arm64_exc_shared_restore_lower_el
.end_function


// The kernel C++ code is done and ready to return to user mode.  The iframe is
// still on the stack as set up in iframe.exc.save, modified in place by C++.
// Other registers have been clobbered following the normal C conventions.
.iframe.function arm64_exc_shared_restore_lower_el

  // Save kernel shadow_call_sp; see the macro definition.
  save_shadow_call_sp x9

  // Restore registers and resume in user mode.
  iframe.exc.restore lower_el_64
  eret

  // Prevent speculation through ERET.
  speculation_postfence

.end_function


// The kernel C++ code is done and ready to return to interrupted kernel code.
// The iframe is still on the stack as set up in iframe.exc.save, modified in
// place by C++.  Other registers have been clobbered following the normal C
// conventions.
.iframe.function arm64_exc_shared_restore

  // Restore registers and resume, still in kernel mode.
  iframe.exc.restore
  eret

  // Prevent speculation through ERET.
  speculation_postfence

.end_function


// Emit a small trampoline to branch to the wrapper routine for the syscall.
// Syscall args are in x0-x7 already.  User PC is in x10 and needs to go in the
// next available argument register, or the stack if out of argument registers.
// The trampolines are a tightly-packed sequence for each syscall in order of
// its number.  Each is exactly 16 bytes long (up to 4 instructions).
.macro syscall_dispatcher nargs, syscall
  .balign 16
  .Lsyscall_dispatcher.\syscall\():
  .if \nargs == 8
    stp x10, xzr, [sp, #-16]!  // Store an extra word to keep SP aligned.
    .cfi_adjust_cfa_offset 16
    bl wrapper_\syscall
    .add.sp 16
  .else
    mov x\nargs, x10
    bl wrapper_\syscall
  .endif
  b .Lpost_syscall
  .if (. - .Lsyscall_dispatcher.\syscall\()) > 16
    .error "syscall_dispatcher \nargs, \syscall code too long!"
  .endif
.endm

// Clear unused GPRs to constrain speculative execution with user-controlled
// values.  While this is not a mitigation for any single vulnerability, this
// does make constructing attacks more difficult - speculatively executed
// gadgets will execute with most register state clear.
// TODO(https://fxbug.dev/42076199): Add to synchronous exception path as well.
.macro speculation_clear_gprs
  // x0 - x7 hold syscall arguments from user.
  // x9 holds ESR_EL1, which should be constant describing the SVC exception.
  // x10 holds ELR_EL1 (user PC).
  // x16 holds syscall number from user.
  // x18 holds kernel shadow_call_sp.
  // x20 holds kernel percpu_ptr
  .irp reg,x8,x11,x12,x13,x14,x15,x17,x19,x21,x22,x23,x24,x25,x26,x27,x28,x29,x30
    mov \reg, xzr
  .endr
.endm

//
// Expected state prior to arm64_syscall_dispatcher branch:
//
// percpu and shadow call stack registers have been restored
//
// x0-x7 - contains syscall arguments
// x9    - contains esr_el1 (not used)
// x10   - contains elr_el1
// x16   - contains syscall_num
// sp    - points to base of iframe
//
// Expected state prior to branching to syscall_dispatcher macro:
//
// x0-x7  - contains syscall arguments
// x10    - contains userspace pc
//
.iframe.function arm64_syscall_dispatcher

  speculation_clear_gprs

  // Check if we're issuing a syscall from restricted mode.
  ldr w12, [percpu_ptr, #PERCPU_IN_RESTRICTED_MODE]
  cbnz w12, .Lrestricted_syscall

  // Verify syscall number and call the unknown handler if bad.
  cmp x16, #ZX_SYS_COUNT
  bhs .Lunknown_syscall

  // Spectre V1: If syscall number >= ZX_SYS_COUNT, replace it with zero. The
  // branch/test above means this can only occur in wrong-path speculative
  // executions.
  csel x16, xzr, x16, hs
  csdb

  // Jump to the right syscall wrapper. The syscall table is an
  // array of 16 byte aligned routines for each syscall. Each routine
  // marshalls some arguments, bls to the routine, and then branches
  // back to .Lpost_syscall (see syscall_dispatcher macro above).
  adr x12, .Lsyscall_table
  add x12, x12, x16, lsl #4
  br x12

  // Prevent speculation through BR.
  speculation_postfence

.Lrestricted_syscall:
  // Move the pointer to the iframe into the first argument.
  mov x0, sp
  bl syscall_from_restricted
  // This does not return.

.Lunknown_syscall:
  mov x0, x16  // Syscall number in the first argument register.
  mov x1, x10  // User PC in the second argument register.
  bl unknown_syscall
  // Fall through.

.Lpost_syscall:
  // Upon return from syscall, x0 = status, x1 = thread signalled
  // Move the status to frame->r[0] for return to userspace.
  str x0, [sp, ARM64_IFRAME_OFFSET_R]

  // Spectre: ARM64 CPUs may speculatively execute instructions after an SVC
  // instruction.  The userspace entry code has a speculation barrier;
  // advance ELR_EL1 past it on the return since it has already done its job.
  .ifne ARM64_SYSCALL_SPECULATION_BARRIER_SIZE - 12
    .error "Syscall speculation barrier must be 12 bytes"
  .endif
  ldr  x10, [sp, ARM64_IFRAME_OFFSET_ELR]
  add  x10, x10, ARM64_SYSCALL_SPECULATION_BARRIER_SIZE
  str  x10, [sp, ARM64_IFRAME_OFFSET_ELR]

  // Check for pending signals. If none, just return.
  cbz x1, arm64_exc_shared_restore_lower_el

  // Call into C++ with the iframe as argument.
  mov x0, sp
  bl arch_iframe_process_pending_signals

  // Now that it's done, actually return to user mode.
  b arm64_exc_shared_restore_lower_el

  // The end of the function is the jump table for calling the wrapper
  // functions.  Each entry is 16 bytes long and naturally aligned so
  // that the arithmetic above uses the right offset from .Lsyscall_table
  // for the syscall number.
  .balign 16
  .Lsyscall_table:

// One of these macros is invoked by kernel.inc for each syscall.

// These don't have kernel entry points.
#define VDSO_SYSCALL(...)

// These are the direct kernel entry points.
#define KERNEL_SYSCALL(name, type, attrs, nargs, arglist, prototype) \
  syscall_dispatcher nargs, name
#define INTERNAL_SYSCALL(...) KERNEL_SYSCALL(__VA_ARGS__)
#define BLOCKING_SYSCALL(...) KERNEL_SYSCALL(__VA_ARGS__)

#include <lib/syscalls/kernel.inc>

#undef VDSO_SYSCALL
#undef KERNEL_SYSCALL
#undef INTERNAL_SYSCALL
#undef BLOCKING_SYSCALL

.end_function
