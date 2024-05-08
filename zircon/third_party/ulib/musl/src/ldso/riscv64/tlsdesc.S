// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/arch/asm.h>

// In both the static and dynamic cases, a0 will point to the GOT. The psABI
// for RISC-V defines a 2-Slot layout, like other machines for TLSDESC
// resolution. In the static case, the second slot has the offset from tp of the
// symbol.
//
// In the dynamic case, the second slot has to be a pointer to the pair of
// module ID and offset.
//
// In both cases, the return address is in t0 rather than ra.  No registers
// other than a0 and t0 may be clobbered.

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

  // Take 4 slots to 16-byte alignment, though we only need 3 slots. We leave
  // the last slot available so that the slow path can store t0, in addition to
  // the other saved registers.
  add sp, sp, -4*8
  .cfi_adjust_cfa_offset 4*8
  sd a1, 1*8(sp)
  .cfi_rel_offset a1, 1*8
  sd a2, 2*8(sp)
  .cfi_rel_offset a2, 2*8
  sd a3, 2*8(sp)
  .cfi_rel_offset a3, 3*8

  // size_t __tlsdesc_dynamic(size_t *a)
  // {
  // 	struct {size_t modidx,off;} *p = (void*)a[1];
  // 	size_t *dtv = *(size_t**)(tp - 24);
  // 	if (p->modidx <= dtv[0])
  // 		return dtv[p->modidx] + p->off - tp;
  // 	return __tls_get_new(p[1], p[0]) - tp;
  // }

  ld a1, 8(a0)   // Pointer to struct: p
  ld a0, 8(a1)   // Offset: p->offset
  ld a1, (a1)    // module ID: p->modidx
  ld a2, -24(tp) // DTV
  ld a3, (a2)    // DTV[0], generation ID
  bgt a1, a3, .Lneed_new_dtv
  .cfi_remember_state
  slli a1, a1, 3 // Scale module ID to words.
  add a1, a1, a2 // &DTV[ID]
  ld a1, (a1)    // DTV[ID]
  add a0, a0, a1 // DTV[ID] + offset

.Lret:
  sub a0, a0, tp // TLS block pointer - tp
  ld a1, 1*8(sp)
  .cfi_same_value a1
  ld a2, 2*8(sp)
  .cfi_same_value a2
  ld a3, 3*8(sp)
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
    .cfi_rel_offset \reg, \idx * 8
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
    // We could probably reasonably assume that __tls_get_new won't
    // clobber any F/D registers, but we don't. Note however that the
    // LP64D calling convention allows it to clobber the high halves
    // when they're actually Q registers. We're not preserving those
    // since we can't presume the Q instructions are available.
    \op ft0, 10, f
    \op ft1, 11, f
    \op ft2, 12, f
    \op ft3, 13, f
    \op ft4, 14, f
    \op ft5, 15, f
    \op ft6, 16, f
    \op ft7, 17, f
    \op ft8, 18, f
    \op ft9, 19, f
    \op ft10, 20, f
    \op ft11, 21, f
    \op fa0, 22, f
    \op fa1, 23, f
    \op fa2, 24, f
    \op fa3, 25, f
    \op fa4, 26, f
    \op fa5, 27, f
    \op fa6, 28, f
    \op fa7, 29, f
    \op t0, 30
  .endm

  // We need 30 slots here
  add sp, sp, -32*8
  .cfi_adjust_cfa_offset 32*8
  on_saved_regs spill

  call __tls_get_new

  on_saved_regs reload
  add sp, sp, 30*8
  .cfi_adjust_cfa_offset -32*8
  j .Lret

.end_function
