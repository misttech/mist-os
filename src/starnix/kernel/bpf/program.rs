// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    bpf::{
        fs::{get_bpf_object, BpfHandle},
        helpers::BPF_HELPERS,
        map::Map,
    },
    task::CurrentTask,
    vfs::{FdNumber, OutputBuffer},
};
use ebpf::{
    error::EbpfError,
    program::{EbpfProgram, EbpfProgramBuilder},
    VerifierLogger, BPF_LDDW,
};
use starnix_logging::{log_error, log_warn, track_stub};
use starnix_uapi::{
    bpf_attr__bindgen_ty_4, bpf_insn, bpf_prog_type_BPF_PROG_TYPE_KPROBE,
    bpf_prog_type_BPF_PROG_TYPE_SOCKET_FILTER, errno, error, errors::Errno,
};
use zerocopy::{AsBytes, FromBytes, NoCell};

/// The different type of BPF programs.
#[derive(Debug, Eq, PartialEq)]
pub enum ProgramType {
    SocketFilter,
    KProbe,
    /// Unhandled program type.
    Unknown(u32),
}

impl From<u32> for ProgramType {
    fn from(program_type: u32) -> Self {
        match program_type {
            #![allow(non_upper_case_globals)]
            bpf_prog_type_BPF_PROG_TYPE_SOCKET_FILTER => Self::SocketFilter,
            bpf_prog_type_BPF_PROG_TYPE_KPROBE => Self::KProbe,
            program_type @ _ => {
                track_stub!(
                    TODO("https://fxbug.dev/324043750"),
                    "Unknown BPF program type",
                    program_type
                );
                Self::Unknown(program_type)
            }
        }
    }
}

#[derive(Debug)]
pub struct ProgramInfo {
    pub program_type: ProgramType,
}

impl From<&bpf_attr__bindgen_ty_4> for ProgramInfo {
    fn from(info: &bpf_attr__bindgen_ty_4) -> Self {
        Self { program_type: info.prog_type.into() }
    }
}

pub struct Program {
    pub info: ProgramInfo,
    vm: Option<EbpfProgram>,
    _objects: Vec<BpfHandle>,
}

fn map_ebpf_error(e: EbpfError) -> Errno {
    log_error!("Failed to load eBPF program: {:?}", e);
    errno!(EINVAL)
}

impl Program {
    pub fn new(
        current_task: &CurrentTask,
        info: ProgramInfo,
        logger: &mut dyn OutputBuffer,
        mut code: Vec<bpf_insn>,
    ) -> Result<Program, Errno> {
        let mut builder = EbpfProgramBuilder::default();
        let objects = link(current_task, &mut code, &mut builder)?;

        let vm = (|| {
            // TODO(https://fxbug.dev/323503929): Only register helper function associated with the
            // type of the bpf program.
            for helper in BPF_HELPERS.iter() {
                builder.register(helper)?;
            }
            let mut logger = BufferVeriferLogger::new(logger);
            builder.load(code, &mut logger)
        })()
        .map_err(map_ebpf_error)?;
        Ok(Program { info, vm: Some(vm), _objects: objects })
    }

    pub fn new_stub(info: ProgramInfo) -> Program {
        Program { info, vm: None, _objects: vec![] }
    }

    pub fn run<T: AsBytes + FromBytes + NoCell>(&self, data: &mut T) -> Result<u64, ()> {
        if let Some(vm) = self.vm.as_ref() {
            Ok(vm.run(data))
        } else {
            // vm is only None when bpf is faked. Return an error on execution, as random value
            // might have stronger side effects.
            Err(())
        }
    }
}

/// A synthetic source register that represents a map object stored in a file descriptor.
const BPF_PSEUDO_MAP_FD: u8 = 1;

/// Pre-process the given eBPF code to link the program against existing kernel resources.
fn link(
    current_task: &CurrentTask,
    code: &mut Vec<bpf_insn>,
    builder: &mut EbpfProgramBuilder,
) -> Result<Vec<BpfHandle>, Errno> {
    let mut objects = vec![];
    let code_len = code.len();
    for pc in 0..code_len {
        let instruction = &mut code[pc];
        if instruction.code == BPF_LDDW as u8 {
            // BPF_LDDW requires 2 instructions.
            if pc >= code_len - 1 {
                return error!(EINVAL);
            }

            // If the instruction references BPF_PSEUDO_MAP_FD, then we need to look up the map fd
            // and create a reference from this program to that object.
            if instruction.src_reg() == BPF_PSEUDO_MAP_FD {
                instruction.set_src_reg(0);
                let fd = FdNumber::from_raw(instruction.imm);
                let object = get_bpf_object(current_task, fd)?;
                let map = object.as_map()?;
                let map_ptr = (map.as_ref() as *const Map) as u64;
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

struct BufferVeriferLogger<'a> {
    buffer: &'a mut dyn OutputBuffer,
    full: bool,
}

impl BufferVeriferLogger<'_> {
    fn new<'a>(buffer: &'a mut dyn OutputBuffer) -> BufferVeriferLogger<'a> {
        BufferVeriferLogger { buffer, full: false }
    }
}

impl VerifierLogger for BufferVeriferLogger<'_> {
    fn log(&mut self, line: &[u8]) {
        debug_assert!(line.is_ascii());

        if self.full {
            return;
        }
        if line.len() + 1 > self.buffer.available() {
            self.full = true;
            return;
        }
        match self.buffer.write(line) {
            Err(e) => {
                log_warn!("Unable to write verifier log: {e:?}");
                self.full = true;
            }
            _ => {}
        }
        match self.buffer.write(b"\n") {
            Err(e) => {
                log_warn!("Unable to write verifier log: {e:?}");
                self.full = true;
            }
            _ => {}
        }
    }
}
