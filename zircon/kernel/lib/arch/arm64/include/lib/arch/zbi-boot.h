// Copyright 2021 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_LIB_ARCH_ARM64_INCLUDE_LIB_ARCH_ZBI_BOOT_H_
#define ZIRCON_KERNEL_LIB_ARCH_ARM64_INCLUDE_LIB_ARCH_ZBI_BOOT_H_

#include <lib/arch/cache.h>
#include <lib/arch/internal/zbi-constants.h>
#include <lib/arch/zbi.h>

#include <cstdint>

namespace arch {

constexpr uint32_t kZbiBootKernelType = ARCH_ZBI_KERNEL_TYPE;

// Alignment required for kernel ZBI passed to arch::ZbiBoot.
constexpr uintptr_t kZbiBootKernelAlignment = ARCH_ZBI_KERNEL_ALIGNMENT;

// Alignment required for data ZBI passed to arch::ZbiBoot.
constexpr uintptr_t kZbiBootDataAlignment = ARCH_ZBI_DATA_ALIGNMENT;

[[noreturn]] inline void ZbiBootRaw(uintptr_t entry, void* data) {
  DisableLocalCachesAndMmu();
  // Clear the stack and frame pointers and the link register so no misleading
  // breadcrumbs are left.
  __asm__ volatile(
      R"""(
      mov x0, %[zbi]
      mov x29, xzr
      mov x30, xzr
      mov sp, x29
      br %[entry]
      )"""
      :
      : [entry] "r"(entry), [zbi] "r"(data)
      // The compiler gets unhappy if x29 (fp) is a clobber.  It's never going
      // to be the register used for %[entry] anyway.  The memory clobber is
      // probably unnecessary, but it expresses that this constitutes access to
      // the memory kernel and zbi point to.
      : "x0", "x30", "memory");
  __builtin_unreachable();
}

// Hand off to a ZBI kernel already loaded in memory.  The kernel and data ZBIs
// are already loaded at arbitrary physical addresses.  The kernel's address
// must be aligned to 64K and the data ZBI to 4K, as per the ZBI spec.  This
// can be called in physical address mode or with identity mapping that covers
// at least the kernel plus its reserve_memory_size and the whole data ZBI.
[[noreturn]] inline void ZbiBoot(ZbiKernelImage* kernel, void* arg) {
  auto entry = reinterpret_cast<uintptr_t>(kernel) + kernel->data_kernel.entry;
  uintptr_t raw_entry = static_cast<uintptr_t>(entry);
  ZX_ASSERT(raw_entry == entry);
  ZbiBootRaw(raw_entry, arg);
}

}  // namespace arch

#endif  // ZIRCON_KERNEL_LIB_ARCH_ARM64_INCLUDE_LIB_ARCH_ZBI_BOOT_H_
