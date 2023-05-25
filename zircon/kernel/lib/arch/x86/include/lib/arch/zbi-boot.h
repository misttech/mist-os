// Copyright 2021 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_LIB_ARCH_X86_INCLUDE_LIB_ARCH_ZBI_BOOT_H_
#define ZIRCON_KERNEL_LIB_ARCH_X86_INCLUDE_LIB_ARCH_ZBI_BOOT_H_

#include <lib/arch/internal/zbi-constants.h>
#include <lib/arch/zbi.h>
#include <zircon/assert.h>

#include <cstdint>

namespace arch {

constexpr uint32_t kZbiBootKernelType = ARCH_ZBI_KERNEL_TYPE;

// Alignment required for kernel ZBI passed to arch::ZbiBoot.
constexpr uintptr_t kZbiBootKernelAlignment = ARCH_ZBI_KERNEL_ALIGNMENT;

// Alignment required for data ZBI passed to arch::ZbiBoot.
constexpr uintptr_t kZbiBootDataAlignment = ARCH_ZBI_DATA_ALIGNMENT;

// Hand off to a ZBI kernel already loaded in memory.  The kernel and data ZBIs
// are already loaded at arbitrary 4K-aligned physical addresses.  This is
// called with identity mappings in place that cover at least the kernel plus
// its reserve_memory_size and the whole data ZBI.

[[noreturn]] void ZbiBootRaw(uintptr_t entry, void* data);

#ifdef __x86_64__
[[noreturn]] inline void ZbiBootRaw(uintptr_t entry, void* data) {
  // Clear the stack and frame pointers so no misleading breadcrumbs are left.
  // Use a register constraint for the indirect jump operand so that it can't
  // materialize it via %rbp or %rsp as "g" could (since the compiler doesn't
  // support using those as clobbers).
  __asm__ volatile(
      R"""(
      xor %%ebp, %%ebp
      xor %%esp, %%esp
      cld
      cli
      jmp *%0
      )"""
      :
      : "r"(entry), "S"(data)
      : "cc", "memory");
  __builtin_unreachable();
}
#endif

[[noreturn]] inline void ZbiBoot(ZbiKernelImage* kernel, void* arg) {
  auto entry = reinterpret_cast<uintptr_t>(kernel) + kernel->data_kernel.entry;
  uintptr_t raw_entry = static_cast<uintptr_t>(entry);
  ZX_ASSERT(raw_entry == entry);
  ZbiBootRaw(raw_entry, arg);
}

}  // namespace arch

#endif  // ZIRCON_KERNEL_LIB_ARCH_X86_INCLUDE_LIB_ARCH_ZBI_BOOT_H_
