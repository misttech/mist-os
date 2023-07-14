// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::filesystems::MOUNT_PATH,
    async_trait::async_trait,
    fidl_fuchsia_fs::AdminMarker,
    fidl_fuchsia_fs_startup::{
        CompressionAlgorithm, EvictionPolicyOverride, StartOptions, StartupMarker,
    },
    fidl_fuchsia_hardware_block::BlockMarker,
    fidl_fuchsia_io as fio,
    fuchsia_component::client::{connect_to_childs_protocol, open_childs_exposed_directory},
    fuchsia_zircon as zx,
    std::path::Path,
    storage_benchmarks::{
        BlockDeviceFactory, CacheClearableFilesystem, Filesystem, FilesystemConfig,
    },
};

/// Config object for starting Memfs instances.
#[derive(Clone)]
pub struct Memfs;

#[async_trait]
impl FilesystemConfig for Memfs {
    type Filesystem = MemfsInstance;

    async fn start_filesystem(
        &self,
        _block_device_factory: &dyn BlockDeviceFactory,
    ) -> MemfsInstance {
        MemfsInstance::new().await
    }

    fn name(&self) -> String {
        "memfs".to_owned()
    }
}

pub struct MemfsInstance {}

impl MemfsInstance {
    async fn new() -> Self {
        let startup = connect_to_childs_protocol::<StartupMarker>("memfs".to_string(), None)
            .await
            .expect("Failed to connect to memfs");
        let options = StartOptions {
            read_only: false,
            verbose: false,
            fsck_after_every_transaction: false,
            write_compression_algorithm: CompressionAlgorithm::ZstdChunked,
            write_compression_level: 0,
            cache_eviction_policy_override: EvictionPolicyOverride::None,
            allow_delivery_blobs: false,
        };
        // Memfs doesn't need a block device but FIDL prevents passing an invalid handle.
        let (device_client_end, _) = fidl::endpoints::create_endpoints::<BlockMarker>();
        startup
            .start(device_client_end, options)
            .await
            .unwrap()
            .map_err(zx::Status::from_raw)
            .unwrap();

        let exposed_dir = open_childs_exposed_directory("memfs", None)
            .await
            .expect("Failed to connect to memfs's exposed directory");

        let (root_dir, server_end) = fidl::endpoints::create_endpoints();
        exposed_dir
            .open(
                fio::OpenFlags::RIGHT_READABLE
                    | fio::OpenFlags::POSIX_EXECUTABLE
                    | fio::OpenFlags::POSIX_WRITABLE,
                fio::ModeType::empty(),
                "root",
                fidl::endpoints::ServerEnd::new(server_end.into_channel()),
            )
            .expect("Failed to open memfs's root");
        let namespace = fdio::Namespace::installed().expect("Failed to get local namespace");
        namespace.bind(MOUNT_PATH, root_dir).expect("Failed to bind memfs");

        Self {}
    }
}

#[async_trait]
impl Filesystem for MemfsInstance {
    async fn shutdown(self) {
        let admin = connect_to_childs_protocol::<AdminMarker>("memfs".to_string(), None)
            .await
            .expect("Failed to connect to memfs Admin");
        admin.shutdown().await.expect("Failed to shutdown memfs");

        let namespace = fdio::Namespace::installed().expect("Failed to get local namespace");
        namespace.unbind(MOUNT_PATH).expect("Failed to unbind memfs");
    }

    fn benchmark_dir(&self) -> &Path {
        Path::new(MOUNT_PATH)
    }
}

// TODO(http://fxbug.dev/130451) Memfs shouldn't implemented CacheClearableFilesystem.
#[async_trait]
impl CacheClearableFilesystem for MemfsInstance {
    async fn clear_cache(&mut self) {}
}

#[cfg(test)]
mod tests {
    use {
        super::Memfs,
        std::{
            fs::OpenOptions,
            io::{Read, Write},
        },
        storage_benchmarks::{
            block_device::PanickingBlockDeviceFactory, Filesystem, FilesystemConfig,
        },
    };

    #[fuchsia::test]
    async fn start_memfs() {
        const FILE_CONTENTS: &str = "file-contents";
        let block_device_factory = PanickingBlockDeviceFactory::new();
        let fs = Memfs.start_filesystem(&block_device_factory).await;

        let file_path = fs.benchmark_dir().join("filename");
        {
            let mut file =
                OpenOptions::new().create_new(true).write(true).open(&file_path).unwrap();
            file.write_all(FILE_CONTENTS.as_bytes()).unwrap();
        }
        {
            let mut file = OpenOptions::new().read(true).open(&file_path).unwrap();
            let mut buf = [0u8; FILE_CONTENTS.len()];
            file.read_exact(&mut buf).unwrap();
            assert_eq!(std::str::from_utf8(&buf).unwrap(), FILE_CONTENTS);
        }
        fs.shutdown().await;
    }
}
