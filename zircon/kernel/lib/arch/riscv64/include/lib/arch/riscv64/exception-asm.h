// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_LIB_ARCH_RISCV64_INCLUDE_LIB_ARCH_RISCV64_EXCEPTION_ASM_H_
#define ZIRCON_KERNEL_LIB_ARCH_RISCV64_INCLUDE_LIB_ARCH_RISCV64_EXCEPTION_ASM_H_

// This file provides macros primarily useful for the assembly code that
// defines an exception vector routine that stvec points to.

#ifdef __ASSEMBLER__  // clang-format off

// This sets up the CFI state to represent the vector entry point conditions.
// It should come after `.cfi_startproc simple`.  The code that saves the
// interrupted registers should then use CFI directives to stay consistent.
// NOTE! This doesn't define a CFA rule, since no stack-switching has been
// done.  That rule must be defined appropriately for what *will* become the
// exception handler's stack frame.
.macro .cfi.stvec
  // The "caller" is actually interrupted register state.  This means the
  // "return address" will be treated as the precise PC of the "caller", rather
  // than as its return address that's one instruction after the call site.
  .cfi_signal_frame

  // 64 is the canonical "alternate frame return column".
  .cfi_return_column 64
  // The interrupted PC is found in the sepc CSR, which has a DWARF number.
#ifndef __clang__  // TODO(fxbug.dev/122138)
  .cfi_register 64, sepc
#endif

  // All other registers still have their interrupted state.
  .cfi_same_value x0
  .cfi_same_value x1
  .cfi_same_value x2
  .cfi_same_value x3
  .cfi_same_value x4
  .cfi_same_value x5
  .cfi_same_value x6
  .cfi_same_value x7
  .cfi_same_value x8
  .cfi_same_value x9
  .cfi_same_value x10
  .cfi_same_value x11
  .cfi_same_value x12
  .cfi_same_value x13
  .cfi_same_value x14
  .cfi_same_value x15
  .cfi_same_value x16
  .cfi_same_value x17
  .cfi_same_value x18
  .cfi_same_value x19
  .cfi_same_value x20
  .cfi_same_value x21
  .cfi_same_value x22
  .cfi_same_value x23
  .cfi_same_value x24
  .cfi_same_value x25
  .cfi_same_value x26
  .cfi_same_value x27
  .cfi_same_value x28
  .cfi_same_value x29
  .cfi_same_value x30
  .cfi_same_value x31

  // TODO(mcgrathr): unwind for float regs?

.endm

#endif  // clang-format on

#endif  // ZIRCON_KERNEL_LIB_ARCH_RISCV64_INCLUDE_LIB_ARCH_RISCV64_EXCEPTION_ASM_H_
