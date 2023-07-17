// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_ARCH_RISCV64_INCLUDE_ARCH_RISCV64_MMU_H_
#define ZIRCON_KERNEL_ARCH_RISCV64_INCLUDE_ARCH_RISCV64_MMU_H_

#include <lib/zircon-internal/macros.h>

#include <arch/defines.h>
#include <arch/kernel_aspace.h>

// These macros assume the sv39 virtual memory scheme which maps 39-bit
// virtual addresses to 56-bit physical addresses.  For details see sections
// 4.1.11, 4.2, and 4.3 in the RISC-V Privileged Spec.
//
// https://github.com/riscv/riscv-isa-manual/releases/download/Priv-v1.12/riscv-privileged-20211203.pdf
#define RISCV64_MMU_SIZE_SHIFT 39

// Sv39x4 for hypervisor guest translation
#define MMU_GUEST_SIZE_SHIFT (RISCV64_MMU_SIZE_SHIFT + 2)

#define RISCV64_MMU_PT_LEVELS 3
#define RISCV64_MMU_PT_SHIFT 9
#define RISCV64_MMU_PT_ENTRIES (1u << RISCV64_MMU_PT_SHIFT)  // 512
#define RISCV64_MMU_PT_KERNEL_BASE_INDEX (RISCV64_MMU_PT_ENTRIES / 2)
#define RISCV64_MMU_CANONICAL_MASK ((1ul << RISCV64_MMU_SIZE_SHIFT) - 1)
#define RISCV64_MMU_PPN_BITS 56

// page table bits
#define RISCV64_PTE_V (1ul << 0)  // valid
#define RISCV64_PTE_R (1ul << 1)  // read
#define RISCV64_PTE_W (1ul << 2)  // write
#define RISCV64_PTE_X (1ul << 3)  // execute
#define RISCV64_PTE_PERM_MASK (7ul << 1)
#define RISCV64_PTE_U (1ul << 4)         // user
#define RISCV64_PTE_G (1ul << 5)         // global
#define RISCV64_PTE_A (1ul << 6)         // accessed
#define RISCV64_PTE_D (1ul << 7)         // dirty
#define RISCV64_PTE_RSW_MASK (3ul << 8)  // reserved for software
#define RISCV64_PTE_PPN_SHIFT (10)
#define RISCV64_PTE_PPN_MASK \
  (((1ul << (RISCV64_MMU_PPN_BITS - PAGE_SIZE_SHIFT)) - 1) << RISCV64_PTE_PPN_SHIFT)

// SATP register, contains the current mmu mode, address space id, and
// pointer to root page table
#define RISCV64_SATP_MODE_NONE (0ul)
#define RISCV64_SATP_MODE_SV32 (1ul)
#define RISCV64_SATP_MODE_SV39 (8ul)
#define RISCV64_SATP_MODE_SV48 (9ul)
#define RISCV64_SATP_MODE_SV57 (10ul)
#define RISCV64_SATP_MODE_SV64 (11ul)

#define RISCV64_SATP_MODE_SHIFT (60)
#define RISCV64_SATP_ASID_SHIFT (44)
#define RISCV64_SATP_ASID_SIZE (16)
#define RISCV64_SATP_ASID_MASK ((1ul << RISCV64_SATP_ASID_SIZE) - 1)

#ifndef __ASSEMBLER__

#include <inttypes.h>
#include <sys/types.h>

typedef uintptr_t pte_t;

const size_t MMU_RISCV64_ASID_BITS = 16;
const uint16_t MMU_RISCV64_GLOBAL_ASID = (1u << MMU_RISCV64_ASID_BITS) - 1;
const uint16_t MMU_RISCV64_UNUSED_ASID = 0;
const uint16_t MMU_RISCV64_FIRST_USER_ASID = 1;
const uint16_t MMU_RISCV64_MAX_USER_ASID = MMU_RISCV64_GLOBAL_ASID - 1;

void riscv64_mmu_early_init();
void riscv64_mmu_early_init_percpu();
void riscv64_mmu_init();

// Helper routines for various page table entry manipulation
constexpr bool riscv64_pte_is_valid(pte_t pte) { return pte & RISCV64_PTE_V; }
constexpr bool riscv64_pte_is_leaf(pte_t pte) { return (pte & RISCV64_PTE_PERM_MASK) != 0; }

// riscv PPN is stored shifted over 2 from the natural alignment
constexpr paddr_t riscv64_pte_pa(pte_t pte) {
  return (pte & RISCV64_PTE_PPN_MASK) << (PAGE_SIZE_SHIFT - RISCV64_PTE_PPN_SHIFT);
}

constexpr pte_t riscv64_pte_pa_to_pte(paddr_t pa) {
  return (pa >> PAGE_SIZE_SHIFT) << RISCV64_PTE_PPN_SHIFT;
}

#endif  // __ASSEMBLER__

#endif  // ZIRCON_KERNEL_ARCH_RISCV64_INCLUDE_ARCH_RISCV64_MMU_H_
