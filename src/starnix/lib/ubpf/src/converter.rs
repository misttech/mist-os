// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use linux_uapi::*;
use std::collections::HashMap;

use crate::UbpfError;
use crate::UbpfError::*;

// These are accessors for bits in an BPF/EBPF instruction.
// Instructions are encoded in one byte.  The first 3 LSB represent
// the operation, and the other bits represent various modifiers.
// Brief comments are given to indicate what the functions broadly
// represent, but for the gory detail, consult a detailed guide to
// BPF, like the one at https://docs.kernel.org/bpf/instruction-set.html

/// The bpf_class is the instruction type.(e.g., load/store/jump/ALU).
pub fn bpf_class(filter: &sock_filter) -> u32 {
    (filter.code & 0x07).into()
}

/// The bpf_size is the 4th and 5th bit of load and store
/// instructions.  It indicates the bit width of the load / store
/// target (8, 16, 32, 64 bits).
fn bpf_size(filter: &sock_filter) -> u32 {
    (filter.code & 0x18).into()
}

/// The addressing mode is the most significant three bits of load and
/// store instructions.  They indicate whether the instrution accesses a
/// constant, accesses from memory, or accesses from memory atomically.
pub fn bpf_addressing_mode(filter: &sock_filter) -> u32 {
    (filter.code & 0xe0).into()
}

/// Modifiers for jumps and alu operations.  For example, a jump can
/// be jeq, jtl, etc.  An ALU operation can be plus, minus, divide,
/// etc.
fn bpf_op(filter: &sock_filter) -> u32 {
    (filter.code & 0xf0).into()
}

/// The source for the operation (either a register or an immediate).
fn bpf_src(filter: &sock_filter) -> u32 {
    (filter.code & 0x08).into()
}

/// Similar to bpf_src, but also allows BPF_A - used for RET.
fn bpf_rval(filter: &sock_filter) -> u32 {
    (filter.code & 0x18).into()
}

// For details on this function, see to_be_patched in the cbpf_to_ebpf converter.
fn prep_patch(to_be_patched: &mut HashMap<i16, Vec<usize>>, cbpf_target: i16, ebpf_source: usize) {
    to_be_patched.entry(cbpf_target).or_insert_with(std::vec::Vec::new);
    to_be_patched.get_mut(&cbpf_target).unwrap().push(ebpf_source);
}

/// Transforms a program in classic BPF (cbpf, as stored in struct
/// sock_filter) to extended BPF (as stored in struct bpf_insn).
/// The bpf_code parameter is kept as an array for easy transfer
/// via FFI.  This currently only allows the subset of BPF permitted
/// by seccomp(2).
pub(crate) fn cbpf_to_ebpf(bpf_code: &[sock_filter]) -> Result<Vec<bpf_insn>, UbpfError> {
    // There are only two BPF registers, A and X. There are 10
    // EBPF registers, numbered 0-9.  We map between the two as
    // follows:

    // r[0]: We map this to A, since it can be used as a return value.
    // r[1]: ubpf makes this the memory passed in,
    // r[2]: ubpf makes this the length of the memory passed in.
    // r[6]: We map this to X, to keep it away from being clobbered by call / return.
    // r[7]: Scratch register, replaces M[0]
    //       cbpf programs running on Linux have a 16 word scratch memory.  ubpf
    //       only has scratch registers and the data passed in.  For now, we use
    //       the extra registers and hope that no one needs more than 3 of them.
    //       Can be replaced by allocating the incoming data into a buffer that
    //       has an extra 16 words, and using those words.  However, this makes an
    //       awkward API, and we don't know of existing use cases for that much
    //       scratch memory.
    // r[8]: Scratch register, replaces M[1]
    // r[9]: Scratch register, replaces M[2]
    // r[10]: ubpf makes this a pointer to the end of the stack.

    const REG_A: u8 = 0;
    const REG_X: u8 = 6;

    // Map from jump targets in the cbpf to a list of jump
    // instructions in the epbf that target it.  When you figure
    // out what the offset of the target is in the ebpf, you need
    // to patch the jump instructions to target it correctly.
    let mut to_be_patched: HashMap<i16, Vec<usize>> = HashMap::new();

    let mut ebpf_code: Vec<bpf_insn> = vec![];
    for (i, bpf_instruction) in bpf_code.iter().enumerate() {
        match bpf_class(bpf_instruction) {
            BPF_ALU => match bpf_op(bpf_instruction) {
                BPF_ADD | BPF_SUB | BPF_MUL | BPF_DIV | BPF_AND | BPF_OR | BPF_XOR | BPF_LSH
                | BPF_RSH => {
                    let mut e_instr = bpf_insn {
                        code: (BPF_ALU | bpf_op(bpf_instruction) | bpf_src(bpf_instruction)) as u8,
                        ..Default::default()
                    };
                    e_instr.set_dst_reg(REG_A);
                    if bpf_src(bpf_instruction) == BPF_K {
                        e_instr.imm = bpf_instruction.k as i32;
                    } else {
                        e_instr.set_src_reg(REG_X);
                    }
                    ebpf_code.push(e_instr);
                }
                BPF_NEG => {
                    let mut e_instr =
                        bpf_insn { code: (BPF_ALU | BPF_NEG) as u8, ..Default::default() };
                    e_instr.set_src_reg(REG_A);
                    e_instr.set_dst_reg(REG_A);
                    ebpf_code.push(e_instr);
                }
                _ => {
                    return Err(UnrecognizedCbpfError {
                        element_type: "op".to_string(),
                        value: format!("{}", bpf_op(bpf_instruction)),
                        op: "alu".to_string(),
                    });
                }
            },
            BPF_LD => {
                match bpf_addressing_mode(bpf_instruction) {
                    BPF_ABS => {
                        // A load from a given address maps in a
                        // very straightforward way.
                        let mut e_instr = bpf_insn {
                            code: (BPF_LDX | BPF_MEM | bpf_size(bpf_instruction)) as u8,
                            ..Default::default()
                        };
                        e_instr.set_dst_reg(REG_A);
                        e_instr.set_src_reg(1);
                        e_instr.off = bpf_instruction.k as i16;
                        ebpf_code.push(e_instr);
                    }
                    BPF_IMM => {
                        let mut e_instr = bpf_insn {
                            code: (BPF_LDX | BPF_IMM | bpf_size(bpf_instruction)) as u8,
                            imm: bpf_instruction.k as i32,
                            ..Default::default()
                        };
                        e_instr.set_dst_reg(REG_A);
                        ebpf_code.push(e_instr);
                    }
                    BPF_MEM => {
                        // cbpf programs running on Linux have a 16 word scratch memory.  ubpf
                        // only has scratch registers and the data passed in.  For now, we use
                        // the extra registers and hope that no one needs more than 3 of them.
                        // Can be replaced by allocating the incoming data into a buffer that
                        // has an extra 16 words, and using those words.  However, this makes an
                        // awkward API, and we don't know of existing use cases for that much
                        // scratch memory.
                        if bpf_instruction.k > 2 {
                            return Err(ScratchBufferOverflow);
                        }
                        let mut e_instr = bpf_insn {
                            code: (BPF_ALU | BPF_MOV | BPF_X) as u8,
                            ..Default::default()
                        };
                        e_instr.set_dst_reg(REG_A);
                        // See comment on reg 7 above.
                        e_instr.set_src_reg((bpf_instruction.k + 7) as u8);
                        ebpf_code.push(e_instr);
                    }
                    _ => {
                        return Err(UnrecognizedCbpfError {
                            element_type: "mode".to_string(),
                            value: format!("{}", bpf_addressing_mode(bpf_instruction)),
                            op: "ld".to_string(),
                        });
                    }
                }
            }
            BPF_JMP => {
                match bpf_op(bpf_instruction) {
                    BPF_JA => {
                        let j_instr = bpf_insn {
                            code: (BPF_JMP | BPF_JA | bpf_src(bpf_instruction)) as u8,
                            off: bpf_instruction.k as i16,
                            ..Default::default()
                        };
                        prep_patch(&mut to_be_patched, (i as i16) + j_instr.off, ebpf_code.len());
                        ebpf_code.push(j_instr);
                    }
                    _ => {
                        // In cbpf, jmps have a jump-if-true and
                        // jump-if-false branch.  ebpf only has
                        // jump-if-true.  Every cbpf jmp therefore turns
                        // into two instructions: a jmp equivalent to the
                        // original jump-if-true; if the condition
                        // evaluates to false, which falls on failure to
                        // an unconditional jump to the original
                        // jump-if-false target.
                        let mut jt_instr = bpf_insn {
                            code: (BPF_JMP32 | bpf_op(bpf_instruction) | bpf_src(bpf_instruction))
                                as u8,
                            off: bpf_instruction.jt as i16,
                            ..Default::default()
                        };
                        if bpf_src(bpf_instruction) == BPF_K {
                            jt_instr.imm = bpf_instruction.k as i32;
                        } else {
                            jt_instr.set_src_reg(REG_X);
                        }
                        jt_instr.set_dst_reg(REG_A);
                        prep_patch(&mut to_be_patched, (i as i16) + jt_instr.off, ebpf_code.len());

                        ebpf_code.push(jt_instr);

                        // Jump if false
                        let jf_instr = bpf_insn {
                            code: (BPF_JMP | BPF_JA) as u8,
                            off: bpf_instruction.jf as i16,
                            ..Default::default()
                        };
                        prep_patch(&mut to_be_patched, (i as i16) + jf_instr.off, ebpf_code.len());

                        ebpf_code.push(jf_instr);
                    }
                }
            }
            BPF_MISC => {
                let mut e_instr =
                    bpf_insn { code: (BPF_ALU | BPF_MOV | BPF_X) as u8, ..Default::default() };

                match bpf_op(bpf_instruction) {
                    BPF_TAX => {
                        e_instr.set_src_reg(REG_A);
                        e_instr.set_dst_reg(REG_X);
                    }
                    BPF_TXA => {
                        e_instr.set_src_reg(REG_X);
                        e_instr.set_dst_reg(REG_A);
                    }
                    _ => {
                        return Err(UnrecognizedCbpfError {
                            element_type: "op".to_string(),
                            value: format!("{}", bpf_op(bpf_instruction)),
                            op: "misc".to_string(),
                        });
                    }
                }
                ebpf_code.push(e_instr);
            }

            BPF_ST => {
                match bpf_addressing_mode(bpf_instruction) {
                    BPF_IMM => {
                        // Only one addressing mode, because there is only one possible destination type.
                        if bpf_instruction.k > 2 {
                            return Err(ScratchBufferOverflow);
                        }
                        let mut e_instr = bpf_insn {
                            code: (BPF_ALU | BPF_MOV | BPF_X) as u8,
                            ..Default::default()
                        };
                        e_instr.set_dst_reg((bpf_instruction.k + 7) as u8); // See comment on reg 7 above.
                        e_instr.set_src_reg(REG_A);
                        ebpf_code.push(e_instr);
                    }
                    _ => {
                        return Err(UnrecognizedCbpfError {
                            element_type: "mode".to_string(),
                            value: format!("{}", bpf_addressing_mode(bpf_instruction)),
                            op: "st".to_string(),
                        });
                    }
                }
            }
            BPF_RET => {
                if bpf_rval(bpf_instruction) != BPF_K && bpf_rval(bpf_instruction) != BPF_A {
                    return Err(UnrecognizedCbpfError {
                        element_type: "mode".to_string(),
                        value: format!("{}", bpf_addressing_mode(bpf_instruction)),
                        op: "ret".to_string(),
                    });
                }
                if bpf_rval(bpf_instruction) == BPF_K {
                    // We're returning a particular value instead of the contents
                    // of the return register, so load that value into the return
                    // register
                    // NB: ubpf only supports loading 64-bit immediates.  This op ors
                    // together this instruction's immediate for the low 32
                    // bits with the following instruction's immediate for the high
                    // 32 bits.
                    let mut ld_instr = bpf_insn {
                        code: (BPF_LD | BPF_IMM | BPF_DW) as u8,
                        imm: bpf_instruction.k as i32,
                        ..Default::default()
                    };
                    ld_instr.set_dst_reg(REG_A);
                    ebpf_code.push(ld_instr);

                    // And the high 32 bits...
                    let dummy_instr: bpf_insn = Default::default();
                    ebpf_code.push(dummy_instr);
                }

                let ret_instr = bpf_insn { code: (BPF_JMP | BPF_EXIT) as u8, ..Default::default() };

                ebpf_code.push(ret_instr);
            }
            _ => {
                return Err(UnrecognizedCbpfError {
                    element_type: "class".to_string(),
                    value: format!("{}", bpf_class(bpf_instruction)),
                    op: "???".to_string(),
                });
            }
        }
        if to_be_patched.contains_key(&(i as i16)) {
            for idx in to_be_patched.get(&(i as i16)).unwrap() {
                ebpf_code[*idx].off = (ebpf_code.len() - *idx - 1) as i16;
            }
            to_be_patched.remove(&(i as i16));
        }
    }
    Ok(ebpf_code)
}
