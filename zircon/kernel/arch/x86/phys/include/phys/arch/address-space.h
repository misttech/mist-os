// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_ARCH_X86_PHYS_INCLUDE_PHYS_ARCH_ADDRESS_SPACE_H_
#define ZIRCON_KERNEL_ARCH_X86_PHYS_INCLUDE_PHYS_ARCH_ADDRESS_SPACE_H_

#include <lib/arch/x86/boot-cpuid.h>
#include <lib/arch/x86/page-table.h>

#include <hwreg/x86msr.h>

using ArchLowerPagingTraits = arch::X86FourLevelPagingTraits;
using ArchUpperPagingTraits = ArchLowerPagingTraits;

inline constexpr arch::X86MemoryType kArchNormalMemoryType = {};
inline constexpr arch::X86MemoryType kArchMmioMemoryType = {};

inline arch::X86SystemPagingState ArchCreatePagingState() {
  return arch::X86SystemPagingState::Create(hwreg::X86MsrIo{}, arch::BootCpuidIo{});
}

#endif  // ZIRCON_KERNEL_ARCH_X86_PHYS_INCLUDE_PHYS_ARCH_ADDRESS_SPACE_H_
