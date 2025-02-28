// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::fmt::Debug;
use std::sync::Arc;

use byteorder::{ByteOrder, NativeEndian};
use zerocopy::{FromBytes, IntoBytes};

use crate::mm::VmsplicePayload;
use crate::security;
use crate::task::CurrentTask;
use crate::vfs::buffers::{InputBuffer, InputBufferExt as _, OutputBuffer};
use crate::vfs::socket::{SocketAddress, SocketMessageFlags};
use crate::vfs::{FdFlags, FdNumber, FileHandle, FsString};
use starnix_uapi::errors::Errno;
use starnix_uapi::{
    c_int, cmsghdr, errno, error, in6_addr, in6_addr__bindgen_ty_1, in6_pktinfo, sockaddr_in,
    timespec, timeval, ucred, IPV6_HOPLIMIT, IPV6_PKTINFO, IPV6_TCLASS, IP_RECVORIGDSTADDR, IP_TOS,
    IP_TTL, SCM_CREDENTIALS, SCM_RIGHTS, SCM_SECURITY, SOL_IP, SOL_IPV6, SOL_SOCKET, SO_TIMESTAMP,
    SO_TIMESTAMPNS,
};

/// A `Message` represents a typed segment of bytes within a `MessageQueue`.
#[derive(Clone, Debug)]
#[cfg_attr(test, derive(PartialEq))]
pub struct Message<D: MessageData = Vec<u8>> {
    /// The data contained in the message.
    pub data: D,

    /// The address from which the message was sent.
    pub address: Option<SocketAddress>,

    /// The ancillary data that is associated with this message.
    pub ancillary_data: Vec<AncillaryData>,
}

impl<D: MessageData> Message<D> {
    /// Creates a a new message with the provided message and ancillary data.
    pub fn new(
        data: D,
        address: Option<SocketAddress>,
        ancillary_data: Vec<AncillaryData>,
    ) -> Self {
        Message { data, address, ancillary_data }
    }

    /// Returns the length of the message in bytes.
    ///
    /// Note that ancillary data does not contribute to the length of the message.
    pub fn len(&self) -> usize {
        self.data.len()
    }

    pub fn clone_at_most(&self, limit: usize) -> Self {
        Message {
            data: self.data.clone_at_most(limit),
            address: self.address.clone(),
            ancillary_data: self.ancillary_data.clone(),
        }
    }

    /// Shortens the message data, keeping the first `limit` elements and dropping the rest.
    ///
    /// If `limit` is greater or equal to the current data length, this has no effect.
    pub fn truncate(&mut self, limit: usize) {
        self.data.truncate(limit);
    }
}

impl<D: MessageData> From<D> for Message<D> {
    fn from(data: D) -> Self {
        Message { data, address: None, ancillary_data: Vec::new() }
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ControlMsg {
    pub header: cmsghdr,
    pub data: Vec<u8>,
}

impl ControlMsg {
    pub fn new(cmsg_level: u32, cmsg_type: u32, data: Vec<u8>) -> ControlMsg {
        let cmsg_len = std::mem::size_of::<cmsghdr>() + data.len();
        let header = cmsghdr { cmsg_len, cmsg_level, cmsg_type };
        ControlMsg { header, data }
    }
}

/// `AncillaryData` converts a `cmsghdr` into a representation suitable for passing around
/// inside of starnix. In AF_UNIX/SCM_RIGHTS, for example, the file descrpitors will be turned
/// into `FileHandle`s that can be sent to other tasks.
///
/// An `AncillaryData` instance can be converted back into a `cmsghdr`. At that point the contained
/// objects will be converted back to what can be stored in a `cmsghdr`. File handles, for example,
/// will be added to the reading task's files and the associated file descriptors will be stored in
///  the `cmsghdr`.
#[derive(Clone, Debug)]
#[cfg_attr(test, derive(PartialEq))]
pub enum AncillaryData {
    Unix(UnixControlData),

    // SOL_IP and SOL_IPV6.
    Ip(syncio::ControlMessage),
}

// Reads int `cmsg` value and tries to convert it to u8.
fn read_u8_value_from_int_cmsg(data: &[u8]) -> Option<u8> {
    u8::try_from(c_int::read_from_prefix(data).ok()?.0).ok()
}

impl AncillaryData {
    /// Creates a new `AncillaryData` instance representing the data in `message`.
    ///
    /// # Parameters
    /// - `current_task`: The current task. Used to interpret SCM_RIGHTS messages.
    /// - `message`: The message header to parse.
    pub fn from_cmsg(current_task: &CurrentTask, message: ControlMsg) -> Result<Self, Errno> {
        match (message.header.cmsg_level, message.header.cmsg_type) {
            (SOL_SOCKET, SCM_RIGHTS | SCM_CREDENTIALS) => {
                Ok(AncillaryData::Unix(UnixControlData::new(current_task, message)?))
            }
            (SOL_IP, IP_TOS) => Ok(AncillaryData::Ip(syncio::ControlMessage::IpTos(
                u8::read_from_prefix(&message.data[..]).map_err(|_| errno!(EINVAL))?.0,
            ))),
            (SOL_IP, IP_TTL) => Ok(AncillaryData::Ip(syncio::ControlMessage::IpTtl(
                read_u8_value_from_int_cmsg(&message.data).ok_or_else(|| errno!(EINVAL))?,
            ))),
            (SOL_IP, IP_RECVORIGDSTADDR) => {
                Ok(AncillaryData::Ip(syncio::ControlMessage::IpRecvOrigDstAddr(
                    <[u8; size_of::<sockaddr_in>()]>::read_from_prefix(&message.data[..])
                        .map_err(|_| errno!(EINVAL))?
                        .0,
                )))
            }
            (SOL_IPV6, IPV6_TCLASS) => Ok(AncillaryData::Ip(syncio::ControlMessage::Ipv6Tclass(
                read_u8_value_from_int_cmsg(&message.data).ok_or_else(|| errno!(EINVAL))?,
            ))),
            (SOL_IPV6, IPV6_HOPLIMIT) => {
                Ok(AncillaryData::Ip(syncio::ControlMessage::Ipv6HopLimit(
                    read_u8_value_from_int_cmsg(&message.data).ok_or_else(|| errno!(EINVAL))?,
                )))
            }
            (SOL_IPV6, IPV6_PKTINFO) => {
                let (pktinfo, _) =
                    in6_pktinfo::read_from_prefix(&message.data[..]).map_err(|_| errno!(EINVAL))?;
                Ok(AncillaryData::Ip(syncio::ControlMessage::Ipv6PacketInfo {
                    local_addr: pktinfo
                        .ipi6_addr
                        .as_bytes()
                        .try_into()
                        .ok()
                        .ok_or_else(|| errno!(EINVAL))?,
                    iface: pktinfo.ipi6_ifindex as u32,
                }))
            }
            (level, type_) => {
                error!(EINVAL, format!("invalid cmsg_level/type: 0x{:x}/0x{:x}", level, type_))
            }
        }
    }

    /// Returns a `ControlMsg` representation of this `AncillaryData`. This includes
    /// creating any objects (e.g., file descriptors) in `task`.
    pub fn into_controlmsg(
        self,
        current_task: &CurrentTask,
        flags: SocketMessageFlags,
    ) -> Result<Option<ControlMsg>, Errno> {
        match self {
            AncillaryData::Unix(control) => control.into_controlmsg(current_task, flags),
            AncillaryData::Ip(syncio::ControlMessage::IpTos(value)) => {
                Ok(Some(ControlMsg::new(SOL_IP, IP_TOS, value.as_bytes().to_vec())))
            }
            AncillaryData::Ip(syncio::ControlMessage::IpTtl(value)) => {
                Ok(Some(ControlMsg::new(SOL_IP, IP_TTL, (value as c_int).as_bytes().to_vec())))
            }
            AncillaryData::Ip(syncio::ControlMessage::IpRecvOrigDstAddr(value)) => {
                Ok(Some(ControlMsg::new(SOL_IP, IP_RECVORIGDSTADDR, value.as_bytes().to_vec())))
            }
            AncillaryData::Ip(syncio::ControlMessage::Ipv6Tclass(value)) => Ok(Some(
                ControlMsg::new(SOL_IPV6, IPV6_TCLASS, (value as c_int).as_bytes().to_vec()),
            )),
            AncillaryData::Ip(syncio::ControlMessage::Ipv6HopLimit(value)) => Ok(Some(
                ControlMsg::new(SOL_IPV6, IPV6_HOPLIMIT, (value as c_int).as_bytes().to_vec()),
            )),
            AncillaryData::Ip(syncio::ControlMessage::Ipv6PacketInfo { iface, local_addr }) => {
                let pktinfo = in6_pktinfo {
                    ipi6_addr: in6_addr { in6_u: in6_addr__bindgen_ty_1 { u6_addr8: local_addr } },
                    ipi6_ifindex: iface as i32,
                };
                Ok(Some(ControlMsg::new(SOL_IPV6, IPV6_PKTINFO, pktinfo.as_bytes().to_vec())))
            }
            AncillaryData::Ip(syncio::ControlMessage::Timestamp { sec, usec }) => {
                let time = timeval { tv_sec: sec, tv_usec: usec };
                Ok(Some(ControlMsg::new(SOL_SOCKET, SO_TIMESTAMP, time.as_bytes().to_vec())))
            }
            AncillaryData::Ip(syncio::ControlMessage::TimestampNs { sec, nsec }) => {
                let time = timespec { tv_sec: sec, tv_nsec: nsec };
                Ok(Some(ControlMsg::new(SOL_SOCKET, SO_TIMESTAMPNS, time.as_bytes().to_vec())))
            }
        }
    }

    /// Returns the total size of all data in this message.
    pub fn total_size(&self) -> usize {
        match self {
            AncillaryData::Unix(control) => control.total_size(),
            AncillaryData::Ip(msg) => msg.get_data_size(),
        }
    }

    /// Returns the minimum size that can fit some amount of this message's data.
    pub fn minimum_size(&self) -> usize {
        match self {
            AncillaryData::Unix(control) => control.minimum_size(),
            AncillaryData::Ip(msg) => msg.get_data_size(),
        }
    }

    /// Convert the message into bytes, truncating it if it exceeds the available space.
    pub fn into_bytes(
        self,
        current_task: &CurrentTask,
        flags: SocketMessageFlags,
        space_available: usize,
    ) -> Result<Vec<u8>, Errno> {
        let header_size = std::mem::size_of::<cmsghdr>();
        let minimum_data_size = self.minimum_size();

        if space_available < header_size + minimum_data_size {
            // If there is not enough space available to fit the header, return an empty vector
            // instead of a partial header.
            return Ok(vec![]);
        }

        let cmsg = self.into_controlmsg(current_task, flags)?;
        if let Some(mut cmsg) = cmsg {
            let cmsg_len = std::cmp::min(header_size + cmsg.data.len(), space_available);
            cmsg.header.cmsg_len = cmsg_len;
            let mut bytes = cmsg.header.as_bytes().to_owned();
            bytes.extend_from_slice(&cmsg.data[..cmsg_len - header_size]);
            return Ok(bytes);
        } else {
            return Ok(vec![]);
        }
    }
}

/// A control message for a Unix domain socket.
#[derive(Clone, Debug)]
pub enum UnixControlData {
    /// "Send or receive a set of open file descriptors from another process. The data portion
    /// contains an integer array of the file descriptors."
    ///
    /// See https://man7.org/linux/man-pages/man7/unix.7.html.
    Rights(Vec<FileHandle>),

    /// "Send or receive UNIX credentials.  This can be used for authentication. The credentials are
    /// passed as a struct ucred ancillary message."
    ///
    /// See https://man7.org/linux/man-pages/man7/unix.7.html.
    Credentials(ucred),

    /// "Receive the SELinux security context (the security label) of the peer socket. The received
    /// ancillary data is a null-terminated string containing the security context."
    ///
    /// See https://man7.org/linux/man-pages/man7/unix.7.html.
    Security(FsString),
}

/// `UnixControlData` cannot derive `PartialEq` due to `Rights` containing file handles.
///
/// This implementation only compares the number of files, not the actual files. The equality
/// should only be used for testing.
#[cfg(test)]
impl PartialEq for UnixControlData {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (UnixControlData::Rights(self_files), UnixControlData::Rights(other_files)) => {
                self_files.len() == other_files.len()
            }
            (
                UnixControlData::Credentials(self_credentials),
                UnixControlData::Credentials(other_credentials),
            ) => self_credentials == other_credentials,
            (
                UnixControlData::Security(self_security),
                UnixControlData::Security(other_security),
            ) => self_security == other_security,
            _ => false,
        }
    }
}

impl UnixControlData {
    /// Creates a new `UnixControlData` instance for the provided `message_header`. This includes
    /// reading the associated data from the `task` (e.g., files from file descriptors).
    pub fn new(current_task: &CurrentTask, message: ControlMsg) -> Result<Self, Errno> {
        match message.header.cmsg_type {
            SCM_RIGHTS => {
                // Compute the number of file descriptors that fit in the provided bytes.
                let bytes_per_file_descriptor = std::mem::size_of::<FdNumber>();
                let num_file_descriptors = message.data.len() / bytes_per_file_descriptor;

                // Get the files associated with the provided file descriptors.
                let files = (0..num_file_descriptors * bytes_per_file_descriptor)
                    .step_by(bytes_per_file_descriptor)
                    .map(|index| NativeEndian::read_i32(&message.data[index..]))
                    // O_PATH allowed for:
                    //
                    //   Passing the file descriptor to another process via a
                    //   UNIX domain socket (see SCM_RIGHTS in unix(7)).
                    //
                    // See https://man7.org/linux/man-pages/man2/open.2.html
                    .map(|fd| current_task.files.get_allowing_opath(FdNumber::from_raw(fd)))
                    .collect::<Result<Vec<FileHandle>, Errno>>()?;

                Ok(UnixControlData::Rights(files))
            }
            SCM_CREDENTIALS => {
                if message.data.len() < std::mem::size_of::<ucred>() {
                    return error!(EINVAL);
                }

                let credentials =
                    ucred::read_from_bytes(&message.data[..std::mem::size_of::<ucred>()])
                        .map_err(|_| errno!(EINVAL))?;
                Ok(UnixControlData::Credentials(credentials))
            }
            SCM_SECURITY => Ok(UnixControlData::Security(message.data.into())),
            _ => error!(EINVAL),
        }
    }

    /// Returns a `UnixControlData` message that can be used when passcred is enabled but no
    /// credentials were sent.
    pub fn unknown_creds() -> Self {
        const NOBODY: u32 = 65534;
        let credentials = ucred { pid: 0, uid: NOBODY, gid: NOBODY };
        UnixControlData::Credentials(credentials)
    }

    /// Constructs a ControlMsg for this control data, with a destination of `task`.
    ///
    /// The provided `task` is used to create any required file descriptors, etc.
    pub fn into_controlmsg(
        self,
        current_task: &CurrentTask,
        flags: SocketMessageFlags,
    ) -> Result<Option<ControlMsg>, Errno> {
        let (msg_type, data) = match self {
            UnixControlData::Rights(files) => {
                let flags = if flags.contains(SocketMessageFlags::CMSG_CLOEXEC) {
                    FdFlags::CLOEXEC
                } else {
                    FdFlags::empty()
                };
                let mut fds = Vec::with_capacity(files.len());
                for file in files {
                    // Truncate the message as soon as a file descriptor is not allowed to be
                    // received.
                    if security::file_receive(current_task, &file).is_err() {
                        break;
                    }
                    let fd = current_task.add_file(file, flags)?;
                    fds.push(fd);
                }
                // If no file descriptors are left, the entirety of the control message should be
                // dropped.
                if fds.is_empty() {
                    return Ok(None);
                }
                (SCM_RIGHTS, fds.as_bytes().to_owned())
            }
            UnixControlData::Credentials(credentials) => {
                (SCM_CREDENTIALS, credentials.as_bytes().to_owned())
            }
            UnixControlData::Security(string) => (SCM_SECURITY, string.as_bytes().to_owned()),
        };
        Ok(Some(ControlMsg::new(SOL_SOCKET, msg_type, data)))
    }

    /// Returns the total size of all data in this message.
    pub fn total_size(&self) -> usize {
        match self {
            UnixControlData::Rights(files) => files.len() * std::mem::size_of::<FdNumber>(),
            UnixControlData::Credentials(_credentials) => std::mem::size_of::<ucred>(),
            UnixControlData::Security(string) => string.len(),
        }
    }

    /// Returns the minimum size that can fit some amount of this message's data. For example, the
    /// minimum size for an SCM_RIGHTS message is the size of a single FD. If the buffer is large
    /// enough for the minimum size but too small for the total size, the message is truncated and
    /// the MSG_CTRUNC flag is set.
    pub fn minimum_size(&self) -> usize {
        match self {
            UnixControlData::Rights(_files) => std::mem::size_of::<FdNumber>(),
            UnixControlData::Credentials(_credentials) => 0,
            UnixControlData::Security(string) => string.len(),
        }
    }
}

/// Stores an arbitrary sequence of bytes.
pub trait MessageData: Sized {
    /// Copies data from user memory into a new MessageData object.
    fn copy_from_user(data: &mut dyn InputBuffer, limit: usize) -> Result<Self, Errno>;

    /// Returns a pointer to this data.
    fn ptr(&self) -> Result<*const u8, Errno>;

    /// Calls the provided function with this data's bytes.
    fn with_bytes<O, F: FnMut(&[u8]) -> Result<O, Errno>>(&self, f: F) -> Result<O, Errno>;

    /// Returns the number of bytes in the message.
    fn len(&self) -> usize;

    /// Splits the message data at `index`.
    ///
    /// After this call returns, at most `at` bytes will be stored in this `MessageData`, and any
    /// remaining bytes will be moved to the returned `MessageData`.
    fn split_off(&mut self, index: usize) -> Option<Self>;

    /// Copies the message out to the output buffer.
    ///
    /// Returns the number of bytes that were read into the buffer.
    fn copy_to_user(&self, data: &mut dyn OutputBuffer) -> Result<usize, Errno>;

    /// Clones this data up to a limit.
    fn clone_at_most(&self, limit: usize) -> Self;

    /// Truncates this data to the specified limit.
    ///
    /// Does nothing if the limit is larger than the data's size.
    fn truncate(&mut self, limit: usize);
}

impl MessageData for Vec<u8> {
    fn copy_from_user(data: &mut dyn InputBuffer, limit: usize) -> Result<Self, Errno> {
        data.read_to_vec_exact(limit)
    }

    fn ptr(&self) -> Result<*const u8, Errno> {
        Ok(self.as_ptr())
    }

    fn with_bytes<O, F: FnMut(&[u8]) -> Result<O, Errno>>(&self, mut f: F) -> Result<O, Errno> {
        f(self)
    }

    fn len(&self) -> usize {
        Vec::<u8>::len(self)
    }

    fn split_off(&mut self, index: usize) -> Option<Self> {
        if index < self.len() {
            let bytes = self.split_off(index);
            Some(bytes)
        } else {
            None
        }
    }

    fn copy_to_user(&self, data: &mut dyn OutputBuffer) -> Result<usize, Errno> {
        data.write(self.as_ref())
    }

    fn clone_at_most(&self, limit: usize) -> Self {
        self[0..std::cmp::min(self.len(), limit)].to_vec()
    }

    fn truncate(&mut self, limit: usize) {
        self.truncate(limit)
    }
}

#[derive(Debug)]
pub enum PipeMessageData {
    Owned(Vec<u8>),
    Vmspliced(Arc<VmsplicePayload>),
}

impl From<Vec<u8>> for PipeMessageData {
    fn from(v: Vec<u8>) -> Self {
        Self::Owned(v)
    }
}

impl MessageData for PipeMessageData {
    fn copy_from_user(data: &mut dyn InputBuffer, limit: usize) -> Result<Self, Errno> {
        MessageData::copy_from_user(data, limit).map(Self::Owned)
    }

    fn ptr(&self) -> Result<*const u8, Errno> {
        match self {
            Self::Owned(d) => MessageData::ptr(d),
            Self::Vmspliced(d) => MessageData::ptr(d),
        }
    }

    fn with_bytes<O, F: FnMut(&[u8]) -> Result<O, Errno>>(&self, f: F) -> Result<O, Errno> {
        match self {
            Self::Owned(d) => MessageData::with_bytes(d, f),
            Self::Vmspliced(d) => MessageData::with_bytes(d, f),
        }
    }

    fn len(&self) -> usize {
        match self {
            Self::Owned(d) => MessageData::len(d),
            Self::Vmspliced(d) => MessageData::len(d),
        }
    }

    fn split_off(&mut self, index: usize) -> Option<Self> {
        match self {
            Self::Owned(d) => MessageData::split_off(d, index).map(Self::Owned),
            Self::Vmspliced(d) => MessageData::split_off(d, index).map(Self::Vmspliced),
        }
    }

    fn copy_to_user(&self, data: &mut dyn OutputBuffer) -> Result<usize, Errno> {
        match self {
            Self::Owned(d) => MessageData::copy_to_user(d, data),
            Self::Vmspliced(d) => MessageData::copy_to_user(d, data),
        }
    }

    fn clone_at_most(&self, limit: usize) -> Self {
        match self {
            Self::Owned(d) => Self::Owned(MessageData::clone_at_most(d, limit)),
            Self::Vmspliced(d) => Self::Vmspliced(MessageData::clone_at_most(d, limit)),
        }
    }

    fn truncate(&mut self, limit: usize) {
        match self {
            Self::Owned(d) => MessageData::truncate(d, limit),
            Self::Vmspliced(d) => MessageData::truncate(d, limit),
        }
    }
}
