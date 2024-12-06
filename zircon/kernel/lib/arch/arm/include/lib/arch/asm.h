// Copyright 2024 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_LIB_ARCH_ARM_INCLUDE_LIB_ARCH_ASM_H_
#define ZIRCON_KERNEL_LIB_ARCH_ARM_INCLUDE_LIB_ARCH_ASM_H_

// Get the generic file.
#include_next <lib/arch/asm.h>

#ifdef __ASSEMBLER__  // clang-format off

// If the compiler uses -mthumb, then tell the assembler to use Thumb too.
#ifdef __thumb__
.thumb
#endif

.macro assert.fail
  udf #0xfe
.endm

.macro .cfi.all_integer op
  .irp reg,r0,r1,r2,r3,r4,r5,r6,r7,r8,r9,r10,r11,r12,r14
    \op \reg
  .endr
.endm

.macro .cfi.all_vectorfp op
  .irp reg,q0,q1,q2,q3,q4,q5,q6,q7,q8,q9,q10,q11,q12,q13,q14,q15,q16,q17,q18,q19,q20,q21,q22,q23,q24,q25,q26,q27,q28,q29,q30,q31
    \op \reg
  .endr
.endm

// Add an immediate value to SP, adjusting CFI assuming the CFA rule
// uses an offset from SP.
.macro .add.sp imm
  add sp, sp, #\imm
  .cfi_adjust_cfa_offset -(\imm)
.endm

#endif  // clang-format on

#endif  // ZIRCON_KERNEL_LIB_ARCH_ARM_INCLUDE_LIB_ARCH_ASM_H_
