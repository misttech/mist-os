// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// TODO(https://github.com/rust-lang/rust/issues/39371): remove
#![allow(non_upper_case_globals)]

use crate::bpf::fs::get_bpf_object;
use crate::task::CurrentTask;
use crate::vfs::socket::{SocketAddress, SocketDomain, SocketProtocol, SocketType};
use crate::vfs::FdNumber;
use ebpf::{EbpfProgram, EbpfProgramContext, ProgramArgument, Type};
use ebpf_api::{PinnedMap, ProgramType, BPF_SOCK_ADDR_TYPE};
use starnix_logging::{log_warn, track_stub};
use starnix_sync::{BpfPrograms, FileOpsCore, Locked, OrderedRwLock, Unlocked};
use starnix_syscalls::{SyscallResult, SUCCESS};
use starnix_uapi::errors::Errno;
use starnix_uapi::{
    bpf_attach_type, bpf_attach_type_BPF_CGROUP_DEVICE, bpf_attach_type_BPF_CGROUP_GETSOCKOPT,
    bpf_attach_type_BPF_CGROUP_INET4_BIND, bpf_attach_type_BPF_CGROUP_INET4_CONNECT,
    bpf_attach_type_BPF_CGROUP_INET4_GETPEERNAME, bpf_attach_type_BPF_CGROUP_INET4_GETSOCKNAME,
    bpf_attach_type_BPF_CGROUP_INET4_POST_BIND, bpf_attach_type_BPF_CGROUP_INET6_BIND,
    bpf_attach_type_BPF_CGROUP_INET6_CONNECT, bpf_attach_type_BPF_CGROUP_INET6_GETPEERNAME,
    bpf_attach_type_BPF_CGROUP_INET6_GETSOCKNAME, bpf_attach_type_BPF_CGROUP_INET6_POST_BIND,
    bpf_attach_type_BPF_CGROUP_INET_EGRESS, bpf_attach_type_BPF_CGROUP_INET_INGRESS,
    bpf_attach_type_BPF_CGROUP_INET_SOCK_CREATE, bpf_attach_type_BPF_CGROUP_INET_SOCK_RELEASE,
    bpf_attach_type_BPF_CGROUP_SETSOCKOPT, bpf_attach_type_BPF_CGROUP_SOCK_OPS,
    bpf_attach_type_BPF_CGROUP_SYSCTL, bpf_attach_type_BPF_CGROUP_UDP4_RECVMSG,
    bpf_attach_type_BPF_CGROUP_UDP4_SENDMSG, bpf_attach_type_BPF_CGROUP_UDP6_RECVMSG,
    bpf_attach_type_BPF_CGROUP_UDP6_SENDMSG, bpf_attach_type_BPF_CGROUP_UNIX_CONNECT,
    bpf_attach_type_BPF_CGROUP_UNIX_GETPEERNAME, bpf_attach_type_BPF_CGROUP_UNIX_GETSOCKNAME,
    bpf_attach_type_BPF_CGROUP_UNIX_RECVMSG, bpf_attach_type_BPF_CGROUP_UNIX_SENDMSG,
    bpf_attach_type_BPF_FLOW_DISSECTOR, bpf_attach_type_BPF_LIRC_MODE2,
    bpf_attach_type_BPF_LSM_CGROUP, bpf_attach_type_BPF_LSM_MAC, bpf_attach_type_BPF_MODIFY_RETURN,
    bpf_attach_type_BPF_NETFILTER, bpf_attach_type_BPF_NETKIT_PEER,
    bpf_attach_type_BPF_NETKIT_PRIMARY, bpf_attach_type_BPF_PERF_EVENT,
    bpf_attach_type_BPF_SK_LOOKUP, bpf_attach_type_BPF_SK_MSG_VERDICT,
    bpf_attach_type_BPF_SK_REUSEPORT_SELECT, bpf_attach_type_BPF_SK_REUSEPORT_SELECT_OR_MIGRATE,
    bpf_attach_type_BPF_SK_SKB_STREAM_PARSER, bpf_attach_type_BPF_SK_SKB_STREAM_VERDICT,
    bpf_attach_type_BPF_SK_SKB_VERDICT, bpf_attach_type_BPF_STRUCT_OPS,
    bpf_attach_type_BPF_TCX_EGRESS, bpf_attach_type_BPF_TCX_INGRESS,
    bpf_attach_type_BPF_TRACE_FENTRY, bpf_attach_type_BPF_TRACE_FEXIT,
    bpf_attach_type_BPF_TRACE_ITER, bpf_attach_type_BPF_TRACE_KPROBE_MULTI,
    bpf_attach_type_BPF_TRACE_KPROBE_SESSION, bpf_attach_type_BPF_TRACE_RAW_TP,
    bpf_attach_type_BPF_TRACE_UPROBE_MULTI, bpf_attach_type_BPF_XDP,
    bpf_attach_type_BPF_XDP_CPUMAP, bpf_attach_type_BPF_XDP_DEVMAP, bpf_attr__bindgen_ty_6,
    bpf_sock_addr, errno, error, CGROUP2_SUPER_MAGIC,
};
use std::ops::{Deref, DerefMut};
use std::sync::Arc;
use zerocopy::FromBytes;

pub type BpfAttachAttr = bpf_attr__bindgen_ty_6;

#[derive(Debug, Clone, Copy)]
pub enum CgroupEbpfAttachment {
    Device,
    Getsockopt,
    Inet4Bind,
    Inet4Connect,
    Inet4Getpeername,
    Inet4Getsockname,
    Inet4PostBind,
    Inet6Bind,
    Inet6Connect,
    Inet6Getpeername,
    Inet6Getsockname,
    Inet6PostBind,
    InetEgress,
    InetIngress,
    InetSockCreate,
    InetSockRelease,
    Setsockopt,
    SockOps,
    Sysctl,
    Udp4Recvmsg,
    Udp4Sendmsg,
    Udp6Recvmsg,
    Udp6Sendmsg,
    UnixConnect,
    UnixGetpeername,
    UnixGetsockname,
    UnixRecvmsg,
    UnixSendmsg,
}

impl CgroupEbpfAttachment {
    fn from_attach_type(attach_type: bpf_attach_type) -> Option<Self> {
        let result = match attach_type {
            bpf_attach_type_BPF_CGROUP_INET_INGRESS => Self::InetIngress,
            bpf_attach_type_BPF_CGROUP_INET_EGRESS => Self::InetEgress,
            bpf_attach_type_BPF_CGROUP_INET_SOCK_CREATE => Self::InetSockCreate,
            bpf_attach_type_BPF_CGROUP_SOCK_OPS => Self::SockOps,
            bpf_attach_type_BPF_CGROUP_DEVICE => Self::Device,
            bpf_attach_type_BPF_CGROUP_INET4_BIND => Self::Inet4Bind,
            bpf_attach_type_BPF_CGROUP_INET6_BIND => Self::Inet6Bind,
            bpf_attach_type_BPF_CGROUP_INET4_CONNECT => Self::Inet4Connect,
            bpf_attach_type_BPF_CGROUP_INET6_CONNECT => Self::Inet6Connect,
            bpf_attach_type_BPF_CGROUP_INET4_POST_BIND => Self::Inet4PostBind,
            bpf_attach_type_BPF_CGROUP_INET6_POST_BIND => Self::Inet6PostBind,
            bpf_attach_type_BPF_CGROUP_UDP4_SENDMSG => Self::Udp4Sendmsg,
            bpf_attach_type_BPF_CGROUP_UDP6_SENDMSG => Self::Udp6Sendmsg,
            bpf_attach_type_BPF_CGROUP_SYSCTL => Self::Sysctl,
            bpf_attach_type_BPF_CGROUP_UDP4_RECVMSG => Self::Udp4Recvmsg,
            bpf_attach_type_BPF_CGROUP_UDP6_RECVMSG => Self::Udp6Recvmsg,
            bpf_attach_type_BPF_CGROUP_GETSOCKOPT => Self::Getsockopt,
            bpf_attach_type_BPF_CGROUP_SETSOCKOPT => Self::Setsockopt,
            bpf_attach_type_BPF_CGROUP_INET4_GETPEERNAME => Self::Inet4Getpeername,
            bpf_attach_type_BPF_CGROUP_INET6_GETPEERNAME => Self::Inet6Getpeername,
            bpf_attach_type_BPF_CGROUP_INET4_GETSOCKNAME => Self::Inet4Getsockname,
            bpf_attach_type_BPF_CGROUP_INET6_GETSOCKNAME => Self::Inet6Getsockname,
            bpf_attach_type_BPF_CGROUP_INET_SOCK_RELEASE => Self::InetSockRelease,
            bpf_attach_type_BPF_CGROUP_UNIX_CONNECT => Self::UnixConnect,
            bpf_attach_type_BPF_CGROUP_UNIX_SENDMSG => Self::UnixSendmsg,
            bpf_attach_type_BPF_CGROUP_UNIX_RECVMSG => Self::UnixRecvmsg,
            bpf_attach_type_BPF_CGROUP_UNIX_GETPEERNAME => Self::UnixGetpeername,
            bpf_attach_type_BPF_CGROUP_UNIX_GETSOCKNAME => Self::UnixGetsockname,
            _ => return None,
        };

        Some(result)
    }

    fn get_program_type(&self) -> ProgramType {
        match self {
            Self::InetIngress | Self::InetEgress => ProgramType::CgroupSkb,
            Self::InetSockCreate
            | Self::Inet4PostBind
            | Self::Inet6PostBind
            | Self::InetSockRelease => ProgramType::CgroupSock,
            Self::SockOps | Self::Getsockopt | Self::Setsockopt => ProgramType::CgroupSockopt,
            Self::Device => ProgramType::CgroupDevice,
            Self::Inet4Bind
            | Self::Inet6Bind
            | Self::Inet4Connect
            | Self::Inet6Connect
            | Self::Udp4Sendmsg
            | Self::Udp6Sendmsg
            | Self::Udp4Recvmsg
            | Self::Udp6Recvmsg
            | Self::Inet4Getpeername
            | Self::Inet6Getpeername
            | Self::Inet4Getsockname
            | Self::Inet6Getsockname
            | Self::UnixConnect
            | Self::UnixSendmsg
            | Self::UnixRecvmsg
            | Self::UnixGetpeername
            | Self::UnixGetsockname => ProgramType::CgroupSockAddr,
            Self::Sysctl => ProgramType::CgroupSysctl,
        }
    }
}

#[derive(Debug, Clone)]
pub enum EbpfAttachment {
    // Currently all cgroup-attachments are assumed to be attached to the root cgroup.
    // TODO(https://fxbug.dev/391380601) Link to a cgroup.
    Cgroup(CgroupEbpfAttachment),

    FlowDissector,
    LircMode2,
    LsmCgroup,
    LsmMac,
    ModifyReturn,
    Netfilter,
    NetkitPeer,
    NetkitPrimary,
    PerfEvent,
    SkLookup,
    SkMsgVerdict,
    SkReuseportSelect,
    SkReuseportSelectOrMigrate,
    SkSkbStreamParser,
    SkSkbStreamVerdict,
    SkSkbVerdict,
    StructOps,
    TcxEgress,
    TcxIngress,
    TraceFentry,
    TraceFexit,
    TraceIter,
    TraceKprobeMulti,
    TraceKprobeSession,
    TraceRawTp,
    TraceUprobeMulti,
    Xdp,
    XdpCpumap,
    XdpDevmap,
}

impl EbpfAttachment {
    fn from_attach_attr(
        locked: &mut Locked<'_, Unlocked>,
        current_task: &CurrentTask,
        attr: &BpfAttachAttr,
    ) -> Result<Self, Errno> {
        if let Some(cgroup_type) = CgroupEbpfAttachment::from_attach_type(attr.attach_type) {
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

            return Ok(Self::Cgroup(cgroup_type));
        }

        let result = match attr.attach_type {
            bpf_attach_type_BPF_SK_SKB_STREAM_PARSER => Self::SkSkbStreamParser,
            bpf_attach_type_BPF_SK_SKB_STREAM_VERDICT => Self::SkSkbStreamVerdict,
            bpf_attach_type_BPF_SK_MSG_VERDICT => Self::SkMsgVerdict,
            bpf_attach_type_BPF_LIRC_MODE2 => Self::LircMode2,
            bpf_attach_type_BPF_FLOW_DISSECTOR => Self::FlowDissector,
            bpf_attach_type_BPF_TRACE_RAW_TP => Self::TraceRawTp,
            bpf_attach_type_BPF_TRACE_FENTRY => Self::TraceFentry,
            bpf_attach_type_BPF_TRACE_FEXIT => Self::TraceFexit,
            bpf_attach_type_BPF_MODIFY_RETURN => Self::ModifyReturn,
            bpf_attach_type_BPF_LSM_MAC => Self::LsmMac,
            bpf_attach_type_BPF_TRACE_ITER => Self::TraceIter,
            bpf_attach_type_BPF_XDP_DEVMAP => Self::XdpDevmap,
            bpf_attach_type_BPF_XDP_CPUMAP => Self::XdpCpumap,
            bpf_attach_type_BPF_SK_LOOKUP => Self::SkLookup,
            bpf_attach_type_BPF_XDP => Self::Xdp,
            bpf_attach_type_BPF_SK_SKB_VERDICT => Self::SkSkbVerdict,
            bpf_attach_type_BPF_SK_REUSEPORT_SELECT => Self::SkReuseportSelect,
            bpf_attach_type_BPF_SK_REUSEPORT_SELECT_OR_MIGRATE => Self::SkReuseportSelectOrMigrate,
            bpf_attach_type_BPF_PERF_EVENT => Self::PerfEvent,
            bpf_attach_type_BPF_TRACE_KPROBE_MULTI => Self::TraceKprobeMulti,
            bpf_attach_type_BPF_LSM_CGROUP => Self::LsmCgroup,
            bpf_attach_type_BPF_STRUCT_OPS => Self::StructOps,
            bpf_attach_type_BPF_NETFILTER => Self::Netfilter,
            bpf_attach_type_BPF_TCX_INGRESS => Self::TcxIngress,
            bpf_attach_type_BPF_TCX_EGRESS => Self::TcxEgress,
            bpf_attach_type_BPF_TRACE_UPROBE_MULTI => Self::TraceUprobeMulti,
            bpf_attach_type_BPF_NETKIT_PRIMARY => Self::NetkitPrimary,
            bpf_attach_type_BPF_NETKIT_PEER => Self::NetkitPeer,
            bpf_attach_type_BPF_TRACE_KPROBE_SESSION => Self::TraceKprobeSession,
            unknown_attach_type => {
                log_warn!("Unknown eBPF attach type: {unknown_attach_type}");
                return error!(EINVAL);
            }
        };

        Ok(result)
    }

    fn get_program_type(&self) -> ProgramType {
        match self {
            Self::Cgroup(cgroup_type) => cgroup_type.get_program_type(),
            Self::FlowDissector => ProgramType::FlowDissector,
            Self::LircMode2 => ProgramType::LircMode2,
            Self::LsmMac | Self::LsmCgroup => ProgramType::Lsm,
            Self::Netfilter => ProgramType::Netfilter,
            Self::PerfEvent => ProgramType::PerfEvent,
            Self::SkLookup => ProgramType::SkLookup,
            Self::SkMsgVerdict | Self::SkSkbVerdict => ProgramType::SkMsg,
            Self::SkReuseportSelect | Self::SkReuseportSelectOrMigrate => ProgramType::SkReuseport,
            Self::SkSkbStreamParser | Self::SkSkbStreamVerdict => ProgramType::SkSkb,
            Self::StructOps => ProgramType::StructOps,
            Self::TcxIngress | Self::TcxEgress | Self::NetkitPrimary | Self::NetkitPeer => {
                ProgramType::SchedCls
            }
            Self::TraceKprobeMulti | Self::TraceUprobeMulti | Self::TraceKprobeSession => {
                ProgramType::Kprobe
            }
            Self::TraceRawTp
            | Self::TraceFentry
            | Self::TraceFexit
            | Self::ModifyReturn
            | Self::TraceIter => ProgramType::Tracing,
            Self::XdpDevmap | Self::XdpCpumap | Self::Xdp => ProgramType::Xdp,
        }
    }
}

pub fn bpf_prog_attach(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    attr: BpfAttachAttr,
) -> Result<SyscallResult, Errno> {
    // SAFETY: reading i32 field from a union is always safe.
    let bpf_fd = FdNumber::from_raw(attr.attach_bpf_fd as i32);
    let program = get_bpf_object(current_task, bpf_fd)?.as_program()?.clone();
    let attach_type = EbpfAttachment::from_attach_attr(locked, current_task, &attr)?;

    let program_type = program.info.program_type;
    if attach_type.get_program_type() != program_type {
        log_warn!("bpf_prog_attach(BPF_PROG_ATTACH) is called with a program of a different type. attach_type: {attach_type:?}, program_type: {program_type:?}");
        return error!(EINVAL);
    }

    match attach_type {
        EbpfAttachment::Cgroup(
            cgroup_attach_type @ (CgroupEbpfAttachment::Inet4Bind
            | CgroupEbpfAttachment::Inet6Bind
            | CgroupEbpfAttachment::Inet4Connect
            | CgroupEbpfAttachment::Inet6Connect
            | CgroupEbpfAttachment::Udp4Sendmsg
            | CgroupEbpfAttachment::Udp6Sendmsg),
        ) => {
            let linked_program =
                SockAddrEbpfProgram(program.link(attach_type.get_program_type(), &[], &[])?);

            let cgroup = &current_task.kernel().root_cgroup_ebpf_programs;
            let prog_lock = match cgroup_attach_type {
                CgroupEbpfAttachment::Inet4Bind => &cgroup.inet4_bind,
                CgroupEbpfAttachment::Inet6Bind => &cgroup.inet6_bind,
                CgroupEbpfAttachment::Inet4Connect => &cgroup.inet4_connect,
                CgroupEbpfAttachment::Inet6Connect => &cgroup.inet6_connect,
                CgroupEbpfAttachment::Udp4Sendmsg => &cgroup.udp4_sendmsg,
                CgroupEbpfAttachment::Udp6Sendmsg => &cgroup.udp6_sendmsg,
                _ => unreachable!(),
            };

            *prog_lock.write(locked) = Some(linked_program);

            Ok(SUCCESS)
        }

        EbpfAttachment::Cgroup(
            CgroupEbpfAttachment::Getsockopt
            | CgroupEbpfAttachment::InetEgress
            | CgroupEbpfAttachment::InetIngress
            | CgroupEbpfAttachment::InetSockCreate
            | CgroupEbpfAttachment::InetSockRelease
            | CgroupEbpfAttachment::Setsockopt
            | CgroupEbpfAttachment::Udp4Recvmsg
            | CgroupEbpfAttachment::Udp6Recvmsg,
        ) => {
            track_stub!(TODO("https://fxbug.dev/322873416"), "BPF_PROG_ATTACH", attr.attach_type);

            // Fake success to avoid breaking apps that depends on the attachments above.
            // TODO(https://fxbug.dev/391380601) Actually implement these attachments.
            Ok(SUCCESS)
        }

        EbpfAttachment::Cgroup(
            CgroupEbpfAttachment::Device
            | CgroupEbpfAttachment::Inet4Getpeername
            | CgroupEbpfAttachment::Inet4Getsockname
            | CgroupEbpfAttachment::Inet4PostBind
            | CgroupEbpfAttachment::Inet6Getpeername
            | CgroupEbpfAttachment::Inet6Getsockname
            | CgroupEbpfAttachment::Inet6PostBind
            | CgroupEbpfAttachment::Sysctl
            | CgroupEbpfAttachment::UnixConnect
            | CgroupEbpfAttachment::UnixGetpeername
            | CgroupEbpfAttachment::UnixGetsockname
            | CgroupEbpfAttachment::UnixRecvmsg
            | CgroupEbpfAttachment::UnixSendmsg
            | CgroupEbpfAttachment::SockOps,
        )
        | EbpfAttachment::SkSkbStreamParser
        | EbpfAttachment::SkSkbStreamVerdict
        | EbpfAttachment::SkMsgVerdict
        | EbpfAttachment::LircMode2
        | EbpfAttachment::FlowDissector
        | EbpfAttachment::TraceRawTp
        | EbpfAttachment::TraceFentry
        | EbpfAttachment::TraceFexit
        | EbpfAttachment::ModifyReturn
        | EbpfAttachment::LsmMac
        | EbpfAttachment::TraceIter
        | EbpfAttachment::XdpDevmap
        | EbpfAttachment::XdpCpumap
        | EbpfAttachment::SkLookup
        | EbpfAttachment::Xdp
        | EbpfAttachment::SkSkbVerdict
        | EbpfAttachment::SkReuseportSelect
        | EbpfAttachment::SkReuseportSelectOrMigrate
        | EbpfAttachment::PerfEvent
        | EbpfAttachment::TraceKprobeMulti
        | EbpfAttachment::LsmCgroup
        | EbpfAttachment::StructOps
        | EbpfAttachment::Netfilter
        | EbpfAttachment::TcxIngress
        | EbpfAttachment::TcxEgress
        | EbpfAttachment::TraceUprobeMulti
        | EbpfAttachment::NetkitPrimary
        | EbpfAttachment::NetkitPeer
        | EbpfAttachment::TraceKprobeSession => {
            track_stub!(TODO("https://fxbug.dev/322873416"), "BPF_PROG_ATTACH", attr.attach_type);
            error!(ENOTSUP)
        }
    }
}

pub fn bpf_prog_detach(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    attr: BpfAttachAttr,
) -> Result<SyscallResult, Errno> {
    let attach_type = EbpfAttachment::from_attach_attr(locked, current_task, &attr)?;

    match attach_type {
        EbpfAttachment::Cgroup(
            cgroup_attach_type @ (CgroupEbpfAttachment::Inet4Bind
            | CgroupEbpfAttachment::Inet6Bind
            | CgroupEbpfAttachment::Inet4Connect
            | CgroupEbpfAttachment::Inet6Connect
            | CgroupEbpfAttachment::Udp4Sendmsg
            | CgroupEbpfAttachment::Udp6Sendmsg),
        ) => {
            let cgroup = &current_task.kernel().root_cgroup_ebpf_programs;
            let prog_cell = match cgroup_attach_type {
                CgroupEbpfAttachment::Inet4Bind => &cgroup.inet4_bind,
                CgroupEbpfAttachment::Inet6Bind => &cgroup.inet6_bind,
                CgroupEbpfAttachment::Inet4Connect => &cgroup.inet4_connect,
                CgroupEbpfAttachment::Inet6Connect => &cgroup.inet6_connect,
                CgroupEbpfAttachment::Udp4Sendmsg => &cgroup.udp4_sendmsg,
                CgroupEbpfAttachment::Udp6Sendmsg => &cgroup.udp6_sendmsg,
                _ => unreachable!(),
            };
            let mut prog_guard = prog_cell.write(locked);

            if prog_guard.is_none() {
                return error!(ENOENT);
            }

            *prog_guard = None;

            Ok(SUCCESS)
        }

        EbpfAttachment::Cgroup(
            CgroupEbpfAttachment::Getsockopt
            | CgroupEbpfAttachment::InetEgress
            | CgroupEbpfAttachment::InetIngress
            | CgroupEbpfAttachment::InetSockCreate
            | CgroupEbpfAttachment::InetSockRelease
            | CgroupEbpfAttachment::Setsockopt
            | CgroupEbpfAttachment::Udp4Recvmsg
            | CgroupEbpfAttachment::Udp6Recvmsg
            | CgroupEbpfAttachment::Device
            | CgroupEbpfAttachment::Inet4Getpeername
            | CgroupEbpfAttachment::Inet4Getsockname
            | CgroupEbpfAttachment::Inet4PostBind
            | CgroupEbpfAttachment::Inet6Getpeername
            | CgroupEbpfAttachment::Inet6Getsockname
            | CgroupEbpfAttachment::Inet6PostBind
            | CgroupEbpfAttachment::Sysctl
            | CgroupEbpfAttachment::UnixConnect
            | CgroupEbpfAttachment::UnixGetpeername
            | CgroupEbpfAttachment::UnixGetsockname
            | CgroupEbpfAttachment::UnixRecvmsg
            | CgroupEbpfAttachment::UnixSendmsg
            | CgroupEbpfAttachment::SockOps,
        )
        | EbpfAttachment::SkSkbStreamParser
        | EbpfAttachment::SkSkbStreamVerdict
        | EbpfAttachment::SkMsgVerdict
        | EbpfAttachment::LircMode2
        | EbpfAttachment::FlowDissector
        | EbpfAttachment::TraceRawTp
        | EbpfAttachment::TraceFentry
        | EbpfAttachment::TraceFexit
        | EbpfAttachment::ModifyReturn
        | EbpfAttachment::LsmMac
        | EbpfAttachment::TraceIter
        | EbpfAttachment::XdpDevmap
        | EbpfAttachment::XdpCpumap
        | EbpfAttachment::SkLookup
        | EbpfAttachment::Xdp
        | EbpfAttachment::SkSkbVerdict
        | EbpfAttachment::SkReuseportSelect
        | EbpfAttachment::SkReuseportSelectOrMigrate
        | EbpfAttachment::PerfEvent
        | EbpfAttachment::TraceKprobeMulti
        | EbpfAttachment::LsmCgroup
        | EbpfAttachment::StructOps
        | EbpfAttachment::Netfilter
        | EbpfAttachment::TcxIngress
        | EbpfAttachment::TcxEgress
        | EbpfAttachment::TraceUprobeMulti
        | EbpfAttachment::NetkitPrimary
        | EbpfAttachment::NetkitPeer
        | EbpfAttachment::TraceKprobeSession => {
            track_stub!(TODO("https://fxbug.dev/322873416"), "BPF_PROG_DETACH", attr.attach_type);
            error!(ENOTSUP)
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
