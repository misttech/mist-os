// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use blob_writer::BlobWriter;
use block_client::{BlockClient as _, RemoteBlockClient};
use delivery_blob::{delivery_blob_path, CompressionMode, Type1Blob};
use fake_block_server::FakeServer;
use fidl::endpoints::{create_proxy, Proxy as _, ServerEnd};
use fidl_fuchsia_fs_startup::{CreateOptions, MountOptions};
use fidl_fuchsia_fxfs::{BlobCreatorProxy, CryptManagementMarker, CryptMarker, KeyPurpose};
use fs_management::filesystem::{
    BlockConnector, DirBasedBlockConnector, Filesystem, ServingMultiVolumeFilesystem,
};
use fs_management::format::constants::{F2FS_MAGIC, FXFS_MAGIC, MINFS_MAGIC};
use fs_management::{Fvm, Fxfs, BLOBFS_TYPE_GUID, DATA_TYPE_GUID, FVM_TYPE_GUID};
use fuchsia_component::client::connect_to_protocol_at_dir_svc;
use fuchsia_component_test::{Capability, ChildOptions, RealmBuilder, RealmInstance, Ref, Route};
use fuchsia_hash::Hash;
use gpt_component::gpt::GptManager;
use key_bag::Aes256Key;
use std::ops::Deref;
use std::sync::Arc;
use storage_isolated_driver_manager::fvm::format_for_fvm;
use uuid::Uuid;
use vfs::directory::entry_container::Directory;
use vfs::execution_scope::ExecutionScope;
use vfs::path::Path;
use vfs::ObjectRequest;
use zerocopy::{Immutable, IntoBytes};
use zx::{self as zx, HandleBased};
use {fidl_fuchsia_io as fio, fidl_fuchsia_logger as flogger, fuchsia_async as fasync};

const TEST_DISK_BLOCK_SIZE: u32 = 512;
pub const FVM_SLICE_SIZE: u64 = 32 * 1024;

// The default disk size is about 110MiB, with about 106MiB dedicated to the data volume. This size
// is chosen because the data volume has to be big enough to support f2fs, which has a relatively
// large minimum size requirement to be formatted.
//
// Only the data volume is actually created with a specific size, the other volumes aren't passed
// any sizes. Blobfs can resize itself on the fvm, and the other two potential volumes are only
// used in specific circumstances and are never formatted. The remaining volume size is just used
// for calculation.
pub const DEFAULT_DATA_VOLUME_SIZE: u64 = 101 * 1024 * 1024;
pub const DEFAULT_REMAINING_VOLUME_SIZE: u64 = 4 * 1024 * 1024;
// For migration tests, we make sure that the default disk size is twice the data volume size to
// allow a second full data partition.
pub const DEFAULT_DISK_SIZE: u64 = DEFAULT_DATA_VOLUME_SIZE + DEFAULT_REMAINING_VOLUME_SIZE;

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

pub const BLOB_CONTENTS: [u8; 1000] = [1; 1000];

const DATA_KEY: Aes256Key = Aes256Key::create([
    0xcf, 0x9e, 0x45, 0x2a, 0x22, 0xa5, 0x70, 0x31, 0x33, 0x3b, 0x4d, 0x6b, 0x6f, 0x78, 0x58, 0x29,
    0x04, 0x79, 0xc7, 0xd6, 0xa9, 0x4b, 0xce, 0x82, 0x04, 0x56, 0x5e, 0x82, 0xfc, 0xe7, 0x37, 0xa8,
]);

const METADATA_KEY: Aes256Key = Aes256Key::create([
    0x0f, 0x4d, 0xca, 0x6b, 0x35, 0x0e, 0x85, 0x6a, 0xb3, 0x8c, 0xdd, 0xe9, 0xda, 0x0e, 0xc8, 0x22,
    0x8e, 0xea, 0xd8, 0x05, 0xc4, 0xc9, 0x0b, 0xa8, 0xd8, 0x85, 0x87, 0x50, 0x75, 0x40, 0x1c, 0x4c,
]);

const FVM_PART_INSTANCE_GUID: [u8; 16] = [3u8; 16];

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
    let wrapping_key_id_0 = [0; 16];
    let mut wrapping_key_id_1 = [0; 16];
    wrapping_key_id_1[0] = 1;
    crypt_management
        .add_wrapping_key(&wrapping_key_id_0, data_key.deref())
        .await
        .unwrap()
        .expect("add_wrapping_key failed");
    crypt_management
        .add_wrapping_key(&wrapping_key_id_1, metadata_key.deref())
        .await
        .unwrap()
        .expect("add_wrapping_key failed");
    crypt_management
        .set_active_key(KeyPurpose::Data, &wrapping_key_id_0)
        .await
        .unwrap()
        .expect("set_active_key failed");
    crypt_management
        .set_active_key(KeyPurpose::Metadata, &wrapping_key_id_1)
        .await
        .unwrap()
        .expect("set_active_key failed");
    realm
}

/// Write a blob to blobfs to ensure that on format, blobfs doesn't get wiped.
pub async fn write_test_blob(
    blob_volume_root: &fio::DirectoryProxy,
    data: &[u8],
    as_delivery_blob: bool,
) -> Hash {
    let hash = fuchsia_merkle::from_slice(data).root();
    let compressed_data = Type1Blob::generate(&data, CompressionMode::Always);
    let (name, data) = if as_delivery_blob {
        (delivery_blob_path(hash), compressed_data.as_slice())
    } else {
        (hash.to_string(), data)
    };

    let (blob, server_end) = create_proxy::<fio::FileMarker>();
    let flags =
        fio::OpenFlags::CREATE | fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE;
    blob_volume_root
        .deprecated_open(
            flags,
            fio::ModeType::empty(),
            &name,
            ServerEnd::new(server_end.into_channel()),
        )
        .expect("open failed");
    let _: Vec<_> = blob.query().await.expect("open file failed");

    blob.resize(data.len() as u64).await.expect("FIDL call failed").expect("truncate failed");
    for chunk in data.chunks(fio::MAX_TRANSFER_SIZE as usize) {
        assert_eq!(
            blob.write(&chunk).await.expect("FIDL call failed").expect("write failed"),
            chunk.len() as u64
        );
    }
    hash
}

/// Write a blob to the fxfs blob volume to ensure that on format, the blob volume does not get
/// wiped.
pub async fn write_test_blob_fxblob(blob_creator: BlobCreatorProxy, data: &[u8]) -> Hash {
    let hash = fuchsia_merkle::from_slice(data).root();
    let compressed_data = Type1Blob::generate(&data, CompressionMode::Always);

    let blob_writer_client_end = blob_creator
        .create(&hash.into(), false)
        .await
        .expect("transport error on create")
        .expect("failed to create blob");

    let writer = blob_writer_client_end.into_proxy();
    let mut blob_writer = BlobWriter::create(writer, compressed_data.len() as u64)
        .await
        .expect("failed to create BlobWriter");
    blob_writer.write(&compressed_data).await.unwrap();
    hash
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

#[derive(Debug, Default)]
pub struct DataSpec {
    pub format: Option<&'static str>,
    pub zxcrypt: bool,
}

#[derive(Debug)]
pub struct VolumesSpec {
    pub fxfs_blob: bool,
    pub create_data_partition: bool,
}

enum FxfsType {
    Fxfs(Box<dyn BlockConnector>),
    FxBlob(ServingMultiVolumeFilesystem, RealmInstance),
}

pub struct DiskBuilder {
    size: u64,
    // Overrides all other options.  The disk will be unformatted.
    uninitialized: bool,
    blob_hash: Option<Hash>,
    data_volume_size: u64,
    data_spec: DataSpec,
    volumes_spec: VolumesSpec,
    // Only used if `format` is Some.
    corrupt_data: bool,
    gpt: bool,
    with_account_and_virtualization: bool,
    // Note: fvm also means fxfs acting as the volume manager when using fxblob.
    format_volume_manager: bool,
    legacy_data_label: bool,
    // Only used if 'fs_switch' set.
    fs_switch: Option<String>,
}

impl DiskBuilder {
    pub fn uninitialized() -> DiskBuilder {
        Self { uninitialized: true, ..Self::new() }
    }

    pub fn new() -> DiskBuilder {
        DiskBuilder {
            size: DEFAULT_DISK_SIZE,
            uninitialized: false,
            blob_hash: None,
            data_volume_size: DEFAULT_DATA_VOLUME_SIZE,
            data_spec: DataSpec { format: None, zxcrypt: false },
            volumes_spec: VolumesSpec { fxfs_blob: false, create_data_partition: true },
            corrupt_data: false,
            gpt: false,
            with_account_and_virtualization: false,
            format_volume_manager: true,
            legacy_data_label: false,
            fs_switch: None,
        }
    }

    pub fn size(&mut self, size: u64) -> &mut Self {
        self.size = size;
        self
    }

    pub fn data_volume_size(&mut self, data_volume_size: u64) -> &mut Self {
        assert_eq!(
            data_volume_size % FVM_SLICE_SIZE,
            0,
            "data_volume_size {} needs to be a multiple of fvm slice size {}",
            data_volume_size,
            FVM_SLICE_SIZE
        );
        self.data_volume_size = data_volume_size;
        // Increase the size of the disk if required. NB: We don't decrease the size of the disk
        // because some tests set a lower initial size and expect to be able to resize to a larger
        // one.
        self.size = self.size.max(self.data_volume_size + DEFAULT_REMAINING_VOLUME_SIZE);
        self
    }

    pub fn format_volumes(&mut self, volumes_spec: VolumesSpec) -> &mut Self {
        self.volumes_spec = volumes_spec;
        self
    }

    pub fn format_data(&mut self, data_spec: DataSpec) -> &mut Self {
        log::info!(data_spec:?; "formatting data volume");
        if !self.volumes_spec.fxfs_blob {
            assert!(self.format_volume_manager);
        } else {
            if let Some(format) = data_spec.format {
                assert_eq!(format, "fxfs");
            }
        }
        self.data_spec = data_spec;
        self
    }

    pub fn set_fs_switch(&mut self, content: &str) -> &mut Self {
        self.fs_switch = Some(content.to_string());
        self
    }

    pub fn corrupt_data(&mut self) -> &mut Self {
        self.corrupt_data = true;
        self
    }

    pub fn with_gpt(&mut self) -> &mut Self {
        self.gpt = true;
        self
    }

    pub fn with_account_and_virtualization(&mut self) -> &mut Self {
        self.with_account_and_virtualization = true;
        self
    }

    pub fn with_unformatted_volume_manager(&mut self) -> &mut Self {
        assert!(self.data_spec.format.is_none());
        self.format_volume_manager = false;
        self
    }

    pub fn with_legacy_data_label(&mut self) -> &mut Self {
        self.legacy_data_label = true;
        self
    }

    pub async fn build(mut self) -> zx::Vmo {
        let vmo = zx::Vmo::create(self.size).unwrap();

        if self.uninitialized {
            return vmo;
        }

        let server = Arc::new(FakeServer::from_vmo(
            TEST_DISK_BLOCK_SIZE,
            vmo.duplicate_handle(zx::Rights::SAME_RIGHTS).unwrap(),
        ));

        if self.gpt {
            // Format the disk with gpt, with a single empty partition named "fvm".
            let client = Arc::new(RemoteBlockClient::new(server.block_proxy()).await.unwrap());
            let _ = gpt::Gpt::format(
                client,
                vec![gpt::PartitionInfo {
                    label: "fvm".to_string(),
                    type_guid: gpt::Guid::from_bytes(FVM_TYPE_GUID),
                    instance_guid: gpt::Guid::from_bytes(FVM_PART_INSTANCE_GUID),
                    start_block: 64,
                    num_blocks: self.size / TEST_DISK_BLOCK_SIZE as u64 - 128,
                    flags: 0,
                }],
            )
            .await
            .expect("gpt format failed");
        }

        if !self.format_volume_manager {
            return vmo;
        }

        let mut gpt = None;
        let connector: Box<dyn BlockConnector> = if self.gpt {
            // Format the volume manager in the gpt partition named "fvm".
            let partitions_dir = vfs::directory::immutable::simple();
            let manager =
                GptManager::new(server.block_proxy(), partitions_dir.clone()).await.unwrap();
            let scope = ExecutionScope::new();
            let (dir, server_end) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>();
            let flags = fio::Flags::PROTOCOL_DIRECTORY | fio::PERM_READABLE | fio::PERM_WRITABLE;
            ObjectRequest::new(flags, &fio::Options::default(), server_end.into_channel())
                .handle(|request| partitions_dir.open3(scope.clone(), Path::dot(), flags, request));
            gpt = Some(manager);
            Box::new(DirBasedBlockConnector::new(dir, String::from("part-000/volume")))
        } else {
            // Format the volume manager onto the disk directly.
            Box::new(move || Ok(server.volume_proxy().into_client_end().unwrap()))
        };

        if self.volumes_spec.fxfs_blob {
            self.build_fxfs_as_volume_manager(connector).await;
        } else {
            self.build_fvm_as_volume_manager(connector).await;
        }
        if let Some(gpt) = gpt {
            gpt.shutdown().await;
        }
        vmo
    }

    async fn build_fxfs_as_volume_manager(&mut self, connector: Box<dyn BlockConnector>) {
        let crypt_realm = create_hermetic_crypt_service(DATA_KEY, METADATA_KEY).await;
        let mut fxfs = Filesystem::from_boxed_config(connector, Box::new(Fxfs::default()));
        // Wipes the device
        fxfs.format().await.expect("format failed");
        let mut fs = fxfs.serve_multi_volume().await.expect("serve_multi_volume failed");
        let blob_volume = fs
            .create_volume(
                "blob",
                CreateOptions::default(),
                MountOptions { as_blob: Some(true), ..MountOptions::default() },
            )
            .await
            .expect("failed to create blob volume");
        let blob_creator = connect_to_protocol_at_dir_svc::<fidl_fuchsia_fxfs::BlobCreatorMarker>(
            blob_volume.exposed_dir(),
        )
        .expect("failed to connect to the Blob service");
        self.blob_hash = Some(write_test_blob_fxblob(blob_creator, &BLOB_CONTENTS).await);

        if self.data_spec.format.is_some() {
            self.init_data_fxfs(FxfsType::FxBlob(fs, crypt_realm)).await;
        } else {
            fs.shutdown().await.expect("shutdown failed");
        }
    }

    async fn build_fvm_as_volume_manager(&mut self, connector: Box<dyn BlockConnector>) {
        let block_device = connector.connect_block().unwrap().into_proxy();
        fasync::unblock(move || format_for_fvm(&block_device, FVM_SLICE_SIZE as usize))
            .await
            .unwrap();
        let mut fvm_fs = Filesystem::from_boxed_config(connector, Box::new(Fvm::dynamic_child()));
        let mut fvm = fvm_fs.serve_multi_volume().await.unwrap();

        {
            let blob_volume = fvm
                .create_volume(
                    "blobfs",
                    CreateOptions {
                        type_guid: Some(BLOBFS_TYPE_GUID),
                        guid: Some(Uuid::new_v4().into_bytes()),
                        ..Default::default()
                    },
                    MountOptions {
                        uri: Some(String::from("#meta/blobfs.cm")),
                        ..Default::default()
                    },
                )
                .await
                .expect("failed to make fvm blobfs volume");
            self.blob_hash = Some(write_test_blob(blob_volume.root(), &BLOB_CONTENTS, false).await);
        }
        fvm.shutdown_volume("blobfs").await.unwrap();

        if self.volumes_spec.create_data_partition {
            let data_label = if self.legacy_data_label { "minfs" } else { "data" };

            let _crypt_service;
            let crypt = if self.data_spec.format != Some("fxfs") && self.data_spec.zxcrypt {
                let (crypt, stream) = fidl::endpoints::create_request_stream();
                _crypt_service = fasync::Task::spawn(zxcrypt_crypt::run_crypt_service(
                    crypt_policy::Policy::Null,
                    stream,
                ));
                Some(crypt)
            } else {
                None
            };
            let uri = match (&self.data_spec.format, self.corrupt_data) {
                (None, _) => None,
                (_, true) => None,
                (Some("fxfs"), false) => None,
                (Some("minfs"), false) => Some(String::from("#meta/minfs.cm")),
                (Some("f2fs"), false) => Some(String::from("#meta/f2fs.cm")),
                (Some(format), _) => panic!("unsupported data volume format '{}'", format),
            };

            let data_volume = fvm
                .create_volume(
                    data_label,
                    CreateOptions {
                        initial_size: Some(self.data_volume_size),
                        type_guid: Some(DATA_TYPE_GUID),
                        guid: Some(Uuid::new_v4().into_bytes()),
                        ..Default::default()
                    },
                    MountOptions { crypt, uri, ..Default::default() },
                )
                .await
                .unwrap();

            if self.corrupt_data {
                let volume_proxy = connect_to_protocol_at_dir_svc::<
                    fidl_fuchsia_hardware_block_volume::VolumeMarker,
                >(data_volume.exposed_dir())
                .unwrap();
                match self.data_spec.format {
                    Some("fxfs") => self.write_magic(volume_proxy, FXFS_MAGIC, 0).await,
                    Some("minfs") => self.write_magic(volume_proxy, MINFS_MAGIC, 0).await,
                    Some("f2fs") => self.write_magic(volume_proxy, F2FS_MAGIC, 1024).await,
                    _ => (),
                }
            } else if self.data_spec.format == Some("fxfs") {
                let dir = fuchsia_fs::directory::clone(data_volume.exposed_dir()).unwrap();
                self.init_data_fxfs(FxfsType::Fxfs(Box::new(DirBasedBlockConnector::new(
                    dir,
                    String::from("svc/fuchsia.hardware.block.volume.Volume"),
                ))))
                .await
            } else if self.data_spec.format.is_some() {
                self.write_test_data(data_volume.root()).await;
                fvm.shutdown_volume(data_label).await.unwrap();
            }
        }

        if self.with_account_and_virtualization {
            fvm.create_volume(
                "account",
                CreateOptions {
                    type_guid: Some(DATA_TYPE_GUID),
                    guid: Some(Uuid::new_v4().into_bytes()),
                    ..Default::default()
                },
                MountOptions::default(),
            )
            .await
            .expect("failed to make fvm account volume");
            fvm.create_volume(
                "virtualization",
                CreateOptions {
                    type_guid: Some(DATA_TYPE_GUID),
                    guid: Some(Uuid::new_v4().into_bytes()),
                    ..Default::default()
                },
                MountOptions::default(),
            )
            .await
            .expect("failed to make fvm virtualization volume");
        }

        fvm.shutdown().await.expect("fvm shutdown failed");
    }

    async fn init_data_fxfs(&self, fxfs: FxfsType) {
        let mut fxblob = false;
        let (mut fs, crypt_realm) = match fxfs {
            FxfsType::Fxfs(connector) => {
                let crypt_realm = create_hermetic_crypt_service(DATA_KEY, METADATA_KEY).await;
                let mut fxfs =
                    Filesystem::from_boxed_config(connector, Box::new(Fxfs::dynamic_child()));
                fxfs.format().await.expect("format failed");
                (fxfs.serve_multi_volume().await.expect("serve_multi_volume failed"), crypt_realm)
            }
            FxfsType::FxBlob(fs, crypt_realm) => {
                fxblob = true;
                (fs, crypt_realm)
            }
        };

        let vol = {
            let vol = fs
                .create_volume("unencrypted", CreateOptions::default(), MountOptions::default())
                .await
                .expect("create_volume failed");
            let keys_dir = fuchsia_fs::directory::create_directory(
                vol.root(),
                "keys",
                fio::PERM_READABLE | fio::PERM_WRITABLE,
            )
            .await
            .unwrap();
            let keys_file = fuchsia_fs::directory::open_file(
                &keys_dir,
                "fxfs-data",
                fio::Flags::FLAG_MAYBE_CREATE
                    | fio::Flags::PROTOCOL_FILE
                    | fio::PERM_READABLE
                    | fio::PERM_WRITABLE,
            )
            .await
            .unwrap();
            let mut key_bag = KEY_BAG_CONTENTS.as_bytes();
            if self.corrupt_data && fxblob {
                key_bag = &BLOB_CONTENTS;
            }
            fuchsia_fs::file::write(&keys_file, key_bag).await.unwrap();
            fuchsia_fs::file::close(keys_file).await.unwrap();
            fuchsia_fs::directory::close(keys_dir).await.unwrap();

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
            fs.create_volume(
                "data",
                CreateOptions::default(),
                MountOptions { crypt: crypt_service, ..MountOptions::default() },
            )
            .await
            .expect("create_volume failed")
        };
        self.write_test_data(&vol.root()).await;
        fs.shutdown().await.expect("shutdown failed");
    }

    /// Create a small set of known files to test for presence. The test tree is
    ///  root
    ///   |- .testdata (file, empty)
    ///   |- ssh (directory, non-empty)
    ///   |   |- authorized_keys (file, non-empty)
    ///   |   |- config (directory, empty)
    ///   |- problems (directory, empty (no problems))
    async fn write_test_data(&self, root: &fio::DirectoryProxy) {
        fuchsia_fs::directory::open_file(
            root,
            ".testdata",
            fio::Flags::FLAG_MAYBE_CREATE | fio::PERM_READABLE,
        )
        .await
        .unwrap();

        let ssh_dir = fuchsia_fs::directory::create_directory(
            root,
            "ssh",
            fio::PERM_READABLE | fio::PERM_WRITABLE,
        )
        .await
        .unwrap();
        let authorized_keys = fuchsia_fs::directory::open_file(
            &ssh_dir,
            "authorized_keys",
            fio::Flags::FLAG_MAYBE_CREATE | fio::PERM_READABLE | fio::PERM_WRITABLE,
        )
        .await
        .unwrap();
        fuchsia_fs::file::write(&authorized_keys, "public key!").await.unwrap();
        fuchsia_fs::directory::create_directory(&ssh_dir, "config", fio::PERM_READABLE)
            .await
            .unwrap();

        fuchsia_fs::directory::create_directory(&root, "problems", fio::PERM_READABLE)
            .await
            .unwrap();

        if let Some(content) = &self.fs_switch {
            let fs_switch = fuchsia_fs::directory::open_file(
                &root,
                "fs_switch",
                fio::Flags::FLAG_MAYBE_CREATE | fio::PERM_READABLE | fio::PERM_WRITABLE,
            )
            .await
            .unwrap();
            fuchsia_fs::file::write(&fs_switch, content).await.unwrap();
        }
    }

    async fn write_magic<const N: usize>(
        &self,
        volume_proxy: fidl_fuchsia_hardware_block_volume::VolumeProxy,
        value: [u8; N],
        offset: u64,
    ) {
        let client = block_client::RemoteBlockClient::new(volume_proxy)
            .await
            .expect("Failed to create client");
        let block_size = client.block_size() as usize;
        assert!(value.len() <= block_size);
        let mut data = vec![0xffu8; block_size];
        data[..value.len()].copy_from_slice(&value);
        let buffer = block_client::BufferSlice::Memory(&data[..]);
        client.write_at(buffer, offset).await.expect("write failed");
    }

    /// Create a vmo artifact with the format of a compressed zbi boot item containing this
    /// filesystem.
    pub(crate) async fn build_as_zbi_ramdisk(self) -> zx::Vmo {
        /// The following types and constants are defined in
        /// sdk/lib/zbi-format/include/lib/zbi-format/zbi.h.
        const ZBI_TYPE_STORAGE_RAMDISK: u32 = 0x4b534452;
        const ZBI_FLAGS_VERSION: u32 = 0x00010000;
        const ZBI_ITEM_MAGIC: u32 = 0xb5781729;
        const ZBI_FLAGS_STORAGE_COMPRESSED: u32 = 0x00000001;

        #[repr(C)]
        #[derive(IntoBytes, Immutable)]
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
