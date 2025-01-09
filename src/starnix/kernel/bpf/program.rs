// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::bpf::fs::get_bpf_object;
use crate::task::CurrentTask;
use crate::vfs::{FdNumber, OutputBuffer};
use ebpf::{
    link_program, verify_program, BpfValue, EbpfError, EbpfHelperImpl, EbpfInstruction,
    EbpfProgram, EbpfProgramContext, MapDescriptor, StructMapping, VerifiedEbpfProgram,
    VerifierLogger, BPF_LDDW, BPF_PSEUDO_BTF_ID, BPF_PSEUDO_FUNC, BPF_PSEUDO_MAP_FD,
    BPF_PSEUDO_MAP_IDX, BPF_PSEUDO_MAP_IDX_VALUE, BPF_PSEUDO_MAP_VALUE,
};
use ebpf_api::{get_common_helpers, Map, PinnedMap, ProgramType};
use starnix_logging::{log_error, log_warn, track_stub};
use starnix_uapi::errors::Errno;
use starnix_uapi::{bpf_attr__bindgen_ty_4, bpf_insn, errno, error};
use std::collections::HashMap;

#[derive(Clone, Debug)]
pub struct ProgramInfo {
    pub program_type: ProgramType,
}

impl TryFrom<&bpf_attr__bindgen_ty_4> for ProgramInfo {
    type Error = Errno;

    fn try_from(info: &bpf_attr__bindgen_ty_4) -> Result<Self, Self::Error> {
        Ok(Self { program_type: info.prog_type.try_into().map_err(map_ebpf_error)? })
    }
}

#[derive(Debug)]
pub struct Program {
    pub info: ProgramInfo,

    program: VerifiedEbpfProgram,

    // Map references kept to ensure that the maps are not dropped before the program.
    //
    // TODO(https://fxbug.dev/378507648): `VerifiedEbpfProgram` should keep these references once
    // we have maps implementation in the `ebpf` crate.
    maps: Vec<PinnedMap>,
}

fn map_ebpf_error(e: EbpfError) -> Errno {
    log_error!("Failed to load eBPF program: {e:?}");
    errno!(EINVAL)
}

impl Program {
    pub fn new(
        current_task: &CurrentTask,
        info: ProgramInfo,
        logger: &mut dyn OutputBuffer,
        mut code: Vec<bpf_insn>,
    ) -> Result<Program, Errno> {
        let maps = link_maps_fds(current_task, &mut code)?;
        let mut logger = BufferVeriferLogger::new(logger);
        let calling_context =
            info.program_type.create_calling_context(maps.iter().map(|m| m.schema).collect());
        let program = verify_program(code, calling_context, &mut logger).map_err(map_ebpf_error)?;
        Ok(Program { info, program, maps })
    }

    pub fn link<C: EbpfProgramContext>(
        &self,
        program_type: ProgramType,
        struct_mappings: &[StructMapping],
        local_helpers: &[(u32, EbpfHelperImpl<C>)],
    ) -> Result<LinkedProgram<C>, Errno> {
        if program_type != self.info.program_type {
            return error!(EINVAL);
        }

        let maps = self
            .maps
            .iter()
            .map(|m| MapDescriptor { schema: m.schema, ptr: BpfValue::from(&(**m) as *const Map) })
            .collect::<Vec<MapDescriptor>>();

        let mut helpers = HashMap::new();
        helpers.extend(get_common_helpers::<C>().iter().cloned());
        helpers.extend(local_helpers.iter().cloned());

        let program = link_program(&self.program, struct_mappings, &maps[..], helpers)
            .map_err(map_ebpf_error)?;

        Ok(LinkedProgram { program, _maps: self.maps.clone() })
    }
}

#[derive(Debug)]
pub struct LinkedProgram<C: EbpfProgramContext> {
    pub program: EbpfProgram<C>,

    // Map references kept to ensure that the maps are not dropped before the
    // program.
    //
    // TODO(https://fxbug.dev/378507648): `EbpfProgram` will keep these
    // references after the implementation is moved to the `ebpf` crate.
    _maps: Vec<PinnedMap>,
}

/// Links maps referenced by FD, replacing them with by-index references.
fn link_maps_fds(
    current_task: &CurrentTask,
    code: &mut Vec<EbpfInstruction>,
) -> Result<Vec<PinnedMap>, Errno> {
    let code_len = code.len();
    let mut maps = Vec::<PinnedMap>::new();
    for (pc, instruction) in code.iter_mut().enumerate() {
        if instruction.code == BPF_LDDW {
            // BPF_LDDW requires 2 instructions.
            if pc >= code_len - 1 {
                return error!(EINVAL);
            }

            match instruction.src_reg() {
                0 => {}
                BPF_PSEUDO_MAP_FD => {
                    // If the instruction references BPF_PSEUDO_MAP_FD, then we need to look up the map fd
                    // and create a reference from this program to that object.
                    instruction.set_src_reg(BPF_PSEUDO_MAP_IDX);

                    let fd = FdNumber::from_raw(instruction.imm);
                    let object = get_bpf_object(current_task, fd)?;
                    let map = object.as_map()?;

                    // Find the map in `maps` or insert it otherwise.
                    let maybe_index =
                        maps.iter().position(|v| (&**v as *const Map) == (&**map as *const Map));
                    let index = match maybe_index {
                        Some(index) => index,
                        None => {
                            let index = maps.len();
                            maps.push(map.clone());
                            index
                        }
                    };

                    instruction.imm = index.try_into().unwrap();
                }
                BPF_PSEUDO_MAP_IDX
                | BPF_PSEUDO_MAP_VALUE
                | BPF_PSEUDO_MAP_IDX_VALUE
                | BPF_PSEUDO_BTF_ID
                | BPF_PSEUDO_FUNC => {
                    track_stub!(
                        TODO("https://fxbug.dev/378564467"),
                        "unsupported pseudo src for ldimm64",
                        instruction.src_reg()
                    );
                    return error!(ENOTSUP);
                }
                _ => {
                    return error!(EINVAL);
                }
            }
        }
    }
    Ok(maps)
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
