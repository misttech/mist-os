// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    device_watcher::recursive_wait_and_open_node,
    fidl::endpoints::Proxy,
    fidl_fuchsia_device::ControllerProxy,
    fidl_fuchsia_fxfs::{CryptManagementMarker, CryptMarker, KeyPurpose},
    fidl_fuchsia_io as fio, fidl_fuchsia_logger as flogger,
    fs_management::{
        format::constants::{F2FS_MAGIC, FXFS_MAGIC, MINFS_MAGIC},
        Blobfs, Fxfs, BLOBFS_TYPE_GUID, DATA_TYPE_GUID, FVM_TYPE_GUID_STR,
    },
    fuchsia_component_test::{Capability, ChildOptions, RealmBuilder, RealmInstance, Ref, Route},
    fuchsia_runtime::vmar_root_self,
    fuchsia_zircon::{self as zx, HandleBased},
    gpt::{partition_types, GptConfig},
    key_bag::Aes256Key,
    ramdevice_client::{RamdiskClient, RamdiskClientBuilder},
    remote_block_device::BlockClient as _,
    std::{io::Write, ops::Deref, path::Path},
    storage_isolated_driver_manager::{
        fvm::{create_fvm_volume, set_up_fvm},
        zxcrypt,
    },
    uuid::Uuid,
    zerocopy::AsBytes,
};

const GPT_DRIVER_PATH: &str = "gpt.so";

const RAMDISK_BLOCK_SIZE: u64 = 512;
pub const FVM_SLICE_SIZE: u64 = 32 * 1024;

// We set the default disk size to be twice the value of
// DEFAULT_F2FS_MIN_BYTES (defined in device/constants.rs)
// to ensure that when f2fs is the data filesystem format,
// we don't run out of space. Similarly, the size of the data
// volume should be > DEFAULT_F2FS_MIN_BYTES but < disk size
pub const DEFAULT_DISK_SIZE: u64 = 200 * 1024 * 1024;
pub const DEFAULT_DATA_VOLUME_SIZE: u64 = 101 * 1024 * 1024;

// We use a static key-bag so that the crypt instance can be shared across test executions safely.
// These keys match the DATA_KEY and METADATA_KEY respectively, when wrapped with the "zxcrypt"
// static key used by fshost.
// Note this isn't used in the legacy crypto format.
const KEY_BAG_CONTENTS: &'static str = "\
{
    \"version\":1,
    \"keys\": {
        \"0\":{
            \"Aes128GcmSivWrapped\": [
                \"7a7c6a718cfde7078f6edec5\",
                \"7cc31b765c74db3191e269d2666267022639e758fe3370e8f36c166d888586454fd4de8aeb47aadd81c531b0a0a66f27\"
            ]
        },
        \"1\":{
            \"Aes128GcmSivWrapped\": [
                \"b7d7f459cbee4cc536cc4324\",
                \"9f6a5d894f526b61c5c091e5e02a7ff94d18e6ad36a0aa439c86081b726eca79e6b60bd86ee5d86a20b3df98f5265a99\"
            ]
        }
    }
}";

const DATA_KEY: Aes256Key = Aes256Key::create([
    0xcf, 0x9e, 0x45, 0x2a, 0x22, 0xa5, 0x70, 0x31, 0x33, 0x3b, 0x4d, 0x6b, 0x6f, 0x78, 0x58, 0x29,
    0x04, 0x79, 0xc7, 0xd6, 0xa9, 0x4b, 0xce, 0x82, 0x04, 0x56, 0x5e, 0x82, 0xfc, 0xe7, 0x37, 0xa8,
]);

const METADATA_KEY: Aes256Key = Aes256Key::create([
    0x0f, 0x4d, 0xca, 0x6b, 0x35, 0x0e, 0x85, 0x6a, 0xb3, 0x8c, 0xdd, 0xe9, 0xda, 0x0e, 0xc8, 0x22,
    0x8e, 0xea, 0xd8, 0x05, 0xc4, 0xc9, 0x0b, 0xa8, 0xd8, 0x85, 0x87, 0x50, 0x75, 0x40, 0x1c, 0x4c,
]);

// Matches the hard-coded value used by fshost when use_native_fxfs_crypto is false.
const LEGACY_DATA_KEY: Aes256Key = Aes256Key::create([
    0x0, 0x1, 0x2, 0x3, 0x4, 0x5, 0x6, 0x7, 0x8, 0x9, 0xa, 0xb, 0xc, 0xd, 0xe, 0xf, 0x10, 0x11,
    0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b, 0x1c, 0x1d, 0x1e, 0x1f,
]);

// Matches the hard-coded value used by fshost when use_native_fxfs_crypto is false.
const LEGACY_METADATA_KEY: Aes256Key = Aes256Key::create([
    0xff, 0xfe, 0xfd, 0xfc, 0xfb, 0xfa, 0xf9, 0xf8, 0xf7, 0xf6, 0xf5, 0xf4, 0xf3, 0xf2, 0xf1, 0xf0,
    0xef, 0xee, 0xed, 0xec, 0xeb, 0xea, 0xe9, 0xe8, 0xe7, 0xe6, 0xe5, 0xe4, 0xe3, 0xe2, 0xe1, 0xe0,
]);

fn initialize_gpt(vmo: &zx::Vmo, block_size: u64) {
    // The GPT library requires a File-like object or a slice of memory to write into. To avoid
    // unnecessary copies, we temporarily map `vmo` into the test's address space, and pass a
    // byte slice to the GPT library to use.
    let flags =
        zx::VmarFlags::PERM_READ | zx::VmarFlags::PERM_WRITE | zx::VmarFlags::REQUIRE_NON_RESIZABLE;
    let size: usize = vmo.get_size().expect("Failed to obtain VMO size").try_into().unwrap();
    let addr = vmar_root_self().map(0, &vmo, 0, size, flags).unwrap();
    // Safety: The `buffer` slice is valid so long as the mapping exists and it's not resizable.
    // We **must** ensure `addr` is unmapped before returning so it cannot outlive `vmo`.
    assert!(flags.contains(zx::VmarFlags::REQUIRE_NON_RESIZABLE));
    let buffer = unsafe { std::slice::from_raw_parts_mut(addr as *mut u8, size) };

    // Initialize a new GPT with a single FVM partition.
    let vmo_wrapper = Box::new(std::io::Cursor::new(buffer));
    let mut disk = GptConfig::new()
        .initialized(false)
        .writable(true)
        .logical_block_size(block_size.try_into().expect("Unsupported logical block size"))
        .create_from_device(vmo_wrapper, None)
        .unwrap();
    disk.update_partitions(std::collections::BTreeMap::new()).expect("Failed to initialize GPT");

    let sectors = disk.find_free_sectors();
    assert!(sectors.len() == 1);
    let block_size: u64 = disk.logical_block_size().clone().into();
    let available_space = block_size * sectors[0].1 as u64;
    let fvm_type = partition_types::Type {
        guid: FVM_TYPE_GUID_STR,
        os: partition_types::OperatingSystem::Custom("Fuchsia".to_owned()),
    };
    disk.add_partition("fvm", available_space, fvm_type, 0, None).unwrap();
    let disk = disk.write().expect("Failed to write GPT");

    // Safety: We have to ensure we drop all objects that own a reference to `buffer`, and ensure
    // that we unmap the region before returning (so it cannot outlive `vmo`).
    unsafe {
        std::mem::drop(disk);
        vmar_root_self().unmap(addr, size).expect("Failed to unmap VMAR");
    }
}

async fn bind_gpt_driver(ramdisk: &RamdiskClient) {
    let ramdisk_channel =
        fidl::AsyncChannel::from_channel(ramdisk.open().await.unwrap().into_channel()).unwrap();
    let controller_proxy = ControllerProxy::new(ramdisk_channel);
    controller_proxy
        .bind(GPT_DRIVER_PATH)
        .await
        .expect("FIDL error calling bind()")
        .map_err(zx::Status::from_raw)
        .expect("bind() returned non-Ok status");
}

async fn create_hermetic_crypt_service(
    data_key: Aes256Key,
    metadata_key: Aes256Key,
) -> RealmInstance {
    let builder = RealmBuilder::new().await.unwrap();
    let url = "#meta/fxfs-crypt.cm";
    let crypt = builder.add_child("fxfs-crypt", url, ChildOptions::new().eager()).await.unwrap();
    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol::<CryptMarker>())
                .capability(Capability::protocol::<CryptManagementMarker>())
                .from(&crypt)
                .to(Ref::parent()),
        )
        .await
        .unwrap();
    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol::<flogger::LogSinkMarker>())
                .from(Ref::parent())
                .to(&crypt),
        )
        .await
        .unwrap();
    let realm = builder.build().await.expect("realm build failed");
    let crypt_management =
        realm.root.connect_to_protocol_at_exposed_dir::<CryptManagementMarker>().unwrap();
    crypt_management
        .add_wrapping_key(0, data_key.deref())
        .await
        .unwrap()
        .expect("add_wrapping_key failed");
    crypt_management
        .add_wrapping_key(1, metadata_key.deref())
        .await
        .unwrap()
        .expect("add_wrapping_key failed");
    crypt_management
        .set_active_key(KeyPurpose::Data, 0)
        .await
        .unwrap()
        .expect("set_active_key failed");
    crypt_management
        .set_active_key(KeyPurpose::Metadata, 1)
        .await
        .unwrap()
        .expect("set_active_key failed");
    realm
}

pub enum Disk {
    Prebuilt(zx::Vmo),
    Builder(DiskBuilder),
}

impl Disk {
    pub async fn get_vmo(self) -> zx::Vmo {
        match self {
            Disk::Prebuilt(vmo) => vmo,
            Disk::Builder(builder) => builder.build().await,
        }
    }

    pub fn builder(&mut self) -> &mut DiskBuilder {
        match self {
            Disk::Prebuilt(_) => panic!("attempted to get builder for prebuilt disk"),
            Disk::Builder(builder) => builder,
        }
    }
}

#[derive(Default, Debug)]
pub struct DataSpec {
    pub format: Option<&'static str>,
    pub zxcrypt: bool,
    pub legacy_crypto_format: bool,
}

pub struct DiskBuilder {
    size: u64,
    data_volume_size: u64,
    data_spec: DataSpec,
    // Only used if `format` is Some.
    corrupt_contents: bool,
    gpt: bool,
    format_fvm: bool,
}

impl DiskBuilder {
    pub fn new() -> DiskBuilder {
        DiskBuilder {
            size: DEFAULT_DISK_SIZE,
            data_volume_size: DEFAULT_DATA_VOLUME_SIZE,
            data_spec: DataSpec { format: None, zxcrypt: false, legacy_crypto_format: false },
            corrupt_contents: false,
            gpt: false,
            format_fvm: true,
        }
    }

    pub fn size(&mut self, size: u64) -> &mut Self {
        self.size = size;
        self
    }

    pub fn data_volume_size(&mut self, data_volume_size: u64) -> &mut Self {
        self.data_volume_size = data_volume_size;
        self
    }

    pub fn format_data(&mut self, data_spec: DataSpec) -> &mut Self {
        tracing::info!(?data_spec, "formatting data volume");
        assert!(self.format_fvm);
        self.data_spec = data_spec;
        self
    }

    pub fn corrupt_data(&mut self) -> &mut Self {
        self.corrupt_contents = true;
        self
    }

    pub fn with_gpt(&mut self) -> &mut Self {
        self.gpt = true;
        self
    }

    pub fn with_unformatted_fvm(&mut self) -> &mut Self {
        assert!(self.data_spec.format.is_none());
        self.format_fvm = false;
        self
    }

    pub async fn build(self) -> zx::Vmo {
        let vmo = zx::Vmo::create(self.size).unwrap();

        // Initialize the VMO with GPT headers and an *empty* FVM partition.
        if self.gpt {
            initialize_gpt(&vmo, RAMDISK_BLOCK_SIZE);
        }

        if !self.format_fvm {
            return vmo;
        }

        // Create a ramdisk with a duplicate handle of `vmo` so we can keep the data once destroyed.
        let ramdisk = RamdiskClientBuilder::new_with_vmo(
            vmo.duplicate_handle(zx::Rights::SAME_RIGHTS).unwrap(),
            Some(RAMDISK_BLOCK_SIZE),
        )
        .build()
        .await
        .unwrap();

        let dev = fuchsia_fs::directory::open_in_namespace(
            "/dev",
            fidl_fuchsia_io::OpenFlags::RIGHT_READABLE,
        )
        .unwrap();

        // Path to block device or partition which will back the FVM. Assumed to be empty/zeroed.
        let base_path = if self.gpt {
            bind_gpt_driver(&ramdisk).await;
            let fvm_partition_path = format!("{}/part-000/block", ramdisk.get_path());
            recursive_wait_and_open_node(&dev, &fvm_partition_path.strip_prefix("/dev/").unwrap())
                .await
                .expect("recursive_wait_and_open_node failed");
            fvm_partition_path
        } else {
            ramdisk.get_path().to_owned()
        };

        // Initialize/provision the FVM headers and bind the FVM driver.
        let volume_manager_proxy =
            set_up_fvm(Path::new(base_path.as_str()), FVM_SLICE_SIZE as usize)
                .await
                .expect("set_up_fvm failed");

        // Create and format the blobfs partition.
        create_fvm_volume(
            &volume_manager_proxy,
            "blobfs",
            &BLOBFS_TYPE_GUID,
            Uuid::new_v4().as_bytes(),
            None,
            0,
        )
        .await
        .expect("create_fvm_volume failed");
        let blobfs_path = format!("{}/fvm/blobfs-p-1/block", base_path);
        recursive_wait_and_open_node(&dev, &blobfs_path.strip_prefix("/dev/").unwrap())
            .await
            .expect("recursive_wait_and_open_node failed");
        let mut blobfs = Blobfs::new(&blobfs_path).expect("new failed");
        blobfs.format().await.expect("format failed");

        // Create and format the data partition.
        create_fvm_volume(
            &volume_manager_proxy,
            "data",
            &DATA_TYPE_GUID,
            Uuid::new_v4().as_bytes(),
            Some(self.data_volume_size),
            0,
        )
        .await
        .expect("create_fvm_volume failed");

        let data_path = format!("{}/fvm/data-p-2/block", base_path);
        let mut data_device =
            recursive_wait_and_open_node(&dev, &data_path.strip_prefix("/dev/").unwrap())
                .await
                .expect("recursive_wait_and_open_node failed");

        // Potentially set up zxcrypt, if we are configured to and aren't using Fxfs.
        if self.data_spec.format != Some("fxfs") && self.data_spec.zxcrypt {
            let zxcrypt_path = zxcrypt::set_up_insecure_zxcrypt(Path::new(&data_path))
                .await
                .expect("failed to set up zxcrypt");
            let zxcrypt_path = zxcrypt_path.as_os_str().to_str().unwrap();
            data_device =
                recursive_wait_and_open_node(&dev, zxcrypt_path.strip_prefix("/dev/").unwrap())
                    .await
                    .expect("recursive_wait_and_open_node failed");
        }

        if let Some(format) = self.data_spec.format {
            match format {
                "fxfs" => self.init_data_fxfs(data_device).await,
                "minfs" => self.init_data_minfs(data_device).await,
                "f2fs" => self.init_data_f2fs(data_device).await,
                _ => panic!("unsupported data filesystem format type"),
            }
        }

        // Destroy the ramdisk device and return a VMO containing the partitions/filesystems.
        ramdisk.destroy().await.expect("destroy failed");
        vmo
    }

    async fn init_data_minfs(&self, data_device: fio::NodeProxy) {
        if self.corrupt_contents {
            // Just write the magic so it appears formatted to fshost.
            self.write_magic(
                fidl_fuchsia_hardware_block::BlockProxy::new(data_device.into_channel().unwrap()),
                MINFS_MAGIC,
                0,
            )
            .await;
            return;
        }

        let mut minfs = fs_management::Minfs::from_channel(
            data_device.into_channel().unwrap().into_zx_channel(),
        )
        .expect("from_channel failed");
        minfs.format().await.expect("format failed");
        let fs = minfs.serve().await.expect("serve_single_volume failed");
        self.write_test_data(&fs.root()).await;
        fs.shutdown().await.expect("shutdown failed");
    }

    async fn init_data_f2fs(&self, data_device: fio::NodeProxy) {
        if self.corrupt_contents {
            // Just write the magic so it appears formatted to fshost.
            self.write_magic(
                fidl_fuchsia_hardware_block::BlockProxy::new(data_device.into_channel().unwrap()),
                F2FS_MAGIC,
                1024,
            )
            .await;
            return;
        }

        let mut f2fs = fs_management::F2fs::from_channel(
            data_device.into_channel().unwrap().into_zx_channel(),
        )
        .expect("from_channel failed");
        f2fs.format().await.expect("format failed");
        let fs = f2fs.serve().await.expect("serve_single_volume failed");
        self.write_test_data(&fs.root()).await;
        fs.shutdown().await.expect("shutdown failed");
    }

    async fn init_data_fxfs(&self, data_device: fio::NodeProxy) {
        if self.corrupt_contents {
            // Just write the magic so it appears formatted to fshost.
            self.write_magic(
                fidl_fuchsia_hardware_block::BlockProxy::new(data_device.into_channel().unwrap()),
                FXFS_MAGIC,
                0,
            )
            .await;
            return;
        }

        let (data_key, metadata_key) = if self.data_spec.legacy_crypto_format {
            (LEGACY_DATA_KEY, LEGACY_METADATA_KEY)
        } else {
            (DATA_KEY, METADATA_KEY)
        };
        let crypt_realm = create_hermetic_crypt_service(data_key, metadata_key).await;
        let mut fxfs = Fxfs::from_channel(data_device.into_channel().unwrap().into_zx_channel())
            .expect("from_channel failed");
        fxfs.format().await.expect("format failed");
        let mut fs = fxfs.serve_multi_volume().await.expect("serve_multi_volume failed");
        let vol = if self.data_spec.legacy_crypto_format {
            let crypt_service = Some(
                crypt_realm
                    .root
                    .connect_to_protocol_at_exposed_dir::<CryptMarker>()
                    .expect("Unable to connect to Crypt service")
                    .into_channel()
                    .unwrap()
                    .into_zx_channel()
                    .into(),
            );
            fs.create_volume("default", crypt_service).await.expect("create_volume failed")
        } else {
            let vol = fs.create_volume("unencrypted", None).await.expect("create_volume failed");
            vol.bind_to_path("/unencrypted_volume").unwrap();
            // Initialize the key-bag with the static keys.
            std::fs::create_dir("/unencrypted_volume/keys").expect("create_dir failed");
            let mut file = std::fs::File::create("/unencrypted_volume/keys/fxfs-data")
                .expect("create file failed");
            file.write_all(KEY_BAG_CONTENTS.as_bytes()).expect("write file failed");

            let crypt_service = Some(
                crypt_realm
                    .root
                    .connect_to_protocol_at_exposed_dir::<CryptMarker>()
                    .expect("Unable to connect to Crypt service")
                    .into_channel()
                    .unwrap()
                    .into_zx_channel()
                    .into(),
            );
            fs.create_volume("data", crypt_service).await.expect("create_volume failed")
        };
        self.write_test_data(&vol.root()).await;
        fs.shutdown().await.expect("shutdown failed");
    }

    /// Create a small set of known files to test for presence. The test tree is
    ///  root
    ///   |- foo (file, empty)
    ///   |- ssh (directory, non-empty)
    ///   |   |- authorized_keys (file, non-empty)
    ///   |   |- config (directory, empty)
    ///   |- problems (directory, empty (no problems))
    async fn write_test_data(&self, root: &fio::DirectoryProxy) {
        fuchsia_fs::directory::open_file(
            root,
            "foo",
            fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::CREATE,
        )
        .await
        .unwrap();

        let ssh_dir = fuchsia_fs::directory::create_directory(
            root,
            "ssh",
            fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE,
        )
        .await
        .unwrap();
        let authorized_keys = fuchsia_fs::directory::open_file(
            &ssh_dir,
            "authorized_keys",
            fio::OpenFlags::RIGHT_READABLE
                | fio::OpenFlags::RIGHT_WRITABLE
                | fio::OpenFlags::CREATE,
        )
        .await
        .unwrap();
        fuchsia_fs::file::write(&authorized_keys, "public key!").await.unwrap();
        fuchsia_fs::directory::create_directory(&ssh_dir, "config", fio::OpenFlags::RIGHT_READABLE)
            .await
            .unwrap();

        fuchsia_fs::directory::create_directory(&root, "problems", fio::OpenFlags::RIGHT_READABLE)
            .await
            .unwrap();
    }

    async fn write_magic<const N: usize>(
        &self,
        block_proxy: fidl_fuchsia_hardware_block::BlockProxy,
        value: [u8; N],
        offset: u64,
    ) {
        let client = remote_block_device::RemoteBlockClient::new(block_proxy)
            .await
            .expect("Failed to create client");
        let block_size = client.block_size() as usize;
        assert!(value.len() <= block_size);
        let mut data = vec![0xffu8; block_size];
        data[..value.len()].copy_from_slice(&value);
        let buffer = remote_block_device::BufferSlice::Memory(&data[..]);
        client.write_at(buffer, offset).await.expect("write failed");
    }

    /// Create a vmo artifact with the format of a compressed zbi boot item containing this
    /// filesystem.
    pub(crate) async fn build_as_zbi_ramdisk(self) -> zx::Vmo {
        /// The following types and constants are defined in
        /// zircon/system/public/zircon/boot/image.h.
        const ZBI_TYPE_STORAGE_RAMDISK: u32 = 0x4b534452;
        const ZBI_FLAGS_VERSION: u32 = 0x00010000;
        const ZBI_ITEM_MAGIC: u32 = 0xb5781729;
        const ZBI_FLAGS_STORAGE_COMPRESSED: u32 = 0x00000001;

        #[repr(C)]
        #[derive(AsBytes)]
        struct ZbiHeader {
            type_: u32,
            length: u32,
            extra: u32,
            flags: u32,
            _reserved0: u32,
            _reserved1: u32,
            magic: u32,
            _crc32: u32,
        }

        let ramdisk_vmo = self.build().await;
        let extra = ramdisk_vmo.get_size().unwrap() as u32;
        let mut decompressed_buf = vec![0u8; extra as usize];
        ramdisk_vmo.read(&mut decompressed_buf, 0).unwrap();
        let compressed_buf = zstd::encode_all(decompressed_buf.as_slice(), 0).unwrap();
        let length = compressed_buf.len() as u32;

        let header = ZbiHeader {
            type_: ZBI_TYPE_STORAGE_RAMDISK,
            length,
            extra,
            flags: ZBI_FLAGS_VERSION | ZBI_FLAGS_STORAGE_COMPRESSED,
            _reserved0: 0,
            _reserved1: 0,
            magic: ZBI_ITEM_MAGIC,
            _crc32: 0,
        };

        let header_size = std::mem::size_of::<ZbiHeader>() as u64;
        let zbi_vmo = zx::Vmo::create(header_size + length as u64).unwrap();
        zbi_vmo.write(header.as_bytes(), 0).unwrap();
        zbi_vmo.write(&compressed_buf, header_size).unwrap();

        zbi_vmo
    }
}
