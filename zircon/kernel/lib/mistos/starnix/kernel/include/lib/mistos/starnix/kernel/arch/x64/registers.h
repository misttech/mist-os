// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_ARCH_X64_REGISTERS_H_
#define ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_ARCH_X64_REGISTERS_H_

#include <zircon/types.h>

namespace starnix {

struct ThreadStartInfo;
struct zx_thread_state_general_regs_t {
  uint64_t rax;
  uint64_t rbx;
  uint64_t rcx;
  uint64_t rdx;
  uint64_t rsi;
  uint64_t rdi;
  uint64_t rbp;
  uint64_t rsp;
  uint64_t r8;
  uint64_t r9;
  uint64_t r10;
  uint64_t r11;
  uint64_t r12;
  uint64_t r13;
  uint64_t r14;
  uint64_t r15;
  uint64_t rip;
  uint64_t rflags;
  uint64_t fs_base;
  uint64_t gs_base;

  static zx_thread_state_general_regs_t From(const ThreadStartInfo& val);
};

/// The state of the task's registers when the thread of execution entered the kernel.
/// This is a thin wrapper around [`zx::sys::zx_thread_state_general_regs_t`].
///
/// Implements [`std::ops::Deref`] and [`std::ops::DerefMut`] as a way to get at the underlying
/// [`zx::sys::zx_thread_state_general_regs_t`] that this type wraps.
struct RegisterState {
  zx_thread_state_general_regs_t real_registers;

  /// A copy of the x64 `rax` register at the time of the `syscall` instruction. This is important
  /// to store, as the return value of a syscall overwrites `rax`, making it impossible to recover
  /// the original syscall number in the case of syscall restart and strace output.
  uint64_t orig_rax;

  static RegisterState From(zx_thread_state_general_regs_t regs) {
    return RegisterState{regs, regs.rax};
  }
};

}  // namespace starnix

#endif  // ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_ARCH_X64_REGISTERS_H_
