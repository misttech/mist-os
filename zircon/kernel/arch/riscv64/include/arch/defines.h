// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_ARCH_RISCV64_INCLUDE_ARCH_DEFINES_H_
#define ZIRCON_KERNEL_ARCH_RISCV64_INCLUDE_ARCH_DEFINES_H_

#define PAGE_SIZE_SHIFT (12)
#define USER_PAGE_SIZE_SHIFT PAGE_SIZE_SHIFT

#define PAGE_SIZE (1L << PAGE_SIZE_SHIFT)
#define PAGE_MASK (PAGE_SIZE - 1)

#define USER_PAGE_SIZE (1L << USER_PAGE_SIZE_SHIFT)
#define USER_PAGE_MASK (USER_PAGE_SIZE - 1)

// Align the heap to 2MiB to optionally support large page mappings in it.
#define ARCH_HEAP_ALIGN_BITS 21

// Zic64b guarantees.
#define MAX_CACHE_LINE 64

#define ARCH_DEFAULT_STACK_SIZE 8192

// Map 512GB at the base of the kernel. This is the max that can be mapped with
// a single level 1 page table using 1GB pages.
#define ARCH_PHYSMAP_SIZE (1UL << 39)

#endif  // ZIRCON_KERNEL_ARCH_RISCV64_INCLUDE_ARCH_DEFINES_H_
