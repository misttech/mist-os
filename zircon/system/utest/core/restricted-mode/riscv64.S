// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// Do something to all the registers so we can read the state on the way out.
.macro twiddle_registers
  addi ra, ra, 1
  addi sp, sp, 1
  addi gp, gp, 1
  // Skip TP as we want to write to TLS later.
  addi t0, t0, 1
  addi t1, t1, 1
  addi t2, t2, 1
  addi s0, s0, 1
  addi s1, s1, 1
  addi a0, a0, 1
  addi a1, a1, 1
  addi a2, a2, 1
  addi a3, a3, 1
  addi a4, a4, 1
  addi a5, a5, 1
  addi a6, a6, 1
  addi a7, a7, 1
  addi s2, s2, 1
  addi s3, s3, 1
  addi s4, s4, 1
  addi s5, s5, 1
  addi s6, s6, 1
  addi s7, s7, 1
  addi s8, s8, 1
  addi s9, s9, 1
  addi s10, s10, 1
  addi s11, s11, 1
  addi t3, t3, 1
  addi t4, t4, 1
  addi t5, t5, 1
  addi t6, t6, 1

  // Save the contents of t0 to TLS prior to running a syscall.
  sd t0, (tp)
.endm

.globl vectab
vectab:
  // Back from restricted mode
  // a0 holds the context, which is the stack pointer
  // a1 holds the reason code

  // Restore the stack pointer at the point of the restricted enter wrapper.
  mv sp, a0

  // Restore the shadow call stack pointer.
  ld gp, 8(sp)

  // Restore the callee saved registers.
  ld s0, 16(sp)
  ld s1, 24(sp)
  ld s2, 32(sp)
  ld s3, 40(sp)
  ld s4, 48(sp)
  ld s5, 56(sp)
  ld s6, 64(sp)
  ld s7, 72(sp)
  ld s8, 80(sp)
  ld s9, 88(sp)
  ld s10, 96(sp)
  ld s11, 104(sp)

  // Restore the return address.
  ld ra, 112(sp)

  // Restore the thread pointer.
  ld tp, 120(sp)

  // Move the reason code into the stored pointer.
  ld t3, (sp)
  sd a1, (t3)

  // Pop all the normal mode context off the stack.
  addi sp, sp, 128

  // Return to whatever address was in RA.
  // Make it appear as if the wrapper had returned ZX_OK.
  mv a0, zero
  ret

.globl syscall_bounce
syscall_bounce:
  // Do something to all the registers so we can read the state on the way out.
  twiddle_registers
0:
  mv t0, zero
  addi t0, t0, 64
  ecall
.globl syscall_bounce_post_syscall
syscall_bounce_post_syscall:
  jal syscall_bounce

.globl restricted_enter_wrapper
restricted_enter_wrapper:
  // Args 0 - 1 are already in a0 and a1.

  // Make space for all of the normal mode context on the stack.
  addi sp, sp, -128

  // Save the reason code pointer.
  sd a2, (sp)

  // Save the shadow call stack pointer.
  sd gp, 8(sp)

  // Save all of the callee saved registers.
  sd s0, 16(sp)
  sd s1, 24(sp)
  sd s2, 32(sp)
  sd s3, 40(sp)
  sd s4, 48(sp)
  sd s5, 56(sp)
  sd s6, 64(sp)
  sd s7, 72(sp)
  sd s8, 80(sp)
  sd s9, 88(sp)
  sd s10, 96(sp)
  sd s11, 104(sp)

  // Save the return address.
  sd ra, 112(sp)

  // Save the thread pointer.
  sd tp, 120(sp)

  // Pass the stack pointer as the context argument to the syscall.
  mv a2, sp

  call zx_restricted_enter

  // If we got here it must have failed.
  // Restore the return address from prior to the syscall. We have to do this
  // because RA is caller-saved.
  ld ra, 112(sp)
  // Reset the stack.
  addi sp, sp, 128
  ret

.globl exception_bounce
exception_bounce:
  // Do something to all the registers so we can read the state on the way out.
  twiddle_registers

.globl exception_bounce_exception_address
exception_bounce_exception_address:
  unimp
  jal exception_bounce_exception_address

// Stores 1 to *a0 in a loop.
.globl store_one
store_one:
  addi a1, a1, 1
  addi t0, zero, 1
.store_one_loop:
  sw t0, (a0)
  jal .store_one_loop

// Stores 1 to *a0, then loops until *a1 is nonzero and then issues a syscall.
.globl wait_then_syscall
wait_then_syscall:
  addi t0, zero, 1
  sw t0, (a0)
.wait_then_syscall_loop:
  lw t0, (a1)
  beqz t0, .wait_then_syscall_loop
  ecall
  unimp // Should never be reached