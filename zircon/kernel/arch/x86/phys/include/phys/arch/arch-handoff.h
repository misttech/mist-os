// Copyright 2021 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_ARCH_X86_PHYS_INCLUDE_PHYS_ARCH_ARCH_HANDOFF_H_
#define ZIRCON_KERNEL_ARCH_X86_PHYS_INCLUDE_PHYS_ARCH_ARCH_HANDOFF_H_

// Note: we refrain from using the ktl namespace as <phys/handoff.h> is
// expected to be compiled in the userboot toolchain.

#include <cstdint>

struct ArchPatchInfo {};

// This holds (or points to) all x86-specific data that is handed off from
// physboot to the kernel proper at boot time.
struct ArchPhysHandoff {};

inline constexpr uint64_t kArchHandoffVirtualAddress = 0xffff'ffff'0000'0000;

// TODO(https://fxbug.dev/42164859): Make this constant the source of truth
// for the physmap in the kernel.
inline constexpr uint64_t kArchPhysmapVirtualBase = 0xffff'ff80'0000'0000;

#endif  // ZIRCON_KERNEL_ARCH_X86_PHYS_INCLUDE_PHYS_ARCH_ARCH_HANDOFF_H_
