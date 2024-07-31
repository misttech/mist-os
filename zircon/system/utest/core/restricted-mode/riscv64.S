// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

.weak zx_restricted_enter

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

// Atomically adds 1 to *a0, then loops until *a1 is nonzero and then issues a syscall.
.globl wait_then_syscall
wait_then_syscall:
  addi t0, zero, 1
  amoadd.w t1, t0, (a0)
.wait_then_syscall_loop:
  lw t0, (a1)
  beqz t0, .wait_then_syscall_loop
  ecall
  unimp // Should never be reached

// Load the contents of the array in *a0 to the FPU registers.
.globl load_fpu_registers
load_fpu_registers:
  fld f0, (a0)
  fld f1, 8(a0)
  fld f2, 16(a0)
  fld f3, 24(a0)
  fld f4, 32(a0)
  fld f5, 40(a0)
  fld f6, 48(a0)
  fld f7, 56(a0)
  fld f8, 64(a0)
  fld f9, 72(a0)
  fld f10, 80(a0)
  fld f11, 88(a0)
  fld f12, 96(a0)
  fld f13, 104(a0)
  fld f14, 112(a0)
  fld f15, 120(a0)
  fld f16, 128(a0)
  fld f17, 136(a0)
  fld f18, 144(a0)
  fld f19, 152(a0)
  fld f20, 160(a0)
  fld f21, 168(a0)
  fld f22, 176(a0)
  fld f23, 184(a0)
  fld f24, 192(a0)
  fld f25, 200(a0)
  fld f26, 208(a0)
  fld f27, 216(a0)
  fld f28, 224(a0)
  fld f29, 232(a0)
  fld f30, 240(a0)
  fld f31, 248(a0)
  ret

// Store the contents of the FPU registers into the array in *a0.
.globl store_fpu_registers
store_fpu_registers:
  fsd f0, (a0)
  fsd f1, 8(a0)
  fsd f2, 16(a0)
  fsd f3, 24(a0)
  fsd f4, 32(a0)
  fsd f5, 40(a0)
  fsd f6, 48(a0)
  fsd f7, 56(a0)
  fsd f8, 64(a0)
  fsd f9, 72(a0)
  fsd f10, 80(a0)
  fsd f11, 88(a0)
  fsd f12, 96(a0)
  fsd f13, 104(a0)
  fsd f14, 112(a0)
  fsd f15, 120(a0)
  fsd f16, 128(a0)
  fsd f17, 136(a0)
  fsd f18, 144(a0)
  fsd f19, 152(a0)
  fsd f20, 160(a0)
  fsd f21, 168(a0)
  fsd f22, 176(a0)
  fsd f23, 184(a0)
  fsd f24, 192(a0)
  fsd f25, 200(a0)
  fsd f26, 208(a0)
  fsd f27, 216(a0)
  fsd f28, 224(a0)
  fsd f29, 232(a0)
  fsd f30, 240(a0)
  fsd f31, 248(a0)
  ret
