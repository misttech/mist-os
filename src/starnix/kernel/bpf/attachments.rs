// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// TODO(https://github.com/rust-lang/rust/issues/39371): remove
#![allow(non_upper_case_globals)]

use crate::bpf::fs::{get_bpf_object, BpfHandle};
use crate::bpf::program::ProgramHandle;
use crate::mm::PAGE_SIZE;
use crate::task::CurrentTask;
use crate::vfs::socket::{
    SockOptValue, SocketDomain, SocketProtocol, SocketType, ZxioBackedSocket,
};
use crate::vfs::FdNumber;
use ebpf::{EbpfProgram, EbpfProgramContext, ProgramArgument, Type};
use ebpf_api::{
    AttachType, BaseEbpfRunContext, CurrentTaskContext, MapValueRef, MapsContext, PinnedMap,
    ProgramType, SocketCookieContext, BPF_SOCK_ADDR_TYPE, BPF_SOCK_TYPE,
};
use fidl_fuchsia_net_filter as fnet_filter;
use fuchsia_component::client::connect_to_protocol_sync;
use starnix_logging::{log_error, log_warn, track_stub};
use starnix_sync::{EbpfStateLock, FileOpsCore, Locked, OrderedRwLock, Unlocked};
use starnix_syscalls::{SyscallResult, SUCCESS};
use starnix_uapi::errors::{Errno, ErrnoCode};
use starnix_uapi::{
    bpf_attr__bindgen_ty_6, bpf_sock, bpf_sock_addr, errno, error, gid_t, pid_t, uid_t,
    CGROUP2_SUPER_MAGIC,
};
use std::ops::{Deref, DerefMut};
use std::sync::{Arc, OnceLock};
use zerocopy::FromBytes;

pub type BpfAttachAttr = bpf_attr__bindgen_ty_6;

fn check_root_cgroup_fd(
    locked: &mut Locked<Unlocked>,
    current_task: &CurrentTask,
    cgroup_fd: FdNumber,
) -> Result<(), Errno> {
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

    Ok(())
}

pub fn bpf_prog_attach(
    locked: &mut Locked<Unlocked>,
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

    // SAFETY: reading i32 field from a union is always safe.
    let target_fd = unsafe { attr.__bindgen_anon_1.target_fd };
    let target_fd = FdNumber::from_raw(target_fd as i32);

    current_task.kernel().ebpf_state.attachments.attach_prog(
        locked,
        current_task,
        attach_type,
        target_fd,
        program,
    )
}

pub fn bpf_prog_detach(
    locked: &mut Locked<Unlocked>,
    current_task: &CurrentTask,
    attr: BpfAttachAttr,
) -> Result<SyscallResult, Errno> {
    let attach_type = AttachType::from(attr.attach_type);

    // SAFETY: reading i32 field from a union is always safe.
    let target_fd = unsafe { attr.__bindgen_anon_1.target_fd };
    let target_fd = FdNumber::from_raw(target_fd as i32);

    current_task.kernel().ebpf_state.attachments.detach_prog(
        locked,
        current_task,
        attach_type,
        target_fd,
    )
}

struct EbpfRunContextImpl<'a> {
    base: BaseEbpfRunContext<'a>,
    current_task: &'a CurrentTask,
}
impl<'a> EbpfRunContextImpl<'a> {
    fn new(current_task: &'a CurrentTask) -> Self {
        Self { base: Default::default(), current_task }
    }
}

impl<'a> MapsContext<'a> for EbpfRunContextImpl<'a> {
    fn add_value_ref(&mut self, map_ref: MapValueRef<'a>) {
        self.base.add_value_ref(map_ref)
    }
}

impl<'a> CurrentTaskContext for EbpfRunContextImpl<'a> {
    fn get_uid_gid(&self) -> (uid_t, gid_t) {
        let creds = self.current_task.current_creds();
        (creds.uid, creds.gid)
    }

    fn get_tid_tgid(&self) -> (pid_t, pid_t) {
        let task = &self.current_task.task;
        (task.get_tid(), task.get_pid())
    }
}

impl<'a, 'b> SocketCookieContext<&'a BpfSock<'a>> for EbpfRunContextImpl<'b> {
    fn get_socket_cookie(&self, bpf_sock: &'a BpfSock<'a>) -> u64 {
        let v = bpf_sock.socket.get_socket_cookie();
        v.unwrap_or_else(|errno| {
            log_error!("Failed to get socket cookie: {:?}", errno);
            0
        })
    }
}

impl<'a, 'b> SocketCookieContext<&'a mut BpfSockAddr<'a>> for EbpfRunContextImpl<'b> {
    fn get_socket_cookie(&self, bpf_sock_addr: &'a mut BpfSockAddr<'a>) -> u64 {
        let v = bpf_sock_addr.socket.get_socket_cookie();
        v.unwrap_or_else(|errno| {
            log_error!("Failed to get socket cookie: {:?}", errno);
            0
        })
    }
}

// Wrapper for `bpf_sock_addr` used to implement `ProgramArgument` trait.
#[repr(C)]
pub struct BpfSockAddr<'a> {
    sock_addr: bpf_sock_addr,

    socket: &'a ZxioBackedSocket,
}

impl<'a> Deref for BpfSockAddr<'a> {
    type Target = bpf_sock_addr;
    fn deref(&self) -> &Self::Target {
        &self.sock_addr
    }
}

impl<'a> DerefMut for BpfSockAddr<'a> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.sock_addr
    }
}

impl<'a> ProgramArgument for &'_ mut BpfSockAddr<'a> {
    fn get_type() -> &'static Type {
        &*BPF_SOCK_ADDR_TYPE
    }
}

// Context for eBPF programs of type BPF_PROG_TYPE_CGROUP_SOCKADDR.
struct SockAddrProgram(EbpfProgram<SockAddrProgram>);

impl EbpfProgramContext for SockAddrProgram {
    type RunContext<'a> = EbpfRunContextImpl<'a>;
    type Packet<'a> = ();
    type Arg1<'a> = &'a mut BpfSockAddr<'a>;
    type Arg2<'a> = ();
    type Arg3<'a> = ();
    type Arg4<'a> = ();
    type Arg5<'a> = ();

    type Map = PinnedMap;
}

#[derive(Debug, PartialEq, Eq)]
pub enum SockAddrProgramResult {
    Allow,
    Block,
}

impl SockAddrProgram {
    fn run<'a>(
        &self,
        current_task: &'a CurrentTask,
        addr: &'a mut BpfSockAddr<'a>,
        can_block: bool,
    ) -> SockAddrProgramResult {
        let mut run_context = EbpfRunContextImpl::new(current_task);
        match self.0.run_with_1_argument(&mut run_context, addr) {
            // UDP_RECVMSG programs are not allowed to block the packet.
            0 if can_block => SockAddrProgramResult::Block,
            1 => SockAddrProgramResult::Allow,
            result => {
                // TODO(https://fxbug.dev/413490751): Change this to panic once
                // result validation is implemented in the eBPF verifier.
                log_error!("eBPF program returned invalid result: {}", result);
                SockAddrProgramResult::Allow
            }
        }
    }
}

type AttachedSockAddrProgramCell = OrderedRwLock<Option<SockAddrProgram>, EbpfStateLock>;

// Wrapper for `bpf_sock` used to implement `ProgramArgument` trait.
#[repr(C)]
pub struct BpfSock<'a> {
    // Must be first field.
    value: bpf_sock,

    socket: &'a ZxioBackedSocket,
}

impl<'a> Deref for BpfSock<'a> {
    type Target = bpf_sock;
    fn deref(&self) -> &Self::Target {
        &self.value
    }
}

impl<'a> DerefMut for BpfSock<'a> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.value
    }
}

impl<'a> ProgramArgument for &'_ BpfSock<'a> {
    fn get_type() -> &'static Type {
        &*BPF_SOCK_TYPE
    }
}

// Context for eBPF programs of type BPF_PROG_TYPE_CGROUP_SOCK.
struct SockProgram(EbpfProgram<SockProgram>);

impl EbpfProgramContext for SockProgram {
    type RunContext<'a> = EbpfRunContextImpl<'a>;
    type Packet<'a> = ();
    type Arg1<'a> = &'a BpfSock<'a>;
    type Arg2<'a> = ();
    type Arg3<'a> = ();
    type Arg4<'a> = ();
    type Arg5<'a> = ();

    type Map = PinnedMap;
}

#[derive(Debug, PartialEq, Eq)]
pub enum SockProgramResult {
    Allow,
    Block,
}

impl SockProgram {
    fn run<'a>(&self, current_task: &'a CurrentTask, sock: &'a BpfSock<'a>) -> SockProgramResult {
        let mut run_context = EbpfRunContextImpl::new(current_task);
        if self.0.run_with_1_argument(&mut run_context, sock) == 0 {
            SockProgramResult::Block
        } else {
            SockProgramResult::Allow
        }
    }
}

type AttachedSockProgramCell = OrderedRwLock<Option<SockProgram>, EbpfStateLock>;

mod internal {
    use ebpf::{ProgramArgument, Type};
    use ebpf_api::BPF_SOCKOPT_TYPE;
    use starnix_uapi::{bpf_sockopt, uaddr};
    use std::ops::Deref;

    // Wrapper for `bpf_sockopt` that implements `ProgramArgument` trait and
    // keeps a buffer for the `optval`.
    #[repr(C)]
    pub struct BpfSockOpt {
        sockopt: bpf_sockopt,

        // Buffer used to store the option value. A pointer to the buffer
        // contents is stored in `sockopt`. `Vec::as_mut_ptr()` guarantees that
        // the pointer remains valid only as long as the `Vec` is not modified,
        // so this field should not be updated directly. `take_value()` can be
        // used to extract the value when `BpfSockOpt` is no longer needed.
        value_buf: Vec<u8>,
    }

    impl BpfSockOpt {
        pub fn new(level: u32, optname: u32, value_buf: Vec<u8>, optlen: u32, retval: i32) -> Self {
            let mut sockopt = Self {
                sockopt: bpf_sockopt {
                    level: level as i32,
                    optname: optname as i32,
                    optlen: optlen as i32,
                    retval: retval as i32,
                    ..Default::default()
                },
                value_buf,
            };

            // SAFETY: Setting buffer bounds in unions is safe.
            unsafe {
                sockopt.sockopt.__bindgen_anon_2.optval =
                    uaddr { addr: sockopt.value_buf.as_mut_ptr() as u64 };
                sockopt.sockopt.__bindgen_anon_3.optval_end = uaddr {
                    addr: sockopt.value_buf.as_mut_ptr().add(sockopt.value_buf.len()) as u64,
                };
            }

            sockopt
        }

        // Returns the value. Consumes `self` since it's not safe to use again
        // after the value buffer is moved.
        pub fn take_value(self) -> Vec<u8> {
            self.value_buf
        }
    }

    impl Deref for BpfSockOpt {
        type Target = bpf_sockopt;
        fn deref(&self) -> &Self::Target {
            &self.sockopt
        }
    }

    impl ProgramArgument for &'_ mut BpfSockOpt {
        fn get_type() -> &'static Type {
            &*BPF_SOCKOPT_TYPE
        }
    }
}

use internal::BpfSockOpt;

// Context for eBPF programs of type BPF_PROG_TYPE_CGROUP_SOCKOPT.
struct SockOptProgram(EbpfProgram<SockOptProgram>);

impl EbpfProgramContext for SockOptProgram {
    type RunContext<'a> = EbpfRunContextImpl<'a>;
    type Packet<'a> = ();
    type Arg1<'a> = &'a mut BpfSockOpt;
    type Arg2<'a> = ();
    type Arg3<'a> = ();
    type Arg4<'a> = ();
    type Arg5<'a> = ();

    type Map = PinnedMap;
}

#[derive(Debug)]
pub enum SetSockOptProgramResult {
    /// Fail the syscall.
    Fail(Errno),

    /// Proceed with the specified option value.
    Allow(SockOptValue),

    /// Return to userspace without invoking the underlying implementation of
    /// setsockopt.
    Bypass,
}

impl SockOptProgram {
    fn run<'a>(&self, current_task: &'a CurrentTask, sockopt: &'a mut BpfSockOpt) -> u64 {
        let mut run_context = EbpfRunContextImpl::new(current_task);
        self.0.run_with_1_argument(&mut run_context, sockopt)
    }
}

type AttachedSockOptProgramCell = OrderedRwLock<Option<SockOptProgram>, EbpfStateLock>;

#[derive(Default)]
pub struct CgroupEbpfProgramSet {
    inet4_bind: AttachedSockAddrProgramCell,
    inet6_bind: AttachedSockAddrProgramCell,
    inet4_connect: AttachedSockAddrProgramCell,
    inet6_connect: AttachedSockAddrProgramCell,
    udp4_sendmsg: AttachedSockAddrProgramCell,
    udp6_sendmsg: AttachedSockAddrProgramCell,
    udp4_recvmsg: AttachedSockAddrProgramCell,
    udp6_recvmsg: AttachedSockAddrProgramCell,
    sock_create: AttachedSockProgramCell,
    sock_release: AttachedSockProgramCell,
    set_sockopt: AttachedSockOptProgramCell,
    get_sockopt: AttachedSockOptProgramCell,
}

#[derive(Eq, PartialEq, Debug, Copy, Clone)]
pub enum SockAddrOp {
    Bind,
    Connect,
    UdpSendMsg,
    UdpRecvMsg,
}

#[derive(Eq, PartialEq, Debug, Copy, Clone)]
pub enum SockOp {
    Create,
    Release,
}

impl CgroupEbpfProgramSet {
    fn get_sock_addr_program(
        &self,
        attach_type: AttachType,
    ) -> Result<&AttachedSockAddrProgramCell, Errno> {
        assert!(attach_type.is_cgroup());

        match attach_type {
            AttachType::CgroupInet4Bind => Ok(&self.inet4_bind),
            AttachType::CgroupInet6Bind => Ok(&self.inet6_bind),
            AttachType::CgroupInet4Connect => Ok(&self.inet4_connect),
            AttachType::CgroupInet6Connect => Ok(&self.inet6_connect),
            AttachType::CgroupUdp4Sendmsg => Ok(&self.udp4_sendmsg),
            AttachType::CgroupUdp6Sendmsg => Ok(&self.udp6_sendmsg),
            AttachType::CgroupUdp4Recvmsg => Ok(&self.udp4_recvmsg),
            AttachType::CgroupUdp6Recvmsg => Ok(&self.udp6_recvmsg),
            _ => error!(ENOTSUP),
        }
    }

    fn get_sock_program(&self, attach_type: AttachType) -> Result<&AttachedSockProgramCell, Errno> {
        assert!(attach_type.is_cgroup());

        match attach_type {
            AttachType::CgroupInetSockCreate => Ok(&self.sock_create),
            AttachType::CgroupInetSockRelease => Ok(&self.sock_release),
            _ => error!(ENOTSUP),
        }
    }

    fn get_sock_opt_program(
        &self,
        attach_type: AttachType,
    ) -> Result<&AttachedSockOptProgramCell, Errno> {
        assert!(attach_type.is_cgroup());

        match attach_type {
            AttachType::CgroupSetsockopt => Ok(&self.set_sockopt),
            AttachType::CgroupGetsockopt => Ok(&self.get_sockopt),
            _ => error!(ENOTSUP),
        }
    }

    // Executes eBPF program for the operation `op`. `socket_address` contains
    // socket address as a `sockaddr` struct.
    pub fn run_sock_addr_prog(
        &self,
        locked: &mut Locked<FileOpsCore>,
        current_task: &CurrentTask,
        op: SockAddrOp,
        domain: SocketDomain,
        socket_type: SocketType,
        protocol: SocketProtocol,
        socket_address: &[u8],
        socket: &ZxioBackedSocket,
    ) -> Result<SockAddrProgramResult, Errno> {
        let prog_cell = match (domain, op) {
            (SocketDomain::Inet, SockAddrOp::Bind) => Some(&self.inet4_bind),
            (SocketDomain::Inet6, SockAddrOp::Bind) => Some(&self.inet6_bind),
            (SocketDomain::Inet, SockAddrOp::Connect) => Some(&self.inet4_connect),
            (SocketDomain::Inet6, SockAddrOp::Connect) => Some(&self.inet6_connect),
            (SocketDomain::Inet, SockAddrOp::UdpSendMsg) => Some(&self.udp4_sendmsg),
            (SocketDomain::Inet6, SockAddrOp::UdpSendMsg) => Some(&self.udp6_sendmsg),
            (SocketDomain::Inet, SockAddrOp::UdpRecvMsg) => Some(&self.udp4_recvmsg),
            (SocketDomain::Inet6, SockAddrOp::UdpRecvMsg) => Some(&self.udp6_recvmsg),
            _ => None,
        };
        let prog_guard = prog_cell.map(|cell| cell.read(locked));
        let Some(prog) = prog_guard.as_ref().and_then(|guard| guard.as_ref()) else {
            return Ok(SockAddrProgramResult::Allow);
        };

        let mut bpf_sockaddr = BpfSockAddr { sock_addr: Default::default(), socket };
        bpf_sockaddr.family = domain.as_raw().into();
        bpf_sockaddr.type_ = socket_type.as_raw();
        bpf_sockaddr.protocol = protocol.as_raw();

        let (sa_family, _) = u16::read_from_prefix(socket_address).map_err(|_| errno!(EINVAL))?;

        if domain.as_raw() != sa_family {
            return error!(EAFNOSUPPORT);
        }
        bpf_sockaddr.user_family = sa_family.into();

        match sa_family.into() {
            linux_uapi::AF_INET => {
                let (sockaddr, _) = linux_uapi::sockaddr_in::ref_from_prefix(socket_address)
                    .map_err(|_| errno!(EINVAL))?;
                bpf_sockaddr.user_port = sockaddr.sin_port.into();
                bpf_sockaddr.user_ip4 = sockaddr.sin_addr.s_addr;
            }
            linux_uapi::AF_INET6 => {
                let sockaddr = linux_uapi::sockaddr_in6::ref_from_prefix(socket_address)
                    .map_err(|_| errno!(EINVAL))?
                    .0;
                bpf_sockaddr.user_port = sockaddr.sin6_port.into();
                // SAFETY: reading an array of u32 from a union is safe.
                bpf_sockaddr.user_ip6 = unsafe { sockaddr.sin6_addr.in6_u.u6_addr32 };
            }
            _ => return error!(EAFNOSUPPORT),
        }

        // UDP recvmsg programs are not allowed to filter packets.
        let can_block = op != SockAddrOp::UdpRecvMsg;

        Ok(prog.run(current_task, &mut bpf_sockaddr, can_block))
    }

    pub fn run_sock_prog(
        &self,
        locked: &mut Locked<FileOpsCore>,
        current_task: &CurrentTask,
        op: SockOp,
        domain: SocketDomain,
        socket_type: SocketType,
        protocol: SocketProtocol,
        socket: &ZxioBackedSocket,
    ) -> SockProgramResult {
        let prog_cell = match op {
            SockOp::Create => &self.sock_create,
            SockOp::Release => &self.sock_release,
        };
        let prog_guard = prog_cell.read(locked);
        let Some(prog) = prog_guard.as_ref() else {
            return SockProgramResult::Allow;
        };

        let bpf_sock = BpfSock {
            value: bpf_sock {
                family: domain.as_raw().into(),
                type_: socket_type.as_raw(),
                protocol: protocol.as_raw(),
                ..Default::default()
            },
            socket,
        };

        prog.run(current_task, &bpf_sock)
    }

    pub fn run_getsockopt_prog(
        &self,
        locked: &mut Locked<FileOpsCore>,
        current_task: &CurrentTask,
        level: u32,
        optname: u32,
        optval: Vec<u8>,
        optlen: usize,
        error: Option<Errno>,
    ) -> Result<(Vec<u8>, usize), Errno> {
        let prog_guard = self.get_sockopt.read(locked);
        let Some(prog) = prog_guard.as_ref() else {
            return error.map(|e| Err(e)).unwrap_or_else(|| Ok((optval, optlen)));
        };

        let retval = error.as_ref().map(|e| -(e.code.error_code() as i32)).unwrap_or(0);
        let mut bpf_sockopt =
            BpfSockOpt::new(level, optname, optval.clone(), optlen as u32, retval);

        // Run the program.
        let result = prog.run(current_task, &mut bpf_sockopt);

        if bpf_sockopt.retval < 0 {
            return Err(Errno::new(ErrnoCode::from_return_value(bpf_sockopt.retval as u64)));
        }

        match (result, bpf_sockopt.optlen) {
            // Reject the call if the program returned 0.
            (0, _) => error!(EPERM),

            // Fail if the program has set an invalid `optlen` (except for the
            // case handled above).
            (1, optlen) if optlen < 0 || (optlen as usize) > optval.len() => {
                error!(EFAULT)
            }

            // If `optlen` is set to 0 then proceed with the original value.
            (1, 0) => Ok((optval, optlen)),

            // Return value from `bpf_sockbuf` - it may be different from the
            // original value.
            (1, new_optlen) => Ok((bpf_sockopt.take_value(), new_optlen as usize)),

            (result, _) => {
                // TODO(https://fxbug.dev/413490751): Change this to panic once
                // result validation is implemented in the verifier.
                log_error!("eBPF getsockopt program returned invalid result: {}", result);
                Ok((optval, optlen))
            }
        }
    }

    pub fn run_setsockopt_prog(
        &self,
        locked: &mut Locked<FileOpsCore>,
        current_task: &CurrentTask,
        level: u32,
        optname: u32,
        value: SockOptValue,
    ) -> SetSockOptProgramResult {
        let prog_guard = self.set_sockopt.read(locked);
        let Some(prog) = prog_guard.as_ref() else {
            return SetSockOptProgramResult::Allow(value);
        };

        let page_size = *PAGE_SIZE as usize;

        // Read only the first page from the user-specified buffer in case it's
        // larger than that.
        let buffer = match value.read_bytes(current_task, page_size) {
            Ok(buffer) => buffer,
            Err(err) => return SetSockOptProgramResult::Fail(err),
        };

        let buffer_len = buffer.len();
        let optlen = value.len();
        let mut bpf_sockopt = BpfSockOpt::new(level, optname, buffer, optlen as u32, 0);
        let result = prog.run(current_task, &mut bpf_sockopt);

        match (result, bpf_sockopt.optlen) {
            // Reject the call if the program returned 0.
            (0, _) => SetSockOptProgramResult::Fail(errno!(EPERM)),

            // `setsockopt` programs can bypass the platform implementation by
            // setting `optlen` to -1.
            (1, -1) => SetSockOptProgramResult::Bypass,

            // If the original value is larger than a page and the program
            // didn't change `optlen` then return the original value. This
            // allows to avoid `EFAULT` below with a no-op program.
            (1, new_optlen) if optlen > page_size && (new_optlen as usize) == optlen => {
                SetSockOptProgramResult::Allow(value)
            }

            // Fail if the program has set an invalid `optlen` (except for the
            // case handled above).
            (1, optlen) if optlen < 0 || (optlen as usize) > buffer_len => {
                SetSockOptProgramResult::Fail(errno!(EFAULT))
            }

            // If `optlen` is set to 0 then proceed with the original value.
            (1, 0) => SetSockOptProgramResult::Allow(value),

            // Return value from `bpf_sockbuf` - it may be different from the
            // original value.
            (1, optlen) => {
                let mut value = bpf_sockopt.take_value();
                value.resize(optlen as usize, 0);
                SetSockOptProgramResult::Allow(value.into())
            }

            (result, _) => {
                // TODO(https://fxbug.dev/413490751): Change this to panic once
                // result validation is implemented in the verifier.
                log_error!("eBPF setsockopt program returned invalid result: {}", result);
                SetSockOptProgramResult::Allow(value)
            }
        }
    }
}

fn attach_type_to_netstack_hook(attach_type: AttachType) -> Option<fnet_filter::SocketHook> {
    let hook = match attach_type {
        AttachType::CgroupInetEgress => fnet_filter::SocketHook::Egress,
        AttachType::CgroupInetIngress => fnet_filter::SocketHook::Ingress,
        _ => return None,
    };
    Some(hook)
}

// Defined a location where eBPF programs can be attached.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum AttachLocation {
    // Attached in Starnix kernel.
    Kernel,

    // Attached in Netstack.
    Netstack,
}

impl TryFrom<AttachType> for AttachLocation {
    type Error = Errno;

    fn try_from(attach_type: AttachType) -> Result<Self, Self::Error> {
        match attach_type {
            AttachType::CgroupInet4Bind
            | AttachType::CgroupInet6Bind
            | AttachType::CgroupInet4Connect
            | AttachType::CgroupInet6Connect
            | AttachType::CgroupUdp4Sendmsg
            | AttachType::CgroupUdp6Sendmsg
            | AttachType::CgroupUdp4Recvmsg
            | AttachType::CgroupUdp6Recvmsg
            | AttachType::CgroupInetSockCreate
            | AttachType::CgroupInetSockRelease
            | AttachType::CgroupGetsockopt
            | AttachType::CgroupSetsockopt => Ok(AttachLocation::Kernel),

            AttachType::CgroupInetEgress | AttachType::CgroupInetIngress => {
                Ok(AttachLocation::Netstack)
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
                track_stub!(TODO("https://fxbug.dev/322873416"), "BPF_PROG_ATTACH", attach_type);
                error!(ENOTSUP)
            }

            AttachType::Unspecified | AttachType::Invalid(_) => {
                error!(EINVAL)
            }
        }
    }
}

#[derive(Default)]
pub struct EbpfAttachments {
    root_cgroup: CgroupEbpfProgramSet,
    socket_control: OnceLock<fnet_filter::SocketControlSynchronousProxy>,
}

impl EbpfAttachments {
    pub fn root_cgroup(&self) -> &CgroupEbpfProgramSet {
        &self.root_cgroup
    }

    fn socket_control(&self) -> &fnet_filter::SocketControlSynchronousProxy {
        self.socket_control.get_or_init(|| {
            connect_to_protocol_sync::<fnet_filter::SocketControlMarker>()
                .expect("Failed to connect to fuchsia.net.filter.SocketControl.")
        })
    }

    fn attach_prog(
        &self,
        locked: &mut Locked<Unlocked>,
        current_task: &CurrentTask,
        attach_type: AttachType,
        target_fd: FdNumber,
        program: ProgramHandle,
    ) -> Result<SyscallResult, Errno> {
        let location: AttachLocation = attach_type.try_into()?;
        let program_type = attach_type.get_program_type();
        match (location, program_type) {
            (AttachLocation::Kernel, ProgramType::CgroupSockAddr) => {
                check_root_cgroup_fd(locked, current_task, target_fd)?;

                let helpers: Vec<_> = ebpf_api::get_current_task_helpers()
                    .into_iter()
                    .chain(ebpf_api::get_cgroup_sock_helpers().into_iter())
                    .collect();
                let linked_program =
                    SockAddrProgram(program.link(attach_type.get_program_type(), &[], &helpers)?);
                *self.root_cgroup.get_sock_addr_program(attach_type)?.write(locked) =
                    Some(linked_program);

                Ok(SUCCESS)
            }

            (AttachLocation::Kernel, ProgramType::CgroupSock) => {
                check_root_cgroup_fd(locked, current_task, target_fd)?;

                let helpers: Vec<_> = ebpf_api::get_current_task_helpers()
                    .into_iter()
                    .chain(ebpf_api::get_cgroup_sock_helpers().into_iter())
                    .collect();
                let linked_program =
                    SockProgram(program.link(attach_type.get_program_type(), &[], &helpers)?);
                *self.root_cgroup.get_sock_program(attach_type)?.write(locked) =
                    Some(linked_program);

                Ok(SUCCESS)
            }

            (AttachLocation::Kernel, ProgramType::CgroupSockopt) => {
                check_root_cgroup_fd(locked, current_task, target_fd)?;

                let helpers = ebpf_api::get_current_task_helpers();
                let linked_program =
                    SockOptProgram(program.link(attach_type.get_program_type(), &[], &helpers)?);
                *self.root_cgroup.get_sock_opt_program(attach_type)?.write(locked) =
                    Some(linked_program);

                Ok(SUCCESS)
            }

            (AttachLocation::Kernel, _) => {
                unreachable!();
            }

            (AttachLocation::Netstack, _) => {
                check_root_cgroup_fd(locked, current_task, target_fd)?;
                self.attach_prog_in_netstack(attach_type, program)
            }
        }
    }

    fn detach_prog(
        &self,
        locked: &mut Locked<Unlocked>,
        current_task: &CurrentTask,
        attach_type: AttachType,
        target_fd: FdNumber,
    ) -> Result<SyscallResult, Errno> {
        let location = attach_type.try_into()?;
        let program_type = attach_type.get_program_type();
        match (location, program_type) {
            (AttachLocation::Kernel, ProgramType::CgroupSockAddr) => {
                check_root_cgroup_fd(locked, current_task, target_fd)?;

                let mut prog_guard =
                    self.root_cgroup.get_sock_addr_program(attach_type)?.write(locked);
                if prog_guard.is_none() {
                    return error!(ENOENT);
                }

                *prog_guard = None;

                Ok(SUCCESS)
            }

            (AttachLocation::Kernel, ProgramType::CgroupSock) => {
                check_root_cgroup_fd(locked, current_task, target_fd)?;

                let mut prog_guard = self.root_cgroup.get_sock_program(attach_type)?.write(locked);
                if prog_guard.is_none() {
                    return error!(ENOENT);
                }

                *prog_guard = None;

                Ok(SUCCESS)
            }

            (AttachLocation::Kernel, ProgramType::CgroupSockopt) => {
                check_root_cgroup_fd(locked, current_task, target_fd)?;

                let mut prog_guard =
                    self.root_cgroup.get_sock_opt_program(attach_type)?.write(locked);
                if prog_guard.is_none() {
                    return error!(ENOENT);
                }

                *prog_guard = None;

                Ok(SUCCESS)
            }

            (AttachLocation::Kernel, _) => {
                unreachable!();
            }

            (AttachLocation::Netstack, _) => {
                check_root_cgroup_fd(locked, current_task, target_fd)?;
                self.detach_prog_in_netstack(attach_type)
            }
        }
    }

    fn attach_prog_in_netstack(
        &self,
        attach_type: AttachType,
        program: ProgramHandle,
    ) -> Result<SyscallResult, Errno> {
        let hook = attach_type_to_netstack_hook(attach_type).ok_or_else(|| errno!(ENOTSUP))?;
        let opts = fnet_filter::AttachEbpfProgramOptions {
            hook: Some(hook),
            program: Some((&**program).try_into()?),
            ..Default::default()
        };
        self.socket_control()
            .attach_ebpf_program(opts, zx::MonotonicInstant::INFINITE)
            .map_err(|e| {
                log_error!(
                    "failed to send fuchsia.net.filter/SocketControl.AttachEbpfProgram: {}",
                    e
                );
                errno!(EIO)
            })?
            .map_err(|e| {
                use fnet_filter::SocketControlAttachEbpfProgramError as Error;
                match e {
                    Error::NotSupported => errno!(ENOTSUP),
                    Error::LinkFailed => errno!(EINVAL),
                    Error::MapFailed => errno!(EIO),
                    Error::DuplicateAttachment => errno!(EEXIST),
                }
            })?;

        Ok(SUCCESS)
    }

    fn detach_prog_in_netstack(&self, attach_type: AttachType) -> Result<SyscallResult, Errno> {
        let hook = attach_type_to_netstack_hook(attach_type).ok_or_else(|| errno!(ENOTSUP))?;
        self.socket_control()
            .detach_ebpf_program(hook, zx::MonotonicInstant::INFINITE)
            .map_err(|e| {
                log_error!(
                    "failed to send fuchsia.net.filter/SocketControl.DetachEbpfProgram: {}",
                    e
                );
                errno!(EIO)
            })?
            .map_err(|e| {
                use fnet_filter::SocketControlDetachEbpfProgramError as Error;
                match e {
                    Error::NotFound => errno!(ENOENT),
                }
            })?;
        Ok(SUCCESS)
    }
}
