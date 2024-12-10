// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::common::{
    decode_extended_attribute_value, encode_extended_attribute_value, extended_attributes_sender,
    inherit_rights_for_clone, io1_to_io2_attrs,
};
use crate::execution_scope::ExecutionScope;
use crate::file::common::new_connection_validate_options;
use crate::file::{File, FileIo, FileOptions, RawFileIoConnection, SyncMode};
use crate::name::parse_name;
use crate::node::OpenNode;
use crate::object_request::Representation;
use crate::protocols::ToFileOptions;
use crate::{ObjectRequestRef, ProtocolsExt, ToObjectRequest};
use anyhow::Error;
use fidl::endpoints::ServerEnd;
use fidl_fuchsia_io as fio;
use futures::stream::StreamExt;
use static_assertions::assert_eq_size;
use std::convert::TryInto as _;
use std::future::Future;
use std::ops::{Deref, DerefMut};
use std::sync::Arc;
use storage_trace::{self as trace, TraceFutureExt};
use zx_status::Status;

#[cfg(target_os = "fuchsia")]
use {
    crate::file::common::get_backing_memory_validate_flags,
    crate::temp_clone::{unblock, TempClonable},
    std::io::SeekFrom,
    zx::{self as zx, HandleBased},
};

/// Initializes a file connection and returns a future which will process the connection.
fn create_connection<T: 'static + File, U: Deref<Target = OpenNode<T>> + DerefMut + IoOpHandler>(
    scope: ExecutionScope,
    file: U,
    options: FileOptions,
    object_request: ObjectRequestRef<'_>,
) -> Result<impl Future<Output = ()>, Status> {
    new_connection_validate_options(&options, file.readable(), file.writable(), file.executable())?;

    let object_request = object_request.take();
    Ok(async move {
        if let Err(s) = (|| async {
            file.open_file(&options).await?;

            if object_request.truncate {
                file.truncate(0).await?;
            }

            Ok(())
        })()
        .await
        {
            object_request.shutdown(s);
            return;
        }

        let connection = FileConnection { scope: scope.clone(), file, options };
        if let Ok(requests) = object_request.into_request_stream(&connection).await {
            connection.handle_requests(requests).await
        }
    })
}

/// Trait for dispatching read, write, and seek FIDL requests.
trait IoOpHandler: Send + Sync + Sized + 'static {
    /// Reads at most `count` bytes from the file starting at the connection's seek offset and
    /// advances the seek offset.
    fn read(&mut self, count: u64) -> impl Future<Output = Result<Vec<u8>, Status>> + Send;

    /// Reads `count` bytes from the file starting at `offset`.
    fn read_at(
        &self,
        offset: u64,
        count: u64,
    ) -> impl Future<Output = Result<Vec<u8>, Status>> + Send;

    /// Writes `data` to the file starting at the connect's seek offset and advances the seek
    /// offset. If the connection is in append mode then the seek offset is moved to the end of the
    /// file before writing. Returns the number of bytes written.
    fn write(&mut self, data: Vec<u8>) -> impl Future<Output = Result<u64, Status>> + Send;

    /// Writes `data` to the file starting at `offset`. Returns the number of bytes written.
    fn write_at(
        &self,
        offset: u64,
        data: Vec<u8>,
    ) -> impl Future<Output = Result<u64, Status>> + Send;

    /// Modifies the connection's seek offset. Returns the connections new seek offset.
    fn seek(
        &mut self,
        offset: i64,
        origin: fio::SeekOrigin,
    ) -> impl Future<Output = Result<u64, Status>> + Send;

    /// Notifies the `IoOpHandler` that the flags of the connection have changed.
    fn update_flags(&mut self, flags: fio::OpenFlags) -> Status;

    /// Duplicates the stream backing this connection if this connection is backed by a stream.
    /// Returns `None` if the connection is not backed by a stream.
    #[cfg(target_os = "fuchsia")]
    fn duplicate_stream(&self) -> Result<Option<zx::Stream>, Status>;

    /// Clones the connection
    fn clone_connection(&self, options: FileOptions) -> Result<Self, Status>;
}

/// Wrapper around a file that manages the seek offset of the connection and transforms `IoOpHandler`
/// requests into `FileIo` requests. All `File` requests are forwarded to `file`.
pub struct FidlIoConnection<T: 'static + File> {
    /// File that requests will be forwarded to.
    file: OpenNode<T>,

    /// Seek position. Next byte to be read or written within the buffer. This might be beyond the
    /// current size of buffer, matching POSIX:
    ///
    ///     http://pubs.opengroup.org/onlinepubs/9699919799/functions/lseek.html
    ///
    /// It will cause the buffer to be extended with zeroes (if necessary) when write() is called.
    // While the content in the buffer vector uses usize for the size, it is easier to use u64 to
    // match the FIDL bindings API. Pseudo files are not expected to cross the 2^64 bytes size
    // limit. And all the code is much simpler when we just assume that usize is the same as u64.
    // Should we need to port to a 128 bit platform, there are static assertions in the code that
    // would fail.
    seek: u64,

    /// Whether the connection is in append mode or not.
    is_append: bool,
}

impl<T: 'static + File> Deref for FidlIoConnection<T> {
    type Target = OpenNode<T>;

    fn deref(&self) -> &Self::Target {
        &self.file
    }
}

impl<T: 'static + File> DerefMut for FidlIoConnection<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.file
    }
}

impl<T: 'static + File + FileIo> FidlIoConnection<T> {
    /// Creates a connection to a file that uses FIDL for all IO.
    pub fn create(
        scope: ExecutionScope,
        file: Arc<T>,
        options: impl ToFileOptions,
        object_request: ObjectRequestRef<'_>,
    ) -> Result<impl Future<Output = ()>, Status> {
        let file = OpenNode::new(file);
        let options = options.to_file_options()?;
        create_connection(
            scope,
            FidlIoConnection { file, seek: 0, is_append: options.is_append },
            options,
            object_request,
        )
    }

    /// Like create, but spawns a task to run the connection.
    pub fn spawn(
        scope: ExecutionScope,
        file: Arc<T>,
        options: FileOptions,
        object_request: ObjectRequestRef<'_>,
    ) -> Result<(), Status> {
        object_request.take().spawn(&scope.clone(), move |object_request| {
            Box::pin(async move { Ok(Self::create(scope, file, options, object_request)?) })
        });
        Ok(())
    }
}

impl<T: 'static + File + FileIo> IoOpHandler for FidlIoConnection<T> {
    async fn read(&mut self, count: u64) -> Result<Vec<u8>, Status> {
        let buffer = self.read_at(self.seek, count).await?;
        let count: u64 = buffer.len().try_into().unwrap();
        self.seek += count;
        Ok(buffer)
    }

    async fn read_at(&self, offset: u64, count: u64) -> Result<Vec<u8>, Status> {
        let mut buffer = vec![0u8; count as usize];
        let count = self.file.read_at(offset, &mut buffer[..]).await?;
        buffer.truncate(count.try_into().unwrap());
        Ok(buffer)
    }

    async fn write(&mut self, data: Vec<u8>) -> Result<u64, Status> {
        if self.is_append {
            let (bytes, offset) = self.file.append(&data).await?;
            self.seek = offset;
            Ok(bytes)
        } else {
            let actual = self.write_at(self.seek, data).await?;
            self.seek += actual;
            Ok(actual)
        }
    }

    async fn write_at(&self, offset: u64, data: Vec<u8>) -> Result<u64, Status> {
        self.file.write_at(offset, &data).await
    }

    async fn seek(&mut self, offset: i64, origin: fio::SeekOrigin) -> Result<u64, Status> {
        // TODO(https://fxbug.dev/42061200) Use mixed_integer_ops when available.
        let new_seek = match origin {
            fio::SeekOrigin::Start => offset as i128,
            fio::SeekOrigin::Current => {
                assert_eq_size!(usize, i64);
                self.seek as i128 + offset as i128
            }
            fio::SeekOrigin::End => {
                let size = self.file.get_size().await?;
                assert_eq_size!(usize, i64, u64);
                size as i128 + offset as i128
            }
        };

        // TODO(https://fxbug.dev/42051503): There is an undocumented constraint that the seek offset can
        // never exceed 63 bits, but this is not currently enforced. For now we just ensure that
        // the values remain consistent internally with a 64-bit unsigned seek offset.
        if let Ok(new_seek) = u64::try_from(new_seek) {
            self.seek = new_seek;
            Ok(self.seek)
        } else {
            Err(Status::OUT_OF_RANGE)
        }
    }

    fn update_flags(&mut self, flags: fio::OpenFlags) -> Status {
        self.is_append = flags.intersects(fio::OpenFlags::APPEND);
        Status::OK
    }

    #[cfg(target_os = "fuchsia")]
    fn duplicate_stream(&self) -> Result<Option<zx::Stream>, Status> {
        Ok(None)
    }

    fn clone_connection(&self, options: FileOptions) -> Result<Self, Status> {
        self.file.will_clone();
        Ok(Self { file: OpenNode::new(self.file.clone()), seek: 0, is_append: options.is_append })
    }
}

pub struct RawIoConnection<T: 'static + File> {
    file: OpenNode<T>,
}

impl<T: 'static + File + RawFileIoConnection> RawIoConnection<T> {
    pub fn create(
        scope: ExecutionScope,
        file: Arc<T>,
        protocols: impl ProtocolsExt,
        object_request: ObjectRequestRef<'_>,
    ) -> Result<impl Future<Output = ()>, Status> {
        let file = OpenNode::new(file);
        create_connection(
            scope,
            RawIoConnection { file },
            protocols.to_file_options()?,
            object_request,
        )
    }
}

impl<T: 'static + File> Deref for RawIoConnection<T> {
    type Target = OpenNode<T>;

    fn deref(&self) -> &Self::Target {
        &self.file
    }
}

impl<T: 'static + File> DerefMut for RawIoConnection<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.file
    }
}

impl<T: 'static + File + RawFileIoConnection> IoOpHandler for RawIoConnection<T> {
    async fn read(&mut self, count: u64) -> Result<Vec<u8>, Status> {
        self.file.read(count).await
    }

    async fn read_at(&self, offset: u64, count: u64) -> Result<Vec<u8>, Status> {
        self.file.read_at(offset, count).await
    }

    async fn write(&mut self, data: Vec<u8>) -> Result<u64, Status> {
        self.file.write(&data).await
    }

    async fn write_at(&self, offset: u64, data: Vec<u8>) -> Result<u64, Status> {
        self.file.write_at(offset, &data).await
    }

    async fn seek(&mut self, offset: i64, origin: fio::SeekOrigin) -> Result<u64, Status> {
        self.file.seek(offset, origin).await
    }

    fn update_flags(&mut self, flags: fio::OpenFlags) -> Status {
        self.file.update_flags(flags)
    }

    #[cfg(target_os = "fuchsia")]
    fn duplicate_stream(&self) -> Result<Option<zx::Stream>, Status> {
        Ok(None)
    }

    fn clone_connection(&self, _options: FileOptions) -> Result<Self, Status> {
        self.file.will_clone();
        Ok(Self { file: OpenNode::new(self.file.clone()) })
    }
}

#[cfg(target_os = "fuchsia")]
mod stream_io {
    use super::*;
    pub trait GetVmo {
        /// True if the vmo is pager backed and the pager is serviced by the same executor as the
        /// `StreamIoConnection`.
        ///
        /// When true, stream operations that touch the contents of the vmo will be run on a
        /// separate thread pool to avoid deadlocks.
        const PAGER_ON_FIDL_EXECUTOR: bool = false;

        /// Returns the underlying VMO for the node.
        fn get_vmo(&self) -> &zx::Vmo;
    }

    /// Wrapper around a file that forwards `File` requests to `file` and
    /// `FileIo` requests to `stream`.
    pub struct StreamIoConnection<T: 'static + File + GetVmo> {
        /// File that requests will be forwarded to.
        file: OpenNode<T>,

        /// The stream backing the connection that all read, write, and seek calls are forwarded to.
        stream: TempClonable<zx::Stream>,
    }

    impl<T: 'static + File + GetVmo> Deref for StreamIoConnection<T> {
        type Target = OpenNode<T>;

        fn deref(&self) -> &Self::Target {
            &self.file
        }
    }

    impl<T: 'static + File + GetVmo> DerefMut for StreamIoConnection<T> {
        fn deref_mut(&mut self) -> &mut Self::Target {
            &mut self.file
        }
    }

    impl<T: 'static + File + GetVmo> StreamIoConnection<T> {
        /// Creates a stream-based file connection. A stream based file connection sends a zx::stream to
        /// clients that can be used for issuing read, write, and seek calls. Any read, write, and seek
        /// calls that continue to come in over FIDL will be forwarded to `stream` instead of being sent
        /// to `file`.
        pub fn create(
            scope: ExecutionScope,
            file: Arc<T>,
            options: impl ToFileOptions,
            object_request: ObjectRequestRef<'_>,
        ) -> Result<impl Future<Output = ()>, Status> {
            let file = OpenNode::new(file);
            let options = options.to_file_options()?;
            let stream = TempClonable::new(zx::Stream::create(
                options.to_stream_options(),
                file.get_vmo(),
                0,
            )?);
            create_connection(scope, StreamIoConnection { file, stream }, options, object_request)
        }

        /// Like create, but spawns a task to run the connection.
        pub fn spawn(
            scope: ExecutionScope,
            file: Arc<T>,
            options: FileOptions,
            object_request: ObjectRequestRef<'_>,
        ) -> Result<(), Status> {
            object_request.take().spawn(&scope.clone(), move |object_request| {
                Box::pin(async move { Ok(Self::create(scope, file, options, object_request)?) })
            });
            Ok(())
        }

        async fn maybe_unblock<F, R>(&self, f: F) -> R
        where
            F: FnOnce(&zx::Stream) -> R + Send + 'static,
            R: Send + 'static,
        {
            if T::PAGER_ON_FIDL_EXECUTOR {
                let stream = self.stream.temp_clone();
                unblock(move || f(&*stream)).await
            } else {
                f(&*self.stream)
            }
        }
    }

    impl<T: 'static + File + GetVmo> IoOpHandler for StreamIoConnection<T> {
        async fn read(&mut self, count: u64) -> Result<Vec<u8>, Status> {
            self.maybe_unblock(move |stream| {
                stream.read_to_vec(zx::StreamReadOptions::empty(), count as usize)
            })
            .await
        }

        async fn read_at(&self, offset: u64, count: u64) -> Result<Vec<u8>, Status> {
            self.maybe_unblock(move |stream| {
                stream.read_at_to_vec(zx::StreamReadOptions::empty(), offset, count as usize)
            })
            .await
        }

        async fn write(&mut self, data: Vec<u8>) -> Result<u64, Status> {
            self.maybe_unblock(move |stream| {
                let actual = stream.write(zx::StreamWriteOptions::empty(), &data)?;
                Ok(actual as u64)
            })
            .await
        }

        async fn write_at(&self, offset: u64, data: Vec<u8>) -> Result<u64, Status> {
            self.maybe_unblock(move |stream| {
                let actual = stream.write_at(zx::StreamWriteOptions::empty(), offset, &data)?;
                Ok(actual as u64)
            })
            .await
        }

        async fn seek(&mut self, offset: i64, origin: fio::SeekOrigin) -> Result<u64, Status> {
            let position = match origin {
                fio::SeekOrigin::Start => {
                    if offset < 0 {
                        return Err(Status::INVALID_ARGS);
                    }
                    SeekFrom::Start(offset as u64)
                }
                fio::SeekOrigin::Current => SeekFrom::Current(offset),
                fio::SeekOrigin::End => SeekFrom::End(offset),
            };
            self.stream.seek(position)
        }

        fn update_flags(&mut self, flags: fio::OpenFlags) -> Status {
            let append_mode = flags.contains(fio::OpenFlags::APPEND) as u8;
            match self.stream.set_mode_append(&append_mode) {
                Ok(()) => Status::OK,
                Err(status) => status,
            }
        }

        fn duplicate_stream(&self) -> Result<Option<zx::Stream>, Status> {
            self.stream.duplicate_handle(zx::Rights::SAME_RIGHTS).map(|s| Some(s))
        }

        fn clone_connection(&self, options: FileOptions) -> Result<Self, Status> {
            let stream = TempClonable::new(zx::Stream::create(
                options.to_stream_options(),
                self.file.get_vmo(),
                0,
            )?);
            self.file.will_clone();
            Ok(Self { file: OpenNode::new(self.file.clone()), stream })
        }
    }
}

#[cfg(target_os = "fuchsia")]
pub use stream_io::*;

/// Return type for [`handle_request()`] functions.
enum ConnectionState {
    /// Connection is still alive.
    Alive,
    /// Connection have received Node::Close message and the [`handle_close`] method has been
    /// already called for this connection.
    Closed(fio::FileCloseResponder),
    /// Connection has been dropped by the peer or an error has occurred.  [`handle_close`] still
    /// need to be called (though it would not be able to report the status to the peer).
    Dropped,
}

/// Represents a FIDL connection to a file.
struct FileConnection<U> {
    /// Execution scope this connection and any async operations and connections it creates will
    /// use.
    scope: ExecutionScope,

    /// File this connection is associated with.
    file: U,

    /// Options for this connection.
    options: FileOptions,
}

impl<T: 'static + File, U: Deref<Target = OpenNode<T>> + DerefMut + IoOpHandler> FileConnection<U> {
    async fn handle_requests(mut self, mut requests: fio::FileRequestStream) {
        while let Some(request) = requests.next().await {
            let _guard = self.scope.active_guard();

            let state = match request {
                Err(_) => {
                    // FIDL level error, such as invalid message format and alike.  Close the
                    // connection on any unexpected error.
                    // TODO: Send an epitaph.
                    ConnectionState::Dropped
                }
                Ok(request) => {
                    self.handle_request(request)
                        .await
                        // Protocol level error.  Close the connection on any unexpected error.
                        // TODO: Send an epitaph.
                        .unwrap_or(ConnectionState::Dropped)
                }
            };

            match state {
                ConnectionState::Alive => (),
                ConnectionState::Closed(responder) => {
                    async move {
                        let _ = responder.send({
                            let result = if self.options.rights.intersects(
                                fio::Operations::WRITE_BYTES | fio::Operations::UPDATE_ATTRIBUTES,
                            ) {
                                self.file.sync(SyncMode::PreClose).await.map_err(Status::into_raw)
                            } else {
                                Ok(())
                            };
                            // The file gets closed when we drop `self`, so we should do that before
                            // sending the response.
                            std::mem::drop(self);
                            result
                        });
                    }
                    .trace(trace::trace_future_args!(c"storage", c"File::Close"))
                    .await;
                    return;
                }
                ConnectionState::Dropped => break,
            }
        }

        if self
            .options
            .rights
            .intersects(fio::Operations::WRITE_BYTES | fio::Operations::UPDATE_ATTRIBUTES)
        {
            let _guard = self.scope.active_guard();
            let _ = self.file.sync(SyncMode::PreClose).await;
        }
    }

    /// Handle a [`FileRequest`]. This function is responsible for handing all the file operations
    /// that operate on the connection-specific buffer.
    async fn handle_request(&mut self, req: fio::FileRequest) -> Result<ConnectionState, Error> {
        match req {
            #[cfg(fuchsia_api_level_at_least = "NEXT")]
            fio::FileRequest::DeprecatedClone { flags, object, control_handle: _ } => {
                trace::duration!(c"storage", c"File::DeprecatedClone");
                self.handle_clone_deprecated(flags, object);
            }
            #[cfg(not(fuchsia_api_level_at_least = "NEXT"))]
            fio::FileRequest::Clone { flags, object, control_handle: _ } => {
                trace::duration!(c"storage", c"File::Clone");
                self.handle_clone_deprecated(flags, object);
            }
            fio::FileRequest::Clone2 { request, control_handle: _ } => {
                trace::duration!(c"storage", c"File::Clone2");
                self.handle_clone(ServerEnd::new(request.into_channel()));
            }
            fio::FileRequest::Close { responder } => {
                return Ok(ConnectionState::Closed(responder));
            }
            #[cfg(not(target_os = "fuchsia"))]
            fio::FileRequest::Describe { responder } => {
                responder.send(fio::FileInfo {
                    stream: None,
                    observer: self.file.event()?,
                    ..Default::default()
                })?;
            }
            #[cfg(target_os = "fuchsia")]
            fio::FileRequest::Describe { responder } => {
                trace::duration!(c"storage", c"File::Describe");
                let stream = self.file.duplicate_stream()?;
                responder.send(fio::FileInfo {
                    stream,
                    observer: self.file.event()?,
                    ..Default::default()
                })?;
            }
            fio::FileRequest::LinkInto { dst_parent_token, dst, responder } => {
                async move {
                    responder.send(
                        self.handle_link_into(dst_parent_token, dst)
                            .await
                            .map_err(Status::into_raw),
                    )
                }
                .trace(trace::trace_future_args!(c"storage", c"File::LinkInto"))
                .await?;
            }
            fio::FileRequest::GetConnectionInfo { responder } => {
                trace::duration!(c"storage", c"File::GetConnectionInfo");
                // TODO(https://fxbug.dev/293947862): Restrict GET_ATTRIBUTES.
                responder.send(fio::ConnectionInfo {
                    rights: Some(self.options.rights),
                    ..Default::default()
                })?;
            }
            fio::FileRequest::Sync { responder } => {
                async move {
                    responder.send(self.file.sync(SyncMode::Normal).await.map_err(Status::into_raw))
                }
                .trace(trace::trace_future_args!(c"storage", c"File::Sync"))
                .await?;
            }
            fio::FileRequest::GetAttr { responder } => {
                async move {
                    let (status, attrs) =
                        crate::common::io2_to_io1_attrs(self.file.as_ref(), self.options.rights)
                            .await;
                    responder.send(status.into_raw(), &attrs)
                }
                .trace(trace::trace_future_args!(c"storage", c"File::GetAttr"))
                .await?;
            }
            fio::FileRequest::SetAttr { flags, attributes, responder } => {
                async move {
                    let result =
                        self.handle_update_attributes(io1_to_io2_attrs(flags, attributes)).await;
                    responder.send(Status::from_result(result).into_raw())
                }
                .trace(trace::trace_future_args!(c"storage", c"File::SetAttr"))
                .await?;
            }
            fio::FileRequest::GetAttributes { query, responder } => {
                async move {
                    // TODO(https://fxbug.dev/293947862): Restrict GET_ATTRIBUTES.
                    let attrs = self.file.get_attributes(query).await;
                    responder.send(
                        attrs
                            .as_ref()
                            .map(|attrs| (&attrs.mutable_attributes, &attrs.immutable_attributes))
                            .map_err(|status| status.into_raw()),
                    )
                }
                .trace(trace::trace_future_args!(c"storage", c"File::GetAttributes"))
                .await?;
            }
            fio::FileRequest::UpdateAttributes { payload, responder } => {
                async move {
                    let result =
                        self.handle_update_attributes(payload).await.map_err(Status::into_raw);
                    responder.send(result)
                }
                .trace(trace::trace_future_args!(c"storage", c"File::UpdateAttributes"))
                .await?;
            }
            fio::FileRequest::ListExtendedAttributes { iterator, control_handle: _ } => {
                self.handle_list_extended_attribute(iterator)
                    .trace(trace::trace_future_args!(c"storage", c"File::ListExtendedAttributes"))
                    .await;
            }
            fio::FileRequest::GetExtendedAttribute { name, responder } => {
                async move {
                    let res =
                        self.handle_get_extended_attribute(name).await.map_err(Status::into_raw);
                    responder.send(res)
                }
                .trace(trace::trace_future_args!(c"storage", c"File::GetExtendedAttribute"))
                .await?;
            }
            fio::FileRequest::SetExtendedAttribute { name, value, mode, responder } => {
                async move {
                    let res = self
                        .handle_set_extended_attribute(name, value, mode)
                        .await
                        .map_err(Status::into_raw);
                    responder.send(res)
                }
                .trace(trace::trace_future_args!(c"storage", c"File::SetExtendedAttribute"))
                .await?;
            }
            fio::FileRequest::RemoveExtendedAttribute { name, responder } => {
                async move {
                    let res =
                        self.handle_remove_extended_attribute(name).await.map_err(Status::into_raw);
                    responder.send(res)
                }
                .trace(trace::trace_future_args!(c"storage", c"File::RemoveExtendedAttribute"))
                .await?;
            }
            #[cfg(fuchsia_api_level_at_least = "HEAD")]
            fio::FileRequest::EnableVerity { options, responder } => {
                async move {
                    let res = self.handle_enable_verity(options).await.map_err(Status::into_raw);
                    responder.send(res)
                }
                .trace(trace::trace_future_args!(c"storage", c"File::EnableVerity"))
                .await?;
            }
            fio::FileRequest::Read { count, responder } => {
                let trace_args =
                    trace::trace_future_args!(c"storage", c"File::Read", "bytes" => count);
                async move {
                    let result = self.handle_read(count).await;
                    responder.send(result.as_deref().map_err(|s| s.into_raw()))
                }
                .trace(trace_args)
                .await?;
            }
            fio::FileRequest::ReadAt { offset, count, responder } => {
                let trace_args = trace::trace_future_args!(
                    c"storage",
                    c"File::ReadAt",
                    "offset" => offset,
                    "bytes" => count
                );
                async move {
                    let result = self.handle_read_at(offset, count).await;
                    responder.send(result.as_deref().map_err(|s| s.into_raw()))
                }
                .trace(trace_args)
                .await?;
            }
            fio::FileRequest::Write { data, responder } => {
                let trace_args =
                    trace::trace_future_args!(c"storage", c"File::Write", "bytes" => data.len());
                async move {
                    let result = self.handle_write(data).await;
                    responder.send(result.map_err(Status::into_raw))
                }
                .trace(trace_args)
                .await?;
            }
            fio::FileRequest::WriteAt { offset, data, responder } => {
                let trace_args = trace::trace_future_args!(
                    c"storage",
                    c"File::WriteAt",
                    "offset" => offset,
                    "bytes" => data.len()
                );
                async move {
                    let result = self.handle_write_at(offset, data).await;
                    responder.send(result.map_err(Status::into_raw))
                }
                .trace(trace_args)
                .await?;
            }
            fio::FileRequest::Seek { origin, offset, responder } => {
                async move {
                    let result = self.handle_seek(offset, origin).await;
                    responder.send(result.map_err(Status::into_raw))
                }
                .trace(trace::trace_future_args!(c"storage", c"File::Seek"))
                .await?;
            }
            fio::FileRequest::Resize { length, responder } => {
                async move {
                    let result = self.handle_truncate(length).await;
                    responder.send(result.map_err(Status::into_raw))
                }
                .trace(trace::trace_future_args!(c"storage", c"File::Resize"))
                .await?;
            }
            fio::FileRequest::GetFlags { responder } => {
                trace::duration!(c"storage", c"File::GetFlags");
                responder.send(Status::OK.into_raw(), self.options.to_io1())?;
            }
            fio::FileRequest::SetFlags { flags, responder } => {
                trace::duration!(c"storage", c"File::SetFlags");
                self.options.is_append = flags.contains(fio::OpenFlags::APPEND);
                responder.send(self.file.update_flags(self.options.to_io1()).into_raw())?;
            }
            #[cfg(target_os = "fuchsia")]
            fio::FileRequest::GetBackingMemory { flags, responder } => {
                async move {
                    let result = self.handle_get_backing_memory(flags).await;
                    responder.send(result.map_err(Status::into_raw))
                }
                .trace(trace::trace_future_args!(c"storage", c"File::GetBackingMemory"))
                .await?;
            }

            #[cfg(not(target_os = "fuchsia"))]
            fio::FileRequest::GetBackingMemory { flags: _, responder } => {
                responder.send(Err(Status::NOT_SUPPORTED.into_raw()))?;
            }
            fio::FileRequest::AdvisoryLock { request: _, responder } => {
                trace::duration!(c"storage", c"File::AdvisoryLock");
                responder.send(Err(Status::NOT_SUPPORTED.into_raw()))?;
            }
            fio::FileRequest::Query { responder } => {
                trace::duration!(c"storage", c"File::Query");
                responder.send(fio::FILE_PROTOCOL_NAME.as_bytes())?;
            }
            fio::FileRequest::QueryFilesystem { responder } => {
                trace::duration!(c"storage", c"File::QueryFilesystem");
                match self.file.query_filesystem() {
                    Err(status) => responder.send(status.into_raw(), None)?,
                    Ok(info) => responder.send(0, Some(&info))?,
                }
            }
            #[cfg(fuchsia_api_level_at_least = "HEAD")]
            fio::FileRequest::Allocate { offset, length, mode, responder } => {
                async move {
                    let result = self.handle_allocate(offset, length, mode).await;
                    responder.send(result.map_err(Status::into_raw))
                }
                .trace(trace::trace_future_args!(c"storage", c"File::Allocate"))
                .await?;
            }
            #[cfg(fuchsia_api_level_at_least = "HEAD")]
            fio::FileRequest::GetFlags2 { responder } => {
                responder.send(Err(Status::NOT_SUPPORTED.into_raw()))?;
            }
            #[cfg(fuchsia_api_level_at_least = "HEAD")]
            fio::FileRequest::SetFlags2 { flags: _, responder } => {
                responder.send(Err(Status::NOT_SUPPORTED.into_raw()))?;
            }
            fio::FileRequest::_UnknownMethod { .. } => (),
        }
        Ok(ConnectionState::Alive)
    }

    fn handle_clone_deprecated(
        &mut self,
        flags: fio::OpenFlags,
        server_end: ServerEnd<fio::NodeMarker>,
    ) {
        flags.to_object_request(server_end).handle(|object_request| {
            let options =
                inherit_rights_for_clone(self.options.to_io1(), flags)?.to_file_options()?;

            let connection = Self {
                scope: self.scope.clone(),
                file: self.file.clone_connection(options)?,
                options,
            };

            object_request.take().spawn(&self.scope, |object_request| {
                Box::pin(async move {
                    let requests = object_request.take().into_request_stream(&connection).await?;
                    Ok(connection.handle_requests(requests))
                })
            });

            Ok(())
        });
    }

    fn handle_clone(&mut self, server_end: ServerEnd<fio::FileMarker>) {
        let connection = match self.file.clone_connection(self.options) {
            Ok(file) => Self { scope: self.scope.clone(), file, options: self.options },
            Err(status) => {
                let _ = server_end.close_with_epitaph(status);
                return;
            }
        };
        self.scope.spawn(async move {
            let requests = server_end.into_stream();
            connection.handle_requests(requests).await;
        });
    }

    async fn handle_read(&mut self, count: u64) -> Result<Vec<u8>, Status> {
        if !self.options.rights.intersects(fio::Operations::READ_BYTES) {
            return Err(Status::BAD_HANDLE);
        }

        if count > fio::MAX_TRANSFER_SIZE {
            return Err(Status::OUT_OF_RANGE);
        }
        self.file.read(count).await
    }

    async fn handle_read_at(&self, offset: u64, count: u64) -> Result<Vec<u8>, Status> {
        if !self.options.rights.intersects(fio::Operations::READ_BYTES) {
            return Err(Status::BAD_HANDLE);
        }
        if count > fio::MAX_TRANSFER_SIZE {
            return Err(Status::OUT_OF_RANGE);
        }
        self.file.read_at(offset, count).await
    }

    async fn handle_write(&mut self, content: Vec<u8>) -> Result<u64, Status> {
        if !self.options.rights.intersects(fio::Operations::WRITE_BYTES) {
            return Err(Status::BAD_HANDLE);
        }
        self.file.write(content).await
    }

    async fn handle_write_at(&self, offset: u64, content: Vec<u8>) -> Result<u64, Status> {
        if !self.options.rights.intersects(fio::Operations::WRITE_BYTES) {
            return Err(Status::BAD_HANDLE);
        }

        self.file.write_at(offset, content).await
    }

    /// Move seek position to byte `offset` relative to the origin specified by `start`.
    async fn handle_seek(&mut self, offset: i64, origin: fio::SeekOrigin) -> Result<u64, Status> {
        self.file.seek(offset, origin).await
    }

    async fn handle_update_attributes(
        &mut self,
        attributes: fio::MutableNodeAttributes,
    ) -> Result<(), Status> {
        if !self.options.rights.intersects(fio::Operations::UPDATE_ATTRIBUTES) {
            return Err(Status::BAD_HANDLE);
        }

        self.file.update_attributes(attributes).await
    }

    #[cfg(fuchsia_api_level_at_least = "HEAD")]
    async fn handle_enable_verity(
        &mut self,
        options: fio::VerificationOptions,
    ) -> Result<(), Status> {
        if !self.options.rights.intersects(fio::Operations::UPDATE_ATTRIBUTES) {
            return Err(Status::BAD_HANDLE);
        }
        self.file.enable_verity(options).await
    }

    async fn handle_truncate(&mut self, length: u64) -> Result<(), Status> {
        if !self.options.rights.intersects(fio::Operations::WRITE_BYTES) {
            return Err(Status::BAD_HANDLE);
        }

        self.file.truncate(length).await
    }

    #[cfg(target_os = "fuchsia")]
    async fn handle_get_backing_memory(&mut self, flags: fio::VmoFlags) -> Result<zx::Vmo, Status> {
        get_backing_memory_validate_flags(flags, self.options.to_io1())?;
        self.file.get_backing_memory(flags).await
    }

    async fn handle_list_extended_attribute(
        &mut self,
        iterator: ServerEnd<fio::ExtendedAttributeIteratorMarker>,
    ) {
        let attributes = match self.file.list_extended_attributes().await {
            Ok(attributes) => attributes,
            Err(status) => {
                tracing::error!(?status, "list extended attributes failed");
                iterator
                    .close_with_epitaph(status)
                    .unwrap_or_else(|error| tracing::error!(?error, "failed to send epitaph"));
                return;
            }
        };
        self.scope.spawn(extended_attributes_sender(iterator, attributes));
    }

    async fn handle_get_extended_attribute(
        &mut self,
        name: Vec<u8>,
    ) -> Result<fio::ExtendedAttributeValue, Status> {
        let value = self.file.get_extended_attribute(name).await?;
        encode_extended_attribute_value(value)
    }

    async fn handle_set_extended_attribute(
        &mut self,
        name: Vec<u8>,
        value: fio::ExtendedAttributeValue,
        mode: fio::SetExtendedAttributeMode,
    ) -> Result<(), Status> {
        if name.iter().any(|c| *c == 0) {
            return Err(Status::INVALID_ARGS);
        }
        let val = decode_extended_attribute_value(value)?;
        self.file.set_extended_attribute(name, val, mode).await
    }

    async fn handle_remove_extended_attribute(&mut self, name: Vec<u8>) -> Result<(), Status> {
        self.file.remove_extended_attribute(name).await
    }

    async fn handle_link_into(
        &mut self,
        target_parent_token: fidl::Event,
        target_name: String,
    ) -> Result<(), Status> {
        let target_name = parse_name(target_name).map_err(|_| Status::INVALID_ARGS)?;

        if !self.options.rights.contains(
            fio::Operations::READ_BYTES
                | fio::Operations::WRITE_BYTES
                | fio::Operations::GET_ATTRIBUTES
                | fio::Operations::UPDATE_ATTRIBUTES,
        ) {
            return Err(Status::ACCESS_DENIED);
        }

        let target_parent = self
            .scope
            .token_registry()
            .get_owner(target_parent_token.into())?
            .ok_or(Err(Status::NOT_FOUND))?;

        self.file.clone().link_into(target_parent, target_name).await
    }

    #[cfg(fuchsia_api_level_at_least = "HEAD")]
    async fn handle_allocate(
        &mut self,
        offset: u64,
        length: u64,
        mode: fio::AllocateMode,
    ) -> Result<(), Status> {
        self.file.allocate(offset, length, mode).await
    }
}

impl<T: 'static + File, U: Deref<Target = OpenNode<T>> + IoOpHandler> Representation
    for FileConnection<U>
{
    type Protocol = fio::FileMarker;

    async fn get_representation(
        &self,
        requested_attributes: fio::NodeAttributesQuery,
    ) -> Result<fio::Representation, Status> {
        // TODO(https://fxbug.dev/324112547): Add support for connecting as Node.
        Ok(fio::Representation::File(fio::FileInfo {
            is_append: Some(self.options.is_append),
            observer: self.file.event()?,
            #[cfg(target_os = "fuchsia")]
            stream: self.file.duplicate_stream()?,
            #[cfg(not(target_os = "fuchsia"))]
            stream: None,
            attributes: if requested_attributes.is_empty() {
                None
            } else {
                Some(self.file.get_attributes(requested_attributes).await?)
            },
            ..Default::default()
        }))
    }

    async fn node_info(&self) -> Result<fio::NodeInfoDeprecated, Status> {
        #[cfg(target_os = "fuchsia")]
        let stream = self.file.duplicate_stream()?;
        #[cfg(not(target_os = "fuchsia"))]
        let stream = None;
        Ok(fio::NodeInfoDeprecated::File(fio::FileObject { event: self.file.event()?, stream }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::directory::entry::{EntryInfo, GetEntryInfo};
    use crate::node::Node;
    use assert_matches::assert_matches;
    use futures::prelude::*;
    use std::sync::Mutex;

    const RIGHTS_R: fio::Operations =
        fio::Operations::READ_BYTES.union(fio::Operations::GET_ATTRIBUTES);
    const RIGHTS_W: fio::Operations = fio::Operations::WRITE_BYTES
        .union(fio::Operations::GET_ATTRIBUTES)
        .union(fio::Operations::UPDATE_ATTRIBUTES);
    const RIGHTS_RW: fio::Operations = fio::Operations::READ_BYTES
        .union(fio::Operations::WRITE_BYTES)
        .union(fio::Operations::GET_ATTRIBUTES)
        .union(fio::Operations::UPDATE_ATTRIBUTES);

    #[derive(Debug, PartialEq)]
    enum FileOperation {
        Init {
            options: FileOptions,
        },
        ReadAt {
            offset: u64,
            count: u64,
        },
        WriteAt {
            offset: u64,
            content: Vec<u8>,
        },
        Append {
            content: Vec<u8>,
        },
        Truncate {
            length: u64,
        },
        #[cfg(target_os = "fuchsia")]
        GetBackingMemory {
            flags: fio::VmoFlags,
        },
        GetSize,
        GetAttributes {
            query: fio::NodeAttributesQuery,
        },
        UpdateAttributes {
            attrs: fio::MutableNodeAttributes,
        },
        Close,
        Sync,
    }

    type MockCallbackType = Box<dyn Fn(&FileOperation) -> Status + Sync + Send>;
    /// A fake file that just tracks what calls `FileConnection` makes on it.
    struct MockFile {
        /// The list of operations that have been called.
        operations: Mutex<Vec<FileOperation>>,
        /// Callback used to determine how to respond to given operation.
        callback: MockCallbackType,
        /// Only used for get_size/get_attributes
        file_size: u64,
        #[cfg(target_os = "fuchsia")]
        /// VMO if using streams.
        vmo: zx::Vmo,
    }

    const MOCK_FILE_SIZE: u64 = 256;
    const MOCK_FILE_ID: u64 = 10;
    const MOCK_FILE_LINKS: u64 = 2;
    const MOCK_FILE_CREATION_TIME: u64 = 10;
    const MOCK_FILE_MODIFICATION_TIME: u64 = 100;
    impl MockFile {
        fn new(callback: MockCallbackType) -> Arc<Self> {
            Arc::new(MockFile {
                operations: Mutex::new(Vec::new()),
                callback,
                file_size: MOCK_FILE_SIZE,
                #[cfg(target_os = "fuchsia")]
                vmo: zx::Handle::invalid().into(),
            })
        }

        #[cfg(target_os = "fuchsia")]
        fn new_with_vmo(callback: MockCallbackType, vmo: zx::Vmo) -> Arc<Self> {
            Arc::new(MockFile {
                operations: Mutex::new(Vec::new()),
                callback,
                file_size: MOCK_FILE_SIZE,
                vmo,
            })
        }

        fn handle_operation(&self, operation: FileOperation) -> Result<(), Status> {
            let result = (self.callback)(&operation);
            self.operations.lock().unwrap().push(operation);
            match result {
                Status::OK => Ok(()),
                err => Err(err),
            }
        }
    }

    impl GetEntryInfo for MockFile {
        fn entry_info(&self) -> EntryInfo {
            EntryInfo::new(MOCK_FILE_ID, fio::DirentType::File)
        }
    }

    impl Node for MockFile {
        async fn get_attributes(
            &self,
            query: fio::NodeAttributesQuery,
        ) -> Result<fio::NodeAttributes2, Status> {
            self.handle_operation(FileOperation::GetAttributes { query })?;
            Ok(attributes!(
                query,
                Mutable {
                    creation_time: MOCK_FILE_CREATION_TIME,
                    modification_time: MOCK_FILE_MODIFICATION_TIME,
                },
                Immutable {
                    protocols: fio::NodeProtocolKinds::FILE,
                    abilities: fio::Operations::GET_ATTRIBUTES
                        | fio::Operations::UPDATE_ATTRIBUTES
                        | fio::Operations::READ_BYTES
                        | fio::Operations::WRITE_BYTES,
                    content_size: self.file_size,
                    storage_size: 2 * self.file_size,
                    link_count: MOCK_FILE_LINKS,
                    id: MOCK_FILE_ID,
                }
            ))
        }

        fn close(self: Arc<Self>) {
            let _ = self.handle_operation(FileOperation::Close);
        }
    }

    impl File for MockFile {
        fn writable(&self) -> bool {
            true
        }

        async fn open_file(&self, options: &FileOptions) -> Result<(), Status> {
            self.handle_operation(FileOperation::Init { options: *options })?;
            Ok(())
        }

        async fn truncate(&self, length: u64) -> Result<(), Status> {
            self.handle_operation(FileOperation::Truncate { length })
        }

        #[cfg(target_os = "fuchsia")]
        async fn get_backing_memory(&self, flags: fio::VmoFlags) -> Result<zx::Vmo, Status> {
            self.handle_operation(FileOperation::GetBackingMemory { flags })?;
            Err(Status::NOT_SUPPORTED)
        }

        async fn get_size(&self) -> Result<u64, Status> {
            self.handle_operation(FileOperation::GetSize)?;
            Ok(self.file_size)
        }

        async fn update_attributes(&self, attrs: fio::MutableNodeAttributes) -> Result<(), Status> {
            self.handle_operation(FileOperation::UpdateAttributes { attrs })?;
            Ok(())
        }

        async fn sync(&self, _mode: SyncMode) -> Result<(), Status> {
            self.handle_operation(FileOperation::Sync)
        }
    }

    impl FileIo for MockFile {
        async fn read_at(&self, offset: u64, buffer: &mut [u8]) -> Result<u64, Status> {
            let count = buffer.len() as u64;
            self.handle_operation(FileOperation::ReadAt { offset, count })?;

            // Return data as if we were a file with 0..255 repeated endlessly.
            let mut i = offset;
            buffer.fill_with(|| {
                let v = (i % 256) as u8;
                i += 1;
                v
            });
            Ok(count)
        }

        async fn write_at(&self, offset: u64, content: &[u8]) -> Result<u64, Status> {
            self.handle_operation(FileOperation::WriteAt { offset, content: content.to_vec() })?;
            Ok(content.len() as u64)
        }

        async fn append(&self, content: &[u8]) -> Result<(u64, u64), Status> {
            self.handle_operation(FileOperation::Append { content: content.to_vec() })?;
            Ok((content.len() as u64, self.file_size + content.len() as u64))
        }
    }

    #[cfg(target_os = "fuchsia")]
    impl GetVmo for MockFile {
        fn get_vmo(&self) -> &zx::Vmo {
            &self.vmo
        }
    }

    /// Only the init operation will succeed, all others fail.
    fn only_allow_init(op: &FileOperation) -> Status {
        match op {
            FileOperation::Init { .. } => Status::OK,
            _ => Status::IO,
        }
    }

    /// All operations succeed.
    fn always_succeed_callback(_op: &FileOperation) -> Status {
        Status::OK
    }

    struct TestEnv {
        pub file: Arc<MockFile>,
        pub proxy: fio::FileProxy,
        pub scope: ExecutionScope,
    }

    fn init_mock_file(callback: MockCallbackType, flags: fio::OpenFlags) -> TestEnv {
        let file = MockFile::new(callback);
        let (proxy, server_end) = fidl::endpoints::create_proxy::<fio::FileMarker>();

        let scope = ExecutionScope::new();

        flags.to_object_request(server_end).handle(|object_request| {
            object_request.spawn_connection(
                scope.clone(),
                file.clone(),
                flags,
                FidlIoConnection::create,
            )
        });

        TestEnv { file, proxy, scope }
    }

    #[fuchsia::test]
    async fn test_open_flag_truncate() {
        let env = init_mock_file(
            Box::new(always_succeed_callback),
            fio::OpenFlags::RIGHT_WRITABLE | fio::OpenFlags::TRUNCATE,
        );
        // Do a no-op sync() to make sure that the open has finished.
        let () = env.proxy.sync().await.unwrap().map_err(Status::from_raw).unwrap();
        let events = env.file.operations.lock().unwrap();
        assert_eq!(
            *events,
            vec![
                FileOperation::Init { options: FileOptions { rights: RIGHTS_W, is_append: false } },
                FileOperation::Truncate { length: 0 },
                FileOperation::Sync,
            ]
        );
    }

    #[fuchsia::test]
    async fn test_clone_same_rights() {
        let env = init_mock_file(
            Box::new(always_succeed_callback),
            fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE,
        );
        // Read from original proxy.
        let _: Vec<u8> = env.proxy.read(6).await.unwrap().map_err(Status::from_raw).unwrap();
        let (clone_proxy, remote) = fidl::endpoints::create_proxy::<fio::FileMarker>();
        env.proxy
            .deprecated_clone(fio::OpenFlags::CLONE_SAME_RIGHTS, remote.into_channel().into())
            .unwrap();
        // Seek and read from clone_proxy.
        let _: u64 = clone_proxy
            .seek(fio::SeekOrigin::Start, 100)
            .await
            .unwrap()
            .map_err(Status::from_raw)
            .unwrap();
        let _: Vec<u8> = clone_proxy.read(5).await.unwrap().map_err(Status::from_raw).unwrap();

        // Read from original proxy.
        let _: Vec<u8> = env.proxy.read(5).await.unwrap().map_err(Status::from_raw).unwrap();

        let events = env.file.operations.lock().unwrap();
        // Each connection should have an independent seek.
        assert_eq!(
            *events,
            vec![
                FileOperation::Init {
                    options: FileOptions { rights: RIGHTS_RW, is_append: false }
                },
                FileOperation::ReadAt { offset: 0, count: 6 },
                FileOperation::ReadAt { offset: 100, count: 5 },
                FileOperation::ReadAt { offset: 6, count: 5 },
            ]
        );
    }

    #[fuchsia::test]
    async fn test_close_succeeds() {
        let env = init_mock_file(Box::new(always_succeed_callback), fio::OpenFlags::RIGHT_READABLE);
        let () = env.proxy.close().await.unwrap().map_err(Status::from_raw).unwrap();

        let events = env.file.operations.lock().unwrap();
        assert_eq!(
            *events,
            vec![
                FileOperation::Init { options: FileOptions { rights: RIGHTS_R, is_append: false } },
                FileOperation::Close {},
            ]
        );
    }

    #[fuchsia::test]
    async fn test_close_fails() {
        let env = init_mock_file(
            Box::new(only_allow_init),
            fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE,
        );
        let status = env.proxy.close().await.unwrap().map_err(Status::from_raw);
        assert_eq!(status, Err(Status::IO));

        let events = env.file.operations.lock().unwrap();
        assert_eq!(
            *events,
            vec![
                FileOperation::Init {
                    options: FileOptions { rights: RIGHTS_RW, is_append: false }
                },
                FileOperation::Sync,
                FileOperation::Close,
            ]
        );
    }

    #[fuchsia::test]
    async fn test_close_called_when_dropped() {
        let env = init_mock_file(Box::new(always_succeed_callback), fio::OpenFlags::RIGHT_READABLE);
        let _ = env.proxy.sync().await;
        std::mem::drop(env.proxy);
        env.scope.shutdown();
        env.scope.wait().await;
        let events = env.file.operations.lock().unwrap();
        assert_eq!(
            *events,
            vec![
                FileOperation::Init { options: FileOptions { rights: RIGHTS_R, is_append: false } },
                FileOperation::Sync,
                FileOperation::Close,
            ]
        );
    }

    #[fuchsia::test]
    async fn test_describe() {
        let env = init_mock_file(Box::new(always_succeed_callback), fio::OpenFlags::RIGHT_READABLE);
        let protocol = env.proxy.query().await.unwrap();
        assert_eq!(protocol, fio::FILE_PROTOCOL_NAME.as_bytes());
    }

    #[fuchsia::test]
    async fn test_get_attributes() {
        let env = init_mock_file(Box::new(always_succeed_callback), fio::OpenFlags::empty());
        let (mutable_attributes, immutable_attributes) = env
            .proxy
            .get_attributes(fio::NodeAttributesQuery::all())
            .await
            .unwrap()
            .map_err(Status::from_raw)
            .unwrap();
        let expected = attributes!(
            fio::NodeAttributesQuery::all(),
            Mutable {
                creation_time: MOCK_FILE_CREATION_TIME,
                modification_time: MOCK_FILE_MODIFICATION_TIME,
            },
            Immutable {
                protocols: fio::NodeProtocolKinds::FILE,
                abilities: fio::Operations::GET_ATTRIBUTES
                    | fio::Operations::UPDATE_ATTRIBUTES
                    | fio::Operations::READ_BYTES
                    | fio::Operations::WRITE_BYTES,
                content_size: MOCK_FILE_SIZE,
                storage_size: 2 * MOCK_FILE_SIZE,
                link_count: MOCK_FILE_LINKS,
                id: MOCK_FILE_ID,
            }
        );
        assert_eq!(mutable_attributes, expected.mutable_attributes);
        assert_eq!(immutable_attributes, expected.immutable_attributes);

        let events = env.file.operations.lock().unwrap();
        assert_eq!(
            *events,
            vec![
                FileOperation::Init {
                    options: FileOptions {
                        rights: fio::Operations::GET_ATTRIBUTES,
                        is_append: false
                    }
                },
                FileOperation::GetAttributes { query: fio::NodeAttributesQuery::all() }
            ]
        );
    }

    #[fuchsia::test]
    async fn test_getbuffer() {
        let env = init_mock_file(Box::new(always_succeed_callback), fio::OpenFlags::RIGHT_READABLE);
        let result = env
            .proxy
            .get_backing_memory(fio::VmoFlags::READ)
            .await
            .unwrap()
            .map_err(Status::from_raw);
        assert_eq!(result, Err(Status::NOT_SUPPORTED));
        let events = env.file.operations.lock().unwrap();
        assert_eq!(
            *events,
            vec![
                FileOperation::Init { options: FileOptions { rights: RIGHTS_R, is_append: false } },
                #[cfg(target_os = "fuchsia")]
                FileOperation::GetBackingMemory { flags: fio::VmoFlags::READ },
            ]
        );
    }

    #[fuchsia::test]
    async fn test_getbuffer_no_perms() {
        let env = init_mock_file(Box::new(always_succeed_callback), fio::OpenFlags::empty());
        let result = env
            .proxy
            .get_backing_memory(fio::VmoFlags::READ)
            .await
            .unwrap()
            .map_err(Status::from_raw);
        // On Target this is ACCESS_DENIED, on host this is NOT_SUPPORTED
        #[cfg(target_os = "fuchsia")]
        assert_eq!(result, Err(Status::ACCESS_DENIED));
        #[cfg(not(target_os = "fuchsia"))]
        assert_eq!(result, Err(Status::NOT_SUPPORTED));
        let events = env.file.operations.lock().unwrap();
        assert_eq!(
            *events,
            vec![FileOperation::Init {
                options: FileOptions { rights: fio::Operations::GET_ATTRIBUTES, is_append: false }
            },]
        );
    }

    #[fuchsia::test]
    async fn test_getbuffer_vmo_exec_requires_right_executable() {
        let env = init_mock_file(Box::new(always_succeed_callback), fio::OpenFlags::RIGHT_READABLE);
        let result = env
            .proxy
            .get_backing_memory(fio::VmoFlags::EXECUTE)
            .await
            .unwrap()
            .map_err(Status::from_raw);
        // On Target this is ACCESS_DENIED, on host this is NOT_SUPPORTED
        #[cfg(target_os = "fuchsia")]
        assert_eq!(result, Err(Status::ACCESS_DENIED));
        #[cfg(not(target_os = "fuchsia"))]
        assert_eq!(result, Err(Status::NOT_SUPPORTED));
        let events = env.file.operations.lock().unwrap();
        assert_eq!(
            *events,
            vec![FileOperation::Init {
                options: FileOptions { rights: RIGHTS_R, is_append: false }
            },]
        );
    }

    #[fuchsia::test]
    async fn test_getflags() {
        let env = init_mock_file(
            Box::new(always_succeed_callback),
            fio::OpenFlags::RIGHT_READABLE
                | fio::OpenFlags::RIGHT_WRITABLE
                | fio::OpenFlags::TRUNCATE,
        );
        let (status, flags) = env.proxy.get_flags().await.unwrap();
        assert_eq!(Status::from_raw(status), Status::OK);
        // OPEN_FLAG_TRUNCATE should get stripped because it only applies at open time.
        assert_eq!(flags, fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE);
        let events = env.file.operations.lock().unwrap();
        assert_eq!(
            *events,
            vec![
                FileOperation::Init {
                    options: FileOptions { rights: RIGHTS_RW, is_append: false }
                },
                FileOperation::Truncate { length: 0 }
            ]
        );
    }

    #[fuchsia::test]
    async fn test_open_flag_describe() {
        let env = init_mock_file(
            Box::new(always_succeed_callback),
            fio::OpenFlags::RIGHT_READABLE
                | fio::OpenFlags::RIGHT_WRITABLE
                | fio::OpenFlags::DESCRIBE,
        );
        let event = env.proxy.take_event_stream().try_next().await.unwrap();
        match event {
            Some(fio::FileEvent::OnOpen_ { s, info: Some(boxed) }) => {
                assert_eq!(Status::from_raw(s), Status::OK);
                assert_eq!(
                    *boxed,
                    fio::NodeInfoDeprecated::File(fio::FileObject { event: None, stream: None })
                );
            }
            Some(fio::FileEvent::OnRepresentation { payload }) => {
                assert_eq!(payload, fio::Representation::File(fio::FileInfo::default()));
            }
            e => panic!("Expected OnOpen event with fio::NodeInfoDeprecated::File, got {:?}", e),
        }
        let events = env.file.operations.lock().unwrap();
        assert_eq!(
            *events,
            vec![FileOperation::Init {
                options: FileOptions { rights: RIGHTS_RW, is_append: false },
            }]
        );
    }

    #[fuchsia::test]
    async fn test_read_succeeds() {
        let env = init_mock_file(Box::new(always_succeed_callback), fio::OpenFlags::RIGHT_READABLE);
        let data = env.proxy.read(10).await.unwrap().map_err(Status::from_raw).unwrap();
        assert_eq!(data, vec![0, 1, 2, 3, 4, 5, 6, 7, 8, 9]);

        let events = env.file.operations.lock().unwrap();
        assert_eq!(
            *events,
            vec![
                FileOperation::Init { options: FileOptions { rights: RIGHTS_R, is_append: false } },
                FileOperation::ReadAt { offset: 0, count: 10 },
            ]
        );
    }

    #[fuchsia::test]
    async fn test_read_not_readable() {
        let env = init_mock_file(Box::new(only_allow_init), fio::OpenFlags::RIGHT_WRITABLE);
        let result = env.proxy.read(10).await.unwrap().map_err(Status::from_raw);
        assert_eq!(result, Err(Status::BAD_HANDLE));
    }

    #[fuchsia::test]
    async fn test_read_validates_count() {
        let env = init_mock_file(Box::new(only_allow_init), fio::OpenFlags::RIGHT_READABLE);
        let result =
            env.proxy.read(fio::MAX_TRANSFER_SIZE + 1).await.unwrap().map_err(Status::from_raw);
        assert_eq!(result, Err(Status::OUT_OF_RANGE));
    }

    #[fuchsia::test]
    async fn test_read_at_succeeds() {
        let env = init_mock_file(Box::new(always_succeed_callback), fio::OpenFlags::RIGHT_READABLE);
        let data = env.proxy.read_at(5, 10).await.unwrap().map_err(Status::from_raw).unwrap();
        assert_eq!(data, vec![10, 11, 12, 13, 14]);

        let events = env.file.operations.lock().unwrap();
        assert_eq!(
            *events,
            vec![
                FileOperation::Init { options: FileOptions { rights: RIGHTS_R, is_append: false } },
                FileOperation::ReadAt { offset: 10, count: 5 },
            ]
        );
    }

    #[fuchsia::test]
    async fn test_read_at_validates_count() {
        let env = init_mock_file(Box::new(only_allow_init), fio::OpenFlags::RIGHT_READABLE);
        let result = env
            .proxy
            .read_at(fio::MAX_TRANSFER_SIZE + 1, 0)
            .await
            .unwrap()
            .map_err(Status::from_raw);
        assert_eq!(result, Err(Status::OUT_OF_RANGE));
    }

    #[fuchsia::test]
    async fn test_seek_start() {
        let env = init_mock_file(Box::new(always_succeed_callback), fio::OpenFlags::RIGHT_READABLE);
        let offset = env
            .proxy
            .seek(fio::SeekOrigin::Start, 10)
            .await
            .unwrap()
            .map_err(Status::from_raw)
            .unwrap();
        assert_eq!(offset, 10);

        let data = env.proxy.read(1).await.unwrap().map_err(Status::from_raw).unwrap();
        assert_eq!(data, vec![10]);
        let events = env.file.operations.lock().unwrap();
        assert_eq!(
            *events,
            vec![
                FileOperation::Init { options: FileOptions { rights: RIGHTS_R, is_append: false } },
                FileOperation::ReadAt { offset: 10, count: 1 },
            ]
        );
    }

    #[fuchsia::test]
    async fn test_seek_cur() {
        let env = init_mock_file(Box::new(always_succeed_callback), fio::OpenFlags::RIGHT_READABLE);
        let offset = env
            .proxy
            .seek(fio::SeekOrigin::Start, 10)
            .await
            .unwrap()
            .map_err(Status::from_raw)
            .unwrap();
        assert_eq!(offset, 10);

        let offset = env
            .proxy
            .seek(fio::SeekOrigin::Current, -2)
            .await
            .unwrap()
            .map_err(Status::from_raw)
            .unwrap();
        assert_eq!(offset, 8);

        let data = env.proxy.read(1).await.unwrap().map_err(Status::from_raw).unwrap();
        assert_eq!(data, vec![8]);
        let events = env.file.operations.lock().unwrap();
        assert_eq!(
            *events,
            vec![
                FileOperation::Init { options: FileOptions { rights: RIGHTS_R, is_append: false } },
                FileOperation::ReadAt { offset: 8, count: 1 },
            ]
        );
    }

    #[fuchsia::test]
    async fn test_seek_before_start() {
        let env = init_mock_file(Box::new(always_succeed_callback), fio::OpenFlags::RIGHT_READABLE);
        let result =
            env.proxy.seek(fio::SeekOrigin::Current, -4).await.unwrap().map_err(Status::from_raw);
        assert_eq!(result, Err(Status::OUT_OF_RANGE));
    }

    #[fuchsia::test]
    async fn test_seek_end() {
        let env = init_mock_file(Box::new(always_succeed_callback), fio::OpenFlags::RIGHT_READABLE);
        let offset = env
            .proxy
            .seek(fio::SeekOrigin::End, -4)
            .await
            .unwrap()
            .map_err(Status::from_raw)
            .unwrap();
        assert_eq!(offset, MOCK_FILE_SIZE - 4);

        let data = env.proxy.read(1).await.unwrap().map_err(Status::from_raw).unwrap();
        assert_eq!(data, vec![(offset % 256) as u8]);
        let events = env.file.operations.lock().unwrap();
        assert_eq!(
            *events,
            vec![
                FileOperation::Init { options: FileOptions { rights: RIGHTS_R, is_append: false } },
                FileOperation::GetSize, // for the seek
                FileOperation::ReadAt { offset, count: 1 },
            ]
        );
    }

    #[fuchsia::test]
    async fn test_update_attributes() {
        let env = init_mock_file(Box::new(always_succeed_callback), fio::OpenFlags::RIGHT_WRITABLE);
        let attributes = fio::MutableNodeAttributes {
            creation_time: Some(40000),
            modification_time: Some(100000),
            mode: Some(1),
            ..Default::default()
        };
        let () = env
            .proxy
            .update_attributes(&attributes)
            .await
            .unwrap()
            .map_err(Status::from_raw)
            .unwrap();

        let events = env.file.operations.lock().unwrap();
        assert_eq!(
            *events,
            vec![
                FileOperation::Init { options: FileOptions { rights: RIGHTS_W, is_append: false } },
                FileOperation::UpdateAttributes { attrs: attributes },
            ]
        );
    }

    #[fuchsia::test]
    async fn test_set_flags() {
        let env = init_mock_file(Box::new(always_succeed_callback), fio::OpenFlags::RIGHT_WRITABLE);
        let status = env.proxy.set_flags(fio::OpenFlags::APPEND).await.unwrap();
        assert_eq!(Status::from_raw(status), Status::OK);
        let (status, flags) = env.proxy.get_flags().await.unwrap();
        assert_eq!(Status::from_raw(status), Status::OK);
        assert_eq!(flags, fio::OpenFlags::RIGHT_WRITABLE | fio::OpenFlags::APPEND);
    }

    #[fuchsia::test]
    async fn test_sync() {
        let env = init_mock_file(Box::new(always_succeed_callback), fio::OpenFlags::empty());
        let () = env.proxy.sync().await.unwrap().map_err(Status::from_raw).unwrap();
        let events = env.file.operations.lock().unwrap();
        assert_eq!(
            *events,
            vec![
                FileOperation::Init {
                    options: FileOptions {
                        rights: fio::Operations::GET_ATTRIBUTES,
                        is_append: false
                    }
                },
                FileOperation::Sync
            ]
        );
    }

    #[fuchsia::test]
    async fn test_resize() {
        let env = init_mock_file(Box::new(always_succeed_callback), fio::OpenFlags::RIGHT_WRITABLE);
        let () = env.proxy.resize(10).await.unwrap().map_err(Status::from_raw).unwrap();
        let events = env.file.operations.lock().unwrap();
        assert_matches!(
            &events[..],
            [
                FileOperation::Init { options: FileOptions { rights: RIGHTS_W, is_append: false } },
                FileOperation::Truncate { length: 10 },
            ]
        );
    }

    #[fuchsia::test]
    async fn test_resize_no_perms() {
        let env = init_mock_file(Box::new(always_succeed_callback), fio::OpenFlags::RIGHT_READABLE);
        let result = env.proxy.resize(10).await.unwrap().map_err(Status::from_raw);
        assert_eq!(result, Err(Status::BAD_HANDLE));
        let events = env.file.operations.lock().unwrap();
        assert_eq!(
            *events,
            vec![FileOperation::Init {
                options: FileOptions { rights: RIGHTS_R, is_append: false }
            },]
        );
    }

    #[fuchsia::test]
    async fn test_write() {
        let env = init_mock_file(Box::new(always_succeed_callback), fio::OpenFlags::RIGHT_WRITABLE);
        let data = "Hello, world!".as_bytes();
        let count = env.proxy.write(data).await.unwrap().map_err(Status::from_raw).unwrap();
        assert_eq!(count, data.len() as u64);
        let events = env.file.operations.lock().unwrap();
        assert_matches!(
            &events[..],
            [
                FileOperation::Init { options: FileOptions { rights: RIGHTS_W, is_append: false } },
                FileOperation::WriteAt { offset: 0, .. },
            ]
        );
        if let FileOperation::WriteAt { content, .. } = &events[1] {
            assert_eq!(content.as_slice(), data);
        } else {
            unreachable!();
        }
    }

    #[fuchsia::test]
    async fn test_write_no_perms() {
        let env = init_mock_file(Box::new(always_succeed_callback), fio::OpenFlags::RIGHT_READABLE);
        let data = "Hello, world!".as_bytes();
        let result = env.proxy.write(data).await.unwrap().map_err(Status::from_raw);
        assert_eq!(result, Err(Status::BAD_HANDLE));
        let events = env.file.operations.lock().unwrap();
        assert_eq!(
            *events,
            vec![FileOperation::Init {
                options: FileOptions { rights: RIGHTS_R, is_append: false }
            },]
        );
    }

    #[fuchsia::test]
    async fn test_write_at() {
        let env = init_mock_file(Box::new(always_succeed_callback), fio::OpenFlags::RIGHT_WRITABLE);
        let data = "Hello, world!".as_bytes();
        let count = env.proxy.write_at(data, 10).await.unwrap().map_err(Status::from_raw).unwrap();
        assert_eq!(count, data.len() as u64);
        let events = env.file.operations.lock().unwrap();
        assert_matches!(
            &events[..],
            [
                FileOperation::Init { options: FileOptions { rights: RIGHTS_W, is_append: false } },
                FileOperation::WriteAt { offset: 10, .. },
            ]
        );
        if let FileOperation::WriteAt { content, .. } = &events[1] {
            assert_eq!(content.as_slice(), data);
        } else {
            unreachable!();
        }
    }

    #[fuchsia::test]
    async fn test_append() {
        let env = init_mock_file(
            Box::new(always_succeed_callback),
            fio::OpenFlags::RIGHT_WRITABLE | fio::OpenFlags::APPEND,
        );
        let data = "Hello, world!".as_bytes();
        let count = env.proxy.write(data).await.unwrap().map_err(Status::from_raw).unwrap();
        assert_eq!(count, data.len() as u64);
        let offset = env
            .proxy
            .seek(fio::SeekOrigin::Current, 0)
            .await
            .unwrap()
            .map_err(Status::from_raw)
            .unwrap();
        assert_eq!(offset, MOCK_FILE_SIZE + data.len() as u64);
        let events = env.file.operations.lock().unwrap();
        assert_matches!(
            &events[..],
            [
                FileOperation::Init {
                    options: FileOptions { rights: RIGHTS_W, is_append: true, .. }
                },
                FileOperation::Append { .. }
            ]
        );
        if let FileOperation::Append { content } = &events[1] {
            assert_eq!(content.as_slice(), data);
        } else {
            unreachable!();
        }
    }

    #[cfg(target_os = "fuchsia")]
    mod stream_tests {
        use super::*;

        fn init_mock_stream_file(vmo: zx::Vmo, flags: fio::OpenFlags) -> TestEnv {
            let file = MockFile::new_with_vmo(Box::new(always_succeed_callback), vmo);
            let (proxy, server_end) = fidl::endpoints::create_proxy::<fio::FileMarker>();

            let scope = ExecutionScope::new();

            let cloned_file = file.clone();
            let cloned_scope = scope.clone();
            flags.to_object_request(server_end).handle(|object_request| {
                object_request.spawn_connection(
                    cloned_scope,
                    cloned_file,
                    flags,
                    StreamIoConnection::create,
                )
            });

            TestEnv { file, proxy, scope }
        }

        #[fuchsia::test]
        async fn test_stream_describe() {
            const VMO_CONTENTS: &[u8] = b"hello there";
            let vmo = zx::Vmo::create(VMO_CONTENTS.len() as u64).unwrap();
            vmo.write(VMO_CONTENTS, 0).unwrap();
            let flags = fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE;
            let env = init_mock_stream_file(vmo, flags);

            let fio::FileInfo { stream: Some(stream), .. } = env.proxy.describe().await.unwrap()
            else {
                panic!("Missing stream")
            };
            let contents =
                stream.read_to_vec(zx::StreamReadOptions::empty(), 20).expect("read failed");
            assert_eq!(contents, VMO_CONTENTS);
        }

        #[fuchsia::test]
        async fn test_stream_read() {
            let vmo_contents = [9, 8, 7, 6, 5, 4, 3, 2, 1, 0];
            let vmo = zx::Vmo::create(vmo_contents.len() as u64).unwrap();
            vmo.write(&vmo_contents, 0).unwrap();
            let flags = fio::OpenFlags::RIGHT_READABLE;
            let env = init_mock_stream_file(vmo, flags);

            let data = env
                .proxy
                .read(vmo_contents.len() as u64)
                .await
                .unwrap()
                .map_err(Status::from_raw)
                .unwrap();
            assert_eq!(data, vmo_contents);

            let events = env.file.operations.lock().unwrap();
            assert_eq!(
                *events,
                [FileOperation::Init {
                    options: FileOptions { rights: RIGHTS_R, is_append: false }
                },]
            );
        }

        #[fuchsia::test]
        async fn test_stream_read_at() {
            let vmo_contents = [9, 8, 7, 6, 5, 4, 3, 2, 1, 0];
            let vmo = zx::Vmo::create(vmo_contents.len() as u64).unwrap();
            vmo.write(&vmo_contents, 0).unwrap();
            let flags = fio::OpenFlags::RIGHT_READABLE;
            let env = init_mock_stream_file(vmo, flags);

            const OFFSET: u64 = 4;
            let data = env
                .proxy
                .read_at((vmo_contents.len() as u64) - OFFSET, OFFSET)
                .await
                .unwrap()
                .map_err(Status::from_raw)
                .unwrap();
            assert_eq!(data, vmo_contents[OFFSET as usize..]);

            let events = env.file.operations.lock().unwrap();
            assert_eq!(
                *events,
                [FileOperation::Init {
                    options: FileOptions { rights: RIGHTS_R, is_append: false }
                },]
            );
        }

        #[fuchsia::test]
        async fn test_stream_write() {
            const DATA_SIZE: u64 = 10;
            let vmo = zx::Vmo::create(DATA_SIZE).unwrap();
            let flags = fio::OpenFlags::RIGHT_WRITABLE;
            let env = init_mock_stream_file(
                vmo.duplicate_handle(zx::Rights::SAME_RIGHTS).unwrap(),
                flags,
            );

            let data: [u8; DATA_SIZE as usize] = [9, 8, 7, 6, 5, 4, 3, 2, 1, 0];
            let written = env.proxy.write(&data).await.unwrap().map_err(Status::from_raw).unwrap();
            assert_eq!(written, DATA_SIZE);
            let mut vmo_contents = [0; DATA_SIZE as usize];
            vmo.read(&mut vmo_contents, 0).unwrap();
            assert_eq!(vmo_contents, data);

            let events = env.file.operations.lock().unwrap();
            assert_eq!(
                *events,
                [FileOperation::Init {
                    options: FileOptions { rights: RIGHTS_W, is_append: false }
                },]
            );
        }

        #[fuchsia::test]
        async fn test_stream_write_at() {
            const OFFSET: u64 = 4;
            const DATA_SIZE: u64 = 10;
            let vmo = zx::Vmo::create(DATA_SIZE + OFFSET).unwrap();
            let flags = fio::OpenFlags::RIGHT_WRITABLE;
            let env = init_mock_stream_file(
                vmo.duplicate_handle(zx::Rights::SAME_RIGHTS).unwrap(),
                flags,
            );

            let data: [u8; DATA_SIZE as usize] = [9, 8, 7, 6, 5, 4, 3, 2, 1, 0];
            let written =
                env.proxy.write_at(&data, OFFSET).await.unwrap().map_err(Status::from_raw).unwrap();
            assert_eq!(written, DATA_SIZE);
            let mut vmo_contents = [0; DATA_SIZE as usize];
            vmo.read(&mut vmo_contents, OFFSET).unwrap();
            assert_eq!(vmo_contents, data);

            let events = env.file.operations.lock().unwrap();
            assert_eq!(
                *events,
                [FileOperation::Init {
                    options: FileOptions { rights: RIGHTS_W, is_append: false }
                }]
            );
        }

        #[fuchsia::test]
        async fn test_stream_seek() {
            let vmo_contents = [9, 8, 7, 6, 5, 4, 3, 2, 1, 0];
            let vmo = zx::Vmo::create(vmo_contents.len() as u64).unwrap();
            vmo.write(&vmo_contents, 0).unwrap();
            let flags = fio::OpenFlags::RIGHT_READABLE;
            let env = init_mock_stream_file(vmo, flags);

            let position = env
                .proxy
                .seek(fio::SeekOrigin::Start, 8)
                .await
                .unwrap()
                .map_err(Status::from_raw)
                .unwrap();
            assert_eq!(position, 8);
            let data = env.proxy.read(2).await.unwrap().map_err(Status::from_raw).unwrap();
            assert_eq!(data, [1, 0]);

            let position = env
                .proxy
                .seek(fio::SeekOrigin::Current, -4)
                .await
                .unwrap()
                .map_err(Status::from_raw)
                .unwrap();
            // Seeked to 8, read 2, seeked backwards 4. 8 + 2 - 4 = 6.
            assert_eq!(position, 6);
            let data = env.proxy.read(2).await.unwrap().map_err(Status::from_raw).unwrap();
            assert_eq!(data, [3, 2]);

            let position = env
                .proxy
                .seek(fio::SeekOrigin::End, -6)
                .await
                .unwrap()
                .map_err(Status::from_raw)
                .unwrap();
            assert_eq!(position, 4);
            let data = env.proxy.read(2).await.unwrap().map_err(Status::from_raw).unwrap();
            assert_eq!(data, [5, 4]);

            let e = env
                .proxy
                .seek(fio::SeekOrigin::Start, -1)
                .await
                .unwrap()
                .map_err(Status::from_raw)
                .expect_err("Seeking before the start of a file should be an error");
            assert_eq!(e, Status::INVALID_ARGS);
        }

        #[fuchsia::test]
        async fn test_stream_set_flags() {
            let data = [0, 1, 2, 3, 4];
            let vmo = zx::Vmo::create_with_opts(zx::VmoOptions::RESIZABLE, 100).unwrap();
            let flags = fio::OpenFlags::RIGHT_WRITABLE;
            let env = init_mock_stream_file(
                vmo.duplicate_handle(zx::Rights::SAME_RIGHTS).unwrap(),
                flags,
            );

            let written = env.proxy.write(&data).await.unwrap().map_err(Status::from_raw).unwrap();
            assert_eq!(written, data.len() as u64);
            // Data was not appended.
            assert_eq!(vmo.get_content_size().unwrap(), 100);

            // Switch to append mode.
            zx::ok(env.proxy.set_flags(fio::OpenFlags::APPEND).await.unwrap()).unwrap();
            env.proxy
                .seek(fio::SeekOrigin::Start, 0)
                .await
                .unwrap()
                .map_err(Status::from_raw)
                .unwrap();
            let written = env.proxy.write(&data).await.unwrap().map_err(Status::from_raw).unwrap();
            assert_eq!(written, data.len() as u64);
            // Data was appended.
            assert_eq!(vmo.get_content_size().unwrap(), 105);

            // Switch out of append mode.
            zx::ok(env.proxy.set_flags(fio::OpenFlags::empty()).await.unwrap()).unwrap();
            env.proxy
                .seek(fio::SeekOrigin::Start, 0)
                .await
                .unwrap()
                .map_err(Status::from_raw)
                .unwrap();
            let written = env.proxy.write(&data).await.unwrap().map_err(Status::from_raw).unwrap();
            assert_eq!(written, data.len() as u64);
            // Data was not appended.
            assert_eq!(vmo.get_content_size().unwrap(), 105);
        }

        #[fuchsia::test]
        async fn test_stream_read_validates_count() {
            let vmo = zx::Vmo::create(10).unwrap();
            let flags = fio::OpenFlags::RIGHT_READABLE;
            let env = init_mock_stream_file(vmo, flags);
            let result =
                env.proxy.read(fio::MAX_TRANSFER_SIZE + 1).await.unwrap().map_err(Status::from_raw);
            assert_eq!(result, Err(Status::OUT_OF_RANGE));
        }

        #[fuchsia::test]
        async fn test_stream_read_at_validates_count() {
            let vmo = zx::Vmo::create(10).unwrap();
            let flags = fio::OpenFlags::RIGHT_READABLE;
            let env = init_mock_stream_file(vmo, flags);
            let result = env
                .proxy
                .read_at(fio::MAX_TRANSFER_SIZE + 1, 0)
                .await
                .unwrap()
                .map_err(Status::from_raw);
            assert_eq!(result, Err(Status::OUT_OF_RANGE));
        }
    }
}
