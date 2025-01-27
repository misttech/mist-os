// Copyright 2021 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_ARCH_X86_PHYS_INCLUDE_PHYS_ARCH_ARCH_HANDOFF_H_
#define ZIRCON_KERNEL_ARCH_X86_PHYS_INCLUDE_PHYS_ARCH_ARCH_HANDOFF_H_

// Note: we refrain from using the ktl namespace as <phys/handoff.h> is
// expected to be compiled in the userboot toolchain.

#include <lib/zbi-format/graphics.h>

#include <optional>

struct ArchPatchInfo {};

// This holds (or points to) all x86-specific data that is handed off from
// physboot to the kernel proper at boot time.
struct ArchPhysHandoff {};

inline constexpr uint64_t kArchHandoffVirtualAddress = 0xffff'ffff'0000'0000;

// Whether a peripheral range for the UART needs to be synthesized.
inline constexpr bool kArchHandoffGenerateUartPeripheralRanges = false;

#endif // ZIRCON_KERNEL_ARCH_X86_PHYS_INCLUDE_PHYS_ARCH_ARCH_HANDOFF_H_
