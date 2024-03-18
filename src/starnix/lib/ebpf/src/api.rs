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

// The instruction code for immediate loads
pub const BPF_IMM: u8 = linux_uapi::BPF_IMM as u8;

// The mask for the swap operations
pub const BPF_TO_BE: u8 = linux_uapi::BPF_TO_BE as u8;
pub const BPF_TO_LE: u8 = linux_uapi::BPF_TO_LE as u8;
pub const BPF_END_TYPE_MASK: u8 = BPF_TO_BE | BPF_TO_LE;

// The mask for the load/store mode
pub const BPF_MEM: u8 = linux_uapi::BPF_MEM as u8;

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

// The load double operation that allows to write 64 bits into a register.
pub const BPF_LDDW: u8 = BPF_LD | BPF_DW;
