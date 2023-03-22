// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/zx/vmo.h>
#include <zircon/status.h>
#include <zircon/syscalls-next.h>
#include <zircon/syscalls.h>
#include <zircon/testonly-syscalls.h>
#include <zircon/types.h>

#include <perftest/perftest.h>

#include "assert.h"

#if __x86_64__
asm(R"(
.globl vectab
vectab:
  // back from restricted mode
  // rdi holds the context
  // rsi holds the error code
  mov  %rdi,%rsp
  pop  %rsp
  pop  %r15
  pop  %r14
  pop  %r13
  pop  %r12
  pop  %rbp
  pop  %rbx

  // pop the error code return slot
  pop  %rdx

  // return the error code from this function
  mov  %rax,(%rdx)

  // return back to whatever the address was on the stack
  // make it appear as if the wrapper had returned ZX_OK
  xor  %rax,%rax
  ret
)");

asm(R"(
.globl bounce
bounce:
  // write rcx and r11 to fs and gs base since they are both
  // trashed by the syscall. also tests that fs and gs base are
  // set properly.
  mov   %rcx, %fs:0
  mov   %r11, %gs:0

0:
  syscall
.globl bounce_post_syscall
bounce_post_syscall:
  jmp 0b
)");

asm(R"(
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
)");

extern "C" void vectab();
extern "C" void bounce();
extern "C" zx_status_t restricted_enter_wrapper(uint32_t options, uintptr_t vector_table,
                                                uint64_t* exit_code);

class RestrictedState {
 public:
  RestrictedState() {
    // Create a VMO and bind it to the current thread.
    zx::vmo vmo;
    ASSERT_OK(zx_restricted_bind_state(0, vmo.reset_and_get_address()));

    state_.ip = (uint64_t)bounce;
    state_.flags = 0;
    state_.fs_base = (uintptr_t)&fs_val_;
    state_.gs_base = (uintptr_t)&gs_val_;

    // Set the state
    ASSERT_OK(vmo.write(&state_, 0, sizeof(state_)));
  }

  ~RestrictedState() { ASSERT_OK(zx_restricted_unbind_state(0)); }

 private:
  uint64_t fs_val_ = 0;
  uint64_t gs_val_ = 0;
  zx_restricted_state state_{};
};

bool RestrictedEnterAndExitViaSyscall(perftest::RepeatState* state) {
  RestrictedState restricted_state;
  uint64_t exit_code;
  while (state->KeepRunning()) {
    ASSERT_OK(restricted_enter_wrapper(0, (uintptr_t)vectab, &exit_code));
  }
  return true;
}

void RegisterTests() {
  perftest::RegisterTest("RestrictedEnterAndExitViaSyscall", RestrictedEnterAndExitViaSyscall);
}

PERFTEST_CTOR(RegisterTests)

#endif  // __x86_64__
