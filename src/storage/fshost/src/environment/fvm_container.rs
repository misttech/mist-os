// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::{Container, Filesystem, FilesystemLauncher};
use crate::device::constants::DATA_TYPE_GUID;
use anyhow::Error;
use async_trait::async_trait;
use crypt_policy::get_policy;
use fidl_fuchsia_fs_startup::{CreateOptions, MountOptions};
use fs_management::filesystem::ServingMultiVolumeFilesystem;
use fs_management::format::constants::{
    BLOBFS_PARTITION_LABEL, DATA_PARTITION_LABEL, LEGACY_DATA_PARTITION_LABEL,
};
use zxcrypt_crypt::with_crypt_service;

pub struct FvmContainer(ServingMultiVolumeFilesystem, bool);

impl FvmContainer {
    pub fn new(fs: ServingMultiVolumeFilesystem, is_ramdisk: bool) -> Self {
        Self(fs, is_ramdisk)
    }
}

#[async_trait]
impl Container for FvmContainer {
    fn fs(&mut self) -> &mut ServingMultiVolumeFilesystem {
        &mut self.0
    }

    fn into_fs(self: Box<Self>) -> ServingMultiVolumeFilesystem {
        self.0
    }

    fn blobfs_volume_label(&self) -> &'static str {
        BLOBFS_PARTITION_LABEL
    }

    async fn serve_data(&mut self, launcher: &FilesystemLauncher) -> Result<Filesystem, Error> {
        fn check_volumes(volumes: Vec<String>) -> Option<String> {
            let mut found_blobfs = false;
            let mut data_label = None;
            for volume in volumes {
                match volume.as_str() {
                    BLOBFS_PARTITION_LABEL if !found_blobfs => found_blobfs = true,
                    DATA_PARTITION_LABEL | LEGACY_DATA_PARTITION_LABEL if data_label.is_none() => {
                        data_label = Some(volume.clone());
                    }
                    _ => return None,
                }
            }
            if found_blobfs && data_label.is_some() {
                data_label
            } else {
                None
            }
        }

        if let Some(data_label) = check_volumes(self.get_volumes().await?) {
            let format = launcher.config.data_filesystem_format.as_str();
            let uri = format!("#meta/{format}.cm");
            let open_volume = |crypt| {
                self.0.open_volume(
                    &data_label,
                    MountOptions { uri: Some(uri), crypt, ..MountOptions::default() },
                )
            };

            match if launcher
                .requires_zxcrypt(launcher.config.data_filesystem_format.as_str().into(), self.1)
            {
                let policy = get_policy().await?;
                with_crypt_service(policy, |crypt| open_volume(Some(crypt))).await
            } else {
                open_volume(None).await
            } {
                Ok(_) => return Ok(Filesystem::ServingVolumeInMultiVolume(None, data_label)),
                Err(error) => match error.root_cause().downcast_ref::<zx::Status>() {
                    Some(status) if status == &zx::Status::WRONG_TYPE => {
                        // Assume that it's more likely that this is because of FDR, or after
                        // flashing rather than due to corruption, so avoid the crash report.
                        tracing::info!("Data volume unexpected type. Reformatting.");
                    }
                    _ => {
                        launcher.report_corruption(format, &error);
                        tracing::error!(
                            ?error,
                            "Unable to mount {format} data volume. Reformatting the data volumes."
                        );
                    }
                },
            };
        }

        self.format_data(launcher).await
    }

    async fn format_data(&mut self, launcher: &FilesystemLauncher) -> Result<Filesystem, Error> {
        self.remove_all_non_blob_volumes().await?;

        let uri = format!("#meta/{}.cm", launcher.config.data_filesystem_format);
        let create_volume = |crypt| async {
            self.0
                .create_volume(
                    DATA_PARTITION_LABEL,
                    CreateOptions { type_guid: Some(DATA_TYPE_GUID), ..CreateOptions::default() },
                    MountOptions { crypt, uri: Some(uri), ..Default::default() },
                )
                .await
                .map(|_| {
                    Filesystem::ServingVolumeInMultiVolume(None, DATA_PARTITION_LABEL.to_string())
                })
        };

        if launcher.requires_zxcrypt(launcher.config.data_filesystem_format.as_str().into(), self.1)
        {
            let policy = get_policy().await?;
            with_crypt_service(policy, |crypt| create_volume(Some(crypt))).await
        } else {
            create_volume(None).await
        }
    }

    async fn shred_data(&mut self) -> Result<(), Error> {
        // TODO(https://fxbug.dev/367171959): Implement this
        todo!();
    }
}
