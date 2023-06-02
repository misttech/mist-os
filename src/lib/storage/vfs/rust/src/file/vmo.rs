// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Implementation of a file backed by a VMO buffer shared by all the file connections. The VMO can
//! be created before, or constructed on the first connection to the file via asynchronous callback.

#[cfg(test)]
mod tests;

use crate::{
    common::rights_to_posix_mode_bits,
    directory::entry::{DirectoryEntry, EntryInfo},
    execution_scope::ExecutionScope,
    file::{common::vmo_flags_to_rights, FidlIoConnection, File, FileIo, FileOptions},
    node::Node,
    path::Path,
    ToObjectRequest,
};

use {
    async_trait::async_trait,
    fidl::endpoints::ServerEnd,
    fidl_fuchsia_io as fio,
    fuchsia_zircon::{self as zx, AsHandleRef as _, HandleBased as _, Status, Vmo},
    futures::lock::Mutex,
    std::{future::Future, sync::Arc},
};

/// Helper trait to avoid generics in the [`VmoFile`] type by using dynamic dispatch.
#[async_trait]
trait AsyncInitVmoFile: Send + Sync {
    // TODO(http://fxbug.dev/99448): Making this non-async FnOnce() can greatly simplify things
    // and would remove the need for a separate trait.
    async fn init_vmo(&self) -> InitVmoResult;
}

struct AsyncInitVmoFileImpl<InitVmo> {
    callback: InitVmo,
}

#[async_trait]
impl<InitVmo, InitVmoFuture> AsyncInitVmoFile for AsyncInitVmoFileImpl<InitVmo>
where
    InitVmo: Fn() -> InitVmoFuture + Send + Sync + 'static,
    InitVmoFuture: Future<Output = InitVmoResult> + Send + 'static,
{
    async fn init_vmo(&self) -> InitVmoResult {
        (self.callback)().await
    }
}

/// Connection buffer initialization result. It is either a byte buffer with the file content, or
/// an error code.
pub type InitVmoResult = Result<Vmo, Status>;

/// Create new read-only `VmoFile` which serves constant content.
///
/// ## Examples
/// ```
/// // Using static data:
/// let from_str = read_only("str");
/// let from_bytes = read_only(b"bytes");
/// // Using owned data:
/// let from_string = read_only(String::from("owned"));
/// let from_vec = read_only(vec![0u8; 2]);
/// ```
pub fn read_only<Bytes>(bytes: Bytes) -> Arc<VmoFile>
where
    Bytes: 'static + AsRef<[u8]> + Send + Sync,
{
    let bytes = Arc::new(bytes);
    VmoFile::new_async(
        move || {
            let bytes = bytes.clone();
            Box::pin(async move {
                let bytes: &[u8] = bytes.as_ref().as_ref();
                let vmo = Vmo::create(bytes.len().try_into().unwrap())?;
                vmo.write(&bytes, 0)?;
                Ok(vmo)
            })
        },
        true,
        false,
        false,
    )
}

/// Create new read-write `VmoFile` with the specified `content` and `capacity`. If `capacity` is
/// smaller than `content.as_ref().len()`, `content` will be truncated. If `capacity` is `None`,
/// the file's size and capacity will both be equal to `content.as_ref().len()`.
///
/// ## Examples
/// ```
/// // Empty file with a capacity of 100 bytes:
/// let empty = read_write("", Some(100));
/// // Initialized file with capacity of 12 bytes:
/// let sized = read_write("Hello world!", None);
/// // File with capacity of 5 bytes containing "Hello":
/// let truncated = read_write("Hello, world!", Some(5));
/// ```
pub fn read_write<Bytes>(content: Bytes, capacity: Option<u64>) -> Arc<VmoFile>
where
    Bytes: 'static + AsRef<[u8]> + Send + Sync,
{
    let content_size: u64 = content.as_ref().len().try_into().unwrap();
    let capacity: u64 = capacity.unwrap_or(content_size);
    let content_size: u64 = std::cmp::min(capacity, content_size);

    let content = Arc::new(content);
    VmoFile::new_async(
        move || {
            let content = content.clone();
            Box::pin(async move {
                let vmo = Vmo::create(capacity)?;
                // Write up to `content_size` bytes from `content`, and set the VMO's content size.
                let content: &[u8] = &(*content).as_ref()[..content_size.try_into().unwrap()];
                vmo.write(&content, 0)?;
                vmo.set_content_size(&content_size)?;
                Ok(vmo)
            })
        },
        /*readable*/ true,
        /*writable*/ true,
        /*executable*/ false,
    )
}

/// Implementation of a VMO-backed file in a virtual file system. Supports both synchronous (from
/// existing Vmo) and asynchronous (from async callback) construction of the backing Vmo.
///
/// Futures returned by these callbacks will be executed by the library using connection specific
/// [`ExecutionScope`].
///
/// See the module documentation for more details.
pub struct VmoFile {
    /// Specifies if the file is readable. Always invoked even for non-readable VMOs.
    readable: bool,

    /// Specifies if the file is writable. If this is the case, the Vmo backing the file is never
    /// destroyed until this object is dropped.
    writable: bool,

    /// Specifies if the file can be opened as executable.
    executable: bool,

    /// Specifies the inode for this file. Can be [`fio::INO_UNKNOWN`] if not required.
    inode: u64,

    /// Vmo that backs the file. If constructed as None, will be initialized on first connection
    /// using [`Self::init_vmo`].
    vmo: Mutex<Option<Vmo>>,

    /// Asynchronous callback used to initialize [`Self::vmo`] on first connection to the file.
    init_vmo: Option<Box<dyn AsyncInitVmoFile + 'static>>,
}

impl VmoFile {
    /// Create a new VmoFile which is backed by an existing Vmo.
    ///
    /// # Arguments
    ///
    /// * `vmo` - Vmo backing this file object.
    /// * `readable` - If true, allow connections with OpenFlags::RIGHT_READABLE.
    /// * `writable` - If true, allow connections with OpenFlags::RIGHT_WRITABLE.
    /// * `executable` - If true, allow connections with OpenFlags::RIGHT_EXECUTABLE.
    pub fn new(vmo: Vmo, readable: bool, writable: bool, executable: bool) -> Arc<Self> {
        Self::new_with_inode(vmo, readable, writable, executable, fio::INO_UNKNOWN)
    }

    /// Create a new VmoFile with the specified options and inode value.
    ///
    /// # Arguments
    ///
    /// * `vmo` - Vmo backing this file object.
    /// * `readable` - If true, allow connections with OpenFlags::RIGHT_READABLE.
    /// * `writable` - If true, allow connections with OpenFlags::RIGHT_WRITABLE.
    /// * `executable` - If true, allow connections with OpenFlags::RIGHT_EXECUTABLE.
    /// * `inode` - Inode value to report when getting the VmoFile's attributes.
    pub fn new_with_inode(
        vmo: Vmo,
        readable: bool,
        writable: bool,
        executable: bool,
        inode: u64,
    ) -> Arc<Self> {
        Arc::new(VmoFile {
            readable,
            writable,
            executable,
            inode,
            vmo: Mutex::new(Some(vmo)),
            init_vmo: None,
        })
    }

    /// Create a new VmoFile which will be asynchronously initialized. The reported inode value will
    /// be [`fio::INO_UNKNOWN`]. See [`VmoFile::new_with_inode()`] to construct a VmoFile with an
    /// explicit inode value.
    ///
    /// # Arguments
    ///
    /// * `init_vmo` - Async callback to create the Vmo backing this file upon first connection.
    /// * `readable` - If true, allow connections with OpenFlags::RIGHT_READABLE.
    /// * `writable` - If true, allow connections with OpenFlags::RIGHT_WRITABLE.
    /// * `executable` - If true, allow connections with OpenFlags::RIGHT_EXECUTABLE.
    pub fn new_async<InitVmo, InitVmoFuture>(
        init_vmo: InitVmo,
        readable: bool,
        writable: bool,
        executable: bool,
    ) -> Arc<Self>
    where
        InitVmo: Fn() -> InitVmoFuture + Send + Sync + 'static,
        InitVmoFuture: Future<Output = InitVmoResult> + Send + 'static,
    {
        Arc::new(VmoFile {
            readable,
            writable,
            executable,
            inode: fio::INO_UNKNOWN,
            vmo: Mutex::new(None),
            init_vmo: Some(Box::new(AsyncInitVmoFileImpl { callback: init_vmo })),
        })
    }
}

impl DirectoryEntry for VmoFile {
    fn open(
        self: Arc<Self>,
        scope: ExecutionScope,
        flags: fio::OpenFlags,
        path: Path,
        server_end: ServerEnd<fio::NodeMarker>,
    ) {
        flags.to_object_request(server_end).handle(|object_request| {
            if !path.is_empty() {
                return Err(Status::NOT_DIR);
            }

            if flags.intersects(fio::OpenFlags::APPEND) {
                return Err(Status::NOT_SUPPORTED);
            }

            object_request.take().spawn(&scope.clone(), move |object_request| {
                Box::pin(async move {
                    {
                        let mut vmo_state = self.vmo.lock().await;
                        if vmo_state.is_none() {
                            *vmo_state = Some(self.init_vmo.as_ref().unwrap().init_vmo().await?);
                        }
                    }
                    object_request.create_connection(scope, self, flags, FidlIoConnection::create)
                })
            });
            Ok(())
        });
    }

    fn entry_info(&self) -> EntryInfo {
        EntryInfo::new(self.inode, fio::DirentType::File)
    }
}

#[async_trait]
impl Node for VmoFile {
    async fn get_attrs(&self) -> Result<fio::NodeAttributes, Status> {
        let content_size = self.get_size().await?;
        Ok(fio::NodeAttributes {
            mode: fio::MODE_TYPE_FILE
                | rights_to_posix_mode_bits(self.readable, self.writable, self.executable),
            id: self.inode,
            content_size,
            storage_size: content_size,
            link_count: 1,
            creation_time: 0,
            modification_time: 0,
        })
    }
}

#[async_trait]
impl FileIo for VmoFile {
    async fn read_at(&self, offset: u64, buffer: &mut [u8]) -> Result<u64, Status> {
        let guard = self.vmo.lock().await;
        let vmo = guard.as_ref().unwrap();
        let content_size = vmo.get_content_size()?;
        if offset >= content_size {
            return Ok(0u64);
        }
        let read_len: u64 = std::cmp::min(content_size - offset, buffer.len().try_into().unwrap());
        let buffer = &mut buffer[..read_len.try_into().unwrap()];
        vmo.read(buffer, offset)?;
        Ok(read_len)
    }

    async fn write_at(&self, offset: u64, content: &[u8]) -> Result<u64, Status> {
        if content.is_empty() {
            return Ok(0u64);
        }
        let guard = self.vmo.lock().await;
        let vmo = guard.as_ref().unwrap();
        let capacity = vmo.get_size()?;
        if offset >= capacity {
            return Err(Status::OUT_OF_RANGE);
        }
        let write_len: u64 = std::cmp::min(capacity - offset, content.len().try_into().unwrap());
        let content = &content[..write_len.try_into().unwrap()];
        vmo.write(content, offset)?;
        let end = offset + write_len;
        if end > vmo.get_content_size()? {
            vmo.set_content_size(&end)?;
        }
        Ok(write_len)
    }

    async fn append(&self, _content: &[u8]) -> Result<(u64, u64), Status> {
        Err(Status::NOT_SUPPORTED)
    }
}

#[async_trait]
impl File for VmoFile {
    fn readable(&self) -> bool {
        self.readable
    }

    fn writable(&self) -> bool {
        self.writable
    }

    fn executable(&self) -> bool {
        self.executable
    }

    async fn open_file(&self, _options: &FileOptions) -> Result<(), Status> {
        Ok(())
    }

    async fn truncate(&self, length: u64) -> Result<(), Status> {
        let guard = self.vmo.lock().await;
        let vmo = guard.as_ref().unwrap();
        let capacity = vmo.get_size()?;

        if length > capacity {
            return Err(Status::OUT_OF_RANGE);
        }

        let old_size = vmo.get_content_size()?;
        if length < old_size {
            // Zero out old data (which will decommit).
            vmo.set_content_size(&length)?;
            vmo.op_range(zx::VmoOp::ZERO, length, old_size - length)?;
        } else if length > old_size {
            // Zero out the range we are extending into.
            vmo.op_range(zx::VmoOp::ZERO, old_size, length - old_size)?;
            vmo.set_content_size(&length)?;
        }

        Ok(())
    }

    async fn get_backing_memory(&self, flags: fio::VmoFlags) -> Result<Vmo, Status> {
        // The only sharing mode we support that disallows the VMO size to change currently
        // is PRIVATE_CLONE (`get_as_private`), so we require that to be set explicitly.
        if flags.contains(fio::VmoFlags::WRITE) && !flags.contains(fio::VmoFlags::PRIVATE_CLONE) {
            return Err(zx::Status::NOT_SUPPORTED);
        }

        // Disallow opening as both writable and executable. In addition to improving W^X
        // enforcement, this also eliminates any inconstiencies related to clones that use
        // SNAPSHOT_AT_LEAST_ON_WRITE since in that case, we cannot satisfy both requirements.
        if flags.contains(fio::VmoFlags::EXECUTE) && flags.contains(fio::VmoFlags::WRITE) {
            return Err(zx::Status::NOT_SUPPORTED);
        }

        let guard = self.vmo.lock().await;
        let vmo = guard.as_ref().unwrap();

        // Logic here matches fuchsia.io requirements and matches what works for memfs.
        // Shared requests are satisfied by duplicating an handle, and private shares are
        // child VMOs.
        let vmo_rights = vmo_flags_to_rights(flags);
        // Unless private sharing mode is specified, we always default to shared.
        let new_vmo = if flags.contains(fio::VmoFlags::PRIVATE_CLONE) {
            get_as_private(&vmo, vmo_rights)?
        } else {
            get_as_shared(&vmo, vmo_rights)?
        };
        Ok(new_vmo)
    }

    async fn get_size(&self) -> Result<u64, Status> {
        let guard = self.vmo.lock().await;
        let vmo = guard.as_ref().unwrap();
        Ok(vmo.get_content_size()?)
    }

    async fn set_attrs(
        &self,
        _flags: fio::NodeAttributeFlags,
        _attrs: fio::NodeAttributes,
    ) -> Result<(), Status> {
        Err(Status::NOT_SUPPORTED)
    }

    async fn sync(&self) -> Result<(), Status> {
        Ok(())
    }
}

fn get_as_shared(vmo: &Vmo, mut rights: zx::Rights) -> Result<Vmo, zx::Status> {
    // Add set of basic rights to include in shared mode before duplicating the VMO handle.
    rights |= zx::Rights::BASIC | zx::Rights::MAP | zx::Rights::GET_PROPERTY;
    vmo.as_handle_ref().duplicate(rights).map(Into::into)
}

fn get_as_private(vmo: &Vmo, mut rights: zx::Rights) -> Result<Vmo, zx::Status> {
    // Add set of basic rights to include in private mode, ensuring we provide SET_PROPERTY.
    rights |=
        zx::Rights::BASIC | zx::Rights::MAP | zx::Rights::GET_PROPERTY | zx::Rights::SET_PROPERTY;

    // Ensure we give out a copy-on-write clone.
    let mut child_options = zx::VmoChildOptions::SNAPSHOT_AT_LEAST_ON_WRITE;
    // If we don't need a writable clone, we need to add CHILD_NO_WRITE since
    // SNAPSHOT_AT_LEAST_ON_WRITE removes ZX_RIGHT_EXECUTE even if the parent VMO has it, but
    // adding CHILD_NO_WRITE will ensure EXECUTE is maintained.
    if !rights.contains(zx::Rights::WRITE) {
        child_options |= zx::VmoChildOptions::NO_WRITE;
    } else {
        // If we need a writable clone, ensure it can be resized.
        child_options |= zx::VmoChildOptions::RESIZABLE;
    }

    let size = vmo.get_content_size()?;
    let new_vmo = vmo.create_child(child_options, 0, size)?;
    new_vmo.into_handle().replace_handle(rights).map(Into::into)
}
