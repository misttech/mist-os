// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_ARCH_RISCV64_PHYS_INCLUDE_PHYS_ARCH_ADDRESS_SPACE_H_
#define ZIRCON_KERNEL_ARCH_RISCV64_PHYS_INCLUDE_PHYS_ARCH_ADDRESS_SPACE_H_

#include <lib/arch/riscv64/page-table.h>

using ArchLowerPagingTraits = arch::RiscvSv39PagingTraits;
using ArchUpperPagingTraits = ArchLowerPagingTraits;

inline constexpr auto kArchNormalMemoryType = arch::RiscvMemoryType::kPma;

// TODO(fxbug.dev/129979): Conditionally set to kIo once we can determine
// whether the svpbmt extension is supported.
inline constexpr auto kArchMmioMemoryType = arch::RiscvMemoryType::kPma;

inline arch::RiscvSystemPagingState ArchCreatePagingState() { return {}; }

#endif  // ZIRCON_KERNEL_ARCH_RISCV64_PHYS_INCLUDE_PHYS_ARCH_ADDRESS_SPACE_H_
