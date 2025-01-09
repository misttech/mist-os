// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::fs::fuchsia::zxio::{zxio_query_events, zxio_wait_async};
use crate::mm::{MemoryAccessorExt, UNIFIED_ASPACES_ENABLED};
use crate::task::{CurrentTask, EventHandler, Task, WaitCanceler, Waiter};
use crate::vfs::socket::{
    Socket, SocketAddress, SocketDomain, SocketHandle, SocketMessageFlags, SocketOps, SocketPeer,
    SocketProtocol, SocketShutdownFlags, SocketType,
};
use crate::vfs::{AncillaryData, InputBuffer, MessageReadInfo, OutputBuffer};
use starnix_logging::track_stub;
use starnix_sync::{FileOpsCore, Locked};
use starnix_types::user_buffer::UserBuffer;
use starnix_uapi::errors::{Errno, ErrnoCode, ENOTSUP};
use starnix_uapi::vfs::FdEvents;
use starnix_uapi::{
    c_int, errno, errno_from_zxio_code, error, from_status_like_fdio, sock_filter, sock_fprog,
    uapi, ucred, AF_PACKET, BPF_MAXINSNS, MSG_DONTWAIT, MSG_WAITALL, SO_ATTACH_FILTER,
};

use ebpf::convert_and_verify_cbpf;
use fidl::endpoints::DiscoverableProtocolMarker as _;
use static_assertions::const_assert_eq;
use std::sync::{Arc, OnceLock};
use syncio::zxio::{IP_RECVERR, SOL_IP, SOL_SOCKET, SO_DOMAIN, SO_PROTOCOL, SO_TYPE};
use syncio::{ControlMessage, RecvMessageInfo, ServiceConnector, Zxio, ZxioErrorCode};
use {
    fidl_fuchsia_posix_socket as fposix_socket,
    fidl_fuchsia_posix_socket_packet as fposix_socket_packet,
    fidl_fuchsia_posix_socket_raw as fposix_socket_raw,
};

/// Connects to the appropriate `fuchsia_posix_socket_*::Provider` protocol.
struct SocketProviderServiceConnector;

impl ServiceConnector for SocketProviderServiceConnector {
    fn connect(service_name: &str) -> Result<&'static zx::Channel, zx::Status> {
        match service_name {
            fposix_socket::ProviderMarker::PROTOCOL_NAME => {
                static CHANNEL: OnceLock<Result<zx::Channel, zx::Status>> = OnceLock::new();
                &CHANNEL
            }
            fposix_socket_packet::ProviderMarker::PROTOCOL_NAME => {
                static CHANNEL: OnceLock<Result<zx::Channel, zx::Status>> = OnceLock::new();
                &CHANNEL
            }
            fposix_socket_raw::ProviderMarker::PROTOCOL_NAME => {
                static CHANNEL: OnceLock<Result<zx::Channel, zx::Status>> = OnceLock::new();
                &CHANNEL
            }
            _ => return Err(zx::Status::INTERNAL),
        }
        .get_or_init(|| {
            let (client, server) = zx::Channel::create();
            let protocol_path = format!("/svc/{service_name}");
            fdio::service_connect(&protocol_path, server)?;
            Ok(client)
        })
        .as_ref()
        .map_err(|status| *status)
    }
}

/// A socket backed by an underlying Zircon I/O object.
pub struct ZxioBackedSocket {
    /// The underlying Zircon I/O object.
    zxio: Arc<syncio::Zxio>,
}

impl ZxioBackedSocket {
    pub fn new(
        domain: SocketDomain,
        socket_type: SocketType,
        protocol: SocketProtocol,
    ) -> Result<ZxioBackedSocket, Errno> {
        let zxio = Zxio::new_socket::<SocketProviderServiceConnector>(
            domain.as_raw() as c_int,
            socket_type.as_raw() as c_int,
            protocol.as_raw() as c_int,
        )
        .map_err(|status| from_status_like_fdio!(status))?
        .map_err(|out_code| errno_from_zxio_code!(out_code))?;

        Ok(Self::new_with_zxio(zxio))
    }
    pub fn new_with_zxio(zxio: syncio::Zxio) -> ZxioBackedSocket {
        ZxioBackedSocket { zxio: Arc::new(zxio) }
    }

    pub fn sendmsg(
        &self,
        addr: &Option<SocketAddress>,
        data: &mut dyn InputBuffer,
        cmsgs: Vec<ControlMessage>,
        flags: SocketMessageFlags,
    ) -> Result<usize, Errno> {
        let mut addr = match addr {
            Some(
                SocketAddress::Inet(sockaddr)
                | SocketAddress::Inet6(sockaddr)
                | SocketAddress::Packet(sockaddr),
            ) => sockaddr.clone(),
            Some(_) => return error!(EINVAL),
            None => vec![],
        };

        let flags = flags.bits() & !MSG_DONTWAIT;
        let sent_bytes = if UNIFIED_ASPACES_ENABLED {
            match data.peek_all_segments_as_iovecs() {
                Ok(mut iovecs) => Some(self.zxio.sendmsg(&mut addr, &mut iovecs, &cmsgs, flags)),
                Err(e) => {
                    if e.code == ENOTSUP {
                        None
                    } else {
                        return Err(e);
                    }
                }
            }
        } else {
            None
        };

        // If we can't pass the iovecs directly so fallback to reading
        // all the bytes from the input buffer first.
        let sent_bytes = if let Some(sent_bytes) = sent_bytes {
            sent_bytes
        } else {
            let mut bytes = data.peek_all()?;
            self.zxio.sendmsg(
                &mut addr,
                &mut [syncio::zxio::iovec {
                    iov_base: bytes.as_mut_ptr() as *mut starnix_uapi::c_void,
                    iov_len: bytes.len(),
                }],
                &cmsgs,
                flags,
            )
        }
        .map_err(|status| match status {
            zx::Status::OUT_OF_RANGE => errno!(EMSGSIZE),
            other => from_status_like_fdio!(other),
        })?
        .map_err(|out_code| errno_from_zxio_code!(out_code))?;
        data.advance(sent_bytes)?;
        Ok(sent_bytes)
    }

    pub fn recvmsg(
        &self,
        data: &mut dyn OutputBuffer,
        flags: SocketMessageFlags,
    ) -> Result<RecvMessageInfo, Errno> {
        let flags = flags.bits() & !MSG_DONTWAIT & !MSG_WAITALL;

        fn with_res<F: FnOnce(&RecvMessageInfo) -> Result<(), Errno>>(
            res: Result<Result<RecvMessageInfo, ZxioErrorCode>, zx::Status>,
            f: F,
        ) -> Result<RecvMessageInfo, Errno> {
            let info = res
                .map_err(|status| from_status_like_fdio!(status))?
                .map_err(|out_code| errno_from_zxio_code!(out_code))?;
            f(&info)?;
            Ok(info)
        }

        let res = if UNIFIED_ASPACES_ENABLED {
            match data.peek_all_segments_as_iovecs() {
                Ok(mut iovecs) => {
                    let res = self.zxio.recvmsg(&mut iovecs, flags);
                    Some(with_res(res, |info| {
                        // SAFETY: we successfully read `info.bytes_read` bytes
                        // directly to the user's buffer segments.
                        unsafe { data.advance(info.bytes_read) }
                    }))
                }
                Err(e) => {
                    if e.code == ENOTSUP {
                        None
                    } else {
                        return Err(e);
                    }
                }
            }
        } else {
            None
        };

        // If we can't pass the segments directly, fallback to receiving
        // all the bytes in an intermediate buffer and writing that
        // to our output buffer.
        res.unwrap_or_else(|| {
            // TODO: use MaybeUninit
            let mut buf = vec![0; data.available()];
            let res = self.zxio.recvmsg(
                &mut [syncio::zxio::iovec {
                    iov_base: buf.as_mut_ptr() as *mut starnix_uapi::c_void,
                    iov_len: buf.len(),
                }],
                flags,
            );
            with_res(res, |info| {
                let written = data.write_all(&buf[..info.bytes_read])?;
                debug_assert_eq!(written, info.bytes_read);
                Ok(())
            })
        })
    }

    fn attach_cbpf_filter(&self, _task: &Task, code: Vec<sock_filter>) -> Result<(), Errno> {
        // SO_ATTACH_FILTER is supported only for packet sockets.
        let domain = self
            .zxio
            .getsockopt(SOL_SOCKET, SO_DOMAIN, std::mem::size_of::<u32>() as u32)
            .map_err(|status| from_status_like_fdio!(status))?
            .map_err(|out_code| errno_from_zxio_code!(out_code))?;
        let domain = u32::from_ne_bytes(domain.try_into().unwrap());
        if domain != u32::from(AF_PACKET) {
            return error!(ENOTSUP);
        }

        let program = convert_and_verify_cbpf(&code, ebpf_api::SK_BUF_TYPE.clone())
            .map_err(|_| errno!(EINVAL))?;

        // TODO(https://fxbug.dev/377332291) Use `zxio_borrow()` to avoid cloning the handle.
        let packet_socket = fidl::endpoints::ClientEnd::<fposix_socket_packet::SocketMarker>::new(
            self.zxio.clone_handle().map_err(|_| errno!(EIO))?.into(),
        )
        .into_sync_proxy();
        let code = program.to_code();
        let code = unsafe { std::slice::from_raw_parts(code.as_ptr() as *const u64, code.len()) };
        let result = packet_socket.attach_bpf_filter_unsafe(code, zx::MonotonicInstant::INFINITE);
        result.map_err(|_: fidl::Error| errno!(EIO))?.map_err(|e| {
            Errno::with_context(
                ErrnoCode::from_error_code(e.into_primitive() as i16),
                "AttachBfpFilterUnsafe",
            )
        })
    }
}

impl SocketOps for ZxioBackedSocket {
    fn get_socket_info(&self) -> Result<(SocketDomain, SocketType, SocketProtocol), Errno> {
        let getsockopt = |optname: u32| -> Result<u32, Errno> {
            Ok(u32::from_ne_bytes(
                self.zxio
                    .getsockopt(SOL_SOCKET, optname, std::mem::size_of::<u32>() as u32)
                    .map_err(|status| from_status_like_fdio!(status))?
                    .map_err(|out_code| errno_from_zxio_code!(out_code))?
                    .try_into()
                    .unwrap(),
            ))
        };

        let domain_raw = getsockopt(SO_DOMAIN)?;
        let domain = SocketDomain::from_raw(domain_raw.try_into().map_err(|_| errno!(EINVAL))?)
            .ok_or_else(|| errno!(EINVAL))?;

        let type_raw = getsockopt(SO_TYPE)?;
        let socket_type = SocketType::from_raw(type_raw).ok_or_else(|| errno!(EINVAL))?;

        let protocol_raw = getsockopt(SO_PROTOCOL)?;
        let protocol = SocketProtocol::from_raw(protocol_raw);

        Ok((domain, socket_type, protocol))
    }

    fn connect(
        &self,
        _socket: &SocketHandle,
        _current_task: &CurrentTask,
        peer: SocketPeer,
    ) -> Result<(), Errno> {
        match peer {
            SocketPeer::Address(SocketAddress::Inet(addr))
            | SocketPeer::Address(SocketAddress::Inet6(addr))
            | SocketPeer::Address(SocketAddress::Packet(addr)) => self
                .zxio
                .connect(&addr)
                .map_err(|status| from_status_like_fdio!(status))?
                .map_err(|out_code| errno_from_zxio_code!(out_code)),
            _ => error!(EINVAL),
        }
    }

    fn listen(&self, _socket: &Socket, backlog: i32, _credentials: ucred) -> Result<(), Errno> {
        self.zxio
            .listen(backlog)
            .map_err(|status| from_status_like_fdio!(status))?
            .map_err(|out_code| errno_from_zxio_code!(out_code))
    }

    fn accept(&self, socket: &Socket) -> Result<SocketHandle, Errno> {
        let zxio = self
            .zxio
            .accept()
            .map_err(|status| from_status_like_fdio!(status))?
            .map_err(|out_code| errno_from_zxio_code!(out_code))?;

        Ok(Socket::new_with_ops_and_info(
            Box::new(ZxioBackedSocket { zxio: Arc::new(zxio) }),
            socket.domain,
            socket.socket_type,
            socket.protocol,
        ))
    }

    fn bind(
        &self,
        _socket: &Socket,
        _current_task: &CurrentTask,
        socket_address: SocketAddress,
    ) -> Result<(), Errno> {
        match socket_address {
            SocketAddress::Inet(addr)
            | SocketAddress::Inet6(addr)
            | SocketAddress::Packet(addr) => self
                .zxio
                .bind(&addr)
                .map_err(|status| from_status_like_fdio!(status))?
                .map_err(|out_code| errno_from_zxio_code!(out_code)),
            _ => error!(EINVAL),
        }
    }

    fn read(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        socket: &Socket,
        _current_task: &CurrentTask,
        data: &mut dyn OutputBuffer,
        flags: SocketMessageFlags,
    ) -> Result<MessageReadInfo, Errno> {
        // MSG_ERRQUEUE is not supported for TCP sockets, but it's expected to fail with EAGAIN.
        if socket.socket_type == SocketType::Stream && flags.contains(SocketMessageFlags::ERRQUEUE)
        {
            return error!(EAGAIN);
        }

        let mut info = self.recvmsg(data, flags)?;

        let bytes_read = info.bytes_read;

        let address = if !info.address.is_empty() {
            Some(SocketAddress::from_bytes(info.address)?)
        } else {
            None
        };

        Ok(MessageReadInfo {
            bytes_read,
            message_length: info.message_length,
            address,
            ancillary_data: info.control_messages.drain(..).map(AncillaryData::Ip).collect(),
        })
    }

    fn write(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        socket: &Socket,
        _current_task: &CurrentTask,
        data: &mut dyn InputBuffer,
        dest_address: &mut Option<SocketAddress>,
        ancillary_data: &mut Vec<AncillaryData>,
    ) -> Result<usize, Errno> {
        let mut cmsgs = vec![];
        for d in ancillary_data.drain(..) {
            match d {
                AncillaryData::Ip(msg) => cmsgs.push(msg),
                _ => return error!(EINVAL),
            }
        }

        // Ignore destination address if this is a stream socket.
        let dest_address =
            if socket.socket_type == SocketType::Stream { &None } else { dest_address };
        self.sendmsg(dest_address, data, cmsgs, SocketMessageFlags::empty())
    }

    fn wait_async(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _socket: &Socket,
        _current_task: &CurrentTask,
        waiter: &Waiter,
        events: FdEvents,
        handler: EventHandler,
    ) -> WaitCanceler {
        zxio_wait_async(&self.zxio, waiter, events, handler)
    }

    fn query_events(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _socket: &Socket,
        _current_task: &CurrentTask,
    ) -> Result<FdEvents, Errno> {
        zxio_query_events(&self.zxio)
    }

    fn shutdown(&self, _socket: &Socket, how: SocketShutdownFlags) -> Result<(), Errno> {
        self.zxio
            .shutdown(how)
            .map_err(|status| from_status_like_fdio!(status))?
            .map_err(|out_code| errno_from_zxio_code!(out_code))
    }

    fn close(&self, _socket: &Socket) {}

    fn getsockname(&self, socket: &Socket) -> Result<SocketAddress, Errno> {
        match self.zxio.getsockname() {
            Err(_) | Ok(Err(_)) => Ok(SocketAddress::default_for_domain(socket.domain)),
            Ok(Ok(addr)) => SocketAddress::from_bytes(addr),
        }
    }

    fn getpeername(&self, _socket: &Socket) -> Result<SocketAddress, Errno> {
        self.zxio
            .getpeername()
            .map_err(|status| from_status_like_fdio!(status))?
            .map_err(|out_code| errno_from_zxio_code!(out_code))
            .and_then(SocketAddress::from_bytes)
    }

    fn setsockopt(
        &self,
        _socket: &Socket,
        task: &Task,
        level: u32,
        optname: u32,
        user_opt: UserBuffer,
    ) -> Result<(), Errno> {
        match (level, optname) {
            (SOL_SOCKET, SO_ATTACH_FILTER) => {
                let fprog = task.read_object::<sock_fprog>(user_opt.try_into()?)?;
                if u32::from(fprog.len) > BPF_MAXINSNS || fprog.len == 0 {
                    return error!(EINVAL);
                }
                let code: Vec<sock_filter> =
                    task.read_objects_to_vec(fprog.filter.into(), fprog.len as usize)?;
                self.attach_cbpf_filter(task, code)
            }
            (SOL_IP, IP_RECVERR) => {
                track_stub!(TODO("https://fxbug.dev/333060595"), "SOL_IP.IP_RECVERR");
                Ok(())
            }
            _ => {
                let optval = task.read_buffer(&user_opt)?;
                self.zxio
                    .setsockopt(level as i32, optname as i32, &optval)
                    .map_err(|status| from_status_like_fdio!(status))?
                    .map_err(|out_code| errno_from_zxio_code!(out_code))
            }
        }
    }

    fn getsockopt(
        &self,
        _socket: &Socket,
        level: u32,
        optname: u32,
        optlen: u32,
    ) -> Result<Vec<u8>, Errno> {
        self.zxio
            .getsockopt(level, optname, optlen)
            .map_err(|status| from_status_like_fdio!(status))?
            .map_err(|out_code| errno_from_zxio_code!(out_code))
    }

    fn to_handle(
        &self,
        _socket: &Socket,
        _current_task: &CurrentTask,
    ) -> Result<Option<zx::Handle>, Errno> {
        self.zxio
            .as_ref()
            .clone()
            .and_then(Zxio::release)
            .map(Some)
            .map_err(|status| from_status_like_fdio!(status))
    }
}

// Check that values that are passed to and from ZXIO have the same meaning.
const_assert_eq!(syncio::zxio::AF_UNSPEC, uapi::AF_UNSPEC as u32);
const_assert_eq!(syncio::zxio::AF_UNIX, uapi::AF_UNIX as u32);
const_assert_eq!(syncio::zxio::AF_INET, uapi::AF_INET as u32);
const_assert_eq!(syncio::zxio::AF_INET6, uapi::AF_INET6 as u32);
const_assert_eq!(syncio::zxio::AF_NETLINK, uapi::AF_NETLINK as u32);
const_assert_eq!(syncio::zxio::AF_PACKET, uapi::AF_PACKET as u32);
const_assert_eq!(syncio::zxio::AF_VSOCK, uapi::AF_VSOCK as u32);

const_assert_eq!(syncio::zxio::SO_DEBUG, uapi::SO_DEBUG);
const_assert_eq!(syncio::zxio::SO_REUSEADDR, uapi::SO_REUSEADDR);
const_assert_eq!(syncio::zxio::SO_TYPE, uapi::SO_TYPE);
const_assert_eq!(syncio::zxio::SO_ERROR, uapi::SO_ERROR);
const_assert_eq!(syncio::zxio::SO_DONTROUTE, uapi::SO_DONTROUTE);
const_assert_eq!(syncio::zxio::SO_BROADCAST, uapi::SO_BROADCAST);
const_assert_eq!(syncio::zxio::SO_SNDBUF, uapi::SO_SNDBUF);
const_assert_eq!(syncio::zxio::SO_RCVBUF, uapi::SO_RCVBUF);
const_assert_eq!(syncio::zxio::SO_KEEPALIVE, uapi::SO_KEEPALIVE);
const_assert_eq!(syncio::zxio::SO_OOBINLINE, uapi::SO_OOBINLINE);
const_assert_eq!(syncio::zxio::SO_NO_CHECK, uapi::SO_NO_CHECK);
const_assert_eq!(syncio::zxio::SO_PRIORITY, uapi::SO_PRIORITY);
const_assert_eq!(syncio::zxio::SO_LINGER, uapi::SO_LINGER);
const_assert_eq!(syncio::zxio::SO_BSDCOMPAT, uapi::SO_BSDCOMPAT);
const_assert_eq!(syncio::zxio::SO_REUSEPORT, uapi::SO_REUSEPORT);
const_assert_eq!(syncio::zxio::SO_PASSCRED, uapi::SO_PASSCRED);
const_assert_eq!(syncio::zxio::SO_PEERCRED, uapi::SO_PEERCRED);
const_assert_eq!(syncio::zxio::SO_RCVLOWAT, uapi::SO_RCVLOWAT);
const_assert_eq!(syncio::zxio::SO_SNDLOWAT, uapi::SO_SNDLOWAT);
const_assert_eq!(syncio::zxio::SO_ACCEPTCONN, uapi::SO_ACCEPTCONN);
const_assert_eq!(syncio::zxio::SO_PEERSEC, uapi::SO_PEERSEC);
const_assert_eq!(syncio::zxio::SO_SNDBUFFORCE, uapi::SO_SNDBUFFORCE);
const_assert_eq!(syncio::zxio::SO_RCVBUFFORCE, uapi::SO_RCVBUFFORCE);
const_assert_eq!(syncio::zxio::SO_PROTOCOL, uapi::SO_PROTOCOL);
const_assert_eq!(syncio::zxio::SO_DOMAIN, uapi::SO_DOMAIN);
const_assert_eq!(syncio::zxio::SO_RCVTIMEO, uapi::SO_RCVTIMEO);
const_assert_eq!(syncio::zxio::SO_SNDTIMEO, uapi::SO_SNDTIMEO);
const_assert_eq!(syncio::zxio::SO_TIMESTAMP, uapi::SO_TIMESTAMP);
const_assert_eq!(syncio::zxio::SO_TIMESTAMPNS, uapi::SO_TIMESTAMPNS);
const_assert_eq!(syncio::zxio::SO_TIMESTAMPING, uapi::SO_TIMESTAMPING);
const_assert_eq!(syncio::zxio::SO_SECURITY_AUTHENTICATION, uapi::SO_SECURITY_AUTHENTICATION);
const_assert_eq!(
    syncio::zxio::SO_SECURITY_ENCRYPTION_TRANSPORT,
    uapi::SO_SECURITY_ENCRYPTION_TRANSPORT
);
const_assert_eq!(
    syncio::zxio::SO_SECURITY_ENCRYPTION_NETWORK,
    uapi::SO_SECURITY_ENCRYPTION_NETWORK
);
const_assert_eq!(syncio::zxio::SO_BINDTODEVICE, uapi::SO_BINDTODEVICE);
const_assert_eq!(syncio::zxio::SO_ATTACH_FILTER, uapi::SO_ATTACH_FILTER);
const_assert_eq!(syncio::zxio::SO_DETACH_FILTER, uapi::SO_DETACH_FILTER);
const_assert_eq!(syncio::zxio::SO_GET_FILTER, uapi::SO_GET_FILTER);
const_assert_eq!(syncio::zxio::SO_PEERNAME, uapi::SO_PEERNAME);
const_assert_eq!(syncio::zxio::SO_PASSSEC, uapi::SO_PASSSEC);
const_assert_eq!(syncio::zxio::SO_MARK, uapi::SO_MARK);
const_assert_eq!(syncio::zxio::SO_RXQ_OVFL, uapi::SO_RXQ_OVFL);
const_assert_eq!(syncio::zxio::SO_WIFI_STATUS, uapi::SO_WIFI_STATUS);
const_assert_eq!(syncio::zxio::SO_PEEK_OFF, uapi::SO_PEEK_OFF);
const_assert_eq!(syncio::zxio::SO_NOFCS, uapi::SO_NOFCS);
const_assert_eq!(syncio::zxio::SO_LOCK_FILTER, uapi::SO_LOCK_FILTER);
const_assert_eq!(syncio::zxio::SO_SELECT_ERR_QUEUE, uapi::SO_SELECT_ERR_QUEUE);
const_assert_eq!(syncio::zxio::SO_BUSY_POLL, uapi::SO_BUSY_POLL);
const_assert_eq!(syncio::zxio::SO_MAX_PACING_RATE, uapi::SO_MAX_PACING_RATE);
const_assert_eq!(syncio::zxio::SO_BPF_EXTENSIONS, uapi::SO_BPF_EXTENSIONS);
const_assert_eq!(syncio::zxio::SO_INCOMING_CPU, uapi::SO_INCOMING_CPU);
const_assert_eq!(syncio::zxio::SO_ATTACH_BPF, uapi::SO_ATTACH_BPF);
const_assert_eq!(syncio::zxio::SO_DETACH_BPF, uapi::SO_DETACH_BPF);
const_assert_eq!(syncio::zxio::SO_ATTACH_REUSEPORT_CBPF, uapi::SO_ATTACH_REUSEPORT_CBPF);
const_assert_eq!(syncio::zxio::SO_ATTACH_REUSEPORT_EBPF, uapi::SO_ATTACH_REUSEPORT_EBPF);
const_assert_eq!(syncio::zxio::SO_CNX_ADVICE, uapi::SO_CNX_ADVICE);
const_assert_eq!(syncio::zxio::SO_MEMINFO, uapi::SO_MEMINFO);
const_assert_eq!(syncio::zxio::SO_INCOMING_NAPI_ID, uapi::SO_INCOMING_NAPI_ID);
const_assert_eq!(syncio::zxio::SO_COOKIE, uapi::SO_COOKIE);
const_assert_eq!(syncio::zxio::SO_PEERGROUPS, uapi::SO_PEERGROUPS);
const_assert_eq!(syncio::zxio::SO_ZEROCOPY, uapi::SO_ZEROCOPY);
const_assert_eq!(syncio::zxio::SO_TXTIME, uapi::SO_TXTIME);
const_assert_eq!(syncio::zxio::SO_BINDTOIFINDEX, uapi::SO_BINDTOIFINDEX);
const_assert_eq!(syncio::zxio::SO_DETACH_REUSEPORT_BPF, uapi::SO_DETACH_REUSEPORT_BPF);
const_assert_eq!(syncio::zxio::SO_ORIGINAL_DST, uapi::SO_ORIGINAL_DST);

const_assert_eq!(syncio::zxio::MSG_WAITALL, uapi::MSG_WAITALL);
const_assert_eq!(syncio::zxio::MSG_PEEK, uapi::MSG_PEEK);
const_assert_eq!(syncio::zxio::MSG_DONTROUTE, uapi::MSG_DONTROUTE);
const_assert_eq!(syncio::zxio::MSG_CTRUNC, uapi::MSG_CTRUNC);
const_assert_eq!(syncio::zxio::MSG_PROXY, uapi::MSG_PROXY);
const_assert_eq!(syncio::zxio::MSG_TRUNC, uapi::MSG_TRUNC);
const_assert_eq!(syncio::zxio::MSG_DONTWAIT, uapi::MSG_DONTWAIT);
const_assert_eq!(syncio::zxio::MSG_EOR, uapi::MSG_EOR);
const_assert_eq!(syncio::zxio::MSG_WAITALL, uapi::MSG_WAITALL);
const_assert_eq!(syncio::zxio::MSG_FIN, uapi::MSG_FIN);
const_assert_eq!(syncio::zxio::MSG_SYN, uapi::MSG_SYN);
const_assert_eq!(syncio::zxio::MSG_CONFIRM, uapi::MSG_CONFIRM);
const_assert_eq!(syncio::zxio::MSG_RST, uapi::MSG_RST);
const_assert_eq!(syncio::zxio::MSG_ERRQUEUE, uapi::MSG_ERRQUEUE);
const_assert_eq!(syncio::zxio::MSG_NOSIGNAL, uapi::MSG_NOSIGNAL);
const_assert_eq!(syncio::zxio::MSG_MORE, uapi::MSG_MORE);
const_assert_eq!(syncio::zxio::MSG_WAITFORONE, uapi::MSG_WAITFORONE);
const_assert_eq!(syncio::zxio::MSG_BATCH, uapi::MSG_BATCH);
const_assert_eq!(syncio::zxio::MSG_FASTOPEN, uapi::MSG_FASTOPEN);
const_assert_eq!(syncio::zxio::MSG_CMSG_CLOEXEC, uapi::MSG_CMSG_CLOEXEC);

const_assert_eq!(syncio::zxio::IP_TOS, uapi::IP_TOS);
const_assert_eq!(syncio::zxio::IP_TTL, uapi::IP_TTL);
const_assert_eq!(syncio::zxio::IP_HDRINCL, uapi::IP_HDRINCL);
const_assert_eq!(syncio::zxio::IP_OPTIONS, uapi::IP_OPTIONS);
const_assert_eq!(syncio::zxio::IP_ROUTER_ALERT, uapi::IP_ROUTER_ALERT);
const_assert_eq!(syncio::zxio::IP_RECVOPTS, uapi::IP_RECVOPTS);
const_assert_eq!(syncio::zxio::IP_RETOPTS, uapi::IP_RETOPTS);
const_assert_eq!(syncio::zxio::IP_PKTINFO, uapi::IP_PKTINFO);
const_assert_eq!(syncio::zxio::IP_PKTOPTIONS, uapi::IP_PKTOPTIONS);
const_assert_eq!(syncio::zxio::IP_MTU_DISCOVER, uapi::IP_MTU_DISCOVER);
const_assert_eq!(syncio::zxio::IP_RECVERR, uapi::IP_RECVERR);
const_assert_eq!(syncio::zxio::IP_RECVTTL, uapi::IP_RECVTTL);
const_assert_eq!(syncio::zxio::IP_RECVTOS, uapi::IP_RECVTOS);
const_assert_eq!(syncio::zxio::IP_MTU, uapi::IP_MTU);
const_assert_eq!(syncio::zxio::IP_FREEBIND, uapi::IP_FREEBIND);
const_assert_eq!(syncio::zxio::IP_IPSEC_POLICY, uapi::IP_IPSEC_POLICY);
const_assert_eq!(syncio::zxio::IP_XFRM_POLICY, uapi::IP_XFRM_POLICY);
const_assert_eq!(syncio::zxio::IP_PASSSEC, uapi::IP_PASSSEC);
const_assert_eq!(syncio::zxio::IP_TRANSPARENT, uapi::IP_TRANSPARENT);
const_assert_eq!(syncio::zxio::IP_ORIGDSTADDR, uapi::IP_ORIGDSTADDR);
const_assert_eq!(syncio::zxio::IP_RECVORIGDSTADDR, uapi::IP_RECVORIGDSTADDR);
const_assert_eq!(syncio::zxio::IP_MINTTL, uapi::IP_MINTTL);
const_assert_eq!(syncio::zxio::IP_NODEFRAG, uapi::IP_NODEFRAG);
const_assert_eq!(syncio::zxio::IP_CHECKSUM, uapi::IP_CHECKSUM);
const_assert_eq!(syncio::zxio::IP_BIND_ADDRESS_NO_PORT, uapi::IP_BIND_ADDRESS_NO_PORT);
const_assert_eq!(syncio::zxio::IP_MULTICAST_IF, uapi::IP_MULTICAST_IF);
const_assert_eq!(syncio::zxio::IP_MULTICAST_TTL, uapi::IP_MULTICAST_TTL);
const_assert_eq!(syncio::zxio::IP_MULTICAST_LOOP, uapi::IP_MULTICAST_LOOP);
const_assert_eq!(syncio::zxio::IP_ADD_MEMBERSHIP, uapi::IP_ADD_MEMBERSHIP);
const_assert_eq!(syncio::zxio::IP_DROP_MEMBERSHIP, uapi::IP_DROP_MEMBERSHIP);
const_assert_eq!(syncio::zxio::IP_UNBLOCK_SOURCE, uapi::IP_UNBLOCK_SOURCE);
const_assert_eq!(syncio::zxio::IP_BLOCK_SOURCE, uapi::IP_BLOCK_SOURCE);
const_assert_eq!(syncio::zxio::IP_ADD_SOURCE_MEMBERSHIP, uapi::IP_ADD_SOURCE_MEMBERSHIP);
const_assert_eq!(syncio::zxio::IP_DROP_SOURCE_MEMBERSHIP, uapi::IP_DROP_SOURCE_MEMBERSHIP);
const_assert_eq!(syncio::zxio::IP_MSFILTER, uapi::IP_MSFILTER);
const_assert_eq!(syncio::zxio::IP_MULTICAST_ALL, uapi::IP_MULTICAST_ALL);
const_assert_eq!(syncio::zxio::IP_UNICAST_IF, uapi::IP_UNICAST_IF);
const_assert_eq!(syncio::zxio::IP_RECVRETOPTS, uapi::IP_RECVRETOPTS);
const_assert_eq!(syncio::zxio::IP_PMTUDISC_DONT, uapi::IP_PMTUDISC_DONT);
const_assert_eq!(syncio::zxio::IP_PMTUDISC_WANT, uapi::IP_PMTUDISC_WANT);
const_assert_eq!(syncio::zxio::IP_PMTUDISC_DO, uapi::IP_PMTUDISC_DO);
const_assert_eq!(syncio::zxio::IP_PMTUDISC_PROBE, uapi::IP_PMTUDISC_PROBE);
const_assert_eq!(syncio::zxio::IP_PMTUDISC_INTERFACE, uapi::IP_PMTUDISC_INTERFACE);
const_assert_eq!(syncio::zxio::IP_PMTUDISC_OMIT, uapi::IP_PMTUDISC_OMIT);
const_assert_eq!(syncio::zxio::IP_DEFAULT_MULTICAST_TTL, uapi::IP_DEFAULT_MULTICAST_TTL);
const_assert_eq!(syncio::zxio::IP_DEFAULT_MULTICAST_LOOP, uapi::IP_DEFAULT_MULTICAST_LOOP);

const_assert_eq!(syncio::zxio::IPV6_ADDRFORM, uapi::IPV6_ADDRFORM);
const_assert_eq!(syncio::zxio::IPV6_2292PKTINFO, uapi::IPV6_2292PKTINFO);
const_assert_eq!(syncio::zxio::IPV6_2292HOPOPTS, uapi::IPV6_2292HOPOPTS);
const_assert_eq!(syncio::zxio::IPV6_2292DSTOPTS, uapi::IPV6_2292DSTOPTS);
const_assert_eq!(syncio::zxio::IPV6_2292RTHDR, uapi::IPV6_2292RTHDR);
const_assert_eq!(syncio::zxio::IPV6_2292PKTOPTIONS, uapi::IPV6_2292PKTOPTIONS);
const_assert_eq!(syncio::zxio::IPV6_CHECKSUM, uapi::IPV6_CHECKSUM);
const_assert_eq!(syncio::zxio::IPV6_2292HOPLIMIT, uapi::IPV6_2292HOPLIMIT);
const_assert_eq!(syncio::zxio::IPV6_NEXTHOP, uapi::IPV6_NEXTHOP);
const_assert_eq!(syncio::zxio::IPV6_AUTHHDR, uapi::IPV6_AUTHHDR);
const_assert_eq!(syncio::zxio::IPV6_UNICAST_HOPS, uapi::IPV6_UNICAST_HOPS);
const_assert_eq!(syncio::zxio::IPV6_MULTICAST_IF, uapi::IPV6_MULTICAST_IF);
const_assert_eq!(syncio::zxio::IPV6_MULTICAST_HOPS, uapi::IPV6_MULTICAST_HOPS);
const_assert_eq!(syncio::zxio::IPV6_MULTICAST_LOOP, uapi::IPV6_MULTICAST_LOOP);
const_assert_eq!(syncio::zxio::IPV6_ROUTER_ALERT, uapi::IPV6_ROUTER_ALERT);
const_assert_eq!(syncio::zxio::IPV6_MTU_DISCOVER, uapi::IPV6_MTU_DISCOVER);
const_assert_eq!(syncio::zxio::IPV6_MTU, uapi::IPV6_MTU);
const_assert_eq!(syncio::zxio::IPV6_RECVERR, uapi::IPV6_RECVERR);
const_assert_eq!(syncio::zxio::IPV6_V6ONLY, uapi::IPV6_V6ONLY);
const_assert_eq!(syncio::zxio::IPV6_JOIN_ANYCAST, uapi::IPV6_JOIN_ANYCAST);
const_assert_eq!(syncio::zxio::IPV6_LEAVE_ANYCAST, uapi::IPV6_LEAVE_ANYCAST);
const_assert_eq!(syncio::zxio::IPV6_IPSEC_POLICY, uapi::IPV6_IPSEC_POLICY);
const_assert_eq!(syncio::zxio::IPV6_XFRM_POLICY, uapi::IPV6_XFRM_POLICY);
const_assert_eq!(syncio::zxio::IPV6_HDRINCL, uapi::IPV6_HDRINCL);
const_assert_eq!(syncio::zxio::IPV6_RECVPKTINFO, uapi::IPV6_RECVPKTINFO);
const_assert_eq!(syncio::zxio::IPV6_PKTINFO, uapi::IPV6_PKTINFO);
const_assert_eq!(syncio::zxio::IPV6_RECVHOPLIMIT, uapi::IPV6_RECVHOPLIMIT);
const_assert_eq!(syncio::zxio::IPV6_HOPLIMIT, uapi::IPV6_HOPLIMIT);
const_assert_eq!(syncio::zxio::IPV6_RECVHOPOPTS, uapi::IPV6_RECVHOPOPTS);
const_assert_eq!(syncio::zxio::IPV6_HOPOPTS, uapi::IPV6_HOPOPTS);
const_assert_eq!(syncio::zxio::IPV6_RTHDRDSTOPTS, uapi::IPV6_RTHDRDSTOPTS);
const_assert_eq!(syncio::zxio::IPV6_RECVRTHDR, uapi::IPV6_RECVRTHDR);
const_assert_eq!(syncio::zxio::IPV6_RTHDR, uapi::IPV6_RTHDR);
const_assert_eq!(syncio::zxio::IPV6_RECVDSTOPTS, uapi::IPV6_RECVDSTOPTS);
const_assert_eq!(syncio::zxio::IPV6_DSTOPTS, uapi::IPV6_DSTOPTS);
const_assert_eq!(syncio::zxio::IPV6_RECVPATHMTU, uapi::IPV6_RECVPATHMTU);
const_assert_eq!(syncio::zxio::IPV6_PATHMTU, uapi::IPV6_PATHMTU);
const_assert_eq!(syncio::zxio::IPV6_DONTFRAG, uapi::IPV6_DONTFRAG);
const_assert_eq!(syncio::zxio::IPV6_RECVTCLASS, uapi::IPV6_RECVTCLASS);
const_assert_eq!(syncio::zxio::IPV6_TCLASS, uapi::IPV6_TCLASS);
const_assert_eq!(syncio::zxio::IPV6_AUTOFLOWLABEL, uapi::IPV6_AUTOFLOWLABEL);
const_assert_eq!(syncio::zxio::IPV6_ADDR_PREFERENCES, uapi::IPV6_ADDR_PREFERENCES);
const_assert_eq!(syncio::zxio::IPV6_MINHOPCOUNT, uapi::IPV6_MINHOPCOUNT);
const_assert_eq!(syncio::zxio::IPV6_ORIGDSTADDR, uapi::IPV6_ORIGDSTADDR);
const_assert_eq!(syncio::zxio::IPV6_RECVORIGDSTADDR, uapi::IPV6_RECVORIGDSTADDR);
const_assert_eq!(syncio::zxio::IPV6_TRANSPARENT, uapi::IPV6_TRANSPARENT);
const_assert_eq!(syncio::zxio::IPV6_UNICAST_IF, uapi::IPV6_UNICAST_IF);
const_assert_eq!(syncio::zxio::IPV6_ADD_MEMBERSHIP, uapi::IPV6_ADD_MEMBERSHIP);
const_assert_eq!(syncio::zxio::IPV6_DROP_MEMBERSHIP, uapi::IPV6_DROP_MEMBERSHIP);
const_assert_eq!(syncio::zxio::IPV6_PMTUDISC_DONT, uapi::IPV6_PMTUDISC_DONT);
const_assert_eq!(syncio::zxio::IPV6_PMTUDISC_WANT, uapi::IPV6_PMTUDISC_WANT);
const_assert_eq!(syncio::zxio::IPV6_PMTUDISC_DO, uapi::IPV6_PMTUDISC_DO);
const_assert_eq!(syncio::zxio::IPV6_PMTUDISC_PROBE, uapi::IPV6_PMTUDISC_PROBE);
const_assert_eq!(syncio::zxio::IPV6_PMTUDISC_INTERFACE, uapi::IPV6_PMTUDISC_INTERFACE);
const_assert_eq!(syncio::zxio::IPV6_PMTUDISC_OMIT, uapi::IPV6_PMTUDISC_OMIT);
const_assert_eq!(syncio::zxio::IPV6_PREFER_SRC_TMP, uapi::IPV6_PREFER_SRC_TMP);
const_assert_eq!(syncio::zxio::IPV6_PREFER_SRC_PUBLIC, uapi::IPV6_PREFER_SRC_PUBLIC);
const_assert_eq!(
    syncio::zxio::IPV6_PREFER_SRC_PUBTMP_DEFAULT,
    uapi::IPV6_PREFER_SRC_PUBTMP_DEFAULT
);
const_assert_eq!(syncio::zxio::IPV6_PREFER_SRC_COA, uapi::IPV6_PREFER_SRC_COA);
const_assert_eq!(syncio::zxio::IPV6_PREFER_SRC_HOME, uapi::IPV6_PREFER_SRC_HOME);
const_assert_eq!(syncio::zxio::IPV6_PREFER_SRC_CGA, uapi::IPV6_PREFER_SRC_CGA);
const_assert_eq!(syncio::zxio::IPV6_PREFER_SRC_NONCGA, uapi::IPV6_PREFER_SRC_NONCGA);
