// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::*;

use crate::fs::buffers::*;
use crate::fs::fuchsia::*;
use crate::fs::*;
use crate::logging::*;
use crate::mm::MemoryAccessorExt;
use crate::task::*;
use crate::types::*;
use fidl::endpoints::ProtocolMarker;
use fidl_fuchsia_posix_socket as fposix_socket;
use fidl_fuchsia_posix_socket_raw as fposix_socket_raw;
use fuchsia_component::client::connect_channel_to_protocol;
use fuchsia_zircon as zx;
use std::sync::Arc;
use syncio::{RecvMessageInfo, ServiceConnector, Zxio};

/// Connects to `fuchsia_posix_socket::Provider` or
/// `fuchsia_posix_socket_raw::Provider`.
struct SocketProviderServiceConnector;

impl ServiceConnector for SocketProviderServiceConnector {
    fn connect(service: &str, server_end: zx::Channel) -> zx::Status {
        let result = if service == fposix_socket_raw::ProviderMarker::DEBUG_NAME {
            connect_channel_to_protocol::<fposix_socket_raw::ProviderMarker>(server_end)
        } else if service == fposix_socket::ProviderMarker::DEBUG_NAME {
            connect_channel_to_protocol::<fposix_socket::ProviderMarker>(server_end)
        } else {
            return zx::Status::INTERNAL;
        };

        if let Err(err) = result {
            if let Some(status) = err.downcast_ref::<zx::Status>() {
                return *status;
            }
            return zx::Status::INTERNAL;
        }

        zx::Status::OK
    }
}

pub struct InetSocket {
    /// The underlying Zircon I/O object.
    zxio: Arc<syncio::Zxio>,
}

impl InetSocket {
    pub fn new(
        domain: SocketDomain,
        socket_type: SocketType,
        protocol: SocketProtocol,
    ) -> Result<InetSocket, Errno> {
        let zxio = Zxio::new_socket::<SocketProviderServiceConnector>(
            domain.as_raw() as c_int,
            socket_type.as_raw() as c_int,
            protocol.as_raw() as c_int,
        )
        .map_err(|status| from_status_like_fdio!(status))?
        .map_err(|out_code| errno_from_zxio_code!(out_code))?;

        Ok(InetSocket { zxio: Arc::new(zxio) })
    }

    pub fn sendmsg(
        &self,
        addr: &Option<SocketAddress>,
        data: &mut dyn InputBuffer,
        cmsg: Vec<u8>,
        flags: SocketMessageFlags,
    ) -> Result<usize, Errno> {
        let addr = match addr {
            Some(sockaddr) => match sockaddr {
                SocketAddress::Inet(sockaddr) | SocketAddress::Inet6(sockaddr) => sockaddr.clone(),
                _ => return error!(EINVAL),
            },
            None => vec![],
        };

        let bytes = data.peek_all()?;
        let send_bytes = self
            .zxio
            .sendmsg(addr, bytes, cmsg, flags.bits() & !MSG_DONTWAIT)
            .map_err(|status| from_status_like_fdio!(status))?
            .map_err(|out_code| errno_from_zxio_code!(out_code))?;
        data.advance(send_bytes)?;
        Ok(send_bytes)
    }

    pub fn recvmsg(
        &self,
        iovec_length: usize,
        flags: SocketMessageFlags,
    ) -> Result<RecvMessageInfo, Errno> {
        self.zxio
            .recvmsg(iovec_length, flags.bits() & !MSG_DONTWAIT & !MSG_WAITALL)
            .map_err(|status| from_status_like_fdio!(status))?
            .map_err(|out_code| errno_from_zxio_code!(out_code))
    }
}

impl SocketOps for InetSocket {
    fn connect(
        &self,
        _socket: &SocketHandle,
        _current_task: &CurrentTask,
        peer: SocketPeer,
    ) -> Result<(), Errno> {
        match peer {
            SocketPeer::Address(SocketAddress::Inet(addr))
            | SocketPeer::Address(SocketAddress::Inet6(addr)) => self
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

        Ok(Socket::new_with_ops(
            socket.domain,
            socket.socket_type,
            socket.protocol,
            Box::new(InetSocket { zxio: Arc::new(zxio) }),
        ))
    }

    fn remote_connection(&self, _socket: &Socket, _file: FileHandle) -> Result<(), Errno> {
        not_implemented!("?", "InetSocket::remote_connection is stubbed");
        Ok(())
    }

    fn bind(
        &self,
        _socket: &Socket,
        _current_task: &CurrentTask,
        socket_address: SocketAddress,
    ) -> Result<(), Errno> {
        match socket_address {
            SocketAddress::Inet(addr) | SocketAddress::Inet6(addr) => self
                .zxio
                .bind(&addr)
                .map_err(|status| from_status_like_fdio!(status))?
                .map_err(|out_code| errno_from_zxio_code!(out_code)),
            _ => error!(EINVAL),
        }
    }

    fn read(
        &self,
        _socket: &Socket,
        _current_task: &CurrentTask,
        data: &mut dyn OutputBuffer,
        flags: SocketMessageFlags,
    ) -> Result<MessageReadInfo, Errno> {
        let iovec_length = data.available();
        let info = self.recvmsg(iovec_length, flags)?;

        let bytes_read = data.write_all(&info.message)?;

        let address = if !info.address.is_empty() {
            Some(SocketAddress::from_bytes(info.address)?)
        } else {
            None
        };

        // TODO: Handle ancillary_data.
        Ok(MessageReadInfo {
            bytes_read,
            message_length: info.message_length,
            address,
            ancillary_data: vec![],
        })
    }

    fn write(
        &self,
        _socket: &Socket,
        _current_task: &CurrentTask,
        data: &mut dyn InputBuffer,
        dest_address: &mut Option<SocketAddress>,
        _ancillary_data: &mut Vec<AncillaryData>,
    ) -> Result<usize, Errno> {
        // TODO: Handle ancillary_data.
        self.sendmsg(dest_address, data, vec![], SocketMessageFlags::empty())
    }

    fn wait_async(
        &self,
        _socket: &Socket,
        _current_task: &CurrentTask,
        waiter: &Waiter,
        events: FdEvents,
        handler: EventHandler,
    ) -> WaitKey {
        zxio_wait_async(&self.zxio, waiter, events, handler)
    }

    fn cancel_wait(
        &self,
        _socket: &Socket,
        _current_task: &CurrentTask,
        waiter: &Waiter,
        key: WaitKey,
    ) {
        zxio_cancel_wait(&self.zxio, waiter, key);
    }

    fn query_events(&self, _socket: &Socket, _current_task: &CurrentTask) -> FdEvents {
        zxio_query_events(&self.zxio)
    }

    fn shutdown(&self, _socket: &Socket, how: SocketShutdownFlags) -> Result<(), Errno> {
        self.zxio
            .shutdown(how)
            .map_err(|status| from_status_like_fdio!(status))?
            .map_err(|out_code| errno_from_zxio_code!(out_code))
    }

    fn close(&self, _socket: &Socket) {}

    fn getsockname(&self, socket: &Socket) -> Vec<u8> {
        match self.zxio.getsockname() {
            Err(_) | Ok(Err(_)) => SocketAddress::default_for_domain(socket.domain).to_bytes(),
            Ok(Ok(addr)) => addr,
        }
    }

    fn getpeername(&self, _socket: &Socket) -> Result<Vec<u8>, Errno> {
        self.zxio
            .getpeername()
            .map_err(|status| from_status_like_fdio!(status))?
            .map_err(|out_code| errno_from_zxio_code!(out_code))
    }

    fn setsockopt(
        &self,
        _socket: &Socket,
        task: &Task,
        level: u32,
        optname: u32,
        user_opt: UserBuffer,
    ) -> Result<(), Errno> {
        let optval = task.mm.read_buffer(&user_opt)?;

        self.zxio
            .setsockopt(level as i32, optname as i32, &optval)
            .map_err(|status| from_status_like_fdio!(status))?
            .map_err(|out_code| errno_from_zxio_code!(out_code))
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
}
