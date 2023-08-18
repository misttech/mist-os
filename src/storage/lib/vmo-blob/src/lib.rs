// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::{anyhow, Error},
    async_trait::async_trait,
    fidl::{endpoints::ServerEnd, AsHandleRef},
    fidl_fuchsia_io as fio, fuchsia_zircon as zx,
    std::sync::{Arc, OnceLock},
    tracing::error,
    vfs::{
        attributes,
        common::rights_to_posix_mode_bits,
        directory::entry::{DirectoryEntry, EntryInfo},
        execution_scope::ExecutionScope,
        file::{File, FileOptions, GetVmo, StreamIoConnection, SyncMode},
        path::Path,
        ToObjectRequest,
    },
};

/// Mimics the c++ blobfs block size.
const BLOCK_SIZE: u64 = 8192;
static VMEX_RESOURCE: OnceLock<zx::Resource> = OnceLock::new();

/// Attempt to initialize the vmex resource. Without a vmex, attempts to get the backing memory
/// of a blob with executable rights will fail with NOT_SUPPORTED.
pub fn init_vmex_resource(vmex: zx::Resource) -> Result<(), Error> {
    VMEX_RESOURCE.set(vmex).map_err(|_| anyhow!(zx::Status::ALREADY_BOUND))
}

/// `VmoBlob` is a wrapper around the fuchsia.io/File protocol. Represents an immutable blob on
/// Fxfs. Clients will use this library to read and execute blobs.
pub struct VmoBlob {
    vmo: zx::Vmo,
}

impl VmoBlob {
    pub fn new(vmo: zx::Vmo) -> Self {
        Self { vmo }
    }
}

impl GetVmo for VmoBlob {
    fn get_vmo(&self) -> &zx::Vmo {
        &self.vmo
    }
}

/// Implement VFS pseudo-directory entry for a blob.
impl DirectoryEntry for VmoBlob {
    fn open(
        self: Arc<Self>,
        scope: ExecutionScope,
        flags: fio::OpenFlags,
        path: Path,
        server_end: ServerEnd<fio::NodeMarker>,
    ) {
        flags.to_object_request(server_end).spawn(&scope.clone(), move |object_request| {
            Box::pin(async move {
                if !path.is_empty() {
                    return Err(zx::Status::NOT_DIR);
                }
                object_request.create_connection(scope, self, flags, StreamIoConnection::create)
            })
        });
    }

    fn entry_info(&self) -> EntryInfo {
        EntryInfo::new(fio::INO_UNKNOWN, fio::DirentType::File)
    }
}

#[async_trait]
impl vfs::node::Node for VmoBlob {
    async fn get_attrs(&self) -> Result<fio::NodeAttributes, zx::Status> {
        let content_size = self.get_size().await?;
        Ok(fio::NodeAttributes {
            mode: fio::MODE_TYPE_FILE
                | rights_to_posix_mode_bits(/*r*/ true, /*w*/ false, /*x*/ true),
            id: fio::INO_UNKNOWN,
            content_size,
            // TODO(https://fxbug.dev/295550170): Get storage_size from fxblob.
            storage_size: ((content_size + BLOCK_SIZE - 1) / BLOCK_SIZE) * BLOCK_SIZE,
            link_count: 1,
            creation_time: 0,
            modification_time: 0,
        })
    }

    async fn get_attributes(
        &self,
        requested_attributes: fio::NodeAttributesQuery,
    ) -> Result<fio::NodeAttributes2, zx::Status> {
        let content_size = self.get_size().await?;
        Ok(attributes!(
            requested_attributes,
            Mutable { creation_time: 0, modification_time: 0, mode: 0, uid: 0, gid: 0, rdev: 0 },
            Immutable {
                protocols: fio::NodeProtocolKinds::FILE,
                abilities: fio::Operations::GET_ATTRIBUTES
                    | fio::Operations::READ_BYTES
                    | fio::Operations::EXECUTE,
                content_size: content_size,
                // TODO(https://fxbug.dev/295550170): Get storage_size from fxblob.
                storage_size: ((content_size + BLOCK_SIZE - 1) / BLOCK_SIZE) * BLOCK_SIZE,
                link_count: 1,
                id: fio::INO_UNKNOWN,
            }
        ))
    }
}

/// Implement VFS trait so blobs can be accessed as files.
#[async_trait]
impl File for VmoBlob {
    fn executable(&self) -> bool {
        true
    }

    async fn open_file(&self, _options: &FileOptions) -> Result<(), zx::Status> {
        Ok(())
    }

    async fn truncate(&self, _length: u64) -> Result<(), zx::Status> {
        Err(zx::Status::ACCESS_DENIED)
    }

    async fn get_size(&self) -> Result<u64, zx::Status> {
        self.vmo.get_content_size()
    }

    async fn get_backing_memory(&self, flags: fio::VmoFlags) -> Result<zx::Vmo, zx::Status> {
        let vmo_size = self.get_size().await?;
        if vmo_size == 0 {
            // Mimics c++ blobfs behavior.
            return Err(zx::Status::BAD_STATE);
        }
        // We do not support exact/duplicate sharing mode.
        if flags.contains(fio::VmoFlags::SHARED_BUFFER) {
            error!("get_backing_memory does not support exact sharing mode!");
            return Err(zx::Status::NOT_SUPPORTED);
        }
        // We only support the combination of WRITE when a private COW clone is explicitly
        // specified. This implicitly restricts any mmap call that attempts to use MAP_SHARED +
        // PROT_WRITE.
        if flags.contains(fio::VmoFlags::WRITE) && !flags.contains(fio::VmoFlags::PRIVATE_CLONE) {
            error!("get_buffer only supports VmoFlags::WRITE with VmoFlags::PRIVATE_CLONE!");
            return Err(zx::Status::NOT_SUPPORTED);
        }

        let mut child_options = zx::VmoChildOptions::SNAPSHOT_AT_LEAST_ON_WRITE;
        // By default, SNAPSHOT includes WRITE, so we explicitly remove it if not required.
        if !flags.contains(fio::VmoFlags::WRITE) {
            child_options |= zx::VmoChildOptions::NO_WRITE
        }
        let mut child_vmo = self.vmo.create_child(child_options, 0, vmo_size)?;

        if flags.contains(fio::VmoFlags::EXECUTE) {
            // TODO(https://fxbug.dev/293606235): Filter out other flags.
            child_vmo = child_vmo
                .replace_as_executable(VMEX_RESOURCE.get().ok_or(zx::Status::NOT_SUPPORTED)?)?;
        }

        Ok(child_vmo)
    }

    async fn set_attrs(
        &self,
        _flags: fio::NodeAttributeFlags,
        _attrs: fio::NodeAttributes,
    ) -> Result<(), zx::Status> {
        Err(zx::Status::ACCESS_DENIED)
    }

    async fn update_attributes(
        &self,
        _attributes: fio::MutableNodeAttributes,
    ) -> Result<(), zx::Status> {
        Err(zx::Status::ACCESS_DENIED)
    }

    async fn sync(&self, _mode: SyncMode) -> Result<(), zx::Status> {
        Ok(())
    }

    fn event(&self) -> Result<Option<zx::Event>, zx::Status> {
        let event = zx::Event::create();
        // The file is immediately readable (see `fuchsia.io2.File.Describe`).
        event.signal_handle(zx::Signals::empty(), zx::Signals::USER_0)?;
        Ok(Some(event))
    }
}
