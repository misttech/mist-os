// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::common::cmd::{ManifestParams, OemFile};
use crate::file_resolver::FileResolver;
use crate::manifest::v1::{
    FlashManifest as FlashManifestV1, Partition as PartitionV1, Product as ProductV1,
};
use crate::manifest::v2::FlashManifest as FlashManifestV2;
use crate::manifest::{Boot, Flash, Unlock};
use crate::util::Event;
use anyhow::Result;
use async_trait::async_trait;
use ffx_fastboot_interface::fastboot_interface::FastbootInterface;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc::Sender;

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct FlashManifest {
    pub hw_revision: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub credentials: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub products: Vec<Product>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct Product {
    pub name: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bootloader_partitions: Vec<Partition>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub partitions: Vec<Partition>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub oem_files: Vec<ExplicitOemFile>,
    #[serde(default)]
    pub requires_unlock: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Partition {
    pub name: String,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub condition: Option<Condition>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Condition {
    pub variable: String,
    pub value: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ExplicitOemFile {
    pub command: String,
    pub path: String,
}

impl From<&ExplicitOemFile> for OemFile {
    fn from(f: &ExplicitOemFile) -> OemFile {
        OemFile::new(f.command.clone(), f.path.clone())
    }
}

impl From<&Partition> for PartitionV1 {
    fn from(p: &Partition) -> PartitionV1 {
        PartitionV1::new(
            p.name.clone(),
            p.path.clone(),
            p.condition.as_ref().map(|c| c.variable.clone()),
            p.condition.as_ref().map(|c| c.value.clone()),
        )
    }
}

impl From<&Product> for ProductV1 {
    fn from(p: &Product) -> ProductV1 {
        ProductV1 {
            name: p.name.clone(),
            bootloader_partitions: p.bootloader_partitions.iter().map(|p| p.into()).collect(),
            partitions: p.partitions.iter().map(|p| p.into()).collect(),
            oem_files: p.oem_files.iter().map(|f| f.into()).collect(),
            requires_unlock: p.requires_unlock,
        }
    }
}

impl From<&FlashManifest> for FlashManifestV2 {
    fn from(p: &FlashManifest) -> FlashManifestV2 {
        FlashManifestV2 {
            hw_revision: p.hw_revision.clone(),
            credentials: p.credentials.iter().map(|c| c.clone()).collect(),
            v1: FlashManifestV1(p.products.iter().map(|p| p.into()).collect()),
        }
    }
}

#[async_trait(?Send)]
impl Flash for FlashManifest {
    #[tracing::instrument(skip(file_resolver, cmd))]
    async fn flash<F, T>(
        &self,
        messenger: &Sender<Event>,
        file_resolver: &mut F,
        fastboot_interface: &mut T,
        cmd: ManifestParams,
    ) -> Result<()>
    where
        F: FileResolver + Sync,
        T: FastbootInterface,
    {
        let v2: FlashManifestV2 = self.into();
        v2.flash(messenger, file_resolver, fastboot_interface, cmd).await
    }
}

#[async_trait(?Send)]
impl Unlock for FlashManifest {
    async fn unlock<F, T>(
        &self,
        messenger: &Sender<Event>,
        file_resolver: &mut F,
        fastboot_interface: &mut T,
    ) -> Result<()>
    where
        F: FileResolver + Sync,
        T: FastbootInterface,
    {
        let v2: FlashManifestV2 = self.into();
        v2.unlock(messenger, file_resolver, fastboot_interface).await
    }
}

#[async_trait(?Send)]
impl Boot for FlashManifest {
    async fn boot<F, T>(
        &self,
        messenger: Sender<Event>,
        file_resolver: &mut F,
        slot: String,
        fastboot_interface: &mut T,
        cmd: ManifestParams,
    ) -> Result<()>
    where
        F: FileResolver + Sync,
        T: FastbootInterface,
    {
        let v2: FlashManifestV2 = self.into();
        v2.boot(messenger, file_resolver, slot, fastboot_interface, cmd).await
    }
}

////////////////////////////////////////////////////////////////////////////////
// tests
#[cfg(test)]
mod test {
    use super::*;
    use crate::common::vars::{IS_USERSPACE_VAR, MAX_DOWNLOAD_SIZE_VAR, REVISION_VAR};
    use crate::test::{setup, TestResolver};
    use serde_json::{from_str, json};
    use std::path::PathBuf;
    use tempfile::NamedTempFile;
    use tokio::sync::mpsc;

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_minimal_manifest_succeeds() -> Result<()> {
        let tmp_file = NamedTempFile::new().expect("tmp access failed");
        let tmp_file_name = tmp_file.path().to_string_lossy().to_string();

        // Setup image files for flashing
        let tmp_img_files = [(); 4].map(|_| NamedTempFile::new().expect("tmp access failed"));
        let tmp_img_file_paths = tmp_img_files
            .iter()
            .map(|tmp| tmp.path().to_str().expect("non-unicode tmp path"))
            .collect::<Vec<&str>>();

        let manifest = json!({
            "hw_revision": "rev_test",
            "products": [
                {
                    "name": "zedboot",
                    "partitions": [
                        {"name": "test1", "path":tmp_img_file_paths[0]  },
                        {"name": "test2", "path":tmp_img_file_paths[1]  },
                        {"name": "test3", "path":tmp_img_file_paths[2]  },
                        {
                            "name": "test4",
                            "path":tmp_img_file_paths[3],
                            "condition": {
                                "variable": "var",
                                "value": "val"
                            }
                        }
                    ]
                }
            ]
        });

        let v: FlashManifest = from_str(&manifest.to_string())?;
        let (state, mut proxy) = setup();
        {
            let mut state = state.lock().unwrap();
            state.set_var(REVISION_VAR.to_string(), "rev_test-b4".to_string());
            state.set_var(IS_USERSPACE_VAR.to_string(), "no".to_string());
            state.set_var("var".to_string(), "val".to_string());
            state.set_var(MAX_DOWNLOAD_SIZE_VAR.to_string(), "8192".to_string());
        }
        let (client, _server) = mpsc::channel(100);
        v.flash(
            &client,
            &mut TestResolver::new(),
            &mut proxy,
            ManifestParams {
                manifest: Some(PathBuf::from(tmp_file_name)),
                product: "zedboot".to_string(),
                ..Default::default()
            },
        )
        .await
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_full_manifest_succeeds() -> Result<()> {
        let tmp_file = NamedTempFile::new().expect("tmp access failed");
        let tmp_file_name = tmp_file.path().to_string_lossy().to_string();

        // Setup image files for flashing
        let tmp_img_files = [(); 4].map(|_| NamedTempFile::new().expect("tmp access failed"));
        let tmp_img_file_paths = tmp_img_files
            .iter()
            .map(|tmp| tmp.path().to_str().expect("non-unicode tmp path"))
            .collect::<Vec<&str>>();

        let manifest = json!({
            "hw_revision": "rev_test",
            "products": [
                {
                    "name": "zedboot",
                    "bootloader_partitions": [
                        {"name": "test1", "path": tmp_img_file_paths[0] },
                        {"name": "test2", "path": tmp_img_file_paths[1] },
                        {"name": "test3", "path": tmp_img_file_paths[2] },
                        {
                            "name": "test4",
                            "path":  tmp_img_file_paths[3] ,
                            "condition": {
                                "variable": "var",
                                "value": "val"
                            }
                        }
                    ],
                    "partitions": [
                        {"name": "test1", "path": tmp_img_file_paths[0] },
                        {"name": "test2", "path": tmp_img_file_paths[1] },
                        {"name": "test3", "path": tmp_img_file_paths[2] },
                        {"name": "test4", "path": tmp_img_file_paths[3] }
                    ],
                    "oem_files": [
                        {"command": "test1", "path": tmp_img_file_paths[0] },
                        {"command": "test2", "path": tmp_img_file_paths[1] },
                        {"command": "test3", "path": tmp_img_file_paths[2] },
                        {"command": "test4", "path": tmp_img_file_paths[3] }
                    ]
                }
            ]
        });

        let v: FlashManifest = from_str(&manifest.to_string())?;
        let (state, mut proxy) = setup();
        {
            let mut state = state.lock().unwrap();
            state.set_var("var".to_string(), "val".to_string());
            state.set_var(IS_USERSPACE_VAR.to_string(), "no".to_string());
            state.set_var(REVISION_VAR.to_string(), "rev_test-b4".to_string());
            state.set_var(MAX_DOWNLOAD_SIZE_VAR.to_string(), "8192".to_string());
        }
        let (client, _server) = mpsc::channel(100);
        v.flash(
            &client,
            &mut TestResolver::new(),
            &mut proxy,
            ManifestParams {
                manifest: Some(PathBuf::from(tmp_file_name)),
                product: "zedboot".to_string(),
                ..Default::default()
            },
        )
        .await
    }
}
