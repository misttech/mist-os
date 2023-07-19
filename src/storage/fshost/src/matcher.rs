// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{
        device::{
            constants::{
                BLOBFS_PARTITION_LABEL, BOOTPART_DRIVER_PATH, DATA_PARTITION_LABEL,
                FVM_DRIVER_PATH, GPT_DRIVER_PATH, LEGACY_DATA_PARTITION_LABEL, MBR_DRIVER_PATH,
                NAND_BROKER_DRIVER_PATH,
            },
            Device,
        },
        environment::Environment,
    },
    anyhow::{bail, Context, Error},
    async_trait::async_trait,
    fs_management::format::DiskFormat,
    std::{
        collections::BTreeMap,
        sync::atomic::{AtomicBool, Ordering},
    },
};

#[async_trait]
pub trait Matcher: Send {
    /// Tries to match this device against this matcher. Matching should be infallible.
    async fn match_device(&self, device: &mut dyn Device) -> bool;

    /// Process this device as the format this matcher is for. This is called when this matcher
    /// returns true during matching. This step is fallible - if a device matched a matcher, but
    /// then this step fails, we stop matching and bubble up the error.
    async fn process_device(
        &mut self,
        device: &mut dyn Device,
        env: &mut dyn Environment,
    ) -> Result<(), Error>;
}

pub struct Matchers {
    matchers: Vec<Box<dyn Matcher>>,

    matched: BTreeMap<String, usize>,
}

impl Matchers {
    /// Create a new set of matchers. This essentially describes the expected partition layout for
    /// a device.
    pub fn new(config: &fshost_config::Config, ramdisk_path: Option<String>) -> Self {
        let mut matchers = Vec::<Box<dyn Matcher>>::new();

        if config.bootpart {
            matchers.push(Box::new(BootpartMatcher::new()));
        }
        if config.nand {
            matchers.push(Box::new(NandMatcher::new()));
        }
        // If ramdisk_image is true but we don't actually have a ramdisk launching, we skip
        // making the fvm+blobfs+data or Fxblob matcher entirely.
        if !config.ramdisk_image || ramdisk_path.is_some() {
            if !config.netboot && config.fxfs_blob {
                matchers.push(Box::new(FxblobMatcher::new(if config.ramdisk_image {
                    ramdisk_path
                } else {
                    None
                })));
            } else {
                let ramdisk_required = if config.ramdisk_image { ramdisk_path } else { None };
                let fvm_matcher = Box::new(FvmMatcher::new(
                    ramdisk_required,
                    config.netboot,
                    config.blobfs,
                    config.data,
                ));
                if config.fvm || (!config.netboot && (config.blobfs || config.data)) {
                    matchers.push(fvm_matcher);
                }
            }
        }
        if config.fvm && config.ramdisk_image {
            // Add another matcher for the non-ramdisk version of fvm.
            matchers.push(Box::new(PartitionMapMatcher::new(
                DiskFormat::Fvm,
                false,
                FVM_DRIVER_PATH,
                None,
            )));
        }

        let gpt_matcher =
            Box::new(PartitionMapMatcher::new(DiskFormat::Gpt, false, GPT_DRIVER_PATH, None));
        if config.gpt {
            matchers.push(gpt_matcher);
        }

        if config.gpt_all {
            matchers.push(Box::new(PartitionMapMatcher::new(
                DiskFormat::Gpt,
                true,
                GPT_DRIVER_PATH,
                None,
            )));
        }

        if config.mbr {
            matchers.push(Box::new(PartitionMapMatcher::new(
                DiskFormat::Mbr,
                true,
                MBR_DRIVER_PATH,
                None,
            )))
        }

        Matchers { matchers, matched: BTreeMap::new() }
    }

    /// Using the set of matchers we created, figure out if this block device matches any of our
    /// expected partitions. If it does, return the information needed to launch the filesystem,
    /// such as the component url or the shared library to pass to the driver binding.
    pub async fn match_device(
        &mut self,
        device: &mut dyn Device,
        env: &mut dyn Environment,
    ) -> Result<bool, Error> {
        for (index, m) in self.matchers.iter_mut().enumerate() {
            if m.match_device(device).await {
                m.process_device(device, env).await?;
                self.matched.insert(device.topological_path().to_string(), index);
                return Ok(true);
            }
        }
        Ok(false)
    }
}

// Matches Bootpart devices.
struct BootpartMatcher();

impl BootpartMatcher {
    fn new() -> Self {
        BootpartMatcher()
    }
}

#[async_trait]
impl Matcher for BootpartMatcher {
    async fn match_device(&self, device: &mut dyn Device) -> bool {
        device
            .get_block_info()
            .await
            .map_or(false, |info| info.flags.contains(fidl_fuchsia_hardware_block::Flag::BOOTPART))
    }

    async fn process_device(
        &mut self,
        device: &mut dyn Device,
        env: &mut dyn Environment,
    ) -> Result<(), Error> {
        env.attach_driver(device, BOOTPART_DRIVER_PATH).await
    }
}

// Matches Nand devices.
struct NandMatcher();

impl NandMatcher {
    fn new() -> Self {
        NandMatcher()
    }
}

#[async_trait]
impl Matcher for NandMatcher {
    async fn match_device(&self, device: &mut dyn Device) -> bool {
        device.is_nand()
    }

    async fn process_device(
        &mut self,
        device: &mut dyn Device,
        env: &mut dyn Environment,
    ) -> Result<(), Error> {
        env.attach_driver(device, NAND_BROKER_DRIVER_PATH).await
    }
}

// Matches against a data partition that exists independent of FVM using the DiskFormat.
struct FxblobMatcher {
    // If this partition is required to exist on a ramdisk, then this contains the prefix it should
    // have.
    ramdisk_required: Option<String>,
    // Because this matcher binds to the system Fxfs component, we can only match on it once.
    // TODO(fxbug.dev/128655): Can we be more precise here, e.g. give the matcher an expected device
    // path based on system configuration?
    already_matched: AtomicBool,
}

impl FxblobMatcher {
    fn new(ramdisk_required: Option<String>) -> Self {
        Self { ramdisk_required, already_matched: AtomicBool::new(false) }
    }
}

#[async_trait]
impl Matcher for FxblobMatcher {
    async fn match_device(&self, device: &mut dyn Device) -> bool {
        if self.already_matched.load(Ordering::Relaxed) {
            return false;
        }
        if let Some(ramdisk_prefix) = &self.ramdisk_required {
            if !device.topological_path().starts_with(ramdisk_prefix) {
                return false;
            }
        }
        device.content_format().await.ok() == Some(DiskFormat::Fxfs)
    }

    async fn process_device(
        &mut self,
        device: &mut dyn Device,
        env: &mut dyn Environment,
    ) -> Result<(), Error> {
        self.already_matched.store(true, Ordering::Relaxed);
        env.mount_fxblob(device).await?;
        env.mount_blob_volume().await?;
        env.mount_data_volume().await?;
        Ok(())
    }
}

// Matches against the fvm partition and explicitly mounts the data and blob partitions.
// Fails if the blob partition doesn't exist. Creates the data partition if it doesn't
// already exist.
struct FvmMatcher {
    // If this partition is required to exist on a ramdisk, then this contains the prefix it should
    // have.
    ramdisk_required: Option<String>,

    netboot: bool,

    // Set if we want to mount the blob partition.
    blobfs: bool,

    // Set if we want to mount the data partition.
    data: bool,
}

impl FvmMatcher {
    fn new(ramdisk_required: Option<String>, netboot: bool, blobfs: bool, data: bool) -> Self {
        Self { ramdisk_required, netboot, blobfs, data }
    }
}

#[async_trait]
impl Matcher for FvmMatcher {
    async fn match_device(&self, device: &mut dyn Device) -> bool {
        if let Some(ramdisk_prefix) = &self.ramdisk_required {
            if !device.topological_path().starts_with(ramdisk_prefix) {
                return false;
            }
        }
        device.content_format().await.ok() == Some(DiskFormat::Fvm)
    }

    async fn process_device(
        &mut self,
        device: &mut dyn Device,
        env: &mut dyn Environment,
    ) -> Result<(), Error> {
        // volume names have the format {label}-p-{index}, e.g. blobfs-p-1
        let volume_names = env.bind_and_enumerate_fvm(device).await?;
        if !self.netboot {
            if self.blobfs {
                if let Some(blob_name) =
                    volume_names.iter().find(|name| name.starts_with(BLOBFS_PARTITION_LABEL))
                {
                    env.mount_blobfs_on(blob_name).await?;
                } else {
                    tracing::error!(?volume_names, "Couldn't find blobfs partition!");
                    bail!("Unable to find blobfs within FVM.");
                }
            }
            if self.data {
                if let Some(data_name) = volume_names.iter().find(|name| {
                    name.starts_with(DATA_PARTITION_LABEL)
                        || name.starts_with(LEGACY_DATA_PARTITION_LABEL)
                }) {
                    env.mount_data_on(data_name).await?;
                } else {
                    let fvm_driver_path = format!("{}/fvm", device.topological_path());
                    tracing::warn!(%fvm_driver_path,
                        "No existing data partition. Calling format_data().",
                    );
                    let fs =
                        env.format_data(&fvm_driver_path).await.context("failed to format data")?;
                    env.bind_data(fs)?;
                }
            }
        }
        Ok(())
    }
}

// Matches partition maps. Matching is done using content sniffing.
struct PartitionMapMatcher {
    // The content format expected.
    content_format: DiskFormat,

    // If true, match against multiple devices. Otherwise, only the first is matched.
    allow_multiple: bool,

    // When matched, this driver is attached to the device.
    driver_path: &'static str,

    ramdisk_path: Option<String>,

    // The topological paths of all devices matched so far.
    device_paths: Vec<String>,
}

impl PartitionMapMatcher {
    fn new(
        content_format: DiskFormat,
        allow_multiple: bool,
        driver_path: &'static str,
        ramdisk_path: Option<String>,
    ) -> Self {
        Self { content_format, allow_multiple, driver_path, ramdisk_path, device_paths: Vec::new() }
    }
}

#[async_trait]
impl Matcher for PartitionMapMatcher {
    async fn match_device(&self, device: &mut dyn Device) -> bool {
        if !self.allow_multiple && !self.device_paths.is_empty() {
            return false;
        }
        if let Some(ramdisk_prefix) = &self.ramdisk_path {
            if device.topological_path().starts_with(ramdisk_prefix) {
                tracing::info!(path = %device.path(), %ramdisk_prefix, "Found the fvm ramdisk");
            } else {
                return false;
            }
        }
        device.content_format().await.ok() == Some(self.content_format)
    }

    async fn process_device(
        &mut self,
        device: &mut dyn Device,
        env: &mut dyn Environment,
    ) -> Result<(), Error> {
        env.attach_driver(device, self.driver_path).await?;
        self.device_paths.push(device.topological_path().to_string());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use {
        super::{Device, DiskFormat, Environment, Matchers},
        crate::{
            config::default_config,
            device::constants::{
                BLOBFS_PARTITION_LABEL, BOOTPART_DRIVER_PATH, DATA_PARTITION_LABEL, DATA_TYPE_GUID,
                FVM_DRIVER_PATH, GPT_DRIVER_PATH, LEGACY_DATA_PARTITION_LABEL,
                NAND_BROKER_DRIVER_PATH,
            },
            environment::Filesystem,
        },
        anyhow::{anyhow, Error},
        async_trait::async_trait,
        fidl_fuchsia_device::ControllerProxy,
        fidl_fuchsia_hardware_block::{BlockInfo, BlockProxy, Flag},
        fidl_fuchsia_hardware_block_volume::VolumeProxy,
        std::sync::Mutex,
    };

    #[derive(Clone)]
    struct MockDevice {
        block_flags: Flag,
        is_nand: bool,
        content_format: DiskFormat,
        topological_path: String,
        partition_label: Option<String>,
        partition_type: Option<[u8; 16]>,
    }

    impl MockDevice {
        fn new() -> Self {
            MockDevice {
                block_flags: Flag::empty(),
                is_nand: false,
                content_format: DiskFormat::Unknown,
                topological_path: "mock_device".to_string(),
                partition_label: None,
                partition_type: None,
            }
        }
        fn set_block_flags(mut self, flags: Flag) -> Self {
            self.block_flags = flags;
            self
        }
        fn set_nand(mut self, v: bool) -> Self {
            self.is_nand = v;
            self
        }
        fn set_content_format(mut self, format: DiskFormat) -> Self {
            self.content_format = format;
            self
        }
        fn set_topological_path(mut self, path: impl ToString) -> Self {
            self.topological_path = path.to_string().into();
            self
        }
        fn set_partition_label(mut self, label: impl ToString) -> Self {
            self.partition_label = Some(label.to_string());
            self
        }
        fn set_partition_type(mut self, partition_type: &[u8; 16]) -> Self {
            self.partition_type = Some(partition_type.clone());
            self
        }
    }

    #[async_trait]
    impl Device for MockDevice {
        async fn get_block_info(&self) -> Result<fidl_fuchsia_hardware_block::BlockInfo, Error> {
            if self.is_nand {
                Err(anyhow!("not supported by nand device"))
            } else {
                Ok(BlockInfo {
                    block_count: 0,
                    block_size: 0,
                    max_transfer_size: 0,
                    flags: self.block_flags,
                })
            }
        }
        fn is_nand(&self) -> bool {
            self.is_nand
        }
        async fn content_format(&mut self) -> Result<DiskFormat, Error> {
            Ok(self.content_format)
        }
        fn topological_path(&self) -> &str {
            &self.topological_path
        }
        fn path(&self) -> &str {
            &self.topological_path
        }
        async fn partition_label(&mut self) -> Result<&str, Error> {
            Ok(self
                .partition_label
                .as_ref()
                .unwrap_or_else(|| panic!("Unexpected call to partition_label")))
        }
        async fn partition_type(&mut self) -> Result<&[u8; 16], Error> {
            Ok(self
                .partition_type
                .as_ref()
                .unwrap_or_else(|| panic!("Unexpected call to partition_type")))
        }
        async fn partition_instance(&mut self) -> Result<&[u8; 16], Error> {
            unreachable!()
        }
        fn controller(&self) -> &ControllerProxy {
            unreachable!()
        }
        fn reopen_controller(&self) -> Result<ControllerProxy, Error> {
            unreachable!()
        }
        fn block_proxy(&self) -> Result<BlockProxy, Error> {
            unreachable!()
        }
        fn volume_proxy(&self) -> Result<VolumeProxy, Error> {
            unreachable!()
        }
        async fn get_child(&self, _suffix: &str) -> Result<Box<dyn Device>, Error> {
            unreachable!()
        }
    }

    struct MockEnv {
        expected_driver_path: Mutex<Option<String>>,
        expect_bind_and_enumerate_fvm: Mutex<bool>,
        expect_mount_blobfs_on: Mutex<bool>,
        expect_mount_fxblob: Mutex<bool>,
        expect_mount_blob_volume: Mutex<bool>,
        expect_mount_data_volume: Mutex<bool>,
        expect_mount_data_on: Mutex<bool>,
        expect_format_data: Mutex<bool>,
        expect_bind_data: Mutex<bool>,
        legacy_data_format: bool,
        create_data_partition: bool,
    }

    impl MockEnv {
        fn new() -> Self {
            MockEnv {
                expected_driver_path: Mutex::new(None),
                expect_bind_and_enumerate_fvm: Mutex::new(false),
                expect_mount_blobfs_on: Mutex::new(false),
                expect_mount_fxblob: Mutex::new(false),
                expect_mount_blob_volume: Mutex::new(false),
                expect_mount_data_volume: Mutex::new(false),
                expect_mount_data_on: Mutex::new(false),
                expect_format_data: Mutex::new(false),
                expect_bind_data: Mutex::new(false),
                legacy_data_format: false,
                create_data_partition: true,
            }
        }
        fn expect_attach_driver(mut self, path: impl ToString) -> Self {
            *self.expected_driver_path.get_mut().unwrap() = Some(path.to_string());
            self
        }
        fn expect_bind_and_enumerate_fvm(mut self) -> Self {
            *self.expect_bind_and_enumerate_fvm.get_mut().unwrap() = true;
            self
        }
        fn expect_mount_blobfs_on(mut self) -> Self {
            *self.expect_mount_blobfs_on.get_mut().unwrap() = true;
            self
        }
        fn expect_format_data(mut self) -> Self {
            *self.expect_format_data.get_mut().unwrap() = true;
            self
        }
        fn expect_bind_data(mut self) -> Self {
            *self.expect_bind_data.get_mut().unwrap() = true;
            self
        }
        fn expect_mount_fxblob(mut self) -> Self {
            *self.expect_mount_fxblob.get_mut().unwrap() = true;
            self
        }
        fn expect_mount_blob_volume(mut self) -> Self {
            *self.expect_mount_blob_volume.get_mut().unwrap() = true;
            self
        }
        fn expect_mount_data_volume(mut self) -> Self {
            *self.expect_mount_data_volume.get_mut().unwrap() = true;
            self
        }
        fn expect_mount_data_on(mut self) -> Self {
            *self.expect_mount_data_on.get_mut().unwrap() = true;
            self
        }
        fn legacy_data_format(mut self) -> Self {
            self.legacy_data_format = true;
            self
        }
        fn without_data_partition(mut self) -> Self {
            self.create_data_partition = false;
            self
        }
    }

    #[async_trait]
    impl Environment for MockEnv {
        async fn attach_driver(
            &self,
            _device: &mut dyn Device,
            driver_path: &str,
        ) -> Result<(), Error> {
            assert_eq!(
                driver_path,
                self.expected_driver_path
                    .lock()
                    .unwrap()
                    .take()
                    .expect("Unexpected call to attach_driver")
            );
            Ok(())
        }

        async fn bind_and_enumerate_fvm(
            &mut self,
            _device: &mut dyn Device,
        ) -> Result<Vec<String>, Error> {
            assert_eq!(
                std::mem::take(&mut *self.expect_bind_and_enumerate_fvm.lock().unwrap()),
                true,
                "Unexpected call to bind_and_enumerate_fvm"
            );
            let mut volume_names = vec![BLOBFS_PARTITION_LABEL.to_string()];
            if self.create_data_partition {
                if self.legacy_data_format {
                    volume_names.push(LEGACY_DATA_PARTITION_LABEL.to_string())
                } else {
                    volume_names.push(DATA_PARTITION_LABEL.to_string())
                };
            }
            Ok(volume_names)
        }

        async fn mount_blobfs_on(&mut self, _blobfs_partition_name: &str) -> Result<(), Error> {
            assert_eq!(
                std::mem::take(&mut *self.expect_mount_blobfs_on.lock().unwrap()),
                true,
                "Unexpected call to mount_blobfs_on"
            );
            Ok(())
        }

        async fn mount_data_on(&mut self, _data_partition_name: &str) -> Result<(), Error> {
            assert_eq!(
                std::mem::take(&mut *self.expect_mount_data_on.lock().unwrap()),
                true,
                "Unexpected call to mount_data_on"
            );
            Ok(())
        }

        async fn format_data(&mut self, _fvm_topo_path: &str) -> Result<Filesystem, Error> {
            assert_eq!(
                std::mem::take(&mut *self.expect_format_data.lock().unwrap()),
                true,
                "Unexpected call to format_data"
            );
            Ok(Filesystem::Queue(vec![]))
        }

        fn bind_data(&mut self, mut _fs: Filesystem) -> Result<(), Error> {
            assert_eq!(
                std::mem::take(&mut *self.expect_bind_data.lock().unwrap()),
                true,
                "Unexpected call to bind_data"
            );
            Ok(())
        }

        async fn mount_fxblob(&mut self, _device: &mut dyn Device) -> Result<(), Error> {
            assert_eq!(
                std::mem::take(&mut *self.expect_mount_fxblob.lock().unwrap()),
                true,
                "Unexpected call to mount_fxblob"
            );
            Ok(())
        }

        async fn mount_blob_volume(&mut self) -> Result<(), Error> {
            assert_eq!(
                std::mem::take(&mut *self.expect_mount_blob_volume.lock().unwrap()),
                true,
                "Unexpected call to mount_blob_volume"
            );
            Ok(())
        }

        async fn mount_data_volume(&mut self) -> Result<(), Error> {
            assert_eq!(
                std::mem::take(&mut *self.expect_mount_data_volume.lock().unwrap()),
                true,
                "Unexpected call to mount_data_volume"
            );
            Ok(())
        }

        async fn shred_data(&mut self) -> Result<(), Error> {
            unreachable!();
        }

        async fn shutdown(&mut self) -> Result<(), Error> {
            unreachable!();
        }
    }

    impl Drop for MockEnv {
        fn drop(&mut self) {
            assert!(self.expected_driver_path.get_mut().unwrap().is_none());
            assert!(!*self.expect_mount_blobfs_on.lock().unwrap());
            assert!(!*self.expect_mount_data_on.lock().unwrap());
            assert!(!*self.expect_bind_and_enumerate_fvm.lock().unwrap());
            assert!(!*self.expect_bind_data.lock().unwrap());
        }
    }

    #[fuchsia::test]
    async fn test_bootpart_matcher() {
        let mut mock_device = MockDevice::new().set_block_flags(Flag::BOOTPART);

        // Check no match when disabled in config.
        assert!(!Matchers::new(
            &fshost_config::Config { bootpart: false, ..default_config() },
            None
        )
        .match_device(&mut mock_device, &mut MockEnv::new())
        .await
        .expect("match_device failed"));

        assert!(Matchers::new(&default_config(), None)
            .match_device(
                &mut mock_device,
                &mut MockEnv::new().expect_attach_driver(BOOTPART_DRIVER_PATH)
            )
            .await
            .expect("match_device failed"));
    }

    #[fuchsia::test]
    async fn test_nand_matcher() {
        let mut device = MockDevice::new().set_nand(true);
        let mut env = MockEnv::new().expect_attach_driver(NAND_BROKER_DRIVER_PATH);

        // Default shouldn't match.
        assert!(!Matchers::new(&default_config(), None)
            .match_device(&mut device, &mut env)
            .await
            .expect("match_device failed"));

        assert!(Matchers::new(&fshost_config::Config { nand: true, ..default_config() }, None)
            .match_device(&mut device, &mut env)
            .await
            .expect("match_device failed"));
    }

    #[fuchsia::test]
    async fn test_partition_map_matcher() {
        let mut env = MockEnv::new().expect_attach_driver(GPT_DRIVER_PATH);

        // Check no match when disabled in config.
        let mut device = MockDevice::new().set_content_format(DiskFormat::Gpt);
        assert!(!Matchers::new(
            &fshost_config::Config { blobfs: false, data: false, gpt: false, ..default_config() },
            None
        )
        .match_device(&mut device, &mut env)
        .await
        .expect("match_device failed"));

        let mut matchers = Matchers::new(&default_config(), None);
        assert!(matchers.match_device(&mut device, &mut env).await.expect("match_device failed"));

        // More GPT devices should not get matched.
        assert!(!matchers.match_device(&mut device, &mut env).await.expect("match_device failed"));

        // The gpt_all config should allow multiple GPT devices to be matched.
        let mut matchers =
            Matchers::new(&fshost_config::Config { gpt_all: true, ..default_config() }, None);
        let mut env = MockEnv::new().expect_attach_driver(GPT_DRIVER_PATH);
        assert!(matchers.match_device(&mut device, &mut env).await.expect("match_device failed"));
        let mut env = MockEnv::new().expect_attach_driver(GPT_DRIVER_PATH);
        assert!(matchers.match_device(&mut device, &mut env).await.expect("match_device failed"));
    }

    #[fuchsia::test]
    async fn test_partition_map_matcher_ramdisk_prefix() {
        // If ramdisk_image is true and one of the devices matches the ramdisk prefix, we will match
        // two fvm devices, and the third one will fail.
        let mut matchers = Matchers::new(
            &fshost_config::Config {
                ramdisk_image: true,
                data_filesystem_format: "minfs".to_string(),
                ..default_config()
            },
            Some("second_prefix".to_string()),
        );
        let mut fvm_device = MockDevice::new()
            .set_content_format(DiskFormat::Fvm)
            .set_topological_path("first_prefix");
        let mut env = MockEnv::new().expect_attach_driver(FVM_DRIVER_PATH);
        assert!(matchers
            .match_device(&mut fvm_device, &mut env)
            .await
            .expect("match_device failed"));

        let mut fvm_device = fvm_device.set_topological_path("second_prefix");
        let mut env = MockEnv::new()
            .expect_bind_and_enumerate_fvm()
            .expect_mount_blobfs_on()
            .expect_mount_data_on();
        assert!(matchers
            .match_device(&mut fvm_device, &mut env)
            .await
            .expect("match_device failed"));

        let mut fvm_device = fvm_device.set_topological_path("third_prefix");
        assert!(!matchers
            .match_device(&mut fvm_device, &mut MockEnv::new())
            .await
            .expect("match_device failed"));
    }

    #[fuchsia::test]
    async fn partition_map_matcher_wrong_prefix_match() {
        // If ramdisk_image is true but no devices match the prefix, only the first device will
        // match.
        let mut matchers = Matchers::new(
            &fshost_config::Config {
                ramdisk_image: true,
                data_filesystem_format: "fxfs".to_string(),
                ..default_config()
            },
            Some("wrong_prefix".to_string()),
        );

        let mut fvm_device = MockDevice::new()
            .set_content_format(DiskFormat::Fvm)
            .set_topological_path("first_prefix");
        let mut env = MockEnv::new().expect_attach_driver(FVM_DRIVER_PATH);
        assert!(matchers
            .match_device(&mut fvm_device, &mut env)
            .await
            .expect("match_device failed"));
        let mut fvm_device = fvm_device.set_topological_path("second_prefix");
        assert!(!matchers
            .match_device(&mut fvm_device, &mut MockEnv::new())
            .await
            .expect("match_device failed"));
    }

    #[fuchsia::test]
    async fn partition_map_matcher_no_prefix_match() {
        // If ramdisk_image is true but no ramdisk path is provided, only the first device will
        // match.
        let mut matchers = Matchers::new(
            &fshost_config::Config {
                ramdisk_image: true,
                data_filesystem_format: "fxfs".to_string(),
                ..default_config()
            },
            None,
        );

        let mut fvm_device = MockDevice::new()
            .set_content_format(DiskFormat::Fvm)
            .set_topological_path("first_prefix");
        let mut env = MockEnv::new().expect_attach_driver(FVM_DRIVER_PATH);
        assert!(matchers
            .match_device(&mut fvm_device, &mut env)
            .await
            .expect("match_device failed"));
        let mut fvm_device = fvm_device.set_topological_path("second_prefix");
        assert!(!matchers
            .match_device(&mut fvm_device, &mut MockEnv::new())
            .await
            .expect("match_device failed"));
    }

    #[fuchsia::test]
    async fn test_blobfs_matcher() {
        let mut fvm_device = MockDevice::new().set_content_format(DiskFormat::Fvm);
        let mut env = MockEnv::new().expect_bind_and_enumerate_fvm().expect_mount_blobfs_on();

        let mut matchers =
            Matchers::new(&fshost_config::Config { data: false, ..default_config() }, None);

        assert!(matchers
            .match_device(&mut fvm_device, &mut env)
            .await
            .expect("match_device failed"));
    }

    #[fuchsia::test]
    async fn test_data_matcher() {
        let mut matchers =
            Matchers::new(&fshost_config::Config { blobfs: false, ..default_config() }, None);

        assert!(matchers
            .match_device(
                &mut MockDevice::new().set_content_format(DiskFormat::Fvm),
                &mut MockEnv::new().expect_bind_and_enumerate_fvm().expect_mount_data_on()
            )
            .await
            .expect("match_device failed"));
    }

    #[fuchsia::test]
    async fn test_legacy_data_matcher() {
        let mut matchers = Matchers::new(&default_config(), None);

        assert!(matchers
            .match_device(
                &mut MockDevice::new().set_content_format(DiskFormat::Fvm),
                &mut MockEnv::new()
                    .legacy_data_format()
                    .expect_bind_and_enumerate_fvm()
                    .expect_mount_blobfs_on()
                    .expect_mount_data_on()
            )
            .await
            .expect("match_device failed"));
    }

    #[fuchsia::test]
    async fn test_matcher_without_data_partition() {
        let mut matchers = Matchers::new(&default_config(), None);

        assert!(matchers
            .match_device(
                &mut MockDevice::new().set_content_format(DiskFormat::Fvm),
                &mut MockEnv::new()
                    .without_data_partition()
                    .expect_bind_and_enumerate_fvm()
                    .expect_mount_blobfs_on()
                    .expect_format_data()
                    .expect_bind_data()
            )
            .await
            .expect("match_device failed"));
    }

    #[fuchsia::test]
    async fn test_netboot_flag_true() {
        let mut matchers =
            Matchers::new(&fshost_config::Config { netboot: true, ..default_config() }, None);

        assert!(matchers
            .match_device(
                &mut MockDevice::new().set_content_format(DiskFormat::Fvm),
                &mut MockEnv::new().expect_bind_and_enumerate_fvm()
            )
            .await
            .expect("match_device failed"));
    }

    #[fuchsia::test]
    async fn test_fxblob_matcher() {
        let mut matchers = Matchers::new(
            &fshost_config::Config {
                fxfs_blob: true,
                data_filesystem_format: "fxfs".to_string(),
                ..default_config()
            },
            None,
        );

        assert!(matchers
            .match_device(
                &mut MockDevice::new()
                    .set_content_format(DiskFormat::Fxfs)
                    .set_partition_label(DATA_PARTITION_LABEL)
                    .set_partition_type(&DATA_TYPE_GUID),
                &mut MockEnv::new()
                    .expect_mount_fxblob()
                    .expect_mount_blob_volume()
                    .expect_mount_data_volume()
            )
            .await
            .expect("match_device failed"));

        // We should only be able to match Fxblob once.
        assert!(!matchers
            .match_device(
                &mut MockDevice::new()
                    .set_content_format(DiskFormat::Fxfs)
                    .set_partition_label(DATA_PARTITION_LABEL)
                    .set_partition_type(&DATA_TYPE_GUID),
                &mut MockEnv::new()
                    .expect_mount_fxblob()
                    .expect_mount_blob_volume()
                    .expect_mount_data_volume()
            )
            .await
            .expect("match_device failed"));
    }

    #[fuchsia::test]
    fn test_device_fvm_path() {
        let device =
            MockDevice::new().set_topological_path("/some/fvm/path/with/another/fvm/inside");
        assert_eq!(device.fvm_path(), Some("/some/fvm/path/with/another/fvm".to_string()));
    }
}
