// Copyright 2016 The Fuchsia Authors
// Copyright (c) 2008 Travis Geiselbrecht
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_ARCH_ARM64_INCLUDE_ARCH_DEFINES_H_
#define ZIRCON_KERNEL_ARCH_ARM64_INCLUDE_ARCH_DEFINES_H_

#define SHIFT_4K (12)
#define SHIFT_16K (14)
#define SHIFT_64K (16)

#ifdef ARM64_LARGE_PAGESIZE_64K
#define PAGE_SIZE_SHIFT (SHIFT_64K)
#elif ARM64_LARGE_PAGESIZE_16K
#define PAGE_SIZE_SHIFT (SHIFT_16K)
#else
#define PAGE_SIZE_SHIFT (SHIFT_4K)
#endif
#define USER_PAGE_SIZE_SHIFT SHIFT_4K

#ifndef PAGE_SIZE
#define PAGE_SIZE (1UL << PAGE_SIZE_SHIFT)
#else
static_assert(PAGE_SIZE == (1UL << PAGE_SIZE_SHIFT), "Page size mismatch!");
#endif

#define PAGE_MASK (PAGE_SIZE - 1)

#define USER_PAGE_SIZE (1UL << USER_PAGE_SIZE_SHIFT)
#define USER_PAGE_MASK (USER_PAGE_SIZE - 1)

// Align the heap to 2MiB to optionally support large page mappings in it.
#define ARCH_HEAP_ALIGN_BITS 21

// The maximum cache line seen on any known ARM hardware.
#define MAX_CACHE_LINE 64

// Map 512GB at the base of the kernel. this is the max that can be mapped with a
// single level 1 page table using 1GB pages.
#define ARCH_PHYSMAP_SIZE (1UL << 39)

#endif  // ZIRCON_KERNEL_ARCH_ARM64_INCLUDE_ARCH_DEFINES_H_
