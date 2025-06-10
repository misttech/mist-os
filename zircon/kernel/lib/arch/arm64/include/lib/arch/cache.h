// Copyright 2021 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_LIB_ARCH_ARM64_INCLUDE_LIB_ARCH_CACHE_H_
#define ZIRCON_KERNEL_LIB_ARCH_ARM64_INCLUDE_LIB_ARCH_CACHE_H_

#ifdef __ASSEMBLER__
#include <lib/arch/arm64/system-asm.h>
#include <lib/arch/asm.h>
#include <lib/arch/internal/cache_loop.h>
#else
#include <lib/arch/arm64/cache.h>
#include <lib/arch/intrin.h>

#include <cstddef>
#include <cstdint>
#endif

#ifdef __ASSEMBLER__
// clang-format off

// Performs the provided cache operation across a provided virtual address
// range, where x0 holds the base address of the range and x1 its size. This
// routine can be instantiated for both data and instruction cache operations.
//
// Clobbers x2-x5.
.macro cache_range_op, cache op ctr_shift ctr_width
  mrs x4, ctr_el0
  ubfx x4, x4, #\ctr_shift, #\ctr_width

  // Cache line size fields carry the log2 of the number of words (in the
  // minimum possible size). Shift left by word size to recover the line size
  // in bytes.
  mov x2, #4
  lsl x4, x2, x4

  add  x2, x0, x1  // End address
  sub  x5, x4, #1  // Line size mask
  bic  x3, x0, x5  // Align the start address by applying the inverse mask

.Lcache_range_op_loop\@:
  \cache  \op, x3
  add  x3, x3, x4
  cmp  x3, x2
  blo  .Lcache_range_op_loop\@
  dsb  sy
.endm

// Applies the data cache operation `op` to the provided address range, where
// x0 holds its base address and x1 its size.
//
// Clobbers x2-x5.
.macro data_cache_range_op op
  cache_range_op dc \op CTR_EL0_DMIN_LINE_SHIFT CTR_EL0_DMIN_LINE_WIDTH
.endm

// Invalidates the the provided address range within the instruction cache,
// where x0 holds its base address and x1 its size.
//
// Clobbers x2-x5.
//
// There is little use in defining a similar instruction_cache_range_op,
// parameterized on \op, since ivau is the only VA instruction cache operation.
.macro instruction_cache_range_invalidate
  cache_range_op ic ivau CTR_EL0_IMIN_LINE_SHIFT CTR_EL0_IMIN_LINE_WIDTH
.endm

// Generate assembly to iterate over all ways/sets across all levels of data
// caches from level 0 to the point of coherence.
//
// "op" should be an ARM64 operation that is called on each set/way, such as
// "csw" (i.e., "Clean by Set and Way").
//
// Generated assembly does not use the stack, but clobbers registers [x0 -- x13].
.macro data_cache_way_set_op op, name
  data_cache_way_set_op_impl \op, \name
.endm

// clang-format on

#else

namespace arch {

// Ensures that the instruction and data caches are in coherence after the
// modification of provided address ranges. The caches are regarded as coherent
// - with respect to the ranges passed to SyncRange() - only after the
// associated object is destroyed.
class GlobalCacheConsistencyContext {
 public:
  // Constructs a GlobalCacheConsistencyContext with an expectation around whether
  // virtual address aliasing is possible among the address ranges to be
  // recorded.
  explicit GlobalCacheConsistencyContext(bool possible_aliasing)
      : possible_aliasing_(possible_aliasing) {}

  // Defaults to the general assumption that aliasing among the address ranges
  // to be recorded is possible if the instruction cache is VIPT.
  GlobalCacheConsistencyContext() = default;

  // Ensures consistency on destruction.
  ~GlobalCacheConsistencyContext();

  // Records a virtual address range that should factor into consistency.
  void SyncRange(uintptr_t vaddr, size_t size);

 private:
  bool possible_aliasing_ = ArmCacheTypeEl0::Read().l1_ip() == ArmL1ICachePolicy::VIPT;
};

extern "C" void CleanDataCacheRange(uintptr_t addr, size_t size);
extern "C" void CleanInvalidateDataCacheRange(uintptr_t addr, size_t size);
extern "C" void InvalidateDataCacheRange(uintptr_t addr, size_t size);

extern "C" void InvalidateInstructionCacheRange(uintptr_t addr, size_t size);

// Invalidate the entire instruction cache.
//
// Caller must perform an instruction barrier (e.g., `__isb(ARM_MB_SY)`)
// prior to relying on the operation being complete.
inline void InvalidateGlobalInstructionCache() {
  // Instruction cache: invalidate all ("iall") inner-sharable ("is") caches
  // to point of unification ("u").
  asm volatile("ic ialluis" ::: "memory");
}

// Invalidate both the instruction and data TLBs.
//
// Caller must perform an instruction barrier (e.g., `__isb(ARM_MB_SY)`)
// prior to relying on the operation being complete.
inline void InvalidateLocalTlbs() { asm volatile("tlbi vmalle1" ::: "memory"); }

// Local per-cpu cache flush routines.
//
// These clean or invalidate the data and instruction caches from the point
// of view of a single CPU to the point of coherence.
//
// These are typically only useful during system setup or shutdown when
// the MMU is not enabled. Other use-cases should use range-based cache operation.
//
// Interrupts must be disabled.  Because these routines may make use of CPU
// registers that are not saved/restored on exception, it's crucial these
// routines are not interrupted.
extern "C" void CleanLocalCaches();
extern "C" void InvalidateLocalCaches();
extern "C" void CleanAndInvalidateLocalCaches();

// Disables the local caches and MMU, ensuring that the former are flushed
// (along with the TLB).
extern "C" void DisableLocalCachesAndMmu();

}  // namespace arch

#endif  // __ASSEMBLER__

#endif  // ZIRCON_KERNEL_LIB_ARCH_ARM64_INCLUDE_LIB_ARCH_CACHE_H_
