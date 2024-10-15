// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_ARCH_X64_REGISTERS_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_ARCH_X64_REGISTERS_H_

#include <inttypes.h>
#include <stdint.h>
#include <stdio.h>
#include <zircon/syscalls/debug.h>
#include <zircon/types.h>

#include <linux/errno.h>

namespace starnix {

struct ThreadStartInfo;

/// The state of the task's registers when the thread of execution entered the kernel.
///
/// Implements [`std::ops::Deref`] and [`std::ops::DerefMut`] as a way to get at the underlying
/// [`zx::sys::zx_thread_state_general_regs_t`] that this type wraps.
struct RegisterState {
 private:
  ::zx_thread_state_general_regs_t real_registers_ = {};

 public:
  /// A copy of the x64 `rax` register at the time of the `syscall` instruction. This is important
  /// to store, as the return value of a syscall overwrites `rax`, making it impossible to recover
  /// the original syscall number in the case of syscall restart and strace output.
  uint64_t orig_rax;

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
  static RegisterState From(const zx_thread_state_general_regs_t& regs) {
    return RegisterState(regs, regs.rax);
  }

  static void print_regs(FILE* f, const zx_thread_state_general_regs_t* regs) {
    fprintf(f, " CS:  %#18llx RIP: %#18" PRIx64 " EFL: %#18" PRIx64, 0ull, regs->rip, regs->rflags);
    fprintf(f, "\n");
    fprintf(f, " RAX: %#18" PRIx64 " RBX: %#18" PRIx64 " RCX: %#18" PRIx64 " RDX: %#18" PRIx64 "\n",
            regs->rax, regs->rbx, regs->rcx, regs->rdx);
    fprintf(f, " RSI: %#18" PRIx64 " RDI: %#18" PRIx64 " RBP: %#18" PRIx64 " RSP: %#18" PRIx64 "\n",
            regs->rsi, regs->rdi, regs->rbp, regs->rsp);
    fprintf(f, "  R8: %#18" PRIx64 "  R9: %#18" PRIx64 " R10: %#18" PRIx64 " R11: %#18" PRIx64 "\n",
            regs->r8, regs->r9, regs->r10, regs->r11);
    fprintf(f, " R12: %#18" PRIx64 " R13: %#18" PRIx64 " R14: %#18" PRIx64 " R15: %#18" PRIx64 "\n",
            regs->r12, regs->r13, regs->r14, regs->r15);
    fprintf(f, " fs.base: %#18" PRIx64 " gs.base: %#18" PRIx64 "\n", regs->fs_base, regs->gs_base);
  }

 public:
  RegisterState() = default;

  zx_thread_state_general_regs_t* operator->() { return &real_registers_; }
  const zx_thread_state_general_regs_t* operator->() const { return &real_registers_; }

  zx_thread_state_general_regs_t& operator*() { return real_registers_; }
  const zx_thread_state_general_regs_t& operator*() const { return real_registers_; }

 private:
  RegisterState(zx_thread_state_general_regs_t regs, uint64_t _orig_rax)
      : real_registers_(regs), orig_rax(_orig_rax) {}
};

}  // namespace starnix

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_ARCH_X64_REGISTERS_H_
