// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::{
    new_netlink_socket, new_socket_file, socket_fs, NetlinkFamily, SocketAddress, SocketDomain,
    SocketFile, SocketMessageFlags, SocketProtocol, SocketType, UnixSocket, VsockSocket,
    ZxioBackedSocket,
};
use crate::mm::MemoryAccessorExt;
use crate::task::{CurrentTask, EventHandler, Task, WaitCanceler, Waiter};
use crate::vfs::buffers::{
    AncillaryData, InputBuffer, MessageReadInfo, OutputBuffer, VecInputBuffer, VecOutputBuffer,
};
use crate::vfs::socket::SocketShutdownFlags;
use crate::vfs::{default_ioctl, Anon, FileHandle, FileObject, FsNodeInfo};
use byteorder::{ByteOrder as _, NativeEndian};

use net_types::ip::IpAddress;
use netlink_packet_core::{ErrorMessage, NetlinkHeader, NetlinkMessage, NetlinkPayload};
use netlink_packet_route::address::{AddressAttribute, AddressMessage};
use netlink_packet_route::link::{LinkAttribute, LinkFlags, LinkMessage};
use netlink_packet_route::{AddressFamily, RouteNetlinkMessage};
use starnix_logging::{log_warn, track_stub};
use starnix_sync::{FileOpsCore, LockBefore, LockEqualOrBefore, Locked, Mutex, Unlocked};
use starnix_syscalls::{SyscallArg, SyscallResult, SUCCESS};
use starnix_types::time::{duration_from_timeval, timeval_from_duration};
use starnix_types::user_buffer::UserBuffer;
use starnix_uapi::as_any::AsAny;
use starnix_uapi::auth::{CAP_NET_ADMIN, CAP_NET_RAW};
use starnix_uapi::errors::{Errno, ErrnoCode};
use starnix_uapi::file_mode::mode;
use starnix_uapi::open_flags::OpenFlags;
use starnix_uapi::union::struct_with_union_into_bytes;
use starnix_uapi::user_address::{UserAddress, UserRef};
use starnix_uapi::vfs::FdEvents;
use starnix_uapi::{
    c_char, errno, error, ifreq, in_addr, sockaddr, sockaddr_in, ucred, AF_INET, SIOCGIFADDR,
    SIOCGIFFLAGS, SIOCGIFHWADDR, SIOCGIFINDEX, SIOCGIFMTU, SIOCGIFNETMASK, SIOCSIFADDR,
    SIOCSIFFLAGS, SIOCSIFNETMASK, SOL_SOCKET, SO_DOMAIN, SO_MARK, SO_PROTOCOL, SO_RCVTIMEO,
    SO_SNDTIMEO, SO_TYPE,
};
use static_assertions::const_assert;
use std::collections::VecDeque;
use std::ffi::CStr;
use std::mem::size_of;
use std::net::IpAddr;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use zerocopy::{FromBytes as _, IntoBytes};

pub const DEFAULT_LISTEN_BACKLOG: usize = 1024;

/// The size of a buffer suitable to carry netlink route messages.
const NETLINK_ROUTE_BUF_SIZE: usize = 1024;

pub trait SocketOps: Send + Sync + AsAny {
    /// Returns the domain, type and protocol of the socket. This is only used for socket that are
    /// build without previous knowledge of this information, and can be ignored if all sockets are
    /// build with it.
    fn get_socket_info(&self) -> Result<(SocketDomain, SocketType, SocketProtocol), Errno> {
        // This should not be used by most socket type that are created with their domain, type and
        // protocol.
        error!(EINVAL)
    }

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
        locked: &mut Locked<'_, FileOpsCore>,
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
        locked: &mut Locked<'_, FileOpsCore>,
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
        locked: &mut Locked<'_, FileOpsCore>,
        socket: &Socket,
        current_task: &CurrentTask,
        waiter: &Waiter,
        events: FdEvents,
        handler: EventHandler,
    ) -> WaitCanceler;

    /// Return the events that are currently active on the `socket`.
    fn query_events(
        &self,
        locked: &mut Locked<'_, FileOpsCore>,
        socket: &Socket,
        current_task: &CurrentTask,
    ) -> Result<FdEvents, Errno>;

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
    fn getsockname(&self, socket: &Socket) -> Result<SocketAddress, Errno>;

    /// Returns the name of the peer of this socket, if such a peer exists.
    ///
    /// Returns an error if the socket is not connected.
    fn getpeername(&self, socket: &Socket) -> Result<SocketAddress, Errno>;

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
        locked: &mut Locked<'_, Unlocked>,
        file: &FileObject,
        current_task: &CurrentTask,
        request: u32,
        arg: SyscallArg,
    ) -> Result<SyscallResult, Errno> {
        default_ioctl(file, locked, current_task, request, arg)
    }

    /// Return a handle that allows access to this file descritor through the zxio protocols.
    ///
    /// If None is returned, the file will be proxied.
    fn to_handle(
        &self,
        _socket: &Socket,
        _current_task: &CurrentTask,
    ) -> Result<Option<zx::Handle>, Errno> {
        Ok(None)
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
    receive_timeout: Option<zx::MonotonicDuration>,

    /// The value for SO_SNDTIMEO.
    send_timeout: Option<zx::MonotonicDuration>,

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
    current_task: &CurrentTask,
    domain: SocketDomain,
    socket_type: SocketType,
    protocol: SocketProtocol,
) -> Result<Box<dyn SocketOps>, Errno> {
    match domain {
        SocketDomain::Unix => Ok(Box::new(UnixSocket::new(socket_type))),
        SocketDomain::Vsock => Ok(Box::new(VsockSocket::new(socket_type))),
        SocketDomain::Inet | SocketDomain::Inet6 => {
            // Follow Linux, and require CAP_NET_RAW to create raw sockets.
            // See https://man7.org/linux/man-pages/man7/raw.7.html.
            if socket_type == SocketType::Raw && !current_task.creds().has_capability(CAP_NET_RAW) {
                error!(EPERM)
            } else {
                Ok(Box::new(ZxioBackedSocket::new(domain, socket_type, protocol)?))
            }
        }
        SocketDomain::Netlink => {
            let netlink_family = NetlinkFamily::from_raw(protocol.as_raw());
            new_netlink_socket(current_task.kernel(), socket_type, netlink_family)
        }
        SocketDomain::Packet => {
            // Follow Linux, and require CAP_NET_RAW to create packet sockets.
            // See https://man7.org/linux/man-pages/man7/packet.7.html.
            if current_task.creds().has_capability(CAP_NET_RAW) {
                Ok(Box::new(ZxioBackedSocket::new(domain, socket_type, protocol)?))
            } else {
                error!(EPERM)
            }
        }
        SocketDomain::Key => {
            track_stub!(
                TODO("https://fxbug.dev/323365389"),
                "Returning a UnixSocket instead of a KeySocket"
            );
            Ok(Box::new(UnixSocket::new(SocketType::Datagram)))
        }
    }
}

impl Socket {
    /// Creates a new unbound socket.
    ///
    /// # Parameters
    /// - `domain`: The domain of the socket (e.g., `AF_UNIX`).
    pub fn new(
        current_task: &CurrentTask,
        domain: SocketDomain,
        socket_type: SocketType,
        protocol: SocketProtocol,
    ) -> Result<SocketHandle, Errno> {
        let ops = create_socket_ops(current_task, domain, socket_type, protocol)?;
        Ok(Self::new_with_ops_and_info(ops, domain, socket_type, protocol))
    }

    pub fn new_with_ops(ops: Box<dyn SocketOps>) -> Result<SocketHandle, Errno> {
        let (domain, socket_type, protocol) = ops.get_socket_info()?;
        Ok(Self::new_with_ops_and_info(ops, domain, socket_type, protocol))
    }

    pub fn new_with_ops_and_info(
        ops: Box<dyn SocketOps>,
        domain: SocketDomain,
        socket_type: SocketType,
        protocol: SocketProtocol,
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
        let node = fs.create_node(
            current_task,
            Anon::default(),
            FsNodeInfo::new_factory(mode, current_task.as_fscred()),
        );
        FileObject::new_anonymous(current_task, SocketFile::new(socket), node, open_flags)
    }

    /// Returns the Socket that this FileHandle refers to. If this file is not a socket file,
    /// returns ENOTSOCK.
    pub fn get_from_file(file: &FileHandle) -> Result<&SocketHandle, Errno> {
        let socket_file = file.downcast_file::<SocketFile>().ok_or_else(|| errno!(ENOTSOCK))?;
        Ok(&socket_file.socket)
    }

    pub fn downcast_socket<T>(&self) -> Option<&T>
    where
        T: 'static,
    {
        let ops = &*self.ops;
        ops.as_any().downcast_ref::<T>()
    }

    pub fn getsockname(&self) -> Result<SocketAddress, Errno> {
        self.ops.getsockname(self)
    }

    pub fn getpeername(&self) -> Result<SocketAddress, Errno> {
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
            let duration = duration_from_timeval(task.read_object(timeval_ref)?)?;
            Ok(if duration == zx::MonotonicDuration::default() { None } else { Some(duration) })
        };

        match level {
            SOL_SOCKET => match optname {
                SO_RCVTIMEO => self.state.lock().receive_timeout = read_timeval()?,
                SO_SNDTIMEO => self.state.lock().send_timeout = read_timeval()?,
                SO_MARK => {
                    self.state.lock().mark = task.read_object(user_opt.try_into()?)?;
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

    pub fn receive_timeout(&self) -> Option<zx::MonotonicDuration> {
        self.state.lock().receive_timeout
    }

    pub fn send_timeout(&self) -> Option<zx::MonotonicDuration> {
        self.state.lock().send_timeout
    }

    pub fn ioctl(
        &self,
        locked: &mut Locked<'_, Unlocked>,
        file: &FileObject,
        current_task: &CurrentTask,
        request: u32,
        arg: SyscallArg,
    ) -> Result<SyscallResult, Errno> {
        let user_addr = UserAddress::from(arg);

        // TODO(https://fxbug.dev/42079507): Share this implementation with `fdio`
        // by moving things to `zxio`.

        // The following netdevice IOCTLs are supported on all sockets for
        // compatibility with Linux.
        //
        // Per https://man7.org/linux/man-pages/man7/netdevice.7.html,
        //
        //     Linux supports some standard ioctls to configure network devices.
        //     They can be used on any socket's file descriptor regardless of
        //     the family or type.
        match request {
            SIOCGIFADDR => {
                let in_ifreq: ifreq = current_task.read_object(UserRef::new(user_addr))?;
                let mut read_buf = VecOutputBuffer::new(NETLINK_ROUTE_BUF_SIZE);
                let (_socket, address_msgs, _if_index) =
                    get_netlink_ipv4_addresses(locked, current_task, &in_ifreq, &mut read_buf)?;
                let mut maybe_errno = None;
                let ifru_addr = {
                    let mut addr = sockaddr::default();
                    let s_addr = address_msgs
                        .into_iter()
                        .next()
                        .and_then(|msg| {
                            msg.attributes.into_iter().find_map(|nla| {
                                if let AddressAttribute::Address(bytes) = nla {
                                    // The bytes are held in network-endian
                                    // order and `in_addr_t` is documented to
                                    // hold values in network order as well. Per
                                    // POSIX specifications for `sockaddr_in`
                                    // https://pubs.opengroup.org/onlinepubs/9699919799/basedefs/netinet_in.h.html.
                                    //
                                    //   The sin_port and sin_addr members shall
                                    //   be in network byte order.
                                    //
                                    // Because of this, we read the bytes in
                                    // native endian which is effectively a
                                    // `core::mem::transmute` to `u32`.
                                    Some(NativeEndian::read_u32(&match bytes {
                                        std::net::IpAddr::V4(v4) => v4.octets(),
                                        std::net::IpAddr::V6(_) => {
                                            maybe_errno =
                                                Some(error!(EINVAL, "expected an ipv4 address"));
                                            return None;
                                        }
                                    }))
                                } else {
                                    None
                                }
                            })
                        })
                        .unwrap_or(0);
                    if let Some(errno) = maybe_errno {
                        return errno;
                    }
                    let _ = sockaddr_in {
                        sin_family: AF_INET,
                        sin_port: 0,
                        sin_addr: in_addr { s_addr },
                        __pad: Default::default(),
                    }
                    .write_to_prefix(addr.as_mut_bytes());
                    addr
                };

                let out_ifreq: [u8; std::mem::size_of::<ifreq>()] = struct_with_union_into_bytes!(ifreq {
                    ifr_ifrn.ifrn_name: unsafe { in_ifreq.ifr_ifrn.ifrn_name },
                    ifr_ifru.ifru_addr: ifru_addr,
                });
                current_task.write_object(UserRef::new(user_addr), &out_ifreq)?;
                Ok(SUCCESS)
            }
            SIOCGIFNETMASK => {
                let in_ifreq: ifreq = current_task.read_object(UserRef::new(user_addr))?;
                let mut read_buf = VecOutputBuffer::new(NETLINK_ROUTE_BUF_SIZE);
                let (_socket, address_msgs, _if_index) =
                    get_netlink_ipv4_addresses(locked, current_task, &in_ifreq, &mut read_buf)?;

                let mut maybe_errno = None;
                let ifru_netmask = {
                    let mut addr = sockaddr::default();
                    let s_addr = address_msgs
                        .into_iter()
                        .next()
                        .and_then(|msg| {
                            let prefix_len = msg.header.prefix_len;
                            if prefix_len > 32 {
                                maybe_errno = Some(error!(EINVAL, "invalid prefix length"));
                                return None;
                            }
                            // Convert prefix length to netmask.
                            let all_ones_address = net_types::ip::Ipv4Addr::new([u8::MAX; 4]);
                            let netmask = all_ones_address.mask(prefix_len);

                            // The bytes of the netmask are already in network byte order.
                            Some(NativeEndian::read_u32(netmask.bytes()))
                        })
                        .unwrap_or(0);

                    if let Some(errno) = maybe_errno {
                        return errno;
                    }

                    let _ = sockaddr_in {
                        sin_family: AF_INET,
                        sin_port: 0,
                        sin_addr: in_addr { s_addr },
                        __pad: Default::default(),
                    }
                    .write_to_prefix(addr.as_mut_bytes());
                    addr
                };

                let out_ifreq: [u8; std::mem::size_of::<ifreq>()] = struct_with_union_into_bytes!(
                ifreq {
                    ifr_ifrn.ifrn_name: unsafe { in_ifreq.ifr_ifrn.ifrn_name },
                    ifr_ifru.ifru_netmask: ifru_netmask,
                });
                current_task.write_object(UserRef::new(user_addr), &out_ifreq)?;
                Ok(SUCCESS)
            }
            SIOCSIFADDR => {
                if !current_task.creds().has_capability(CAP_NET_ADMIN) {
                    return error!(EPERM, "tried to SIOCSIFADDR without CAP_NET_ADMIN");
                }

                let in_ifreq: ifreq = current_task.read_object(UserRef::new(user_addr))?;
                let mut read_buf = VecOutputBuffer::new(NETLINK_ROUTE_BUF_SIZE);
                let (socket, address_msgs, if_index) =
                    get_netlink_ipv4_addresses(locked, current_task, &in_ifreq, &mut read_buf)?;

                let request_header = {
                    let mut header = NetlinkHeader::default();
                    // Always request the ACK response so that we know the
                    // request has been handled before we return from this
                    // operation.
                    header.flags =
                        netlink_packet_core::NLM_F_REQUEST | netlink_packet_core::NLM_F_ACK;
                    header
                };

                // Helper to verify the response of a Netlink request
                let expect_ack = |msg: NetlinkMessage<RouteNetlinkMessage>| {
                    match msg.payload {
                        NetlinkPayload::Error(ErrorMessage {
                            code: Some(code), header: _, ..
                        }) => {
                            // Don't propagate the error up because its not the fault of the
                            // caller - the stack state can change underneath the caller.
                            log_warn!(
                            "got NACK netlink route response when handling ioctl(_, {:#x}, _): {}",
                            request,
                            code
                        );
                        }
                        // `ErrorMessage` with no code represents an ACK.
                        NetlinkPayload::Error(ErrorMessage { code: None, header: _, .. }) => {}
                        payload => panic!("unexpected message = {:?}", payload),
                    }
                };

                // Remove the first IPv4 address for the requested interface, if there is one.
                for addr in address_msgs.into_iter().take(1) {
                    let resp = send_netlink_msg_and_wait_response(
                        locked,
                        current_task,
                        &socket,
                        NetlinkMessage::new(
                            request_header,
                            NetlinkPayload::InnerMessage(RouteNetlinkMessage::DelAddress(addr)),
                        ),
                        &mut read_buf,
                    )?;
                    expect_ack(resp);
                }

                // Next, add the requested address.
                const_assert!(size_of::<sockaddr_in>() <= size_of::<sockaddr>());
                let addr = sockaddr_in::read_from_prefix(
                    unsafe { in_ifreq.ifr_ifru.ifru_addr }.as_bytes(),
                )
                .expect("sockaddr_in is smaller than sockaddr")
                .0
                .sin_addr
                .s_addr;
                if addr != 0 {
                    let resp = send_netlink_msg_and_wait_response(
                        locked,
                        current_task,
                        &socket,
                        NetlinkMessage::new(
                            request_header,
                            NetlinkPayload::InnerMessage(RouteNetlinkMessage::NewAddress({
                                let mut msg = AddressMessage::default();
                                msg.header.family = AddressFamily::Inet;
                                msg.header.index = if_index;

                                // The SIOCSIFADDR ioctl already provides the address to set in
                                // network byte order.
                                let addr = addr.to_ne_bytes();
                                // The request does not include the prefix
                                // length so we use the default prefix for the
                                // address's class.
                                msg.header.prefix_len = net_types::ip::Ipv4Addr::new(addr)
                                    .class()
                                    .default_prefix_len()
                                    .unwrap_or(net_types::ip::Ipv4Addr::BYTES * 8);
                                msg.attributes =
                                    vec![AddressAttribute::Address(IpAddr::V4(addr.into()))];
                                msg
                            })),
                        ),
                        &mut read_buf,
                    )?;
                    expect_ack(resp);
                }

                Ok(SUCCESS)
            }
            SIOCSIFNETMASK => {
                if !current_task.creds().has_capability(CAP_NET_ADMIN) {
                    return error!(EPERM, "tried to SIOCSIFNETMASK without CAP_NET_ADMIN");
                }

                let in_ifreq: ifreq = current_task.read_object(UserRef::new(user_addr))?;
                const_assert!(size_of::<sockaddr_in>() <= size_of::<sockaddr>());
                let addr = sockaddr_in::read_from_prefix(
                    unsafe { in_ifreq.ifr_ifru.ifru_netmask }.as_bytes(),
                )
                .expect("sockaddr_in is smaller than sockaddr")
                .0
                .sin_addr
                .s_addr;
                let prefix_len = addr.count_ones() as u8;
                // Check that the subnet is valid. The netmask is already in network byte order.
                match net_types::ip::Subnet::new(
                    net_types::ip::Ipv4Addr::new(addr.to_ne_bytes()),
                    prefix_len,
                ) {
                    Ok(_) => (),
                    Err(_) => {
                        return error!(EINVAL, "invalid netmask: {addr:?}");
                    }
                }

                let mut read_buf = VecOutputBuffer::new(NETLINK_ROUTE_BUF_SIZE);
                let (socket, address_msgs, _if_index) =
                    get_netlink_ipv4_addresses(locked, current_task, &in_ifreq, &mut read_buf)?;

                let request_header = {
                    let mut header = NetlinkHeader::default();
                    // Always request the ACK response so that we know the
                    // request has been handled before we return from this
                    // operation.
                    header.flags =
                        netlink_packet_core::NLM_F_REQUEST | netlink_packet_core::NLM_F_ACK;
                    header
                };

                // Helper to verify the response of a Netlink request
                let expect_ack = |msg: NetlinkMessage<RouteNetlinkMessage>| {
                    match msg.payload {
                        NetlinkPayload::Error(ErrorMessage {
                            code: Some(code), header: _, ..
                        }) => {
                            // Don't propagate the error up because its not the fault of the
                            // caller - the stack state can change underneath the caller.
                            log_warn!(
                            "got NACK netlink route response when handling ioctl(_, {:#x}, _): {}",
                            request,
                            code
                        );
                        }
                        // `ErrorMessage` with no code represents an ACK.
                        NetlinkPayload::Error(ErrorMessage { code: None, header: _, .. }) => {}
                        payload => panic!("unexpected message = {:?}", payload),
                    }
                };

                // Remove the first IPv4 address on the requested interface.
                let Some(addr) = address_msgs.into_iter().next() else {
                    // There's nothing to do if there are no addresses on the interface.
                    // TODO(https://fxbug.dev/387998791): We should actually return an error here,
                    // but a workaround for us not supporting blackhole devices means that we need
                    // to allow this to succeed as a no-op instead. Once the workaround is removed
                    // this should be an EADDRNOTAVAIL.
                    return Ok(SUCCESS);
                };

                let resp = send_netlink_msg_and_wait_response(
                    locked,
                    current_task,
                    &socket,
                    NetlinkMessage::new(
                        request_header,
                        NetlinkPayload::InnerMessage(RouteNetlinkMessage::DelAddress(addr.clone())),
                    ),
                    &mut read_buf,
                )?;
                expect_ack(resp);

                // Then, re-add it with the new netmask.
                let resp = send_netlink_msg_and_wait_response(
                    locked,
                    current_task,
                    &socket,
                    NetlinkMessage::new(
                        request_header,
                        NetlinkPayload::InnerMessage(RouteNetlinkMessage::NewAddress({
                            let mut msg = addr;
                            msg.header.prefix_len = prefix_len;
                            msg
                        })),
                    ),
                    &mut read_buf,
                )?;
                expect_ack(resp);
                Ok(SUCCESS)
            }
            SIOCGIFHWADDR => {
                let user_addr = UserAddress::from(arg);
                let in_ifreq: ifreq = current_task.read_object(UserRef::new(user_addr))?;
                let mut read_buf = VecOutputBuffer::new(NETLINK_ROUTE_BUF_SIZE);
                let (_socket, link_msg) =
                    get_netlink_interface_info(locked, current_task, &in_ifreq, &mut read_buf)?;

                let hw_addr_and_type = {
                    let hw_type = link_msg.header.link_layer_type;
                    link_msg.attributes.into_iter().find_map(|nla| {
                        if let LinkAttribute::Address(addr) = nla {
                            Some((addr, hw_type))
                        } else {
                            None
                        }
                    })
                };

                let out_ifreq: [u8; std::mem::size_of::<ifreq>()] = struct_with_union_into_bytes!(
                    ifreq {
                        // Safety: The `ifr_ifrn` union only has one field, so it
                        // must be `ifrn_name`.
                        ifr_ifrn.ifrn_name: unsafe { in_ifreq.ifr_ifrn.ifrn_name },
                        ifr_ifru.ifru_hwaddr: hw_addr_and_type.map(|(addr_bytes, sa_family)| {
                            let mut addr = sockaddr {
                                sa_family:sa_family.into(),
                                sa_data: Default::default(),
                            };
                            // We need to manually assign from one to the other
                            // because we may be copying a vector of `u8` into
                            // an array of `i8` and regular `copy_from_slice`
                            // expects both src/dst slices to have the same
                            // element type.
                            //
                            // See /src/starnix/lib/linux_uapi/src/types.rs,
                            // `c_char` is an `i8` on `x86_64` and a `u8` on
                            // `arm64` and `riscv`.
                            addr.sa_data.iter_mut().zip(addr_bytes)
                                .for_each(|(sa_data_byte, link_addr_byte): (&mut c_char, u8)| {
                                    *sa_data_byte = link_addr_byte as c_char;
                                });
                            addr
                        }).unwrap_or_else(Default::default),
                    }
                );
                current_task.write_object(UserRef::new(user_addr), &out_ifreq)?;
                Ok(SUCCESS)
            }
            SIOCGIFINDEX => {
                let in_ifreq: ifreq = current_task.read_object(UserRef::new(user_addr))?;
                let mut read_buf = VecOutputBuffer::new(NETLINK_ROUTE_BUF_SIZE);
                let (_socket, link_msg) =
                    get_netlink_interface_info(locked, current_task, &in_ifreq, &mut read_buf)?;
                let out_ifreq: [u8; std::mem::size_of::<ifreq>()] = struct_with_union_into_bytes!(ifreq {
                    ifr_ifrn.ifrn_name: unsafe { in_ifreq.ifr_ifrn.ifrn_name },
                    ifr_ifru.ifru_ivalue: {
                        let index: u32 = link_msg.header.index;
                        i32::try_from(index).expect("interface ID should fit in an i32")
                    },
                });
                current_task.write_object(UserRef::new(user_addr), &out_ifreq)?;
                Ok(SUCCESS)
            }
            SIOCGIFMTU => {
                track_stub!(TODO("https://fxbug.dev/297369462"), "return actual socket MTU");
                let ifru_mtu = 1280; /* IPv6 MIN MTU */
                let in_ifreq: ifreq = current_task.read_object(UserRef::new(user_addr))?;
                let out_ifreq: [u8; std::mem::size_of::<ifreq>()] = struct_with_union_into_bytes!(ifreq {
                    ifr_ifrn.ifrn_name: unsafe { in_ifreq.ifr_ifrn.ifrn_name },
                    ifr_ifru.ifru_mtu: ifru_mtu,
                });
                current_task.write_object(UserRef::new(user_addr), &out_ifreq)?;
                Ok(SUCCESS)
            }
            SIOCGIFFLAGS => {
                let in_ifreq: ifreq = current_task.read_object(UserRef::new(user_addr))?;
                let mut read_buf = VecOutputBuffer::new(NETLINK_ROUTE_BUF_SIZE);
                let (_socket, link_msg) =
                    get_netlink_interface_info(locked, current_task, &in_ifreq, &mut read_buf)?;
                let out_ifreq: [u8; std::mem::size_of::<ifreq>()] = struct_with_union_into_bytes!(ifreq {
                    ifr_ifrn.ifrn_name: unsafe { in_ifreq.ifr_ifrn.ifrn_name },
                    ifr_ifru.ifru_flags: {
                        // Perform an `as` cast rather than `try_into` because:
                        //   - flags are a bit mask and should not be
                        //     interpreted as negative,
                        //   - SIOCGIFFLAGS returns a subset of the flags
                        //     returned by netlink; the flags lost by truncating
                        //     from 32 to 16 bits is expected.
                        link_msg.header.flags.bits() as i16
                    },
                });
                current_task.write_object(UserRef::new(user_addr), &out_ifreq)?;
                Ok(SUCCESS)
            }
            SIOCSIFFLAGS => {
                let user_addr = UserAddress::from(arg);
                let in_ifreq: ifreq = current_task.read_object(UserRef::new(user_addr))?;
                set_netlink_interface_flags(locked, current_task, &in_ifreq).map(|()| SUCCESS)
            }
            _ => self.ops.ioctl(self, locked, file, current_task, request, arg),
        }
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

    pub fn listen(&self, current_task: &CurrentTask, backlog: i32) -> Result<(), Errno> {
        let max_connections =
            current_task.kernel().system_limits.socket.max_connections.load(Ordering::Relaxed);
        let backlog = std::cmp::min(backlog, max_connections);
        let credentials = current_task.as_ucred();
        self.ops.listen(self, backlog, credentials)
    }

    pub fn accept(&self) -> Result<SocketHandle, Errno> {
        self.ops.accept(self)
    }

    pub fn read<L>(
        &self,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
        data: &mut dyn OutputBuffer,
        flags: SocketMessageFlags,
    ) -> Result<MessageReadInfo, Errno>
    where
        L: LockEqualOrBefore<FileOpsCore>,
    {
        let mut locked = locked.cast_locked::<FileOpsCore>();
        self.ops.read(&mut locked, self, current_task, data, flags)
    }

    pub fn write<L>(
        &self,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
        data: &mut dyn InputBuffer,
        dest_address: &mut Option<SocketAddress>,
        ancillary_data: &mut Vec<AncillaryData>,
    ) -> Result<usize, Errno>
    where
        L: LockEqualOrBefore<FileOpsCore>,
    {
        let mut locked = locked.cast_locked::<FileOpsCore>();
        self.ops.write(&mut locked, self, current_task, data, dest_address, ancillary_data)
    }

    pub fn wait_async<L>(
        &self,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
        waiter: &Waiter,
        events: FdEvents,
        handler: EventHandler,
    ) -> WaitCanceler
    where
        L: LockEqualOrBefore<FileOpsCore>,
    {
        self.ops.wait_async(
            &mut locked.cast_locked::<FileOpsCore>(),
            self,
            current_task,
            waiter,
            events,
            handler,
        )
    }

    pub fn query_events<L>(
        &self,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
    ) -> Result<FdEvents, Errno>
    where
        L: LockEqualOrBefore<FileOpsCore>,
    {
        self.ops.query_events(&mut locked.cast_locked::<FileOpsCore>(), self, current_task)
    }

    pub fn shutdown(&self, how: SocketShutdownFlags) -> Result<(), Errno> {
        self.ops.shutdown(self, how)
    }

    pub fn close(&self) {
        self.ops.close(self)
    }

    pub fn to_handle(
        &self,
        _file: &FileObject,
        current_task: &CurrentTask,
    ) -> Result<Option<zx::Handle>, Errno> {
        self.ops.to_handle(self, current_task)
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

/// Creates a netlink socket and performs an `RTM_GETLINK` request for the
/// requested interface requested in `in_ifreq`.
///
/// Returns the netlink socket and the interface's information, or an [`Errno`]
/// if the operation failed.
fn get_netlink_interface_info<L>(
    locked: &mut Locked<'_, L>,
    current_task: &CurrentTask,
    in_ifreq: &ifreq,
    read_buf: &mut VecOutputBuffer,
) -> Result<(FileHandle, LinkMessage), Errno>
where
    L: LockBefore<FileOpsCore>,
    L: LockBefore<FileOpsCore>,
{
    let iface_name = unsafe { CStr::from_ptr(in_ifreq.ifr_ifrn.ifrn_name.as_ptr()) }
        .to_str()
        .map_err(|std::str::Utf8Error { .. }| errno!(EINVAL))?;
    let socket = new_socket_file(
        current_task,
        SocketDomain::Netlink,
        SocketType::Datagram,
        OpenFlags::RDWR,
        SocketProtocol::from_raw(NetlinkFamily::Route.as_raw()),
    )?;

    // Send the request to get the link details with the requested
    // interface name.
    let msg = NetlinkMessage::new(
        {
            let mut header = NetlinkHeader::default();
            header.flags = netlink_packet_core::NLM_F_REQUEST;
            header
        },
        NetlinkPayload::InnerMessage(RouteNetlinkMessage::GetLink({
            let mut msg = LinkMessage::default();
            msg.attributes = vec![LinkAttribute::IfName(iface_name.to_string())];
            msg
        })),
    );
    let resp = send_netlink_msg_and_wait_response(locked, current_task, &socket, msg, read_buf)?;
    let link_msg = match resp.payload {
        NetlinkPayload::Error(ErrorMessage { code: Some(code), header: _, .. }) => {
            // `code` is an `i32` and may hold negative values so
            // we need to do an `as u64` cast instead of `try_into`.
            // Note that `ErrnoCode::from_return_value` will
            // cast the value to an `i64` to check that it is a
            // valid (negative) errno value.
            let code = ErrnoCode::from_return_value(code.get() as u64);
            return Err(Errno::with_context(code, "error code from RTM_GETLINK"));
        }
        NetlinkPayload::InnerMessage(RouteNetlinkMessage::NewLink(msg)) => msg,
        // netlink is only expected to return an error or
        // RTM_NEWLINK response for our RTM_GETLINK request.
        payload => panic!("unexpected message = {:?}", payload),
    };
    Ok((socket, link_msg))
}

/// Creates a netlink socket and performs an `RTM_GETADDR` dump request for the
/// requested interface requested in `in_ifreq`.
///
/// Returns the netlink socket, the list of addresses and interface index, or an
/// [`Errno`] if the operation failed.
fn get_netlink_ipv4_addresses<L>(
    locked: &mut Locked<'_, L>,
    current_task: &CurrentTask,
    in_ifreq: &ifreq,
    read_buf: &mut VecOutputBuffer,
) -> Result<(FileHandle, Vec<AddressMessage>, u32), Errno>
where
    L: LockBefore<FileOpsCore>,
    L: LockBefore<FileOpsCore>,
{
    let sockaddr { sa_family, sa_data: _ } = unsafe { in_ifreq.ifr_ifru.ifru_addr };
    if sa_family != AF_INET {
        return error!(EINVAL);
    }

    let (socket, link_msg) = get_netlink_interface_info(locked, current_task, in_ifreq, read_buf)?;
    let if_index = link_msg.header.index;

    // Send the request to dump all IPv4 addresses.
    {
        let mut msg = NetlinkMessage::new(
            {
                let mut header = NetlinkHeader::default();
                header.flags = netlink_packet_core::NLM_F_DUMP | netlink_packet_core::NLM_F_REQUEST;
                header
            },
            NetlinkPayload::InnerMessage(RouteNetlinkMessage::GetAddress({
                let mut msg = AddressMessage::default();
                msg.header.family = AddressFamily::Inet;
                msg
            })),
        );
        msg.finalize();
        let mut buf = vec![0; msg.buffer_len()];
        msg.serialize(&mut buf[..]);
        assert_eq!(
            socket.write(locked, current_task, &mut VecInputBuffer::from(buf))?,
            msg.buffer_len()
        );
    }

    // Collect all the addresses.
    let mut addrs = Vec::new();
    loop {
        read_buf.reset();
        let n = socket.read(locked, current_task, read_buf)?;

        let msg = NetlinkMessage::<RouteNetlinkMessage>::deserialize(&read_buf.data()[..n])
            .expect("netlink should always send well-formed messages");
        match msg.payload {
            NetlinkPayload::Done(_) => break,
            NetlinkPayload::InnerMessage(RouteNetlinkMessage::NewAddress(msg)) => {
                if msg.header.index == if_index {
                    addrs.push(msg);
                }
            }
            payload => panic!("unexpected message = {:?}", payload),
        }
    }

    Ok((socket, addrs, if_index))
}

/// Creates a netlink socket and performs `RTM_SETLINK` to update the flags.
fn set_netlink_interface_flags<L>(
    locked: &mut Locked<'_, L>,
    current_task: &CurrentTask,
    in_ifreq: &ifreq,
) -> Result<(), Errno>
where
    L: LockBefore<FileOpsCore>,
    L: LockBefore<FileOpsCore>,
{
    let iface_name = unsafe { CStr::from_ptr(in_ifreq.ifr_ifrn.ifrn_name.as_ptr()) }
        .to_str()
        .map_err(|std::str::Utf8Error { .. }| errno!(EINVAL))?;
    let flags: i16 = unsafe { in_ifreq.ifr_ifru.ifru_flags };
    // Perform an `as` cast rather than `try_into` because:
    //   - flags are a bit mask and should not be interpreted as negative,
    //   - no loss in precision when upcasting 16 bits to 32 bits.
    let flags: u32 = flags as u32;

    let socket = new_socket_file(
        current_task,
        SocketDomain::Netlink,
        SocketType::Datagram,
        OpenFlags::RDWR,
        SocketProtocol::from_raw(NetlinkFamily::Route.as_raw()),
    )?;

    // Send the request to set the link flags with the requested interface name.
    let msg = NetlinkMessage::new(
        {
            let mut header = NetlinkHeader::default();
            header.flags = netlink_packet_core::NLM_F_REQUEST | netlink_packet_core::NLM_F_ACK;
            header
        },
        NetlinkPayload::InnerMessage(RouteNetlinkMessage::SetLink({
            let mut msg = LinkMessage::default();
            msg.header.flags = LinkFlags::from_bits(flags).unwrap();
            // Only attempt to change flags in the first 16 bits, because
            // `ifreq` represents flags as a short (i16).
            msg.header.change_mask = LinkFlags::from_bits(u16::MAX as u32).unwrap();
            msg.attributes = vec![LinkAttribute::IfName(iface_name.to_string())];
            msg
        })),
    );
    let mut read_buf = VecOutputBuffer::new(NETLINK_ROUTE_BUF_SIZE);
    let resp =
        send_netlink_msg_and_wait_response(locked, current_task, &socket, msg, &mut read_buf)?;
    match resp.payload {
        NetlinkPayload::Error(ErrorMessage { code: Some(code), header: _, .. }) => {
            // `code` is an `i32` and may hold negative values so
            // we need to do an `as u64` cast instead of `try_into`.
            // Note that `ErrnoCode::from_return_value` will
            // cast the value to an `i64` to check that it is a
            // valid (negative) errno value.
            let code = ErrnoCode::from_return_value(code.get() as u64);
            Err(Errno::with_context(code, "error code from RTM_SETLINK"))
        }
        // `ErrorMessage` with no code represents an ACK.
        NetlinkPayload::Error(ErrorMessage { code: None, header: _, .. }) => Ok(()),
        // Netlink is only expected to return an error or an ack.
        payload => panic!("unexpected message = {:?}", payload),
    }
}

/// Sends the msg on the provided NETLINK ROUTE socket, returning the response.
fn send_netlink_msg_and_wait_response<L>(
    locked: &mut Locked<'_, L>,
    current_task: &CurrentTask,
    socket: &FileHandle,
    mut msg: NetlinkMessage<RouteNetlinkMessage>,
    read_buf: &mut VecOutputBuffer,
) -> Result<NetlinkMessage<RouteNetlinkMessage>, Errno>
where
    L: LockBefore<FileOpsCore>,
    L: LockBefore<FileOpsCore>,
{
    msg.finalize();
    let mut buf = vec![0; msg.buffer_len()];
    msg.serialize(&mut buf[..]);
    assert_eq!(
        socket.write(locked, current_task, &mut VecInputBuffer::from(buf))?,
        msg.buffer_len()
    );

    read_buf.reset();
    let n = socket.read(locked, current_task, read_buf)?;
    let msg = NetlinkMessage::<RouteNetlinkMessage>::deserialize(&read_buf.data()[..n])
        .expect("netlink should always send well-formed messages");
    Ok(msg)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{create_kernel_task_and_unlocked, map_memory};
    use crate::vfs::UnixControlData;
    use starnix_uapi::SO_PASSCRED;

    #[::fuchsia::test]
    #[ignore]
    async fn test_dgram_socket() {
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let bind_address = SocketAddress::Unix(b"dgram_test".into());
        let rec_dgram = Socket::new(
            &current_task,
            SocketDomain::Unix,
            SocketType::Datagram,
            SocketProtocol::default(),
        )
        .expect("Failed to create socket.");
        let passcred: u32 = 1;
        let opt_size = std::mem::size_of::<u32>();
        let user_address =
            map_memory(&mut locked, &current_task, UserAddress::default(), opt_size as u64);
        let opt_ref = UserRef::<u32>::new(user_address);
        current_task.write_object(opt_ref, &passcred).unwrap();
        let opt_buf = UserBuffer { address: user_address, length: opt_size };
        rec_dgram.setsockopt(&current_task, SOL_SOCKET, SO_PASSCRED, opt_buf).unwrap();

        rec_dgram.bind(&current_task, bind_address).expect("failed to bind datagram socket");

        let xfer_value: u64 = 1234567819;
        let xfer_bytes = xfer_value.to_ne_bytes();

        let send = Socket::new(
            &current_task,
            SocketDomain::Unix,
            SocketType::Datagram,
            SocketProtocol::default(),
        )
        .expect("Failed to connect socket.");
        send.connect(&current_task, SocketPeer::Handle(rec_dgram.clone())).unwrap();
        let mut source_iter = VecInputBuffer::new(&xfer_bytes);
        send.write(&mut locked, &current_task, &mut source_iter, &mut None, &mut vec![]).unwrap();
        assert_eq!(source_iter.available(), 0);
        // Previously, this would cause the test to fail,
        // because rec_dgram was shut down.
        send.close();

        let mut rec_buffer = VecOutputBuffer::new(8);
        let read_info = rec_dgram
            .read(&mut locked, &current_task, &mut rec_buffer, SocketMessageFlags::empty())
            .unwrap();
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
