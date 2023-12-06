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

.macro .cfi.all_integer op
  .irp reg,x1,x2,x3,x4,x5,x6,x7,x8,x9,x10,x11,x12,x13,x14,x15,x16,x17,x18,x19,x20,x21,x22,x23,x24,x25,x26,x27,x28,x29,x30,x31
    \op \reg
  .endr
.endm

.macro .cfi.all_vectorfp op
#ifdef __riscv_f
  .irp reg,f0,f1,f2,f3,f4,f5,f6,f7,f8,f9,f10,f11,f12,f13,f14,f15,f16,f17,f18,f19,f20,f21,f22,f23,f24,f25,f26,f27,f28,f29,f30,f31
    \op \reg
  .endr
#endif
#ifdef __riscv_v
    .irp reg,v0,v1,v2,v3,v4,v5,v6,v7,v8,v9,v10,v11,v12,v13,v14,v15,v16,v17,v18,v19,v20,v21,v22,v23,v24,v25,v26,v27,v28,v29,v30,v31
    \op \reg
  .endr
#endif
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
