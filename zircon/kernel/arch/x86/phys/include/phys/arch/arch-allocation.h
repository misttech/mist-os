// Copyright 2022 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_ARCH_X86_PHYS_INCLUDE_PHYS_ARCH_ARCH_ALLOCATION_H_
#define ZIRCON_KERNEL_ARCH_X86_PHYS_INCLUDE_PHYS_ARCH_ARCH_ALLOCATION_H_

#include <stdint.h>

#include <ktl/optional.h>

// The first 1MiB is reserved for 16-bit real-mode uses.
constexpr ktl::optional<uint64_t> kArchAllocationMinAddr = uint64_t{1} << 20;

// TODO(https://fxbug.dev/42164859): Currently early kernel start-up assumes
// that key physical allocations are within the first 4GiB. This assumption
// will be removed in the context of this bug, but to be safe in the meantime
// we set this to forbid any allocations past this cut-off.
constexpr ktl::optional<uint64_t> kArchAllocationMaxAddr = uint64_t{1} << 32;

#endif  // ZIRCON_KERNEL_ARCH_X86_PHYS_INCLUDE_PHYS_ARCH_ARCH_ALLOCATION_H_
