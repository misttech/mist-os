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
    sd    gp, CONTEXT_SWITCH_FRAME_OFFSET_GP(sp)

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

    ld    gp, CONTEXT_SWITCH_FRAME_OFFSET_GP(sp)
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

// Riscv64UserCopyRet _riscv64_user_copy(void *dst, const void *src, size_t len, uint64_t *fault_return, uint64_t capture_faults_mask)
.balign 64 // Align to cache line.  This code fits in one cache line.
FUNCTION(_riscv64_user_copy)
    addi   sp, sp, -48
    sd     ra, 32(sp)
    sd     s3, 24(sp)
    sd     gp, 16(sp)
    sd     s1, 8(sp)
    sd     s0, (sp)

    // Allow supervisor accesses to user memory by setting the SUM bit
    li     a5, RISCV64_CSR_SSTATUS_SUM
    csrs   sstatus, a5

    // Set *fault_return to fault_from_user orred with capture_faults_mask
    // to signal to the exception handler when to run the routine
    lla    a5, .Lfault_from_user
    or     a5, a5, a4
    sd     a5, (a3)

    // Save fault_return into a saved reg
    mv     s1, a3

    // Save a copy of the stack pointer
    mv     s3, sp

    // Just call our normal memcpy.  The caller has ensured that the
    // address range is in the user portion of the address space.
    // While fault_return_ptr is set, userspace data faults will be
    // redirected to .Lfault_from_user, below.
    //
    // Note makes serious assumptions about the implementation of memcpy.
    // Notably the memcpy implementation will not save or modify anything other than s0, gp and sp
    // as per normal frame pointer and shadow call stack usage. We save these registers above
    // as a result in case the fault path is taken and we're effectively pulled out of
    // the guts of the memcpy so that we can restore them below.
    // TODO-rvbringup: either implement copy inline or acquire/write a memset/memcpy routine
    // in assembly that can guarantee the above tighter.
    call   memcpy

    // Store a successful status for the return.
    // Zero out the top word of the structure to be clean about it.
    li     a0, ZX_OK
    li     a1, 0

.Luser_copy_return:
    // Restore *fault_return and the ra register
    sd     zero, (s1)

    // Disable supervisor accesses to user memory
    li     a5, RISCV64_CSR_SSTATUS_SUM
    csrc   sstatus, a5

    // Recover the old stack pointer
    mv     sp, s3

    ld     s0, (sp)
    ld     s1, 8(sp)
    ld     gp, 16(sp)
    ld     s3, 24(sp)
    ld     ra, 32(sp)
    addi   sp, sp, 48
    ret

// If we are capturing faults the exception handler will have placed the faulting virtual address
// for us in a1 and the flags in a2. We do not touch a1 and rely on the caller to know if the value
// is meaningful based on whether it specified fault capture or not, we just need to construct a
// valid a0 before jmping to user_copy_return.
.Lfault_from_user:
    li     a0, (ZX_ERR_INVALID_ARGS & 0xffffffff)
    // If we are capturing faults the flags will have been placed in a2 and we want them placed in
    // the high bits of a0. If not capturing faults then we will copy some garbage bits which will
    // be ignored by the caller.
    slli   a2, a2, 32
    or     a0, a0, a2
    j      .Luser_copy_return

END_FUNCTION(_riscv64_user_copy)
