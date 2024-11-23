// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

.globl breakpoint_for_module_changes
.type breakpoint_for_module_changes STT_FUNC
breakpoint_for_module_changes:
  int3
  ret


//       fn restricted_enter_loop(
// %rdi    options: u32,
// %rsi    restricted_return: usize,
// %rdx    restricted_exit_callback: usize,
// %rcx    restricted_exit_callback_context: usize,
// %r8     restricted_state_addr: usize,
// %r9     extended_pstate_addr: usize,
// %rax  ) -> zx::sys::zx_status_t;
.globl restricted_enter_loop
.type restricted_enter_loop STT_FUNC
restricted_enter_loop:
  // Save callee saved registers on the stack.
  push  %rbx
  push  %rbp
  push  %r12
  push  %r13
  push  %r14
  push  %r15
  push  %rsp

  // Save original options
  push %rdi // Will be at rsp+40
  // Save original vector table ptr
  push %rsi // Will be at rsp+32

  // Save address of callback function on the stack.
  push %rdx // Will be at rsp+24
  // Save address of callback function context on the stack.
  push %rcx // Will be at rsp+16
  // Save address of restricted state mapping on the stack
  push %r8  // Will be at rsp+8
  // Save address of the extended processor state object on the stack.
  push %r9  // Will be at rsp+0

  // Restore the extended processor state.
  mov %r9, %rdi
  call restore_extended_pstate

.restricted_enter_loop_top:
  //      fn zx_restricted_enter(
  // %rdi   uint32_t options,
  // %rsi   uintptr_t vector_table_ptr,
  // %rdx   uintptr_t context
  // %rax ) -> zx_status_t

  // Restore zx_restricted_enter parameters from stack
  mov 32(%rsp), %rsi
  mov 40(%rsp), %rdi

  // Save pointer to stack as context pointer in the syscall
  mov %rsp,%rdx

  // Call restricted enter syscall
  call zx_restricted_enter

  // If zx_restricted_enter returned then we never entered restricted mode. Unwind and
  // return the error to our caller.

.restricted_enter_loop_ret:
  // Pop temporaries
  add $(8*6), %rsp
  // Pop callee saved registers
  pop  %rsp
  pop  %r15
  pop  %r14
  pop  %r13
  pop  %r12
  pop  %rbp
  pop  %rbx

  // Return value is already in %rax
  ret


// The restricted return entry point is not really a function but we treat it like one. It has the following
// parameters:
// fn restricted_return(
//   %rdi   context: usize,
//   %rsi   reason_code: u64
// )
.globl restricted_return_loop
.type restricted_return_loop STT_FUNC
restricted_return_loop:
  // Back from restricted mode, rdi holds our context (stack pointer, before call)
  mov %rdi,%rsp

  // Save the reason code in a callee-saved register
  mov %rsi, %rbx

  // Save the extended processor state.
  mov 0(%rsp), %rdi
  call save_extended_pstate

  // Load callback function pointer from stack
  mov 24(%rsp), %rdx

  // Load callback function context pointer from stack
  mov 16(%rsp), %rdi

  // Load restricted state mapping address from stack to a callee-saved register.
  mov 8(%rsp), %r14

  // Load frame pointer from restricted state to connect Starnix stack to Linux's.
  // This offset matches the offset of rbp in the `zx_restricted_state_t` struct.
  mov 0x10(%r14), %rbp

  // Emit CFI directives referring to the current restricted mode register state to
  // emulate the restricted mode code "calling" the restricted exit callback.
  .cfi_startproc
  .cfi_remember_state
  .cfi_def_cfa r14, 0
  // These offsets match the offsets of
  // the register values in the `zx_restricted_state_t` struct.
  .cfi_offset rdi, 0x00
  .cfi_offset rsi, 0x08
  .cfi_offset rbp, 0x10
  .cfi_offset rbx, 0x18
  .cfi_offset rdx, 0x20
  .cfi_offset rcx, 0x28
  .cfi_offset rax, 0x30
  .cfi_offset rsp, 0x38
  .cfi_offset r8,  0x40
  .cfi_offset r9,  0x48
  .cfi_offset r10, 0x50
  .cfi_offset r11, 0x58
  .cfi_offset r12, 0x60
  .cfi_offset r13, 0x68
  .cfi_offset r14, 0x70
  .cfi_offset r15, 0x78
  .cfi_offset rip, 0x80

  // Invoke callback with context and reason_code:
  //       fn restricted_exit_callback_c(
  // %rdi    context: usize,
  // %rsi    reason_code: zx::sys::zx_restricted_reason_t,
  // %rax  ) -> bool

  // Restore reason code
  mov %rbx, %rsi
  // Invoke callback
  callq *%rdx

  // Restore CFI state
  .cfi_restore_state
  .cfi_endproc

  // Did the callback tell us to exit?
  test %eax, %eax
  je .restricted_enter_loop_ret

  // Restore extended processor state
  mov 0(%rsp), %rdi
  call restore_extended_pstate

  // Go back to the loop.
  jmp .restricted_enter_loop_top
