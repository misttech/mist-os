// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <lib/arch/arm64/system.h>
#include <lib/arch/cache.h>

#include <phys/boot-zbi.h>

// TODO(https://fxbug.dev/408020980): Remove gnu::weak when we delete the
// TrampolineBoot-specific override.
[[gnu::weak]] void BootZbi::ZbiBoot(uintptr_t entry, void* data) const {
  // The ZBI protocol requires that any data cache lines backing the kernel
  // memory image are cleaned (that is, including the "reserve" memory)
  // allowing the image to be present for its own direct memory access.
  arch::CleanDataCacheRange(static_cast<uintptr_t>(KernelLoadAddress()), KernelMemorySize());

  // The ZBI protocol requires that any instruction cache lines backing the
  // kernel load image are invalidated. We can exclude any "reserve" memory in
  // this as we don't expect that to contain code (and if it does, then
  // coherence around that range should be managed by the kernel itself).
  //
  // While we could more simply issue `ic iallu` to invalidate the whole
  // instruction cache in a single instruction, we choose not to. This is a
  // reference implementation of the ZBI protocol and we want platform
  // exercise of out ZBI kernel code in the context of a minimally compliant
  // bootloader.
  arch::InvalidateInstructionCacheRange(static_cast<uintptr_t>(KernelLoadAddress()),
                                        KernelLoadSize());

  // If this code is running in the context of a ZBI or Linux kernel (e.g., a
  // boot shim), then this isn't strictly necessary as we have protocol
  // guarantees that we were loaded with this instruction memory clean (and
  // we weren't likely to have modified ourselves). But better to be maximally
  // defensive when it comes to cache coherency.
  {
    uint64_t start, size;
    __asm__ volatile(
        "adr %[start], .L.ZbiBoot.mmu_possibly_off\n"
        "mov %[size], #.L.ZbiBoot.end - .L.ZbiBoot.mmu_possibly_off"
        : [start] "=r"(start), [size] "=r"(size));
    arch::CleanDataCacheRange(start, size);
  }

  uint64_t is_el1 = arch::ArmCurrentEl::Read().el() == 1 ? 1 : 0;
  uint64_t sctlr;  // Disable the MMU, and the instruction and data caches.
  if (is_el1) {
    sctlr = arch::ArmSctlrEl1::Read().set_m(false).set_i(false).set_c(false).reg_value();
  } else {
    sctlr = arch::ArmSctlrEl2::Read().set_m(false).set_i(false).set_c(false).reg_value();
  }
  uint64_t tmp;
  __asm__ volatile(
      R"""(
  cbnz  %[is_el1], .L.ZbiBoot.disable_el1
.L.ZbiBoot.disable_el2:
  msr   sctlr_el2, %[sctlr]
.L.ZbiBoot.mmu_possibly_off:
  b  .L.ZbiBoot.mmu_off
.L.ZbiBoot.disable_el1:
  msr  sctlr_el1, %[sctlr]
.L.ZbiBoot.mmu_off:
  isb

  // Clear the stack and frame pointers and the link register so no misleading
  // breadcrumbs are left.
  mov x29, xzr
  mov x30, xzr
  mov sp, x29

  mov x0, %[zbi]
  br %[entry]
.L.ZbiBoot.end:
      )"""
      : [tmp] "=&r"(tmp)       //
      : [entry] "r"(entry),    //
        [is_el1] "r"(is_el1),  //
        [sctlr] "r"(sctlr),    //
        [zbi] "r"(data)        //
      // The compiler gets unhappy if x29 (fp) is a clobber.  It's never going
      // to be the register used for %[entry] anyway.  The memory clobber is
      // probably unnecessary, but it expresses that this constitutes access to
      // the memory kernel and zbi point to.
      : "x0", "x30", "memory");
  __builtin_unreachable();
}
