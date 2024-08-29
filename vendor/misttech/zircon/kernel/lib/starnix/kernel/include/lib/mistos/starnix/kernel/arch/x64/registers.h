// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_ARCH_X64_REGISTERS_H_
#define ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_ARCH_X64_REGISTERS_H_

#include <zircon/types.h>

#include <linux/errno.h>

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
 private:
  mutable zx_thread_state_general_regs_t real_registers_;

 public:
  /// A copy of the x64 `rax` register at the time of the `syscall` instruction. This is important
  /// to store, as the return value of a syscall overwrites `rax`, making it impossible to recover
  /// the original syscall number in the case of syscall restart and strace output.
  uint64_t orig_rax;

 public:
  /// impl RegisterState

  void save_registers_for_restart(uint64_t syscall_number) {
    // The `rax` register read from the thread's state is clobbered by
    // zircon with ZX_ERR_BAD_SYSCALL.  Similarly, Linux sets it to ENOSYS
    // until it has determined the correct return value for the syscall; we
    // emulate this behavior because ptrace callers expect it.
    real_registers_.rax = -static_cast<uint64_t>(ENOSYS);

    // `orig_rax` should hold the original value loaded into `rax` by the userspace process.
    orig_rax = syscall_number;
  }

  /// Sets the register that indicates the single-machine-word return value from a
  /// function call.
  void set_return_register(uint64_t return_value) { real_registers_.rax = return_value; }

  /// Sets the register that indicates the current stack pointer.
  void set_stack_pointer_register(uint64_t sp) { real_registers_.rsp = sp; }

  /// Sets the register that indicates the TLS.
  void set_thread_pointer_register(uint64_t tp) { real_registers_.fs_base = tp; }

 public:
  static RegisterState From(zx_thread_state_general_regs_t regs) {
    return RegisterState(regs, regs.rax);
  }

 public:
  RegisterState() = default;
  zx_thread_state_general_regs_t* operator->() const { return &real_registers_; }

 private:
  RegisterState(zx_thread_state_general_regs_t regs, uint64_t _orig_rax)
      : real_registers_(regs), orig_rax(_orig_rax) {}
};

}  // namespace starnix

#endif  // ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_ARCH_X64_REGISTERS_H_
