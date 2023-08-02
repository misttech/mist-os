// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_LIB_ARCH_RISCV64_INCLUDE_LIB_ARCH_ASM_H_
#define ZIRCON_KERNEL_LIB_ARCH_RISCV64_INCLUDE_LIB_ARCH_ASM_H_

// Get the generic file.
#include_next <lib/arch/asm.h>

#ifdef __ASSEMBLER__  // clang-format off

#ifndef __has_feature
#define __has_feature(x) 0
#endif

// Kernel code is compiled with -ffixed-x27 (x27 == s11) so the compiler won't
// use it.
#define percpu_ptr s11

// This register is permanently reserved by the ABI in the compiler.
// #if __has_feature(shadow_call_stack) it's used for the SCSP.
#define shadow_call_sp gp

#define DWARF_REGNO_shadow_call_sp 3 // x3 == gp

.macro assert.fail
  unimp
.endm

// Standard prologue sequence for FP setup, with CFI.
.macro .prologue.fp frame_extra_size=0
  add sp, sp, -(16 + \frame_extra_size)
  // The CFA is still computed relative to the SP so code will
  // continue to use .cfi_adjust_cfa_offset for pushes and pops.
  .cfi_adjust_cfa_offset 16 + \frame_extra_size
  sd s0, 0(sp)
  .cfi_offset s0, 0 - (16 + \frame_extra_size)
  sd ra, 8(sp)
  .cfi_offset ra, 8 - (16 + \frame_extra_size)
  mv s0, sp
.endm

// Epilogue sequence to match .prologue.fp with the same argument.
.macro .epilogue.fp frame_extra_size=0
  ld s0, 0(sp)
  .cfi_same_value s0
  ld ra, 8(sp)
  .cfi_same_value ra
  add sp, sp, 16 + \frame_extra_size
  .cfi_adjust_cfa_offset -(16 + \frame_extra_size)
.endm

// Standard prologue sequence for shadow call stack, with CFI.
.macro .prologue.shadow_call_sp
#if __has_feature(shadow_call_stack)
  sd ra, (shadow_call_sp)
  // Set the ra (x1) rule to DW_CFA_expression{DW_OP_breg3(-8)}.
  .cfi_escape 0x0f, 1, 2, 0x70 + DWARF_REGNO_shadow_call_sp, (-8) & 0x7f
  add shadow_call_sp, shadow_call_sp, 8
  // Set the x3 (gp) rule to DW_CFA_val_expression{DW_OP_breg3(-8)} to
  // compensate for the increment just done.
  .cfi_escape 0x16, DWARF_REGNO_shadow_call_sp, 2, 0x70 + DWARF_REGNO_shadow_call_sp, (-8) & 0x7f
#endif
.endm

// Epilogue sequence to match .prologue.shadow_call_sp.
.macro .epilogue.shadow_call_sp
#if __has_feature(shadow_call_stack)
  ld ra, -8(shadow_call_sp)
  .cfi_same_value ra
  add shadow_call_sp, shadow_call_sp, -8
  .cfi_same_value shadow_call_sp
#endif
.endm

#endif  // clang-format on

#endif  // ZIRCON_KERNEL_LIB_ARCH_RISCV64_INCLUDE_LIB_ARCH_ASM_H_
