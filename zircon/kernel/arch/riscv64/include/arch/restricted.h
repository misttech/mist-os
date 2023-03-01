// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_ARCH_RISCV64_INCLUDE_ARCH_RESTRICTED_H_
#define ZIRCON_KERNEL_ARCH_RISCV64_INCLUDE_ARCH_RESTRICTED_H_

#include <debug.h>

#include <arch/regs.h>

// ArchRestrictedState
class Riscv64ArchRestrictedState final : public ArchRestrictedStateImpl {
 public:
  Riscv64ArchRestrictedState() = default;

  ~Riscv64ArchRestrictedState() = default;

  // Since we're unimplemented, always NAK the pre-entry check.
  bool ValidatePreRestrictedEntry() override { return false; }

  // The rest of these should not be called by the restricted mode engine if
  // the Validate check above fails.
  void SaveStatePreRestrictedEntry() override { PANIC_UNIMPLEMENTED; }

  [[noreturn]] void EnterRestricted() override { PANIC_UNIMPLEMENTED; }

  void SaveRestrictedSyscallState(const syscall_regs_t *regs) override { PANIC_UNIMPLEMENTED; }

  [[noreturn]] void EnterFull(uintptr_t vector_table, uintptr_t context, uint64_t code) override {
    PANIC_UNIMPLEMENTED;
  }

  void Dump() override {}
};

using ArchRestrictedState = Riscv64ArchRestrictedState;

#endif  // ZIRCON_KERNEL_ARCH_RISCV64_INCLUDE_ARCH_RESTRICTED_H_
