// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

.weak zx_restricted_enter

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
  call  zx_restricted_enter@PLT

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

// Atomically adds 1 to *rdi, then loops until *rsi is nonzero and then issues a syscall.
.globl wait_then_syscall
wait_then_syscall:
  lock incl    (%rdi)
.wait_then_syscall_loop:
  movl    (%rsi), %eax
  testl   %eax, %eax
  je      .wait_then_syscall_loop
  syscall
  ud2  // Should never be reached

// Load the contents of the array in *rdi to the FPU.
.globl load_fpu_registers
load_fpu_registers:
  // Load registers ST0-ST7 with data.
  fldt (%rdi)
  fldt 10(%rdi)
  fldt 20(%rdi)
  fldt 30(%rdi)
  fldt 40(%rdi)
  fldt 50(%rdi)
  fldt 60(%rdi)
  fldt 70(%rdi)

  // Load values into the SSE registers.
  movups 80(%rdi), %xmm0
  movups 96(%rdi), %xmm1
  movups 112(%rdi), %xmm2
  movups 128(%rdi), %xmm3
  movups 144(%rdi), %xmm4
  movups 160(%rdi), %xmm5
  movups 176(%rdi), %xmm6
  movups 192(%rdi), %xmm7
  movups 208(%rdi), %xmm8
  movups 224(%rdi), %xmm9
  movups 240(%rdi), %xmm10
  movups 256(%rdi), %xmm11
  movups 272(%rdi), %xmm12
  movups 288(%rdi), %xmm13
  movups 304(%rdi), %xmm14
  movups 320(%rdi), %xmm15
  ret

// Store the contents of the FPU into the array in *rdi.
.globl store_fpu_registers
store_fpu_registers:
  // Pop the float values out of ST0-ST7.
  // These registers are treated as a stack and thus must be popped LIFO.
  fstpt 70(%rdi)
  fstpt 60(%rdi)
  fstpt 50(%rdi)
  fstpt 40(%rdi)
  fstpt 30(%rdi)
  fstpt 20(%rdi)
  fstpt 10(%rdi)
  fstpt (%rdi)

  // Store the SSE registers.
  movups %xmm0, 80(%rdi)
  movups %xmm1, 96(%rdi)
  movups %xmm2, 112(%rdi)
  movups %xmm3, 128(%rdi)
  movups %xmm4, 144(%rdi)
  movups %xmm5, 160(%rdi)
  movups %xmm6, 176(%rdi)
  movups %xmm7, 192(%rdi)
  movups %xmm8, 208(%rdi)
  movups %xmm9, 224(%rdi)
  movups %xmm10, 240(%rdi)
  movups %xmm11, 256(%rdi)
  movups %xmm12, 272(%rdi)
  movups %xmm13, 288(%rdi)
  movups %xmm14, 304(%rdi)
  movups %xmm15, 320(%rdi)
  ret
