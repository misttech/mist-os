// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    bpf::{
        fs::{get_bpf_object, BpfHandle, BpfObject},
        map::Map,
    },
    task::CurrentTask,
    vfs::FdNumber,
};

use starnix_logging::log_error;
use starnix_uapi::{bpf_insn, errno, error, errors::Errno};
use ubpf::{
    error::UbpfError,
    program::{UbpfVm, UbpfVmBuilder},
    ubpf::EBPF_OP_LDDW,
};

const MAP_LOOKUP_ELEM_INDEX: u32 = 1;
const MAP_LOOKUP_ELEM_NAME: &'static str = "map_lookup_elem";

const MAP_UPDATE_ELEM_INDEX: u32 = 2;
const MAP_UPDATE_ELEM_NAME: &'static str = "map_update_elem";

const MAP_DELETE_ELEM_INDEX: u32 = 3;
const MAP_DELETE_ELEM_NAME: &'static str = "map_delete_elem";

pub struct Program {
    _vm: Option<UbpfVm>,
    _objects: Vec<BpfHandle>,
}

impl BpfObject for Program {}

fn map_ubpf_error(e: UbpfError) -> Errno {
    log_error!("Failed to load BPF program: {:?}", e);
    errno!(EINVAL)
}

impl Program {
    pub fn new(current_task: &CurrentTask, mut code: Vec<bpf_insn>) -> Result<Program, Errno> {
        let mut builder = UbpfVmBuilder::new().map_err(map_ubpf_error)?;
        let objects = link(current_task, &mut code, &mut builder)?;

        let vm = (|| {
            builder.register(MAP_LOOKUP_ELEM_INDEX, MAP_LOOKUP_ELEM_NAME)?;
            builder.register(MAP_UPDATE_ELEM_INDEX, MAP_UPDATE_ELEM_NAME)?;
            builder.register(MAP_DELETE_ELEM_INDEX, MAP_DELETE_ELEM_NAME)?;
            builder.load(code)
        })()
        .map_err(map_ubpf_error)?;
        Ok(Program { _vm: Some(vm), _objects: objects })
    }

    pub fn new_stub() -> Program {
        Program { _vm: None, _objects: vec![] }
    }
}

/// A synthetic source register that represents a map object stored in a file descriptor.
const BPF_PSEUDO_MAP_FD: u8 = 1;

/// Pre-process the given eBPF code to link the program against existing kernel resources.
fn link(
    current_task: &CurrentTask,
    code: &mut Vec<bpf_insn>,
    builder: &mut UbpfVmBuilder,
) -> Result<Vec<BpfHandle>, Errno> {
    let mut objects = vec![];
    let code_len = code.len();
    for pc in 0..code_len {
        let instruction = &mut code[pc];
        if instruction.code == EBPF_OP_LDDW as u8 {
            // EBPF_OP_LDDW requires 2 instructions.
            if pc >= code_len - 1 {
                return error!(EINVAL);
            }

            // If the instruction references BPF_PSEUDO_MAP_FD, then we need to look up the map fd
            // and create a reference from this program to that object.
            if instruction.src_reg() == BPF_PSEUDO_MAP_FD {
                instruction.set_src_reg(0);
                let fd = FdNumber::from_raw(instruction.imm);
                let object = get_bpf_object(current_task, fd)?;
                let map = object.downcast::<Map>().ok_or_else(|| errno!(EINVAL))?;
                let map_ptr = (map as *const Map) as u64;
                let (high, low) = ((map_ptr >> 32) as i32, map_ptr as i32);
                instruction.imm = low;
                // The validation that the next instruction op code is correct will be done by
                // either the verifier or the vm loader.
                let next_instruction = &mut code[pc + 1];
                next_instruction.imm = high;
                builder.register_map_reference(pc, map.schema);
                objects.push(object);
            }
        }
    }
    Ok(objects)
}
