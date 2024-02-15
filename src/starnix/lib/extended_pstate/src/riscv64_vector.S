// Copyright 2023 The Fuchsia Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// Offsets for `RiscvVectorCsrs` fields.
#define CSRS_STRUCT_VCSR (0)
#define CSRS_STRUCT_VSTART (1 * 8)
#define CSRS_STRUCT_VL (2 * 8)
#define CSRS_STRUCT_VTYPE (3 * 8)

#define FUNCTION(x) .global x; .hidden x; .type x,STT_FUNC; x: .cfi_startproc
#define END_FUNCTION(x) .cfi_endproc; .size x, . - x

// If vector instructions are not already enabled, do so at the assembler level.
#ifndef __riscv_v
.option arch, +v
#endif

// size_t get_riscv64_vlenb()
FUNCTION(get_riscv64_vlenb)
    csrr  a0, vlenb
    ret
END_FUNCTION(get_riscv64_vlenb)


// void save_riscv64_v_registers(uint8_t* v_registers, RiscvVectorCsrs* vcsrs)
FUNCTION(save_riscv64_v_registers)
    csrrw   t0, vstart, zero
    sd      t0, CSRS_STRUCT_VSTART(a1)

    csrr    t0, vcsr
    sd      t0, CSRS_STRUCT_VCSR(a1)

    csrr    t0, vl
    sd      t0, CSRS_STRUCT_VL(a1)

    csrr    t0, vtype
    sd      t0, CSRS_STRUCT_VTYPE(a1)

    // Save V registers, 8 at a time. `vstart` was zeroed by the `csrrw` above.
    csrr    t0, vlenb
    slli    t0, t0, 3
    vs8r.v  v0, (a0)
    add     a0, a0, t0
    vs8r.v  v8, (a0)
    add     a0, a0, t0
    vs8r.v  v16, (a0)
    add     a0, a0, t0
    vs8r.v  v24, (a0)

    ret
END_FUNCTION(save_riscv64_v_registers)


// void restore_riscv64_v_registers(const RiscvVectorState* state)
FUNCTION(restore_riscv64_v_registers)

    // Restore Vector CSR registers
    lw      t0, CSRS_STRUCT_VCSR(a1)
    csrw    vcsr, a1

    lw      t0, CSRS_STRUCT_VL(a1)
    lw      t1, CSRS_STRUCT_VTYPE(a1)
    vsetvl  x0, t0, t1

    // Restore V registers, 8 at a time. `vstart` was zeroed by the `vsetvl` above.
    csrr   t0, vlenb
    slli   t0, t0, 3
    vl8r.v v0, (a0)
    add    a0, a0, t0
    vl8r.v v8, (a0)
    add    a0, a0, t0
    vl8r.v v16, (a0)
    add    a0, a0, t0
    vl8r.v v24, (a0)

    // `vsetvl` and `vl8r.v` reset `vstart`, so this register has to be restored the last.
    lw      a1, CSRS_STRUCT_VSTART(a1)
    csrw    vstart, a1

    ret
END_FUNCTION(restore_riscv64_v_registers)
