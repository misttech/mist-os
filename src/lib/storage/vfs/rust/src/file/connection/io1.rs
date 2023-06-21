// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{
        common::{
            decode_extended_attribute_value, encode_extended_attribute_value,
            extended_attributes_sender, inherit_rights_for_clone, send_on_open_with_error,
        },
        directory::entry::DirectoryEntry,
        execution_scope::ExecutionScope,
        file::{
            common::{get_backing_memory_validate_flags, new_connection_validate_options},
            File, FileIo, FileOptions, RawFileIoConnection, SyncMode,
        },
        node::OpenNode,
        object_request::Representation,
        path::Path,
        ObjectRequestRef, ProtocolsExt,
    },
    anyhow::Error,
    async_trait::async_trait,
    fidl::endpoints::ServerEnd,
    fidl_fuchsia_io as fio,
    fuchsia_zircon::{
        self as zx,
        sys::{ZX_ERR_NOT_SUPPORTED, ZX_OK},
        HandleBased, Status,
    },
    futures::stream::StreamExt,
    static_assertions::assert_eq_size,
    std::{
        convert::TryInto as _,
        future::Future,
        io::SeekFrom,
        marker::{Send, Sync},
        ops::{Deref, DerefMut},
        sync::Arc,
    },
};

/// Initializes a file connection and returns a future which will process the connection.
fn create_connection<T: 'static + File, U: Deref<Target = OpenNode<T>> + DerefMut + IoOpHandler>(
    scope: ExecutionScope,
    file: U,
    options: FileOptions,
    object_request: ObjectRequestRef,
) -> Result<impl Future<Output = ()>, zx::Status> {
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
#[async_trait]
trait IoOpHandler: Send + Sync {
    /// Reads at most `count` bytes from the file starting at the connection's seek offset and
    /// advances the seek offset.
    async fn read(&mut self, count: u64) -> Result<Vec<u8>, zx::Status>;

    /// Reads `count` bytes from the file starting at `offset`.
    async fn read_at(&self, offset: u64, count: u64) -> Result<Vec<u8>, zx::Status>;

    /// Writes `data` to the file starting at the connect's seek offset and advances the seek
    /// offset. If the connection is in append mode then the seek offset is moved to the end of the
    /// file before writing. Returns the number of bytes written.
    async fn write(&mut self, data: &[u8]) -> Result<u64, zx::Status>;

    /// Writes `data` to the file starting at `offset`. Returns the number of bytes written.
    async fn write_at(&self, offset: u64, data: &[u8]) -> Result<u64, zx::Status>;

    /// Modifies the connection's seek offset. Returns the connections new seek offset.
    async fn seek(&mut self, offset: i64, origin: fio::SeekOrigin) -> Result<u64, zx::Status>;

    /// Notifies the `IoOpHandler` that the flags of the connection have changed.
    fn update_flags(&mut self, flags: fio::OpenFlags) -> zx::Status;

    /// Duplicates the stream backing this connection if this connection is backed by a stream.
    /// Returns `None` if the connection is not backed by a stream.
    fn duplicate_stream(&self) -> Result<Option<zx::Stream>, zx::Status>;
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

impl<T: 'static + DirectoryEntry + File + FileIo> FidlIoConnection<T> {
    /// Creates a connection to a file that uses FIDL for all IO.
    pub fn create(
        scope: ExecutionScope,
        file: Arc<T>,
        protocols: impl ProtocolsExt,
        object_request: ObjectRequestRef,
    ) -> Result<impl Future<Output = ()>, zx::Status> {
        let file = OpenNode::new(file);
        let options = protocols.to_file_options()?;
        create_connection(
            scope,
            FidlIoConnection { file, seek: 0, is_append: options.is_append },
            options,
            object_request,
        )
    }
}

#[async_trait]
impl<T: 'static + File + FileIo> IoOpHandler for FidlIoConnection<T> {
    async fn read(&mut self, count: u64) -> Result<Vec<u8>, zx::Status> {
        let buffer = self.read_at(self.seek, count).await?;
        let count: u64 = buffer.len().try_into().unwrap();
        self.seek += count;
        Ok(buffer)
    }

    async fn read_at(&self, offset: u64, count: u64) -> Result<Vec<u8>, zx::Status> {
        let mut buffer = vec![0u8; count as usize];
        let count = self.file.read_at(offset, &mut buffer[..]).await?;
        buffer.truncate(count.try_into().unwrap());
        Ok(buffer)
    }

    async fn write(&mut self, data: &[u8]) -> Result<u64, zx::Status> {
        if self.is_append {
            let (bytes, offset) = self.file.append(data).await?;
            self.seek = offset;
            Ok(bytes)
        } else {
            let actual = self.write_at(self.seek, data).await?;
            self.seek += actual;
            Ok(actual)
        }
    }

    async fn write_at(&self, offset: u64, data: &[u8]) -> Result<u64, zx::Status> {
        self.file.write_at(offset, &data).await
    }

    async fn seek(&mut self, offset: i64, origin: fio::SeekOrigin) -> Result<u64, zx::Status> {
        // TODO(fxbug.dev/109832) Use mixed_integer_ops when available.
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

        // TODO(fxbug.dev/100754): There is an undocumented constraint that the seek offset can
        // never exceed 63 bits, but this is not currently enforced. For now we just ensure that
        // the values remain consistent internally with a 64-bit unsigned seek offset.
        if let Ok(new_seek) = u64::try_from(new_seek) {
            self.seek = new_seek;
            Ok(self.seek)
        } else {
            Err(zx::Status::OUT_OF_RANGE)
        }
    }

    fn update_flags(&mut self, flags: fio::OpenFlags) -> zx::Status {
        self.is_append = flags.intersects(fio::OpenFlags::APPEND);
        zx::Status::OK
    }

    fn duplicate_stream(&self) -> Result<Option<zx::Stream>, zx::Status> {
        Ok(None)
    }
}

pub struct RawIoConnection<T: 'static + File> {
    file: OpenNode<T>,
}

impl<T: 'static + File + RawFileIoConnection + DirectoryEntry> RawIoConnection<T> {
    pub fn create(
        scope: ExecutionScope,
        file: Arc<T>,
        protocols: impl ProtocolsExt,
        object_request: ObjectRequestRef,
    ) -> Result<impl Future<Output = ()>, zx::Status> {
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

#[async_trait]
impl<T: 'static + File + RawFileIoConnection + DirectoryEntry> IoOpHandler for RawIoConnection<T> {
    async fn read(&mut self, count: u64) -> Result<Vec<u8>, zx::Status> {
        self.file.read(count).await
    }

    async fn read_at(&self, offset: u64, count: u64) -> Result<Vec<u8>, zx::Status> {
        self.file.read_at(offset, count).await
    }

    async fn write(&mut self, data: &[u8]) -> Result<u64, zx::Status> {
        self.file.write(data).await
    }

    async fn write_at(&self, offset: u64, data: &[u8]) -> Result<u64, zx::Status> {
        self.file.write_at(offset, data).await
    }

    async fn seek(&mut self, offset: i64, origin: fio::SeekOrigin) -> Result<u64, zx::Status> {
        self.file.seek(offset, origin).await
    }

    fn update_flags(&mut self, flags: fio::OpenFlags) -> zx::Status {
        self.file.update_flags(flags)
    }

    fn duplicate_stream(&self) -> Result<Option<zx::Stream>, zx::Status> {
        Ok(None)
    }
}

pub trait GetVmo {
    /// Returns the underlying VMO for the node.
    fn get_vmo(&self) -> &zx::Vmo;
}

/// Wrapper around a file that forwards `File` requests to `file` and `FileIo` requests to `stream`.
pub struct StreamIoConnection<T: 'static + File> {
    /// File that requests will be forwarded to.
    file: OpenNode<T>,

    /// The stream backing the connection that all read, write, and seek calls are forwarded to.
    stream: zx::Stream,
}

impl<T: 'static + File> Deref for StreamIoConnection<T> {
    type Target = OpenNode<T>;

    fn deref(&self) -> &Self::Target {
        &self.file
    }
}

impl<T: 'static + File> DerefMut for StreamIoConnection<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.file
    }
}

impl<T: 'static + File + DirectoryEntry> StreamIoConnection<T> {
    /// Creates a stream-based file connection. A stream based file connection sends a zx::stream to
    /// clients that can be used for issuing read, write, and seek calls. Any read, write, and seek
    /// calls that continue to come in over FIDL will be forwarded to `stream` instead of being sent
    /// to `file`.
    pub fn create(
        scope: ExecutionScope,
        file: Arc<T>,
        protocols: impl ProtocolsExt,
        object_request: ObjectRequestRef,
    ) -> Result<impl Future<Output = ()>, zx::Status>
    where
        T: GetVmo,
    {
        let file = OpenNode::new(file);
        let options = protocols.to_file_options()?;
        let stream = zx::Stream::create(options.to_stream_options(), file.get_vmo(), 0)?;
        create_connection(scope, StreamIoConnection { file, stream }, options, object_request)
    }
}

#[async_trait]
impl<T: 'static + File> IoOpHandler for StreamIoConnection<T> {
    async fn read(&mut self, count: u64) -> Result<Vec<u8>, zx::Status> {
        let mut data = vec![0u8; count as usize];
        let actual = self.stream.readv(zx::StreamReadOptions::empty(), &[&mut data])?;
        data.truncate(actual);
        Ok(data)
    }

    async fn read_at(&self, offset: u64, count: u64) -> Result<Vec<u8>, zx::Status> {
        let mut data = vec![0u8; count as usize];
        let actual = self.stream.readv_at(zx::StreamReadOptions::empty(), offset, &[&mut data])?;
        data.truncate(actual);
        Ok(data)
    }

    async fn write(&mut self, data: &[u8]) -> Result<u64, zx::Status> {
        let actual = self.stream.writev(zx::StreamWriteOptions::empty(), &[data])?;
        Ok(actual as u64)
    }

    async fn write_at(&self, offset: u64, data: &[u8]) -> Result<u64, zx::Status> {
        let actual = self.stream.writev_at(zx::StreamWriteOptions::empty(), offset, &[data])?;
        Ok(actual as u64)
    }

    async fn seek(&mut self, offset: i64, origin: fio::SeekOrigin) -> Result<u64, zx::Status> {
        let position = match origin {
            fio::SeekOrigin::Start => {
                if offset < 0 {
                    return Err(zx::Status::INVALID_ARGS);
                }
                SeekFrom::Start(offset as u64)
            }
            fio::SeekOrigin::Current => SeekFrom::Current(offset),
            fio::SeekOrigin::End => SeekFrom::End(offset),
        };
        self.stream.seek(position)
    }

    fn update_flags(&mut self, flags: fio::OpenFlags) -> zx::Status {
        let append_mode = flags.contains(fio::OpenFlags::APPEND) as u8;
        match self.stream.set_mode_append(&append_mode) {
            Ok(()) => zx::Status::OK,
            Err(status) => status,
        }
    }

    fn duplicate_stream(&self) -> Result<Option<zx::Stream>, zx::Status> {
        self.stream.duplicate_handle(zx::Rights::SAME_RIGHTS).map(|s| Some(s))
    }
}

/// Return type for [`handle_request()`] functions.
enum ConnectionState {
    /// Connection is still alive.
    Alive,
    /// Connection have received Node::Close message and the [`handle_close`] method has been
    /// already called for this connection.
    Closed,
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
            let Some(_guard) = self.scope.try_active_guard() else { break };

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
                ConnectionState::Closed => return,
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
            fio::FileRequest::Clone { flags, object, control_handle: _ } => {
                fuchsia_trace::duration!("storage", "File::Clone");
                self.handle_clone(flags, object);
            }
            fio::FileRequest::Reopen { rights_request: _, object_request, control_handle: _ } => {
                fuchsia_trace::duration!("storage", "File::Reopen");
                // TODO(https://fxbug.dev/77623): Handle unimplemented io2 method.
                // Suppress any errors in the event a bad `object_request` channel was provided.
                let _: Result<_, _> = object_request.close_with_epitaph(zx::Status::NOT_SUPPORTED);
            }
            fio::FileRequest::Close { responder } => {
                fuchsia_trace::duration!("storage", "File::Close");
                responder.send(
                    if self.options.rights.intersects(
                        fio::Operations::WRITE_BYTES | fio::Operations::UPDATE_ATTRIBUTES,
                    ) {
                        self.file.sync(SyncMode::PreClose).await.map_err(|status| status.into_raw())
                    } else {
                        Ok(())
                    },
                )?;
                return Ok(ConnectionState::Closed);
            }
            fio::FileRequest::Describe { responder } => {
                fuchsia_trace::duration!("storage", "File::Describe");
                let stream = self.file.duplicate_stream()?;
                responder.send(fio::FileInfo {
                    stream,
                    observer: self.file.event()?,
                    ..Default::default()
                })?;
            }
            fio::FileRequest::GetConnectionInfo { responder } => {
                fuchsia_trace::duration!("storage", "File::GetConnectionInfo");
                // TODO(https://fxbug.dev/77623): Restrict GET_ATTRIBUTES.
                responder.send(fio::ConnectionInfo {
                    rights: Some(self.options.rights),
                    ..Default::default()
                })?;
            }
            fio::FileRequest::Sync { responder } => {
                fuchsia_trace::duration!("storage", "File::Sync");
                responder.send(
                    self.file.sync(SyncMode::Normal).await.map_err(|status| status.into_raw()),
                )?;
            }
            fio::FileRequest::GetAttr { responder } => {
                fuchsia_trace::duration!("storage", "File::GetAttr");
                let (status, attrs) = self.handle_get_attr().await;
                responder.send(status.into_raw(), &attrs)?;
            }
            fio::FileRequest::SetAttr { flags, attributes, responder } => {
                fuchsia_trace::duration!("storage", "File::SetAttr");
                let status = self.handle_set_attr(flags, attributes).await;
                responder.send(status.into_raw())?;
            }
            fio::FileRequest::GetAttributes { query, responder } => {
                fuchsia_trace::duration!("storage", "File::GetAttributes");
                let result = self.handle_get_attributes(query).await;
                responder.send(
                    result
                        .as_ref()
                        .map(|a| {
                            let fio::NodeAttributes2 {
                                mutable_attributes: m,
                                immutable_attributes: i,
                            } = a;
                            (m, i)
                        })
                        .map_err(|status| zx::Status::into_raw(*status)),
                )?;
            }
            fio::FileRequest::UpdateAttributes { payload: _, responder } => {
                fuchsia_trace::duration!("storage", "File::UpdateAttributes");
                // TODO(https://fxbug.dev/77623): Handle unimplemented io2 method.
                responder.send(Err(zx::Status::NOT_SUPPORTED.into_raw()))?;
            }
            fio::FileRequest::ListExtendedAttributes { iterator, control_handle: _ } => {
                fuchsia_trace::duration!("storage", "File::ListExtendedAttributes");
                self.handle_list_extended_attribute(iterator).await;
            }
            fio::FileRequest::GetExtendedAttribute { name, responder } => {
                fuchsia_trace::duration!("storage", "File::GetExtendedAttribute");
                let res = self.handle_get_extended_attribute(name).await.map_err(|s| s.into_raw());
                responder.send(res)?;
            }
            fio::FileRequest::SetExtendedAttribute { name, value, mode, responder } => {
                fuchsia_trace::duration!("storage", "File::SetExtendedAttribute");
                let res = self
                    .handle_set_extended_attribute(name, value, mode)
                    .await
                    .map_err(|s| s.into_raw());
                responder.send(res)?;
            }
            fio::FileRequest::RemoveExtendedAttribute { name, responder } => {
                fuchsia_trace::duration!("storage", "File::RemoveExtendedAttribute");
                let res =
                    self.handle_remove_extended_attribute(name).await.map_err(|s| s.into_raw());
                responder.send(res)?;
            }
            fio::FileRequest::Read { count, responder } => {
                fuchsia_trace::duration!("storage", "File::Read", "bytes" => count);
                let result = self.handle_read(count).await;
                let () = responder.send(result.as_deref().map_err(|s| s.into_raw()))?;
            }
            fio::FileRequest::ReadAt { offset, count, responder } => {
                fuchsia_trace::duration!(
                    "storage",
                    "File::ReadAt",
                    "offset" => offset,
                    "bytes" => count
                );
                let result = self.handle_read_at(offset, count).await;
                let () = responder.send(result.as_deref().map_err(|s| s.into_raw()))?;
            }
            fio::FileRequest::Write { data, responder } => {
                fuchsia_trace::duration!("storage", "File::Write", "bytes" => data.len() as u64);
                let result = self.handle_write(&data).await;
                responder.send(result.map_err(zx::Status::into_raw))?;
            }
            fio::FileRequest::WriteAt { offset, data, responder } => {
                fuchsia_trace::duration!(
                    "storage",
                    "File::WriteAt",
                    "offset" => offset,
                    "bytes" => data.len() as u64
                );
                let result = self.handle_write_at(offset, &data).await;
                responder.send(result.map_err(zx::Status::into_raw))?;
            }
            fio::FileRequest::Seek { origin, offset, responder } => {
                fuchsia_trace::duration!("storage", "File::Seek");
                let result = self.handle_seek(offset, origin).await;
                responder.send(result.map_err(zx::Status::into_raw))?;
            }
            fio::FileRequest::Resize { length, responder } => {
                fuchsia_trace::duration!("storage", "File::Resize", "length" => length);
                let result = self.handle_truncate(length).await;
                responder.send(result.map_err(zx::Status::into_raw))?;
            }
            fio::FileRequest::GetFlags { responder } => {
                fuchsia_trace::duration!("storage", "File::GetFlags");
                responder.send(ZX_OK, self.options.to_io1())?;
            }
            fio::FileRequest::SetFlags { flags, responder } => {
                fuchsia_trace::duration!("storage", "File::SetFlags");
                self.options.is_append = flags.contains(fio::OpenFlags::APPEND);
                responder.send(self.file.update_flags(self.options.to_io1()).into_raw())?;
            }
            fio::FileRequest::GetBackingMemory { flags, responder } => {
                fuchsia_trace::duration!("storage", "File::GetBackingMemory");
                let result = self.handle_get_backing_memory(flags).await;
                responder.send(result.map_err(zx::Status::into_raw))?;
            }
            fio::FileRequest::AdvisoryLock { request: _, responder } => {
                fuchsia_trace::duration!("storage", "File::AdvisoryLock");
                responder.send(Err(ZX_ERR_NOT_SUPPORTED))?;
            }
            fio::FileRequest::Query { responder } => {
                responder.send(fio::FILE_PROTOCOL_NAME.as_bytes())?;
            }
            fio::FileRequest::QueryFilesystem { responder } => {
                fuchsia_trace::duration!("storage", "File::QueryFilesystem");
                match self.file.query_filesystem() {
                    Err(status) => responder.send(status.into_raw(), None)?,
                    Ok(info) => responder.send(0, Some(&info))?,
                }
            }
        }
        Ok(ConnectionState::Alive)
    }

    fn handle_clone(&mut self, flags: fio::OpenFlags, server_end: ServerEnd<fio::NodeMarker>) {
        let describe = flags.intersects(fio::OpenFlags::DESCRIBE);
        let flags = match inherit_rights_for_clone(self.options.to_io1(), flags) {
            Ok(updated) => updated,
            Err(status) => {
                send_on_open_with_error(describe, server_end, status);
                return;
            }
        };

        self.file.clone().open(self.scope.clone(), flags, Path::dot(), server_end);
    }

    async fn handle_get_attr(&mut self) -> (zx::Status, fio::NodeAttributes) {
        let attributes = match self.file.get_attrs().await {
            Ok(attr) => attr,
            Err(status) => {
                return (
                    status,
                    fio::NodeAttributes {
                        mode: 0,
                        id: fio::INO_UNKNOWN,
                        content_size: 0,
                        storage_size: 0,
                        link_count: 0,
                        creation_time: 0,
                        modification_time: 0,
                    },
                )
            }
        };
        (zx::Status::OK, attributes)
    }

    async fn handle_get_attributes(
        &mut self,
        query: fio::NodeAttributesQuery,
    ) -> Result<fio::NodeAttributes2, zx::Status> {
        self.file.get_attributes(query).await
    }

    async fn handle_read(&mut self, count: u64) -> Result<Vec<u8>, zx::Status> {
        if !self.options.rights.intersects(fio::Operations::READ_BYTES) {
            return Err(zx::Status::BAD_HANDLE);
        }

        if count > fio::MAX_TRANSFER_SIZE {
            return Err(zx::Status::OUT_OF_RANGE);
        }
        self.file.read(count).await
    }

    async fn handle_read_at(&self, offset: u64, count: u64) -> Result<Vec<u8>, zx::Status> {
        if !self.options.rights.intersects(fio::Operations::READ_BYTES) {
            return Err(zx::Status::BAD_HANDLE);
        }
        if count > fio::MAX_TRANSFER_SIZE {
            return Err(zx::Status::OUT_OF_RANGE);
        }
        self.file.read_at(offset, count).await
    }

    async fn handle_write(&mut self, content: &[u8]) -> Result<u64, zx::Status> {
        if !self.options.rights.intersects(fio::Operations::WRITE_BYTES) {
            return Err(zx::Status::BAD_HANDLE);
        }
        self.file.write(content).await
    }

    async fn handle_write_at(&self, offset: u64, content: &[u8]) -> Result<u64, zx::Status> {
        if !self.options.rights.intersects(fio::Operations::WRITE_BYTES) {
            return Err(zx::Status::BAD_HANDLE);
        }

        self.file.write_at(offset, content).await
    }

    /// Move seek position to byte `offset` relative to the origin specified by `start`.
    async fn handle_seek(
        &mut self,
        offset: i64,
        origin: fio::SeekOrigin,
    ) -> Result<u64, zx::Status> {
        self.file.seek(offset, origin).await
    }

    async fn handle_set_attr(
        &mut self,
        flags: fio::NodeAttributeFlags,
        attrs: fio::NodeAttributes,
    ) -> zx::Status {
        if !self.options.rights.intersects(fio::Operations::WRITE_BYTES) {
            return zx::Status::BAD_HANDLE;
        }

        match self.file.set_attrs(flags, attrs).await {
            Ok(()) => zx::Status::OK,
            Err(status) => status,
        }
    }

    async fn handle_truncate(&mut self, length: u64) -> Result<(), zx::Status> {
        if !self.options.rights.intersects(fio::Operations::WRITE_BYTES) {
            return Err(zx::Status::BAD_HANDLE);
        }

        self.file.truncate(length).await
    }

    async fn handle_get_backing_memory(
        &mut self,
        flags: fio::VmoFlags,
    ) -> Result<zx::Vmo, zx::Status> {
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
    ) -> Result<fio::ExtendedAttributeValue, zx::Status> {
        let value = self.file.get_extended_attribute(name).await?;
        encode_extended_attribute_value(value)
    }

    async fn handle_set_extended_attribute(
        &mut self,
        name: Vec<u8>,
        value: fio::ExtendedAttributeValue,
        mode: fio::SetExtendedAttributeMode,
    ) -> Result<(), zx::Status> {
        if name.iter().any(|c| *c == 0) {
            return Err(zx::Status::INVALID_ARGS);
        }
        let val = decode_extended_attribute_value(value)?;
        self.file.set_extended_attribute(name, val, mode).await
    }

    async fn handle_remove_extended_attribute(&mut self, name: Vec<u8>) -> Result<(), zx::Status> {
        self.file.remove_extended_attribute(name).await
    }
}

#[async_trait]
impl<T: 'static + File, U: Deref<Target = OpenNode<T>> + IoOpHandler> Representation
    for FileConnection<U>
{
    type Protocol = fio::FileMarker;

    async fn get_representation(
        &self,
        requested_attributes: fio::NodeAttributesQuery,
    ) -> Result<fio::Representation, zx::Status> {
        // TODO(fxbug.dev/77623): Add support for connecting as Node.
        Ok(fio::Representation::File(fio::FileInfo {
            is_append: Some(self.options.is_append),
            observer: self.file.event()?,
            stream: self.file.duplicate_stream()?,
            attributes: if requested_attributes.is_empty() {
                None
            } else {
                Some(self.file.get_attributes(requested_attributes).await?)
            },
            ..Default::default()
        }))
    }

    async fn node_info(&self) -> Result<fio::NodeInfoDeprecated, Status> {
        let stream = self.file.duplicate_stream()?;
        Ok(fio::NodeInfoDeprecated::File(fio::FileObject { event: self.file.event()?, stream }))
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::{file::FileOptions, node::Node, ToObjectRequest},
        assert_matches::assert_matches,
        async_trait::async_trait,
        fuchsia_zircon::{self as zx, HandleBased},
        futures::prelude::*,
        std::sync::Mutex,
    };

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
        Init { options: FileOptions },
        ReadAt { offset: u64, count: u64 },
        WriteAt { offset: u64, content: Vec<u8> },
        Append { content: Vec<u8> },
        Truncate { length: u64 },
        GetBackingMemory { flags: fio::VmoFlags },
        GetSize,
        GetAttrs,
        GetAttributes { query: fio::NodeAttributesQuery },
        SetAttrs { flags: fio::NodeAttributeFlags, attrs: fio::NodeAttributes },
        Close,
        Sync,
    }

    type MockCallbackType = Box<dyn Fn(&FileOperation) -> zx::Status + Sync + Send>;
    /// A fake file that just tracks what calls `FileConnection` makes on it.
    struct MockFile {
        /// The list of operations that have been called.
        operations: Mutex<Vec<FileOperation>>,
        /// Callback used to determine how to respond to given operation.
        callback: MockCallbackType,
        /// Only used for get_size/get_attributes
        file_size: u64,
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
            Self::new_with_vmo(callback, zx::Handle::invalid().into())
        }

        fn new_with_vmo(callback: MockCallbackType, vmo: zx::Vmo) -> Arc<Self> {
            Arc::new(MockFile {
                operations: Mutex::new(Vec::new()),
                callback,
                file_size: MOCK_FILE_SIZE,
                vmo,
            })
        }

        fn handle_operation(&self, operation: FileOperation) -> Result<(), zx::Status> {
            let result = (self.callback)(&operation);
            self.operations.lock().unwrap().push(operation);
            match result {
                zx::Status::OK => Ok(()),
                err => Err(err),
            }
        }
    }

    #[async_trait]
    impl Node for MockFile {
        async fn get_attrs(&self) -> Result<fio::NodeAttributes, zx::Status> {
            self.handle_operation(FileOperation::GetAttrs)?;
            Ok(fio::NodeAttributes {
                mode: fio::MODE_TYPE_FILE,
                id: MOCK_FILE_ID,
                content_size: self.file_size,
                storage_size: 2 * self.file_size,
                link_count: MOCK_FILE_LINKS,
                creation_time: MOCK_FILE_CREATION_TIME,
                modification_time: MOCK_FILE_MODIFICATION_TIME,
            })
        }

        async fn get_attributes(
            &self,
            query: fio::NodeAttributesQuery,
        ) -> Result<fio::NodeAttributes2, zx::Status> {
            self.handle_operation(FileOperation::GetAttributes { query })?;
            Ok(attributes!(
                query,
                Mutable {
                    creation_time: MOCK_FILE_CREATION_TIME,
                    modification_time: MOCK_FILE_MODIFICATION_TIME,
                    mode: 0,
                    uid: 0,
                    gid: 0,
                    rdev: 0
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

    #[async_trait]
    impl File for MockFile {
        fn writable(&self) -> bool {
            true
        }

        async fn open_file(&self, options: &FileOptions) -> Result<(), zx::Status> {
            self.handle_operation(FileOperation::Init { options: *options })?;
            Ok(())
        }

        async fn truncate(&self, length: u64) -> Result<(), zx::Status> {
            self.handle_operation(FileOperation::Truncate { length })
        }

        async fn get_backing_memory(&self, flags: fio::VmoFlags) -> Result<zx::Vmo, zx::Status> {
            self.handle_operation(FileOperation::GetBackingMemory { flags })?;
            Err(zx::Status::NOT_SUPPORTED)
        }

        async fn get_size(&self) -> Result<u64, zx::Status> {
            self.handle_operation(FileOperation::GetSize)?;
            Ok(self.file_size)
        }

        async fn set_attrs(
            &self,
            flags: fio::NodeAttributeFlags,
            attrs: fio::NodeAttributes,
        ) -> Result<(), zx::Status> {
            self.handle_operation(FileOperation::SetAttrs { flags, attrs })?;
            Ok(())
        }

        async fn sync(&self, _mode: SyncMode) -> Result<(), zx::Status> {
            self.handle_operation(FileOperation::Sync)
        }
    }

    #[async_trait]
    impl FileIo for MockFile {
        async fn read_at(&self, offset: u64, buffer: &mut [u8]) -> Result<u64, zx::Status> {
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

        async fn write_at(&self, offset: u64, content: &[u8]) -> Result<u64, zx::Status> {
            self.handle_operation(FileOperation::WriteAt { offset, content: content.to_vec() })?;
            Ok(content.len() as u64)
        }

        async fn append(&self, content: &[u8]) -> Result<(u64, u64), zx::Status> {
            self.handle_operation(FileOperation::Append { content: content.to_vec() })?;
            Ok((content.len() as u64, self.file_size + content.len() as u64))
        }
    }

    impl DirectoryEntry for MockFile {
        fn open(
            self: Arc<Self>,
            scope: ExecutionScope,
            flags: fio::OpenFlags,
            path: Path,
            server_end: ServerEnd<fio::NodeMarker>,
        ) {
            assert!(path.is_empty());

            flags.to_object_request(server_end).handle(|object_request| {
                object_request.spawn_connection(
                    scope.clone(),
                    self.clone(),
                    flags,
                    FidlIoConnection::create,
                )
            });
        }

        fn entry_info(&self) -> crate::directory::entry::EntryInfo {
            todo!()
        }
    }

    impl GetVmo for MockFile {
        fn get_vmo(&self) -> &zx::Vmo {
            &self.vmo
        }
    }

    /// Only the init operation will succeed, all others fail.
    fn only_allow_init(op: &FileOperation) -> zx::Status {
        match op {
            FileOperation::Init { .. } => zx::Status::OK,
            _ => zx::Status::IO,
        }
    }

    /// All operations succeed.
    fn always_succeed_callback(_op: &FileOperation) -> zx::Status {
        zx::Status::OK
    }

    struct TestEnv {
        pub file: Arc<MockFile>,
        pub proxy: fio::FileProxy,
        pub scope: ExecutionScope,
    }

    fn init_mock_file(callback: MockCallbackType, flags: fio::OpenFlags) -> TestEnv {
        let file = MockFile::new(callback);
        let (proxy, server_end) =
            fidl::endpoints::create_proxy::<fio::FileMarker>().expect("Create proxy to succeed");

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
        let () = env.proxy.sync().await.unwrap().map_err(zx::Status::from_raw).unwrap();
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
        let _: Vec<u8> = env.proxy.read(6).await.unwrap().map_err(zx::Status::from_raw).unwrap();
        let (clone_proxy, remote) = fidl::endpoints::create_proxy::<fio::FileMarker>().unwrap();
        env.proxy.clone(fio::OpenFlags::CLONE_SAME_RIGHTS, remote.into_channel().into()).unwrap();
        // Seek and read from clone_proxy.
        let _: u64 = clone_proxy
            .seek(fio::SeekOrigin::Start, 100)
            .await
            .unwrap()
            .map_err(zx::Status::from_raw)
            .unwrap();
        let _: Vec<u8> = clone_proxy.read(5).await.unwrap().map_err(zx::Status::from_raw).unwrap();

        // Read from original proxy.
        let _: Vec<u8> = env.proxy.read(5).await.unwrap().map_err(zx::Status::from_raw).unwrap();

        let events = env.file.operations.lock().unwrap();
        // Each connection should have an independent seek.
        assert_eq!(
            *events,
            vec![
                FileOperation::Init {
                    options: FileOptions { rights: RIGHTS_RW, is_append: false }
                },
                FileOperation::ReadAt { offset: 0, count: 6 },
                FileOperation::Init {
                    options: FileOptions { rights: RIGHTS_RW, is_append: false }
                },
                FileOperation::ReadAt { offset: 100, count: 5 },
                FileOperation::ReadAt { offset: 6, count: 5 },
            ]
        );
    }

    #[fuchsia::test]
    async fn test_close_succeeds() {
        let env = init_mock_file(Box::new(always_succeed_callback), fio::OpenFlags::RIGHT_READABLE);
        let () = env.proxy.close().await.unwrap().map_err(zx::Status::from_raw).unwrap();

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
        let status = env.proxy.close().await.unwrap().map_err(zx::Status::from_raw);
        assert_eq!(status, Err(zx::Status::IO));

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
    async fn test_getattr() {
        let env = init_mock_file(Box::new(always_succeed_callback), fio::OpenFlags::empty());
        let (status, attributes) = env.proxy.get_attr().await.unwrap();
        assert_eq!(zx::Status::from_raw(status), zx::Status::OK);
        assert_eq!(
            attributes,
            fio::NodeAttributes {
                mode: fio::MODE_TYPE_FILE,
                id: MOCK_FILE_ID,
                content_size: MOCK_FILE_SIZE,
                storage_size: 2 * MOCK_FILE_SIZE,
                link_count: MOCK_FILE_LINKS,
                creation_time: MOCK_FILE_CREATION_TIME,
                modification_time: MOCK_FILE_MODIFICATION_TIME,
            }
        );
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
                FileOperation::GetAttrs
            ]
        );
    }

    #[fuchsia::test]
    async fn test_get_attributes() {
        let env = init_mock_file(Box::new(always_succeed_callback), fio::OpenFlags::empty());
        let (mutable_attributes, immutable_attributes) = env
            .proxy
            .get_attributes(fio::NodeAttributesQuery::all())
            .await
            .unwrap()
            .map_err(zx::Status::from_raw)
            .unwrap();
        let expected = attributes!(
            fio::NodeAttributesQuery::all(),
            Mutable {
                creation_time: MOCK_FILE_CREATION_TIME,
                modification_time: MOCK_FILE_MODIFICATION_TIME,
                mode: 0,
                uid: 0,
                gid: 0,
                rdev: 0,
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
            .map_err(zx::Status::from_raw);
        assert_eq!(result, Err(zx::Status::NOT_SUPPORTED));
        let events = env.file.operations.lock().unwrap();
        assert_eq!(
            *events,
            vec![
                FileOperation::Init { options: FileOptions { rights: RIGHTS_R, is_append: false } },
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
            .map_err(zx::Status::from_raw);
        assert_eq!(result, Err(zx::Status::ACCESS_DENIED));
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
            .map_err(zx::Status::from_raw);
        assert_eq!(result, Err(zx::Status::ACCESS_DENIED));
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
        assert_eq!(zx::Status::from_raw(status), zx::Status::OK);
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
                assert_eq!(zx::Status::from_raw(s), zx::Status::OK);
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
        let data = env.proxy.read(10).await.unwrap().map_err(zx::Status::from_raw).unwrap();
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
        let result = env.proxy.read(10).await.unwrap().map_err(zx::Status::from_raw);
        assert_eq!(result, Err(zx::Status::BAD_HANDLE));
    }

    #[fuchsia::test]
    async fn test_read_validates_count() {
        let env = init_mock_file(Box::new(only_allow_init), fio::OpenFlags::RIGHT_READABLE);
        let result =
            env.proxy.read(fio::MAX_TRANSFER_SIZE + 1).await.unwrap().map_err(zx::Status::from_raw);
        assert_eq!(result, Err(zx::Status::OUT_OF_RANGE));
    }

    #[fuchsia::test]
    async fn test_read_at_succeeds() {
        let env = init_mock_file(Box::new(always_succeed_callback), fio::OpenFlags::RIGHT_READABLE);
        let data = env.proxy.read_at(5, 10).await.unwrap().map_err(zx::Status::from_raw).unwrap();
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
            .map_err(zx::Status::from_raw);
        assert_eq!(result, Err(zx::Status::OUT_OF_RANGE));
    }

    #[fuchsia::test]
    async fn test_seek_start() {
        let env = init_mock_file(Box::new(always_succeed_callback), fio::OpenFlags::RIGHT_READABLE);
        let offset = env
            .proxy
            .seek(fio::SeekOrigin::Start, 10)
            .await
            .unwrap()
            .map_err(zx::Status::from_raw)
            .unwrap();
        assert_eq!(offset, 10);

        let data = env.proxy.read(1).await.unwrap().map_err(zx::Status::from_raw).unwrap();
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
            .map_err(zx::Status::from_raw)
            .unwrap();
        assert_eq!(offset, 10);

        let offset = env
            .proxy
            .seek(fio::SeekOrigin::Current, -2)
            .await
            .unwrap()
            .map_err(zx::Status::from_raw)
            .unwrap();
        assert_eq!(offset, 8);

        let data = env.proxy.read(1).await.unwrap().map_err(zx::Status::from_raw).unwrap();
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
        let result = env
            .proxy
            .seek(fio::SeekOrigin::Current, -4)
            .await
            .unwrap()
            .map_err(zx::Status::from_raw);
        assert_eq!(result, Err(zx::Status::OUT_OF_RANGE));
    }

    #[fuchsia::test]
    async fn test_seek_end() {
        let env = init_mock_file(Box::new(always_succeed_callback), fio::OpenFlags::RIGHT_READABLE);
        let offset = env
            .proxy
            .seek(fio::SeekOrigin::End, -4)
            .await
            .unwrap()
            .map_err(zx::Status::from_raw)
            .unwrap();
        assert_eq!(offset, MOCK_FILE_SIZE - 4);

        let data = env.proxy.read(1).await.unwrap().map_err(zx::Status::from_raw).unwrap();
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
    async fn test_set_attrs() {
        let env = init_mock_file(Box::new(always_succeed_callback), fio::OpenFlags::RIGHT_WRITABLE);
        let set_attrs = fio::NodeAttributes {
            mode: 0,
            id: 0,
            content_size: 0,
            storage_size: 0,
            link_count: 0,
            creation_time: 40000,
            modification_time: 100000,
        };
        let status = env
            .proxy
            .set_attr(
                fio::NodeAttributeFlags::CREATION_TIME | fio::NodeAttributeFlags::MODIFICATION_TIME,
                &set_attrs,
            )
            .await
            .unwrap();
        assert_eq!(zx::Status::from_raw(status), zx::Status::OK);
        let events = env.file.operations.lock().unwrap();
        assert_eq!(
            *events,
            vec![
                FileOperation::Init { options: FileOptions { rights: RIGHTS_W, is_append: false } },
                FileOperation::SetAttrs {
                    flags: fio::NodeAttributeFlags::CREATION_TIME
                        | fio::NodeAttributeFlags::MODIFICATION_TIME,
                    attrs: set_attrs
                },
            ]
        );
    }

    #[fuchsia::test]
    async fn test_set_flags() {
        let env = init_mock_file(Box::new(always_succeed_callback), fio::OpenFlags::RIGHT_WRITABLE);
        let status = env.proxy.set_flags(fio::OpenFlags::APPEND).await.unwrap();
        assert_eq!(zx::Status::from_raw(status), zx::Status::OK);
        let (status, flags) = env.proxy.get_flags().await.unwrap();
        assert_eq!(zx::Status::from_raw(status), zx::Status::OK);
        assert_eq!(flags, fio::OpenFlags::RIGHT_WRITABLE | fio::OpenFlags::APPEND);
    }

    #[fuchsia::test]
    async fn test_sync() {
        let env = init_mock_file(Box::new(always_succeed_callback), fio::OpenFlags::empty());
        let () = env.proxy.sync().await.unwrap().map_err(zx::Status::from_raw).unwrap();
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
        let () = env.proxy.resize(10).await.unwrap().map_err(zx::Status::from_raw).unwrap();
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
        let result = env.proxy.resize(10).await.unwrap().map_err(zx::Status::from_raw);
        assert_eq!(result, Err(zx::Status::BAD_HANDLE));
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
        let count = env.proxy.write(data).await.unwrap().map_err(zx::Status::from_raw).unwrap();
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
        let result = env.proxy.write(data).await.unwrap().map_err(zx::Status::from_raw);
        assert_eq!(result, Err(zx::Status::BAD_HANDLE));
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
        let count =
            env.proxy.write_at(data, 10).await.unwrap().map_err(zx::Status::from_raw).unwrap();
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
        let count = env.proxy.write(data).await.unwrap().map_err(zx::Status::from_raw).unwrap();
        assert_eq!(count, data.len() as u64);
        let offset = env
            .proxy
            .seek(fio::SeekOrigin::Current, 0)
            .await
            .unwrap()
            .map_err(zx::Status::from_raw)
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

    fn init_mock_stream_file(vmo: zx::Vmo, flags: fio::OpenFlags) -> TestEnv {
        let file = MockFile::new_with_vmo(Box::new(always_succeed_callback), vmo);
        let (proxy, server_end) =
            fidl::endpoints::create_proxy::<fio::FileMarker>().expect("Create proxy to succeed");

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

        let fio::FileInfo { stream: Some(stream), .. } = env.proxy.describe().await.unwrap() else { panic!("Missing stream") };
        let mut buf = [0; 20];
        assert_eq!(
            stream.readv(zx::StreamReadOptions::empty(), &[&mut buf]).expect("readv failed"),
            VMO_CONTENTS.len()
        );
        assert_eq!(&buf[..VMO_CONTENTS.len()], &VMO_CONTENTS[..]);
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
            .map_err(zx::Status::from_raw)
            .unwrap();
        assert_eq!(data, vmo_contents);

        let events = env.file.operations.lock().unwrap();
        assert_eq!(
            *events,
            [FileOperation::Init { options: FileOptions { rights: RIGHTS_R, is_append: false } },]
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
            .map_err(zx::Status::from_raw)
            .unwrap();
        assert_eq!(data, vmo_contents[OFFSET as usize..]);

        let events = env.file.operations.lock().unwrap();
        assert_eq!(
            *events,
            [FileOperation::Init { options: FileOptions { rights: RIGHTS_R, is_append: false } },]
        );
    }

    #[fuchsia::test]
    async fn test_stream_write() {
        const DATA_SIZE: u64 = 10;
        let vmo = zx::Vmo::create(DATA_SIZE).unwrap();
        let flags = fio::OpenFlags::RIGHT_WRITABLE;
        let env =
            init_mock_stream_file(vmo.duplicate_handle(zx::Rights::SAME_RIGHTS).unwrap(), flags);

        let data: [u8; DATA_SIZE as usize] = [9, 8, 7, 6, 5, 4, 3, 2, 1, 0];
        let written = env.proxy.write(&data).await.unwrap().map_err(zx::Status::from_raw).unwrap();
        assert_eq!(written, DATA_SIZE);
        let mut vmo_contents = [0; DATA_SIZE as usize];
        vmo.read(&mut vmo_contents, 0).unwrap();
        assert_eq!(vmo_contents, data);

        let events = env.file.operations.lock().unwrap();
        assert_eq!(
            *events,
            [FileOperation::Init { options: FileOptions { rights: RIGHTS_W, is_append: false } },]
        );
    }

    #[fuchsia::test]
    async fn test_stream_write_at() {
        const OFFSET: u64 = 4;
        const DATA_SIZE: u64 = 10;
        let vmo = zx::Vmo::create(DATA_SIZE + OFFSET).unwrap();
        let flags = fio::OpenFlags::RIGHT_WRITABLE;
        let env =
            init_mock_stream_file(vmo.duplicate_handle(zx::Rights::SAME_RIGHTS).unwrap(), flags);

        let data: [u8; DATA_SIZE as usize] = [9, 8, 7, 6, 5, 4, 3, 2, 1, 0];
        let written =
            env.proxy.write_at(&data, OFFSET).await.unwrap().map_err(zx::Status::from_raw).unwrap();
        assert_eq!(written, DATA_SIZE);
        let mut vmo_contents = [0; DATA_SIZE as usize];
        vmo.read(&mut vmo_contents, OFFSET).unwrap();
        assert_eq!(vmo_contents, data);

        let events = env.file.operations.lock().unwrap();
        assert_eq!(
            *events,
            [FileOperation::Init { options: FileOptions { rights: RIGHTS_W, is_append: false } }]
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
            .map_err(zx::Status::from_raw)
            .unwrap();
        assert_eq!(position, 8);
        let data = env.proxy.read(2).await.unwrap().map_err(zx::Status::from_raw).unwrap();
        assert_eq!(data, [1, 0]);

        let position = env
            .proxy
            .seek(fio::SeekOrigin::Current, -4)
            .await
            .unwrap()
            .map_err(zx::Status::from_raw)
            .unwrap();
        // Seeked to 8, read 2, seeked backwards 4. 8 + 2 - 4 = 6.
        assert_eq!(position, 6);
        let data = env.proxy.read(2).await.unwrap().map_err(zx::Status::from_raw).unwrap();
        assert_eq!(data, [3, 2]);

        let position = env
            .proxy
            .seek(fio::SeekOrigin::End, -6)
            .await
            .unwrap()
            .map_err(zx::Status::from_raw)
            .unwrap();
        assert_eq!(position, 4);
        let data = env.proxy.read(2).await.unwrap().map_err(zx::Status::from_raw).unwrap();
        assert_eq!(data, [5, 4]);

        let e = env
            .proxy
            .seek(fio::SeekOrigin::Start, -1)
            .await
            .unwrap()
            .map_err(zx::Status::from_raw)
            .expect_err("Seeking before the start of a file should be an error");
        assert_eq!(e, zx::Status::INVALID_ARGS);
    }

    #[fuchsia::test]
    async fn test_stream_set_flags() {
        let data = [0, 1, 2, 3, 4];
        let vmo = zx::Vmo::create_with_opts(zx::VmoOptions::RESIZABLE, 100).unwrap();
        let flags = fio::OpenFlags::RIGHT_WRITABLE;
        let env =
            init_mock_stream_file(vmo.duplicate_handle(zx::Rights::SAME_RIGHTS).unwrap(), flags);

        let written = env.proxy.write(&data).await.unwrap().map_err(zx::Status::from_raw).unwrap();
        assert_eq!(written, data.len() as u64);
        // Data was not appended.
        assert_eq!(vmo.get_content_size().unwrap(), 100);

        // Switch to append mode.
        zx::ok(env.proxy.set_flags(fio::OpenFlags::APPEND).await.unwrap()).unwrap();
        env.proxy
            .seek(fio::SeekOrigin::Start, 0)
            .await
            .unwrap()
            .map_err(zx::Status::from_raw)
            .unwrap();
        let written = env.proxy.write(&data).await.unwrap().map_err(zx::Status::from_raw).unwrap();
        assert_eq!(written, data.len() as u64);
        // Data was appended.
        assert_eq!(vmo.get_content_size().unwrap(), 105);

        // Switch out of append mode.
        zx::ok(env.proxy.set_flags(fio::OpenFlags::empty()).await.unwrap()).unwrap();
        env.proxy
            .seek(fio::SeekOrigin::Start, 0)
            .await
            .unwrap()
            .map_err(zx::Status::from_raw)
            .unwrap();
        let written = env.proxy.write(&data).await.unwrap().map_err(zx::Status::from_raw).unwrap();
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
            env.proxy.read(fio::MAX_TRANSFER_SIZE + 1).await.unwrap().map_err(zx::Status::from_raw);
        assert_eq!(result, Err(zx::Status::OUT_OF_RANGE));
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
            .map_err(zx::Status::from_raw);
        assert_eq!(result, Err(zx::Status::OUT_OF_RANGE));
    }
}
