// Copyright 2016 The Fuchsia Authors
// Copyright (c) 2014 Google Inc. All rights reserved
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_ARCH_ARM64_INCLUDE_ARCH_ARM64_MMU_H_
#define ZIRCON_KERNEL_ARCH_ARM64_INCLUDE_ARCH_ARM64_MMU_H_

#include <arch/defines.h>
#include <arch/kernel_aspace.h>

// clang-format off
#define IFTE(c,t,e) (!!(c) * (t) | !(c) * (e))
#define NBITS01(n)      IFTE(n, 1, 0)
#define NBITS02(n)      IFTE((n) >>  1,  1 + NBITS01((n) >>  1), NBITS01(n))
#define NBITS04(n)      IFTE((n) >>  2,  2 + NBITS02((n) >>  2), NBITS02(n))
#define NBITS08(n)      IFTE((n) >>  4,  4 + NBITS04((n) >>  4), NBITS04(n))
#define NBITS16(n)      IFTE((n) >>  8,  8 + NBITS08((n) >>  8), NBITS08(n))
#define NBITS32(n)      IFTE((n) >> 16, 16 + NBITS16((n) >> 16), NBITS16(n))
#define NBITS(n)        IFTE((n) >> 32, 32 + NBITS32((n) >> 32), NBITS32(n))

#ifndef MMU_KERNEL_SIZE_SHIFT
#define KERNEL_ASPACE_BITS (NBITS(0xffffffffffffffff-KERNEL_ASPACE_BASE))

#if KERNEL_ASPACE_BITS < 25
#define MMU_KERNEL_SIZE_SHIFT (25)
#else
#define MMU_KERNEL_SIZE_SHIFT (KERNEL_ASPACE_BITS)
#endif
#endif

#ifndef MMU_USER_SIZE_SHIFT
#define MMU_USER_SIZE_SHIFT 48
#endif

#ifndef MMU_USER_RESTRICTED_SIZE_SHIFT
#define MMU_USER_RESTRICTED_SIZE_SHIFT (MMU_USER_SIZE_SHIFT - 1)
#endif

// See ARM DDI 0487B.b, Table D4-25 for the maximum IPA range that can be used.
// This size is based on a 4KB granule and a starting level of 1. We chose this
// size due to the 40-bit physical address range on Cortex-A53.
#define MMU_GUEST_SIZE_SHIFT 36

#define MMU_MAX_PAGE_SIZE_SHIFT 48

#define MMU_KERNEL_PAGE_SIZE_SHIFT      (PAGE_SIZE_SHIFT)
#define MMU_USER_PAGE_SIZE_SHIFT        (USER_PAGE_SIZE_SHIFT)
#define MMU_GUEST_PAGE_SIZE_SHIFT       (USER_PAGE_SIZE_SHIFT)

// The identity map handed off from physboot uses 4KiB-paging with a maximum
// virtual address width of 48 bits.
//
// LINT.IfChange
#define MMU_IDENT_SIZE_SHIFT 48
#define MMU_IDENT_PAGE_SIZE_SHIFT (SHIFT_4K)
// LINT.ThenChange(/zircon/kernel/arch/arm64/phys/include/phys/arch/address-space.h)

// TCR TGx values
//
// Page size:   4K      16K     64K
// TG0:         0       2       1
// TG1:         2       1       3

#define MMU_TG0(page_size_shift) ((((page_size_shift == 14) & 1) << 1) | \
                                  ((page_size_shift == 16) & 1))

#define MMU_TG1(page_size_shift) ((((page_size_shift == 12) & 1) << 1) | \
                                  ((page_size_shift == 14) & 1) | \
                                  ((page_size_shift == 16) & 1) | \
                                  (((page_size_shift == 16) & 1) << 1))

#define MMU_LX_X(page_shift, level) ((4 - (level)) * ((page_shift) - 3) + 3)

#if MMU_USER_SIZE_SHIFT > MMU_LX_X(MMU_USER_PAGE_SIZE_SHIFT, 0)
#define MMU_USER_TOP_SHIFT MMU_LX_X(MMU_USER_PAGE_SIZE_SHIFT, 0)
#elif MMU_USER_SIZE_SHIFT > MMU_LX_X(MMU_USER_PAGE_SIZE_SHIFT, 1)
#define MMU_USER_TOP_SHIFT MMU_LX_X(MMU_USER_PAGE_SIZE_SHIFT, 1)
#elif MMU_USER_SIZE_SHIFT > MMU_LX_X(MMU_USER_PAGE_SIZE_SHIFT, 2)
#define MMU_USER_TOP_SHIFT MMU_LX_X(MMU_USER_PAGE_SIZE_SHIFT, 2)
#elif MMU_USER_SIZE_SHIFT > MMU_LX_X(MMU_USER_PAGE_SIZE_SHIFT, 3)
#define MMU_USER_TOP_SHIFT MMU_LX_X(MMU_USER_PAGE_SIZE_SHIFT, 3)
#else
#error User address space size must be larger than page size
#endif
#define MMU_USER_PAGE_TABLE_ENTRIES_TOP (0x1 << (MMU_USER_SIZE_SHIFT - MMU_USER_TOP_SHIFT))
#define MMU_USER_PAGE_TABLE_ENTRIES (0x1 << (MMU_USER_PAGE_SIZE_SHIFT - 3))

#if MMU_KERNEL_SIZE_SHIFT > MMU_LX_X(MMU_KERNEL_PAGE_SIZE_SHIFT, 0)
#define MMU_KERNEL_TOP_SHIFT MMU_LX_X(MMU_KERNEL_PAGE_SIZE_SHIFT, 0)
#elif MMU_KERNEL_SIZE_SHIFT > MMU_LX_X(MMU_KERNEL_PAGE_SIZE_SHIFT, 1)
#define MMU_KERNEL_TOP_SHIFT MMU_LX_X(MMU_KERNEL_PAGE_SIZE_SHIFT, 1)
#elif MMU_KERNEL_SIZE_SHIFT > MMU_LX_X(MMU_KERNEL_PAGE_SIZE_SHIFT, 2)
#define MMU_KERNEL_TOP_SHIFT MMU_LX_X(MMU_KERNEL_PAGE_SIZE_SHIFT, 2)
#elif MMU_KERNEL_SIZE_SHIFT > MMU_LX_X(MMU_KERNEL_PAGE_SIZE_SHIFT, 3)
#define MMU_KERNEL_TOP_SHIFT MMU_LX_X(MMU_KERNEL_PAGE_SIZE_SHIFT, 3)
#else
#error Kernel address space size must be larger than page size
#endif
#define MMU_KERNEL_PAGE_TABLE_ENTRIES_TOP (0x1 << (MMU_KERNEL_SIZE_SHIFT - MMU_KERNEL_TOP_SHIFT))
#define MMU_KERNEL_PAGE_TABLE_ENTRIES (0x1 << (MMU_KERNEL_PAGE_SIZE_SHIFT - 3))

#if MMU_IDENT_SIZE_SHIFT > MMU_LX_X(MMU_IDENT_PAGE_SIZE_SHIFT, 0)
#define MMU_IDENT_TOP_SHIFT MMU_LX_X(MMU_IDENT_PAGE_SIZE_SHIFT, 0)
#elif MMU_IDENT_SIZE_SHIFT > MMU_LX_X(MMU_IDENT_PAGE_SIZE_SHIFT, 1)
#define MMU_IDENT_TOP_SHIFT MMU_LX_X(MMU_IDENT_PAGE_SIZE_SHIFT, 1)
#elif MMU_IDENT_SIZE_SHIFT > MMU_LX_X(MMU_IDENT_PAGE_SIZE_SHIFT, 2)
#define MMU_IDENT_TOP_SHIFT MMU_LX_X(MMU_IDENT_PAGE_SIZE_SHIFT, 2)
#elif MMU_IDENT_SIZE_SHIFT > MMU_LX_X(MMU_IDENT_PAGE_SIZE_SHIFT, 3)
#define MMU_IDENT_TOP_SHIFT MMU_LX_X(MMU_IDENT_PAGE_SIZE_SHIFT, 3)
#else
#error Ident address space size must be larger than page size
#endif
#define MMU_IDENT_PAGE_TABLE_ENTRIES_TOP_SHIFT (MMU_IDENT_SIZE_SHIFT - MMU_IDENT_TOP_SHIFT)
#define MMU_IDENT_PAGE_TABLE_ENTRIES_TOP (0x1 << MMU_IDENT_PAGE_TABLE_ENTRIES_TOP_SHIFT)
#define MMU_IDENT_PAGE_TABLE_ENTRIES (0x1 << (MMU_IDENT_PAGE_SIZE_SHIFT - 3))

#if MMU_GUEST_SIZE_SHIFT > MMU_LX_X(MMU_GUEST_PAGE_SIZE_SHIFT, 0)
#define MMU_GUEST_TOP_SHIFT MMU_LX_X(MMU_GUEST_PAGE_SIZE_SHIFT, 0)
#elif MMU_GUEST_SIZE_SHIFT > MMU_LX_X(MMU_GUEST_PAGE_SIZE_SHIFT, 1)
#define MMU_GUEST_TOP_SHIFT MMU_LX_X(MMU_GUEST_PAGE_SIZE_SHIFT, 1)
#elif MMU_GUEST_SIZE_SHIFT > MMU_LX_X(MMU_GUEST_PAGE_SIZE_SHIFT, 2)
#define MMU_GUEST_TOP_SHIFT MMU_LX_X(MMU_GUEST_PAGE_SIZE_SHIFT, 2)
#elif MMU_GUEST_SIZE_SHIFT > MMU_LX_X(MMU_GUEST_PAGE_SIZE_SHIFT, 3)
#define MMU_GUEST_TOP_SHIFT MMU_LX_X(MMU_GUEST_PAGE_SIZE_SHIFT, 3)
#else
#error Guest physical address space size must be larger than page size
#endif
#define MMU_GUEST_PAGE_TABLE_ENTRIES_TOP (0x1 << (MMU_GUEST_SIZE_SHIFT - MMU_GUEST_TOP_SHIFT))
#define MMU_GUEST_PAGE_TABLE_ENTRIES (0x1 << (MMU_GUEST_PAGE_SIZE_SHIFT - 3))

#define MMU_PTE_DESCRIPTOR_BLOCK_MAX_SHIFT      (30)

#ifndef __ASSEMBLER__
#define BM(base, count, val) (((val) & ((1UL << (count)) - 1)) << (base))
#else
#define BM(base, count, val) (((val) & ((0x1 << (count)) - 1)) << (base))
#endif

#define MMU_SH_NON_SHAREABLE                    (0)
#define MMU_SH_OUTER_SHAREABLE                  (2)
#define MMU_SH_INNER_SHAREABLE                  (3)

#define MMU_RGN_NON_CACHEABLE                   (0)
#define MMU_RGN_WRITE_BACK_ALLOCATE             (1)
#define MMU_RGN_WRITE_THROUGH_NO_ALLOCATE       (2)
#define MMU_RGN_WRITE_BACK_NO_ALLOCATE          (3)

#define MMU_TCR_TBI1                            BM(38, 1, 1)
#define MMU_TCR_TBI0                            BM(37, 1, 1)
#define MMU_TCR_AS                              BM(36, 1, 1)
#define MMU_TCR_IPS(size)                       BM(32, 3, (size))
#define MMU_TCR_TG1(granule_size)               BM(30, 2, (granule_size))
#define MMU_TCR_SH1(shareability_flags)         BM(28, 2, (shareability_flags))
#define MMU_TCR_ORGN1(cache_flags)              BM(26, 2, (cache_flags))
#define MMU_TCR_IRGN1(cache_flags)              BM(24, 2, (cache_flags))
#define MMU_TCR_EPD1                            BM(23, 1, 1)
#define MMU_TCR_A1                              BM(22, 1, 1)
#define MMU_TCR_T1SZ(size)                      BM(16, 6, (size))
#define MMU_TCR_TG0(granule_size)               BM(14, 2, (granule_size))
#define MMU_TCR_SH0(shareability_flags)         BM(12, 2, (shareability_flags))
#define MMU_TCR_ORGN0(cache_flags)              BM(10, 2, (cache_flags))
#define MMU_TCR_IRGN0(cache_flags)              BM( 8, 2, (cache_flags))
#define MMU_TCR_EPD0                            BM( 7, 1, 1)
#define MMU_TCR_T0SZ(size)                      BM( 0, 6, (size))

#define MMU_TCR_EL2_RES1                        (BM(31, 1, 1) | BM(23, 1, 1))

#define MMU_VTCR_EL2_RES1                       BM(31, 1, 1)
#define MMU_VTCR_EL2_SL0(starting_level)        BM( 6, 2, (starting_level))

#define MMU_MAIR_ATTR(index, attr)              BM(index * 8, 8, (attr))

// Bit 0 is valid, also part of the descriptor field
#define MMU_PTE_VALID                           BM(0, 1, 1)

// L0/L1/L2/L3 descriptor types
#define MMU_PTE_DESCRIPTOR_INVALID              BM(0, 2, 0)
#define MMU_PTE_DESCRIPTOR_MASK                 BM(0, 2, 3)

// L0/L1/L2 descriptor types
#define MMU_PTE_L012_DESCRIPTOR_BLOCK           BM(0, 2, 1)
#define MMU_PTE_L012_DESCRIPTOR_TABLE           BM(0, 2, 3)

// L3 descriptor types
#define MMU_PTE_L3_DESCRIPTOR_PAGE              BM(0, 2, 3)

// Output address mask
#define MMU_PTE_OUTPUT_ADDR_MASK                BM(12, 36, 0xfffffffff)

// Table attrs
#define MMU_PTE_ATTR_NS_TABLE                   BM(63, 1, 1)
#define MMU_PTE_ATTR_AP_TABLE_NO_WRITE          BM(62, 1, 1)
#define MMU_PTE_ATTR_AP_TABLE_NO_EL0            BM(61, 1, 1)
#define MMU_PTE_ATTR_UXN_TABLE                  BM(60, 1, 1)
#define MMU_PTE_ATTR_PXN_TABLE                  BM(59, 1, 1)

// Block/Page attrs
#define MMU_PTE_ATTR_RES_SOFTWARE               BM(55, 4, 0xf)
#define MMU_PTE_ATTR_XN                         BM(54, 1, 1) // for single translation regimes
#define MMU_PTE_ATTR_UXN                        BM(54, 1, 1)
#define MMU_PTE_ATTR_PXN                        BM(53, 1, 1)
#define MMU_PTE_ATTR_CONTIGUOUS                 BM(52, 1, 1)

#define MMU_PTE_ATTR_NON_GLOBAL                 BM(11, 1, 1)
#define MMU_PTE_ATTR_AF                         BM(10, 1, 1)

#define MMU_PTE_ATTR_SH_NON_SHAREABLE           BM(8, 2, 0)
#define MMU_PTE_ATTR_SH_OUTER_SHAREABLE         BM(8, 2, 2)
#define MMU_PTE_ATTR_SH_INNER_SHAREABLE         BM(8, 2, 3)

#define MMU_PTE_ATTR_AP_P_RW_U_NA               BM(6, 2, 0)
#define MMU_PTE_ATTR_AP_P_RW_U_RW               BM(6, 2, 1)
#define MMU_PTE_ATTR_AP_P_RO_U_NA               BM(6, 2, 2)
#define MMU_PTE_ATTR_AP_P_RO_U_RO               BM(6, 2, 3)
#define MMU_PTE_ATTR_AP_MASK                    BM(6, 2, 3)

#define MMU_PTE_ATTR_NON_SECURE                 BM(5, 1, 1)

#define MMU_PTE_ATTR_ATTR_INDEX(attrindex)      BM(2, 3, attrindex)
#define MMU_PTE_ATTR_ATTR_INDEX_MASK            MMU_PTE_ATTR_ATTR_INDEX(7)

#define MMU_PTE_PERMISSION_MASK                 (MMU_PTE_ATTR_AP_MASK | \
                                                 MMU_PTE_ATTR_UXN | \
                                                 MMU_PTE_ATTR_PXN)

#define MMU_S2_PTE_ATTR_XN                      BM(53, 2, 2)

#define MMU_S2_PTE_ATTR_S2AP_RO                 BM(6, 2, 1)
#define MMU_S2_PTE_ATTR_S2AP_RW                 BM(6, 2, 3)

#define MMU_S2_PTE_ATTR_ATTR_INDEX_MASK         BM(2, 4, 0xf)
// Normal, Outer Write-Back Cacheable, Inner Write-Back Cacheable.
#define MMU_S2_PTE_ATTR_NORMAL_MEMORY           BM(2, 4, 0xf)
// Normal, Outer Non-cacheable, Inner Non-cacheable.
#define MMU_S2_PTE_ATTR_NORMAL_UNCACHED         BM(2, 4, 0x5)
// Device, Device-nGnRnE memory.
#define MMU_S2_PTE_ATTR_STRONGLY_ORDERED        BM(2, 4, 0x0)
// Device, Device-nGnRE memory.
#define MMU_S2_PTE_ATTR_DEVICE                  BM(2, 4, 0x1)

// The following four attributes and indices must be kept in sync with those
// used for the boot CPU in physboot.
//
// LINT.IfChange

// Device-nGnRnE memory
#define MMU_MAIR_ATTR0                  MMU_MAIR_ATTR(0, 0x00)
#define MMU_PTE_ATTR_STRONGLY_ORDERED   MMU_PTE_ATTR_ATTR_INDEX(0)

// Device-nGnRE memory
#define MMU_MAIR_ATTR1                  MMU_MAIR_ATTR(1, 0x04)
#define MMU_PTE_ATTR_DEVICE             MMU_PTE_ATTR_ATTR_INDEX(1)

// Normal Memory, Outer Write-back non-transient Read/Write allocate,
// Inner Write-back non-transient Read/Write allocate
//
#define MMU_MAIR_ATTR2                  MMU_MAIR_ATTR(2, 0xff)
#define MMU_PTE_ATTR_NORMAL_MEMORY      MMU_PTE_ATTR_ATTR_INDEX(2)

// Normal Memory, Inner/Outer uncached, Write Combined
#define MMU_MAIR_ATTR3                  MMU_MAIR_ATTR(3, 0x44)
#define MMU_PTE_ATTR_NORMAL_UNCACHED    MMU_PTE_ATTR_ATTR_INDEX(3)

// LINT.ThenChange(/zircon/kernel/arch/arm64/phys/address-space.cc)

#define MMU_MAIR_ATTR4                  (0)
#define MMU_MAIR_ATTR5                  (0)
#define MMU_MAIR_ATTR6                  (0)
#define MMU_MAIR_ATTR7                  (0)

#define MMU_MAIR_VAL                    (MMU_MAIR_ATTR0 | MMU_MAIR_ATTR1 | \
                                         MMU_MAIR_ATTR2 | MMU_MAIR_ATTR3 | \
                                         MMU_MAIR_ATTR4 | MMU_MAIR_ATTR5 | \
                                         MMU_MAIR_ATTR6 | MMU_MAIR_ATTR7 )

// TODO: read at runtime, or configure per platform
#define MMU_TCR_IPS_DEFAULT MMU_TCR_IPS(2) // 40 bits

// Enable cached page table walks:
// inner/outer (IRGN/ORGN): write-back + write-allocate
#define MMU_TCR_FLAGS1 (MMU_TCR_TG1(MMU_TG1(MMU_KERNEL_PAGE_SIZE_SHIFT)) | \
                        MMU_TCR_SH1(MMU_SH_INNER_SHAREABLE) | \
                        MMU_TCR_ORGN1(MMU_RGN_WRITE_BACK_ALLOCATE) | \
                        MMU_TCR_IRGN1(MMU_RGN_WRITE_BACK_ALLOCATE) | \
                        MMU_TCR_T1SZ(64 - MMU_KERNEL_SIZE_SHIFT))
#define MMU_TCR_FLAGS0 (MMU_TCR_TG0(MMU_TG0(MMU_USER_PAGE_SIZE_SHIFT)) | \
                        MMU_TCR_SH0(MMU_SH_INNER_SHAREABLE) | \
                        MMU_TCR_ORGN0(MMU_RGN_WRITE_BACK_ALLOCATE) | \
                        MMU_TCR_IRGN0(MMU_RGN_WRITE_BACK_ALLOCATE) | \
                        MMU_TCR_T0SZ(64 - MMU_USER_SIZE_SHIFT))
#define MMU_TCR_FLAGS0_RESTRICTED \
                       (MMU_TCR_TG0(MMU_TG0(MMU_USER_PAGE_SIZE_SHIFT)) | \
                        MMU_TCR_SH0(MMU_SH_INNER_SHAREABLE) | \
                        MMU_TCR_ORGN0(MMU_RGN_WRITE_BACK_ALLOCATE) | \
                        MMU_TCR_IRGN0(MMU_RGN_WRITE_BACK_ALLOCATE) | \
                        MMU_TCR_T0SZ(64 - MMU_USER_RESTRICTED_SIZE_SHIFT))
#define MMU_TCR_FLAGS0_IDENT \
                       (MMU_TCR_TG0(MMU_TG0(MMU_IDENT_PAGE_SIZE_SHIFT)) | \
                        MMU_TCR_SH0(MMU_SH_INNER_SHAREABLE) | \
                        MMU_TCR_ORGN0(MMU_RGN_WRITE_BACK_ALLOCATE) | \
                        MMU_TCR_IRGN0(MMU_RGN_WRITE_BACK_ALLOCATE) | \
                        MMU_TCR_T0SZ(64 - MMU_IDENT_SIZE_SHIFT))

// TCR while using the boot trampoline.
// Both TTBRs active, ASID set to kernel, ident page granule selected for user half.
#define MMU_TCR_FLAGS_IDENT (MMU_TCR_IPS_DEFAULT | \
                        MMU_TCR_FLAGS1 | \
                        MMU_TCR_FLAGS0_IDENT | \
                        MMU_TCR_AS | \
                        MMU_TCR_A1)

// TCR while a kernel only (no user address space) thread is active.
// User page walks disabled, ASID set to kernel.
#define MMU_TCR_FLAGS_KERNEL (MMU_TCR_IPS_DEFAULT | \
                              MMU_TCR_FLAGS1 | \
                              MMU_TCR_FLAGS0 | \
                              MMU_TCR_EPD0 | \
                              MMU_TCR_AS | \
                              MMU_TCR_A1)

// TCR while a user mode thread is active in user or kernel space.
// Both TTBrs active, ASID set to user.
#define MMU_TCR_FLAGS_USER (MMU_TCR_IPS_DEFAULT | \
                            MMU_TCR_FLAGS1 | \
                            MMU_TCR_FLAGS0 | \
                            MMU_TCR_TBI0 | \
                            MMU_TCR_AS)

// TCR while a user mode thread is active in restricted mode.
#define MMU_TCR_FLAGS_USER_RESTRICTED (MMU_TCR_IPS_DEFAULT | \
                            MMU_TCR_FLAGS1 | \
                            MMU_TCR_FLAGS0_RESTRICTED | \
                            MMU_TCR_TBI0 | \
                            MMU_TCR_AS)

#define MMU_VTCR_FLAGS_GUEST \
                       (MMU_TCR_TG0(MMU_TG0(MMU_GUEST_PAGE_SIZE_SHIFT)) | \
                        MMU_TCR_SH0(MMU_SH_INNER_SHAREABLE) | \
                        MMU_TCR_ORGN0(MMU_RGN_WRITE_BACK_ALLOCATE) | \
                        MMU_TCR_IRGN0(MMU_RGN_WRITE_BACK_ALLOCATE) | \
                        MMU_TCR_T0SZ(64 - MMU_GUEST_SIZE_SHIFT))

#define MMU_TCR_EL2_FLAGS (MMU_TCR_EL2_RES1 | MMU_TCR_FLAGS0_IDENT)

// See ARM DDI 0487B.b, Table D4-7 for details on how to configure SL0. We chose
// a starting level of 1, due to our use of a 4KB granule and a 40-bit PARange.
#define MMU_VTCR_EL2_SL0_DEFAULT MMU_VTCR_EL2_SL0(1)

// NOTE(abdulla): VTCR_EL2.PS still must be set, based upon ID_AA64MMFR0_EL1.
// Furthermore, this only covers what's required by ARMv8.0.
#define MMU_VTCR_EL2_FLAGS (MMU_VTCR_EL2_RES1 | MMU_VTCR_EL2_SL0_DEFAULT | MMU_VTCR_FLAGS_GUEST)

#define MMU_PTE_KERNEL_RWX_FLAGS \
    (MMU_PTE_ATTR_UXN | \
     MMU_PTE_ATTR_AF | \
     MMU_PTE_ATTR_SH_INNER_SHAREABLE | \
     MMU_PTE_ATTR_NORMAL_MEMORY | \
     MMU_PTE_ATTR_AP_P_RW_U_NA)

#define MMU_PTE_KERNEL_RO_FLAGS \
    (MMU_PTE_ATTR_UXN | \
     MMU_PTE_ATTR_PXN | \
     MMU_PTE_ATTR_AF | \
     MMU_PTE_ATTR_SH_INNER_SHAREABLE | \
     MMU_PTE_ATTR_NORMAL_MEMORY | \
     MMU_PTE_ATTR_AP_P_RO_U_NA)

#define MMU_PTE_KERNEL_DATA_FLAGS \
    (MMU_PTE_ATTR_UXN | \
     MMU_PTE_ATTR_PXN | \
     MMU_PTE_ATTR_AF | \
     MMU_PTE_ATTR_SH_INNER_SHAREABLE | \
     MMU_PTE_ATTR_NORMAL_MEMORY | \
     MMU_PTE_ATTR_AP_P_RW_U_NA)

#define MMU_INITIAL_MAP_STRONGLY_ORDERED \
    (MMU_PTE_ATTR_UXN | \
     MMU_PTE_ATTR_PXN | \
     MMU_PTE_ATTR_AF | \
     MMU_PTE_ATTR_STRONGLY_ORDERED | \
     MMU_PTE_ATTR_AP_P_RW_U_NA)

#define MMU_INITIAL_MAP_DEVICE \
    (MMU_PTE_ATTR_UXN | \
     MMU_PTE_ATTR_PXN | \
     MMU_PTE_ATTR_AF | \
     MMU_PTE_ATTR_DEVICE | \
     MMU_PTE_ATTR_AP_P_RW_U_NA)

#if MMU_IDENT_SIZE_SHIFT > MMU_LX_X(MMU_IDENT_PAGE_SIZE_SHIFT, 2)
#define MMU_PTE_IDENT_DESCRIPTOR MMU_PTE_L012_DESCRIPTOR_BLOCK
#else
#define MMU_PTE_IDENT_DESCRIPTOR MMU_PTE_L3_DESCRIPTOR_PAGE
#endif
#define MMU_PTE_IDENT_FLAGS \
    (MMU_PTE_IDENT_DESCRIPTOR | \
     MMU_PTE_KERNEL_RWX_FLAGS)
#define MMU_PTE_IDENT_DEVICE_FLAGS \
    (MMU_PTE_IDENT_DESCRIPTOR | \
     MMU_INITIAL_MAP_DEVICE)

/* TLBI VADDR mask, VA[55:12], bits [43:0] */
#define TLBI_VADDR_MASK                BM(0, 44, 0xfffffffffff)

// clang-format on

#ifndef __ASSEMBLER__

#include <assert.h>
#include <sys/types.h>
#include <zircon/compiler.h>

#include <arch/arm64.h>
#include <ktl/tuple.h>

using pte_t = uint64_t;

#define ARM64_TLBI_NOADDR(op)        \
  ({                                 \
    __asm__ volatile("tlbi " #op::); \
    __isb(ARM_MB_SY);                \
  })

#define ARM64_TLBI(op, val)                                      \
  ({                                                             \
    __asm__ volatile("tlbi " #op ", %0" ::"r"((uint64_t)(val))); \
    __isb(ARM_MB_SY);                                            \
  })

#define ARM64_TLBI_ASID(op, val)                         \
  ({                                                     \
    uint64_t v = val;                                    \
    __asm__ volatile("tlbi " #op ", %0" ::"r"(v << 48)); \
    __isb(ARM_MB_SY);                                    \
  })

// dedicated address space ids
const uint16_t MMU_ARM64_UNUSED_ASID = 0;
const uint16_t MMU_ARM64_GLOBAL_ASID = 1;  // NOTE: keep in sync with start.S
const uint16_t MMU_ARM64_FIRST_USER_ASID = 2;
// User asids should not overlap with the unused or global asid sentinels.
static_assert(MMU_ARM64_FIRST_USER_ASID > MMU_ARM64_UNUSED_ASID);
static_assert(MMU_ARM64_FIRST_USER_ASID > MMU_ARM64_GLOBAL_ASID);

// Assert that a restricted address space covers exactly half of a top level
// page table. This also indirectly verifies that `MMU_TCR_FLAGS0_RESTRICTED`
// contains the right value of T0SZ.
static_assert(((USER_ASPACE_BASE + USER_RESTRICTED_ASPACE_SIZE) &
               ~(1UL << MMU_USER_RESTRICTED_SIZE_SHIFT)) == 0);

// max address space id for 8 and 16 bit asid ranges
const uint16_t MMU_ARM64_MAX_USER_ASID_8 = (1u << 8) - 1;
const uint16_t MMU_ARM64_MAX_USER_ASID_16 = (1u << 16) - 1;

// Physical addresses of the kernel(/upper) and lower root page tables,
// saved in start.S.
extern paddr_t root_kernel_page_table_phys;
extern paddr_t root_lower_page_table_phys;

// use built-in virtual to physical translation instructions to query
// the physical address of a virtual address
zx_status_t arm64_mmu_translate(vaddr_t va, paddr_t* pa, bool user, bool write);

void arm64_mmu_early_init();

#endif  // __ASSEMBLER__

#endif  // ZIRCON_KERNEL_ARCH_ARM64_INCLUDE_ARCH_ARM64_MMU_H_
