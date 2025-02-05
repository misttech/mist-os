// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// This file is based on arm64.S. Please see it for detailed comments.
#include <lib/arch/asm.h>

// do something to all the registers so we can read the state on the way out
.macro twiddle_registers
  add r0, #1
  add r1, #1
  add r2, #1
  add r3, #1
  add r4, #1
  add r5, #1
  add r6, #1
  add r7, #1
  add r8, #1
  add r9, #1
  add r10, #1
  add r11, #1
  add r12, #1
  // Keep the SP 16-byte aligned, as required by the spec.
  add sp, #16

  // Save the contents of r7 to TLS prior to running a syscall.
  // Load TPIDRURO into r0 as zircon should shadow TPIDR into it.
  mrc p15, 0, r0, c13, c0, 3
  str r7, [r0]
.endm

.function syscall_bounce, export
  // do something to all the registers so we can read
  // the state on the way out
  twiddle_registers
.Lsyscall_only:
  mov r7, #64
  svc #0
.label syscall_bounce_post_syscall, export
  b .Lsyscall_only
.end_function

.function exception_bounce, export
  // do something to all the registers so we can read
  // the state on the way out
  twiddle_registers
.label exception_bounce_exception_address, export
.Lexception_bounce_exception_address:
  udf #0
  b .Lexception_bounce_exception_address
.end_function

// Stores 1 to *r0 in a loop.
.function store_one, export
  add     r1, #1
  mov     r8, #1
.Lstore_one_loop:
  str     r8, [r0]
  b       .Lstore_one_loop
.end_function

// Atomically adds 1 to *r0, then loops until *r1 is nonzero and then issues a syscall.
.function wait_then_syscall, export
.Lwait_then_syscall:
  ldrex   r8, [r0]
  add     r8, #1
  strex   r9, r8, [r0]
  cmp     r9, #0
  bne     .Lwait_then_syscall
.Lwait_then_syscall_loop:
  ldr     r7, [r1]
  cmp     r7, #0
  beq     .Lwait_then_syscall_loop
  svc     #0
  bkpt    #0x1
.end_function
