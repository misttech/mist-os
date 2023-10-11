// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_ARCH_RISCV64_PHYS_INCLUDE_PHYS_ARCH_ADDRESS_SPACE_H_
#define ZIRCON_KERNEL_ARCH_RISCV64_PHYS_INCLUDE_PHYS_ARCH_ADDRESS_SPACE_H_

#include <lib/arch/paging.h>

//
// TODO(fxbug.dev/91187): Wire up actual RISC-V traits
//

using ArchLowerPagingTraits = arch::ExamplePagingTraits;
using ArchUpperPagingTraits = ArchLowerPagingTraits;

inline constexpr arch::ExamplePagingTraits::MemoryType kArchNormalMemoryType = {};
inline constexpr arch::ExamplePagingTraits::MemoryType kArchMmioMemoryType = {};

inline arch::ExamplePagingTraits::SystemState ArchCreatePagingState() { return {}; }

#endif  // ZIRCON_KERNEL_ARCH_RISCV64_PHYS_INCLUDE_PHYS_ARCH_ADDRESS_SPACE_H_
