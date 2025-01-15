// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use linux_uapi::{
    bpf_insn, sock_filter, BPF_A, BPF_ABS, BPF_ADD, BPF_ALU, BPF_ALU64, BPF_AND, BPF_B, BPF_DIV,
    BPF_EXIT, BPF_H, BPF_IMM, BPF_IND, BPF_JA, BPF_JEQ, BPF_JGE, BPF_JGT, BPF_JLE, BPF_JLT,
    BPF_JMP, BPF_JMP32, BPF_JNE, BPF_JSET, BPF_K, BPF_LD, BPF_LDX, BPF_LEN, BPF_LSH, BPF_MEM,
    BPF_MISC, BPF_MOV, BPF_MSH, BPF_MUL, BPF_NEG, BPF_OR, BPF_RET, BPF_RSH, BPF_ST, BPF_STX,
    BPF_SUB, BPF_TAX, BPF_TXA, BPF_W, BPF_X, BPF_XOR,
};
use std::collections::HashMap;

use crate::program::{link_program, BpfProgramContext, EbpfProgram, ProgramArgument};
use crate::verifier::{
    verify_program, CallingContext, NullVerifierLogger, Type, VerifiedEbpfProgram,
};
use crate::visitor::Register;
use crate::EbpfError;
use crate::EbpfError::*;

const CBPF_WORD_SIZE: u32 = 4;

// cBPF supports 16 words for scratch memory.
const CBPF_SCRATCH_SIZE: u32 = 16;

pub enum CbpfLenInstruction {
    Static { len: i32 },
    ContextField { offset: i16 },
}

pub struct CbpfConfig {
    pub len: CbpfLenInstruction,
    pub allow_msh: bool,
}

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

/// Returns offset for the scratch memory with the specified address.
fn cbpf_scratch_offset(addr: u32) -> Result<i16, EbpfError> {
    if addr < CBPF_SCRATCH_SIZE {
        Ok((-(CBPF_SCRATCH_SIZE as i16) + addr as i16) * CBPF_WORD_SIZE as i16)
    } else {
        Err(EbpfError::InvalidCbpfScratchOffset(addr))
    }
}

const fn new_bpf_insn(code: u32, dst: Register, src: Register, offset: i16, imm: i32) -> bpf_insn {
    bpf_insn {
        code: code as u8,
        _bitfield_1: linux_uapi::__BindgenBitfieldUnit::new([dst | src << 4]),
        off: offset,
        imm,
    }
}

/// Transforms a program in classic BPF (cbpf, as stored in struct
/// sock_filter) to extended BPF (as stored in struct bpf_insn).
/// The bpf_code parameter is kept as an array for easy transfer
/// via FFI.  This currently only allows the subset of BPF permitted
/// by seccomp(2).
fn cbpf_to_ebpf(bpf_code: &[sock_filter], config: &CbpfConfig) -> Result<Vec<bpf_insn>, EbpfError> {
    // There are only two BPF registers, A and X. There are 10
    // EBPF registers, numbered 0-9.  We map between the two as
    // follows:

    // R0: Mapped to A.
    const REG_A: u8 = 0;

    // R1: Incoming argument pointing at the packet context (e.g. `sk_buff`). Moved to R6.
    const REG_ARG1: u8 = 1;

    // R6: Pointer to the program context. Initially passed as the first argument. Implicitly
    //     used by eBPF when executing the legacy packet access instructions (`BPF_LD | BPF_ABS`
    //     and `BPF_LD | BPF_IND`).
    const REG_CONTEXT: u8 = 6;

    // R7: Temp register used in the `BPF_MSH` implementation.
    const REG_TMP: u8 = 7;

    // R9: Mapped to X
    const REG_X: u8 = 9;

    // R10: Const stack pointer. cBFP scratch memory (16 words) is stored on top of the stack.
    const REG_STACK: u8 = 10;

    // Map from jump targets in the cbpf to a list of jump instructions in the epbf that target
    // it. When you figure out what the offset of the target is in the ebpf, you need to patch the
    // jump instructions to target it correctly.
    let mut to_be_patched: HashMap<usize, Vec<usize>> = HashMap::new();

    let mut ebpf_code: Vec<bpf_insn> = vec![];
    ebpf_code.reserve(bpf_code.len() * 2 + 2);

    // Save the arguments to registers that won't get clobbered by `BPF_LD`.
    ebpf_code.push(new_bpf_insn(BPF_ALU64 | BPF_MOV | BPF_X, REG_CONTEXT, REG_ARG1, 0, 0));

    // Reset A to 0. This is necessary in case one of the load operation exits prematurely.
    ebpf_code.push(new_bpf_insn(BPF_ALU | BPF_MOV | BPF_K, REG_A, 0, 0, 0));

    for (i, bpf_instruction) in bpf_code.iter().enumerate() {
        // Update instructions processed previously that jump to the current one.
        if let Some((_, entries)) = to_be_patched.remove_entry(&i) {
            for index in entries {
                ebpf_code[index].off = (ebpf_code.len() - index - 1) as i16;
            }
        }

        // Helper to queue a new entry into `to_be_patched`.
        let mut prep_patch = |cbpf_offset: usize, ebpf_source: usize| -> Result<(), EbpfError> {
            let cbpf_target = i + 1 + cbpf_offset;
            if cbpf_target >= bpf_code.len() {
                return Err(EbpfError::InvalidCbpfJumpOffset(cbpf_offset as u32));
            }
            to_be_patched.entry(cbpf_target).or_insert_with(Vec::new).push(ebpf_source);
            Ok(())
        };

        match bpf_class(bpf_instruction) {
            BPF_ALU => match bpf_op(bpf_instruction) {
                BPF_ADD | BPF_SUB | BPF_MUL | BPF_DIV | BPF_AND | BPF_OR | BPF_XOR | BPF_LSH
                | BPF_RSH => {
                    let e_instr = if bpf_src(bpf_instruction) == BPF_K {
                        new_bpf_insn(
                            bpf_instruction.code as u32,
                            REG_A,
                            0,
                            0,
                            bpf_instruction.k as i32,
                        )
                    } else {
                        new_bpf_insn(bpf_instruction.code as u32, REG_A, REG_X, 0, 0)
                    };
                    ebpf_code.push(e_instr);
                }
                BPF_NEG => {
                    ebpf_code.push(new_bpf_insn(BPF_ALU | BPF_NEG, REG_A, REG_A, 0, 0));
                }
                _ => return Err(InvalidCbpfInstruction(bpf_instruction.code)),
            },
            class @ (BPF_LD | BPF_LDX) => {
                let dst_reg = if class == BPF_LDX { REG_X } else { REG_A };

                let mode = bpf_addressing_mode(bpf_instruction);
                let size = bpf_size(bpf_instruction);

                // Half-word (`BPF_H`) and byte (`BPF_B`) loads are allowed only for `BPD_ABS` and
                // `BPD_IND`. Also `BPD_ABS` and `BPD_IND` are not allowed with `BPD_LDX`.
                // `BPF_LEN`, `BPF_IMM` and `BPF_MEM` loads should be word-sized (i.e. `BPF_W`).
                // `BPF_MSH` is allowed only with `BPF_B` and `BPF_LDX`.
                match (size, mode, class) {
                    (BPF_H | BPF_B | BPF_W, BPF_ABS | BPF_IND, BPF_LD) => (),
                    (BPF_W, BPF_LEN | BPF_IMM | BPF_MEM, BPF_LD | BPF_LDX) => (),
                    (BPF_B, BPF_MSH, BPF_LDX) if config.allow_msh => (),
                    _ => return Err(InvalidCbpfInstruction(bpf_instruction.code)),
                };

                let k = bpf_instruction.k;

                match mode {
                    BPF_ABS => {
                        ebpf_code.push(new_bpf_insn(
                            BPF_LD | BPF_ABS | size,
                            REG_A,
                            0,
                            0,
                            k as i32,
                        ));
                    }
                    BPF_IND => {
                        ebpf_code.push(new_bpf_insn(
                            BPF_LD | BPF_IND | size,
                            REG_A,
                            REG_X,
                            0,
                            k as i32,
                        ));
                    }
                    BPF_IMM => {
                        let imm = k as i32;
                        ebpf_code.push(new_bpf_insn(BPF_ALU | BPF_MOV | BPF_K, dst_reg, 0, 0, imm));
                    }
                    BPF_MEM => {
                        // cBPF's scratch memory is stored in the stack referenced by R10.
                        let offset = cbpf_scratch_offset(k)?;
                        ebpf_code.push(new_bpf_insn(
                            BPF_LDX | BPF_MEM,
                            dst_reg,
                            REG_STACK,
                            offset,
                            0,
                        ));
                    }
                    BPF_LEN => {
                        ebpf_code.push(match config.len {
                            CbpfLenInstruction::Static { len } => {
                                new_bpf_insn(BPF_ALU | BPF_MOV | BPF_K, REG_A, 0, 0, len)
                            }
                            CbpfLenInstruction::ContextField { offset } => new_bpf_insn(
                                BPF_LDX | BPF_MEM | BPF_W,
                                REG_A,
                                REG_CONTEXT,
                                offset,
                                0,
                            ),
                        });
                    }
                    BPF_MSH => {
                        // `BPF_MSH` loads `4 * (P[k:1] & 0xf)`, which translates to 6 instructions.
                        ebpf_code.extend_from_slice(&[
                            // mov TMP, A
                            new_bpf_insn(BPF_ALU | BPF_MOV | BPF_X, REG_TMP, REG_A, 0, 0),
                            // ldpb [k]
                            new_bpf_insn(BPF_LD | BPF_ABS | BPF_B, REG_A, 0, 0, k as i32),
                            // and A, 0xf
                            new_bpf_insn(BPF_ALU | BPF_AND | BPF_K, REG_A, 0, 0, 0x0f),
                            // mul A, 4
                            new_bpf_insn(BPF_ALU | BPF_MUL | BPF_K, REG_A, 0, 0, 4),
                            // mov X, A
                            new_bpf_insn(BPF_ALU | BPF_MOV | BPF_X, REG_X, REG_A, 0, 0),
                            // mov A, TMP
                            new_bpf_insn(BPF_ALU | BPF_MOV | BPF_X, REG_A, REG_TMP, 0, 0),
                        ]);
                    }
                    _ => return Err(InvalidCbpfInstruction(bpf_instruction.code)),
                }
            }
            BPF_JMP => {
                match bpf_op(bpf_instruction) {
                    BPF_JA => {
                        ebpf_code.push(new_bpf_insn(BPF_JMP | BPF_JA, 0, 0, -1, 0));
                        prep_patch(bpf_instruction.k as usize, ebpf_code.len() - 1)?;
                    }
                    op @ (BPF_JGT | BPF_JGE | BPF_JEQ | BPF_JSET) => {
                        // In cBPD, JMPs have a jump-if-true and jump-if-false branch. eBPF only
                        // has jump-if-true. In most cases only one of the two branches actually
                        // jumps (the other one is set to 0). In these cases the instruction can
                        // be translated to 1 eBPF instruction. Otherwise two instructions are
                        // produced in the output.

                        let src = bpf_src(bpf_instruction);
                        let sock_filter { k, jt, jf, .. } = *bpf_instruction;
                        let (src_reg, imm) = if src == BPF_K { (0, k as i32) } else { (REG_X, 0) };

                        // When jumping only for the false case we can negate the comparison
                        // operator to achieve the same effect with a single jump-if-true eBPF
                        // instruction. That doesn't work for `BPF_JSET`. It is handled below
                        // using 2 instructions.
                        if jt == 0 && op != BPF_JSET {
                            let op = match op {
                                BPF_JGT => BPF_JLE,
                                BPF_JGE => BPF_JLT,
                                BPF_JEQ => BPF_JNE,
                                _ => panic!("Unexpected operation: {op:?}"),
                            };

                            ebpf_code.push(new_bpf_insn(
                                BPF_JMP32 | op | src,
                                REG_A,
                                src_reg,
                                -1,
                                imm,
                            ));
                            prep_patch(jf as usize, ebpf_code.len() - 1)?;
                        } else {
                            // Jump if true.
                            ebpf_code.push(new_bpf_insn(
                                BPF_JMP32 | op | src,
                                REG_A,
                                src_reg,
                                -1,
                                imm,
                            ));
                            prep_patch(jt as usize, ebpf_code.len() - 1)?;

                            // Jump if false. Jumps with 0 offset are no-op and can be omitted.
                            if jf > 0 {
                                ebpf_code.push(new_bpf_insn(BPF_JMP | BPF_JA, 0, 0, -1, 0));
                                prep_patch(jf as usize, ebpf_code.len() - 1)?;
                            }
                        }
                    }
                    _ => return Err(InvalidCbpfInstruction(bpf_instruction.code)),
                }
            }
            BPF_MISC => match bpf_op(bpf_instruction) {
                BPF_TAX => {
                    ebpf_code.push(new_bpf_insn(BPF_ALU | BPF_MOV | BPF_X, REG_X, REG_A, 0, 0));
                }
                BPF_TXA => {
                    ebpf_code.push(new_bpf_insn(BPF_ALU | BPF_MOV | BPF_X, REG_A, REG_X, 0, 0));
                }
                _ => return Err(InvalidCbpfInstruction(bpf_instruction.code)),
            },

            class @ (BPF_ST | BPF_STX) => {
                if bpf_addressing_mode(bpf_instruction) != 0 || bpf_size(bpf_instruction) != 0 {
                    return Err(InvalidCbpfInstruction(bpf_instruction.code));
                }

                // cBPF's scratch memory is stored in the stack referenced by R10.
                let src_reg = if class == BPF_STX { REG_X } else { REG_A };
                let offset = cbpf_scratch_offset(bpf_instruction.k)?;
                ebpf_code.push(new_bpf_insn(
                    BPF_STX | BPF_MEM | BPF_W,
                    REG_STACK,
                    src_reg,
                    offset,
                    0,
                ));
            }
            BPF_RET => {
                match bpf_rval(bpf_instruction) {
                    BPF_K => {
                        // We're returning a particular value instead of the contents of the
                        // return register, so load that value into the return register.
                        let imm = bpf_instruction.k as i32;
                        ebpf_code.push(new_bpf_insn(BPF_ALU | BPF_MOV | BPF_IMM, REG_A, 0, 0, imm));
                    }
                    BPF_A => (),
                    _ => return Err(InvalidCbpfInstruction(bpf_instruction.code)),
                };

                ebpf_code.push(new_bpf_insn(BPF_JMP | BPF_EXIT, 0, 0, 0, 0));
            }
            _ => return Err(InvalidCbpfInstruction(bpf_instruction.code)),
        }
    }

    assert!(to_be_patched.is_empty());

    Ok(ebpf_code)
}

/// Instantiates an EbpfProgram given a cbpf original that will work with a packet of the
/// specified type.
pub fn convert_and_verify_cbpf(
    bpf_code: &[sock_filter],
    packet_type: Type,
    config: &CbpfConfig,
) -> Result<VerifiedEbpfProgram, EbpfError> {
    let context = CallingContext {
        maps: vec![],
        helpers: HashMap::new(),
        args: vec![packet_type.clone()],
        packet_type: Some(packet_type),
    };
    let ebpf_code = cbpf_to_ebpf(bpf_code, config)?;
    verify_program(ebpf_code, context, &mut NullVerifierLogger)
}

/// Converts, verifies and links a cBPF program for execution in the specified context.
pub fn convert_and_link_cbpf<C: BpfProgramContext>(
    bpf_code: &[sock_filter],
) -> Result<EbpfProgram<C>, EbpfError> {
    let verified =
        convert_and_verify_cbpf(bpf_code, C::Packet::get_type().clone(), C::CBPF_CONFIG)?;
    link_program(&verified, &[], &[], HashMap::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    pub const TEST_CONFIG: CbpfConfig =
        CbpfConfig { len: CbpfLenInstruction::Static { len: 0 }, allow_msh: false };

    #[test]
    fn test_cbpf_to_ebpf() {
        // Jump to the next instruction.
        assert_eq!(
            cbpf_to_ebpf(
                &vec![
                    sock_filter { code: (BPF_JMP | BPF_JA) as u16, jt: 0, jf: 0, k: 0 },
                    sock_filter { code: (BPF_RET | BPF_A) as u16, jt: 0, jf: 0, k: 0 },
                ],
                &TEST_CONFIG
            ),
            Ok(vec![
                new_bpf_insn(BPF_ALU64 | BPF_MOV | BPF_X, 6, 1, 0, 0),
                new_bpf_insn(BPF_ALU | BPF_MOV | BPF_K, 0, 0, 0, 0),
                new_bpf_insn(BPF_JMP | BPF_JA, 0, 0, 0, 0),
                new_bpf_insn(BPF_JMP | BPF_EXIT, 0, 0, 0, 0),
            ]),
        );

        // Jump after last instruction.
        assert_eq!(
            cbpf_to_ebpf(
                &vec![
                    sock_filter { code: (BPF_JMP | BPF_JA) as u16, jt: 0, jf: 0, k: 1 },
                    sock_filter { code: (BPF_RET | BPF_A) as u16, jt: 0, jf: 0, k: 0 },
                ],
                &TEST_CONFIG
            ),
            Err(EbpfError::InvalidCbpfJumpOffset(1)),
        );

        // Jump out of bounds.
        assert_eq!(
            cbpf_to_ebpf(
                &vec![sock_filter { code: (BPF_JMP | BPF_JA) as u16, jt: 0, jf: 0, k: 0xffffffff }],
                &TEST_CONFIG
            ),
            Err(EbpfError::InvalidCbpfJumpOffset(0xffffffff)),
        );

        // BPF_JNE is allowed only in eBPF.
        assert_eq!(
            cbpf_to_ebpf(
                &vec![
                    sock_filter { code: (BPF_JMP | BPF_JNE) as u16, jt: 0, jf: 0, k: 0 },
                    sock_filter { code: (BPF_RET | BPF_A) as u16, jt: 0, jf: 0, k: 0 },
                ],
                &TEST_CONFIG
            ),
            Err(EbpfError::InvalidCbpfInstruction((BPF_JMP | BPF_JNE) as u16)),
        );

        // BPF_JEQ is supported in BPF.
        assert_eq!(
            cbpf_to_ebpf(
                &vec![
                    sock_filter { code: (BPF_JMP | BPF_JEQ) as u16, jt: 1, jf: 0, k: 0 },
                    sock_filter { code: (BPF_RET | BPF_A) as u16, jt: 0, jf: 0, k: 0 },
                    sock_filter { code: (BPF_RET | BPF_A) as u16, jt: 0, jf: 0, k: 0 },
                ],
                &TEST_CONFIG
            ),
            Ok(vec![
                new_bpf_insn(BPF_ALU64 | BPF_MOV | BPF_X, 6, 1, 0, 0),
                new_bpf_insn(BPF_ALU | BPF_MOV | BPF_K, 0, 0, 0, 0),
                new_bpf_insn(BPF_JMP32 | BPF_JEQ, 0, 0, 1, 0),
                new_bpf_insn(BPF_JMP | BPF_EXIT, 0, 0, 0, 0),
                new_bpf_insn(BPF_JMP | BPF_EXIT, 0, 0, 0, 0),
            ]),
        );

        // Make sure the jump is translated correctly when the jump target produces 2 instructions.
        assert_eq!(
            cbpf_to_ebpf(
                &vec![
                    sock_filter { code: (BPF_JMP | BPF_JA) as u16, jt: 0, jf: 0, k: 0 },
                    sock_filter { code: (BPF_RET | BPF_K) as u16, jt: 0, jf: 0, k: 1 },
                ],
                &TEST_CONFIG
            ),
            Ok(vec![
                new_bpf_insn(BPF_ALU64 | BPF_MOV | BPF_X, 6, 1, 0, 0),
                new_bpf_insn(BPF_ALU | BPF_MOV | BPF_K, 0, 0, 0, 0),
                new_bpf_insn(BPF_JMP | BPF_JA, 0, 0, 0, 0),
                new_bpf_insn(BPF_ALU | BPF_MOV | BPF_IMM, 0, 0, 0, 1),
                new_bpf_insn(BPF_JMP | BPF_EXIT, 0, 0, 0, 0),
            ]),
        );

        // BPF_MEM access.
        assert_eq!(
            cbpf_to_ebpf(
                &vec![
                    sock_filter { code: (BPF_LD | BPF_MEM) as u16, jt: 0, jf: 0, k: 0 },
                    sock_filter { code: (BPF_LDX | BPF_MEM) as u16, jt: 0, jf: 0, k: 15 },
                    sock_filter { code: BPF_ST as u16, jt: 0, jf: 0, k: 0 },
                    sock_filter { code: BPF_STX as u16, jt: 0, jf: 0, k: 15 },
                ],
                &TEST_CONFIG
            ),
            Ok(vec![
                new_bpf_insn(BPF_ALU64 | BPF_MOV | BPF_X, 6, 1, 0, 0),
                new_bpf_insn(BPF_ALU | BPF_MOV | BPF_K, 0, 0, 0, 0),
                new_bpf_insn(BPF_LDX | BPF_MEM | BPF_W, 0, 10, -64, 0),
                new_bpf_insn(BPF_LDX | BPF_MEM | BPF_W, 9, 10, -4, 0),
                new_bpf_insn(BPF_STX | BPF_MEM | BPF_W, 10, 0, -64, 0),
                new_bpf_insn(BPF_STX | BPF_MEM | BPF_W, 10, 9, -4, 0),
            ]),
        );

        // BPF_MEM access out of bounds.
        assert_eq!(
            cbpf_to_ebpf(
                &vec![sock_filter { code: (BPF_LD | BPF_MEM) as u16, jt: 0, jf: 0, k: 17 }],
                &TEST_CONFIG
            ),
            Err(EbpfError::InvalidCbpfScratchOffset(17)),
        );
    }
}
