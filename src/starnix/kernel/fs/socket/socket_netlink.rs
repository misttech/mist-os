// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::*;

use std::{marker::PhantomData, num::NonZeroU32, sync::Arc};

use futures::channel::mpsc::{self, UnboundedReceiver, UnboundedSender};
use netlink::{
    messaging::{Sender, SenderReceiverProvider},
    multicast_groups::{
        InvalidLegacyGroupsError, LegacyGroups, ModernGroup, NoMappingFromModernToLegacyGroupError,
        SingleLegacyGroup,
    },
    protocol_family::route::NetlinkRouteClient,
    NewClientError, NETLINK_LOG_TAG,
};
use netlink_packet_core::{NetlinkMessage, NetlinkSerializable};
use netlink_packet_route::rtnl::RtnlMessage;
use netlink_packet_utils::Emitable as _;
use zerocopy::{AsBytes, FromBytes};

use crate::{
    device::{DeviceListener, DeviceListenerKey},
    fs::{buffers::*, kobject::*, *},
    lock::Mutex,
    logging::{log_error, log_info, log_warn, not_implemented},
    mm::MemoryAccessorExt,
    task::*,
    types::*,
};

// From netlink/socket.go in gVisor.
pub const SOCKET_MIN_SIZE: usize = 4 << 10;
pub const SOCKET_DEFAULT_SIZE: usize = 16 * 1024;
pub const SOCKET_MAX_SIZE: usize = 4 << 20;

pub fn new_netlink_socket(
    kernel: &Arc<Kernel>,
    socket_type: SocketType,
    family: NetlinkFamily,
) -> Result<Box<dyn SocketOps>, Errno> {
    log_info!(tag = NETLINK_LOG_TAG, "Creating {:?} Netlink Socket", family);
    if socket_type != SocketType::Datagram && socket_type != SocketType::Raw {
        return error!(ESOCKTNOSUPPORT);
    }

    let ops: Box<dyn SocketOps> = match family {
        NetlinkFamily::KobjectUevent => Box::new(UEventNetlinkSocket::new(kernel)),
        NetlinkFamily::Route => Box::new(RouteNetlinkSocket::new(kernel)?),
        _ => Box::new(BaseNetlinkSocket::new(family)),
    };
    Ok(ops)
}

#[derive(Default, Debug, Clone, PartialEq, Eq)]
#[repr(C)]
pub struct NetlinkAddress {
    pid: u32,
    groups: u32,
}

impl NetlinkAddress {
    pub fn new(pid: u32, groups: u32) -> Self {
        NetlinkAddress { pid, groups }
    }

    pub fn set_pid_if_zero(&mut self, pid: i32) {
        if self.pid == 0 {
            self.pid = pid as u32;
        }
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        sockaddr_nl { nl_family: AF_NETLINK, nl_pid: self.pid, nl_pad: 0, nl_groups: self.groups }
            .as_bytes()
            .to_vec()
    }
}

#[derive(Debug, Hash, Eq, PartialEq, Clone)]
pub enum NetlinkFamily {
    Unsupported,
    Route,
    Usersock,
    Firewall,
    SockDiag,
    Nflog,
    Xfrm,
    Selinux,
    Iscsi,
    Audit,
    FibLookup,
    Connector,
    Netfilter,
    Ip6Fw,
    Dnrtmsg,
    KobjectUevent,
    Generic,
    Scsitransport,
    Ecryptfs,
    Rdma,
    Crypto,
    Smc,
}

impl NetlinkFamily {
    pub fn from_raw(family: u32) -> Self {
        match family {
            NETLINK_ROUTE => NetlinkFamily::Route,
            NETLINK_USERSOCK => NetlinkFamily::Usersock,
            NETLINK_FIREWALL => NetlinkFamily::Firewall,
            NETLINK_SOCK_DIAG => NetlinkFamily::SockDiag,
            NETLINK_NFLOG => NetlinkFamily::Nflog,
            NETLINK_XFRM => NetlinkFamily::Xfrm,
            NETLINK_SELINUX => NetlinkFamily::Selinux,
            NETLINK_ISCSI => NetlinkFamily::Iscsi,
            NETLINK_AUDIT => NetlinkFamily::Audit,
            NETLINK_FIB_LOOKUP => NetlinkFamily::FibLookup,
            NETLINK_CONNECTOR => NetlinkFamily::Connector,
            NETLINK_NETFILTER => NetlinkFamily::Netfilter,
            NETLINK_IP6_FW => NetlinkFamily::Ip6Fw,
            NETLINK_DNRTMSG => NetlinkFamily::Dnrtmsg,
            NETLINK_KOBJECT_UEVENT => NetlinkFamily::KobjectUevent,
            NETLINK_GENERIC => NetlinkFamily::Generic,
            NETLINK_SCSITRANSPORT => NetlinkFamily::Scsitransport,
            NETLINK_ECRYPTFS => NetlinkFamily::Ecryptfs,
            NETLINK_RDMA => NetlinkFamily::Rdma,
            NETLINK_CRYPTO => NetlinkFamily::Crypto,
            NETLINK_SMC => NetlinkFamily::Smc,
            _ => NetlinkFamily::Unsupported,
        }
    }

    pub fn as_raw(&self) -> u32 {
        match self {
            NetlinkFamily::Route => NETLINK_ROUTE,
            NetlinkFamily::KobjectUevent => NETLINK_KOBJECT_UEVENT,
            _ => 0,
        }
    }
}

#[allow(dead_code)]
struct NetlinkSocketInner {
    /// The specific type of netlink socket.
    family: NetlinkFamily,

    /// The `MessageQueue` that contains messages sent to this socket.
    messages: MessageQueue,

    /// This queue will be notified on reads, writes, disconnects etc.
    waiters: WaitQueue,

    /// The address of this socket.
    address: Option<NetlinkAddress>,

    /// See SO_PASSCRED.
    pub passcred: bool,

    /// See SO_TIMESTAMP.
    pub timestamp: bool,
}

impl NetlinkSocketInner {
    fn bind(
        &mut self,
        current_task: &CurrentTask,
        socket_address: SocketAddress,
    ) -> Result<(), Errno> {
        if self.address.is_some() {
            return error!(EINVAL);
        }

        let netlink_address = match socket_address {
            SocketAddress::Netlink(mut netlink_address) => {
                // TODO: Support distinct IDs for processes with multiple netlink sockets.
                netlink_address.set_pid_if_zero(current_task.get_pid());
                netlink_address
            }
            _ => return error!(EINVAL),
        };

        self.address = Some(netlink_address);
        Ok(())
    }

    fn connect(&mut self, current_task: &CurrentTask, peer: SocketPeer) -> Result<(), Errno> {
        let address = match peer {
            SocketPeer::Address(address) => address,
            _ => return error!(EINVAL),
        };
        // Connect is equivalent to bind, but error are ignored.
        let _ = self.bind(current_task, address);
        Ok(())
    }

    fn set_capacity(&mut self, requested_capacity: usize) {
        let capacity = requested_capacity.clamp(SOCKET_MIN_SIZE, SOCKET_MAX_SIZE);
        let capacity = std::cmp::max(capacity, self.messages.len());
        // We have validated capacity sufficiently that set_capacity should always succeed.
        self.messages.set_capacity(capacity).unwrap();
    }

    fn read_message(&mut self) -> Option<Message> {
        let message = self.messages.read_message();
        if message.is_some() {
            self.waiters.notify_fd_events(FdEvents::POLLOUT);
        }
        message
    }

    fn read_datagram(&mut self, data: &mut dyn OutputBuffer) -> Result<MessageReadInfo, Errno> {
        let mut info = self.messages.read_datagram(data)?;
        if info.message_length == 0 {
            return error!(EAGAIN);
        }

        if self.passcred {
            not_implemented!("TODO(http://fxbug.dev/128863): SCM_CREDENTIALS/SO_PASSCRED not fully implemented, returning unknown credentials for now");
            info.ancillary_data.push(AncillaryData::Unix(UnixControlData::unknown_creds()));
        }

        Ok(info)
    }

    fn write_to_queue(
        &mut self,
        data: &mut dyn InputBuffer,
        address: Option<NetlinkAddress>,
        ancillary_data: &mut Vec<AncillaryData>,
    ) -> Result<usize, Errno> {
        let socket_address = match address {
            Some(addr) => Some(SocketAddress::Netlink(addr)),
            None => self.address.as_ref().map(|addr| SocketAddress::Netlink(addr.clone())),
        };
        let bytes_written = self.messages.write_datagram(data, socket_address, ancillary_data)?;
        if bytes_written > 0 {
            self.waiters.notify_fd_events(FdEvents::POLLIN);
        }
        Ok(bytes_written)
    }

    fn wait_async(
        &mut self,
        waiter: &Waiter,
        events: FdEvents,
        handler: EventHandler,
    ) -> WaitCanceler {
        self.waiters.wait_async_events(waiter, events, handler)
    }

    fn query_events(&self) -> FdEvents {
        self.messages.query_events()
    }

    fn getsockname(&self) -> Vec<u8> {
        match &self.address {
            Some(addr) => addr.to_bytes(),
            _ => vec![],
        }
    }

    fn getpeername(&self) -> Result<Vec<u8>, Errno> {
        match &self.address {
            Some(addr) => Ok(addr.to_bytes()),
            None => {
                let addr = sockaddr_nl { nl_family: AF_NETLINK, ..Default::default() };
                Ok(addr.as_bytes().to_vec())
            }
        }
    }

    fn getsockopt(&self, level: u32, optname: u32) -> Result<Vec<u8>, Errno> {
        let opt_value = match level {
            SOL_SOCKET => match optname {
                SO_PASSCRED => (self.passcred as u32).as_bytes().to_vec(),
                SO_TIMESTAMP => (self.timestamp as u32).as_bytes().to_vec(),
                SO_SNDBUF => (self.messages.capacity() as socklen_t).to_ne_bytes().to_vec(),
                SO_RCVBUF => (self.messages.capacity() as socklen_t).to_ne_bytes().to_vec(),
                SO_SNDBUFFORCE => (self.messages.capacity() as socklen_t).to_ne_bytes().to_vec(),
                SO_RCVBUFFORCE => (self.messages.capacity() as socklen_t).to_ne_bytes().to_vec(),
                SO_PROTOCOL => self.family.as_raw().as_bytes().to_vec(),
                _ => return error!(ENOSYS),
            },
            _ => vec![],
        };

        Ok(opt_value)
    }

    fn setsockopt(
        &mut self,
        task: &Task,
        level: u32,
        optname: u32,
        user_opt: UserBuffer,
    ) -> Result<(), Errno> {
        match level {
            SOL_SOCKET => match optname {
                SO_SNDBUF => {
                    let requested_capacity: socklen_t =
                        task.mm.read_object(user_opt.try_into()?)?;
                    self.set_capacity(requested_capacity as usize * 2);
                }
                SO_RCVBUF => {
                    let requested_capacity: socklen_t =
                        task.mm.read_object(user_opt.try_into()?)?;
                    self.set_capacity(requested_capacity as usize);
                }
                SO_PASSCRED => {
                    let passcred: u32 = task.mm.read_object(user_opt.try_into()?)?;
                    self.passcred = passcred != 0;
                }
                SO_TIMESTAMP => {
                    let timestamp: u32 = task.mm.read_object(user_opt.try_into()?)?;
                    self.timestamp = timestamp != 0;
                }
                SO_SNDBUFFORCE => {
                    if !task.creds().has_capability(CAP_NET_ADMIN) {
                        return error!(EPERM);
                    }
                    let requested_capacity: socklen_t =
                        task.mm.read_object(user_opt.try_into()?)?;
                    self.set_capacity(requested_capacity as usize * 2);
                }
                SO_RCVBUFFORCE => {
                    if !task.creds().has_capability(CAP_NET_ADMIN) {
                        return error!(EPERM);
                    }
                    let requested_capacity: socklen_t =
                        task.mm.read_object(user_opt.try_into()?)?;
                    self.set_capacity(requested_capacity as usize);
                }
                _ => return error!(ENOSYS),
            },
            _ => return error!(ENOSYS),
        }

        Ok(())
    }
}

struct BaseNetlinkSocket {
    inner: Mutex<NetlinkSocketInner>,
}

impl BaseNetlinkSocket {
    pub fn new(family: NetlinkFamily) -> Self {
        BaseNetlinkSocket {
            inner: Mutex::new(NetlinkSocketInner {
                family,
                messages: MessageQueue::new(SOCKET_DEFAULT_SIZE),
                waiters: WaitQueue::default(),
                address: None,
                passcred: false,
                timestamp: false,
            }),
        }
    }

    /// Locks and returns the inner state of the Socket.
    fn lock(&self) -> crate::lock::MutexGuard<'_, NetlinkSocketInner> {
        self.inner.lock()
    }
}

impl SocketOps for BaseNetlinkSocket {
    fn connect(
        &self,
        _socket: &SocketHandle,
        current_task: &CurrentTask,
        peer: SocketPeer,
    ) -> Result<(), Errno> {
        self.lock().connect(current_task, peer)
    }

    fn listen(&self, _socket: &Socket, _backlog: i32, _credentials: ucred) -> Result<(), Errno> {
        error!(EOPNOTSUPP)
    }

    fn accept(&self, _socket: &Socket) -> Result<SocketHandle, Errno> {
        error!(EOPNOTSUPP)
    }

    fn bind(
        &self,
        _socket: &Socket,
        current_task: &CurrentTask,
        socket_address: SocketAddress,
    ) -> Result<(), Errno> {
        self.lock().bind(current_task, socket_address)
    }

    fn read(
        &self,
        _socket: &Socket,
        _current_task: &CurrentTask,
        data: &mut dyn OutputBuffer,
        _flags: SocketMessageFlags,
    ) -> Result<MessageReadInfo, Errno> {
        let msg = self.lock().read_message();
        match msg {
            Some(message) => {
                // Mark the message as complete and return it.
                let mut nl_msg = nlmsghdr::read_from_prefix(message.data.bytes())
                    .ok_or_else(|| errno!(EINVAL))?;
                nl_msg.nlmsg_type = NLMSG_DONE as u16;
                nl_msg.nlmsg_flags &= NLM_F_MULTI as u16;
                let msg_bytes = nl_msg.as_bytes();
                let bytes_read = data.write(msg_bytes)?;

                let info = MessageReadInfo {
                    bytes_read,
                    message_length: msg_bytes.len(),
                    address: Some(SocketAddress::Netlink(NetlinkAddress::default())),
                    ancillary_data: vec![],
                };
                Ok(info)
            }
            None => Ok(MessageReadInfo::default()),
        }
    }

    fn write(
        &self,
        _socket: &Socket,
        _current_task: &CurrentTask,
        data: &mut dyn InputBuffer,
        dest_address: &mut Option<SocketAddress>,
        ancillary_data: &mut Vec<AncillaryData>,
    ) -> Result<usize, Errno> {
        let mut local_address = self.lock().address.clone();

        let destination = match dest_address {
            Some(SocketAddress::Netlink(addr)) => addr,
            _ => match &mut local_address {
                Some(addr) => addr,
                _ => return Ok(data.drain()),
            },
        };

        if destination.groups != 0 {
            not_implemented!("BaseNetlinkSockets multicasting is stubbed");
            return Ok(data.drain());
        }

        self.lock().write_to_queue(data, Some(NetlinkAddress::default()), ancillary_data)
    }

    fn wait_async(
        &self,
        _socket: &Socket,
        _current_task: &CurrentTask,
        waiter: &Waiter,
        events: FdEvents,
        handler: EventHandler,
    ) -> WaitCanceler {
        self.lock().wait_async(waiter, events, handler)
    }

    fn query_events(
        &self,
        _socket: &Socket,
        _current_task: &CurrentTask,
    ) -> Result<FdEvents, Errno> {
        Ok(self.lock().query_events() & FdEvents::POLLIN)
    }

    fn shutdown(&self, _socket: &Socket, _how: SocketShutdownFlags) -> Result<(), Errno> {
        not_implemented!("BaseNetlinkSocket::shutdown is stubbed");
        Ok(())
    }

    fn close(&self, _socket: &Socket) {}

    fn getsockname(&self, _socket: &Socket) -> Vec<u8> {
        self.lock().getsockname()
    }

    fn getpeername(&self, _socket: &Socket) -> Result<Vec<u8>, Errno> {
        self.lock().getpeername()
    }

    fn getsockopt(
        &self,
        _socket: &Socket,
        level: u32,
        optname: u32,
        _optlen: u32,
    ) -> Result<Vec<u8>, Errno> {
        self.lock().getsockopt(level, optname)
    }

    fn setsockopt(
        &self,
        _socket: &Socket,
        task: &Task,
        level: u32,
        optname: u32,
        user_opt: UserBuffer,
    ) -> Result<(), Errno> {
        self.lock().setsockopt(task, level, optname, user_opt)
    }
}

/// Socket implementation for the NETLINK_KOBJECT_UEVENT family of netlink sockets.
struct UEventNetlinkSocket {
    kernel: Arc<Kernel>,
    inner: Arc<Mutex<NetlinkSocketInner>>,
    device_listener_key: Mutex<Option<DeviceListenerKey>>,
}

impl UEventNetlinkSocket {
    #[allow(clippy::let_and_return)]
    pub fn new(kernel: &Arc<Kernel>) -> Self {
        let result = Self {
            kernel: Arc::clone(kernel),
            inner: Arc::new(Mutex::new(NetlinkSocketInner {
                family: NetlinkFamily::KobjectUevent,
                messages: MessageQueue::new(SOCKET_DEFAULT_SIZE),
                waiters: WaitQueue::default(),
                address: None,
                passcred: false,
                timestamp: false,
            })),
            device_listener_key: Default::default(),
        };
        #[cfg(any(test, debug_assertions))]
        {
            let _l1 = result.device_listener_key.lock();
            let _l2 = kernel.device_registry.read();
            let _l3 = result.lock();
        }
        result
    }

    /// Locks and returns the inner state of the Socket.
    fn lock(&self) -> crate::lock::MutexGuard<'_, NetlinkSocketInner> {
        self.inner.lock()
    }

    fn register_listener(&self, state: crate::lock::MutexGuard<'_, NetlinkSocketInner>) {
        if state.address.is_none() {
            return;
        }
        std::mem::drop(state);
        let mut key_state = self.device_listener_key.lock();
        if key_state.is_none() {
            *key_state =
                Some(self.kernel.device_registry.write().register_listener(self.inner.clone()));
        }
    }
}

impl SocketOps for UEventNetlinkSocket {
    fn connect(
        &self,
        _socket: &SocketHandle,
        current_task: &CurrentTask,
        peer: SocketPeer,
    ) -> Result<(), Errno> {
        let mut state = self.lock();
        state.connect(current_task, peer)?;
        self.register_listener(state);
        Ok(())
    }

    fn listen(&self, _socket: &Socket, _backlog: i32, _credentials: ucred) -> Result<(), Errno> {
        error!(EOPNOTSUPP)
    }

    fn accept(&self, _socket: &Socket) -> Result<SocketHandle, Errno> {
        error!(EOPNOTSUPP)
    }

    fn bind(
        &self,
        _socket: &Socket,
        current_task: &CurrentTask,
        socket_address: SocketAddress,
    ) -> Result<(), Errno> {
        let mut state = self.lock();
        state.bind(current_task, socket_address)?;
        self.register_listener(state);
        Ok(())
    }

    fn read(
        &self,
        _socket: &Socket,
        _current_task: &CurrentTask,
        data: &mut dyn OutputBuffer,
        _flags: SocketMessageFlags,
    ) -> Result<MessageReadInfo, Errno> {
        self.lock().read_datagram(data)
    }

    fn write(
        &self,
        _socket: &Socket,
        _current_task: &CurrentTask,
        _data: &mut dyn InputBuffer,
        _dest_address: &mut Option<SocketAddress>,
        _ancillary_data: &mut Vec<AncillaryData>,
    ) -> Result<usize, Errno> {
        error!(EOPNOTSUPP)
    }

    fn wait_async(
        &self,
        _socket: &Socket,
        _current_task: &CurrentTask,
        waiter: &Waiter,
        events: FdEvents,
        handler: EventHandler,
    ) -> WaitCanceler {
        self.lock().wait_async(waiter, events, handler)
    }

    fn query_events(
        &self,
        _socket: &Socket,
        _current_task: &CurrentTask,
    ) -> Result<FdEvents, Errno> {
        Ok(self.lock().query_events() & FdEvents::POLLIN)
    }

    fn shutdown(&self, _socket: &Socket, _how: SocketShutdownFlags) -> Result<(), Errno> {
        not_implemented!("BaseNetlinkSocket::shutdown is stubbed");
        Ok(())
    }

    fn close(&self, _socket: &Socket) {
        let id = self.device_listener_key.lock().take();
        if let Some(id) = id {
            self.kernel.device_registry.write().unregister_listener(&id);
        }
    }

    fn getsockname(&self, _socket: &Socket) -> Vec<u8> {
        self.lock().getsockname()
    }

    fn getpeername(&self, _socket: &Socket) -> Result<Vec<u8>, Errno> {
        self.lock().getpeername()
    }

    fn getsockopt(
        &self,
        _socket: &Socket,
        level: u32,
        optname: u32,
        _optlen: u32,
    ) -> Result<Vec<u8>, Errno> {
        self.lock().getsockopt(level, optname)
    }

    fn setsockopt(
        &self,
        _socket: &Socket,
        task: &Task,
        level: u32,
        optname: u32,
        user_opt: UserBuffer,
    ) -> Result<(), Errno> {
        self.lock().setsockopt(task, level, optname, user_opt)
    }
}

impl DeviceListener for Arc<Mutex<NetlinkSocketInner>> {
    fn on_device_event(
        &self,
        action: UEventAction,
        kobject: KObjectHandle,
        context: UEventContext,
    ) {
        let message = match kobject.ktype() {
            KType::Device { name: device_name, device_type } => {
                let subsystem = kobject.parent().unwrap().name();
                // TODO(fxb/127713): Pass the synthetic UUID when available.
                // Otherwise, default as "0".
                let mut message = format!(
                    "{action}@/devices{path}\0\
                            ACTION={action}\0\
                            DEVPATH=/devices{path}\0\
                            SUBSYSTEM={subsystem}\0\
                            SYNTH_UUID=0\0\
                            MAJOR={major}\0\
                            MINOR={minor}\0\
                            SEQNUM={seqnum}\0",
                    path = String::from_utf8_lossy(&kobject.path()),
                    subsystem = String::from_utf8_lossy(&subsystem),
                    major = device_type.major(),
                    minor = device_type.minor(),
                    seqnum = context.seqnum,
                );
                if device_name.is_some() {
                    message +=
                        &format!("DEVNAME={}\0", String::from_utf8_lossy(&device_name.unwrap()));
                }
                message
            }
            _ => {
                log_error!("This kobject ({:?}) is not a device type.", kobject);
                return;
            }
        };
        let ancillary_data = AncillaryData::Unix(UnixControlData::Credentials(Default::default()));
        let mut ancillary_data = vec![ancillary_data];
        // Ignore write errors
        let _ = self.lock().write_to_queue(
            &mut VecInputBuffer::new(message.as_bytes()),
            Some(NetlinkAddress { pid: 0, groups: 1 }),
            &mut ancillary_data,
        );
    }
}

/// Type for sending messages from [`netlink::Netlink`] to an individual socket.
#[derive(Clone)]
pub struct NetlinkToClientSender<M> {
    _message_type: PhantomData<M>,
    /// The inner socket implementation, which holds a message queue.
    inner: Arc<Mutex<NetlinkSocketInner>>,
}

impl<M> NetlinkToClientSender<M> {
    fn new(inner: Arc<Mutex<NetlinkSocketInner>>) -> Self {
        NetlinkToClientSender { _message_type: PhantomData::<M>, inner }
    }
}

impl<M: Clone + NetlinkSerializable + Send + Sync + 'static> Sender<M>
    for NetlinkToClientSender<M>
{
    fn send(&mut self, message: NetlinkMessage<M>, group: Option<ModernGroup>) {
        // Serialize the message
        let mut buf = vec![0; message.buffer_len()];
        message.emit(&mut buf);
        let mut buf: VecInputBuffer = buf.into();
        // Write the message into the inner socket buffer.
        let NetlinkToClientSender { _message_type: _, inner } = self;
        let _bytes_written: usize = inner
            .lock()
            .write_to_queue(
                &mut buf,
                Some(NetlinkAddress {
                    // All messages come from the "kernel" which has PID of 0.
                    pid: 0,
                    // If this is a multicast message, set the group the multicast
                    // message is from.
                    groups: group
                        .map(SingleLegacyGroup::try_from)
                        .and_then(Result::<_, NoMappingFromModernToLegacyGroupError>::ok)
                        .map_or(0, |g| g.inner()),
                }),
                &mut Vec::new(),
            )
            .unwrap_or_else(|e| {
                log_warn!(
                    tag = NETLINK_LOG_TAG,
                    "Failed to write message into buffer for socket. Errno: {:?}",
                    e
                );
                0
            });
    }
}

/// Provide Starnix implementations of `Sender` and `Receiver to [`Netlink`].
pub(crate) struct NetlinkSenderReceiverProvider;

impl SenderReceiverProvider for NetlinkSenderReceiverProvider {
    type Sender<M: Clone + NetlinkSerializable + Send + Sync + 'static> = NetlinkToClientSender<M>;
    type Receiver<M: Send + 'static> = UnboundedReceiver<NetlinkMessage<M>>;
}

/// Socket implementation for the NETLINK_ROUTE family of netlink sockets.
struct RouteNetlinkSocket {
    /// The inner Netlink socket implementation
    inner: Arc<Mutex<NetlinkSocketInner>>,
    /// The implementation of a client (socket connection) to NETLINK_ROUTE.
    client: NetlinkRouteClient,
    /// The sender of messages from this socket to Netlink.
    // TODO(https://issuetracker.google.com/285880057): Bound the capacity of
    // the "send buffer".
    message_sender: UnboundedSender<NetlinkMessage<RtnlMessage>>,
}

impl RouteNetlinkSocket {
    pub fn new(kernel: &Arc<Kernel>) -> Result<Self, Errno> {
        let inner = Arc::new(Mutex::new(NetlinkSocketInner {
            family: NetlinkFamily::Route,
            messages: MessageQueue::new(SOCKET_DEFAULT_SIZE),
            waiters: WaitQueue::default(),
            address: None,
            passcred: false,
            timestamp: false,
        }));
        let (message_sender, message_receiver) = mpsc::unbounded();
        let client = match kernel
            .network_netlink()
            .new_route_client(NetlinkToClientSender::new(inner.clone()), message_receiver)
        {
            Ok(client) => client,
            Err(NewClientError::Disconnected) => {
                log_error!(
                    tag = NETLINK_LOG_TAG,
                    "Netlink async worker is unexpectedly disconnected"
                );
                return error!(EPIPE);
            }
        };
        Ok(RouteNetlinkSocket { inner, client, message_sender })
    }
}

impl SocketOps for RouteNetlinkSocket {
    fn connect(
        &self,
        _socket: &SocketHandle,
        current_task: &CurrentTask,
        peer: SocketPeer,
    ) -> Result<(), Errno> {
        let RouteNetlinkSocket { inner, client: _, message_sender: _ } = self;
        inner.lock().connect(current_task, peer)
    }

    fn listen(&self, _socket: &Socket, _backlog: i32, _credentials: ucred) -> Result<(), Errno> {
        error!(EOPNOTSUPP)
    }

    fn accept(&self, _socket: &Socket) -> Result<SocketHandle, Errno> {
        error!(EOPNOTSUPP)
    }

    fn bind(
        &self,
        _socket: &Socket,
        current_task: &CurrentTask,
        socket_address: SocketAddress,
    ) -> Result<(), Errno> {
        let RouteNetlinkSocket { inner, client, message_sender: _ } = self;
        let multicast_groups = match socket_address {
            SocketAddress::Unspecified
            | SocketAddress::Inet(_)
            | SocketAddress::Inet6(_)
            | SocketAddress::Unix(_)
            | SocketAddress::Vsock(_) => return error!(EINVAL),
            SocketAddress::Netlink(NetlinkAddress { pid: _, groups }) => groups,
        };
        let pid = {
            let mut inner = inner.lock();
            inner.bind(current_task, socket_address)?;
            inner
                .address
                .as_ref()
                .and_then(|NetlinkAddress { pid, groups: _ }| NonZeroU32::new(*pid))
        };
        if let Some(pid) = pid {
            client.set_pid(pid);
        }
        match client.set_legacy_memberships(LegacyGroups(multicast_groups)) {
            Err(InvalidLegacyGroupsError {}) => error!(EPERM),
            Ok(()) => Ok(()),
        }
    }

    fn read(
        &self,
        _socket: &Socket,
        _current_task: &CurrentTask,
        data: &mut dyn OutputBuffer,
        _flags: SocketMessageFlags,
    ) -> Result<MessageReadInfo, Errno> {
        let RouteNetlinkSocket { inner, client: _, message_sender: _ } = self;
        inner.lock().read_datagram(data)
    }

    fn write(
        &self,
        _socket: &Socket,
        _current_task: &CurrentTask,
        data: &mut dyn InputBuffer,
        _dest_address: &mut Option<SocketAddress>,
        _ancillary_data: &mut Vec<AncillaryData>,
    ) -> Result<usize, Errno> {
        let RouteNetlinkSocket { inner: _, client: _, message_sender } = self;
        let data = data.read_all()?;
        match NetlinkMessage::<RtnlMessage>::deserialize(&data) {
            Err(e) => {
                log_warn!(
                    tag = NETLINK_LOG_TAG,
                    "Failed to process write; data could not be deserialized: {:?}",
                    e
                );
                error!(EINVAL)
            }
            Ok(msg) => match message_sender.unbounded_send(msg) {
                Ok(()) => Ok(data.len()),
                Err(e) => {
                    log_warn!(
                        tag = NETLINK_LOG_TAG,
                        "Netlink receiver unexpectedly disconnected for socket: {:?}",
                        e
                    );
                    error!(EPIPE)
                }
            },
        }
    }

    fn wait_async(
        &self,
        _socket: &Socket,
        _current_task: &CurrentTask,
        waiter: &Waiter,
        events: FdEvents,
        handler: EventHandler,
    ) -> WaitCanceler {
        let RouteNetlinkSocket { inner, client: _, message_sender: _ } = self;
        inner.lock().wait_async(waiter, events, handler)
    }

    fn query_events(
        &self,
        _socket: &Socket,
        _current_task: &CurrentTask,
    ) -> Result<FdEvents, Errno> {
        let RouteNetlinkSocket { inner, client: _, message_sender: _ } = self;
        Ok(inner.lock().query_events() & FdEvents::POLLIN)
    }

    fn shutdown(&self, _socket: &Socket, _how: SocketShutdownFlags) -> Result<(), Errno> {
        error!(EOPNOTSUPP)
    }

    fn close(&self, _socket: &Socket) {
        // TODO(https://issuetracker.google.com/285013342): Support Close.
    }

    fn getsockname(&self, _socket: &Socket) -> Vec<u8> {
        let RouteNetlinkSocket { inner, client: _, message_sender: _ } = self;
        inner.lock().getsockname()
    }

    fn getpeername(&self, _socket: &Socket) -> Result<Vec<u8>, Errno> {
        self.inner.lock().getpeername()
    }

    fn getsockopt(
        &self,
        _socket: &Socket,
        level: u32,
        optname: u32,
        _optlen: u32,
    ) -> Result<Vec<u8>, Errno> {
        self.inner.lock().getsockopt(level, optname)
    }

    fn setsockopt(
        &self,
        _socket: &Socket,
        task: &Task,
        level: u32,
        optname: u32,
        user_opt: UserBuffer,
    ) -> Result<(), Errno> {
        // TODO(https://issuetracker.google.com/283827094): Support add/del
        // multicast group membership.
        self.inner.lock().setsockopt(task, level, optname, user_opt)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use netlink_packet_route::rtnl::RouteMessage;
    use test_case::test_case;

    // Successfully send the message and observe it's stored in the queue.
    #[test_case(true; "sufficient_capacity")]
    // Attempting to send when the queue is full should succeed without changing
    // the contents of the queue.
    #[test_case(false; "insufficient_capacity")]
    fn test_netlink_to_client_sender(sufficient_capacity: bool) {
        const MODERN_GROUP: u32 = 5;

        let mut message: NetlinkMessage<RtnlMessage> =
            RtnlMessage::NewRoute(RouteMessage::default()).into();
        message.finalize();

        let (queue_size, expected_message) = if sufficient_capacity {
            (SOCKET_DEFAULT_SIZE, Some(message.clone()))
        } else {
            (0, None)
        };

        let socket_inner = Arc::new(Mutex::new(NetlinkSocketInner {
            family: NetlinkFamily::Route,
            messages: MessageQueue::new(queue_size),
            waiters: WaitQueue::default(),
            address: None,
            passcred: false,
            timestamp: false,
        }));

        let mut sender = NetlinkToClientSender::<RtnlMessage>::new(socket_inner.clone());
        sender.send(message, Some(ModernGroup(MODERN_GROUP)));
        let message = socket_inner.lock().read_message();

        let data = message.map(|Message { data, address, ancillary_data: _ }| {
            assert_eq!(
                address,
                Some(SocketAddress::Netlink(NetlinkAddress { pid: 0, groups: 1 << MODERN_GROUP }))
            );
            data
        });

        assert_eq!(
            data.map(|data| NetlinkMessage::<RtnlMessage>::deserialize(data.bytes())
                .expect("message should deserialize into RtnlMessage")),
            expected_message
        )
    }
}
