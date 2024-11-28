// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{Context, Result};
use async_trait::async_trait;
use blackout_target::static_tree::{DirectoryEntry, EntryDistribution};
use blackout_target::{Test, TestServer};
use fidl::endpoints::{create_proxy, Proxy as _};
use fidl_fuchsia_device::{ControllerMarker, ControllerProxy};
use fidl_fuchsia_fs_startup::{CreateOptions, MountOptions};
use fidl_fuchsia_fxfs::{CryptManagementMarker, CryptMarker, KeyPurpose};
use fidl_fuchsia_hardware_block_volume::VolumeManagerMarker;
use fidl_fuchsia_io as fio;
use fs_management::filesystem::{ServingMultiVolumeFilesystem, ServingSingleVolumeFilesystem};
use fs_management::format::DiskFormat;
use fs_management::{Fxfs, Minfs};
use fuchsia_component::client::{connect_to_protocol, connect_to_protocol_at_path};
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use storage_isolated_driver_manager::{
    create_random_guid, find_block_device, into_guid, wait_for_block_device, BlockDeviceMatcher,
    Guid,
};

const DATA_FILESYSTEM_FORMAT: &'static str = std::env!("DATA_FILESYSTEM_FORMAT");

const DATA_KEY: [u8; 32] = [
    0xcf, 0x9e, 0x45, 0x2a, 0x22, 0xa5, 0x70, 0x31, 0x33, 0x3b, 0x4d, 0x6b, 0x6f, 0x78, 0x58, 0x29,
    0x04, 0x79, 0xc7, 0xd6, 0xa9, 0x4b, 0xce, 0x82, 0x04, 0x56, 0x5e, 0x82, 0xfc, 0xe7, 0x37, 0xa8,
];

const METADATA_KEY: [u8; 32] = [
    0x0f, 0x4d, 0xca, 0x6b, 0x35, 0x0e, 0x85, 0x6a, 0xb3, 0x8c, 0xdd, 0xe9, 0xda, 0x0e, 0xc8, 0x22,
    0x8e, 0xea, 0xd8, 0x05, 0xc4, 0xc9, 0x0b, 0xa8, 0xd8, 0x85, 0x87, 0x50, 0x75, 0x40, 0x1c, 0x4c,
];

/// This type guid is only used if the test has to create the gpt partition itself. Otherwise, only
/// the label is used to find the partition.
const BLACKOUT_TYPE_GUID: &Guid = &[
    0x68, 0x45, 0x23, 0x01, 0xab, 0x89, 0xef, 0xcd, 0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef,
];

const GPT_PARTITION_SIZE: u64 = 60 * 1024 * 1024;

async fn find_partition_helper(
    device_path: Option<String>,
    device_label: String,
) -> Result<ControllerProxy> {
    let mut partition_path = if let Some(path) = device_path {
        tracing::info!("fs-tree blackout using provided path for load gen");
        path.into()
    } else {
        tracing::info!("fs-tree blackout finding gpt for load gen");
        find_block_device(&[BlockDeviceMatcher::Name(&device_label)])
            .await
            .context("finding block device")?
    };
    partition_path.push("device_controller");
    tracing::info!(?partition_path, "found partition to use");
    connect_to_protocol_at_path::<ControllerMarker>(partition_path.to_str().unwrap())
        .context("connecting to provided path")
}

#[derive(Copy, Clone)]
struct FsTree;

impl FsTree {
    async fn setup_crypt_service(&self) -> Result<()> {
        static INITIALIZED: AtomicBool = AtomicBool::new(false);
        if INITIALIZED.load(Ordering::SeqCst) {
            return Ok(());
        }
        let crypt_management = connect_to_protocol::<CryptManagementMarker>()?;
        let wrapping_key_id_0 = [0; 16];
        let mut wrapping_key_id_1 = [0; 16];
        wrapping_key_id_1[0] = 1;
        crypt_management
            .add_wrapping_key(&wrapping_key_id_0, &DATA_KEY)
            .await?
            .map_err(zx::Status::from_raw)?;
        crypt_management
            .add_wrapping_key(&wrapping_key_id_1, &METADATA_KEY)
            .await?
            .map_err(zx::Status::from_raw)?;
        crypt_management
            .set_active_key(KeyPurpose::Data, &wrapping_key_id_0)
            .await?
            .map_err(zx::Status::from_raw)?;
        crypt_management
            .set_active_key(KeyPurpose::Metadata, &wrapping_key_id_1)
            .await?
            .map_err(zx::Status::from_raw)?;
        INITIALIZED.store(true, Ordering::SeqCst);
        Ok(())
    }

    async fn setup_fxfs(&self, controller: ControllerProxy) -> Result<()> {
        let mut fxfs = Fxfs::new(controller);
        fxfs.format().await?;
        let mut fs = fxfs.serve_multi_volume().await?;
        self.setup_crypt_service().await?;
        let crypt_service = Some(
            connect_to_protocol::<CryptMarker>()?.into_channel().unwrap().into_zx_channel().into(),
        );
        fs.create_volume(
            "default",
            CreateOptions::default(),
            MountOptions { crypt: crypt_service, ..MountOptions::default() },
        )
        .await?;
        Ok(())
    }

    async fn setup_minfs(&self, controller: ControllerProxy) -> Result<()> {
        let mut minfs = Minfs::new(controller);
        minfs.format().await.context("failed to format minfs")?;
        Ok(())
    }

    async fn serve_fxfs(&self, controller: ControllerProxy) -> Result<FsInstance> {
        let mut fxfs = Fxfs::new(controller);
        let mut fs = fxfs.serve_multi_volume().await?;
        self.setup_crypt_service().await?;
        let crypt_service = Some(
            connect_to_protocol::<CryptMarker>()?.into_channel().unwrap().into_zx_channel().into(),
        );
        let _ = fs
            .open_volume(
                "default",
                MountOptions { crypt: crypt_service, ..MountOptions::default() },
            )
            .await?;
        Ok(FsInstance::Fxfs(fs))
    }

    async fn serve_minfs(&self, controller: ControllerProxy) -> Result<FsInstance> {
        let mut minfs = Minfs::new(controller);
        let fs = minfs.serve().await?;
        Ok(FsInstance::Minfs(fs))
    }

    async fn verify_fxfs(&self, controller: ControllerProxy) -> Result<()> {
        let mut fxfs = Fxfs::new(controller);
        fxfs.fsck().await.context("overall fsck")?;

        // Also check the volume we created.
        let mut fs = fxfs.serve_multi_volume().await.context("failed to serve")?;
        self.setup_crypt_service().await?;
        let crypt_service = Some(
            connect_to_protocol::<CryptMarker>()?.into_channel().unwrap().into_zx_channel().into(),
        );
        fs.check_volume("default", crypt_service).await.context("default volume check")?;
        Ok(())
    }

    async fn verify_minfs(&self, controller: ControllerProxy) -> Result<()> {
        let mut minfs = Minfs::new(controller);
        minfs.fsck().await?;
        Ok(())
    }
}

enum FsInstance {
    Fxfs(ServingMultiVolumeFilesystem),
    Minfs(ServingSingleVolumeFilesystem),
}

impl FsInstance {
    fn root(&self) -> &fio::DirectoryProxy {
        match &self {
            FsInstance::Fxfs(fs) => {
                let vol = fs.volume("default").expect("failed to get default volume");
                vol.root()
            }
            FsInstance::Minfs(fs) => fs.root(),
        }
    }
}

#[async_trait]
impl Test for FsTree {
    async fn setup(
        self: Arc<Self>,
        device_label: String,
        device_path: Option<String>,
        _seed: u64,
    ) -> Result<()> {
        tracing::info!(device_label, device_path, "setting up fs-tree blackout test");
        let mut partition_path = if let Some(path) = device_path {
            tracing::info!("fs-tree blackout test using provided path");
            path.into()
        } else if let Ok(path) = find_block_device(&[BlockDeviceMatcher::Name(&device_label)]).await
        {
            tracing::info!("fs-tree blackout test found existing partition");
            path
        } else {
            tracing::info!("fs-tree blackout test setting up gpt");
            let mut gpt_block_path =
                find_block_device(&[BlockDeviceMatcher::ContentsMatch(DiskFormat::Gpt)])
                    .await
                    .context("finding gpt device failed")?;
            gpt_block_path.push("device_controller");
            let gpt_block_controller =
                connect_to_protocol_at_path::<ControllerMarker>(gpt_block_path.to_str().unwrap())
                    .context("connecting to block controller")?;
            let gpt_path = gpt_block_controller
                .get_topological_path()
                .await
                .context("get_topo fidl error")?
                .map_err(zx::Status::from_raw)
                .context("get_topo failed")?;
            let gpt_controller = connect_to_protocol_at_path::<ControllerMarker>(&format!(
                "{}/gpt/device_controller",
                gpt_path
            ))
            .context("connecting to gpt controller")?;

            let (volume_manager, server) = create_proxy::<VolumeManagerMarker>();
            gpt_controller
                .connect_to_device_fidl(server.into_channel())
                .context("connecting to gpt fidl")?;
            let slice_size = {
                let (status, info) =
                    volume_manager.get_info().await.context("get_info fidl error")?;
                zx::ok(status).context("get_info returned error")?;
                info.unwrap().slice_size
            };
            let slice_count = GPT_PARTITION_SIZE / slice_size;
            let instance_guid = into_guid(create_random_guid());
            let status = volume_manager
                .allocate_partition(
                    slice_count,
                    &into_guid(BLACKOUT_TYPE_GUID.clone()),
                    &instance_guid,
                    &device_label,
                    0,
                )
                .await
                .context("allocating test partition fidl error")?;
            zx::ok(status).context("allocating test partition returned error")?;

            wait_for_block_device(&[
                BlockDeviceMatcher::Name(&device_label),
                BlockDeviceMatcher::TypeGuid(&BLACKOUT_TYPE_GUID),
            ])
            .await
            .context("waiting for new gpt partition")?
        };
        partition_path.push("device_controller");
        tracing::info!(?partition_path, "found partition to use");
        let partition_controller =
            connect_to_protocol_at_path::<ControllerMarker>(partition_path.to_str().unwrap())
                .context("connecting to provided path")?;

        match DATA_FILESYSTEM_FORMAT {
            "fxfs" => self.setup_fxfs(partition_controller).await?,
            "minfs" => self.setup_minfs(partition_controller).await?,
            _ => panic!("Unsupported filesystem"),
        }

        tracing::info!("setup complete");
        Ok(())
    }

    async fn test(
        self: Arc<Self>,
        device_label: String,
        device_path: Option<String>,
        seed: u64,
    ) -> Result<()> {
        tracing::info!(device_label, device_path, "fs-tree blackout test running load gen");
        let partition_controller = find_partition_helper(device_path, device_label)
            .await
            .context("test failed to find partition")?;
        let fs = match DATA_FILESYSTEM_FORMAT {
            "fxfs" => self.serve_fxfs(partition_controller).await?,
            "minfs" => self.serve_minfs(partition_controller).await?,
            _ => panic!("Unsupported filesystem"),
        };

        tracing::info!("generating load");
        let mut rng = StdRng::seed_from_u64(seed);
        loop {
            tracing::debug!("generating tree");
            let dist = EntryDistribution::new(6);
            let tree: DirectoryEntry = rng.sample(&dist);
            tracing::debug!("generated tree: {:?}", tree);
            let tree_name = tree.get_name();
            tracing::debug!("writing tree");
            tree.write_tree_at(fs.root()).await.context("failed to write directory tree")?;
            // now try renaming the tree root
            let tree_name2 = format!("{}-renamed", tree_name);
            tracing::debug!("moving tree");
            fuchsia_fs::directory::rename(fs.root(), &tree_name, &tree_name2)
                .await
                .context("failed to rename directory tree")?;
            // then try deleting the entire thing.
            tracing::debug!("deleting tree");
            fuchsia_fs::directory::remove_dir_recursive(fs.root(), &tree_name2)
                .await
                .context("failed to delete directory tree")?;
        }
    }

    async fn verify(
        self: Arc<Self>,
        device_label: String,
        device_path: Option<String>,
        _seed: u64,
    ) -> Result<()> {
        tracing::info!(
            device_label,
            device_path,
            "fs-tree blackout test verifying disk consistency"
        );
        let partition_controller = find_partition_helper(device_path, device_label)
            .await
            .context("verify failed to find partition")?;

        match DATA_FILESYSTEM_FORMAT {
            "fxfs" => self.verify_fxfs(partition_controller).await?,
            "minfs" => self.verify_minfs(partition_controller).await?,
            _ => panic!("Unsupported filesystem"),
        }

        tracing::info!("verification complete");
        Ok(())
    }
}

#[fuchsia::main]
async fn main() -> Result<()> {
    let server = TestServer::new(FsTree)?;
    server.serve().await;

    Ok(())
}
