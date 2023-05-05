// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// do something to all the registers so we can read the state on the way out
.macro twiddle_registers
  inc  %rax
  inc  %rbx
  inc  %rcx
  inc  %rdx
  inc  %rsi
  inc  %rdi
  inc  %rbp
  inc  %rsp
  inc  %r8
  inc  %r9
  inc  %r10
  inc  %r11
  inc  %r12
  inc  %r13
  inc  %r14
  inc  %r15

  // write rcx and r11 to fs and gs base since they are both
  // trashed by the syscall. also tests that fs and gs base are
  // set properly.
  mov   %rcx, %fs:0
  mov   %r11, %gs:0

.endm

.globl vectab
vectab:
  // back from restricted mode
  // rdi holds the context
  // rsi holds the reason code
  mov  %rdi,%rsp
  pop  %rsp
  pop  %r15
  pop  %r14
  pop  %r13
  pop  %r12
  pop  %rbp
  pop  %rbx

  // pop the reason code return slot
  pop  %rdx

  // return the reason code from this function
  mov  %rsi,(%rdx)

  // return back to whatever the address was on the stack
  // make it appear as if the wrapper had returned ZX_OK
  xor  %eax,%eax
  ret

.globl syscall_bounce
syscall_bounce:
  twiddle_registers
0:
  syscall
.globl syscall_bounce_post_syscall
syscall_bounce_post_syscall:
  jmp 0b

.globl exception_bounce
exception_bounce:
  twiddle_registers
.globl exception_bounce_exception_address
exception_bounce_exception_address:
  ud2
  jmp exception_bounce_exception_address
.globl exception_bounce_exit
exception_bounce_exit:
  syscall

.globl restricted_enter_wrapper
restricted_enter_wrapper:
  // args 0 - 1 are already in place in rdi, rsi

  // save the return code pointer on the stack
  push  %rdx

  // save the callee saved regs since the return from restricted mode
  // will zero out all of the registers except rdi and rsi
  push  %rbx
  push  %rbp
  push  %r12
  push  %r13
  push  %r14
  push  %r15
  push  %rsp

  // save the pointer the stack as the context pointer in the syscall
  mov   %rsp,%rdx

  // call the syscall
  call  zx_restricted_enter

  // if we got here it must have failed
  add   $(8*8),%rsp // pop the previous state on the stack
  ret

// Stores 1 to *rax in a loop.
.globl store_one
store_one:
  inc %rbx
.store_one_loop:
  movl     $1, (%rax)
  jmp     .store_one_loop

// Stores 1 to *rdi, then loops until *rsi is nonzero and then issues a syscall.
.globl wait_then_syscall
wait_then_syscall:
  movl    $1, %eax
  xchgl   %eax, (%rdi)
.wait_then_syscall_loop:
  movl    (%rsi), %eax
  testl   %eax, %eax
  je      .wait_then_syscall_loop
  syscall
  ud2  // Should never be reached
