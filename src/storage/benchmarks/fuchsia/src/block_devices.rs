// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    async_trait::async_trait,
    fidl::endpoints::{create_proxy, Proxy},
    fidl_fuchsia_device::{ControllerMarker, ControllerProxy},
    fidl_fuchsia_hardware_block::BlockMarker,
    fidl_fuchsia_hardware_block_volume::ALLOCATE_PARTITION_FLAG_INACTIVE,
    fidl_fuchsia_hardware_block_volume::{
        VolumeManagerMarker, VolumeManagerProxy, VolumeSynchronousProxy,
    },
    fidl_fuchsia_io as fio,
    fs_management::{format::DiskFormat, BLOBFS_TYPE_GUID},
    fuchsia_component::client::{
        connect_to_named_protocol_at_dir_root, connect_to_protocol_at_path,
    },
    fuchsia_zircon::{self as zx},
    ramdevice_client::RamdiskClient,
    std::path::PathBuf,
    storage_benchmarks::{block_device::BlockDevice, BlockDeviceConfig, BlockDeviceFactory},
    storage_isolated_driver_manager::{
        create_random_guid, find_block_device, fvm, into_guid, wait_for_block_device, zxcrypt,
        BlockDeviceMatcher, Guid,
    },
};

const RAMDISK_FVM_SLICE_SIZE: usize = 1024 * 1024;
const BLOBFS_VOLUME_NAME: &str = "blobfs";

const BENCHMARK_FVM_SIZE_BYTES: u64 = 128 * 1024 * 1024;

// On systems which don't have FVM (i.e. Fxblob), we create an FVM partition the test can use, with
// this GUID.  See connect_to_test_fvm for details.

const BENCHMARK_FVM_TYPE_GUID: &Guid = &[
    0x67, 0x45, 0x23, 0x01, 0xab, 0x89, 0xef, 0xcd, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88,
];
const BENCHMARK_FVM_VOLUME_NAME: &str = "benchmark-fvm";

const BENCHMARK_TYPE_GUID: &Guid = &[
    0x67, 0x45, 0x23, 0x01, 0xab, 0x89, 0xef, 0xcd, 0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef,
];
const BENCHMARK_VOLUME_NAME: &str = "benchmark";

/// Creates block devices on ramdisks.
pub struct RamdiskFactory {
    block_size: u64,
    block_count: u64,
}

impl RamdiskFactory {
    #[allow(dead_code)]
    pub async fn new(block_size: u64, block_count: u64) -> Self {
        Self { block_size, block_count }
    }
}

#[async_trait]
impl BlockDeviceFactory for RamdiskFactory {
    async fn create_block_device(&self, config: &BlockDeviceConfig) -> Box<dyn BlockDevice> {
        Box::new(Ramdisk::new(self.block_size, self.block_count, config).await)
    }
}

/// A ramdisk backed block device.
pub struct Ramdisk {
    _ramdisk: RamdiskClient,
    volume_dir: fio::DirectoryProxy,
    volume_controller: ControllerProxy,
}

impl Ramdisk {
    async fn new(block_size: u64, block_count: u64, config: &BlockDeviceConfig) -> Self {
        let ramdisk = RamdiskClient::create(block_size, block_count)
            .await
            .expect("Failed to create RamdiskClient");

        let volume_manager = fvm::set_up_fvm(
            ramdisk.as_controller().expect("invalid controller"),
            ramdisk.as_dir().expect("invalid directory proxy"),
            RAMDISK_FVM_SLICE_SIZE,
        )
        .await
        .expect("Failed to set up FVM");
        let volume_dir = set_up_fvm_volume(&volume_manager, config.fvm_volume_size).await;
        let volume_dir = if config.use_zxcrypt {
            zxcrypt::set_up_insecure_zxcrypt(&volume_dir).await.expect("Failed to set up zxcrypt")
        } else {
            volume_dir
        };

        let volume_controller =
            connect_to_named_protocol_at_dir_root::<ControllerMarker>(&volume_dir, ".")
                .expect("failed to connect to the device controller");

        Self { _ramdisk: ramdisk, volume_dir, volume_controller: volume_controller }
    }
}

impl BlockDevice for Ramdisk {
    fn dir(&self) -> &fio::DirectoryProxy {
        &self.volume_dir
    }

    fn controller(&self) -> &ControllerProxy {
        &self.volume_controller
    }
}

/// Creates block devices on top of the system's FVM instance.
pub struct FvmVolumeFactory {
    fvm: VolumeManagerProxy,
}

async fn connect_to_system_fvm() -> Option<VolumeManagerProxy> {
    let blobfs_dev_path = find_block_device(&[
        BlockDeviceMatcher::Name(BLOBFS_VOLUME_NAME),
        BlockDeviceMatcher::TypeGuid(&BLOBFS_TYPE_GUID),
    ])
    .await
    .ok()?;

    let blobfs_controller =
        connect_to_protocol_at_path::<ControllerMarker>(blobfs_dev_path.to_str().unwrap())
            .unwrap_or_else(|_| panic!("Failed to connect to Controller at {:?}", blobfs_dev_path));
    let path = blobfs_controller
        .get_topological_path()
        .await
        .expect("FIDL error")
        .expect("get_topological_path failed");

    let mut path = PathBuf::from(path);
    if !path.pop() || !path.pop() {
        panic!("Unexpected topological path for Blobfs {}", path.display());
    }

    match path.file_name() {
        Some(p) => assert!(p == "fvm", "Unexpected FVM path: {}", path.display()),
        None => panic!("Unexpected FVM path: {}", path.display()),
    }
    Some(
        connect_to_protocol_at_path::<VolumeManagerMarker>(path.to_str().unwrap())
            .unwrap_or_else(|_| panic!("Failed to connect to VolumeManager at {:?}", path)),
    )
}

// Connects to a test-only instance of the FVM, or adds it to the GPT if absent.
// This is used on systems which don't have a real FVM, i.e. Fxblob.
// The benchmarks have to have an FVM somewhere, since minfs doesn't work properly without FVM.
async fn connect_to_test_fvm() -> Option<VolumeManagerProxy> {
    let mut fvm_path = if let Ok(path) = find_block_device(&[
        BlockDeviceMatcher::Name(BENCHMARK_FVM_VOLUME_NAME),
        BlockDeviceMatcher::TypeGuid(&BENCHMARK_FVM_TYPE_GUID),
    ])
    .await
    {
        // If the test FVM already exists, just use it.
        path
    } else {
        // Otherwise, create it in the GPT.
        let mut gpt_block_path =
            find_block_device(&[BlockDeviceMatcher::ContentsMatch(DiskFormat::Gpt)]).await.ok()?;
        gpt_block_path.push("device_controller");
        let gpt_block_controller =
            connect_to_protocol_at_path::<ControllerMarker>(gpt_block_path.to_str().unwrap())
                .expect("Failed to connect to GPT controller");

        let mut gpt_path = gpt_block_controller
            .get_topological_path()
            .await
            .expect("FIDL error")
            .expect("get_topological_path failed");
        gpt_path.push_str("/gpt/device_controller");

        let gpt_controller = connect_to_protocol_at_path::<ControllerMarker>(&gpt_path)
            .expect("Failed to connect to GPT controller");

        let (volume_manager, server) = create_proxy::<VolumeManagerMarker>().unwrap();
        gpt_controller
            .connect_to_device_fidl(server.into_channel())
            .expect("Failed to connect to device FIDL");
        let slice_size = {
            let (status, info) = volume_manager.get_info().await.expect("FIDL error");
            zx::ok(status).expect("Failed to get VolumeManager info");
            info.unwrap().slice_size
        };
        let slice_count = BENCHMARK_FVM_SIZE_BYTES / slice_size;
        let instance_guid = into_guid(create_random_guid());
        let status = volume_manager
            .allocate_partition(
                slice_count,
                &into_guid(BENCHMARK_FVM_TYPE_GUID.clone()),
                &instance_guid,
                BENCHMARK_FVM_VOLUME_NAME,
                0,
            )
            .await
            .expect("FIDL error");
        zx::ok(status).expect("Failed to allocate benchmark FVM");

        wait_for_block_device(&[
            BlockDeviceMatcher::Name(BENCHMARK_FVM_VOLUME_NAME),
            BlockDeviceMatcher::TypeGuid(&BENCHMARK_FVM_TYPE_GUID),
        ])
        .await
        .expect("Failed to wait for newly created benchmark FVM to appear")
    };
    fvm_path.push("device_controller");
    let fvm_controller =
        connect_to_protocol_at_path::<ControllerMarker>(fvm_path.to_str().unwrap())
            .expect("failed to connect to controller");

    // Unbind so we can reformat.
    fvm_controller.unbind_children().await.expect("FIDL error").expect("failed to unbind children");

    // We need to connect to the volume's DirectoryProxy via its topological path in order to
    // allow the caller to access its zxcrypt child. Hence, we use the controller to get access
    // to the topological path and then call open().
    // Connect to the controller and get the device's topological path.
    let topo_path = fvm_controller
        .get_topological_path()
        .await
        .expect("FIDL error")
        .expect("get_topological_path failed");
    let dir = fuchsia_fs::directory::open_in_namespace(&topo_path, fuchsia_fs::OpenFlags::empty())
        .expect("failed to open device");
    fvm::format_for_fvm(&dir, 32_768).expect("Failed to format FVM");
    let fvm = fvm::start_fvm_driver(&fvm_controller, &dir).await.expect("Failed to start FVM");
    Some(fvm)
}

impl FvmVolumeFactory {
    pub async fn new() -> Option<Self> {
        // There are two places we might find the FVM:
        // 1. The system might be running out of FVM, in which case we'll work backwards from
        //    Blobfs.
        // 2. The system might not be running out of FVM (i.e. Fxblob), in which case a special
        //    partition called "benchmark" with BENCHMARK_TYPE_GUID should exist that we'll format
        //    as FVM for this test.
        let fvm = if let Some(fvm) = connect_to_system_fvm().await {
            tracing::info!("Using system FVM");
            fvm
        } else if let Some(fvm) = connect_to_test_fvm().await {
            tracing::info!("Using test FVM");
            fvm
        } else {
            return None;
        };

        Some(Self { fvm })
    }
}

#[async_trait]
impl BlockDeviceFactory for FvmVolumeFactory {
    async fn create_block_device(&self, config: &BlockDeviceConfig) -> Box<dyn BlockDevice> {
        Box::new(FvmVolume::new(&self.fvm, config).await)
    }
}

/// A block device created on top of the system's FVM instance.
pub struct FvmVolume {
    volume: VolumeSynchronousProxy,
    volume_dir: fio::DirectoryProxy,
    volume_controller: ControllerProxy,
}

impl FvmVolume {
    async fn new(fvm: &VolumeManagerProxy, config: &BlockDeviceConfig) -> Self {
        let volume_dir = set_up_fvm_volume(fvm, config.fvm_volume_size).await;
        // TODO(https://fxbug.dev/112484): In order to allow multiplexing to be removed, use
        // connect_to_device_fidl to connect to the BlockProxy instead of connect_to_.._dir_root.
        // Requires downstream work, i.e. set_up_fvm_volume() and set_up_insecure_zxcrypt should
        // return controllers.
        let block = connect_to_named_protocol_at_dir_root::<BlockMarker>(&volume_dir, ".").unwrap();
        let volume = VolumeSynchronousProxy::new(block.into_channel().unwrap().into());
        let volume_dir = if config.use_zxcrypt {
            zxcrypt::set_up_insecure_zxcrypt(&volume_dir).await.expect("Failed to set up zxcrypt")
        } else {
            volume_dir
        };

        let volume_controller =
            connect_to_named_protocol_at_dir_root::<ControllerMarker>(&volume_dir, ".")
                .expect("failed to connect to the device controller");

        Self { volume, volume_dir, volume_controller: volume_controller }
    }
}

impl BlockDevice for FvmVolume {
    fn dir(&self) -> &fio::DirectoryProxy {
        &self.volume_dir
    }

    fn controller(&self) -> &ControllerProxy {
        &self.volume_controller
    }
}

impl Drop for FvmVolume {
    fn drop(&mut self) {
        let status =
            self.volume.destroy(zx::Time::INFINITE).expect("Failed to destroy the FVM volume");
        zx::ok(status).expect("Failed to destroy the FVM volume");
    }
}

async fn set_up_fvm_volume(
    volume_manager: &VolumeManagerProxy,
    volume_size: Option<u64>,
) -> fio::DirectoryProxy {
    let instance_guid = create_random_guid();
    fvm::create_fvm_volume(
        volume_manager,
        BENCHMARK_VOLUME_NAME,
        BENCHMARK_TYPE_GUID,
        &instance_guid,
        volume_size,
        ALLOCATE_PARTITION_FLAG_INACTIVE,
    )
    .await
    .expect("Failed to create FVM volume");

    let device_path = wait_for_block_device(&[
        BlockDeviceMatcher::TypeGuid(BENCHMARK_TYPE_GUID),
        BlockDeviceMatcher::InstanceGuid(&instance_guid),
        BlockDeviceMatcher::Name(BENCHMARK_VOLUME_NAME),
    ])
    .await
    .expect("Failed to find the FVM volume");

    // We need to connect to the volume's DirectoryProxy via its topological path in order to
    // allow the caller to access its zxcrypt child. Hence, we use the controller to get access
    // to the topological path and then call open().
    // Connect to the controller and get the device's topological path.
    let controller = connect_to_protocol_at_path::<ControllerMarker>(device_path.to_str().unwrap())
        .expect("failed to connect to controller");
    let topo_path = controller
        .get_topological_path()
        .await
        .expect("transport error on get_topological_path")
        .expect("get_topological_path failed");

    // Open the device via its topological path.
    fuchsia_fs::directory::open_in_namespace(&topo_path, fuchsia_fs::OpenFlags::empty())
        .expect("failed to open device")
}

#[cfg(test)]
mod tests {
    use {
        super::*, fidl_fuchsia_hardware_block_volume::VolumeMarker,
        fuchsia_runtime::vmar_root_self, ramdevice_client::RamdiskClientBuilder,
        test_util::assert_gt,
    };

    const BLOCK_SIZE: u64 = 4 * 1024;
    const BLOCK_COUNT: u64 = 1024;
    // We need more blocks for the GPT version of the test, since the library will by default
    // allocate 128MiB for the embedded FVM.  This is big enough for a 192MiB device.
    const GPT_BLOCK_COUNT: u64 = 49152;

    #[fuchsia::test]
    async fn ramdisk_create_block_device_with_zxcrypt() {
        let ramdisk_factory = RamdiskFactory::new(BLOCK_SIZE, BLOCK_COUNT).await;
        let ramdisk = ramdisk_factory
            .create_block_device(&BlockDeviceConfig { use_zxcrypt: true, fvm_volume_size: None })
            .await;
        let path = ramdisk
            .controller()
            .get_topological_path()
            .await
            .expect("Failed to get topological path")
            .map_err(zx::Status::from_raw)
            .expect("Failed to get topological path");
        assert!(path.contains("/zxcrypt/"), "block device path does not contain zxcrypt: {}", path);
    }

    #[fuchsia::test]
    async fn ramdisk_create_block_device_without_zxcrypt() {
        let ramdisk_factory = RamdiskFactory::new(BLOCK_SIZE, BLOCK_COUNT).await;
        let ramdisk = ramdisk_factory
            .create_block_device(&BlockDeviceConfig { use_zxcrypt: false, fvm_volume_size: None })
            .await;
        let path = ramdisk
            .controller()
            .get_topological_path()
            .await
            .expect("Failed to get topological path")
            .map_err(zx::Status::from_raw)
            .expect("Failed to get topological path");
        assert!(
            !path.contains("/zxcrypt/"),
            "block device path should not contain zxcrypt: {}",
            path
        );
    }

    #[fuchsia::test]
    async fn ramdisk_create_block_device_without_volume_size() {
        let ramdisk_factory = RamdiskFactory::new(BLOCK_SIZE, BLOCK_COUNT).await;
        let ramdisk = ramdisk_factory
            .create_block_device(&BlockDeviceConfig { use_zxcrypt: false, fvm_volume_size: None })
            .await;
        let (volume, server) =
            fidl::endpoints::create_proxy::<VolumeMarker>().expect("failed to create proxy");
        let () = ramdisk
            .controller()
            .connect_to_device_fidl(server.into_channel())
            .expect("failed to connect to device fidl");
        let volume_info = volume.get_volume_info().await.unwrap();
        zx::ok(volume_info.0).unwrap();
        let volume_info = volume_info.2.unwrap();
        assert_eq!(volume_info.partition_slice_count, 1);
    }

    #[fuchsia::test]
    async fn ramdisk_create_block_device_with_volume_size() {
        let ramdisk_factory = RamdiskFactory::new(BLOCK_SIZE, BLOCK_COUNT).await;
        let ramdisk = ramdisk_factory
            .create_block_device(&BlockDeviceConfig {
                use_zxcrypt: false,
                fvm_volume_size: Some(RAMDISK_FVM_SLICE_SIZE as u64 * 3),
            })
            .await;
        let (volume, server) =
            fidl::endpoints::create_proxy::<VolumeMarker>().expect("failed to create proxy");
        let () = ramdisk
            .controller()
            .connect_to_device_fidl(server.into_channel())
            .expect("failed to connect to device fidl");
        let volume_info = volume.get_volume_info().await.unwrap();
        zx::ok(volume_info.0).unwrap();
        let volume_info = volume_info.2.unwrap();
        assert_eq!(volume_info.partition_slice_count, 3);
    }

    #[derive(PartialEq)]
    enum TestMode {
        // Simuate an environment where there is a GPT, but no FVM.
        Gpt,
        // Simuate an environment where there is a system FVM.
        Fvm,
    }

    async fn init_ramdisk_for_fvm_volume_factory(mode: TestMode) -> RamdiskClient {
        // The tests are run in an isolated devmgr which doesn't have access to the real FVM or
        // blobfs. Create a ramdisk and initialize it as needed for `mode`.
        if mode == TestMode::Gpt {
            // Initialize a new GPT.

            // The GPT library requires a File-like object or a slice of memory to write into. To
            // avoid unnecessary copies, we temporarily map `vmo` into the test's address space, and
            // pass a byte slice to the GPT library to use.
            let vmo = {
                let size = (BLOCK_SIZE * GPT_BLOCK_COUNT) as usize;
                let vmo = zx::Vmo::create(size as u64).unwrap();
                let flags = zx::VmarFlags::PERM_READ
                    | zx::VmarFlags::PERM_WRITE
                    | zx::VmarFlags::REQUIRE_NON_RESIZABLE;
                let addr = vmar_root_self().map(0, &vmo, 0, size, flags).unwrap();
                // Safety: The `buffer` slice is valid so long as the mapping exists and it's not
                // resizable.  We **must** ensure `addr` is unmapped before returning so it cannot
                // outlive `vmo`.
                assert!(flags.contains(zx::VmarFlags::REQUIRE_NON_RESIZABLE));
                let buffer = unsafe { std::slice::from_raw_parts_mut(addr as *mut u8, size) };

                let vmo_wrapper = Box::new(std::io::Cursor::new(buffer));
                let mut disk = gpt::GptConfig::new()
                    .initialized(false)
                    .writable(true)
                    .logical_block_size(
                        BLOCK_SIZE.try_into().expect("Unsupported logical block size"),
                    )
                    .create_from_device(vmo_wrapper, None)
                    .unwrap();
                disk.update_partitions(std::collections::BTreeMap::new())
                    .expect("Failed to init partitions");
                disk.write().expect("Failed to write GPT");
                vmo
            };
            let ramdisk_client = RamdiskClientBuilder::new_with_vmo(vmo, Some(BLOCK_SIZE))
                .build()
                .await
                .expect("Failed to create ramdisk");
            ramdisk_client
                .as_controller()
                .expect("invalid controller")
                .bind("gpt.cm")
                .await
                .expect("FIDL error calling bind()")
                .map_err(zx::Status::from_raw)
                .expect("bind() returned non-Ok status");
            wait_for_block_device(&[BlockDeviceMatcher::ContentsMatch(DiskFormat::Gpt)])
                .await
                .expect("Failed to wait for GPT to appear");
            ramdisk_client
        } else {
            // Initialize a new FVM with a blob volume.
            let ramdisk_client = RamdiskClient::create(BLOCK_SIZE, BLOCK_COUNT)
                .await
                .expect("Failed to create RamdiskClient");
            let volume_manager = fvm::set_up_fvm(
                ramdisk_client.as_controller().expect("invalid controller"),
                ramdisk_client.as_dir().expect("invalid directory proxy"),
                RAMDISK_FVM_SLICE_SIZE,
            )
            .await
            .expect("Failed to set up FVM");
            fvm::create_fvm_volume(
                &volume_manager,
                BLOBFS_VOLUME_NAME,
                &BLOBFS_TYPE_GUID,
                &create_random_guid(),
                None,
                ALLOCATE_PARTITION_FLAG_INACTIVE,
            )
            .await
            .expect("Failed to create blobfs");
            wait_for_block_device(&[
                BlockDeviceMatcher::Name(BLOBFS_VOLUME_NAME),
                BlockDeviceMatcher::TypeGuid(&BLOBFS_TYPE_GUID),
            ])
            .await
            .expect("Failed to wait for blobfs to appear");
            ramdisk_client
        }
    }

    async fn fvm_volume_factory_can_find_fvm_instance(mode: TestMode) {
        let _ramdisk = init_ramdisk_for_fvm_volume_factory(mode).await;
        let volume_factory = FvmVolumeFactory::new().await.unwrap();

        // Verify that a volume can be created.
        volume_factory
            .create_block_device(&BlockDeviceConfig { use_zxcrypt: false, fvm_volume_size: None })
            .await;
    }

    #[fuchsia::test]
    async fn fvm_volume_factory_can_find_fvm_instance_with_system_fvm() {
        fvm_volume_factory_can_find_fvm_instance(TestMode::Fvm).await;
    }

    #[fuchsia::test]
    async fn fvm_volume_factory_can_find_fvm_instance_without_system_fvm() {
        fvm_volume_factory_can_find_fvm_instance(TestMode::Gpt).await;
    }

    async fn get_fvm_used_slices(fvm: &VolumeManagerProxy) -> u64 {
        let info = fvm.get_info().await.unwrap();
        zx::ok(info.0).unwrap();
        let info = info.1.unwrap();
        info.assigned_slice_count
    }

    async fn dropping_an_fvm_volume_removes_the_volume(mode: TestMode) {
        let _ramdisk = init_ramdisk_for_fvm_volume_factory(mode).await;
        let volume_factory = FvmVolumeFactory::new().await.unwrap();
        let used_slices = get_fvm_used_slices(&volume_factory.fvm).await;

        let volume = volume_factory
            .create_block_device(&BlockDeviceConfig { use_zxcrypt: false, fvm_volume_size: None })
            .await;
        // The number of used slices should have gone up.
        assert_gt!(get_fvm_used_slices(&volume_factory.fvm).await, used_slices);

        std::mem::drop(volume);
        // The number of used slices should have gone back down.
        assert_eq!(get_fvm_used_slices(&volume_factory.fvm).await, used_slices);
    }

    #[fuchsia::test]
    async fn dropping_an_fvm_volume_removes_the_volume_with_system_fvm() {
        dropping_an_fvm_volume_removes_the_volume(TestMode::Fvm).await;
    }

    #[fuchsia::test]
    async fn dropping_an_fvm_volume_removes_the_volume_without_system_fvm() {
        dropping_an_fvm_volume_removes_the_volume(TestMode::Gpt).await;
    }

    async fn fvm_volume_factory_create_block_device_with_zxcrypt(mode: TestMode) {
        let _ramdisk = init_ramdisk_for_fvm_volume_factory(mode).await;
        let volume_factory = FvmVolumeFactory::new().await.unwrap();
        let volume = volume_factory
            .create_block_device(&BlockDeviceConfig { use_zxcrypt: true, fvm_volume_size: None })
            .await;

        let path = volume
            .controller()
            .get_topological_path()
            .await
            .expect("Failed to get topological path")
            .map_err(zx::Status::from_raw)
            .expect("Failed to get topological path");
        assert!(path.contains("/zxcrypt/"), "block device path does not contain zxcrypt: {}", path);
    }

    #[fuchsia::test]
    async fn fvm_volume_factory_create_block_device_with_zxcrypt_with_fvm() {
        fvm_volume_factory_create_block_device_with_zxcrypt(TestMode::Fvm).await;
    }

    #[fuchsia::test]
    async fn fvm_volume_factory_create_block_device_with_zxcrypt_without_fvm() {
        fvm_volume_factory_create_block_device_with_zxcrypt(TestMode::Gpt).await;
    }

    async fn fvm_volume_factory_create_block_device_without_zxcrypt(mode: TestMode) {
        let _ramdisk = init_ramdisk_for_fvm_volume_factory(mode).await;
        let volume_factory = FvmVolumeFactory::new().await.unwrap();
        let volume = volume_factory
            .create_block_device(&BlockDeviceConfig { use_zxcrypt: false, fvm_volume_size: None })
            .await;

        let path = volume
            .controller()
            .get_topological_path()
            .await
            .expect("Failed to get topological path")
            .map_err(zx::Status::from_raw)
            .expect("Failed to get topological path");
        assert!(
            !path.contains("/zxcrypt/"),
            "block device path should not contain zxcrypt: {}",
            path
        );
    }

    #[fuchsia::test]
    async fn fvm_volume_factory_create_block_device_without_zxcrypt_with_system_fvm() {
        fvm_volume_factory_create_block_device_without_zxcrypt(TestMode::Fvm).await;
    }

    #[fuchsia::test]
    async fn fvm_volume_factory_create_block_device_without_zxcrypt_without_system_fvm() {
        fvm_volume_factory_create_block_device_without_zxcrypt(TestMode::Gpt).await;
    }
}
