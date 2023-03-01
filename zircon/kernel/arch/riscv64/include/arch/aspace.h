// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_ARCH_RISCV64_INCLUDE_ARCH_ASPACE_H_
#define ZIRCON_KERNEL_ARCH_RISCV64_INCLUDE_ARCH_ASPACE_H_

// RISC-V arch-specific declarations for VmAspace implementation.

#include <debug.h>

#include <arch/riscv64/mmu.h>
#include <vm/arch_vm_aspace.h>

class Riscv64ArchVmAspace : public ArchVmAspaceInterface {
 public:
  Riscv64ArchVmAspace(vaddr_t base, size_t size, uint mmu_flags,
                      page_alloc_fn_t test_paf = nullptr);

  ~Riscv64ArchVmAspace() override;

  zx_status_t Init() override { PANIC_UNIMPLEMENTED; }

  void DisableUpdates() override { PANIC_UNIMPLEMENTED; }

  zx_status_t Destroy() override { PANIC_UNIMPLEMENTED; }

  zx_status_t MapContiguous(vaddr_t vaddr, paddr_t paddr, size_t count, uint mmu_flags,
                            size_t* mapped) override {
    PANIC_UNIMPLEMENTED;
  }

  zx_status_t Map(vaddr_t vaddr, paddr_t* phys, size_t count, uint mmu_flags,
                  ExistingEntryAction existing_action, size_t* mapped) override {
    PANIC_UNIMPLEMENTED;
  }

  zx_status_t Unmap(vaddr_t vaddr, size_t count, EnlargeOperation enlarge,
                    size_t* unmapped) override {
    PANIC_UNIMPLEMENTED;
  }

  zx_status_t Protect(vaddr_t vaddr, size_t count, uint mmu_flags) override { PANIC_UNIMPLEMENTED; }

  zx_status_t Query(vaddr_t vaddr, paddr_t* paddr, uint* mmu_flags) override {
    PANIC_UNIMPLEMENTED;
  }

  vaddr_t PickSpot(vaddr_t base, vaddr_t end, vaddr_t align, size_t size, uint mmu_flags) override {
    PANIC_UNIMPLEMENTED;
  }

  zx_status_t HarvestAccessed(vaddr_t vaddr, size_t count, NonTerminalAction non_terminal_action,
                              TerminalAction terminal_action) override {
    PANIC_UNIMPLEMENTED;
  }

  zx_status_t MarkAccessed(vaddr_t vaddr, size_t count) override { PANIC_UNIMPLEMENTED; }

  bool ActiveSinceLastCheck(bool clear) override { PANIC_UNIMPLEMENTED; }

  paddr_t arch_table_phys() const override { PANIC_UNIMPLEMENTED; }

  static void ContextSwitch(Riscv64ArchVmAspace* from, Riscv64ArchVmAspace* to);

  static vaddr_t NextUserPageTableOffset(vaddr_t va) { PANIC_UNIMPLEMENTED; }
};

class Riscv64ArchVmICacheConsistencyManager : public ArchVmICacheConsistencyManagerInterface {
 public:
  void SyncAddr(vaddr_t start, size_t len) override { PANIC_UNIMPLEMENTED; }

  void Finish() override { PANIC_UNIMPLEMENTED; }
};

using ArchVmAspace = Riscv64ArchVmAspace;
using ArchVmICacheConsistencyManager = Riscv64ArchVmICacheConsistencyManager;

#endif  // ZIRCON_KERNEL_ARCH_RISCV64_INCLUDE_ARCH_ASPACE_H_
