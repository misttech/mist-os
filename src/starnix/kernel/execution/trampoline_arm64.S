// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

.globl restricted_enter
restricted_enter:
  // This function takes two arguments, the options value (in r0) and the vector_table (in r1) to
  // pass to the zx_restricted_enter() syscall. These are untouched as they are also the first two
  // parameters to zx_restricted_enter().
  //
  // This function:
  //   - Saves the callee saved registers (including the link register) on the stack.
  //   - Passes the stack pointer as the third ("context") parameter (in r2) to zx_restricted_enter().
  //   - Calls zx_restricted_enter().
  //   - Returns any failures (this will not return at all on success).

  // Save the reason code pointer and shadow call stack pointer on the stack.
  stp x2, x18, [sp, #-16]!

  // Save the callee saved registers (including the link register).
  stp x20, x19, [sp, #-16]!
  stp x22, x21, [sp, #-16]!
  stp x24, x23, [sp, #-16]!
  stp x26, x25, [sp, #-16]!
  stp x28, x27, [sp, #-16]!
  stp x30, x29, [sp, #-16]!

  // Pass the stack pointer as the "context" parameter to zx_restricted_enter().
  mov x2, sp

  bl zx_restricted_enter

  // If we got here we must have failed.
  add sp, sp, #(14 * 8) // Pop the previous state on the stack.
  ret

.globl restricted_return
restricted_return:
  // This function does the reverse of restricted_enter:
  //   - Restore the stack pointer which will be passed in as the first parameter (r0).
  //   - Write the reason code to the pointer provided by the caller.
  //   - Pop the callee saved registers we pushed in restricted_enter() above.
  //   - Return to the link register. This will be the one passed in to restricted_enter so will
  //     have the effect of returning from that call.

  // x0 holds the context, which is the stack pointer.
  mov sp, x0

  // Set the return value to ZX_OK.
  mov x0, xzr

  // Restore the callee saved registers.
  ldp x30, x29, [sp], #16
  ldp x28, x27, [sp], #16
  ldp x26, x25, [sp], #16
  ldp x24, x23, [sp], #16
  ldp x22, x21, [sp], #16
  ldp x20, x19, [sp], #16

  // Load the shadow call stack pointer and reason code pointer.
  ldp x2, x18, [sp], #16

  // Write the reason code to the pointer provided by the caller.
  str  x1, [x2]
  ret

.globl breakpoint_for_module_changes
breakpoint_for_module_changes:
  brk #0
  ret
