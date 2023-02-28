// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_LIB_ARCH_X86_INCLUDE_LIB_ARCH_ASM_H_
#define ZIRCON_KERNEL_LIB_ARCH_X86_INCLUDE_LIB_ARCH_ASM_H_

// Get the generic file.
#include_next <lib/arch/asm.h>

#ifdef __ASSEMBLER__  // clang-format off

.macro assert.fail
  ud2
.endm

#endif  // clang-format on

#endif  // ZIRCON_KERNEL_LIB_ARCH_X86_INCLUDE_LIB_ARCH_ASM_H_
