// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::convert::From;

use crate::zxio::{
    zxio_dirent_iterator_next, zxio_dirent_iterator_t, ZXIO_NODE_PROTOCOL_CONNECTOR,
    ZXIO_NODE_PROTOCOL_DIRECTORY, ZXIO_NODE_PROTOCOL_FILE, ZXIO_NODE_PROTOCOL_SYMLINK,
};
use bitflags::bitflags;
use fidl::{encoding::const_assert_eq, endpoints::ServerEnd};
use fidl_fuchsia_io as fio;
use fuchsia_zircon::{
    self as zx,
    sys::{ZX_ERR_INVALID_ARGS, ZX_ERR_NO_MEMORY, ZX_OK},
    zx_status_t, HandleBased,
};
use std::{
    ffi::CStr,
    mem::{size_of, size_of_val},
    os::raw::{c_char, c_int, c_uint, c_void},
    pin::Pin,
};
use zerocopy::{AsBytes, FromBytes};
use zxio::{
    msghdr, sockaddr, sockaddr_storage, socklen_t, zx_handle_t, zxio_object_type_t,
    zxio_seek_origin_t, zxio_storage_t, ZXIO_SHUTDOWN_OPTIONS_READ, ZXIO_SHUTDOWN_OPTIONS_WRITE,
};

pub mod zxio;

pub use zxio::zxio_dirent_t;
pub use zxio::zxio_node_attr_zxio_node_attr_has_t as zxio_node_attr_has_t;
pub use zxio::zxio_node_attributes_t;
pub use zxio::zxio_signals_t;

bitflags! {
    // These values should match the values in sdk/lib/zxio/include/lib/zxio/types.h
    pub struct ZxioSignals : zxio_signals_t {
        const NONE            =      0;
        const READABLE        = 1 << 0;
        const WRITABLE        = 1 << 1;
        const READ_DISABLED   = 1 << 2;
        const WRITE_DISABLED  = 1 << 3;
        const READ_THRESHOLD  = 1 << 4;
        const WRITE_THRESHOLD = 1 << 5;
        const OUT_OF_BAND     = 1 << 6;
        const ERROR           = 1 << 7;
        const PEER_CLOSED     = 1 << 8;
    }
}

bitflags! {
    /// The flags for shutting down sockets.
    pub struct ZxioShutdownFlags: u32 {
        /// Further transmissions will be disallowed.
        const WRITE = 1 << 0;

        /// Further receptions will be disallowed.
        const READ = 1 << 1;
    }
}

const_assert_eq!(ZxioShutdownFlags::WRITE.bits(), ZXIO_SHUTDOWN_OPTIONS_WRITE);
const_assert_eq!(ZxioShutdownFlags::READ.bits(), ZXIO_SHUTDOWN_OPTIONS_READ);

pub enum SeekOrigin {
    Start,
    Current,
    End,
}

impl From<SeekOrigin> for zxio_seek_origin_t {
    fn from(origin: SeekOrigin) -> Self {
        match origin {
            SeekOrigin::Start => zxio::ZXIO_SEEK_ORIGIN_START,
            SeekOrigin::Current => zxio::ZXIO_SEEK_ORIGIN_CURRENT,
            SeekOrigin::End => zxio::ZXIO_SEEK_ORIGIN_END,
        }
    }
}

// TODO: We need a more comprehensive error strategy.
// Our dependencies create elaborate error objects, but Starnix would prefer
// this library produce zx::Status errors for easier conversion to Errno.

#[derive(Default, Debug)]
pub struct ZxioDirent {
    pub protocols: Option<zxio::zxio_node_protocols_t>,
    pub abilities: Option<zxio::zxio_abilities_t>,
    pub id: Option<zxio::zxio_id_t>,
    pub name: Vec<u8>,
}

pub struct DirentIterator {
    iterator: Box<zxio_dirent_iterator_t>,

    /// Whether the iterator has reached the end of dir entries.
    /// This is necessary because the zxio API returns only once the error code
    /// indicating the iterator has reached the end, where subsequent calls may
    /// return other error codes.
    finished: bool,
}

impl DirentIterator {
    /// Rewind the iterator to the beginning.
    pub fn rewind(&mut self) -> Result<(), zx::Status> {
        let status = unsafe { zxio::zxio_dirent_iterator_rewind(&mut *self.iterator) };
        zx::ok(status)?;
        self.finished = false;
        Ok(())
    }
}

/// It is important that all methods here are &mut self, to require the client
/// to obtain exclusive access to the object, externally locking it.
impl Iterator for DirentIterator {
    type Item = Result<ZxioDirent, zx::Status>;

    /// Returns the next dir entry for this iterator.
    fn next(&mut self) -> Option<Result<ZxioDirent, zx::Status>> {
        if self.finished {
            return None;
        }
        let mut entry = zxio_dirent_t::default();
        let mut name_buffer = Vec::with_capacity(fio::MAX_FILENAME as usize);
        // The FFI interface expects a pointer to c_char which is i8 on x86_64.
        // The Rust str and OsStr types expect raw character data to be stored in a buffer u8 values.
        // The types are equivalent for all practical purposes and Rust permits casting between the types,
        // so we insert a type cast here in the FFI bindings.
        entry.name = name_buffer.as_mut_ptr() as *mut c_char;
        let status = unsafe { zxio_dirent_iterator_next(&mut *self.iterator.as_mut(), &mut entry) };
        let result = match zx::ok(status) {
            Ok(()) => {
                let result = ZxioDirent::from(entry, name_buffer);
                Ok(result)
            }
            Err(zx::Status::NOT_FOUND) => {
                self.finished = true;
                return None;
            }
            Err(e) => Err(e),
        };
        return Some(result);
    }
}

impl Drop for DirentIterator {
    fn drop(&mut self) {
        unsafe {
            zxio::zxio_dirent_iterator_destroy(&mut *self.iterator.as_mut());
        };
    }
}

unsafe impl Send for DirentIterator {}
unsafe impl Sync for DirentIterator {}

impl ZxioDirent {
    fn from(dirent: zxio_dirent_t, name_buffer: Vec<u8>) -> ZxioDirent {
        let protocols = if dirent.has.protocols { Some(dirent.protocols) } else { None };
        let abilities = if dirent.has.abilities { Some(dirent.abilities) } else { None };
        let id = if dirent.has.id { Some(dirent.id) } else { None };
        let mut name = name_buffer;
        unsafe { name.set_len(dirent.name_length as usize) };
        ZxioDirent { protocols, abilities, id, name }
    }

    pub fn is_dir(&self) -> bool {
        self.protocols.map(|p| p & ZXIO_NODE_PROTOCOL_DIRECTORY > 0).unwrap_or(false)
    }

    pub fn is_file(&self) -> bool {
        self.protocols.map(|p| p & ZXIO_NODE_PROTOCOL_FILE > 0).unwrap_or(false)
    }
}

pub struct ZxioErrorCode(i16);
impl ZxioErrorCode {
    pub fn raw(&self) -> i16 {
        self.0
    }
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum ControlMessage {
    IpTos(u8),
    IpTtl(u8),
    Ipv6Tclass(u8),
    Ipv6HopLimit(u8),
    Ipv6PacketInfo { iface: u32, local_addr: [u8; size_of::<zxio::in6_addr>()] },
}

const fn align_cmsg_size(len: usize) -> usize {
    (len + size_of::<usize>() - 1) & !(size_of::<usize>() - 1)
}

const CMSG_HEADER_SIZE: usize = align_cmsg_size(size_of::<zxio::cmsghdr>());

// Size of the buffer to allocate in recvmsg() for cmsgs buffer. We need a buffer that can fit
// Ipv6Tclass, Ipv6HopLimit and Ipv6HopLimit messages.
const MAX_CMSGS_BUFFER: usize =
    CMSG_HEADER_SIZE * 3 + align_cmsg_size(1) * 2 + align_cmsg_size(size_of::<zxio::in6_pktinfo>());

impl ControlMessage {
    pub fn get_data_size(&self) -> usize {
        match self {
            ControlMessage::IpTos(_) => 1,
            ControlMessage::IpTtl(_) => size_of::<c_int>(),
            ControlMessage::Ipv6Tclass(_) => size_of::<c_int>(),
            ControlMessage::Ipv6HopLimit(_) => size_of::<c_int>(),
            ControlMessage::Ipv6PacketInfo { .. } => size_of::<zxio::in6_pktinfo>(),
        }
    }

    // Serializes the data in the format expected by ZXIO.
    fn serialize<'a>(&'a self, out: &'a mut [u8]) -> usize {
        let data = &mut out[CMSG_HEADER_SIZE..];
        let (size, level, type_) = match self {
            ControlMessage::IpTos(v) => {
                v.write_to_prefix(data).unwrap();
                (1, zxio::SOL_IP, zxio::IP_TOS)
            }
            ControlMessage::IpTtl(v) => {
                (*v as c_int).write_to_prefix(data).unwrap();
                (size_of::<c_int>(), zxio::SOL_IP, zxio::IP_TTL)
            }
            ControlMessage::Ipv6Tclass(v) => {
                (*v as c_int).write_to_prefix(data).unwrap();
                (size_of::<c_int>(), zxio::SOL_IPV6, zxio::IPV6_TCLASS)
            }
            ControlMessage::Ipv6HopLimit(v) => {
                (*v as c_int).write_to_prefix(data).unwrap();
                (size_of::<c_int>(), zxio::SOL_IPV6, zxio::IPV6_HOPLIMIT)
            }
            ControlMessage::Ipv6PacketInfo { iface, local_addr } => {
                let pktinfo = zxio::in6_pktinfo {
                    ipi6_addr: zxio::in6_addr {
                        __in6_union: zxio::in6_addr__bindgen_ty_1 { __s6_addr: *local_addr },
                    },
                    ipi6_ifindex: *iface,
                };
                pktinfo.write_to_prefix(data).unwrap();
                (size_of_val(&pktinfo), zxio::SOL_IPV6, zxio::IPV6_PKTINFO)
            }
        };
        let total_size = CMSG_HEADER_SIZE + size;
        let header = zxio::cmsghdr {
            cmsg_len: total_size as c_uint,
            cmsg_level: level as i32,
            cmsg_type: type_ as i32,
        };
        header.write_to_prefix(&mut out[..]).unwrap();

        total_size
    }
}

fn serialize_control_messages(messages: &[ControlMessage]) -> Vec<u8> {
    let size = messages
        .iter()
        .fold(0, |sum, x| sum + CMSG_HEADER_SIZE + align_cmsg_size(x.get_data_size()));
    let mut buffer = vec![0u8; size];
    let mut pos = 0;
    for msg in messages {
        pos += align_cmsg_size(msg.serialize(&mut buffer[pos..]));
    }
    assert_eq!(pos, buffer.len());
    buffer
}

fn parse_control_messages(data: &[u8]) -> Vec<ControlMessage> {
    let mut result = vec![];
    let mut pos = 0;
    loop {
        if pos >= data.len() {
            return result;
        }
        let header_data = &data[pos..];
        let header = match zxio::cmsghdr::read_from_prefix(header_data) {
            Some(h) if h.cmsg_len as usize > CMSG_HEADER_SIZE => h,
            _ => return result,
        };

        let msg_data = &data[pos + CMSG_HEADER_SIZE..pos + header.cmsg_len as usize];
        let msg = match (header.cmsg_level as u32, header.cmsg_type as u32) {
            (zxio::SOL_IP, zxio::IP_TOS) => {
                ControlMessage::IpTos(u8::read_from_prefix(msg_data).unwrap())
            }
            (zxio::SOL_IP, zxio::IP_TTL) => {
                ControlMessage::IpTtl(c_int::read_from_prefix(msg_data).unwrap() as u8)
            }
            (zxio::SOL_IPV6, zxio::IPV6_TCLASS) => {
                ControlMessage::Ipv6Tclass(c_int::read_from_prefix(msg_data).unwrap() as u8)
            }
            (zxio::SOL_IPV6, zxio::IPV6_HOPLIMIT) => {
                ControlMessage::Ipv6HopLimit(c_int::read_from_prefix(msg_data).unwrap() as u8)
            }
            (zxio::SOL_IPV6, zxio::IPV6_PKTINFO) => {
                let pkt_info = zxio::in6_pktinfo::read_from_prefix(msg_data).unwrap();
                ControlMessage::Ipv6PacketInfo {
                    local_addr: unsafe { pkt_info.ipi6_addr.__in6_union.__s6_addr },
                    iface: pkt_info.ipi6_ifindex,
                }
            }
            _ => panic!(
                "ZXIO produced unexpected cmsg level={}, type={}",
                header.cmsg_level, header.cmsg_type
            ),
        };
        result.push(msg);

        pos += align_cmsg_size(header.cmsg_len as usize);
    }
}

pub struct RecvMessageInfo {
    pub address: Vec<u8>,
    pub message: Vec<u8>,
    pub message_length: usize,
    pub control_messages: Vec<ControlMessage>,
    pub flags: i32,
}

/// Options for open2.
pub struct OpenOptions {
    /// If None, connects to a service.
    pub node_protocols: Option<fio::NodeProtocols>,

    /// Behaviour with respect ot existence. See fuchsia.io for precise semantics.
    pub mode: fio::OpenMode,

    /// See fuchsia.io for semantics. If empty, then it is regarded as absent i.e.  rights will be
    /// inherited.
    pub rights: fio::Operations,

    /// If an object is to be created, attributes that should be stored with the object at creation
    /// time. Not all servers support all attributes.
    pub create_attr: Option<zxio::zxio_node_attr>,
}

impl OpenOptions {
    /// Returns options to open a directory.
    pub fn directory(maximum_rights: Option<fio::Operations>) -> Self {
        Self {
            node_protocols: Some(fio::NodeProtocols {
                directory: Some(fio::DirectoryProtocolOptions {
                    maximum_rights,
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    /// Returns options to open a file.
    pub fn file(flags: fio::FileProtocolFlags) -> Self {
        Self {
            node_protocols: Some(fio::NodeProtocols { file: Some(flags), ..Default::default() }),
            ..Default::default()
        }
    }
}

impl Default for OpenOptions {
    fn default() -> Self {
        Self {
            // Default to opening a node protocol.
            node_protocols: Some(fio::NodeProtocols::default()),
            mode: fio::OpenMode::OpenExisting,
            rights: fio::Operations::empty(),
            create_attr: None,
        }
    }
}

// `ZxioStorage` is marked as `PhantomPinned` in order to prevent unsafe moves
// of the `zxio_storage_t`, because it may store self-referential types defined
// in zxio.
#[derive(Default)]
struct ZxioStorage {
    storage: zxio::zxio_storage_t,
    _pin: std::marker::PhantomPinned,
}

/// A handle to a zxio object.
///
/// Note: the underlying storage backing the object is pinned on the heap
/// because it can contain self referential data.
pub struct Zxio {
    inner: Pin<Box<ZxioStorage>>,
}

impl Default for Zxio {
    fn default() -> Self {
        Self { inner: Box::pin(ZxioStorage::default()) }
    }
}

impl std::fmt::Debug for Zxio {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Zxio").finish()
    }
}

/// A trait that provides functionality to connect a channel to a FIDL service.
//
// TODO(https://github.com/rust-lang/rust/issues/44291): allow clients to pass
// in a more general function pointer (`fn(&str, zx::Channel) -> zx::Status`)
// rather than having to implement this trait.
pub trait ServiceConnector {
    /// Connect `server_end` to the protocol available at `service`.
    fn connect(service: &str, server_end: zx::Channel) -> zx::Status;
}

/// Connect a handle to a specified service.
///
/// SAFETY: Dereferences the raw pointers `service_name` and `provider_handle`.
unsafe extern "C" fn service_connector<S: ServiceConnector>(
    service_name: *const c_char,
    provider_handle: *mut zx_handle_t,
) -> zx_status_t {
    let (client_end, server_end) = zx::Channel::create();

    let service = match CStr::from_ptr(service_name).to_str() {
        Ok(s) => s,
        Err(_) => return ZX_ERR_INVALID_ARGS,
    };

    let status = S::connect(service, server_end).into_raw();
    if status != ZX_OK {
        return status;
    }

    *provider_handle = client_end.into_handle().into_raw();
    ZX_OK
}

/// Sets `out_storage` as the zxio_storage of `out_context`.
///
/// This function is intended to be passed to zxio_socket().
///
/// SAFETY: Dereferences the raw pointer `out_storage`.
unsafe extern "C" fn storage_allocator(
    _type: zxio_object_type_t,
    out_storage: *mut *mut zxio_storage_t,
    out_context: *mut *mut c_void,
) -> zx_status_t {
    let zxio_ptr_ptr = out_context as *mut *mut zxio_storage_t;
    if let Some(zxio_ptr) = zxio_ptr_ptr.as_mut() {
        if let Some(zxio) = zxio_ptr.as_mut() {
            *out_storage = zxio;
            return ZX_OK;
        }
    }
    ZX_ERR_NO_MEMORY
}

impl Zxio {
    pub fn new_socket<S: ServiceConnector>(
        domain: c_int,
        socket_type: c_int,
        protocol: c_int,
    ) -> Result<Result<Self, ZxioErrorCode>, zx::Status> {
        let zxio = Zxio::default();
        let mut out_context = zxio.as_storage_ptr() as *mut c_void;
        let mut out_code = 0;

        let status = unsafe {
            zxio::zxio_socket(
                Some(service_connector::<S>),
                domain,
                socket_type,
                protocol,
                Some(storage_allocator),
                &mut out_context as *mut *mut c_void,
                &mut out_code,
            )
        };
        zx::ok(status)?;
        match out_code {
            0 => Ok(Ok(zxio)),
            _ => Ok(Err(ZxioErrorCode(out_code))),
        }
    }

    fn as_ptr(&self) -> *mut zxio::zxio_t {
        &self.inner.storage.io as *const zxio::zxio_t as *mut zxio::zxio_t
    }

    fn as_storage_ptr(&self) -> *mut zxio::zxio_storage_t {
        &self.inner.storage as *const zxio::zxio_storage_t as *mut zxio::zxio_storage_t
    }

    pub fn create(handle: zx::Handle) -> Result<Zxio, zx::Status> {
        let zxio = Zxio::default();
        let status = unsafe { zxio::zxio_create(handle.into_raw(), zxio.as_storage_ptr()) };
        zx::ok(status)?;
        Ok(zxio)
    }

    pub fn release(self) -> Result<zx::Handle, zx::Status> {
        let mut handle = 0;
        let status = unsafe { zxio::zxio_release(self.as_ptr(), &mut handle) };
        zx::ok(status)?;
        unsafe { Ok(zx::Handle::from_raw(handle)) }
    }

    pub fn open(&self, flags: fio::OpenFlags, path: &str) -> Result<Self, zx::Status> {
        let zxio = Zxio::default();
        let status = unsafe {
            zxio::zxio_open(
                self.as_ptr(),
                flags.bits(),
                path.as_ptr() as *const c_char,
                path.len(),
                zxio.as_storage_ptr(),
            )
        };
        zx::ok(status)?;
        Ok(zxio)
    }

    pub fn open2(
        &self,
        path: &str,
        options: OpenOptions,
        attributes: Option<&mut zxio_node_attributes_t>,
    ) -> Result<Self, zx::Status> {
        let zxio = Zxio::default();

        let mut open_options = zxio::zxio_open_options::default();
        if let Some(p) = options.node_protocols {
            if let Some(dir_options) = p.directory {
                open_options.protocols |= ZXIO_NODE_PROTOCOL_DIRECTORY;
                open_options.maximum_rights =
                    dir_options.maximum_rights.unwrap_or(fio::Operations::empty()).bits();
            }
            if let Some(file_flags) = p.file {
                open_options.protocols |= ZXIO_NODE_PROTOCOL_FILE;
                open_options.file_flags = file_flags.bits();
            }
            if p.symlink.is_some() {
                open_options.protocols |= ZXIO_NODE_PROTOCOL_SYMLINK;
            }
            if open_options.protocols == 0 {
                // Ask for any protocol.
                open_options.protocols = ZXIO_NODE_PROTOCOL_DIRECTORY
                    | ZXIO_NODE_PROTOCOL_FILE
                    | ZXIO_NODE_PROTOCOL_SYMLINK;
            }
            open_options.mode = options.mode as u32;
            open_options.rights = options.rights.bits();
            open_options.create_attr =
                options.create_attr.as_ref().map(|a| a as *const _).unwrap_or(std::ptr::null());
        } else {
            open_options.protocols = ZXIO_NODE_PROTOCOL_CONNECTOR;
        }

        let status = unsafe {
            zxio::zxio_open2(
                self.as_ptr(),
                path.as_ptr() as *const c_char,
                path.len(),
                &open_options,
                attributes.map(|a| a as *mut _).unwrap_or(std::ptr::null_mut()),
                zxio.as_storage_ptr(),
            )
        };
        zx::ok(status)?;
        Ok(zxio)
    }

    pub fn read(&self, data: &mut [u8]) -> Result<usize, zx::Status> {
        let flags = zxio::zxio_flags_t::default();
        let mut actual = 0usize;
        let status = unsafe {
            zxio::zxio_read(
                self.as_ptr(),
                data.as_ptr() as *mut c_void,
                data.len(),
                flags,
                &mut actual,
            )
        };
        zx::ok(status)?;
        Ok(actual)
    }

    pub fn clone(&self) -> Result<Zxio, zx::Status> {
        let mut handle = 0;
        let status = unsafe { zxio::zxio_clone(self.as_ptr(), &mut handle) };
        zx::ok(status)?;
        unsafe { Zxio::create(zx::Handle::from_raw(handle)) }
    }

    pub fn read_at(&self, offset: u64, data: &mut [u8]) -> Result<usize, zx::Status> {
        let flags = zxio::zxio_flags_t::default();
        let mut actual = 0usize;
        let status = unsafe {
            zxio::zxio_read_at(
                self.as_ptr(),
                offset,
                data.as_ptr() as *mut c_void,
                data.len(),
                flags,
                &mut actual,
            )
        };
        zx::ok(status)?;
        Ok(actual)
    }

    pub fn write(&self, data: &[u8]) -> Result<usize, zx::Status> {
        let flags = zxio::zxio_flags_t::default();
        let mut actual = 0;
        let status = unsafe {
            zxio::zxio_write(
                self.as_ptr(),
                data.as_ptr() as *const c_void,
                data.len(),
                flags,
                &mut actual,
            )
        };
        zx::ok(status)?;
        Ok(actual)
    }

    pub fn write_at(&self, offset: u64, data: &[u8]) -> Result<usize, zx::Status> {
        let flags = zxio::zxio_flags_t::default();
        let mut actual = 0;
        let status = unsafe {
            zxio::zxio_write_at(
                self.as_ptr(),
                offset,
                data.as_ptr() as *const c_void,
                data.len(),
                flags,
                &mut actual,
            )
        };
        zx::ok(status)?;
        Ok(actual)
    }

    pub fn truncate(&self, length: u64) -> Result<(), zx::Status> {
        let status = unsafe { zxio::zxio_truncate(self.as_ptr(), length) };
        zx::ok(status)?;
        Ok(())
    }

    pub fn seek(&self, seek_origin: SeekOrigin, offset: i64) -> Result<usize, zx::Status> {
        let mut result = 0;
        let status =
            unsafe { zxio::zxio_seek(self.as_ptr(), seek_origin.into(), offset, &mut result) };
        zx::ok(status)?;
        Ok(result)
    }

    pub fn vmo_get(&self, flags: zx::VmarFlags) -> Result<zx::Vmo, zx::Status> {
        let mut vmo = 0;
        let status = unsafe { zxio::zxio_vmo_get(self.as_ptr(), flags.bits(), &mut vmo) };
        zx::ok(status)?;
        let handle = unsafe { zx::Handle::from_raw(vmo) };
        Ok(zx::Vmo::from(handle))
    }

    pub fn attr_get(&self) -> Result<zxio_node_attributes_t, zx::Status> {
        let mut attributes = zxio_node_attributes_t::default();
        let status = unsafe { zxio::zxio_attr_get(self.as_ptr(), &mut attributes) };
        zx::ok(status)?;
        Ok(attributes)
    }

    pub fn attr_set(&self, attributes: &zxio_node_attributes_t) -> Result<(), zx::Status> {
        let status = unsafe { zxio::zxio_attr_set(self.as_ptr(), attributes) };
        zx::ok(status)?;
        Ok(())
    }

    pub fn rename(
        &self,
        old_path: &str,
        new_directory: &Zxio,
        new_path: &str,
    ) -> Result<(), zx::Status> {
        let mut handle = zx::sys::ZX_HANDLE_INVALID;
        let status = unsafe { zxio::zxio_token_get(new_directory.as_ptr(), &mut handle) };
        zx::ok(status)?;
        let status = unsafe {
            zxio::zxio_rename(
                self.as_ptr(),
                old_path.as_ptr() as *const c_char,
                old_path.len(),
                handle,
                new_path.as_ptr() as *const c_char,
                new_path.len(),
            )
        };
        zx::ok(status)?;
        Ok(())
    }

    pub fn wait_begin(
        &self,
        zxio_signals: zxio_signals_t,
    ) -> (zx::Unowned<'_, zx::Handle>, zx::Signals) {
        let mut handle = zx::sys::ZX_HANDLE_INVALID;
        let mut zx_signals = zx::sys::ZX_SIGNAL_NONE;
        unsafe { zxio::zxio_wait_begin(self.as_ptr(), zxio_signals, &mut handle, &mut zx_signals) };
        let handle = unsafe { zx::Unowned::<zx::Handle>::from_raw_handle(handle) };
        let signals = zx::Signals::from_bits_truncate(zx_signals);
        (handle, signals)
    }

    pub fn wait_end(&self, signals: zx::Signals) -> zxio_signals_t {
        let mut zxio_signals = ZxioSignals::NONE.bits();
        unsafe {
            zxio::zxio_wait_end(self.as_ptr(), signals.bits(), &mut zxio_signals);
        }
        zxio_signals
    }

    pub fn create_dirent_iterator(&self) -> Result<DirentIterator, zx::Status> {
        let mut zxio_iterator = Box::default();
        let status = unsafe { zxio::zxio_dirent_iterator_init(&mut *zxio_iterator, self.as_ptr()) };
        zx::ok(status)?;
        let iterator = DirentIterator { iterator: zxio_iterator, finished: false };
        Ok(iterator)
    }

    pub fn connect(&self, addr: &[u8]) -> Result<Result<(), ZxioErrorCode>, zx::Status> {
        let mut out_code = 0;
        let status = unsafe {
            zxio::zxio_connect(
                self.as_ptr(),
                addr.as_ptr() as *const sockaddr,
                addr.len() as socklen_t,
                &mut out_code,
            )
        };
        zx::ok(status)?;
        match out_code {
            0 => Ok(Ok(())),
            _ => Ok(Err(ZxioErrorCode(out_code))),
        }
    }

    pub fn bind(&self, addr: &[u8]) -> Result<Result<(), ZxioErrorCode>, zx::Status> {
        let mut out_code = 0;
        let status = unsafe {
            zxio::zxio_bind(
                self.as_ptr(),
                addr.as_ptr() as *const sockaddr,
                addr.len() as socklen_t,
                &mut out_code,
            )
        };
        zx::ok(status)?;
        match out_code {
            0 => Ok(Ok(())),
            _ => Ok(Err(ZxioErrorCode(out_code))),
        }
    }

    pub fn listen(&self, backlog: i32) -> Result<Result<(), ZxioErrorCode>, zx::Status> {
        let mut out_code = 0;
        let status = unsafe { zxio::zxio_listen(self.as_ptr(), backlog as c_int, &mut out_code) };
        zx::ok(status)?;
        match out_code {
            0 => Ok(Ok(())),
            _ => Ok(Err(ZxioErrorCode(out_code))),
        }
    }

    pub fn accept(&self) -> Result<Result<Zxio, ZxioErrorCode>, zx::Status> {
        let mut addrlen = std::mem::size_of::<sockaddr_storage>() as socklen_t;
        let mut addr = vec![0u8; addrlen as usize];
        let zxio = Zxio::default();
        let mut out_code = 0;
        let status = unsafe {
            zxio::zxio_accept(
                self.as_ptr(),
                addr.as_mut_ptr() as *mut sockaddr,
                &mut addrlen,
                zxio.as_storage_ptr(),
                &mut out_code,
            )
        };
        zx::ok(status)?;
        match out_code {
            0 => Ok(Ok(zxio)),
            _ => Ok(Err(ZxioErrorCode(out_code))),
        }
    }

    pub fn getsockname(&self) -> Result<Result<Vec<u8>, ZxioErrorCode>, zx::Status> {
        let mut addrlen = std::mem::size_of::<sockaddr_storage>() as socklen_t;
        let mut addr = vec![0u8; addrlen as usize];
        let mut out_code = 0;
        let status = unsafe {
            zxio::zxio_getsockname(
                self.as_ptr(),
                addr.as_mut_ptr() as *mut sockaddr,
                &mut addrlen,
                &mut out_code,
            )
        };
        zx::ok(status)?;
        match out_code {
            0 => Ok(Ok(addr[..addrlen as usize].to_vec())),
            _ => Ok(Err(ZxioErrorCode(out_code))),
        }
    }

    pub fn getpeername(&self) -> Result<Result<Vec<u8>, ZxioErrorCode>, zx::Status> {
        let mut addrlen = std::mem::size_of::<sockaddr_storage>() as socklen_t;
        let mut addr = vec![0u8; addrlen as usize];
        let mut out_code = 0;
        let status = unsafe {
            zxio::zxio_getpeername(
                self.as_ptr(),
                addr.as_mut_ptr() as *mut sockaddr,
                &mut addrlen,
                &mut out_code,
            )
        };
        zx::ok(status)?;
        match out_code {
            0 => Ok(Ok(addr[..addrlen as usize].to_vec())),
            _ => Ok(Err(ZxioErrorCode(out_code))),
        }
    }

    pub fn getsockopt(
        &self,
        level: u32,
        optname: u32,
        mut optlen: socklen_t,
    ) -> Result<Result<Vec<u8>, ZxioErrorCode>, zx::Status> {
        let mut optval = vec![0u8; optlen as usize];
        let mut out_code = 0;
        let status = unsafe {
            zxio::zxio_getsockopt(
                self.as_ptr(),
                level as c_int,
                optname as c_int,
                optval.as_mut_ptr() as *mut c_void,
                &mut optlen,
                &mut out_code,
            )
        };
        zx::ok(status)?;
        match out_code {
            0 => Ok(Ok(optval[..optlen as usize].to_vec())),
            _ => Ok(Err(ZxioErrorCode(out_code))),
        }
    }

    pub fn setsockopt(
        &self,
        level: i32,
        optname: i32,
        optval: &[u8],
    ) -> Result<Result<(), ZxioErrorCode>, zx::Status> {
        let mut out_code = 0;
        let status = unsafe {
            zxio::zxio_setsockopt(
                self.as_ptr(),
                level,
                optname,
                optval.as_ptr() as *const c_void,
                optval.len() as socklen_t,
                &mut out_code,
            )
        };
        zx::ok(status)?;
        match out_code {
            0 => Ok(Ok(())),
            _ => Ok(Err(ZxioErrorCode(out_code))),
        }
    }

    pub fn shutdown(
        &self,
        flags: ZxioShutdownFlags,
    ) -> Result<Result<(), ZxioErrorCode>, zx::Status> {
        let mut out_code = 0;
        let status = unsafe { zxio::zxio_shutdown(self.as_ptr(), flags.bits(), &mut out_code) };
        zx::ok(status)?;
        match out_code {
            0 => Ok(Ok(())),
            _ => Ok(Err(ZxioErrorCode(out_code))),
        }
    }

    pub fn sendmsg(
        &self,
        mut addr: Vec<u8>,
        mut buffer: Vec<u8>,
        cmsg: Vec<ControlMessage>,
        flags: u32,
    ) -> Result<Result<usize, ZxioErrorCode>, zx::Status> {
        let mut msg = zxio::msghdr::default();
        msg.msg_name = match addr.len() {
            0 => std::ptr::null_mut() as *mut c_void,
            _ => addr.as_mut_ptr() as *mut c_void,
        };
        msg.msg_namelen = addr.len() as u32;

        let mut iov =
            zxio::iovec { iov_base: buffer.as_mut_ptr() as *mut c_void, iov_len: buffer.len() };
        msg.msg_iov = &mut iov;
        msg.msg_iovlen = 1;

        let mut cmsg_buffer = serialize_control_messages(&cmsg);
        msg.msg_control = cmsg_buffer.as_mut_ptr() as *mut c_void;
        msg.msg_controllen = cmsg_buffer.len() as u32;

        let mut out_code = 0;
        let mut out_actual = 0;

        let status = unsafe {
            zxio::zxio_sendmsg(self.as_ptr(), &msg, flags as c_int, &mut out_actual, &mut out_code)
        };

        zx::ok(status)?;
        match out_code {
            0 => Ok(Ok(out_actual)),
            _ => Ok(Err(ZxioErrorCode(out_code))),
        }
    }

    pub fn recvmsg(
        &self,
        iovec_length: usize,
        flags: u32,
    ) -> Result<Result<RecvMessageInfo, ZxioErrorCode>, zx::Status> {
        let mut msg = msghdr::default();
        let mut addr = vec![0u8; std::mem::size_of::<sockaddr_storage>()];
        msg.msg_name = addr.as_mut_ptr() as *mut c_void;
        msg.msg_namelen = addr.len() as u32;

        let mut iov_buf = vec![0u8; iovec_length];
        let mut iov =
            zxio::iovec { iov_base: iov_buf.as_mut_ptr() as *mut c_void, iov_len: iovec_length };
        msg.msg_iov = &mut iov;
        msg.msg_iovlen = 1;

        let mut cmsg_buffer = vec![0u8; MAX_CMSGS_BUFFER];
        msg.msg_control = cmsg_buffer.as_mut_ptr() as *mut c_void;
        msg.msg_controllen = cmsg_buffer.len() as u32;

        let mut out_code = 0;
        let mut out_actual = 0;
        let status = unsafe {
            zxio::zxio_recvmsg(
                self.as_ptr(),
                &mut msg,
                flags as c_int,
                &mut out_actual,
                &mut out_code,
            )
        };
        zx::ok(status)?;

        if out_code != 0 {
            return Ok(Err(ZxioErrorCode(out_code)));
        }

        let control_messages = parse_control_messages(&cmsg_buffer[..msg.msg_controllen as usize]);
        let min_buf_len = std::cmp::min(iov_buf.len(), out_actual);
        Ok(Ok(RecvMessageInfo {
            address: addr[..msg.msg_namelen as usize].to_vec(),
            message: iov_buf[..min_buf_len].to_vec(),
            message_length: out_actual,
            control_messages,
            flags: msg.msg_flags,
        }))
    }

    pub fn read_link(&self) -> Result<&[u8], zx::Status> {
        let mut target = std::ptr::null();
        let mut target_len = 0;
        let status = unsafe { zxio::zxio_read_link(self.as_ptr(), &mut target, &mut target_len) };
        zx::ok(status)?;
        // SAFETY: target will live as long as the underlying zxio object lives.
        unsafe { Ok(std::slice::from_raw_parts(target, target_len)) }
    }

    pub fn create_symlink(&self, name: &str, target: &[u8]) -> Result<Zxio, zx::Status> {
        let name = name.as_bytes();
        let zxio = Zxio::default();
        let status = unsafe {
            zxio::zxio_create_symlink(
                self.as_ptr(),
                name.as_ptr() as *const c_char,
                name.len(),
                target.as_ptr(),
                target.len(),
                zxio.as_storage_ptr(),
            )
        };
        zx::ok(status)?;
        Ok(zxio)
    }

    pub fn xattr_list(&self) -> Result<Vec<Vec<u8>>, zx::Status> {
        unsafe extern "C" fn callback(context: *mut c_void, name: *const u8, name_len: usize) {
            let out_names = &mut *(context as *mut Vec<Vec<u8>>);
            let name_slice = std::slice::from_raw_parts(name, name_len);
            out_names.push(name_slice.to_vec());
        }
        let mut out_names = Vec::new();
        let status = unsafe {
            zxio::zxio_xattr_list(
                self.as_ptr(),
                Some(callback),
                &mut out_names as *mut _ as *mut c_void,
            )
        };
        zx::ok(status)?;
        Ok(out_names)
    }

    pub fn xattr_get(&self, name: &[u8], value: &mut [u8]) -> Result<usize, zx::Status> {
        let mut out_value_len = 0;
        let status = unsafe {
            zxio::zxio_xattr_get(
                self.as_ptr(),
                name.as_ptr(),
                name.len(),
                value.as_mut_ptr(),
                value.len(),
                &mut out_value_len,
            )
        };
        zx::ok(status)?;
        Ok(out_value_len)
    }

    pub fn xattr_set(&self, name: &[u8], value: &[u8]) -> Result<(), zx::Status> {
        let status = unsafe {
            zxio::zxio_xattr_set(
                self.as_ptr(),
                name.as_ptr(),
                name.len(),
                value.as_ptr(),
                value.len(),
            )
        };
        zx::ok(status)
    }

    pub fn xattr_remove(&self, name: &[u8]) -> Result<(), zx::Status> {
        zx::ok(unsafe { zxio::zxio_xattr_remove(self.as_ptr(), name.as_ptr(), name.len()) })
    }
}

impl Drop for Zxio {
    fn drop(&mut self) {
        unsafe {
            zxio::zxio_close(self.as_ptr(), true);
        };
    }
}

enum NodeKind {
    File,
    Directory,
    Unknown,
}

impl NodeKind {
    fn from(info: &fio::NodeInfoDeprecated) -> NodeKind {
        match info {
            fio::NodeInfoDeprecated::File(_) => NodeKind::File,
            fio::NodeInfoDeprecated::Directory(_) => NodeKind::Directory,
            _ => NodeKind::Unknown,
        }
    }

    fn from2(representation: &fio::Representation) -> NodeKind {
        match representation {
            fio::Representation::File(_) => NodeKind::File,
            fio::Representation::Directory(_) => NodeKind::Directory,
            _ => NodeKind::Unknown,
        }
    }
}

/// A fuchsia.io.Node along with its NodeInfoDeprecated.
///
/// The NodeInfoDeprecated provides information about the concrete protocol spoken by the
/// node.
struct DescribedNode {
    node: fio::NodeSynchronousProxy,
    kind: NodeKind,
}

/// Open the given path in the given directory.
///
/// The semantics for the flags argument are defined by the
/// fuchsia.io/Directory.Open message.
///
/// This function adds OPEN_FLAG_DESCRIBE to the given flags and then blocks
/// until the directory describes the newly opened node.
///
/// Returns the opened Node, along with its NodeInfoDeprecated, or an error.
fn directory_open(
    directory: &fio::DirectorySynchronousProxy,
    path: &str,
    flags: fio::OpenFlags,
    deadline: zx::Time,
) -> Result<DescribedNode, zx::Status> {
    let flags = flags | fio::OpenFlags::DESCRIBE;

    let (client_end, server_end) = zx::Channel::create();
    directory
        .open(flags, fio::ModeType::empty(), path, ServerEnd::new(server_end))
        .map_err(|_| zx::Status::IO)?;
    let node = fio::NodeSynchronousProxy::new(client_end);

    match node.wait_for_event(deadline).map_err(|_| zx::Status::IO)? {
        fio::NodeEvent::OnOpen_ { s: status, info } => {
            zx::Status::ok(status)?;
            Ok(DescribedNode { node, kind: NodeKind::from(&*info.ok_or(zx::Status::IO)?) })
        }
        fio::NodeEvent::OnRepresentation { payload } => {
            Ok(DescribedNode { node, kind: NodeKind::from2(&payload) })
        }
    }
}

/// Open a VMO at the given path in the given directory.
///
/// The semantics for the vmo_flags argument are defined by the
/// fuchsia.io/File.GetBackingMemory message (i.e., VmoFlags::*).
///
/// If the node at the given path is not a VMO, then this function returns
/// a zx::Status::IO error.
pub fn directory_open_vmo(
    directory: &fio::DirectorySynchronousProxy,
    path: &str,
    vmo_flags: fio::VmoFlags,
    deadline: zx::Time,
) -> Result<zx::Vmo, zx::Status> {
    let mut open_flags = fio::OpenFlags::empty();
    if vmo_flags.contains(fio::VmoFlags::WRITE) {
        open_flags |= fio::OpenFlags::RIGHT_WRITABLE;
    }
    if vmo_flags.contains(fio::VmoFlags::READ) {
        open_flags |= fio::OpenFlags::RIGHT_READABLE;
    }
    if vmo_flags.contains(fio::VmoFlags::EXECUTE) {
        open_flags |= fio::OpenFlags::RIGHT_EXECUTABLE;
    }

    let description = directory_open(directory, path, open_flags, deadline)?;
    let file = match description.kind {
        NodeKind::File => fio::FileSynchronousProxy::new(description.node.into_channel()),
        _ => return Err(zx::Status::IO),
    };

    let vmo = file
        .get_backing_memory(vmo_flags, deadline)
        .map_err(|_: fidl::Error| zx::Status::IO)?
        .map_err(zx::Status::from_raw)?;
    Ok(vmo)
}

/// Read the content of the file at the given path in the given directory.
///
/// If the node at the given path is not a file, then this function returns
/// a zx::Status::IO error.
pub fn directory_read_file(
    directory: &fio::DirectorySynchronousProxy,
    path: &str,
    deadline: zx::Time,
) -> Result<Vec<u8>, zx::Status> {
    let description = directory_open(directory, path, fio::OpenFlags::RIGHT_READABLE, deadline)?;
    let file = match description.kind {
        NodeKind::File => fio::FileSynchronousProxy::new(description.node.into_channel()),
        _ => return Err(zx::Status::IO),
    };

    let mut result = Vec::new();
    loop {
        let mut data = file
            .read(fio::MAX_TRANSFER_SIZE, deadline)
            .map_err(|_: fidl::Error| zx::Status::IO)?
            .map_err(zx::Status::from_raw)?;
        let finished = (data.len() as u64) < fio::MAX_TRANSFER_SIZE;
        result.append(&mut data);
        if finished {
            return Ok(result);
        }
    }
}

/// Open the given path in the given directory without blocking.
///
/// A zx::Channel to the opened node is returned (or an error).
///
/// It is an error to supply the OPEN_FLAG_DESCRIBE flag in flags.
///
/// This function will "succeed" even if the given path does not exist in the
/// given directory because this function does not wait for the directory to
/// confirm that the path exists.
pub fn directory_open_async(
    directory: &fio::DirectorySynchronousProxy,
    path: &str,
    flags: fio::OpenFlags,
) -> Result<zx::Channel, zx::Status> {
    if flags.intersects(fio::OpenFlags::DESCRIBE) {
        return Err(zx::Status::INVALID_ARGS);
    }

    let (client_end, server_end) = zx::Channel::create();
    directory
        .open(flags, fio::ModeType::empty(), path, ServerEnd::new(server_end))
        .map_err(|_| zx::Status::IO)?;
    Ok(client_end)
}

/// Open a directory at the given path in the given directory without blocking.
///
/// This function adds the OPEN_FLAG_DIRECTORY flag
/// to ensure that the open operation completes only
/// if the given path is actually a directory, which means clients can start
/// using the returned DirectorySynchronousProxy immediately without waiting
/// for the server to complete the operation.
///
/// This function will "succeed" even if the given path does not exist in the
/// given directory or if the path is not a directory because this function
/// does not wait for the directory to confirm that the path exists and is a
/// directory.
pub fn directory_open_directory_async(
    directory: &fio::DirectorySynchronousProxy,
    path: &str,
    flags: fio::OpenFlags,
) -> Result<fio::DirectorySynchronousProxy, zx::Status> {
    let flags = flags | fio::OpenFlags::DIRECTORY;
    let client = directory_open_async(directory, path, flags)?;
    Ok(fio::DirectorySynchronousProxy::new(client))
}

pub fn directory_clone(
    directory: &fio::DirectorySynchronousProxy,
    flags: fio::OpenFlags,
) -> Result<fio::DirectorySynchronousProxy, zx::Status> {
    let (client_end, server_end) = zx::Channel::create();
    directory.clone(flags, ServerEnd::new(server_end)).map_err(|_| zx::Status::IO)?;
    Ok(fio::DirectorySynchronousProxy::new(client_end))
}

pub fn file_clone(
    file: &fio::FileSynchronousProxy,
    flags: fio::OpenFlags,
) -> Result<fio::FileSynchronousProxy, zx::Status> {
    let (client_end, server_end) = zx::Channel::create();
    file.clone(flags, ServerEnd::new(server_end)).map_err(|_| zx::Status::IO)?;
    Ok(fio::FileSynchronousProxy::new(client_end))
}

#[cfg(test)]
mod test {
    use super::*;

    use anyhow::Error;
    use fidl::endpoints::Proxy;
    use fidl_fuchsia_io as fio;
    use fuchsia_async as fasync;
    use fuchsia_fs::directory;
    use fuchsia_zircon::{AsHandleRef, HandleBased};

    fn open_pkg() -> fio::DirectorySynchronousProxy {
        let pkg_proxy = directory::open_in_namespace(
            "/pkg",
            fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_EXECUTABLE,
        )
        .expect("failed to open /pkg");
        fio::DirectorySynchronousProxy::new(
            pkg_proxy
                .into_channel()
                .expect("failed to convert proxy into channel")
                .into_zx_channel(),
        )
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_directory_open() -> Result<(), Error> {
        let pkg = open_pkg();
        let description = directory_open(
            &pkg,
            "bin/syncio_lib_test",
            fio::OpenFlags::RIGHT_READABLE,
            zx::Time::INFINITE,
        )?;
        assert!(match description.kind {
            NodeKind::File => true,
            _ => false,
        });
        Ok(())
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_directory_open_vmo() -> Result<(), Error> {
        let pkg = open_pkg();
        let vmo = directory_open_vmo(
            &pkg,
            "bin/syncio_lib_test",
            fio::VmoFlags::READ | fio::VmoFlags::EXECUTE,
            zx::Time::INFINITE,
        )?;
        assert!(!vmo.is_invalid_handle());

        let info = vmo.basic_info()?;
        assert_eq!(zx::Rights::READ, info.rights & zx::Rights::READ);
        assert_eq!(zx::Rights::EXECUTE, info.rights & zx::Rights::EXECUTE);
        Ok(())
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_directory_read_file() -> Result<(), Error> {
        let pkg = open_pkg();
        let data = directory_read_file(&pkg, "bin/syncio_lib_test", zx::Time::INFINITE)?;

        assert!(!data.is_empty());
        Ok(())
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_directory_open_directory_async() -> Result<(), Error> {
        let pkg = open_pkg();
        let bin = directory_open_directory_async(
            &pkg,
            "bin",
            fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_EXECUTABLE,
        )?;
        let vmo = directory_open_vmo(
            &bin,
            "syncio_lib_test",
            fio::VmoFlags::READ | fio::VmoFlags::EXECUTE,
            zx::Time::INFINITE,
        )?;
        assert!(!vmo.is_invalid_handle());

        let info = vmo.basic_info()?;
        assert_eq!(zx::Rights::READ, info.rights & zx::Rights::READ);
        assert_eq!(zx::Rights::EXECUTE, info.rights & zx::Rights::EXECUTE);
        Ok(())
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_directory_open_zxio_async() -> Result<(), Error> {
        let pkg_proxy = directory::open_in_namespace(
            "/pkg",
            fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_EXECUTABLE,
        )
        .expect("failed to open /pkg");
        let zx_channel = pkg_proxy
            .into_channel()
            .expect("failed to convert proxy into channel")
            .into_zx_channel();
        let storage = zxio::zxio_storage_t::default();
        let status = unsafe {
            zxio::zxio_create(
                zx_channel.into_raw(),
                &storage as *const zxio::zxio_storage_t as *mut zxio::zxio_storage_t,
            )
        };
        assert_eq!(status, zx::sys::ZX_OK);
        let io = &storage.io as *const zxio::zxio_t as *mut zxio::zxio_t;
        let close_status = unsafe { zxio::zxio_close(io, true) };
        assert_eq!(close_status, zx::sys::ZX_OK);
        Ok(())
    }

    #[fuchsia::test]
    async fn test_directory_enumerate() -> Result<(), Error> {
        let pkg_dir_handle = directory::open_in_namespace(
            "/pkg",
            fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_EXECUTABLE,
        )
        .expect("failed to open /pkg")
        .into_channel()
        .expect("could not unwrap channel")
        .into_zx_channel()
        .into();

        let io: Zxio = Zxio::create(pkg_dir_handle)?;
        let iter = io.create_dirent_iterator().expect("failed to create iterator");
        let expected_dir_names = vec![".", "bin", "lib", "meta"];
        let mut found_dir_names = iter
            .map(|e| {
                let dirent = e.expect("dirent");
                assert!(dirent.is_dir());
                std::str::from_utf8(&dirent.name).expect("name was not valid utf8").to_string()
            })
            .collect::<Vec<_>>();
        found_dir_names.sort();
        assert_eq!(expected_dir_names, found_dir_names);

        // Check all entry inside bin are either "." or a file
        let bin_io = io
            .open(fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_EXECUTABLE, "bin")
            .expect("open");
        for entry in bin_io.create_dirent_iterator().expect("failed to create iterator") {
            let dirent = entry.expect("dirent");
            if dirent.name == b"." {
                assert!(dirent.is_dir());
            } else {
                assert!(dirent.is_file());
            }
        }

        Ok(())
    }

    #[fuchsia::test]
    fn test_storage_allocator() {
        let mut out_storage = zxio_storage_t::default();
        let mut out_storage_ptr = &mut out_storage as *mut zxio_storage_t;

        let mut out_context = Zxio::default();
        let mut out_context_ptr = &mut out_context as *mut Zxio;

        let out = unsafe {
            storage_allocator(
                0 as zxio_object_type_t,
                &mut out_storage_ptr as *mut *mut zxio_storage_t,
                &mut out_context_ptr as *mut *mut Zxio as *mut *mut c_void,
            )
        };
        assert_eq!(out, ZX_OK);
    }

    #[fuchsia::test]
    fn test_storage_allocator_bad_context() {
        let mut out_storage = zxio_storage_t::default();
        let mut out_storage_ptr = &mut out_storage as *mut zxio_storage_t;

        let out_context = std::ptr::null_mut();

        let out = unsafe {
            storage_allocator(
                0 as zxio_object_type_t,
                &mut out_storage_ptr as *mut *mut zxio_storage_t,
                out_context,
            )
        };
        assert_eq!(out, ZX_ERR_NO_MEMORY);
    }
}
