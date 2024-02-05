// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{MapSchema, UbpfError};
use linux_uapi::bpf_insn;
use std::collections::HashMap;

pub type ProgramCounter = usize;

#[derive(Clone, Debug, Default)]
pub enum Type {
    #[default]
    NotInit,
    ScalarValue,
    ConstPtrToMap,
    ConstMapKey {
        /// The index in the arguments list that contains a `ConstPtrToMap` for the map this key is
        /// associated with.
        map_ptr_index: usize,
    },
    ConstMapValue {
        /// The index in the arguments list that contains a `ConstPtrToMap` for the map this key is
        /// associated with.
        map_ptr_index: usize,
    },
    NullOr(&'static Type),
}

#[derive(Clone, Debug)]
pub struct FunctionSignature {
    pub args: &'static [Type],
    pub return_value: Type,
}

#[derive(Debug, Default)]
pub struct CallingContext {
    /// For all pc that represents the load of a map address, keep track of the schema of the
    /// associated map.
    map_references: HashMap<ProgramCounter, MapSchema>,
    functions: HashMap<u32, FunctionSignature>,
}

impl CallingContext {
    pub fn register_map_reference(&mut self, pc: ProgramCounter, schema: MapSchema) {
        self.map_references.insert(pc, schema);
    }
    pub fn register_function(&mut self, index: u32, signature: FunctionSignature) {
        self.functions.insert(index, signature);
    }
}

pub fn verify(_code: &Vec<bpf_insn>, _calling_context: CallingContext) -> Result<(), UbpfError> {
    Ok(())
}
