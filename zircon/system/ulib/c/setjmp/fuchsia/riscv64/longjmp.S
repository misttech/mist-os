// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/arch/asm.h>
#include <zircon/tls.h>

#include "../jmp_buf.h"

jmp_buf.llvm_libc_function longjmp

  // The sanitizer runtime wants to be informed of non-local exits.
  // Call __asan_handle_no_return() before doing the actual longjmp.
#if __has_feature(address_sanitizer)
  // Save our incoming argument registers and return address on the stack
  // around calling __asan_handle_no_return.
  add sp, sp, -32
  .cfi_adjust_cfa_offset 32
  sd ra, 0(sp)
  .cfi_offset ra, -32 + 0
  sd a0, 8(sp)
  .cfi_offset ra, -32 + 8
  sd a1, 16(sp)
  .cfi_offset ra, -32 + 16
  call __asan_handle_no_return
  ld ra, 0(sp)
  .cfi_same_value ra
  ld a0, 8(sp)
  .cfi_same_value a0
  ld a1, 16(sp)
  .cfi_same_value a1
  add sp, sp, 32
  .cfi_adjust_cfa_offset -32
#endif

  // Find the manglers.
  lla a2, LIBC_ASM_LINKAGE(gJmpBufManglers)

  // Load the words that need to be demangled into temporaries.
  // These just hold values that were in the jmp_buf, so we don't
  // care about leaking them.
  ld a3, JB_PC*8(a0)
  ld a4, JB_SP*8(a0)
  ld a5, JB_FP*8(a0)
  ld a6, JB_USP*8(a0)
  ld a7, JB_SCSP*8(a0)

  // The next instruction clobbers the state of longjmp's caller.
  // So from here on, we'll use CFI that unwinds to setjmp's caller instead.
  // Both callers have the same a0 value (the jmp_buf).
  // The a1 value is that of longjmp's caller, not setjmp's.
  .cfi_same_value a0
  .cfi_undefined a1

   // CFI to find regno at \offset(a0).
  .macro jb_cfi regno, offset
    .if \offset < 0x7f
      .cfi_escape DW_CFA_expression, \regno, 2, DW_OP_breg(10), \offset
    .elseif \offset < 0x3fff
      .cfi_escape DW_CFA_expression, \regno, 3, DW_OP_breg(10), \
        (\offset & 0x7f) | 0x80, \offset >> 7
    .else
      .error "offset too large for two-byte SLEB128"
    .endif
  .endm

  // CFI to find fs\n at JB_FS(\n)*8(a0).
  .macro jb_cfi_fs n
    .if \n < 2
      jb_cfi (32 + 8 + \n), JB_FS(\n)*8
    .else
      jb_cfi (32 + 18 + \n - 2), JB_FS(\n)*8
    .endif
  .endm

  // CFI to compute regno as (\offset(a0) ^ \offset(a2)).
  .macro jb_cfi_mangled regno, offset
    .ifgt \offset - 0x7f
      .error "offset too large for one-byte SLEB128"
    .endif
    .cfi_escape DW_CFA_val_expression, \regno, 7, \
                DW_OP_breg(10), \offset, DW_OP_deref, \
                DW_OP_breg(12), \offset, DW_OP_deref, DW_OP_xor
  .endm

  jb_cfi_mangled 1, JB_PC*8  // ra
  jb_cfi_mangled 2, JB_SP*8  // sp
  jb_cfi_mangled 8, JB_FP*8  // s0
  // There's no CFI for the unsafe SP!
  jb_cfi_mangled 3, JB_SCSP*8 // gp
  jb_cfi 9, JB_S(1)*8
  jb_cfi 18, JB_S(2)*8
  jb_cfi 19, JB_S(3)*8
  jb_cfi 20, JB_S(4)*8
  jb_cfi 21, JB_S(5)*8
  jb_cfi 22, JB_S(6)*8
  jb_cfi 23, JB_S(7)*8
  jb_cfi 24, JB_S(8)*8
  jb_cfi 25, JB_S(9)*8
  jb_cfi 26, JB_S(10)*8
  jb_cfi 27, JB_S(11)*8
#ifndef __riscv_float_abi_soft
  jb_cfi_fs 0
  jb_cfi_fs 1
  jb_cfi_fs 2
  jb_cfi_fs 3
  jb_cfi_fs 4
  jb_cfi_fs 5
  jb_cfi_fs 6
  jb_cfi_fs 7
  jb_cfi_fs 8
  jb_cfi_fs 9
  jb_cfi_fs 10
  jb_cfi_fs 11
#endif

  // We don't want to leak the raw mangler values, so load
  // them into their target registers rather than temporaries
  // so we don't have more temporaries to clear.
  .macro load_mangler regno, offset, tmpno
    ld x\regno, \offset(a2)
    // CFI to compute x\regno as (x\regno ^ x\tmpno).
    .cfi_escape DW_CFA_val_expression, \regno, 5, \
                DW_OP_breg(\regno), 0, DW_OP_breg(\tmpno), 0, DW_OP_xor
  .endm
  load_mangler 1, JB_PC*8, 13     // a3 = x13
  load_mangler 2, JB_SP*8, 14     // a4 = x14
  load_mangler 8, JB_FP*8, 15     // a5 = x15
  load_mangler 3, JB_SCSP*8, 17   // a7 = x17

  // Reuse the temporary for the unsafe SP.  There's no CFI for it!
  ld a2, JB_USP*8(a2)

  // Now demangle the jmp_buf values in place.
  .macro restore_mangled reg, tmp
    xor \reg, \reg, \tmp
    .cfi_same_value \reg
  .endm
  xor a2, a2, a6  // Unsafe SP temporary location.
  restore_mangled ra, a3
  restore_mangled sp, a4
  restore_mangled s0, a5
  restore_mangled gp, a7

  // Store the unsafe SP into its real slot.
  // Then clear the temporary so its value isn't leaked.
  sd a2, ZX_TLS_UNSAFE_SP_OFFSET(tp)
  mv a2, zero

  // Restore the vanilla call-saved registers.
  .macro restore_s n
    ld s\n, JB_S(\n)*8(a0)
    .cfi_same_value s\n
  .endm
  restore_s 1
  restore_s 2
  restore_s 3
  restore_s 4
  restore_s 5
  restore_s 6
  restore_s 7
  restore_s 8
  restore_s 9
  restore_s 10
  restore_s 11

#ifndef __riscv_float_abi_soft
  // Restore the call-saved FP registers.
  .macro restore_fs n
    fld fs\n, JB_FS(\n)*8(a0)
    .cfi_same_value fs\n
  .endm
  restore_fs 0
  restore_fs 1
  restore_fs 2
  restore_fs 3
  restore_fs 4
  restore_fs 5
  restore_fs 6
  restore_fs 7
  restore_fs 8
  restore_fs 9
  restore_fs 10
  restore_fs 11
#endif

  seqz a0, a1                   // a0 = (a1 == 0) ? 1 : 0
  .cfi_undefined a0             // Caller's a0 is no longer recoverable.
  add a0, a0, a1                // a0 += a1; AKA a0 = (a1 == 0) ? 1 : a1
  ret

.end_function
