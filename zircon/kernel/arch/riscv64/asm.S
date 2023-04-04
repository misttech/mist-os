// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <asm.h>
#include <arch/regs.h>
#include <arch/riscv64.h>
#include <zircon/errors.h>

// void riscv64_context_switch(vaddr_t *old_sp, vaddr_t new_sp);
FUNCTION(riscv64_context_switch)
    /* save old frame */
    addi  sp, sp, -SIZEOF_CONTEXT_SWITCH_FRAME
    sd    ra, CONTEXT_SWITCH_FRAME_OFFSET_RA(sp)
    sd    tp, CONTEXT_SWITCH_FRAME_OFFSET_TP(sp)

    sd    s0, CONTEXT_SWITCH_FRAME_OFFSET_S(0)(sp)
    sd    s1, CONTEXT_SWITCH_FRAME_OFFSET_S(1)(sp)
    sd    s2, CONTEXT_SWITCH_FRAME_OFFSET_S(2)(sp)
    sd    s3, CONTEXT_SWITCH_FRAME_OFFSET_S(3)(sp)
    sd    s4, CONTEXT_SWITCH_FRAME_OFFSET_S(4)(sp)
    sd    s5, CONTEXT_SWITCH_FRAME_OFFSET_S(5)(sp)
    sd    s6, CONTEXT_SWITCH_FRAME_OFFSET_S(6)(sp)
    sd    s7, CONTEXT_SWITCH_FRAME_OFFSET_S(7)(sp)
    sd    s8, CONTEXT_SWITCH_FRAME_OFFSET_S(8)(sp)
    sd    s9, CONTEXT_SWITCH_FRAME_OFFSET_S(9)(sp)
    sd    s10, CONTEXT_SWITCH_FRAME_OFFSET_S(10)(sp)
    sd    s11, CONTEXT_SWITCH_FRAME_OFFSET_S(11)(sp)

    /* save old sp */
    sd    sp, (a0)

    /* load new sp */
    mv    sp, a1

    /* restore new frame */
    ld    s0, CONTEXT_SWITCH_FRAME_OFFSET_S(0)(sp)
    ld    s1, CONTEXT_SWITCH_FRAME_OFFSET_S(1)(sp)
    ld    s2, CONTEXT_SWITCH_FRAME_OFFSET_S(2)(sp)
    ld    s3, CONTEXT_SWITCH_FRAME_OFFSET_S(3)(sp)
    ld    s4, CONTEXT_SWITCH_FRAME_OFFSET_S(4)(sp)
    ld    s5, CONTEXT_SWITCH_FRAME_OFFSET_S(5)(sp)
    ld    s6, CONTEXT_SWITCH_FRAME_OFFSET_S(6)(sp)
    ld    s7, CONTEXT_SWITCH_FRAME_OFFSET_S(7)(sp)
    ld    s8, CONTEXT_SWITCH_FRAME_OFFSET_S(8)(sp)
    ld    s9, CONTEXT_SWITCH_FRAME_OFFSET_S(9)(sp)
    ld    s10, CONTEXT_SWITCH_FRAME_OFFSET_S(10)(sp)
    ld    s11, CONTEXT_SWITCH_FRAME_OFFSET_S(11)(sp)

    ld    tp, CONTEXT_SWITCH_FRAME_OFFSET_TP(sp)
    ld    ra, CONTEXT_SWITCH_FRAME_OFFSET_RA(sp)
    addi  sp, sp, SIZEOF_CONTEXT_SWITCH_FRAME

    ret
END_FUNCTION(riscv64_context_switch)

// TODO-rvbringup: revisit if/when adding mexec support
FUNCTION(mexec_asm)
    unimp
END_FUNCTION(mexec_asm)

DATA(mexec_asm_end)

// Riscv64UserCopyRet _riscv64_user_copy(void *dst, const void *src, size_t len, uint64_t *fault_return)
.balign 64 // Align to cache line.  This code fits in one cache line.
FUNCTION(_riscv64_user_copy)
    addi   sp, sp, -24
    sd     s1, 16(sp)
    sd     s2, 8(sp)
    sd     s3, (sp)

    // Allow supervisor accesses to user memory
    li     s1, (1 << 18)
    csrs   sstatus, s1

    // Save fault_return and the ra register
    mv     s2, a3
    mv     s3, ra

    // Set *fault_return to fault_from_user
    la     t0, .Lfault_from_user
    sd     t0, (a3)

    // Just call our normal memcpy.  The caller has ensured that the
    // address range is in the user portion of the address space.
    // While fault_return_ptr is set, userspace data faults will be
    // redirected to .Lfault_from_user, below.
    //
    // NOTE! We make important assumptions here about what the memcpy
    // code does: it never moves the stack pointer, and it never touches a6 and
    // a7 where we saved fault_return and ra
    call   memcpy

    // Store a successful status for the return. In this case since we do not set x1 the value of
    // the fault address in the return struct is undefined.
    li     a0, ZX_OK

.Luser_copy_return:
    // Restore *fault_return and the ra register
    sd     x0, (s2)
    mv     ra, s3

    // Disable supervisor accesses to user memory
    csrc   sstatus, s1

    ld     s1, 16(sp)
    ld     s2, 8(sp)
    ld     s3, (sp)
    addi   sp, sp, 24
    ret
END_FUNCTION(_riscv64_user_copy)

// If we are capturing faults the exception handler will have placed the faulting virtual address
// for us in a1 and the flags in a2. We do not touch a1 and rely on the caller to know if the value
// is meaningful based on whether it specified fault capture or not, we just need to construct a
// valid a0 before jmping to user_copy_return.
.Lfault_from_user:
    li     a0, ZX_ERR_INVALID_ARGS
    // If we are capturing faults the flags will have been placed in a2 and we want them placed in
    // the high bits of a0. If not capturing faults then we will copy some garbage bits which will
    // be ignored by the caller.
    //bfi a0, a2, 32, 32
    j      .Luser_copy_return
