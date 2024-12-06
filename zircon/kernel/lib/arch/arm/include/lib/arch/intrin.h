// Copyright 2024 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_LIB_ARCH_ARM_INCLUDE_LIB_ARCH_INTRIN_H_
#define ZIRCON_KERNEL_LIB_ARCH_ARM_INCLUDE_LIB_ARCH_INTRIN_H_

// Constants from ACLE section 8.3, used as the argument for __dmb(),
// __dsb(), and __isb().  Values are the architecturally defined
// immediate values encoded in barrier instructions DMB, DSB, and ISB.

#define ARM_MB_OSHLD 0x1
#define ARM_MB_OSHST 0x2
#define ARM_MB_OSH 0x3

#define ARM_MB_NSHLD 0x5
#define ARM_MB_NSHST 0x6
#define ARM_MB_NSH 0x7

#define ARM_MB_ISHLD 0x9
#define ARM_MB_ISHST 0xa
#define ARM_MB_ISH 0xb

#define ARM_MB_LD 0xd
#define ARM_MB_ST 0xe
#define ARM_MB_SY 0xf

#ifndef __ASSEMBLER__

// Provide the standard ARM C Language Extensions API.
#include <arm_acle.h>

// Provide the machine-independent <lib/arch/intrin.h> API.

#ifdef __cplusplus

namespace arch {

/// Yield the processor momentarily.  This should be used in busy waits.
inline void Yield() { __yield(); }

/// Synchronize all memory accesses of all kinds.
inline void DeviceMemoryBarrier() { __dsb(ARM_MB_SY); }

/// Synchronize the ordering of all memory accesses wrt other CPUs.
inline void ThreadMemoryBarrier() { __dmb(ARM_MB_SY); }

/// Return the current CPU cycle count.
inline uint64_t Cycles() { return __arm_rsr64("cp15:0:c11"); }

}  // namespace arch

#endif  // __cplusplus

#endif  // !__ASSEMBLER__

#endif  // ZIRCON_KERNEL_LIB_ARCH_ARM_INCLUDE_LIB_ARCH_INTRIN_H_
