// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#[cfg(test)]
mod test {
    use crate::{EbpfProgramBuilder, NullVerifierLogger, Type};
    use linux_uapi::{
        bpf_insn, BPF_ADD, BPF_ALU, BPF_ALU64, BPF_AND, BPF_ARSH, BPF_B, BPF_CALL, BPF_DIV, BPF_DW,
        BPF_EXIT, BPF_H, BPF_IMM, BPF_JA, BPF_JEQ, BPF_JGE, BPF_JGT, BPF_JLE, BPF_JLT, BPF_JMP,
        BPF_JMP32, BPF_JNE, BPF_JSET, BPF_JSGE, BPF_JSGT, BPF_JSLE, BPF_JSLT, BPF_LD, BPF_LDX,
        BPF_LSH, BPF_MEM, BPF_MOD, BPF_MOV, BPF_MUL, BPF_NEG, BPF_OR, BPF_RSH, BPF_ST, BPF_STX,
        BPF_SUB, BPF_W, BPF_XOR,
    };
    use pest::{iterators::Pair, Parser};
    use pest_derive::Parser;
    use std::str::FromStr;
    use test_case::test_case;

    #[derive(Parser)]
    #[grammar = "../../src/starnix/lib/ubpf/src/test_grammar.pest"]
    struct TestGrammar {}

    struct TestCase {
        code: Vec<bpf_insn>,
        result: Option<u64>,
        memory: Option<Vec<u8>>,
    }

    const BPF_REG: u32 = 0x08;
    const BPF_SWAP: u32 = 0xd0;
    const HEXADECIMAL_BASE: u32 = 16;

    enum Value {
        Plus(u64),
        Minus(u64),
    }

    impl Value {
        fn as_u64(&self) -> u64 {
            match self {
                Self::Plus(v) => *v,
                Self::Minus(v) => -(*v as i64) as u64,
            }
        }

        fn as_i32(&self) -> i32 {
            match self {
                Self::Plus(v) => u32::try_from(*v).unwrap() as i32,
                Self::Minus(v) => -i32::try_from(*v).unwrap(),
            }
        }

        fn as_i16(&self) -> i16 {
            match self {
                Self::Plus(v) => u16::try_from(*v).unwrap() as i16,
                Self::Minus(v) => -i16::try_from(*v).unwrap(),
            }
        }

        fn as_i32_pair(&self) -> (i32, i32) {
            let v = self.as_u64();
            let (low, high) = (v as i32, (v >> 32) as i32);
            (low, high)
        }
    }

    impl TestCase {
        fn parse_result(pair: Pair<'_, Rule>) -> u64 {
            assert_eq!(pair.as_rule(), Rule::RESULT);
            Self::parse_value(pair.into_inner().next().unwrap()).as_u64()
        }

        fn parse_asm(pair: Pair<'_, Rule>) -> Vec<bpf_insn> {
            assert_eq!(pair.as_rule(), Rule::ASM);
            let mut result: Vec<bpf_insn> = vec![];
            for entry in pair.into_inner() {
                match entry.as_rule() {
                    Rule::ASM_INSTRUCTION => {
                        for instruction in Self::parse_asm_instruction(entry) {
                            result.push(instruction);
                        }
                    }
                    r @ _ => unreachable!("unexpected rule {r:?}"),
                }
            }
            result
        }

        fn parse_deref(pair: Pair<'_, Rule>) -> (u8, i16) {
            assert_eq!(pair.as_rule(), Rule::DEREF);
            let mut inner = pair.into_inner();
            let reg = Self::parse_reg(inner.next().unwrap());
            let offset =
                if let Some(token) = inner.next() { Self::parse_offset_or_exit(token) } else { 0 };
            (reg, offset)
        }

        fn parse_memory_size(value: &str) -> u8 {
            (match value {
                "b" => BPF_B,
                "h" => BPF_H,
                "w" => BPF_W,
                "dw" => BPF_DW,
                r @ _ => unreachable!("unexpected memory size {r:?}"),
            }) as u8
        }

        fn parse_mem_instruction(pair: Pair<'_, Rule>) -> Vec<bpf_insn> {
            assert_eq!(pair.as_rule(), Rule::MEM_INSTRUCTION);
            let mut inner = pair.into_inner();
            let op = inner.next().unwrap();
            match op.as_rule() {
                Rule::STORE_REG_OP => {
                    let (dst_reg, offset) = Self::parse_deref(inner.next().unwrap());
                    let src_reg = Self::parse_reg(inner.next().unwrap());
                    let mut instruction = bpf_insn::default();
                    instruction.set_dst_reg(dst_reg);
                    instruction.set_src_reg(src_reg);
                    instruction.off = offset;
                    instruction.code =
                        (BPF_MEM | BPF_STX) as u8 | Self::parse_memory_size(&op.as_str()[3..]);
                    vec![instruction]
                }
                Rule::STORE_IMM_OP => {
                    let (dst_reg, offset) = Self::parse_deref(inner.next().unwrap());
                    let imm = Self::parse_value(inner.next().unwrap()).as_i32();
                    let mut instruction = bpf_insn::default();
                    instruction.set_dst_reg(dst_reg);
                    instruction.imm = imm;
                    instruction.off = offset;
                    instruction.code =
                        (BPF_MEM | BPF_ST) as u8 | Self::parse_memory_size(&op.as_str()[2..]);
                    vec![instruction]
                }
                Rule::LOAD_OP => {
                    let dst_reg = Self::parse_reg(inner.next().unwrap());
                    let (src_reg, offset) = Self::parse_deref(inner.next().unwrap());
                    let mut instruction = bpf_insn::default();
                    instruction.set_dst_reg(dst_reg);
                    instruction.set_src_reg(src_reg);
                    instruction.off = offset;
                    instruction.code =
                        (BPF_MEM | BPF_LDX) as u8 | Self::parse_memory_size(&op.as_str()[3..]);
                    vec![instruction]
                }
                Rule::LDDW_OP => {
                    let mut instructions: Vec<bpf_insn> = vec![];
                    let dst_reg = Self::parse_reg(inner.next().unwrap());
                    let value = Self::parse_value(inner.next().unwrap());
                    let (low, high) = value.as_i32_pair();
                    let mut instruction = bpf_insn::default();
                    instruction.set_dst_reg(dst_reg);
                    instruction.imm = low;
                    instruction.code = (BPF_IMM | BPF_LD | BPF_DW) as u8;
                    instructions.push(instruction);
                    let mut instruction = bpf_insn::default();
                    instruction.imm = high;
                    instructions.push(instruction);
                    instructions
                }
                r @ _ => unreachable!("unexpected rule {r:?}"),
            }
        }

        fn parse_asm_instruction(pair: Pair<'_, Rule>) -> Vec<bpf_insn> {
            assert_eq!(pair.as_rule(), Rule::ASM_INSTRUCTION);
            if let Some(entry) = pair.into_inner().next() {
                let mut instruction = bpf_insn::default();
                instruction.code = 0;
                match entry.as_rule() {
                    Rule::ALU_INSTRUCTION => {
                        vec![Self::parse_alu_instruction(entry)]
                    }
                    Rule::JMP_INSTRUCTION => {
                        vec![Self::parse_jmp_instruction(entry)]
                    }
                    Rule::MEM_INSTRUCTION => Self::parse_mem_instruction(entry),
                    r @ _ => unreachable!("unexpected rule {r:?}"),
                }
            } else {
                vec![]
            }
        }

        fn parse_alu_binary_op(value: &str) -> u8 {
            let mut code: u8 = 0;
            let op = if &value[value.len() - 2..] == "32" {
                code |= BPF_ALU as u8;
                &value[..value.len() - 2]
            } else {
                code |= BPF_ALU64 as u8;
                value
            };
            let alu_op_code = match op {
                "add" => BPF_ADD,
                "sub" => BPF_SUB,
                "mul" => BPF_MUL,
                "div" => BPF_DIV,
                "or" => BPF_OR,
                "and" => BPF_AND,
                "lsh" => BPF_LSH,
                "rsh" => BPF_RSH,
                "mod" => BPF_MOD,
                "xor" => BPF_XOR,
                "mov" => BPF_MOV,
                "arsh" => BPF_ARSH,
                _ => unreachable!("unexpected operation {op}"),
            };
            code |= alu_op_code as u8;
            code
        }

        fn parse_alu_unary_op(value: &str) -> (u8, i32) {
            let (code, imm) = match value {
                "neg" => (BPF_ALU64 | BPF_NEG, 0),
                "neg32" => (BPF_ALU | BPF_NEG, 0),
                "be16" => (BPF_ALU | BPF_SWAP | BPF_REG, 16),
                "be32" => (BPF_ALU | BPF_SWAP | BPF_REG, 32),
                "be64" => (BPF_ALU | BPF_SWAP | BPF_REG, 64),
                "le16" => (BPF_ALU | BPF_SWAP | BPF_IMM, 16),
                "le32" => (BPF_ALU | BPF_SWAP | BPF_IMM, 32),
                "le64" => (BPF_ALU | BPF_SWAP | BPF_IMM, 64),
                _ => unreachable!("unexpected operation {value}"),
            };
            (code as u8, imm)
        }

        fn parse_reg(pair: Pair<'_, Rule>) -> u8 {
            assert_eq!(pair.as_rule(), Rule::REG_NUMBER);
            u8::from_str(&pair.as_str()).expect("parse register")
        }

        fn parse_num(pair: Pair<'_, Rule>) -> u64 {
            assert_eq!(pair.as_rule(), Rule::NUM);
            let num = pair.into_inner().next().unwrap();
            match num.as_rule() {
                Rule::DECNUM => num.as_str().parse().unwrap(),
                Rule::HEXSUFFIX => u64::from_str_radix(num.as_str(), HEXADECIMAL_BASE).unwrap(),
                r @ _ => unreachable!("unexpected rule {r:?}"),
            }
        }

        fn parse_value(pair: Pair<'_, Rule>) -> Value {
            assert!(pair.as_rule() == Rule::IMM || pair.as_rule() == Rule::OFFSET);
            let mut inner = pair.into_inner();
            let mut negative = false;
            let maybe_sign = inner.next().unwrap();
            let num = {
                match maybe_sign.as_rule() {
                    Rule::SIGN => {
                        negative = maybe_sign.as_str() == "-";
                        inner.next().unwrap()
                    }
                    Rule::NUM => maybe_sign,
                    r @ _ => unreachable!("unexpected rule {r:?}"),
                }
            };
            let num = Self::parse_num(num);
            if negative {
                Value::Minus(num)
            } else {
                Value::Plus(num)
            }
        }

        fn parse_src(pair: Pair<'_, Rule>, instruction: &mut bpf_insn) {
            match pair.as_rule() {
                Rule::REG_NUMBER => {
                    instruction.set_src_reg(Self::parse_reg(pair));
                    instruction.code |= BPF_REG as u8;
                }
                Rule::IMM => {
                    instruction.imm = Self::parse_value(pair).as_i32();
                    instruction.code |= BPF_IMM as u8;
                }
                r @ _ => unreachable!("unexpected rule {r:?}"),
            }
        }

        fn parse_alu_instruction(pair: Pair<'_, Rule>) -> bpf_insn {
            let mut instruction = bpf_insn::default();
            let mut inner = pair.into_inner();
            let op = inner.next().unwrap();
            match op.as_rule() {
                Rule::BINARY_OP => {
                    instruction.code = Self::parse_alu_binary_op(op.as_str());
                    instruction.set_dst_reg(Self::parse_reg(inner.next().unwrap()));
                    Self::parse_src(inner.next().unwrap(), &mut instruction);
                }
                Rule::UNARY_OP => {
                    instruction.set_dst_reg(Self::parse_reg(inner.next().unwrap()));
                    let (code, imm) = Self::parse_alu_unary_op(op.as_str());
                    instruction.code = code;
                    instruction.imm = imm;
                }
                r @ _ => unreachable!("unexpected rule {r:?}"),
            }
            instruction
        }

        fn parse_offset_or_exit(pair: Pair<'_, Rule>) -> i16 {
            match pair.as_rule() {
                Rule::OFFSET => Self::parse_value(pair).as_i16(),
                // This has no equivalent in ebpf. Just ignore.
                Rule::EXIT => 0,
                r @ _ => unreachable!("unexpected rule {r:?}"),
            }
        }

        fn parse_jmp_op(value: &str) -> u8 {
            let mut code: u8 = 0;
            // Special case for operation ending by 32 but not being BPF_ALU necessarily
            let op = if &value[value.len() - 2..] == "32" {
                code |= BPF_JMP32 as u8;
                &value[..value.len() - 2]
            } else {
                code |= BPF_JMP as u8;
                value
            };
            let jmp_op_code = match op {
                "jeq" => BPF_JEQ,
                "jgt" => BPF_JGT,
                "jge" => BPF_JGE,
                "jlt" => BPF_JLT,
                "jle" => BPF_JLE,
                "jset" => BPF_JSET,
                "jne" => BPF_JNE,
                "jsgt" => BPF_JSGT,
                "jsge" => BPF_JSGE,
                "jslt" => BPF_JSLT,
                "jsle" => BPF_JSLE,
                _ => unreachable!("unexpected operation {op}"),
            };
            code |= jmp_op_code as u8;
            code
        }
        fn parse_jmp_instruction(pair: Pair<'_, Rule>) -> bpf_insn {
            let mut instruction = bpf_insn::default();
            let mut inner = pair.into_inner();
            let op = inner.next().unwrap();
            match op.as_rule() {
                Rule::JMP_CONDITIONAL => {
                    let mut inner = op.into_inner();
                    instruction.code = Self::parse_jmp_op(inner.next().unwrap().as_str());
                    instruction.set_dst_reg(Self::parse_reg(inner.next().unwrap()));
                    Self::parse_src(inner.next().unwrap(), &mut instruction);
                    instruction.off = Self::parse_offset_or_exit(inner.next().unwrap());
                }
                Rule::JMP => {
                    let mut inner = op.into_inner();
                    instruction.code = (BPF_JMP | BPF_JA) as u8;
                    instruction.off = Self::parse_offset_or_exit(inner.next().unwrap());
                }
                Rule::CALL => {
                    let mut inner = op.into_inner();
                    instruction.code = (BPF_JMP | BPF_CALL) as u8;
                    instruction.imm = Self::parse_value(inner.next().unwrap()).as_i32();
                }
                Rule::EXIT => {
                    instruction.code = (BPF_JMP | BPF_EXIT) as u8;
                }
                r @ _ => unreachable!("unexpected rule {r:?}"),
            }
            instruction
        }

        fn parse(content: &str) -> Self {
            let mut pairs =
                TestGrammar::parse(Rule::rules, &content).expect("Parsing must be successful");
            let mut code: Option<Vec<bpf_insn>> = None;
            let mut result: Option<Option<u64>> = None;
            let mut memory: Option<Vec<u8>> = None;
            for entry in pairs.next().unwrap().into_inner() {
                match entry.as_rule() {
                    Rule::ASM => {
                        assert!(code.is_none());
                        code = Some(Self::parse_asm(entry));
                    }
                    Rule::RESULT => {
                        if result.is_none() {
                            result = Some(Some(Self::parse_result(entry)));
                        }
                    }
                    Rule::ERROR => {
                        result = Some(None);
                    }
                    Rule::MEMORY => {
                        assert!(memory.is_none());
                        let mut array = vec![];
                        for byte_pair in entry.into_inner() {
                            assert_eq!(byte_pair.as_rule(), Rule::MEMORY_DATA);
                            array.push(
                                u8::from_str_radix(byte_pair.as_str(), HEXADECIMAL_BASE).unwrap(),
                            );
                        }
                        memory = Some(array);
                    }
                    Rule::EOI => (),
                    r @ _ => unreachable!("unexpected rule {r:?}"),
                }
            }
            TestCase { code: code.unwrap(), result: result.unwrap(), memory }
        }
    }

    macro_rules! test_data {
        ($file_name:tt) => {
            include_str!(concat!("../../../../../third_party/ubpf/src/tests/", $file_name))
        };
    }

    #[test_case(test_data!("add64.data"))]
    #[test_case(test_data!("add.data"))]
    #[test_case(test_data!("alu64-arith.data"))]
    #[test_case(test_data!("alu64-bit.data"))]
    #[test_case(test_data!("alu-arith.data"))]
    #[test_case(test_data!("alu-bit.data"))]
    #[test_case(test_data!("arsh32-high-shift.data"))]
    #[test_case(test_data!("arsh64.data"))]
    #[test_case(test_data!("arsh.data"))]
    #[test_case(test_data!("arsh-reg.data"))]
    #[test_case(test_data!("be16.data"))]
    #[test_case(test_data!("be16-high.data"))]
    #[test_case(test_data!("be32.data"))]
    #[test_case(test_data!("be32-high.data"))]
    #[test_case(test_data!("be64.data"))]
    #[test_case(test_data!("div32-by-zero-reg.data"))]
    #[test_case(test_data!("div32-high-divisor.data"))]
    #[test_case(test_data!("div32-imm.data"))]
    #[test_case(test_data!("div32-reg.data"))]
    #[test_case(test_data!("div64-by-zero-imm.data"))]
    #[test_case(test_data!("div64-by-zero-reg.data"))]
    #[test_case(test_data!("div64-imm.data"))]
    #[test_case(test_data!("div64-negative-imm.data"))]
    #[test_case(test_data!("div64-negative-reg.data"))]
    #[test_case(test_data!("div64-reg.data"))]
    #[test_case(test_data!("div-by-zero-imm.data"))]
    #[test_case(test_data!("div-by-zero-reg.data"))]
    #[test_case(test_data!("early-exit.data"))]
    #[test_case(test_data!("err-infinite-loop.data"))]
    #[test_case(test_data!("err-invalid-reg-dst.data"))]
    #[test_case(test_data!("err-invalid-reg-src.data"))]
    #[test_case(test_data!("err-jmp-lddw.data"))]
    #[test_case(test_data!("err-jmp-out.data"))]
    #[test_case(test_data!("err-stack-oob.data"))]
    #[test_case(test_data!("exit.data"))]
    #[test_case(test_data!("exit-not-last.data"))]
    #[test_case(test_data!("ja.data"))]
    #[test_case(test_data!("jeq-imm.data"))]
    #[test_case(test_data!("jeq-reg.data"))]
    #[test_case(test_data!("jge-imm.data"))]
    #[test_case(test_data!("jgt-imm.data"))]
    #[test_case(test_data!("jgt-reg.data"))]
    #[test_case(test_data!("jit-bounce.data"))]
    #[test_case(test_data!("jle-imm.data"))]
    #[test_case(test_data!("jle-reg.data"))]
    #[test_case(test_data!("jlt-imm.data"))]
    #[test_case(test_data!("jlt-reg.data"))]
    #[test_case(test_data!("jne-reg.data"))]
    #[test_case(test_data!("jset-imm.data"))]
    #[test_case(test_data!("jset-reg.data"))]
    #[test_case(test_data!("jsge-imm.data"))]
    #[test_case(test_data!("jsge-reg.data"))]
    #[test_case(test_data!("jsgt-imm.data"))]
    #[test_case(test_data!("jsgt-reg.data"))]
    #[test_case(test_data!("jsle-imm.data"))]
    #[test_case(test_data!("jsle-reg.data"))]
    #[test_case(test_data!("jslt-imm.data"))]
    #[test_case(test_data!("jslt-reg.data"))]
    #[test_case(test_data!("lddw2.data"))]
    #[test_case(test_data!("ldxb-all.data"))]
    #[test_case(test_data!("ldxb.data"))]
    #[test_case(test_data!("ldxdw.data"))]
    #[test_case(test_data!("ldxh-all2.data"))]
    #[test_case(test_data!("ldxh-all.data"))]
    #[test_case(test_data!("ldxh.data"))]
    #[test_case(test_data!("ldxh-same-reg.data"))]
    #[test_case(test_data!("ldxw-all.data"))]
    #[test_case(test_data!("ldxw.data"))]
    #[test_case(test_data!("le16.data"))]
    #[test_case(test_data!("le32.data"))]
    #[test_case(test_data!("le64.data"))]
    #[test_case(test_data!("lsh-reg.data"))]
    #[test_case(test_data!("mem-len.data"))]
    #[test_case(test_data!("mod32.data"))]
    #[test_case(test_data!("mod64-by-zero-imm.data"))]
    #[test_case(test_data!("mod64-by-zero-reg.data"))]
    #[test_case(test_data!("mod64.data"))]
    #[test_case(test_data!("mod-by-zero-imm.data"))]
    #[test_case(test_data!("mod-by-zero-reg.data"))]
    #[test_case(test_data!("mod.data"))]
    #[test_case(test_data!("mov64-sign-extend.data"))]
    #[test_case(test_data!("mov.data"))]
    #[test_case(test_data!("mul32-imm.data"))]
    #[test_case(test_data!("mul32-reg.data"))]
    #[test_case(test_data!("mul32-reg-overflow.data"))]
    #[test_case(test_data!("mul64-imm.data"))]
    #[test_case(test_data!("mul64-reg.data"))]
    #[test_case(test_data!("mul-loop.data"))]
    #[test_case(test_data!("neg64.data"))]
    #[test_case(test_data!("neg.data"))]
    #[test_case(test_data!("prime.data"))]
    #[test_case(test_data!("rsh32.data"))]
    #[test_case(test_data!("rsh-reg.data"))]
    #[test_case(test_data!("stack3.data"))]
    #[test_case(test_data!("stack.data"))]
    #[test_case(test_data!("stb.data"))]
    #[test_case(test_data!("stdw.data"))]
    #[test_case(test_data!("sth.data"))]
    #[test_case(test_data!("stw.data"))]
    #[test_case(test_data!("stxb-all2.data"))]
    #[test_case(test_data!("stxb-all.data"))]
    #[test_case(test_data!("stxb-chain.data"))]
    #[test_case(test_data!("stxb.data"))]
    #[test_case(test_data!("stxdw.data"))]
    #[test_case(test_data!("stxh.data"))]
    #[test_case(test_data!("stxw.data"))]
    #[test_case(test_data!("subnet.data"))]
    fn test_ebpf_conformance(content: &str) {
        let mut test_case = TestCase::parse(content);
        let mut builder = EbpfProgramBuilder::new().expect("unable to create builder");
        if let Some(memory) = test_case.memory.as_ref() {
            let buffer_size = memory.len() as u64;
            builder.set_args(&[
                Type::PtrToMemory { id: 0, offset: 0, buffer_size },
                Type::from(buffer_size),
            ]);
        } else {
            builder.set_args(&[Type::from(0), Type::from(0)]);
        }

        let program = builder.load(test_case.code, &mut NullVerifierLogger);
        if let Some(value) = test_case.result {
            let program = program.expect("program must be loadable");
            let result = if let Some(memory) = test_case.memory.as_mut() {
                program.run_with_slice(memory.as_mut_slice())
            } else {
                program.run_with_zeroes()
            };
            let result = result.expect("run");
            assert_eq!(result, value);
        } else {
            assert!(program.is_err());
        }
    }
}
