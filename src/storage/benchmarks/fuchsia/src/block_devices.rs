// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    async_trait::async_trait,
    fidl::endpoints::Proxy,
    fidl_fuchsia_device::{ControllerMarker, ControllerProxy},
    fidl_fuchsia_hardware_block::BlockMarker,
    fidl_fuchsia_hardware_block_volume::ALLOCATE_PARTITION_FLAG_INACTIVE,
    fidl_fuchsia_hardware_block_volume::{
        VolumeManagerMarker, VolumeManagerProxy, VolumeSynchronousProxy,
    },
    fidl_fuchsia_io as fio,
    fs_management::BLOBFS_TYPE_GUID,
    fuchsia_component::client::{
        connect_to_named_protocol_at_dir_root, connect_to_protocol_at_path,
    },
    fuchsia_zircon::{self as zx},
    ramdevice_client::RamdiskClient,
    std::path::PathBuf,
    storage_benchmarks::{block_device::BlockDevice, BlockDeviceConfig, BlockDeviceFactory},
    storage_isolated_driver_manager::{
        create_random_guid, fvm, wait_for_block_device, zxcrypt, BlockDeviceMatcher, Guid,
    },
};

const RAMDISK_FVM_SLICE_SIZE: usize = 1024 * 1024;
const BLOBFS_VOLUME_NAME: &str = "blobfs";

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
    volume_controller: Option<ControllerProxy>,
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

        Self { _ramdisk: ramdisk, volume_dir, volume_controller: Some(volume_controller) }
    }
}

impl BlockDevice for Ramdisk {
    fn as_dir(&self) -> &fio::DirectoryProxy {
        &self.volume_dir
    }

    fn as_controller(&self) -> Option<&ControllerProxy> {
        self.volume_controller.as_ref()
    }

    fn take_controller(&mut self) -> Option<ControllerProxy> {
        self.volume_controller.take()
    }
}

/// Creates block devices on top of the system's FVM instance.
pub struct FvmVolumeFactory {
    fvm: VolumeManagerProxy,
}

impl FvmVolumeFactory {
    pub async fn new() -> Self {
        // Find Blobfs' volume then work backwards from Blobfs' topological path to find FVM.
        let blobfs_dev_path = wait_for_block_device(&[
            BlockDeviceMatcher::Name(BLOBFS_VOLUME_NAME),
            BlockDeviceMatcher::TypeGuid(&BLOBFS_TYPE_GUID),
        ])
        .await
        .expect("Failed to find Blobfs");

        let blobfs_controller =
            connect_to_protocol_at_path::<ControllerMarker>(blobfs_dev_path.to_str().unwrap())
                .unwrap();
        let path = blobfs_controller
            .get_topological_path()
            .await
            .expect("Failed to get topological path")
            .map_err(zx::Status::from_raw)
            .expect("Failed to get topological path");

        let mut path = PathBuf::from(path);
        if !path.pop() || !path.pop() {
            panic!("Unexpected topological path for Blobfs {}", path.display());
        }

        match path.file_name() {
            Some(p) => assert!(p == "fvm", "Unexpected FVM path: {}", path.display()),
            None => panic!("Unexpected FVM path: {}", path.display()),
        }
        let fvm =
            connect_to_protocol_at_path::<VolumeManagerMarker>(path.to_str().unwrap()).unwrap();

        Self { fvm }
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
    volume_controller: Option<ControllerProxy>,
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

        Self { volume, volume_dir, volume_controller: Some(volume_controller) }
    }
}

impl BlockDevice for FvmVolume {
    fn as_dir(&self) -> &fio::DirectoryProxy {
        &self.volume_dir
    }

    fn as_controller(&self) -> Option<&ControllerProxy> {
        self.volume_controller.as_ref()
    }

    fn take_controller(&mut self) -> Option<ControllerProxy> {
        self.volume_controller.take()
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
    const BENCHMARK_TYPE_GUID: &Guid = &[
        0x67, 0x45, 0x23, 0x01, 0xab, 0x89, 0xef, 0xcd, 0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd,
        0xef,
    ];
    const BENCHMARK_VOLUME_NAME: &str = "benchmark";

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
    use {super::*, fidl_fuchsia_hardware_block_volume::VolumeMarker, test_util::assert_gt};

    const BLOCK_SIZE: u64 = 4 * 1024;
    const BLOCK_COUNT: u64 = 1024;

    #[fuchsia::test]
    async fn ramdisk_create_block_device_with_zxcrypt() {
        let ramdisk_factory = RamdiskFactory::new(BLOCK_SIZE, BLOCK_COUNT).await;
        let ramdisk = ramdisk_factory
            .create_block_device(&BlockDeviceConfig { use_zxcrypt: true, fvm_volume_size: None })
            .await;
        let controller = ramdisk.as_controller().expect("invalid device controller");
        let path = controller
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
        let controller = ramdisk.as_controller().expect("invalid device controller");
        let path = controller
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
        let ramdisk_controller = ramdisk.as_controller().expect("invalid ramdisk controller");
        let (volume, server) =
            fidl::endpoints::create_proxy::<VolumeMarker>().expect("failed to create proxy");
        let () = ramdisk_controller
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
        let ramdisk_controller = ramdisk.as_controller().expect("invalid ramdisk controller");
        let (volume, server) =
            fidl::endpoints::create_proxy::<VolumeMarker>().expect("failed to create proxy");
        let () = ramdisk_controller
            .connect_to_device_fidl(server.into_channel())
            .expect("failed to connect to device fidl");
        let volume_info = volume.get_volume_info().await.unwrap();
        zx::ok(volume_info.0).unwrap();
        let volume_info = volume_info.2.unwrap();
        assert_eq!(volume_info.partition_slice_count, 3);
    }

    async fn init_ramdisk_for_fvm_volume_factory() -> RamdiskClient {
        // The tests are run in an isolated devmgr which doesn't have access to the real FVM or
        // blobfs. Create a ramdisk, set up fvm, and add a blobfs volume for `FvmVolumeFactory` to
        // find.
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
        ramdisk_client
    }

    #[fuchsia::test]
    async fn fvm_volume_factory_can_find_fvm_instance() {
        let _ramdisk = init_ramdisk_for_fvm_volume_factory().await;
        let volume_factory = FvmVolumeFactory::new().await;

        // Verify that a volume can be created.
        volume_factory
            .create_block_device(&BlockDeviceConfig { use_zxcrypt: false, fvm_volume_size: None })
            .await;
    }

    async fn get_fvm_used_slices(fvm: &VolumeManagerProxy) -> u64 {
        let info = fvm.get_info().await.unwrap();
        zx::ok(info.0).unwrap();
        let info = info.1.unwrap();
        info.assigned_slice_count
    }

    #[fuchsia::test]
    async fn dropping_an_fvm_volume_removes_the_volume() {
        let _ramdisk = init_ramdisk_for_fvm_volume_factory().await;
        let volume_factory = FvmVolumeFactory::new().await;
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
    async fn fvm_volume_factory_create_block_device_with_zxcrypt() {
        let _ramdisk = init_ramdisk_for_fvm_volume_factory().await;
        let volume_factory = FvmVolumeFactory::new().await;
        let volume = volume_factory
            .create_block_device(&BlockDeviceConfig { use_zxcrypt: true, fvm_volume_size: None })
            .await;

        let controller = volume.as_controller().expect("invalid device controller");
        let path = controller
            .get_topological_path()
            .await
            .expect("Failed to get topological path")
            .map_err(zx::Status::from_raw)
            .expect("Failed to get topological path");
        assert!(path.contains("/zxcrypt/"), "block device path does not contain zxcrypt: {}", path);
    }

    #[fuchsia::test]
    async fn fvm_volume_factory_create_block_device_without_zxcrypt() {
        let _ramdisk = init_ramdisk_for_fvm_volume_factory().await;
        let volume_factory = FvmVolumeFactory::new().await;
        let volume = volume_factory
            .create_block_device(&BlockDeviceConfig { use_zxcrypt: false, fvm_volume_size: None })
            .await;

        let controller = volume.as_controller().expect("invalid device controller");
        let path = controller
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
}
