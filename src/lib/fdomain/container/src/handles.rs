// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use zx::{AsHandleRef, HandleBased, Peered};
use {fidl_fuchsia_fdomain as proto, fuchsia_zircon as zx};

/// Amount of buffer space we allocate for reading from handles in order to
/// serve read requests.
const READ_BUFFER_SIZE: usize = 4096;

fn convert_object_type(ty: zx::ObjectType) -> proto::ObjType {
    match ty {
        zx::ObjectType::NONE => proto::ObjType::None,
        zx::ObjectType::PROCESS => proto::ObjType::Process,
        zx::ObjectType::THREAD => proto::ObjType::Thread,
        zx::ObjectType::VMO => proto::ObjType::Vmo,
        zx::ObjectType::CHANNEL => proto::ObjType::Channel,
        zx::ObjectType::EVENT => proto::ObjType::Event,
        zx::ObjectType::PORT => proto::ObjType::Port,
        zx::ObjectType::INTERRUPT => proto::ObjType::Interrupt,
        zx::ObjectType::PCI_DEVICE => proto::ObjType::PciDevice,
        zx::ObjectType::DEBUGLOG => proto::ObjType::Debuglog,
        zx::ObjectType::SOCKET => proto::ObjType::Socket,
        zx::ObjectType::RESOURCE => proto::ObjType::Resource,
        zx::ObjectType::EVENTPAIR => proto::ObjType::Eventpair,
        zx::ObjectType::JOB => proto::ObjType::Job,
        zx::ObjectType::VMAR => proto::ObjType::Vmar,
        zx::ObjectType::FIFO => proto::ObjType::Fifo,
        zx::ObjectType::GUEST => proto::ObjType::Guest,
        zx::ObjectType::VCPU => proto::ObjType::Vcpu,
        zx::ObjectType::TIMER => proto::ObjType::Timer,
        zx::ObjectType::IOMMU => proto::ObjType::Iommu,
        zx::ObjectType::BTI => proto::ObjType::Bti,
        zx::ObjectType::PROFILE => proto::ObjType::Profile,
        zx::ObjectType::PMT => proto::ObjType::Pmt,
        zx::ObjectType::SUSPEND_TOKEN => proto::ObjType::SuspendToken,
        zx::ObjectType::PAGER => proto::ObjType::Pager,
        zx::ObjectType::EXCEPTION => proto::ObjType::Exception,
        zx::ObjectType::CLOCK => proto::ObjType::Clock,
        zx::ObjectType::STREAM => proto::ObjType::Stream,
        zx::ObjectType::MSI => proto::ObjType::Msi,
        zx::ObjectType::IOB => proto::ObjType::Iob,
        _ => unreachable!("Unknown object type!"),
    }
}

/// This is implemented on the `zx::*` objects for every type of handle FDomain
/// supports. It essentially makes the handle object a responder to a stream of
/// [`HandleOperation`]s.
pub trait HandleType: Sync + Sized + Into<zx::Handle> + zx::AsHandleRef + 'static {
    /// This should be the handle type corresponding to the implementing handle.
    /// We use this to generalize some of the error reporting in this trait.
    fn zx_type(&self) -> zx::ObjectType;

    fn proto_type(&self) -> proto::ObjType {
        convert_object_type(self.zx_type())
    }

    /// Returns `Ok` if this handle is the given type, `Err` otherwise.
    fn expected_type(&self, expected_type: zx::ObjectType) -> Result<(), proto::Error> {
        if expected_type == self.zx_type() {
            Ok(())
        } else {
            Err(proto::Error::WrongHandleType(proto::WrongHandleType {
                expected: convert_object_type(expected_type),
                got: self.proto_type(),
            }))
        }
    }

    /// Implements [`HandleOperation::SocketDisposition`].
    fn socket_disposition(
        &self,
        _disposition: proto::SocketDisposition,
        _disposition_peer: proto::SocketDisposition,
    ) -> Result<(), proto::Error> {
        self.expected_type(zx::ObjectType::SOCKET)
    }

    /// Implements [`HandleOperation::ReadSocket`].
    fn read_socket(&self, _max_bytes: u64) -> Result<Option<Vec<u8>>, proto::Error> {
        Err(self.expected_type(zx::ObjectType::SOCKET).unwrap_err())
    }

    /// Implements [`HandleOperation::ReadChannel`].
    fn read_channel(&self) -> Result<Option<zx::MessageBufEtc>, proto::Error> {
        Err(self.expected_type(zx::ObjectType::CHANNEL).unwrap_err())
    }

    /// Implements [`HandleOperation::WriteSocket`].
    fn write_socket(&self, _data: &[u8]) -> Result<usize, proto::Error> {
        Err(self.expected_type(zx::ObjectType::SOCKET).unwrap_err())
    }

    /// Implements [`HandleOperation::WriteChannel`].
    fn write_channel(
        &self,
        _data: &[u8],
        _handles: &mut Vec<zx::HandleDisposition<'static>>,
    ) -> Option<Result<(), proto::WriteChannelError>> {
        Some(Err(proto::WriteChannelError::Error(
            self.expected_type(zx::ObjectType::CHANNEL).unwrap_err(),
        )))
    }
}

impl HandleType for zx::Socket {
    fn zx_type(&self) -> zx::ObjectType {
        zx::ObjectType::SOCKET
    }

    fn socket_disposition(
        &self,
        disposition: proto::SocketDisposition,
        disposition_peer: proto::SocketDisposition,
    ) -> Result<(), proto::Error> {
        fn map_disposition(
            disposition: proto::SocketDisposition,
        ) -> Result<Option<zx::SocketWriteDisposition>, proto::Error> {
            match disposition {
                proto::SocketDisposition::NoChange => Ok(None),
                proto::SocketDisposition::WriteDisabled => {
                    Ok(Some(zx::SocketWriteDisposition::Disabled))
                }
                proto::SocketDisposition::WriteEnabled => {
                    Ok(Some(zx::SocketWriteDisposition::Enabled))
                }
                disposition => {
                    Err(proto::Error::SocketDispositionUnknown(proto::SocketDispositionUnknown {
                        disposition,
                    }))
                }
            }
        }

        self.set_disposition(map_disposition(disposition)?, map_disposition(disposition_peer)?)
            .map_err(|e| proto::Error::TargetError(e.into_raw()))
    }

    fn read_socket(&self, max_bytes: u64) -> Result<Option<Vec<u8>>, proto::Error> {
        let mut buf = [0u8; READ_BUFFER_SIZE];
        let buf = if max_bytes < READ_BUFFER_SIZE as u64 {
            &mut buf[..max_bytes as usize]
        } else {
            &mut buf
        };
        match self.read(buf) {
            Ok(size) => Ok(Some(buf[..size].to_vec())),
            Err(zx::Status::SHOULD_WAIT) => Ok(None),
            Err(other) => Err(proto::Error::TargetError(other.into_raw())),
        }
    }

    fn write_socket(&self, data: &[u8]) -> Result<usize, proto::Error> {
        let mut wrote = 0;
        loop {
            match self.write(&data[wrote..]) {
                Ok(count) => {
                    wrote += count;

                    if wrote >= data.len() {
                        break Ok(wrote);
                    }
                }
                Err(zx::Status::SHOULD_WAIT) => break Ok(wrote),

                Err(other) => break Err(proto::Error::TargetError(other.into_raw())),
            }
        }
    }
}

impl HandleType for zx::Channel {
    fn zx_type(&self) -> zx::ObjectType {
        zx::ObjectType::CHANNEL
    }

    fn read_channel(&self) -> Result<Option<zx::MessageBufEtc>, proto::Error> {
        let mut buf = zx::MessageBufEtc::new();
        match self.read_etc(&mut buf) {
            Err(zx::Status::SHOULD_WAIT) => Ok(None),
            other => other.map(|_| Some(buf)).map_err(|e| proto::Error::TargetError(e.into_raw())),
        }
    }

    fn write_channel(
        &self,
        data: &[u8],
        handles: &mut Vec<zx::HandleDisposition<'static>>,
    ) -> Option<Result<(), proto::WriteChannelError>> {
        match self.write_etc(data, handles) {
            Ok(()) => Some(Ok(())),
            Err(zx::Status::SHOULD_WAIT) => None,
            Err(other) => {
                if handles.iter().any(|x| x.result != zx::Status::OK) {
                    Some(Err(proto::WriteChannelError::OpErrors(
                        handles
                            .into_iter()
                            .map(|x| {
                                Result::from(x.result)
                                    .err()
                                    .map(|e| Box::new(proto::Error::TargetError(e.into_raw())))
                            })
                            .collect(),
                    )))
                } else {
                    Some(Err(proto::WriteChannelError::Error(proto::Error::TargetError(
                        other.into_raw(),
                    ))))
                }
            }
        }
    }
}

impl HandleType for zx::EventPair {
    fn zx_type(&self) -> zx::ObjectType {
        zx::ObjectType::EVENTPAIR
    }
}

impl HandleType for zx::Event {
    fn zx_type(&self) -> zx::ObjectType {
        zx::ObjectType::EVENT
    }
}

pub struct Unknown(pub zx::Handle, pub zx::ObjectType);

impl Into<zx::Handle> for Unknown {
    fn into(self) -> zx::Handle {
        self.0
    }
}

impl zx::AsHandleRef for Unknown {
    fn as_handle_ref(&self) -> zx::HandleRef<'_> {
        self.0.as_handle_ref()
    }
}

impl HandleType for Unknown {
    fn zx_type(&self) -> zx::ObjectType {
        self.1
    }
}

pub enum AnyHandle {
    Socket(zx::Socket),
    EventPair(zx::EventPair),
    Event(zx::Event),
    Channel(zx::Channel),
    Unknown(Unknown),
}

impl AnyHandle {
    /// Signals that indicate we should do write processing on a handle.
    pub(crate) fn write_signals(&self) -> fidl::Signals {
        let sock_signals = if let AnyHandle::Socket(_) = self {
            fidl::Signals::SOCKET_WRITE_DISABLED
        } else {
            fidl::Signals::empty()
        };

        sock_signals | fidl::Signals::OBJECT_WRITABLE | fidl::Signals::OBJECT_PEER_CLOSED
    }

    /// Signals that indicate we should do write processing on a handle.
    pub(crate) fn read_signals(&self) -> fidl::Signals {
        let sock_signals = if let AnyHandle::Socket(_) = self {
            fidl::Signals::SOCKET_PEER_WRITE_DISABLED
        } else {
            fidl::Signals::empty()
        };

        sock_signals | fidl::Signals::OBJECT_READABLE | fidl::Signals::OBJECT_PEER_CLOSED
    }

    /// zx_handle_duplicate but preserving our metadata.
    pub fn duplicate(&self, rights: zx::Rights) -> Result<AnyHandle, proto::Error> {
        let handle = self
            .as_handle_ref()
            .duplicate(rights)
            .map_err(|e| proto::Error::TargetError(e.into_raw()))?;

        Ok(match self {
            AnyHandle::Socket(_) => AnyHandle::Socket(zx::Socket::from(handle)),
            AnyHandle::EventPair(_) => AnyHandle::EventPair(zx::EventPair::from(handle)),
            AnyHandle::Event(_) => AnyHandle::Event(zx::Event::from(handle)),
            AnyHandle::Channel(_) => AnyHandle::Channel(zx::Channel::from(handle)),
            AnyHandle::Unknown(Unknown(_, ty)) => AnyHandle::Unknown(Unknown(handle, *ty)),
        })
    }

    /// zx_handle_replace but preserving our metadata.
    pub fn replace(self, rights: zx::Rights) -> Result<AnyHandle, proto::Error> {
        Ok(match self {
            AnyHandle::Socket(h) => AnyHandle::Socket(
                h.replace_handle(rights).map_err(|e| proto::Error::TargetError(e.into_raw()))?,
            ),
            AnyHandle::EventPair(h) => AnyHandle::EventPair(
                h.replace_handle(rights).map_err(|e| proto::Error::TargetError(e.into_raw()))?,
            ),
            AnyHandle::Event(h) => AnyHandle::Event(
                h.replace_handle(rights).map_err(|e| proto::Error::TargetError(e.into_raw()))?,
            ),
            AnyHandle::Channel(h) => AnyHandle::Channel(
                h.replace_handle(rights).map_err(|e| proto::Error::TargetError(e.into_raw()))?,
            ),
            AnyHandle::Unknown(Unknown(h, ty)) => AnyHandle::Unknown(Unknown(
                h.replace(rights).map_err(|e| proto::Error::TargetError(e.into_raw()))?,
                ty,
            )),
        })
    }

    pub fn signal_peer(&self, clear: zx::Signals, set: zx::Signals) -> Result<(), proto::Error> {
        // TODO: Rust is being helpful and only letting us signal peers for
        // things we know have them, but we'd really like to just make the
        // syscall and get it to report whatever error it will. Especially for
        // `Unknown`, which may well have peers the type system just isn't aware
        // of.
        match self {
            AnyHandle::Socket(h) => h.signal_peer(clear, set),
            AnyHandle::EventPair(h) => h.signal_peer(clear, set),
            AnyHandle::Event(_) => Err(zx::Status::INVALID_ARGS),
            AnyHandle::Channel(h) => h.signal_peer(clear, set),
            AnyHandle::Unknown(_) => Err(zx::Status::INVALID_ARGS),
        }
        .map_err(|e| proto::Error::TargetError(e.into_raw()))
    }
}

impl From<zx::Channel> for AnyHandle {
    fn from(other: zx::Channel) -> AnyHandle {
        AnyHandle::Channel(other)
    }
}

impl From<zx::Socket> for AnyHandle {
    fn from(other: zx::Socket) -> AnyHandle {
        AnyHandle::Socket(other)
    }
}

impl From<zx::EventPair> for AnyHandle {
    fn from(other: zx::EventPair) -> AnyHandle {
        AnyHandle::EventPair(other)
    }
}

impl From<zx::Event> for AnyHandle {
    fn from(other: zx::Event) -> AnyHandle {
        AnyHandle::Event(other)
    }
}

macro_rules! impl_method {
    ($this:ident => $h:ident . $meth:ident ( $($args:tt)* )) => {
        match $this {
            AnyHandle::Socket($h) => $h.$meth($($args)*),
            AnyHandle::EventPair($h) => $h.$meth($($args)*),
            AnyHandle::Event($h) => $h.$meth($($args)*),
            AnyHandle::Channel($h) => $h.$meth($($args)*),
            AnyHandle::Unknown($h) => $h.$meth($($args)*),
        }
    };
}

impl HandleType for AnyHandle {
    fn zx_type(&self) -> zx::ObjectType {
        impl_method!(self => h.zx_type())
    }

    fn socket_disposition(
        &self,
        disposition: proto::SocketDisposition,
        disposition_peer: proto::SocketDisposition,
    ) -> Result<(), proto::Error> {
        impl_method!(self => h.socket_disposition(disposition, disposition_peer))
    }

    fn read_socket(&self, max_bytes: u64) -> Result<Option<Vec<u8>>, proto::Error> {
        impl_method!(self => h.read_socket(max_bytes))
    }

    fn read_channel(&self) -> Result<Option<zx::MessageBufEtc>, proto::Error> {
        impl_method!(self => h.read_channel())
    }

    fn write_socket(&self, data: &[u8]) -> Result<usize, proto::Error> {
        impl_method!(self => h.write_socket(data))
    }

    fn write_channel(
        &self,
        data: &[u8],
        handles: &mut Vec<zx::HandleDisposition<'static>>,
    ) -> Option<Result<(), proto::WriteChannelError>> {
        impl_method!(self => h.write_channel(data, handles))
    }
}

impl zx::AsHandleRef for AnyHandle {
    fn as_handle_ref(&self) -> zx::HandleRef<'_> {
        impl_method!(self => h.as_handle_ref())
    }
}

impl Into<zx::Handle> for AnyHandle {
    fn into(self) -> zx::Handle {
        impl_method!(self => h.into())
    }
}
