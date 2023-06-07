// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fuchsia_zircon as zx;
use std::collections::VecDeque;
use zerocopy::AsBytes;

use super::*;

use crate::{
    fs::{buffers::*, *},
    lock::Mutex,
    mm::MemoryAccessorExt,
    syscalls::SyscallResult,
    task::*,
    types::{as_any::*, *},
};

use std::sync::Arc;

pub const DEFAULT_LISTEN_BACKLOG: usize = 1024;

pub trait SocketOps: Send + Sync + AsAny {
    /// Connect the `socket` to the listening `peer`. On success
    /// a new socket is created and added to the accept queue.
    fn connect(
        &self,
        socket: &SocketHandle,
        current_task: &CurrentTask,
        peer: SocketPeer,
    ) -> Result<(), Errno>;

    /// Start listening at the bound address for `connect` calls.
    fn listen(&self, socket: &Socket, backlog: i32, credentials: ucred) -> Result<(), Errno>;

    /// Returns the eariest socket on the accept queue of this
    /// listening socket. Returns EAGAIN if the queue is empty.
    fn accept(&self, socket: &Socket) -> Result<SocketHandle, Errno>;

    /// Binds this socket to a `socket_address`.
    ///
    /// Returns an error if the socket could not be bound.
    fn bind(
        &self,
        socket: &Socket,
        current_task: &CurrentTask,
        socket_address: SocketAddress,
    ) -> Result<(), Errno>;

    /// Reads the specified number of bytes from the socket, if possible.
    ///
    /// # Parameters
    /// - `task`: The task to which the user buffers belong (i.e., the task to which the read bytes
    ///           are written.
    /// - `data`: The buffers to write the read data into.
    ///
    /// Returns the number of bytes that were written to the user buffers, as well as any ancillary
    /// data associated with the read messages.
    fn read(
        &self,
        socket: &Socket,
        current_task: &CurrentTask,
        data: &mut dyn OutputBuffer,
        flags: SocketMessageFlags,
    ) -> Result<MessageReadInfo, Errno>;

    /// Writes the data in the provided user buffers to this socket.
    ///
    /// # Parameters
    /// - `task`: The task to which the user buffers belong, used to read the memory.
    /// - `data`: The data to write to the socket.
    /// - `ancillary_data`: Optional ancillary data (a.k.a., control message) to write.
    ///
    /// Advances the iterator to indicate how much was actually written.
    fn write(
        &self,
        socket: &Socket,
        current_task: &CurrentTask,
        data: &mut dyn InputBuffer,
        dest_address: &mut Option<SocketAddress>,
        ancillary_data: &mut Vec<AncillaryData>,
    ) -> Result<usize, Errno>;

    /// Queues an asynchronous wait for the specified `events`
    /// on the `waiter`. Note that no wait occurs until a
    /// wait functions is called on the `waiter`.
    ///
    /// # Parameters
    /// - `waiter`: The Waiter that can be waited on, for example by
    ///             calling Waiter::wait_until.
    /// - `events`: The events that will trigger the waiter to wake up.
    /// - `handler`: A handler that will be called on wake-up.
    /// Returns a WaitCanceler that can be used to cancel the wait.
    fn wait_async(
        &self,
        socket: &Socket,
        current_task: &CurrentTask,
        waiter: &Waiter,
        events: FdEvents,
        handler: EventHandler,
    ) -> WaitCanceler;

    /// Return the events that are currently active on the `socket`.
    fn query_events(&self, socket: &Socket, current_task: &CurrentTask) -> FdEvents;

    /// Shuts down this socket according to how, preventing any future reads and/or writes.
    ///
    /// Used by the shutdown syscalls.
    fn shutdown(&self, socket: &Socket, how: SocketShutdownFlags) -> Result<(), Errno>;

    /// Close this socket.
    ///
    /// Called by SocketFile when the file descriptor that is holding this
    /// socket is closed.
    ///
    /// Close differs from shutdown in two ways. First, close will call
    /// mark_peer_closed_with_unread_data if this socket has unread data,
    /// which changes how read() behaves on that socket. Second, close
    /// transitions the internal state of this socket to Closed, which breaks
    /// the reference cycle that exists in the connected state.
    fn close(&self, socket: &Socket);

    /// Returns the name of this socket.
    ///
    /// The name is derived from the address and domain. A socket
    /// will always have a name, even if it is not bound to an address.
    fn getsockname(&self, socket: &Socket) -> Vec<u8>;

    /// Returns the name of the peer of this socket, if such a peer exists.
    ///
    /// Returns an error if the socket is not connected.
    fn getpeername(&self, socket: &Socket) -> Result<Vec<u8>, Errno>;

    /// Sets socket-specific options.
    fn setsockopt(
        &self,
        _socket: &Socket,
        _task: &Task,
        _level: u32,
        _optname: u32,
        _user_opt: UserBuffer,
    ) -> Result<(), Errno> {
        error!(ENOPROTOOPT)
    }

    /// Retrieves socket-specific options.
    fn getsockopt(
        &self,
        _socket: &Socket,
        _level: u32,
        _optname: u32,
        _optlen: u32,
    ) -> Result<Vec<u8>, Errno> {
        error!(ENOPROTOOPT)
    }

    /// Implements ioctl.
    fn ioctl(
        &self,
        _socket: &Socket,
        _current_task: &CurrentTask,
        request: u32,
        _address: UserAddress,
    ) -> Result<SyscallResult, Errno> {
        default_ioctl(request)
    }
}

/// A `Socket` represents one endpoint of a bidirectional communication channel.
pub struct Socket {
    ops: Box<dyn SocketOps>,

    /// The domain of this socket.
    pub domain: SocketDomain,

    /// The type of this socket.
    pub socket_type: SocketType,

    /// The protocol of this socket.
    pub protocol: SocketProtocol,

    state: Mutex<SocketState>,
}

#[derive(Default)]
struct SocketState {
    /// The value of SO_RCVTIMEO.
    receive_timeout: Option<zx::Duration>,

    /// The value for SO_SNDTIMEO.
    send_timeout: Option<zx::Duration>,

    /// The socket's mark. Can get and set with SO_MARK.
    mark: u32,
}

pub type SocketHandle = Arc<Socket>;

#[derive(Clone)]
pub enum SocketPeer {
    Handle(SocketHandle),
    Address(SocketAddress),
}

fn create_socket_ops(
    kernel: &Arc<Kernel>,
    domain: SocketDomain,
    socket_type: SocketType,
    protocol: SocketProtocol,
) -> Result<Box<dyn SocketOps>, Errno> {
    match domain {
        SocketDomain::Unix => Ok(Box::new(UnixSocket::new(socket_type))),
        SocketDomain::Vsock => Ok(Box::new(VsockSocket::new(socket_type))),
        SocketDomain::Inet | SocketDomain::Inet6 => {
            Ok(Box::new(InetSocket::new(domain, socket_type, protocol)?))
        }
        SocketDomain::Netlink => {
            let netlink_family = NetlinkFamily::from_raw(protocol.as_raw());
            new_netlink_socket(kernel, socket_type, netlink_family)
        }
    }
}

impl Socket {
    /// Creates a new unbound socket.
    ///
    /// # Parameters
    /// - `domain`: The domain of the socket (e.g., `AF_UNIX`).
    pub fn new(
        kernel: &Arc<Kernel>,
        domain: SocketDomain,
        socket_type: SocketType,
        protocol: SocketProtocol,
    ) -> Result<SocketHandle, Errno> {
        let ops = create_socket_ops(kernel, domain, socket_type, protocol)?;
        Ok(Arc::new(Socket { ops, domain, socket_type, protocol, state: Mutex::default() }))
    }

    pub fn new_with_ops(
        domain: SocketDomain,
        socket_type: SocketType,
        protocol: SocketProtocol,
        ops: Box<dyn SocketOps>,
    ) -> SocketHandle {
        Arc::new(Socket { ops, domain, socket_type, protocol, state: Mutex::default() })
    }

    /// Creates a `FileHandle` where the associated `FsNode` contains a socket.
    ///
    /// # Parameters
    /// - `kernel`: The kernel that is used to fetch `SocketFs`, to store the created socket node.
    /// - `socket`: The socket to store in the `FsNode`.
    /// - `open_flags`: The `OpenFlags` which are used to create the `FileObject`.
    pub fn new_file(
        current_task: &CurrentTask,
        socket: SocketHandle,
        open_flags: OpenFlags,
    ) -> FileHandle {
        let fs = socket_fs(current_task.kernel());
        let mode = mode!(IFSOCK, 0o777);
        let node = fs.create_node(Anon, FsNodeInfo::new_factory(mode, current_task.as_fscred()));
        node.set_socket(socket.clone());
        FileObject::new_anonymous(SocketFile::new(socket), node, open_flags)
    }

    pub fn downcast_socket<T>(&self) -> Option<&T>
    where
        T: 'static,
    {
        let ops = &*self.ops;
        ops.as_any().downcast_ref::<T>()
    }

    pub fn getsockname(&self) -> Vec<u8> {
        self.ops.getsockname(self)
    }

    pub fn getpeername(&self) -> Result<Vec<u8>, Errno> {
        self.ops.getpeername(self)
    }

    pub fn setsockopt(
        &self,
        task: &Task,
        level: u32,
        optname: u32,
        user_opt: UserBuffer,
    ) -> Result<(), Errno> {
        let read_timeval = || {
            let timeval_ref = user_opt.try_into()?;
            let duration = duration_from_timeval(task.mm.read_object(timeval_ref)?)?;
            Ok(if duration == zx::Duration::default() { None } else { Some(duration) })
        };

        match level {
            SOL_SOCKET => match optname {
                SO_RCVTIMEO => self.state.lock().receive_timeout = read_timeval()?,
                SO_SNDTIMEO => self.state.lock().send_timeout = read_timeval()?,
                SO_MARK => {
                    self.state.lock().mark = task.mm.read_object(user_opt.try_into()?)?;
                }
                _ => self.ops.setsockopt(self, task, level, optname, user_opt)?,
            },
            _ => self.ops.setsockopt(self, task, level, optname, user_opt)?,
        }
        Ok(())
    }

    pub fn getsockopt(&self, level: u32, optname: u32, optlen: u32) -> Result<Vec<u8>, Errno> {
        let value = match level {
            SOL_SOCKET => match optname {
                SO_TYPE => self.socket_type.as_raw().to_ne_bytes().to_vec(),
                SO_DOMAIN => {
                    let domain = self.domain.as_raw() as u32;
                    domain.to_ne_bytes().to_vec()
                }
                SO_PROTOCOL if !self.domain.is_inet() => {
                    self.protocol.as_raw().to_ne_bytes().to_vec()
                }
                SO_RCVTIMEO => {
                    let duration = self.receive_timeout().unwrap_or_default();
                    timeval_from_duration(duration).as_bytes().to_owned()
                }
                SO_SNDTIMEO => {
                    let duration = self.send_timeout().unwrap_or_default();
                    timeval_from_duration(duration).as_bytes().to_owned()
                }
                SO_MARK => self.state.lock().mark.as_bytes().to_owned(),
                _ => self.ops.getsockopt(self, level, optname, optlen)?,
            },
            _ => self.ops.getsockopt(self, level, optname, optlen)?,
        };
        Ok(value)
    }

    pub fn receive_timeout(&self) -> Option<zx::Duration> {
        self.state.lock().receive_timeout
    }

    pub fn send_timeout(&self) -> Option<zx::Duration> {
        self.state.lock().send_timeout
    }

    pub fn ioctl(
        &self,
        current_task: &CurrentTask,
        request: u32,
        address: UserAddress,
    ) -> Result<SyscallResult, Errno> {
        self.ops.ioctl(self, current_task, request, address)
    }

    pub fn bind(
        &self,
        current_task: &CurrentTask,
        socket_address: SocketAddress,
    ) -> Result<(), Errno> {
        self.ops.bind(self, current_task, socket_address)
    }

    pub fn connect(
        self: &SocketHandle,
        current_task: &CurrentTask,
        peer: SocketPeer,
    ) -> Result<(), Errno> {
        self.ops.connect(self, current_task, peer)
    }

    pub fn listen(&self, backlog: i32, credentials: ucred) -> Result<(), Errno> {
        self.ops.listen(self, backlog, credentials)
    }

    pub fn accept(&self) -> Result<SocketHandle, Errno> {
        self.ops.accept(self)
    }

    pub fn read(
        &self,
        current_task: &CurrentTask,
        data: &mut dyn OutputBuffer,
        flags: SocketMessageFlags,
    ) -> Result<MessageReadInfo, Errno> {
        self.ops.read(self, current_task, data, flags)
    }

    pub fn write(
        &self,
        current_task: &CurrentTask,
        data: &mut dyn InputBuffer,
        dest_address: &mut Option<SocketAddress>,
        ancillary_data: &mut Vec<AncillaryData>,
    ) -> Result<usize, Errno> {
        self.ops.write(self, current_task, data, dest_address, ancillary_data)
    }

    pub fn wait_async(
        &self,
        current_task: &CurrentTask,
        waiter: &Waiter,
        events: FdEvents,
        handler: EventHandler,
    ) -> WaitCanceler {
        self.ops.wait_async(self, current_task, waiter, events, handler)
    }

    pub fn query_events(&self, current_task: &CurrentTask) -> FdEvents {
        self.ops.query_events(self, current_task)
    }

    pub fn shutdown(&self, how: SocketShutdownFlags) -> Result<(), Errno> {
        self.ops.shutdown(self, how)
    }

    pub fn close(&self) {
        self.ops.close(self)
    }
}

pub struct AcceptQueue {
    pub sockets: VecDeque<SocketHandle>,
    pub backlog: usize,
}

impl AcceptQueue {
    pub fn new(backlog: usize) -> AcceptQueue {
        AcceptQueue { sockets: VecDeque::with_capacity(backlog), backlog }
    }

    pub fn set_backlog(&mut self, backlog: usize) -> Result<(), Errno> {
        if self.sockets.len() > backlog {
            return error!(EINVAL);
        }
        self.backlog = backlog;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::*;

    #[::fuchsia::test]
    async fn test_dgram_socket() {
        let (kernel, current_task) = create_kernel_and_task();
        let bind_address = SocketAddress::Unix(b"dgram_test".to_vec());
        let rec_dgram = Socket::new(
            &kernel,
            SocketDomain::Unix,
            SocketType::Datagram,
            SocketProtocol::default(),
        )
        .expect("Failed to create socket.");
        let passcred: u32 = 1;
        let opt_size = std::mem::size_of::<u32>();
        let user_address = map_memory(&current_task, UserAddress::default(), opt_size as u64);
        let opt_ref = UserRef::<u32>::new(user_address);
        current_task.mm.write_object(opt_ref, &passcred).unwrap();
        let opt_buf = UserBuffer { address: user_address, length: opt_size };
        rec_dgram.setsockopt(&current_task, SOL_SOCKET, SO_PASSCRED, opt_buf).unwrap();

        rec_dgram.bind(&current_task, bind_address).expect("failed to bind datagram socket");

        let xfer_value: u64 = 1234567819;
        let xfer_bytes = xfer_value.to_ne_bytes();

        let send = Socket::new(
            &kernel,
            SocketDomain::Unix,
            SocketType::Datagram,
            SocketProtocol::default(),
        )
        .expect("Failed to connect socket.");
        send.connect(&current_task, SocketPeer::Handle(rec_dgram.clone())).unwrap();
        let mut source_iter = VecInputBuffer::new(&xfer_bytes);
        send.write(&current_task, &mut source_iter, &mut None, &mut vec![]).unwrap();
        assert_eq!(source_iter.available(), 0);
        // Previously, this would cause the test to fail,
        // because rec_dgram was shut down.
        send.close();

        let mut rec_buffer = VecOutputBuffer::new(8);
        let read_info =
            rec_dgram.read(&current_task, &mut rec_buffer, SocketMessageFlags::empty()).unwrap();
        assert_eq!(read_info.bytes_read, xfer_bytes.len());
        assert_eq!(rec_buffer.data(), xfer_bytes);
        assert_eq!(1, read_info.ancillary_data.len());
        assert_eq!(
            read_info.ancillary_data[0],
            AncillaryData::Unix(UnixControlData::Credentials(ucred {
                pid: current_task.get_pid(),
                uid: 0,
                gid: 0
            }))
        );

        rec_dgram.close();
    }
}
