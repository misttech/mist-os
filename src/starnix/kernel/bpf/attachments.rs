// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// TODO(https://github.com/rust-lang/rust/issues/39371): remove
#![allow(non_upper_case_globals)]

use crate::bpf::fs::{get_bpf_object, BpfHandle};
use crate::task::CurrentTask;
use crate::vfs::socket::{SocketAddress, SocketDomain, SocketProtocol, SocketType};
use crate::vfs::FdNumber;
use ebpf::{EbpfProgram, EbpfProgramContext, ProgramArgument, Type};
use ebpf_api::{AttachType, PinnedMap, BPF_SOCK_ADDR_TYPE};
use starnix_logging::{log_warn, track_stub};
use starnix_sync::{BpfPrograms, FileOpsCore, Locked, OrderedRwLock, Unlocked};
use starnix_syscalls::{SyscallResult, SUCCESS};
use starnix_uapi::errors::Errno;
use starnix_uapi::{bpf_attr__bindgen_ty_6, bpf_sock_addr, errno, error, CGROUP2_SUPER_MAGIC};
use std::ops::{Deref, DerefMut};
use std::sync::Arc;
use zerocopy::FromBytes;

pub type BpfAttachAttr = bpf_attr__bindgen_ty_6;

fn get_cgroup_program_set<'a>(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &'a CurrentTask,
    attr: &'_ BpfAttachAttr,
) -> Result<&'a CgroupEbpfProgramSet, Errno> {
    // SAFETY: reading i32 field from a union is always safe.
    let cgroup_fd = unsafe { attr.__bindgen_anon_1.target_fd };
    let cgroup_fd = FdNumber::from_raw(cgroup_fd as i32);
    let file = current_task.files.get(cgroup_fd)?;

    // Check that `cgroup_fd` is from the CGROUP2 file system.
    let is_cgroup =
        file.node().fs().statfs(locked, current_task)?.f_type == CGROUP2_SUPER_MAGIC as i64;
    if !is_cgroup {
        log_warn!("bpf_prog_attach(BPF_PROG_ATTACH) is called with an invalid cgroup2 FD.");
        return error!(EINVAL);
    }

    // Currently cgroup attachments are supported only for the root cgroup.
    // TODO(https://fxbug.dev//388077431) Allow attachments to any cgroup once cgroup
    // hierarchy is moved to starnix_core.
    let is_root = file
        .node()
        .fs()
        .maybe_root()
        .map(|root| Arc::ptr_eq(&root.node, file.node()))
        .unwrap_or(false);
    if !is_root {
        log_warn!("bpf_prog_attach(BPF_PROG_ATTACH) is supported only for root cgroup.");
        return error!(EINVAL);
    }

    Ok(&current_task.kernel().root_cgroup_ebpf_programs)
}

pub fn bpf_prog_attach(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    attr: BpfAttachAttr,
) -> Result<SyscallResult, Errno> {
    // SAFETY: reading i32 field from a union is always safe.
    let bpf_fd = FdNumber::from_raw(attr.attach_bpf_fd as i32);
    let object = get_bpf_object(current_task, bpf_fd)?;
    if matches!(object, BpfHandle::ProgramStub(_)) {
        log_warn!("Stub program. Faking successful attach");
        return Ok(SUCCESS);
    }
    let program = object.as_program()?.clone();
    let attach_type = AttachType::from(attr.attach_type);

    let program_type = program.info.program_type;
    if attach_type.get_program_type() != program_type {
        log_warn!(
            "bpf_prog_attach(BPF_PROG_ATTACH): program not compatible with attach_type \
                   attach_type: {attach_type:?}, program_type: {program_type:?}"
        );
        return error!(EINVAL);
    }

    if !attach_type.is_compatible_with_expected_attach_type(program.info.expected_attach_type) {
        log_warn!(
            "bpf_prog_attach(BPF_PROG_ATTACH): expected_attach_type didn't match attach_type \
                   expected_attach_type: {:?}, attach_type: {:?}",
            program.info.expected_attach_type,
            attach_type
        );
        return error!(EINVAL);
    }

    match attach_type {
        AttachType::CgroupInet4Bind
        | AttachType::CgroupInet6Bind
        | AttachType::CgroupInet4Connect
        | AttachType::CgroupInet6Connect
        | AttachType::CgroupUdp4Sendmsg
        | AttachType::CgroupUdp6Sendmsg => {
            let cgroup = get_cgroup_program_set(locked, current_task, &attr)?;
            let linked_program =
                SockAddrEbpfProgram(program.link(attach_type.get_program_type(), &[], &[])?);
            *cgroup.get_program_by_type(attach_type)?.write(locked) = Some(linked_program);

            Ok(SUCCESS)
        }

        AttachType::CgroupGetsockopt
        | AttachType::CgroupInetEgress
        | AttachType::CgroupInetIngress
        | AttachType::CgroupInetSockCreate
        | AttachType::CgroupInetSockRelease
        | AttachType::CgroupSetsockopt
        | AttachType::CgroupUdp4Recvmsg
        | AttachType::CgroupUdp6Recvmsg => {
            // Validate cgroup FD.
            let _cgroup = get_cgroup_program_set(locked, current_task, &attr)?;

            track_stub!(TODO("https://fxbug.dev/322873416"), "BPF_PROG_ATTACH", attr.attach_type);

            // Fake success to avoid breaking apps that depends on the attachments above.
            // TODO(https://fxbug.dev/391380601) Actually implement these attachments.
            Ok(SUCCESS)
        }

        AttachType::CgroupDevice
        | AttachType::CgroupInet4Getpeername
        | AttachType::CgroupInet4Getsockname
        | AttachType::CgroupInet4PostBind
        | AttachType::CgroupInet6Getpeername
        | AttachType::CgroupInet6Getsockname
        | AttachType::CgroupInet6PostBind
        | AttachType::CgroupSysctl
        | AttachType::CgroupUnixConnect
        | AttachType::CgroupUnixGetpeername
        | AttachType::CgroupUnixGetsockname
        | AttachType::CgroupUnixRecvmsg
        | AttachType::CgroupUnixSendmsg
        | AttachType::CgroupSockOps
        | AttachType::SkSkbStreamParser
        | AttachType::SkSkbStreamVerdict
        | AttachType::SkMsgVerdict
        | AttachType::LircMode2
        | AttachType::FlowDissector
        | AttachType::TraceRawTp
        | AttachType::TraceFentry
        | AttachType::TraceFexit
        | AttachType::ModifyReturn
        | AttachType::LsmMac
        | AttachType::TraceIter
        | AttachType::XdpDevmap
        | AttachType::XdpCpumap
        | AttachType::SkLookup
        | AttachType::Xdp
        | AttachType::SkSkbVerdict
        | AttachType::SkReuseportSelect
        | AttachType::SkReuseportSelectOrMigrate
        | AttachType::PerfEvent
        | AttachType::TraceKprobeMulti
        | AttachType::LsmCgroup
        | AttachType::StructOps
        | AttachType::Netfilter
        | AttachType::TcxIngress
        | AttachType::TcxEgress
        | AttachType::TraceUprobeMulti
        | AttachType::NetkitPrimary
        | AttachType::NetkitPeer
        | AttachType::TraceKprobeSession => {
            track_stub!(TODO("https://fxbug.dev/322873416"), "BPF_PROG_ATTACH", attr.attach_type);
            error!(ENOTSUP)
        }

        AttachType::Unspecified | AttachType::Invalid(_) => {
            error!(EINVAL)
        }
    }
}

pub fn bpf_prog_detach(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    attr: BpfAttachAttr,
) -> Result<SyscallResult, Errno> {
    let attach_type = AttachType::from(attr.attach_type);

    match attach_type {
        AttachType::CgroupInet4Bind
        | AttachType::CgroupInet6Bind
        | AttachType::CgroupInet4Connect
        | AttachType::CgroupInet6Connect
        | AttachType::CgroupUdp4Sendmsg
        | AttachType::CgroupUdp6Sendmsg => {
            let cgroup = get_cgroup_program_set(locked, current_task, &attr)?;
            let mut prog_guard = cgroup.get_program_by_type(attach_type)?.write(locked);

            if prog_guard.is_none() {
                return error!(ENOENT);
            }

            *prog_guard = None;

            Ok(SUCCESS)
        }

        AttachType::CgroupGetsockopt
        | AttachType::CgroupInetEgress
        | AttachType::CgroupInetIngress
        | AttachType::CgroupInetSockCreate
        | AttachType::CgroupInetSockRelease
        | AttachType::CgroupSetsockopt
        | AttachType::CgroupUdp4Recvmsg
        | AttachType::CgroupUdp6Recvmsg
        | AttachType::CgroupDevice
        | AttachType::CgroupInet4Getpeername
        | AttachType::CgroupInet4Getsockname
        | AttachType::CgroupInet4PostBind
        | AttachType::CgroupInet6Getpeername
        | AttachType::CgroupInet6Getsockname
        | AttachType::CgroupInet6PostBind
        | AttachType::CgroupSysctl
        | AttachType::CgroupUnixConnect
        | AttachType::CgroupUnixGetpeername
        | AttachType::CgroupUnixGetsockname
        | AttachType::CgroupUnixRecvmsg
        | AttachType::CgroupUnixSendmsg
        | AttachType::CgroupSockOps
        | AttachType::SkSkbStreamParser
        | AttachType::SkSkbStreamVerdict
        | AttachType::SkMsgVerdict
        | AttachType::LircMode2
        | AttachType::FlowDissector
        | AttachType::TraceRawTp
        | AttachType::TraceFentry
        | AttachType::TraceFexit
        | AttachType::ModifyReturn
        | AttachType::LsmMac
        | AttachType::TraceIter
        | AttachType::XdpDevmap
        | AttachType::XdpCpumap
        | AttachType::SkLookup
        | AttachType::Xdp
        | AttachType::SkSkbVerdict
        | AttachType::SkReuseportSelect
        | AttachType::SkReuseportSelectOrMigrate
        | AttachType::PerfEvent
        | AttachType::TraceKprobeMulti
        | AttachType::LsmCgroup
        | AttachType::StructOps
        | AttachType::Netfilter
        | AttachType::TcxIngress
        | AttachType::TcxEgress
        | AttachType::TraceUprobeMulti
        | AttachType::NetkitPrimary
        | AttachType::NetkitPeer
        | AttachType::TraceKprobeSession => {
            track_stub!(TODO("https://fxbug.dev/322873416"), "BPF_PROG_DETACH", attr.attach_type);
            error!(ENOTSUP)
        }

        AttachType::Unspecified | AttachType::Invalid(_) => {
            error!(EINVAL)
        }
    }
}

// Wrapper for `bpf_sock_addr` used to implement `ProgramArgument` trait.
#[repr(C)]
#[derive(Default)]
pub struct BpfSockAddr(bpf_sock_addr);

impl Deref for BpfSockAddr {
    type Target = bpf_sock_addr;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for BpfSockAddr {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl ProgramArgument for &'_ mut BpfSockAddr {
    fn get_type() -> &'static Type {
        &*BPF_SOCK_ADDR_TYPE
    }
}

struct SockAddrEbpfProgram(EbpfProgram<SockAddrEbpfProgram>);

impl EbpfProgramContext for SockAddrEbpfProgram {
    type RunContext<'a> = ();
    type Packet<'a> = ();
    type Arg1<'a> = &'a mut BpfSockAddr;
    type Arg2<'a> = ();
    type Arg3<'a> = ();
    type Arg4<'a> = ();
    type Arg5<'a> = ();

    type Map = PinnedMap;
}

#[derive(Debug, PartialEq, Eq)]
pub enum SockAddrEbpfProgramResult {
    Allow,
    Block,
}

impl SockAddrEbpfProgram {
    fn run(&self, addr: &mut BpfSockAddr) -> SockAddrEbpfProgramResult {
        if self.0.run_with_1_argument(&mut (), addr) == 0 {
            SockAddrEbpfProgramResult::Block
        } else {
            SockAddrEbpfProgramResult::Allow
        }
    }
}

type AttachedEbpfProgramCell = OrderedRwLock<Option<SockAddrEbpfProgram>, BpfPrograms>;

#[derive(Default)]
pub struct CgroupEbpfProgramSet {
    inet4_bind: AttachedEbpfProgramCell,
    inet6_bind: AttachedEbpfProgramCell,
    inet4_connect: AttachedEbpfProgramCell,
    inet6_connect: AttachedEbpfProgramCell,
    udp4_sendmsg: AttachedEbpfProgramCell,
    udp6_sendmsg: AttachedEbpfProgramCell,
}

pub enum SockAddrOp {
    Bind,
    Connect,
    UdpSendMsg,
}

impl CgroupEbpfProgramSet {
    fn get_program_by_type(
        &self,
        attach_type: AttachType,
    ) -> Result<&AttachedEbpfProgramCell, Errno> {
        assert!(attach_type.is_cgroup());

        match attach_type {
            AttachType::CgroupInet4Bind => Ok(&self.inet4_bind),
            AttachType::CgroupInet6Bind => Ok(&self.inet6_bind),
            AttachType::CgroupInet4Connect => Ok(&self.inet4_connect),
            AttachType::CgroupInet6Connect => Ok(&self.inet6_connect),
            AttachType::CgroupUdp4Sendmsg => Ok(&self.udp4_sendmsg),
            AttachType::CgroupUdp6Sendmsg => Ok(&self.udp6_sendmsg),
            _ => error!(ENOTSUP),
        }
    }

    pub fn run_sock_addr_prog(
        &self,
        locked: &mut Locked<'_, FileOpsCore>,
        op: SockAddrOp,
        domain: SocketDomain,
        socket_type: SocketType,
        protocol: SocketProtocol,
        socket_address: &SocketAddress,
    ) -> Result<SockAddrEbpfProgramResult, Errno> {
        let prog_cell = match (domain, op) {
            (SocketDomain::Inet, SockAddrOp::Bind) => Some(&self.inet4_bind),
            (SocketDomain::Inet6, SockAddrOp::Bind) => Some(&self.inet6_bind),
            (SocketDomain::Inet, SockAddrOp::Connect) => Some(&self.inet4_connect),
            (SocketDomain::Inet6, SockAddrOp::Connect) => Some(&self.inet6_connect),
            (SocketDomain::Inet, SockAddrOp::UdpSendMsg) => Some(&self.udp4_sendmsg),
            (SocketDomain::Inet6, SockAddrOp::UdpSendMsg) => Some(&self.udp6_sendmsg),
            _ => None,
        };
        let prog_guard = prog_cell.map(|cell| cell.read(locked));
        let Some(prog) = prog_guard.as_ref().and_then(|guard| guard.as_ref()) else {
            return Ok(SockAddrEbpfProgramResult::Allow);
        };

        let mut bpf_sockaddr = BpfSockAddr::default();
        bpf_sockaddr.family = domain.as_raw().into();
        bpf_sockaddr.type_ = socket_type.as_raw();
        bpf_sockaddr.protocol = protocol.as_raw();

        match socket_address {
            SocketAddress::Inet(addr) => {
                let sockaddr =
                    linux_uapi::sockaddr_in::ref_from_prefix(&addr).map_err(|_| errno!(EINVAL))?.0;
                bpf_sockaddr.user_family = linux_uapi::AF_INET;
                bpf_sockaddr.user_port = sockaddr.sin_port.into();
                bpf_sockaddr.user_ip4 = sockaddr.sin_addr.s_addr;
            }
            SocketAddress::Inet6(addr) => {
                let sockaddr =
                    linux_uapi::sockaddr_in6::ref_from_prefix(&addr).map_err(|_| errno!(EINVAL))?.0;
                bpf_sockaddr.user_family = linux_uapi::AF_INET6;
                bpf_sockaddr.user_port = sockaddr.sin6_port.into();
                // SAFETY: reading an array of u32 from a union is safe.
                bpf_sockaddr.user_ip6 = unsafe { sockaddr.sin6_addr.in6_u.u6_addr32 };
            }
            _ => (),
        };

        Ok(prog.run(&mut bpf_sockaddr))
    }
}
