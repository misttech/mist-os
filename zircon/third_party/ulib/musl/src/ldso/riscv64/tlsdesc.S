// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/arch/asm.h>

// The argument in a0 points to the GOT.  Unlike other machines, this has not
// just a slot for the function pointer and one other slot, but three more
// slots.  So we store both the offset and the module ID there with no
// additional indirection.  On other machines the second slot has to be a
// pointer to the pair of module ID and offset.  The return address is in t0
// rather than ra.  No registers other than a0 and t0 may be clobbered.

// For static TLS, the offset stored is the tp offset.  Just return it.
.function __tlsdesc_static, global
  ld a0, 8(a0)
  jr t0
.end_function

// For dynamic TLS, the offset stored is the offset within the module's
// PT_TLS block and the module ID is stored too.
.function __tlsdesc_dynamic, global
  // All the normally call-clobbered registers are actually preserved.
  .cfi_same_value ra
  .cfi_same_value a0
  .cfi_same_value a1
  .cfi_same_value a2
  .cfi_same_value a3
  .cfi_same_value a4
  .cfi_same_value a5
  .cfi_same_value a6
  .cfi_same_value a7
  .cfi_same_value t0
  .cfi_same_value t1
  .cfi_same_value t2
  .cfi_same_value t3
  .cfi_same_value t4
  .cfi_same_value t5
  .cfi_same_value t6

  // Take 4 slots to 16-byte alignment, though we only need 3 slots.
  // Skip the bottom slot (at the new sp) so that it can be used below.
  add sp, sp, -4*8
  .cfi_adjust_cfa_offset 4*8
  sd a1, 1*8(sp)
  .cfi_offset a1, (1-4)*8(sp)
  sd a2, 2*8(sp)
  .cfi_offset a2, (2-4)*8(sp)
  sd a3, 3*8(sp)
  .cfi_offset a3, (3-4)*8(sp)

  ld a2, 8(tp)   // DTV
  ld a1, 16(a0)  // module ID
  ld a0, 8(a0)   // Offset.
  ld a2, (a2)    // DTV[0], generation ID
  bgt a1, a2, .Lneed_new_dtv
  .cfi_remember_state
  slli a1, a1, 3 // Scale module ID to words.
  add a1, a1, a2 // &DTV[ID]
  ld a1, (a1)    // DTV[ID]
  add a0, a0, a1 // DTV[ID] + offset

.Lret:
  sub a0, a0, tp // TLS block pointer - tp
  ld a1, 0*8(sp)
  .cfi_same_value a1
  ld a2, 1*8(sp)
  .cfi_same_value a2
  ld a3, 2*8(sp)
  .cfi_same_value a2
  add sp, sp, 4*8
  .cfi_adjust_cfa_offset -4*8
  jr t0

.Lneed_new_dtv:
  // a0 is the offset and a1 is the module ID.
  // Those are the arguments to __tls_get_new.
  .cfi_restore_state

  .macro spill reg, idx, f=
    \f\()sd \reg, \idx*8(sp)
    // a1..a3 were already saved at -1*8..-3*8 from the CFA, when sp was
    // adjusted down to be -4*8 from the CFA itself.  Now sp has been further
    // adjusted so it's now -34*8 from the CFA, with the a1..a3 slots where
    // they already were and 31 more slots for the remaining saved registers.
    // So now CFA - 34*8 matches the sp, and the \idx is how many slots above
    // that \reg is stored.
    .cfi_offset \reg, (\idx - 34) * 8
  .endm
  .macro reload reg, idx, f=
    \f\()ld \reg, \idx*8(sp)
    .cfi_same_value \reg
  .endm
  .macro on_saved_regs op
    \op ra, 0
    \op a4, 1
    \op a5, 2
    \op a6, 3
    \op a7, 4
    \op t2, 5
    \op t3, 6
    \op t4, 7
    \op t5, 8
    \op t6, 9
    \op t7, 10
    // We could probably reasonably assume that __tls_get_new won't
    // clobber any F/D registers, but we don't. Note however that the
    // LP64D calling convention allows it to clobber the high halves
    // when they're actually Q registers. We're not preserving those
    // since we can't presume the Q instructions are available.
    \op ft0, 11, f
    \op ft1, 12, f
    \op ft2, 13, f
    \op ft3, 14, f
    \op ft4, 15, f
    \op ft5, 16, f
    \op ft6, 17, f
    \op ft7, 18, f
    \op ft8, 19, f
    \op ft9, 20, f
    \op ft10, 21, f
    \op ft11, 22, f
    \op fa0, 23, f
    \op fa1, 24, f
    \op fa2, 25, f
    \op fa3, 26, f
    \op fa4, 27, f
    \op fa5, 28, f
    \op fa6, 29, f
    \op fa7, 30, f
  .endm

  // We need 31 slots here, but an unused one was left at sp in the prologue.
  add sp, sp, -30*8
  .cfi_adjust_cfa_offset 30*8
  on_saved_regs spill

  call __tls_get_new

  on_saved_regs reload
  add sp, sp, 30*8
  .cfi_adjust_cfa_offset -30*8
  j .Lret

.end_function
