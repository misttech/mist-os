// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

.globl breakpoint_for_module_changes
.type breakpoint_for_module_changes, %function
breakpoint_for_module_changes:
  brk #0
  ret

//       fn restricted_enter_loop(
//  x0    options: u32,
//  x1    restricted_return: usize,
//  x2    restricted_exit_callback: usize,
//  x3    restricted_exit_callback_context: usize,
//  x4    restricted_state_addr: usize,
//  x5    extended_pstate_addr: usize,
//  x0 ) -> zx::sys::zx_status_t;
.globl restricted_enter_loop
.type restricted_enter_loop, %function
restricted_enter_loop:
  // This function:
  //   - Saves the callee saved registers (including the link register) on the stack.
  //   - Passes the stack pointer as the third ("context") parameter (in r2) to zx_restricted_enter().
  //   - Calls zx_restricted_enter().
  //   - Returns any failures (this will not return at all on success).

  // Save the callee saved registers (including the link register).
  stp x20, x19, [sp, #-16]!
  stp x22, x21, [sp, #-16]!
  stp x24, x23, [sp, #-16]!
  stp x26, x25, [sp, #-16]!
  stp x28, x27, [sp, #-16]!
  stp x30, x29, [sp, #-16]!

  // Save shadow call stack pointer on the stack (along with a zero to keep the stack aligned)
  stp x18, xzr, [sp, #-16]!   // Will be at [sp], #48

  // Save original options and vector table ptr
  stp  x1,  x0,  [sp, #-16]!  // Will be at [sp], #32

  // Save address of callback function and callback function context on the stack
  stp  x3,  x2,  [sp, #-16]!  // Will be at [sp], #16

  // Save address of the restricted and extended processor state mappings on the stack
  stp  x5,  x4,  [sp, #-16]!  // Will be at [sp], #0

  // Restore extended processor state
  mov x0, x5
  bl restore_extended_pstate

.restricted_enter_loop_top:
  // Restore zx_restricted_enter parameters from stack
  ldp x1, x0, [sp, #32]

  // Save stack pointer as context parameter for syscall
  mov x2, sp

//      fn zx_restricted_enter(
//  x0     uint32_t options,
//  x1     uintptr_t vector_table_ptr,
//  x2     uintptr_t context,
//  x0   ) -> zx_status_t
  bl zx_restricted_enter

  // If zx_restricted_enter returns to here then we did not enter restricted mode. Unwind the
  // stack and return the error in x0 to the caller.

.restricted_enter_loop_ret:
  // Pop temporaries
  add sp, sp, #(8 * 8)
  
  // Restore callee saved registers
  ldp x30, x29, [sp], #16
  ldp x28, x27, [sp], #16
  ldp x26, x25, [sp], #16
  ldp x24, x23, [sp], #16
  ldp x22, x21, [sp], #16
  ldp x20, x19, [sp], #16

  ret

// The restricted return entry point is not really a function but we treat it like one. It has the following
// parameters:
// fn restricted_return_loop(
//    x0   context: usize,
//    x1   reason_code: u64
// )
.globl restricted_return_loop
.type restricted_return_loop, %function
restricted_return_loop:
  // x0 holds the context, which is the stack pointer.
  mov sp, x0

  // Save the reason code in a callee-saved register
  mov x19, x1

  // Restore shadow call stack
  ldr x18, [sp, #48]

  // Save extended processor state
  ldr x0, [sp]
  bl save_extended_pstate

  // Load callback context and function pointers stack
  ldp x0, x2, [sp, #16]

  // Load the address of the mapped restricted mode register state to x27
  ldr x27, [sp, #8]

  // Load frame pointer from restricted state to connect Starnix stack to Linux's.
  // This offset matches the offset of x29 in the `zx_restricted_state_t` struct.
  ldr x29, [x27, 0xE8]

  // Emit CFI directives referring to the restricted mode register state
  .cfi_startproc
  .cfi_remember_state
  .cfi_def_cfa x27, 0
  .cfi_offset x0, 0
  .cfi_offset x1, 0x08
  .cfi_offset x2, 0x10
  .cfi_offset x3, 0x18
  .cfi_offset x4, 0x20
  .cfi_offset x5, 0x28
  .cfi_offset x6, 0x30
  .cfi_offset x7, 0x38
  .cfi_offset x8, 0x40
  .cfi_offset x9, 0x48
  .cfi_offset x10, 0x50
  .cfi_offset x11, 0x58
  .cfi_offset x12, 0x60
  .cfi_offset x13, 0x68
  .cfi_offset x14, 0x70
  .cfi_offset x15, 0x78
  .cfi_offset x16, 0x80
  .cfi_offset x17, 0x88
  .cfi_offset x18, 0x90
  .cfi_offset x19, 0x98
  .cfi_offset x20, 0xA0
  .cfi_offset x21, 0xA8
  .cfi_offset x22, 0xB0
  .cfi_offset x23, 0xB8
  .cfi_offset x24, 0xC0
  .cfi_offset x25, 0xC8
  .cfi_offset x26, 0xD0
  .cfi_offset x27, 0xD8
  .cfi_offset x28, 0xE0
  .cfi_offset x29, 0xE8
  .cfi_offset lr, 0xF0   // x30
  .cfi_offset sp, 0xF8   // x31
  .cfi_offset pc, 0x100  // x32

  // Invoke callback with context and reason_code:
  //       fn restricted_exit_callback_c(
  // x0      context: usize,
  // x1      reason_code: zx::sys::zx_restricted_reason_t,
  // x0    ) -> bool

  mov x1, x19
  blr x2
  
  // Restore CFI state after callback
  .cfi_restore_state
  .cfi_endproc

  // Did the callback tell us to exit?
  cbz x0, .restricted_enter_loop_ret

  // Restore extended processor state
  ldr x0, [sp]
  bl restore_extended_pstate

  // Go back to the loop
  b .restricted_enter_loop_top
