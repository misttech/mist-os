// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::bpf::fs::get_bpf_object;
use crate::security;
use crate::task::CurrentTask;
use crate::vfs::{FdNumber, OutputBuffer};
use ebpf::{
    link_program, verify_program, EbpfError, EbpfHelperImpl, EbpfInstruction, EbpfProgram,
    EbpfProgramContext, StructMapping, VerifiedEbpfProgram, VerifierLogger, BPF_LDDW,
    BPF_PSEUDO_BTF_ID, BPF_PSEUDO_FUNC, BPF_PSEUDO_MAP_FD, BPF_PSEUDO_MAP_IDX,
    BPF_PSEUDO_MAP_IDX_VALUE, BPF_PSEUDO_MAP_VALUE,
};
use ebpf_api::{
    get_common_helpers, AttachType, EbpfApiError, Map, MapsContext, PinnedMap, ProgramType,
};
use fidl_fuchsia_ebpf as febpf;
use starnix_logging::{log_error, log_warn, track_stub};
use starnix_uapi::auth::{CAP_BPF, CAP_NET_ADMIN, CAP_PERFMON, CAP_SYS_ADMIN};
use starnix_uapi::errors::Errno;
use starnix_uapi::{bpf_attr__bindgen_ty_4, bpf_insn, errno, error};
use std::collections::HashMap;
use std::sync::atomic::Ordering;

#[derive(Clone, Debug)]
pub struct ProgramInfo {
    pub program_type: ProgramType,
    pub expected_attach_type: AttachType,
}

impl TryFrom<&bpf_attr__bindgen_ty_4> for ProgramInfo {
    type Error = Errno;

    fn try_from(info: &bpf_attr__bindgen_ty_4) -> Result<Self, Self::Error> {
        Ok(Self {
            program_type: info.prog_type.try_into().map_err(map_ebpf_api_error)?,
            expected_attach_type: info.expected_attach_type.into(),
        })
    }
}

#[derive(Debug)]
pub struct Program {
    pub info: ProgramInfo,
    program: VerifiedEbpfProgram,
    maps: Vec<PinnedMap>,

    /// The security state associated with this bpf Program.
    pub security_state: security::BpfProgState,
}

fn map_ebpf_error(e: EbpfError) -> Errno {
    log_error!("Failed to load eBPF program: {e:?}");
    errno!(EINVAL)
}

fn map_ebpf_api_error(e: EbpfApiError) -> Errno {
    log_error!("Failed to load eBPF program: {e:?}");
    match e {
        EbpfApiError::InvalidProgramType(_) | EbpfApiError::InvalidExpectedAttachType(_) => {
            errno!(EINVAL)
        }
        EbpfApiError::UnsupportedProgramType(_) => errno!(ENOTSUP),
    }
}

impl Program {
    pub fn new(
        current_task: &CurrentTask,
        info: ProgramInfo,
        logger: &mut dyn OutputBuffer,
        mut code: Vec<bpf_insn>,
    ) -> Result<Program, Errno> {
        Self::check_load_access(current_task, &info)?;
        let maps = link_maps_fds(current_task, &mut code)?;
        let maps_schema = maps.iter().map(|m| m.schema).collect();
        let mut logger = BufferVeriferLogger::new(logger);
        let calling_context = info
            .program_type
            .create_calling_context(info.expected_attach_type, maps_schema)
            .map_err(map_ebpf_api_error)?;
        let program = verify_program(code, calling_context, &mut logger).map_err(map_ebpf_error)?;
        let security_state = security::bpf_prog_alloc(current_task);
        Ok(Program { info, program, maps, security_state })
    }

    pub fn link<C: EbpfProgramContext<Map = PinnedMap>>(
        &self,
        program_type: ProgramType,
        struct_mappings: &[StructMapping],
        local_helpers: &[(u32, EbpfHelperImpl<C>)],
    ) -> Result<EbpfProgram<C>, Errno>
    where
        for<'a> C::RunContext<'a>: MapsContext<'a>,
    {
        if program_type != self.info.program_type {
            return error!(EINVAL);
        }

        let mut helpers = HashMap::new();
        helpers.extend(get_common_helpers::<C>().drain(..));
        helpers.extend(local_helpers.iter().cloned());

        let program = link_program(&self.program, struct_mappings, self.maps.clone(), helpers)
            .map_err(map_ebpf_error)?;

        Ok(program)
    }

    fn check_load_access(current_task: &CurrentTask, info: &ProgramInfo) -> Result<(), Errno> {
        if matches!(info.program_type, ProgramType::CgroupSkb | ProgramType::SocketFilter)
            && current_task.kernel().disable_unprivileged_bpf.load(Ordering::Relaxed) == 0
        {
            return Ok(());
        }
        if security::is_task_capable_noaudit(current_task, CAP_SYS_ADMIN) {
            return Ok(());
        }
        security::check_task_capable(current_task, CAP_BPF)?;
        match info.program_type {
            // Loading tracing program types additionally require the CAP_PERFMON capability.
            ProgramType::Kprobe
            | ProgramType::Tracepoint
            | ProgramType::PerfEvent
            | ProgramType::RawTracepoint
            | ProgramType::RawTracepointWritable
            | ProgramType::Tracing => security::check_task_capable(current_task, CAP_PERFMON),

            // Loading networking program types additionally require the CAP_NET_ADMIN capability.
            ProgramType::SocketFilter
            | ProgramType::SchedCls
            | ProgramType::SchedAct
            | ProgramType::Xdp
            | ProgramType::SockOps
            | ProgramType::SkSkb
            | ProgramType::SkMsg
            | ProgramType::SkLookup
            | ProgramType::SkReuseport
            | ProgramType::FlowDissector
            | ProgramType::Netfilter => security::check_task_capable(current_task, CAP_NET_ADMIN),

            // No additional checks are necessary for other program types.
            ProgramType::CgroupDevice
            | ProgramType::CgroupSkb
            | ProgramType::CgroupSock
            | ProgramType::CgroupSockAddr
            | ProgramType::CgroupSockopt
            | ProgramType::CgroupSysctl
            | ProgramType::Ext
            | ProgramType::LircMode2
            | ProgramType::Lsm
            | ProgramType::LwtIn
            | ProgramType::LwtOut
            | ProgramType::LwtSeg6Local
            | ProgramType::LwtXmit
            | ProgramType::StructOps
            | ProgramType::Syscall
            | ProgramType::Unspec
            | ProgramType::Fuse => Ok(()),
        }
    }
}

impl TryFrom<&Program> for febpf::VerifiedProgram {
    type Error = Errno;

    fn try_from(program: &Program) -> Result<febpf::VerifiedProgram, Errno> {
        let mut maps = Vec::with_capacity(program.maps.len());
        for map in program.maps.iter() {
            maps.push(map.share().map_err(|_| errno!(EIO))?);
        }

        // SAFETY: EbpfInstruction is 64-bit, so it's safe to transmute it to u64.
        let code = program.program.code();
        let code_u64 =
            unsafe { std::slice::from_raw_parts(code.as_ptr() as *const u64, code.len()) };

        let struct_access_instructions = program
            .program
            .struct_access_instructions()
            .iter()
            .map(|v| febpf::StructAccess {
                pc: v.pc.try_into().unwrap(),
                struct_memory_id: v.memory_id.id(),
                field_offset: v.field_offset.try_into().unwrap(),
                is_32_bit_ptr_load: v.is_32_bit_ptr_load,
            })
            .collect();

        Ok(febpf::VerifiedProgram {
            code: Some(code_u64.to_vec()),
            struct_access_instructions: Some(struct_access_instructions),
            maps: Some(maps),
            ..Default::default()
        })
    }
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
                    let map: &PinnedMap = object.as_map()?;

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
