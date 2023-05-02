// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// do something to all the registers so we can read the state on the way out
.macro twiddle_registers
  add x0, x0, #1
  add x1, x1, #1
  add x2, x2, #1
  add x3, x3, #1
  add x4, x4, #1
  add x5, x5, #1
  add x6, x6, #1
  add x7, x7, #1
  add x8, x8, #1
  add x9, x9, #1
  add x10, x10, #1
  add x11, x11, #1
  add x12, x12, #1
  add x13, x13, #1
  add x14, x14, #1
  add x15, x15, #1
  add x16, x16, #1
  add x17, x17, #1
  add x18, x18, #1
  add x19, x19, #1
  add x20, x20, #1
  add x21, x21, #1
  add x22, x22, #1
  add x23, x23, #1
  add x24, x24, #1
  add x25, x25, #1
  add x26, x26, #1
  add x27, x27, #1
  add x28, x28, #1
  add x29, x29, #1
  add x30, x30, #1
  add sp, sp, #1

  // Save the contents of x16 to TLS prior to running a syscall.
  mrs x0, tpidr_el0
  str x16, [x0]
.endm

.globl vectab
vectab:
  // Back from restricted mode
  // x0 holds the context, which is the stack pointer
  // x1 holds the reason code

  // Restore the stack pointer at the point of the restricted enter wrapper.
  mov  sp,x0

  // Load the frame pointer and return address from the wrapper.
  ldp x30, x29, [sp], #16

  // Restore the callee saved registers.
  ldp x28, x27, [sp], #16
  ldp x26, x25, [sp], #16
  ldp x24, x23, [sp], #16
  ldp x22, x21, [sp], #16
  ldp x20, x19, [sp], #16

  // Load the shadow call stack pointer and reason code pointer.
  ldp x2, x18, [sp], #16

  // Return the reason code from this function by setting the reason code pointer.
  str  x1, [x2]

  // Return back to whatever the address was in the link register.
  // Make it appear as if the wrapper had returned ZX_OK
  mov  x0, xzr
  ret

.globl syscall_bounce
syscall_bounce:
  // do something to all the registers so we can read
  // the state on the way out
  twiddle_registers
0:
  mov x16, xzr
  add x16, x16, #64
  svc #0
.globl syscall_bounce_post_syscall
syscall_bounce_post_syscall:
  bl syscall_bounce

.globl exception_bounce
exception_bounce:
  // do something to all the registers so we can read
  // the state on the way out
  twiddle_registers
.globl exception_bounce_exception_address
exception_bounce_exception_address:
  udf #0
  b exception_bounce_exception_address
.globl exception_bounce_exit
exception_bounce_exit:
  // Perform a syscall to exit. This is needed if we're using channel-based
  // exception handlers.
  mov x16, xzr
  add x16, x16, #64
  svc #0

.globl restricted_enter_wrapper
restricted_enter_wrapper:
  // Args 0 - 1 are already in place in X0 and X1.

  // Save the reason code pointer and shadow call stack pointer on the stack.
  stp x2, x18, [sp, #-16]!

  // Save the callee saved regs since the return from restricted mode
  // will modify all registers.
  stp x20, x19, [sp, #-16]!
  stp x22, x21, [sp, #-16]!
  stp x24, x23, [sp, #-16]!
  stp x26, x25, [sp, #-16]!
  stp x28, x27, [sp, #-16]!

  // Save the frame pointer and return address to the stack.
  stp x30, x29, [sp, #-16]!

  // Pass the stack pointer as the context argument to the syscall.
  mov x2, sp

  bl zx_restricted_enter

  // if we got here it must have failed
  add  sp, sp, #(14*8) // pop the previous state on the stack
  ret
