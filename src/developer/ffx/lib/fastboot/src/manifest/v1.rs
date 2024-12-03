// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::boot::boot;
use crate::common::cmd::{ManifestParams, OemFile};
use crate::common::{
    flash_and_reboot, is_locked, Boot, Flash, Partition as PartitionTrait, Product as ProductTrait,
    Unlock, MISSING_PRODUCT, UNLOCK_ERR,
};
use crate::file_resolver::FileResolver;
use crate::util::Event;
use anyhow::Result;
use async_trait::async_trait;
use errors::ffx_bail;
use ffx_fastboot_interface::fastboot_interface::FastbootInterface;
use futures::try_join;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc::{self, Sender};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Product {
    pub name: String,
    pub bootloader_partitions: Vec<Partition>,
    pub partitions: Vec<Partition>,
    pub oem_files: Vec<OemFile>,
    #[serde(default)]
    pub requires_unlock: bool,
}

impl ProductTrait<Partition> for Product {
    fn bootloader_partitions(&self) -> &Vec<Partition> {
        &self.bootloader_partitions
    }

    fn partitions(&self) -> &Vec<Partition> {
        &self.partitions
    }

    fn oem_files(&self) -> &Vec<OemFile> {
        &self.oem_files
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Partition(
    String,
    String,
    #[serde(default)] Option<String>,
    #[serde(default)] Option<String>,
);

impl Partition {
    pub fn new(
        name: String,
        file: String,
        variable: Option<String>,
        variable_value: Option<String>,
    ) -> Self {
        Self(name, file, variable, variable_value)
    }
}

impl PartitionTrait for Partition {
    fn name(&self) -> &str {
        self.0.as_str()
    }

    fn file(&self) -> &str {
        self.1.as_str()
    }

    fn variable(&self) -> Option<&str> {
        self.2.as_ref().map(|s| s.as_str())
    }

    fn variable_value(&self) -> Option<&str> {
        self.3.as_ref().map(|s| s.as_str())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FlashManifest(pub Vec<Product>);

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
        let product = match self.0.iter().find(|product| product.name == cmd.product) {
            Some(res) => res,
            None => ffx_bail!("{} {}", MISSING_PRODUCT, cmd.product),
        };
        if product.requires_unlock && is_locked(fastboot_interface).await? {
            ffx_bail!("{}", UNLOCK_ERR);
        }
        flash_and_reboot(messenger, file_resolver, product, fastboot_interface, cmd).await
    }
}

#[async_trait(?Send)]
impl Unlock for FlashManifest {}

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
        let product = match self.0.iter().find(|product| product.name == cmd.product) {
            Some(res) => res,
            None => ffx_bail!("{} {}", MISSING_PRODUCT, cmd.product),
        };
        let partitions: Vec<&Partition> = product
            .partitions
            .iter()
            .filter(|p| p.name().ends_with(&format!("_{}", slot)))
            .collect();
        let zbi =
            partitions.iter().find(|p| p.name().contains("zircon")).map(|p| p.file().to_string());
        let vbmeta =
            partitions.iter().find(|p| p.name().contains("vbmeta")).map(|p| p.file().to_string());
        match zbi {
            Some(z) => {
                let (up_client, mut up_server) = mpsc::channel(100);
                try_join!(boot(up_client, file_resolver, z, vbmeta, fastboot_interface), async {
                    loop {
                        match up_server.recv().await {
                            Some(u) => messenger.send(Event::Upload(u)).await?,
                            None => {
                                return Ok(());
                            }
                        }
                    }
                })?;
                Ok(())
            }
            None => ffx_bail!("Could not find matching partitions for slot {}", slot),
        }
    }
}

////////////////////////////////////////////////////////////////////////////////
// tests

#[cfg(test)]
mod test {
    use super::*;
    use crate::common::vars::{IS_USERSPACE_VAR, LOCKED_VAR, MAX_DOWNLOAD_SIZE_VAR};
    use crate::test::{setup, TestResolver};
    use serde_json::{from_str, json};
    use std::path::PathBuf;
    use tempfile::NamedTempFile;
    use tokio::sync::mpsc;

    const MANIFEST: &'static str = r#"[
        {
            "name": "zedboot",
            "bootloader_partitions": [
                ["test1", "path1"],
                ["test2", "path2"]
            ],
            "partitions": [
                ["test1", "path1"],
                ["test2", "path2"],
                ["test3", "path3"],
                ["test4", "path4"],
                ["test5", "path5"]
            ],
            "oem_files": [
                ["test1", "path1"],
                ["test2", "path2"]
            ]
        },
        {
            "name": "fuchsia",
            "bootloader_partitions": [],
            "partitions": [
                ["test10", "path10"],
                ["test20", "path20"],
                ["test30", "path30"]
            ],
            "oem_files": []
        }
    ]"#;

    const LOCKED_MANIFEST: &'static str = r#"[
        {
            "name": "zedboot",
            "bootloader_partitions": [
                ["btest1", "bpath1", "var1", "value1"]
            ],
            "partitions": [],
            "oem_files": [],
            "requires_unlock": true
        }
    ]"#;

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_deserializing_should_work() -> Result<()> {
        let v: FlashManifest = from_str(MANIFEST)?;
        let zedboot: &Product = &v.0[0];
        assert_eq!("zedboot", zedboot.name);
        assert_eq!(2, zedboot.bootloader_partitions.len());
        let bootloader_expected = [["test1", "path1"], ["test2", "path2"]];
        for x in 0..bootloader_expected.len() {
            assert_eq!(zedboot.bootloader_partitions[x].name(), bootloader_expected[x][0]);
            assert_eq!(zedboot.bootloader_partitions[x].file(), bootloader_expected[x][1]);
        }
        assert_eq!(5, zedboot.partitions.len());
        let expected = [
            ["test1", "path1"],
            ["test2", "path2"],
            ["test3", "path3"],
            ["test4", "path4"],
            ["test5", "path5"],
        ];
        for x in 0..expected.len() {
            assert_eq!(zedboot.partitions[x].name(), expected[x][0]);
            assert_eq!(zedboot.partitions[x].file(), expected[x][1]);
        }
        assert_eq!(2, zedboot.oem_files.len());
        let oem_files_expected = [["test1", "path1"], ["test2", "path2"]];
        for x in 0..oem_files_expected.len() {
            assert_eq!(zedboot.oem_files[x].command(), oem_files_expected[x][0]);
            assert_eq!(zedboot.oem_files[x].file(), oem_files_expected[x][1]);
        }
        let product: &Product = &v.0[1];
        assert_eq!("fuchsia", product.name);
        assert_eq!(0, product.bootloader_partitions.len());
        assert_eq!(3, product.partitions.len());
        let expected2 = [["test10", "path10"], ["test20", "path20"], ["test30", "path30"]];
        for x in 0..expected2.len() {
            assert_eq!(product.partitions[x].name(), expected2[x][0]);
            assert_eq!(product.partitions[x].file(), expected2[x][1]);
        }
        assert_eq!(0, product.oem_files.len());
        Ok(())
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_should_fail_if_product_missing() -> Result<()> {
        let tmp_file = NamedTempFile::new().expect("tmp access failed");
        let tmp_file_name = tmp_file.path().to_string_lossy().to_string();
        let v: FlashManifest = from_str(MANIFEST)?;
        let (_, mut proxy) = setup();
        let (client, _server) = mpsc::channel(100);
        assert!(v
            .flash(
                &client,
                &mut TestResolver::new(),
                &mut proxy,
                ManifestParams {
                    manifest: Some(PathBuf::from(tmp_file_name)),
                    product: "Unknown".to_string(),
                    ..Default::default()
                }
            )
            .await
            .is_err());
        Ok(())
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_should_succeed_if_product_found() -> Result<()> {
        let tmp_file = NamedTempFile::new().expect("tmp access failed");
        let tmp_file_name = tmp_file.path().to_string_lossy().to_string();

        // Setup image files for flashing
        let tmp_img_files = [(); 3].map(|_| NamedTempFile::new().expect("tmp access failed"));
        let tmp_img_file_paths = tmp_img_files
            .iter()
            .map(|tmp| tmp.path().to_str().expect("non-unicode tmp path"))
            .collect::<Vec<&str>>();

        let manifest = json!([
            {
                "name": "zedboot",
                "bootloader_partitions": [
                    ["test1", "path1"],
                    ["test2", "path2"]
                ],
                "partitions": [
                    ["test1", "path1"],
                    ["test2", "path2"],
                    ["test3", "path3"],
                    ["test4", "path4"],
                    ["test5", "path5"]
                ],
                "oem_files": [
                    ["test1", "path1"],
                    ["test2", "path2"]
                ]
            },
            {
                "name": "fuchsia",
                "bootloader_partitions": [],
                "partitions": [
                    ["test10",tmp_img_file_paths[0]],
                    ["test20",tmp_img_file_paths[1]],
                    ["test30",tmp_img_file_paths[2]]
                ],
                "oem_files": []
            }
        ]);

        let v: FlashManifest = from_str(&manifest.to_string())?;

        let (state, mut proxy) = setup();
        {
            let mut state = state.lock().unwrap();
            state.set_var(IS_USERSPACE_VAR.to_string(), "yes".to_string());
            state.set_var(MAX_DOWNLOAD_SIZE_VAR.to_string(), "8192".to_string());
        }
        let (client, _server) = mpsc::channel(100);
        v.flash(
            &client,
            &mut TestResolver::new(),
            &mut proxy,
            ManifestParams {
                manifest: Some(PathBuf::from(tmp_file_name)),
                product: "fuchsia".to_string(),
                ..Default::default()
            },
        )
        .await?;
        Ok(())
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_oem_file_should_be_staged_from_command() -> Result<()> {
        let test_oem_cmd = "test-oem-cmd";
        let tmp_file = NamedTempFile::new().expect("tmp access failed");
        let tmp_file_name = tmp_file.path().to_string_lossy().to_string();
        let test_staged_file = format!("{},{}", test_oem_cmd, tmp_file_name).parse::<OemFile>()?;
        let manifest_file = NamedTempFile::new().expect("tmp access failed");
        let manifest_file_name = manifest_file.path().to_string_lossy().to_string();

        // Set up the temporary files to read
        let tmp_img_files = [(); 5].map(|_| NamedTempFile::new().expect("tmp access failed"));
        let tmp_img_file_paths = tmp_img_files
            .iter()
            .map(|tmp| tmp.path().to_str().expect("non-unicode tmp path"))
            .collect::<Vec<&str>>();

        let manifest = json!([
                {
                    "name": "fuchsia",
                    "bootloader_partitions": [],
                    "partitions": [
                        ["test1", tmp_img_file_paths[0]],
                        ["test2", tmp_img_file_paths[1]],
                        ["test3", tmp_img_file_paths[2]],
                        ["test4", tmp_img_file_paths[3]],
                        ["test5", tmp_img_file_paths[4]]
                    ],
                    "oem_files": []
                }
        ]);

        let v: FlashManifest = from_str(&manifest.to_string())?;
        let (state, mut proxy) = setup();
        {
            let mut state = state.lock().unwrap();
            state.set_var(IS_USERSPACE_VAR.to_string(), "yes".to_string());
            state.set_var(MAX_DOWNLOAD_SIZE_VAR.to_string(), "8192".to_string());
        }
        let (client, _server) = mpsc::channel(100);
        v.flash(
            &client,
            &mut TestResolver::new(),
            &mut proxy,
            ManifestParams {
                manifest: Some(PathBuf::from(manifest_file_name)),
                product: "fuchsia".to_string(),
                oem_stage: vec![test_staged_file],
                ..Default::default()
            },
        )
        .await?;
        let state = state.lock().unwrap();
        assert_eq!(1, state.staged_files.len());
        assert_eq!(1, state.oem_commands.len());
        assert_eq!(test_oem_cmd, state.oem_commands[0]);
        Ok(())
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_should_upload_conditional_partitions_that_match() -> Result<()> {
        let tmp_file = NamedTempFile::new().expect("tmp access failed");
        let tmp_file_name = tmp_file.path().to_string_lossy().to_string();

        // Setup images to flash
        let temp_image_file_2 = NamedTempFile::new().expect("tmp access failed");
        let tmp_image_file_2_name = temp_image_file_2.path().to_string_lossy().to_string();
        let manifest = json!([
                {
                    "name": "zedboot",
                    "bootloader_partitions": [
                        ["btest1", "bpath1", "var1", "value1"],
                        ["btest2", tmp_image_file_2_name, "var2", "value2"],
                        ["btest3", "bpath3", "var3", "value3"]
                    ],
                    "partitions": [],
                    "oem_files": []
                }
        ]);
        let v: FlashManifest = from_str(&manifest.to_string())?;

        let (state, mut proxy) = setup();
        {
            let mut state = state.lock().unwrap();
            state.set_var(IS_USERSPACE_VAR.to_string(), "no".to_string());
            state.set_var("var1".to_string(), "not_value1".to_string());
            state.set_var("var2".to_string(), "value2".to_string());
            state.set_var("var3".to_string(), "not_value3".to_string());
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
        .await?;
        Ok(())
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_should_succeed_and_not_reboot_bootloader() -> Result<()> {
        let tmp_file = NamedTempFile::new().expect("tmp access failed");
        let tmp_file_name = tmp_file.path().to_string_lossy().to_string();

        let tmp_img_files = [(); 3].map(|_| NamedTempFile::new().expect("tmp access failed"));
        let tmp_img_file_paths = tmp_img_files
            .iter()
            .map(|tmp| tmp.path().to_str().expect("non-unicode tmp path"))
            .collect::<Vec<&str>>();

        let manifest = json!([
                {
                    "name": "zedboot",
                    "bootloader_partitions": [
                        ["test1", "path1"],
                        ["test2", "path2"]
                    ],
                    "partitions": [
                        ["test1", "path1"],
                        ["test2", "path2"],
                        ["test3", "path3"],
                        ["test4", "path4"],
                        ["test5", "path5"]
                    ],
                    "oem_files": [
                        ["test1", "path1"],
                        ["test2", "path2"]
                    ]
                },
                {
                    "name": "fuchsia",
                    "bootloader_partitions": [],
                    "partitions": [
                        ["test10", tmp_img_file_paths[0]],
                        ["test20", tmp_img_file_paths[1]],
                        ["test30", tmp_img_file_paths[2]]
                    ],
                    "oem_files": []
                }
            ]
        );

        let v: FlashManifest = from_str(&manifest.to_string())?;
        let (state, mut proxy) = setup();
        {
            let mut state = state.lock().unwrap();
            state.set_var(IS_USERSPACE_VAR.to_string(), "no".to_string());
            state.set_var(MAX_DOWNLOAD_SIZE_VAR.to_string(), "8192".to_string());
        }

        let (client, _server) = mpsc::channel(100);
        v.flash(
            &client,
            &mut TestResolver::new(),
            &mut proxy,
            ManifestParams {
                manifest: Some(PathBuf::from(tmp_file_name)),
                product: "fuchsia".to_string(),
                no_bootloader_reboot: true,
                ..Default::default()
            },
        )
        .await?;
        let state = state.lock().unwrap();
        assert_eq!(0, state.bootloader_reboots);
        Ok(())
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_should_not_flash_if_target_is_locked_and_product_requires_unlock() -> Result<()> {
        let v: FlashManifest = from_str(LOCKED_MANIFEST)?;
        let tmp_file = NamedTempFile::new().expect("tmp access failed");
        let tmp_file_name = tmp_file.path().to_string_lossy().to_string();
        let (state, mut proxy) = setup();
        {
            let mut state = state.lock().unwrap();
            state.set_var(LOCKED_VAR.to_string(), "vx-locked".to_string());
            state.set_var(LOCKED_VAR.to_string(), "yes".to_string());
        }
        let (client, _server) = mpsc::channel(100);
        let res = v
            .flash(
                &client,
                &mut TestResolver::new(),
                &mut proxy,
                ManifestParams {
                    manifest: Some(PathBuf::from(tmp_file_name)),
                    product: "zedboot".to_string(),
                    ..Default::default()
                },
            )
            .await;
        assert_eq!(true, res.is_err());
        Ok(())
    }
}
