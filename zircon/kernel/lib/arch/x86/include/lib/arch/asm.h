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

// Standard prologue sequence for FP setup, with CFI.
// Note that this realigns the SP from entry state to be ready for a call.
.macro .prologue.fp
  push %rbp
  .cfi_adjust_cfa_offset 8
  .cfi_offset %rbp, -16
  mov %rsp, %rbp
.endm

// Epilogue sequence to match .prologue.fp.
.macro .epilogue.fp
  pop %rbp
  .cfi_same_value %rbp
  .cfi_adjust_cfa_offset -8
.endm

#endif  // clang-format on

#endif  // ZIRCON_KERNEL_LIB_ARCH_X86_INCLUDE_LIB_ARCH_ASM_H_
