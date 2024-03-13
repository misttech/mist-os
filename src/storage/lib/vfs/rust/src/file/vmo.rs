// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Implementation of a file backed by a VMO buffer shared by all the file connections.

#[cfg(test)]
mod tests;

use crate::{
    common::rights_to_posix_mode_bits,
    directory::entry::{DirectoryEntry, EntryInfo, OpenRequest},
    execution_scope::ExecutionScope,
    file::{
        common::vmo_flags_to_rights, File, FileLike, FileOptions, GetVmo, StreamIoConnection,
        SyncMode,
    },
    node::Node,
    ObjectRequestRef,
};

use {
    async_trait::async_trait,
    fidl_fuchsia_io as fio,
    fuchsia_zircon::{self as zx, HandleBased as _, Status, Vmo},
    std::sync::Arc,
};

/// Creates a new read-only `VmoFile` with the specified `content`.
///
/// ## Panics
///
/// This function panics if a VMO could not be created, or if `content` could not be written to the
/// VMO.
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
pub fn read_only(content: impl AsRef<[u8]>) -> Arc<VmoFile> {
    let bytes: &[u8] = content.as_ref();
    let vmo = Vmo::create(bytes.len().try_into().unwrap()).unwrap();
    if bytes.len() > 0 {
        vmo.write(bytes, 0).unwrap();
    }
    VmoFile::new(vmo, true, false, false)
}

/// Creates a new read-write `VmoFile` with the specified `content`.
///
/// ## Panics
///
/// This function panics if a VMO could not be created, or if `content` could not be written to the
/// VMO.
///
/// ## Examples
/// ```
/// // Initially empty file:
/// let empty = read_write("");
/// // File created with contents:
/// let sized = read_write("Hello world!");
/// ```
pub fn read_write(content: impl AsRef<[u8]>) -> Arc<VmoFile> {
    let bytes: &[u8] = content.as_ref();
    let vmo = zx::Vmo::create_with_opts(zx::VmoOptions::RESIZABLE, bytes.len().try_into().unwrap())
        .unwrap();
    if bytes.len() > 0 {
        vmo.write(bytes, 0).unwrap();
    }
    VmoFile::new(vmo, /*readable*/ true, /*writable*/ true, /*executable*/ false)
}

/// Implementation of a VMO-backed file in a virtual file system.
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

    /// Vmo that backs the file.
    vmo: Vmo,
}

unsafe impl Sync for VmoFile {}

impl VmoFile {
    /// Create a new VmoFile which is backed by an existing Vmo.
    ///
    /// # Arguments
    ///
    /// * `vmo` - Vmo backing this file object.
    /// * `readable` - If true, allow connections with OpenFlags::RIGHT_READABLE.
    /// * `writable` - If true, allow connections with OpenFlags::RIGHT_WRITABLE.
    /// * `executable` - If true, allow connections with OpenFlags::RIGHT_EXECUTABLE.
    pub fn new(vmo: zx::Vmo, readable: bool, writable: bool, executable: bool) -> Arc<Self> {
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
        vmo: zx::Vmo,
        readable: bool,
        writable: bool,
        executable: bool,
        inode: u64,
    ) -> Arc<Self> {
        Arc::new(VmoFile { readable, writable, executable, inode, vmo })
    }
}

impl FileLike for VmoFile {
    fn open(
        self: Arc<Self>,
        scope: ExecutionScope,
        options: FileOptions,
        object_request: ObjectRequestRef<'_>,
    ) -> Result<(), Status> {
        StreamIoConnection::spawn(scope, self, options, object_request)
    }
}

impl DirectoryEntry for VmoFile {
    fn entry_info(&self) -> EntryInfo {
        EntryInfo::new(self.inode, fio::DirentType::File)
    }

    fn open_entry(self: Arc<Self>, request: OpenRequest<'_>) -> Result<(), Status> {
        request.open_file(self)
    }
}

#[async_trait]
impl Node for VmoFile {
    async fn get_attrs(&self) -> Result<fio::NodeAttributes, Status> {
        let content_size = self.vmo.get_content_size()?;
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

    async fn get_attributes(
        &self,
        requested_attributes: fio::NodeAttributesQuery,
    ) -> Result<fio::NodeAttributes2, Status> {
        let content_size = self.vmo.get_content_size()?;

        let mut abilities = fio::Operations::GET_ATTRIBUTES | fio::Operations::UPDATE_ATTRIBUTES;
        if self.readable {
            abilities |= fio::Operations::READ_BYTES
        }
        if self.writable {
            abilities |= fio::Operations::WRITE_BYTES
        }
        if self.executable {
            abilities |= fio::Operations::EXECUTE
        }
        Ok(immutable_attributes!(
            requested_attributes,
            Immutable {
                protocols: fio::NodeProtocolKinds::FILE,
                abilities: abilities,
                content_size: content_size,
                storage_size: content_size,
                link_count: 1,
                id: self.inode,
            }
        ))
    }
}

// Required by `StreamIoConnection`.
impl GetVmo for VmoFile {
    fn get_vmo(&self) -> &zx::Vmo {
        &self.vmo
    }
}

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
        self.vmo.set_size(length)
    }

    async fn get_backing_memory(&self, flags: fio::VmoFlags) -> Result<zx::Vmo, Status> {
        // Disallow opening as both writable and executable. In addition to improving W^X
        // enforcement, this also eliminates any inconsistencies related to clones that use
        // SNAPSHOT_AT_LEAST_ON_WRITE since in that case, we cannot satisfy both requirements.
        if flags.contains(fio::VmoFlags::EXECUTE) && flags.contains(fio::VmoFlags::WRITE) {
            return Err(zx::Status::NOT_SUPPORTED);
        }

        // Logic here matches fuchsia.io requirements and matches what works for memfs.
        // Shared requests are satisfied by duplicating an handle, and private shares are
        // child VMOs.
        let vmo_rights = vmo_flags_to_rights(flags)
            | zx::Rights::BASIC
            | zx::Rights::MAP
            | zx::Rights::GET_PROPERTY;
        // Unless private sharing mode is specified, we always default to shared.
        if flags.contains(fio::VmoFlags::PRIVATE_CLONE) {
            get_as_private(&self.vmo, vmo_rights)
        } else {
            self.vmo.duplicate_handle(vmo_rights)
        }
    }

    async fn get_size(&self) -> Result<u64, Status> {
        Ok(self.vmo.get_content_size()?)
    }

    // TODO(https://fxbug.dev/42152303)
    async fn set_attrs(
        &self,
        _flags: fio::NodeAttributeFlags,
        _attrs: fio::NodeAttributes,
    ) -> Result<(), Status> {
        Err(Status::NOT_SUPPORTED)
    }

    // TODO(https://fxbug.dev/42152303)
    async fn update_attributes(
        &self,
        _attributes: fio::MutableNodeAttributes,
    ) -> Result<(), Status> {
        Err(Status::NOT_SUPPORTED)
    }

    async fn sync(&self, _mode: SyncMode) -> Result<(), Status> {
        Ok(())
    }
}

fn get_as_private(vmo: &zx::Vmo, mut rights: zx::Rights) -> Result<zx::Vmo, zx::Status> {
    // Allow for the child VMO's content size and name to be changed.
    rights |= zx::Rights::SET_PROPERTY;

    // Ensure we give out a copy-on-write child.
    let mut child_options = zx::VmoChildOptions::SNAPSHOT_AT_LEAST_ON_WRITE;
    if !rights.contains(zx::Rights::WRITE) {
        // If we don't need a writable clone, we need to add CHILD_NO_WRITE since
        // SNAPSHOT_AT_LEAST_ON_WRITE removes ZX_RIGHT_EXECUTE even if the parent VMO has it, but
        // adding CHILD_NO_WRITE will ensure EXECUTE is maintained.
        child_options |= zx::VmoChildOptions::NO_WRITE;
    } else {
        // If we need a writable child, ensure it can be resized.
        child_options |= zx::VmoChildOptions::RESIZABLE;
        rights |= zx::Rights::RESIZE;
    }

    let size = vmo.get_content_size()?;
    let new_vmo = vmo.create_child(child_options, 0, size)?;
    new_vmo.replace_handle(rights)
}
