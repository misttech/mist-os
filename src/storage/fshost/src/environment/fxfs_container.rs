// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::{Container, Filesystem, FilesystemLauncher};
use crate::crypt;
use crate::device::constants::{BLOB_VOLUME_LABEL, DATA_VOLUME_LABEL, UNENCRYPTED_VOLUME_LABEL};
use anyhow::{Context, Error};
use async_trait::async_trait;
use fs_management::filesystem::ServingMultiVolumeFilesystem;
use std::collections::HashSet;

pub struct FxfsContainer(ServingMultiVolumeFilesystem);

impl FxfsContainer {
    pub fn new(fs: ServingMultiVolumeFilesystem) -> Self {
        Self(fs)
    }
}

#[async_trait]
impl Container for FxfsContainer {
    fn fs(&mut self) -> &mut ServingMultiVolumeFilesystem {
        &mut self.0
    }

    fn into_fs(self: Box<Self>) -> ServingMultiVolumeFilesystem {
        self.0
    }

    fn blobfs_volume_label(&self) -> &'static str {
        BLOB_VOLUME_LABEL
    }

    async fn maybe_check_blob_volume(&mut self) -> Result<(), Error> {
        self.0
            .check_volume(BLOB_VOLUME_LABEL, None)
            .await
            .context("Failed to verify the blob volume")
    }

    async fn serve_data(&mut self, launcher: &FilesystemLauncher) -> Result<Filesystem, Error> {
        let mut expected =
            HashSet::from([BLOB_VOLUME_LABEL, DATA_VOLUME_LABEL, UNENCRYPTED_VOLUME_LABEL]);
        for volume in self.get_volumes().await? {
            expected.remove(volume.as_str());
        }
        if expected.is_empty() {
            match crypt::fxfs::unlock_data_volume(&mut self.0, &launcher.config).await {
                Ok(Some((crypt_service, volume_name, _))) => {
                    return Ok(Filesystem::ServingVolumeInMultiVolume(
                        Some(crypt_service),
                        volume_name,
                    ))
                }
                Ok(None) => {
                    log::warn!(
                        "could not find keybag. Perhaps the keys were shredded? \
                         Reformatting the data and unencrypted volumes."
                    );
                }
                Err(error) => {
                    launcher.report_corruption("fxfs", &error);
                    log::error!(
                        error:?;
                        "unlock_data_volume failed. Reformatting the data and unencrypted volumes."
                    );
                }
            }
        } else {
            log::warn!("The following volumes were expected but not found: {:?}", expected);
        }
        self.format_data(launcher).await
    }

    async fn format_data(&mut self, launcher: &FilesystemLauncher) -> Result<Filesystem, Error> {
        self.remove_all_non_blob_volumes().await?;

        let (crypt_service, volume_name, _) =
            crypt::fxfs::init_data_volume(&mut self.0, &launcher.config)
                .await
                .context("initializing data volume encryption")?;

        Ok(Filesystem::ServingVolumeInMultiVolume(Some(crypt_service), volume_name))
    }

    async fn shred_data(&mut self) -> Result<(), Error> {
        crypt::fxfs::shred_key_bag(
            self.0
                .volume_mut(UNENCRYPTED_VOLUME_LABEL)
                .context("Failed to find unencrypted volume")?,
        )
        .await
    }
}
