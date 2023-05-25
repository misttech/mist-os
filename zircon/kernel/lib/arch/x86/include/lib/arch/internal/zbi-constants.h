// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_LIB_ARCH_X86_INCLUDE_LIB_ARCH_INTERNAL_ZBI_CONSTANTS_H_
#define ZIRCON_KERNEL_LIB_ARCH_X86_INCLUDE_LIB_ARCH_INTERNAL_ZBI_CONSTANTS_H_

#include <lib/zbi-format/zbi.h>

#define ARCH_ZBI_KERNEL_TYPE (ZBI_TYPE_KERNEL_X64)

// Alignment required for an x86 kernel ZBI.
#define ARCH_ZBI_KERNEL_ALIGNMENT (1 << 12)

// Alignment required for an x86 data ZBI.
#define ARCH_ZBI_DATA_ALIGNMENT (1 << 12)

#endif  // ZIRCON_KERNEL_LIB_ARCH_X86_INCLUDE_LIB_ARCH_INTERNAL_ZBI_CONSTANTS_H_
