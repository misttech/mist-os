// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{MapSchema, UbpfError};
use linux_uapi::bpf_insn;
use std::collections::HashMap;

pub type ProgramCounter = usize;

#[derive(Debug, Default)]
pub struct CallingContext {
    /// For all pc that represents the load of a map address, keep track of the schema of the
    /// associated map.
    map_references: HashMap<ProgramCounter, MapSchema>,
}

impl CallingContext {
    pub fn register_map_reference(&mut self, pc: ProgramCounter, schema: MapSchema) {
        self.map_references.insert(pc, schema);
    }
}

pub fn verify(_code: &Vec<bpf_insn>, _calling_context: CallingContext) -> Result<(), UbpfError> {
    Ok(())
}
