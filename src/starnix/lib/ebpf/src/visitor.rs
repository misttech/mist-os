// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    DataWidth, BPF_ABS, BPF_ADD, BPF_ALU, BPF_ALU64, BPF_AND, BPF_ARSH, BPF_ATOMIC, BPF_B,
    BPF_CALL, BPF_CLS_MASK, BPF_CMPXCHG, BPF_DIV, BPF_DW, BPF_END, BPF_EXIT, BPF_FETCH, BPF_H,
    BPF_IND, BPF_JA, BPF_JEQ, BPF_JGE, BPF_JGT, BPF_JLE, BPF_JLT, BPF_JMP, BPF_JMP32, BPF_JNE,
    BPF_JSET, BPF_JSGE, BPF_JSGT, BPF_JSLE, BPF_JSLT, BPF_LD, BPF_LDDW, BPF_LDX,
    BPF_LOAD_STORE_MASK, BPF_LSH, BPF_MEM, BPF_MOD, BPF_MOV, BPF_MUL, BPF_NEG, BPF_OR, BPF_RSH,
    BPF_SIZE_MASK, BPF_SRC_MASK, BPF_SRC_REG, BPF_ST, BPF_STX, BPF_SUB, BPF_SUB_OP_MASK, BPF_TO_BE,
    BPF_W, BPF_XCHG, BPF_XOR,
};
use linux_uapi::bpf_insn;

/// The index into the registers. 10 is the stack pointer.
pub type Register = u8;

/// The index into the program
pub type ProgramCounter = usize;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Source {
    Reg(Register),
    Value(u64),
}

impl From<&bpf_insn> for Source {
    fn from(instruction: &bpf_insn) -> Self {
        if instruction.code & BPF_SRC_MASK == BPF_SRC_REG {
            Self::Reg(instruction.src_reg())
        } else {
            Self::Value(instruction.imm as u64)
        }
    }
}

pub trait BpfVisitor {
    type Context<'a>;

    fn add<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String>;
    fn add64<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String>;
    fn and<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String>;
    fn and64<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String>;
    fn arsh<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String>;
    fn arsh64<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String>;
    fn div<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String>;
    fn div64<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String>;
    fn lsh<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String>;
    fn lsh64<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String>;
    fn r#mod<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String>;
    fn mod64<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String>;
    fn mov<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String>;
    fn mov64<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String>;
    fn mul<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String>;
    fn mul64<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String>;
    fn or<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String>;
    fn or64<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String>;
    fn rsh<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String>;
    fn rsh64<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String>;
    fn sub<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String>;
    fn sub64<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String>;
    fn xor<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String>;
    fn xor64<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String>;

    fn neg<'a>(&mut self, context: &mut Self::Context<'a>, dst: Register) -> Result<(), String>;
    fn neg64<'a>(&mut self, context: &mut Self::Context<'a>, dst: Register) -> Result<(), String>;

    fn be<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        width: DataWidth,
    ) -> Result<(), String>;
    fn le<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        width: DataWidth,
    ) -> Result<(), String>;

    fn call_external<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        index: u32,
    ) -> Result<(), String>;

    fn exit<'a>(&mut self, context: &mut Self::Context<'a>) -> Result<(), String>;

    fn jump<'a>(&mut self, context: &mut Self::Context<'a>, offset: i16) -> Result<(), String>;

    fn jeq<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String>;
    fn jeq64<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String>;
    fn jne<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String>;
    fn jne64<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String>;
    fn jge<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String>;
    fn jge64<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String>;
    fn jgt<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String>;
    fn jgt64<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String>;
    fn jle<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String>;
    fn jle64<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String>;
    fn jlt<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String>;
    fn jlt64<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String>;
    fn jsge<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String>;
    fn jsge64<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String>;
    fn jsgt<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String>;
    fn jsgt64<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String>;
    fn jsle<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String>;
    fn jsle64<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String>;
    fn jslt<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String>;
    fn jslt64<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String>;
    fn jset<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String>;
    fn jset64<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String>;

    fn atomic_add<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        fetch: bool,
        dst: Register,
        offset: i16,
        src: Register,
    ) -> Result<(), String>;

    fn atomic_add64<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        fetch: bool,
        dst: Register,
        offset: i16,
        src: Register,
    ) -> Result<(), String>;

    fn atomic_and<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        fetch: bool,
        dst: Register,
        offset: i16,
        src: Register,
    ) -> Result<(), String>;

    fn atomic_and64<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        fetch: bool,
        dst: Register,
        offset: i16,
        src: Register,
    ) -> Result<(), String>;

    fn atomic_or<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        fetch: bool,
        dst: Register,
        offset: i16,
        src: Register,
    ) -> Result<(), String>;

    fn atomic_or64<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        fetch: bool,
        dst: Register,
        offset: i16,
        src: Register,
    ) -> Result<(), String>;

    fn atomic_xor<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        fetch: bool,
        dst: Register,
        offset: i16,
        src: Register,
    ) -> Result<(), String>;

    fn atomic_xor64<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        fetch: bool,
        dst: Register,
        offset: i16,
        src: Register,
    ) -> Result<(), String>;

    fn atomic_xchg<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        fetch: bool,
        dst: Register,
        offset: i16,
        src: Register,
    ) -> Result<(), String>;

    fn atomic_xchg64<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        fetch: bool,
        dst: Register,
        offset: i16,
        src: Register,
    ) -> Result<(), String>;

    fn atomic_cmpxchg<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        offset: i16,
        src: Register,
    ) -> Result<(), String>;

    fn atomic_cmpxchg64<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        offset: i16,
        src: Register,
    ) -> Result<(), String>;

    fn load<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        offset: i16,
        src: Register,
        width: DataWidth,
    ) -> Result<(), String>;

    fn load64<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        value: u64,
        jump_offset: i16,
    ) -> Result<(), String>;

    fn load_from_packet<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Register,
        offset: i32,
        register_offset: Option<Register>,
        width: DataWidth,
    ) -> Result<(), String>;

    fn store<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        offset: i16,
        src: Source,
        width: DataWidth,
    ) -> Result<(), String>;

    fn visit<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        code: &[bpf_insn],
    ) -> Result<(), String> {
        if code.is_empty() {
            return Err("incomplete instruction".to_string());
        }
        let instruction = &code[0];
        let invalid_op_code =
            || -> Result<(), String> { Err(format!("invalid op code {:x}", instruction.code)) };

        let class = instruction.code & BPF_CLS_MASK;
        match class {
            BPF_ALU64 | BPF_ALU => {
                let alu_op = instruction.code & BPF_SUB_OP_MASK;
                let is_64 = class == BPF_ALU64;
                match alu_op {
                    BPF_ADD => {
                        if is_64 {
                            return self.add64(
                                context,
                                instruction.dst_reg(),
                                Source::from(instruction),
                            );
                        } else {
                            return self.add(
                                context,
                                instruction.dst_reg(),
                                Source::from(instruction),
                            );
                        }
                    }
                    BPF_SUB => {
                        if is_64 {
                            return self.sub64(
                                context,
                                instruction.dst_reg(),
                                Source::from(instruction),
                            );
                        } else {
                            return self.sub(
                                context,
                                instruction.dst_reg(),
                                Source::from(instruction),
                            );
                        }
                    }
                    BPF_MUL => {
                        if is_64 {
                            return self.mul64(
                                context,
                                instruction.dst_reg(),
                                Source::from(instruction),
                            );
                        } else {
                            return self.mul(
                                context,
                                instruction.dst_reg(),
                                Source::from(instruction),
                            );
                        }
                    }
                    BPF_DIV => {
                        if is_64 {
                            return self.div64(
                                context,
                                instruction.dst_reg(),
                                Source::from(instruction),
                            );
                        } else {
                            return self.div(
                                context,
                                instruction.dst_reg(),
                                Source::from(instruction),
                            );
                        }
                    }
                    BPF_OR => {
                        if is_64 {
                            return self.or64(
                                context,
                                instruction.dst_reg(),
                                Source::from(instruction),
                            );
                        } else {
                            return self.or(
                                context,
                                instruction.dst_reg(),
                                Source::from(instruction),
                            );
                        }
                    }
                    BPF_AND => {
                        if is_64 {
                            return self.and64(
                                context,
                                instruction.dst_reg(),
                                Source::from(instruction),
                            );
                        } else {
                            return self.and(
                                context,
                                instruction.dst_reg(),
                                Source::from(instruction),
                            );
                        }
                    }
                    BPF_LSH => {
                        if is_64 {
                            return self.lsh64(
                                context,
                                instruction.dst_reg(),
                                Source::from(instruction),
                            );
                        } else {
                            return self.lsh(
                                context,
                                instruction.dst_reg(),
                                Source::from(instruction),
                            );
                        }
                    }
                    BPF_RSH => {
                        if is_64 {
                            return self.rsh64(
                                context,
                                instruction.dst_reg(),
                                Source::from(instruction),
                            );
                        } else {
                            return self.rsh(
                                context,
                                instruction.dst_reg(),
                                Source::from(instruction),
                            );
                        }
                    }
                    BPF_MOD => {
                        if is_64 {
                            return self.mod64(
                                context,
                                instruction.dst_reg(),
                                Source::from(instruction),
                            );
                        } else {
                            return self.r#mod(
                                context,
                                instruction.dst_reg(),
                                Source::from(instruction),
                            );
                        }
                    }
                    BPF_XOR => {
                        if is_64 {
                            return self.xor64(
                                context,
                                instruction.dst_reg(),
                                Source::from(instruction),
                            );
                        } else {
                            return self.xor(
                                context,
                                instruction.dst_reg(),
                                Source::from(instruction),
                            );
                        }
                    }
                    BPF_MOV => {
                        if is_64 {
                            return self.mov64(
                                context,
                                instruction.dst_reg(),
                                Source::from(instruction),
                            );
                        } else {
                            return self.mov(
                                context,
                                instruction.dst_reg(),
                                Source::from(instruction),
                            );
                        }
                    }
                    BPF_ARSH => {
                        if is_64 {
                            return self.arsh64(
                                context,
                                instruction.dst_reg(),
                                Source::from(instruction),
                            );
                        } else {
                            return self.arsh(
                                context,
                                instruction.dst_reg(),
                                Source::from(instruction),
                            );
                        }
                    }

                    BPF_NEG => {
                        if is_64 {
                            return self.neg64(context, instruction.dst_reg());
                        } else {
                            return self.neg(context, instruction.dst_reg());
                        }
                    }
                    BPF_END => {
                        let is_be = instruction.code & BPF_TO_BE == BPF_TO_BE;
                        let width = match instruction.imm {
                            16 => DataWidth::U16,
                            32 => DataWidth::U32,
                            64 => DataWidth::U64,
                            _ => {
                                return Err(format!(
                                    "invalid width for endianness operation: {}",
                                    instruction.imm
                                ))
                            }
                        };
                        if is_be {
                            return self.be(context, instruction.dst_reg(), width);
                        } else {
                            return self.le(context, instruction.dst_reg(), width);
                        }
                    }
                    _ => return invalid_op_code(),
                }
            }
            BPF_JMP | BPF_JMP32 => {
                let jmp_op = instruction.code & BPF_SUB_OP_MASK;
                let is_64 = class == BPF_JMP;
                match jmp_op {
                    BPF_JEQ => {
                        if is_64 {
                            return self.jeq64(
                                context,
                                instruction.dst_reg(),
                                Source::from(instruction),
                                instruction.off,
                            );
                        } else {
                            return self.jeq(
                                context,
                                instruction.dst_reg(),
                                Source::from(instruction),
                                instruction.off,
                            );
                        }
                    }
                    BPF_JGT => {
                        if is_64 {
                            return self.jgt64(
                                context,
                                instruction.dst_reg(),
                                Source::from(instruction),
                                instruction.off,
                            );
                        } else {
                            return self.jgt(
                                context,
                                instruction.dst_reg(),
                                Source::from(instruction),
                                instruction.off,
                            );
                        }
                    }
                    BPF_JGE => {
                        if is_64 {
                            return self.jge64(
                                context,
                                instruction.dst_reg(),
                                Source::from(instruction),
                                instruction.off,
                            );
                        } else {
                            return self.jge(
                                context,
                                instruction.dst_reg(),
                                Source::from(instruction),
                                instruction.off,
                            );
                        }
                    }
                    BPF_JSET => {
                        if is_64 {
                            return self.jset64(
                                context,
                                instruction.dst_reg(),
                                Source::from(instruction),
                                instruction.off,
                            );
                        } else {
                            return self.jset(
                                context,
                                instruction.dst_reg(),
                                Source::from(instruction),
                                instruction.off,
                            );
                        }
                    }
                    BPF_JNE => {
                        if is_64 {
                            return self.jne64(
                                context,
                                instruction.dst_reg(),
                                Source::from(instruction),
                                instruction.off,
                            );
                        } else {
                            return self.jne(
                                context,
                                instruction.dst_reg(),
                                Source::from(instruction),
                                instruction.off,
                            );
                        }
                    }
                    BPF_JSGT => {
                        if is_64 {
                            return self.jsgt64(
                                context,
                                instruction.dst_reg(),
                                Source::from(instruction),
                                instruction.off,
                            );
                        } else {
                            return self.jsgt(
                                context,
                                instruction.dst_reg(),
                                Source::from(instruction),
                                instruction.off,
                            );
                        }
                    }
                    BPF_JSGE => {
                        if is_64 {
                            return self.jsge64(
                                context,
                                instruction.dst_reg(),
                                Source::from(instruction),
                                instruction.off,
                            );
                        } else {
                            return self.jsge(
                                context,
                                instruction.dst_reg(),
                                Source::from(instruction),
                                instruction.off,
                            );
                        }
                    }
                    BPF_JLT => {
                        if is_64 {
                            return self.jlt64(
                                context,
                                instruction.dst_reg(),
                                Source::from(instruction),
                                instruction.off,
                            );
                        } else {
                            return self.jlt(
                                context,
                                instruction.dst_reg(),
                                Source::from(instruction),
                                instruction.off,
                            );
                        }
                    }
                    BPF_JLE => {
                        if is_64 {
                            return self.jle64(
                                context,
                                instruction.dst_reg(),
                                Source::from(instruction),
                                instruction.off,
                            );
                        } else {
                            return self.jle(
                                context,
                                instruction.dst_reg(),
                                Source::from(instruction),
                                instruction.off,
                            );
                        }
                    }
                    BPF_JSLT => {
                        if is_64 {
                            return self.jslt64(
                                context,
                                instruction.dst_reg(),
                                Source::from(instruction),
                                instruction.off,
                            );
                        } else {
                            return self.jslt(
                                context,
                                instruction.dst_reg(),
                                Source::from(instruction),
                                instruction.off,
                            );
                        }
                    }
                    BPF_JSLE => {
                        if is_64 {
                            return self.jsle64(
                                context,
                                instruction.dst_reg(),
                                Source::from(instruction),
                                instruction.off,
                            );
                        } else {
                            return self.jsle(
                                context,
                                instruction.dst_reg(),
                                Source::from(instruction),
                                instruction.off,
                            );
                        }
                    }

                    BPF_JA => {
                        return self.jump(context, instruction.off);
                    }
                    BPF_CALL => {
                        if instruction.src_reg() == 0 {
                            // Call to external function
                            return self.call_external(context, instruction.imm as u32);
                        }
                        // Unhandled call
                        return Err(format!(
                            "unsupported call with src = {}",
                            instruction.src_reg()
                        ));
                    }
                    BPF_EXIT => {
                        return self.exit(context);
                    }
                    _ => return invalid_op_code(),
                }
            }
            BPF_LD => {
                if instruction.code == BPF_LDDW {
                    if code.len() < 2 {
                        return Err(format!("incomplete lddw"));
                    }
                    let next_instruction = &code[1];
                    let value: u64 = ((instruction.imm as u32) as u64)
                        | (((next_instruction.imm as u32) as u64) << 32);
                    return self.load64(context, instruction.dst_reg(), value, 1);
                }
                let width = match instruction.code & BPF_SIZE_MASK {
                    BPF_B => DataWidth::U8,
                    BPF_H => DataWidth::U16,
                    BPF_W => DataWidth::U32,
                    BPF_DW => DataWidth::U64,
                    _ => unreachable!(),
                };
                let register_offset = match instruction.code & BPF_LOAD_STORE_MASK {
                    BPF_ABS => None,
                    BPF_IND => Some(instruction.src_reg()),
                    _ => return invalid_op_code(),
                };
                return self.load_from_packet(
                    context,
                    // Store the result in r0
                    0,
                    // Read the packet from r6
                    6,
                    instruction.imm,
                    register_offset,
                    width,
                );
            }
            BPF_STX | BPF_ST | BPF_LDX => {
                let width = match instruction.code & BPF_SIZE_MASK {
                    BPF_B => DataWidth::U8,
                    BPF_H => DataWidth::U16,
                    BPF_W => DataWidth::U32,
                    BPF_DW => DataWidth::U64,
                    _ => unreachable!(),
                };
                if class == BPF_LDX {
                    if instruction.code & BPF_LOAD_STORE_MASK != BPF_MEM {
                        // Unsupported instruction.
                        return invalid_op_code();
                    }
                    return self.load(
                        context,
                        instruction.dst_reg(),
                        instruction.off,
                        instruction.src_reg(),
                        width,
                    );
                } else {
                    if instruction.code & BPF_LOAD_STORE_MASK == BPF_MEM {
                        let src = if class == BPF_ST {
                            Source::Value(instruction.imm as u64)
                        } else {
                            Source::Reg(instruction.src_reg())
                        };
                        return self.store(
                            context,
                            instruction.dst_reg(),
                            instruction.off,
                            src,
                            width,
                        );
                    } else if instruction.code & BPF_LOAD_STORE_MASK == BPF_ATOMIC {
                        if !matches!(width, DataWidth::U32 | DataWidth::U64) {
                            return Err(format!(
                                "unsupported atomic operation of width {}",
                                width.bytes()
                            ));
                        }
                        let operation = instruction.imm as u8;
                        let fetch = operation & BPF_FETCH == BPF_FETCH;
                        let is_64 = width == DataWidth::U64;
                        const BPF_ADD_AND_FETCH: u8 = BPF_ADD | BPF_FETCH;
                        const BPF_AND_AND_FETCH: u8 = BPF_AND | BPF_FETCH;
                        const BPF_OR_AND_FETCH: u8 = BPF_OR | BPF_FETCH;
                        const BPF_XOR_AND_FETCH: u8 = BPF_XOR | BPF_FETCH;
                        return match operation {
                            BPF_ADD | BPF_ADD_AND_FETCH => {
                                if is_64 {
                                    self.atomic_add64(
                                        context,
                                        fetch,
                                        instruction.dst_reg(),
                                        instruction.off,
                                        instruction.src_reg(),
                                    )
                                } else {
                                    self.atomic_add(
                                        context,
                                        fetch,
                                        instruction.dst_reg(),
                                        instruction.off,
                                        instruction.src_reg(),
                                    )
                                }
                            }
                            BPF_AND | BPF_AND_AND_FETCH => {
                                if is_64 {
                                    self.atomic_and64(
                                        context,
                                        fetch,
                                        instruction.dst_reg(),
                                        instruction.off,
                                        instruction.src_reg(),
                                    )
                                } else {
                                    self.atomic_and(
                                        context,
                                        fetch,
                                        instruction.dst_reg(),
                                        instruction.off,
                                        instruction.src_reg(),
                                    )
                                }
                            }
                            BPF_OR | BPF_OR_AND_FETCH => {
                                if is_64 {
                                    self.atomic_or64(
                                        context,
                                        fetch,
                                        instruction.dst_reg(),
                                        instruction.off,
                                        instruction.src_reg(),
                                    )
                                } else {
                                    self.atomic_or(
                                        context,
                                        fetch,
                                        instruction.dst_reg(),
                                        instruction.off,
                                        instruction.src_reg(),
                                    )
                                }
                            }
                            BPF_XOR | BPF_XOR_AND_FETCH => {
                                if is_64 {
                                    self.atomic_xor64(
                                        context,
                                        fetch,
                                        instruction.dst_reg(),
                                        instruction.off,
                                        instruction.src_reg(),
                                    )
                                } else {
                                    self.atomic_xor(
                                        context,
                                        fetch,
                                        instruction.dst_reg(),
                                        instruction.off,
                                        instruction.src_reg(),
                                    )
                                }
                            }
                            BPF_XCHG => {
                                if is_64 {
                                    self.atomic_xchg64(
                                        context,
                                        fetch,
                                        instruction.dst_reg(),
                                        instruction.off,
                                        instruction.src_reg(),
                                    )
                                } else {
                                    self.atomic_xchg(
                                        context,
                                        fetch,
                                        instruction.dst_reg(),
                                        instruction.off,
                                        instruction.src_reg(),
                                    )
                                }
                            }
                            BPF_CMPXCHG => {
                                if is_64 {
                                    self.atomic_cmpxchg64(
                                        context,
                                        instruction.dst_reg(),
                                        instruction.off,
                                        instruction.src_reg(),
                                    )
                                } else {
                                    self.atomic_cmpxchg(
                                        context,
                                        instruction.dst_reg(),
                                        instruction.off,
                                        instruction.src_reg(),
                                    )
                                }
                            }
                            _ => Err(format!("invalid atomic operation {:x}", operation)),
                        };
                    } else {
                        // Unsupported instruction.
                        return invalid_op_code();
                    }
                }
            }
            _ => unreachable!(),
        }
    }
}
