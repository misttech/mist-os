// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// Rust compiler is complaining about the constant, but refuses to compile without these
#![allow(dead_code)]

use linux_uapi::bpf_insn;

pub type Register = u8;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Source {
    Reg(Register),
    Value(u64),
}

impl From<&bpf_insn> for Source {
    fn from(instruction: &bpf_insn) -> Self {
        if instruction.code & EBPF_SRC_REG == EBPF_SRC_REG {
            Self::Reg(instruction.src_reg())
        } else {
            Self::Value(instruction.imm as u64)
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DataWidth {
    U8,
    U16,
    U32,
    U64,
}

impl DataWidth {
    pub fn bits(&self) -> usize {
        match self {
            Self::U8 => 8,
            Self::U16 => 16,
            Self::U32 => 32,
            Self::U64 => 64,
        }
    }

    pub fn bytes(&self) -> usize {
        match self {
            Self::U8 => 1,
            Self::U16 => 2,
            Self::U32 => 4,
            Self::U64 => 8,
        }
    }

    pub fn str(&self) -> &'static str {
        match self {
            Self::U8 => "b",
            Self::U16 => "h",
            Self::U32 => "w",
            Self::U64 => "dw",
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

        let class = instruction.code & EBPF_CLS_MASK;
        match class {
            EBPF_CLS_ALU64 | EBPF_CLS_ALU => {
                let alu_op = instruction.code & EBPF_SUB_OP_MASK;
                let is_64 = class == EBPF_CLS_ALU64;
                match alu_op {
                    EBPF_ALU_OP_ADD => {
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
                    EBPF_ALU_OP_SUB => {
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
                    EBPF_ALU_OP_MUL => {
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
                    EBPF_ALU_OP_DIV => {
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
                    EBPF_ALU_OP_OR => {
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
                    EBPF_ALU_OP_AND => {
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
                    EBPF_ALU_OP_LSH => {
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
                    EBPF_ALU_OP_RSH => {
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
                    EBPF_ALU_OP_MOD => {
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
                    EBPF_ALU_OP_XOR => {
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
                    EBPF_ALU_OP_MOV => {
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
                    EBPF_ALU_OP_ARSH => {
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

                    EBPF_ALU_OP_NEG => {
                        if is_64 {
                            return self.neg64(context, instruction.dst_reg());
                        } else {
                            return self.neg(context, instruction.dst_reg());
                        }
                    }
                    EBPF_ALU_OP_ENDIANNESS => {
                        let is_be = instruction.code & EBPF_SRC_REG == EBPF_SRC_REG;
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
            EBPF_CLS_JMP | EBPF_CLS_JMP32 => {
                let jmp_op = instruction.code & EBPF_SUB_OP_MASK;
                let is_64 = class == EBPF_CLS_JMP;
                match jmp_op {
                    EBPF_JMP_OP_JEQ => {
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
                    EBPF_JMP_OP_JGT => {
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
                    EBPF_JMP_OP_JGE => {
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
                    EBPF_JMP_OP_JSET => {
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
                    EBPF_JMP_OP_JNE => {
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
                    EBPF_JMP_OP_JSGT => {
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
                    EBPF_JMP_OP_JSGE => {
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
                    EBPF_JMP_OP_JLT => {
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
                    EBPF_JMP_OP_JLE => {
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
                    EBPF_JMP_OP_JSLT => {
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
                    EBPF_JMP_OP_JSLE => {
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

                    EBPF_JMP_OP_JA => {
                        return self.jump(context, instruction.off);
                    }
                    EBPF_JMP_OP_CALL => {
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
                    EBPF_JMP_OP_EXIT => {
                        return self.exit(context);
                    }
                    _ => return invalid_op_code(),
                }
            }
            EBPF_CLS_LD => {
                if instruction.code == EBPF_OP_LDDW {
                    if code.len() < 2 {
                        return Err(format!("incomplete lddw"));
                    }
                    let next_instruction = &code[1];
                    let value: u64 =
                        (instruction.imm as u64) | ((next_instruction.imm as u64) << 32);
                    return self.load64(context, instruction.dst_reg(), value, 1);
                }
                // Other ld are not supported.
                return invalid_op_code();
            }
            EBPF_CLS_STX | EBPF_CLS_ST | EBPF_CLS_LDX => {
                if instruction.code & EBPF_MODE_MEM != EBPF_MODE_MEM {
                    // Unsupported instruction.
                    return invalid_op_code();
                }
                let width = match instruction.code & EBPF_SIZE_MASK {
                    EBPF_SIZE_B => DataWidth::U8,
                    EBPF_SIZE_H => DataWidth::U16,
                    EBPF_SIZE_W => DataWidth::U32,
                    EBPF_SIZE_DW => DataWidth::U64,
                    _ => unreachable!(),
                };
                if class == EBPF_CLS_LDX {
                    return self.load(
                        context,
                        instruction.dst_reg(),
                        instruction.off,
                        instruction.src_reg(),
                        width,
                    );
                } else {
                    let src = if class == EBPF_CLS_ST {
                        Source::Value(instruction.imm as u64)
                    } else {
                        Source::Reg(instruction.src_reg())
                    };
                    return self.store(context, instruction.dst_reg(), instruction.off, src, width);
                }
            }
            _ => unreachable!(),
        }
    }
}

// The different operation types
const EBPF_CLS_ALU: u8 = crate::ubpf::EBPF_CLS_ALU as u8;
const EBPF_CLS_ALU64: u8 = crate::ubpf::EBPF_CLS_ALU64 as u8;
const EBPF_CLS_LD: u8 = crate::ubpf::EBPF_CLS_LD as u8;
const EBPF_CLS_LDX: u8 = crate::ubpf::EBPF_CLS_LDX as u8;
const EBPF_CLS_ST: u8 = crate::ubpf::EBPF_CLS_ST as u8;
const EBPF_CLS_STX: u8 = crate::ubpf::EBPF_CLS_STX as u8;
const EBPF_CLS_JMP32: u8 = crate::ubpf::EBPF_CLS_JMP32 as u8;
const EBPF_CLS_JMP: u8 = crate::ubpf::EBPF_CLS_JMP as u8;
const EBPF_CLS_MASK: u8 = crate::ubpf::EBPF_CLS_MASK as u8;

// The mask for the sub operation
const EBPF_SUB_OP_MASK: u8 = crate::ubpf::EBPF_ALU_OP_MASK as u8;

// The mask for the src register
const EBPF_SRC_REG: u8 = crate::ubpf::EBPF_SRC_REG as u8;

// The mask for the load/store mode
const EBPF_MODE_MEM: u8 = crate::ubpf::EBPF_MODE_MEM as u8;

// The different size value
const EBPF_SIZE_MASK: u8 = crate::ubpf::EBPF_SIZE_DW as u8;
const EBPF_SIZE_B: u8 = crate::ubpf::EBPF_SIZE_B as u8;
const EBPF_SIZE_H: u8 = crate::ubpf::EBPF_SIZE_H as u8;
const EBPF_SIZE_W: u8 = crate::ubpf::EBPF_SIZE_W as u8;
const EBPF_SIZE_DW: u8 = crate::ubpf::EBPF_SIZE_DW as u8;

// The different alu operations
const EBPF_ALU_OP_ADD: u8 = 0x00;
const EBPF_ALU_OP_SUB: u8 = 0x10;
const EBPF_ALU_OP_MUL: u8 = 0x20;
const EBPF_ALU_OP_DIV: u8 = 0x30;
const EBPF_ALU_OP_OR: u8 = 0x40;
const EBPF_ALU_OP_AND: u8 = 0x50;
const EBPF_ALU_OP_LSH: u8 = 0x60;
const EBPF_ALU_OP_RSH: u8 = 0x70;
const EBPF_ALU_OP_NEG: u8 = 0x80;
const EBPF_ALU_OP_MOD: u8 = 0x90;
const EBPF_ALU_OP_XOR: u8 = 0xa0;
const EBPF_ALU_OP_MOV: u8 = 0xb0;
const EBPF_ALU_OP_ARSH: u8 = 0xc0;
const EBPF_ALU_OP_ENDIANNESS: u8 = 0xd0;

// The different jump operation
const EBPF_JMP_OP_JA: u8 = crate::ubpf::EBPF_MODE_JA as u8;
const EBPF_JMP_OP_JEQ: u8 = crate::ubpf::EBPF_MODE_JEQ as u8;
const EBPF_JMP_OP_JGT: u8 = crate::ubpf::EBPF_MODE_JGT as u8;
const EBPF_JMP_OP_JGE: u8 = crate::ubpf::EBPF_MODE_JGE as u8;
const EBPF_JMP_OP_JSET: u8 = crate::ubpf::EBPF_MODE_JSET as u8;
const EBPF_JMP_OP_JNE: u8 = crate::ubpf::EBPF_MODE_JNE as u8;
const EBPF_JMP_OP_JSGT: u8 = crate::ubpf::EBPF_MODE_JSGT as u8;
const EBPF_JMP_OP_JSGE: u8 = crate::ubpf::EBPF_MODE_JSGE as u8;
const EBPF_JMP_OP_CALL: u8 = crate::ubpf::EBPF_MODE_CALL as u8;
const EBPF_JMP_OP_EXIT: u8 = crate::ubpf::EBPF_MODE_EXIT as u8;
const EBPF_JMP_OP_JLT: u8 = crate::ubpf::EBPF_MODE_JLT as u8;
const EBPF_JMP_OP_JLE: u8 = crate::ubpf::EBPF_MODE_JLE as u8;
const EBPF_JMP_OP_JSLT: u8 = crate::ubpf::EBPF_MODE_JSLT as u8;
const EBPF_JMP_OP_JSLE: u8 = crate::ubpf::EBPF_MODE_JSLE as u8;

// The load double operation that allows to write 64 bits into a register.
const EBPF_OP_LDDW: u8 = crate::ubpf::EBPF_OP_LDDW as u8;
