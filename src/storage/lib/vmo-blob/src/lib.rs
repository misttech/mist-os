// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{anyhow, Error};
use fidl_fuchsia_io as fio;
use log::error;
use std::sync::{Arc, OnceLock};
use vfs::directory::entry::{EntryInfo, GetEntryInfo};
use vfs::file::{File, FileOptions, GetVmo, SyncMode};
use vfs::immutable_attributes;

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
    pub fn new(vmo: zx::Vmo) -> Arc<Self> {
        Arc::new(Self { vmo })
    }
}

impl GetVmo for VmoBlob {
    fn get_vmo(&self) -> &zx::Vmo {
        &self.vmo
    }
}

impl GetEntryInfo for VmoBlob {
    fn entry_info(&self) -> EntryInfo {
        EntryInfo::new(fio::INO_UNKNOWN, fio::DirentType::File)
    }
}

impl vfs::node::Node for VmoBlob {
    async fn get_attributes(
        &self,
        requested_attributes: fio::NodeAttributesQuery,
    ) -> Result<fio::NodeAttributes2, zx::Status> {
        let content_size = self.get_size().await?;
        Ok(immutable_attributes!(
            requested_attributes,
            Immutable {
                protocols: fio::NodeProtocolKinds::FILE,
                abilities: fio::Operations::GET_ATTRIBUTES
                    | fio::Operations::READ_BYTES
                    | fio::Operations::EXECUTE,
                content_size: content_size,
                // TODO(https://fxbug.dev/295550170): Get storage_size from fxblob.
                storage_size: content_size.div_ceil(BLOCK_SIZE) * BLOCK_SIZE,
            }
        ))
    }
}

/// Implement VFS trait so blobs can be accessed as files.
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
        let mut child_vmo =
            self.vmo.create_child(child_options, 0, self.vmo.get_content_size()?)?;

        if flags.contains(fio::VmoFlags::EXECUTE) {
            // TODO(https://fxbug.dev/293606235): Filter out other flags.
            child_vmo = child_vmo
                .replace_as_executable(VMEX_RESOURCE.get().ok_or(zx::Status::NOT_SUPPORTED)?)?;
        }

        Ok(child_vmo)
    }

    async fn update_attributes(
        &self,
        _attributes: fio::MutableNodeAttributes,
    ) -> Result<(), zx::Status> {
        Err(zx::Status::NOT_SUPPORTED)
    }

    async fn sync(&self, _mode: SyncMode) -> Result<(), zx::Status> {
        Ok(())
    }
}
