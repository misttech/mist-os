// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/arch/asm.h>

// The argument in a0 points to the GOT pair: function pointer, datum.
// The return address is in t0 rather than ra.  No registers other than
// a0, t0, and t1 may be clobbered.

// TODO(fxbug.dev/121305): This implementation is speculative and unused
// until the ABI is actually decided.

// For static TLS, the datum is the tp offset.  Just return it.
.function __tlsdesc_static, global
  ld a0, 8(a0)
  jr t0
.end_function

// For dynamic TLS, the datum points to an ID, offset pair.
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
// TODO(fxbug.dev/121305): currently assuming t1 is clobbered by the tlsdesc
// calling convention. When that ABI is finalized, might need to update this to
// use a different temporary or to spill another register so it can be used.
//  .cfi_same_value t1
  .cfi_same_value t2
  .cfi_same_value t3
  .cfi_same_value t4
  .cfi_same_value t5
  .cfi_same_value t6

  add sp, sp, -16
  .cfi_adjust_cfa_offset 16
  sd a1, 8(sp)
  .cfi_offset a1, -8
  sd a2, 0(sp)
  .cfi_offset a2, -16

  ld a0, 8(a0)  // Pointer to {module ID, offset} pair.
  .cfi_undefined a0
  ld a1, 0(a0)  // module ID
  ld a2, 8(tp)  // DTV
  ld t1, (a2)   // DTV[0], generation ID
  bgt t1, a1, .Lneed_new_dtv
  .cfi_remember_state
  ld a0, 8(a0)  // offset
  slli a1, a1, 3 // Scale module ID to words.
  add a1, a1, a2 // &DTV[ID]
  ld a1, (a1)    // DTV[ID]
  add a0, a0, a1 // DTV[ID] + offset

.Lret:
  sub a0, a0, tp // DTV[ID] + offset - tp
  ld a1, 8(sp)
  .cfi_same_value a1
  ld a2, 0(sp)
  .cfi_same_value a2
  add sp, sp, 16
  .cfi_adjust_cfa_offset -16
  jr t0

.Lneed_new_dtv:
  // a0 points to the {module ID, offset} pair.
  // That pointer is the argument to __tls_get_new.
  .cfi_restore_state

  .macro spill reg, idx
    sd \reg, \idx*8(sp)
    // a1 and a2 were already saved at -1*8 and -2*8 from the CFA,
    // when sp was adjusted down to be -2*8 from the CFA itself.
    // Now sp has been further adjusted so it's now -14*8 from the CFA,
    // with the a1, a2 slots where they already were, 11 more slots for
    // the remaining saved registers, and a wasted slot to preserve 16-byte
    // alignment.  So now CFA - 14*8 matches the sp, and the \idx is how many
    // slots above that \reg is stored.
    .cfi_offset \reg, (\idx - 14) * 8
  .endm
  .macro reload reg, idx
    ld \reg, \idx*8(sp)
    .cfi_same_value \reg
  .endm
  .macro on_saved_regs op
    \op ra, 0
    \op a3, 1
    \op a4, 2
    \op a5, 3
    \op a6, 4
    \op a7, 5
    \op t2, 6
    \op t3, 7
    \op t4, 8
    \op t5, 9
    \op t6, 10
  .endm

  add sp, sp, -12*8
  .cfi_adjust_cfa_offset 12*8
  on_saved_regs spill

  call __tls_get_new

  on_saved_regs reload
  add sp, sp, 12*8
  .cfi_adjust_cfa_offset -12*8
  j .Lret

.end_function
