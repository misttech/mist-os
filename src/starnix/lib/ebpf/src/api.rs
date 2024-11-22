// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

/// The stack size in bytes
pub const BPF_STACK_SIZE: usize = 512;

/// The maximum number of instructions in an ebpf program
pub const BPF_MAX_INSTS: usize = 65536;

/// The number of registers
pub const REGISTER_COUNT: u8 = 11;

/// The number of general r/w registers.
pub const GENERAL_REGISTER_COUNT: u8 = 10;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DataWidth {
    U8,
    U16,
    U32,
    U64,
}

/// The different data width used by ebpf
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

    pub fn instruction_bits(&self) -> u8 {
        match self {
            Self::U8 => BPF_B,
            Self::U16 => BPF_H,
            Self::U32 => BPF_W,
            Self::U64 => BPF_DW,
        }
    }
}

// The different operation types
pub const BPF_LD: u8 = linux_uapi::BPF_LD as u8;
pub const BPF_LDX: u8 = linux_uapi::BPF_LDX as u8;
pub const BPF_ST: u8 = linux_uapi::BPF_ST as u8;
pub const BPF_STX: u8 = linux_uapi::BPF_STX as u8;
pub const BPF_ALU: u8 = linux_uapi::BPF_ALU as u8;
pub const BPF_JMP: u8 = linux_uapi::BPF_JMP as u8;
pub const BPF_JMP32: u8 = linux_uapi::BPF_JMP32 as u8;
pub const BPF_ALU64: u8 = linux_uapi::BPF_ALU64 as u8;
pub const BPF_CLS_MASK: u8 =
    BPF_LD | BPF_LDX | BPF_ST | BPF_STX | BPF_ALU | BPF_JMP | BPF_JMP | BPF_JMP32 | BPF_ALU64;

// The mask for the sub operation
pub const BPF_SUB_OP_MASK: u8 = 0xf0;

// The mask for the imm vs src register
pub const BPF_SRC_REG: u8 = linux_uapi::BPF_X as u8;
pub const BPF_SRC_IMM: u8 = linux_uapi::BPF_K as u8;
pub const BPF_SRC_MASK: u8 = BPF_SRC_REG | BPF_SRC_IMM;

// The mods for the load/store
// The instruction code for immediate loads
pub const BPF_IMM: u8 = linux_uapi::BPF_IMM as u8;
pub const BPF_MEM: u8 = linux_uapi::BPF_MEM as u8;
pub const BPF_ATOMIC: u8 = linux_uapi::BPF_ATOMIC as u8;
pub const BPF_ABS: u8 = linux_uapi::BPF_ABS as u8;
pub const BPF_IND: u8 = linux_uapi::BPF_IND as u8;
pub const BPF_LOAD_STORE_MASK: u8 = BPF_IMM | BPF_MEM | BPF_ATOMIC | BPF_ABS | BPF_IND;

// The mask for the swap operations
pub const BPF_TO_BE: u8 = linux_uapi::BPF_TO_BE as u8;
pub const BPF_TO_LE: u8 = linux_uapi::BPF_TO_LE as u8;
pub const BPF_END_TYPE_MASK: u8 = BPF_TO_BE | BPF_TO_LE;

// The different size value
pub const BPF_B: u8 = linux_uapi::BPF_B as u8;
pub const BPF_H: u8 = linux_uapi::BPF_H as u8;
pub const BPF_W: u8 = linux_uapi::BPF_W as u8;
pub const BPF_DW: u8 = linux_uapi::BPF_DW as u8;
pub const BPF_SIZE_MASK: u8 = BPF_B | BPF_H | BPF_W | BPF_DW;

// The different alu operations
pub const BPF_ADD: u8 = linux_uapi::BPF_ADD as u8;
pub const BPF_SUB: u8 = linux_uapi::BPF_SUB as u8;
pub const BPF_MUL: u8 = linux_uapi::BPF_MUL as u8;
pub const BPF_DIV: u8 = linux_uapi::BPF_DIV as u8;
pub const BPF_OR: u8 = linux_uapi::BPF_OR as u8;
pub const BPF_AND: u8 = linux_uapi::BPF_AND as u8;
pub const BPF_LSH: u8 = linux_uapi::BPF_LSH as u8;
pub const BPF_RSH: u8 = linux_uapi::BPF_RSH as u8;
pub const BPF_NEG: u8 = linux_uapi::BPF_NEG as u8;
pub const BPF_MOD: u8 = linux_uapi::BPF_MOD as u8;
pub const BPF_XOR: u8 = linux_uapi::BPF_XOR as u8;
pub const BPF_MOV: u8 = linux_uapi::BPF_MOV as u8;
pub const BPF_ARSH: u8 = linux_uapi::BPF_ARSH as u8;
pub const BPF_END: u8 = linux_uapi::BPF_END as u8;

// The different jump operation
pub const BPF_JA: u8 = linux_uapi::BPF_JA as u8;
pub const BPF_JEQ: u8 = linux_uapi::BPF_JEQ as u8;
pub const BPF_JGT: u8 = linux_uapi::BPF_JGT as u8;
pub const BPF_JGE: u8 = linux_uapi::BPF_JGE as u8;
pub const BPF_JSET: u8 = linux_uapi::BPF_JSET as u8;
pub const BPF_JNE: u8 = linux_uapi::BPF_JNE as u8;
pub const BPF_JSGT: u8 = linux_uapi::BPF_JSGT as u8;
pub const BPF_JSGE: u8 = linux_uapi::BPF_JSGE as u8;
pub const BPF_CALL: u8 = linux_uapi::BPF_CALL as u8;
pub const BPF_EXIT: u8 = linux_uapi::BPF_EXIT as u8;
pub const BPF_JLT: u8 = linux_uapi::BPF_JLT as u8;
pub const BPF_JLE: u8 = linux_uapi::BPF_JLE as u8;
pub const BPF_JSLT: u8 = linux_uapi::BPF_JSLT as u8;
pub const BPF_JSLE: u8 = linux_uapi::BPF_JSLE as u8;

// Specific atomic operation
pub const BPF_FETCH: u8 = linux_uapi::BPF_FETCH as u8;
pub const BPF_XCHG: u8 = linux_uapi::BPF_XCHG as u8;
pub const BPF_CMPXCHG: u8 = linux_uapi::BPF_CMPXCHG as u8;

// The load double operation that allows to write 64 bits into a register.
pub const BPF_LDDW: u8 = BPF_LD | BPF_DW;

// Offset used to access auxiliary packet information in cBPF.
pub const SKF_AD_OFF: i32 = linux_uapi::SKF_AD_OFF;
pub const SKF_AD_PROTOCOL: i32 = linux_uapi::SKF_AD_PROTOCOL as i32;
pub const SKF_AD_PKTTYPE: i32 = linux_uapi::SKF_AD_PKTTYPE as i32;
pub const SKF_AD_IFINDEX: i32 = linux_uapi::SKF_AD_IFINDEX as i32;
pub const SKF_AD_NLATTR: i32 = linux_uapi::SKF_AD_NLATTR as i32;
pub const SKF_AD_NLATTR_NEST: i32 = linux_uapi::SKF_AD_NLATTR_NEST as i32;
pub const SKF_AD_MARK: i32 = linux_uapi::SKF_AD_MARK as i32;
pub const SKF_AD_QUEUE: i32 = linux_uapi::SKF_AD_QUEUE as i32;
pub const SKF_AD_HATYPE: i32 = linux_uapi::SKF_AD_HATYPE as i32;
pub const SKF_AD_RXHASH: i32 = linux_uapi::SKF_AD_RXHASH as i32;
pub const SKF_AD_CPU: i32 = linux_uapi::SKF_AD_CPU as i32;
pub const SKF_AD_ALU_XOR_X: i32 = linux_uapi::SKF_AD_ALU_XOR_X as i32;
pub const SKF_AD_VLAN_TAG: i32 = linux_uapi::SKF_AD_VLAN_TAG as i32;
pub const SKF_AD_VLAN_TAG_PRESENT: i32 = linux_uapi::SKF_AD_VLAN_TAG_PRESENT as i32;
pub const SKF_AD_PAY_OFFSET: i32 = linux_uapi::SKF_AD_PAY_OFFSET as i32;
pub const SKF_AD_RANDOM: i32 = linux_uapi::SKF_AD_RANDOM as i32;
pub const SKF_AD_VLAN_TPID: i32 = linux_uapi::SKF_AD_VLAN_TPID as i32;
pub const SKF_AD_MAX: i32 = linux_uapi::SKF_AD_MAX as i32;

// Offset used to reference IP headers in cBPF.
pub const SKF_NET_OFF: i32 = linux_uapi::SKF_NET_OFF;

// Offset used to reference Ethernet headers in cBPF.
pub const SKF_LL_OFF: i32 = linux_uapi::SKF_LL_OFF;

// Values that can be used in src reg with the `ldimm64`. These instructions
// should be updated when the program is linked.
pub const BPF_PSEUDO_MAP_FD: u8 = linux_uapi::BPF_PSEUDO_MAP_FD as u8;
pub const BPF_PSEUDO_MAP_IDX: u8 = linux_uapi::BPF_PSEUDO_MAP_IDX as u8;
pub const BPF_PSEUDO_MAP_VALUE: u8 = linux_uapi::BPF_PSEUDO_MAP_VALUE as u8;
pub const BPF_PSEUDO_MAP_IDX_VALUE: u8 = linux_uapi::BPF_PSEUDO_MAP_IDX_VALUE as u8;
pub const BPF_PSEUDO_BTF_ID: u8 = linux_uapi::BPF_PSEUDO_BTF_ID as u8;
pub const BPF_PSEUDO_FUNC: u8 = linux_uapi::BPF_PSEUDO_FUNC as u8;

// Values that can be used in src reg with the `call` instruction. These
// instructions should be updated when the program is linked.
pub const BPF_PSEUDO_CALL: u8 = linux_uapi::BPF_PSEUDO_CALL as u8;
pub const BPF_PSEUDO_KFUNC_CALL: u8 = linux_uapi::BPF_PSEUDO_KFUNC_CALL as u8;

pub type EbpfInstruction = linux_uapi::bpf_insn;
pub type CbpfInstruction = linux_uapi::sock_filter;
