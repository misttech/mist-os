// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <asm.h>
#include <arch/regs.h>
#include <arch/riscv64.h>

// Put these routines in this file separate from others so the F and D bits
// of the architecture can be added to allow for floating point instructions
.option arch, +f, +d

// Zero out the FPU single and double precision hardware state
// void riscv64_fpu_zero();
FUNCTION(riscv64_fpu_zero)
.macro ZERO_FPU_REG regnum
    fmv.d.x f\regnum, zero
.endm

    // Enable the fpu so we don't trap while setting it
    li      a0, RISCV64_CSR_SSTATUS_FS_DIRTY
    csrs    sstatus, a0

    csrw    fcsr, zero
    ZERO_FPU_REG  0
    ZERO_FPU_REG  1
    ZERO_FPU_REG  2
    ZERO_FPU_REG  3
    ZERO_FPU_REG  4
    ZERO_FPU_REG  5
    ZERO_FPU_REG  6
    ZERO_FPU_REG  7
    ZERO_FPU_REG  8
    ZERO_FPU_REG  9
    ZERO_FPU_REG  10
    ZERO_FPU_REG  11
    ZERO_FPU_REG  12
    ZERO_FPU_REG  13
    ZERO_FPU_REG  14
    ZERO_FPU_REG  15
    ZERO_FPU_REG  16
    ZERO_FPU_REG  17
    ZERO_FPU_REG  18
    ZERO_FPU_REG  19
    ZERO_FPU_REG  20
    ZERO_FPU_REG  21
    ZERO_FPU_REG  22
    ZERO_FPU_REG  23
    ZERO_FPU_REG  24
    ZERO_FPU_REG  25
    ZERO_FPU_REG  26
    ZERO_FPU_REG  27
    ZERO_FPU_REG  28
    ZERO_FPU_REG  29
    ZERO_FPU_REG  30
    ZERO_FPU_REG  31

    // Set the FPU hardware to the initial state. Clear the top bit
    // of the two bit field to go from dirty (0b11) to initial (0b01)
    // via the clean state (0b10)
    li      a0, RISCV64_CSR_SSTATUS_FS_CLEAN
    csrc    sstatus, a0

    ret
END_FUNCTION(riscv64_fpu_zero)

// void riscv64_fpu_save(riscv64_fpu_state *state)
FUNCTION(riscv64_fpu_save)
.macro SAVE_FPU_REG regnum
    fsd     f\regnum, (8 * \regnum)(a0)
.endm

    SAVE_FPU_REG 0
    SAVE_FPU_REG 1
    SAVE_FPU_REG 2
    SAVE_FPU_REG 3
    SAVE_FPU_REG 4
    SAVE_FPU_REG 5
    SAVE_FPU_REG 6
    SAVE_FPU_REG 7
    SAVE_FPU_REG 8
    SAVE_FPU_REG 9
    SAVE_FPU_REG 10
    SAVE_FPU_REG 11
    SAVE_FPU_REG 12
    SAVE_FPU_REG 13
    SAVE_FPU_REG 14
    SAVE_FPU_REG 15
    SAVE_FPU_REG 16
    SAVE_FPU_REG 17
    SAVE_FPU_REG 18
    SAVE_FPU_REG 19
    SAVE_FPU_REG 20
    SAVE_FPU_REG 21
    SAVE_FPU_REG 22
    SAVE_FPU_REG 23
    SAVE_FPU_REG 24
    SAVE_FPU_REG 25
    SAVE_FPU_REG 26
    SAVE_FPU_REG 27
    SAVE_FPU_REG 28
    SAVE_FPU_REG 29
    SAVE_FPU_REG 30
    SAVE_FPU_REG 31
    csrr    a1, fcsr
    sw      a1, 32*8(a0)

    ret
END_FUNCTION(riscv64_fpu_save)

// void riscv64_fpu_restore(const riscv64_fpu_state *state)
FUNCTION(riscv64_fpu_restore)
.macro LOAD_FPU_REG regnum
    fld     f\regnum, (8 * \regnum)(a0)
.endm

    LOAD_FPU_REG 0
    LOAD_FPU_REG 1
    LOAD_FPU_REG 2
    LOAD_FPU_REG 3
    LOAD_FPU_REG 4
    LOAD_FPU_REG 5
    LOAD_FPU_REG 6
    LOAD_FPU_REG 7
    LOAD_FPU_REG 8
    LOAD_FPU_REG 9
    LOAD_FPU_REG 10
    LOAD_FPU_REG 11
    LOAD_FPU_REG 12
    LOAD_FPU_REG 13
    LOAD_FPU_REG 14
    LOAD_FPU_REG 15
    LOAD_FPU_REG 16
    LOAD_FPU_REG 17
    LOAD_FPU_REG 18
    LOAD_FPU_REG 19
    LOAD_FPU_REG 20
    LOAD_FPU_REG 21
    LOAD_FPU_REG 22
    LOAD_FPU_REG 23
    LOAD_FPU_REG 24
    LOAD_FPU_REG 25
    LOAD_FPU_REG 26
    LOAD_FPU_REG 27
    LOAD_FPU_REG 28
    LOAD_FPU_REG 29
    LOAD_FPU_REG 30
    LOAD_FPU_REG 31
    lw      a1, 32*8(a0)
    csrw    fcsr, a1

    ret
END_FUNCTION(riscv64_fpu_restore)
