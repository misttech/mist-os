// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use assert_matches::assert_matches;
use diagnostics_assertions::assert_data_tree;
use diagnostics_reader::{ArchiveReader, Inspect};
use fidl::endpoints::{create_proxy, ServiceMarker as _};
use fidl_fuchsia_fxfs::BlobReaderMarker;
use fuchsia_component::client::connect_to_protocol_at_dir_root;
use fuchsia_component_test::{Capability, ChildOptions, RealmBuilder, RealmInstance, Ref, Route};
use futures::channel::mpsc::{self};
use futures::{FutureExt as _, StreamExt as _};
use ramdevice_client::{RamdiskClient, RamdiskClientBuilder};
use std::pin::pin;
use std::time::Duration;
use {
    fidl_fuchsia_boot as fboot, fidl_fuchsia_feedback as ffeedback,
    fidl_fuchsia_hardware_block_volume as fvolume, fidl_fuchsia_hardware_ramdisk as framdisk,
    fidl_fuchsia_io as fio, fidl_fuchsia_logger as flogger, fidl_fuchsia_process as fprocess,
    fuchsia_async as fasync,
};

pub mod disk_builder;
pub mod fshost_builder;
mod mocks;

pub use disk_builder::{write_test_blob, write_test_blob_fxblob};

pub const VFS_TYPE_BLOBFS: u32 = 0x9e694d21;
pub const VFS_TYPE_MINFS: u32 = 0x6e694d21;
pub const VFS_TYPE_MEMFS: u32 = 0x3e694d21;
pub const VFS_TYPE_FXFS: u32 = 0x73667866;
pub const VFS_TYPE_F2FS: u32 = 0xfe694d21;
pub const BLOBFS_MAX_BYTES: u64 = 8765432;
// DATA_MAX_BYTES must be greater than DEFAULT_F2FS_MIN_BYTES
// (defined in device/constants.rs) to ensure that when f2fs is
// the data filesystem format, we don't run out of space
pub const DATA_MAX_BYTES: u64 = 109876543;

pub struct TestFixtureBuilder {
    netboot: bool,
    no_fuchsia_boot: bool,
    disk: Option<disk_builder::Disk>,
    extra_disks: Vec<disk_builder::Disk>,
    fshost: fshost_builder::FshostBuilder,
    zbi_ramdisk: Option<disk_builder::DiskBuilder>,
    storage_host: bool,
}

impl TestFixtureBuilder {
    pub fn new(fshost_component_name: &'static str, storage_host: bool) -> Self {
        Self {
            netboot: false,
            no_fuchsia_boot: false,
            disk: None,
            extra_disks: vec![],
            fshost: fshost_builder::FshostBuilder::new(fshost_component_name),
            zbi_ramdisk: None,
            storage_host,
        }
    }

    pub fn fshost(&mut self) -> &mut fshost_builder::FshostBuilder {
        &mut self.fshost
    }

    pub fn with_disk(&mut self) -> &mut disk_builder::DiskBuilder {
        self.disk = Some(disk_builder::Disk::Builder(disk_builder::DiskBuilder::new()));
        self.disk.as_mut().unwrap().builder()
    }

    pub fn with_extra_disk(&mut self) -> &mut disk_builder::DiskBuilder {
        self.extra_disks.push(disk_builder::Disk::Builder(disk_builder::DiskBuilder::new()));
        self.extra_disks.last_mut().unwrap().builder()
    }

    pub fn with_uninitialized_disk(mut self) -> Self {
        self.disk = Some(disk_builder::Disk::Builder(disk_builder::DiskBuilder::uninitialized()));
        self
    }

    pub fn with_disk_from_vmo(mut self, vmo: zx::Vmo) -> Self {
        self.disk = Some(disk_builder::Disk::Prebuilt(vmo));
        self
    }

    pub fn with_zbi_ramdisk(&mut self) -> &mut disk_builder::DiskBuilder {
        self.zbi_ramdisk = Some(disk_builder::DiskBuilder::new());
        self.zbi_ramdisk.as_mut().unwrap()
    }

    pub fn netboot(mut self) -> Self {
        self.netboot = true;
        self
    }

    pub fn no_fuchsia_boot(mut self) -> Self {
        self.no_fuchsia_boot = true;
        self
    }

    pub async fn build(self) -> TestFixture {
        let builder = RealmBuilder::new().await.unwrap();
        let fshost = self.fshost.build(&builder).await;

        let maybe_zbi_vmo = match self.zbi_ramdisk {
            Some(disk_builder) => Some(disk_builder.build_as_zbi_ramdisk().await),
            None => None,
        };
        let (tx, crash_reports) = mpsc::channel(32);
        let mocks = mocks::new_mocks(self.netboot, maybe_zbi_vmo, tx).await;

        let mocks = builder
            .add_local_child("mocks", move |h| mocks(h).boxed(), ChildOptions::new())
            .await
            .unwrap();
        builder
            .add_route(
                Route::new()
                    .capability(Capability::protocol::<ffeedback::CrashReporterMarker>())
                    .from(&mocks)
                    .to(&fshost),
            )
            .await
            .unwrap();
        if !self.no_fuchsia_boot {
            builder
                .add_route(
                    Route::new()
                        .capability(Capability::protocol::<fboot::ArgumentsMarker>())
                        .capability(Capability::protocol::<fboot::ItemsMarker>())
                        .from(&mocks)
                        .to(&fshost),
                )
                .await
                .unwrap();
        }

        let drivers = builder
            .add_child(
                "storage_driver_test_realm",
                "#meta/storage_driver_test_realm.cm",
                ChildOptions::new().eager(),
            )
            .await
            .unwrap();
        builder
            .add_route(
                Route::new()
                    .capability(Capability::directory("dev-topological").rights(fio::R_STAR_DIR))
                    .from(&drivers)
                    .to(Ref::parent())
                    .to(&fshost),
            )
            .await
            .unwrap();
        builder
            .add_route(
                Route::new()
                    .capability(
                        Capability::directory("dev-class")
                            .rights(fio::R_STAR_DIR)
                            .subdir("block")
                            .as_("dev-class-block"),
                    )
                    .from(&drivers)
                    .to(Ref::parent()),
            )
            .await
            .unwrap();
        builder
            .add_route(
                Route::new()
                    .capability(Capability::protocol::<fprocess::LauncherMarker>())
                    .capability(Capability::protocol::<flogger::LogSinkMarker>())
                    .from(Ref::parent())
                    .to(&drivers),
            )
            .await
            .unwrap();
        builder
            .add_route(
                Route::new()
                    .capability(Capability::dictionary("diagnostics"))
                    .from(Ref::parent())
                    .to(&drivers)
                    .to(&fshost),
            )
            .await
            .unwrap();

        builder
            .add_route(
                Route::new()
                    .capability(Capability::service::<framdisk::ServiceMarker>())
                    .from(&drivers)
                    .to(Ref::parent())
                    .to(&fshost),
            )
            .await
            .unwrap();
        builder
            .add_route(
                Route::new()
                    .capability(Capability::service::<fvolume::ServiceMarker>())
                    .from(&drivers)
                    .to(&fshost),
            )
            .await
            .unwrap();

        let mut fixture = TestFixture {
            realm: builder.build().await.unwrap(),
            ramdisks: Vec::new(),
            ramdisk_vmo: None,
            crash_reports,
            torn_down: TornDown(false),
            ignore_crash_reports: false,
            storage_host: self.storage_host,
        };

        tracing::info!(
            realm_name = ?fixture.realm.root.child_name(),
            "built new test realm",
        );

        if let Some(disk) = self.disk {
            let vmo = disk.get_vmo().await;
            let vmo_clone =
                vmo.create_child(zx::VmoChildOptions::SLICE, 0, vmo.get_size().unwrap()).unwrap();

            fixture.add_ramdisk(vmo).await;
            fixture.ramdisk_vmo = Some(vmo_clone);
        }
        for disk in self.extra_disks.into_iter() {
            let vmo = disk.get_vmo().await;
            fixture.add_ramdisk(vmo).await;
        }

        fixture
    }
}

/// Create a separate struct that does the drop-assert because fixture.tear_down can't call
/// realm.destroy if it has the drop impl itself.
struct TornDown(bool);

impl Drop for TornDown {
    fn drop(&mut self) {
        // Because tear_down is async, it needs to be called by the test in an async context. It
        // checks some properties so for correctness it must be called.
        assert!(self.0, "fixture.tear_down() must be called");
    }
}

pub struct TestFixture {
    pub realm: RealmInstance,
    pub ramdisks: Vec<RamdiskClient>,
    pub ramdisk_vmo: Option<zx::Vmo>,
    pub crash_reports: mpsc::Receiver<ffeedback::CrashReport>,
    torn_down: TornDown,
    ignore_crash_reports: bool,
    storage_host: bool,
}

impl TestFixture {
    pub async fn tear_down(mut self) {
        tracing::info!(realm_name = ?self.realm.root.child_name(), "tearing down");
        // Check the crash reports before destroying the realm because tearing down the realm can
        // cause mounting errors that trigger a crash report.
        if !self.ignore_crash_reports {
            assert_matches!(self.crash_reports.try_next(), Ok(None) | Err(_));
        }
        self.realm.destroy().await.unwrap();
        self.torn_down.0 = true;
    }

    pub async fn into_vmo(mut self) -> Option<zx::Vmo> {
        let vmo = self.ramdisk_vmo.take();
        self.tear_down().await;
        vmo
    }

    pub async fn set_ramdisk_vmo(&mut self, vmo: zx::Vmo) {
        let vmo_clone =
            vmo.create_child(zx::VmoChildOptions::SLICE, 0, vmo.get_size().unwrap()).unwrap();
        self.add_ramdisk(vmo).await;
        self.ramdisk_vmo = Some(vmo_clone);
    }

    pub fn dir(&self, dir: &str, flags: fio::Flags) -> fio::DirectoryProxy {
        let (dev, server) = create_proxy::<fio::DirectoryMarker>().expect("create_proxy failed");
        let flags = flags | fio::Flags::PROTOCOL_DIRECTORY;
        self.realm
            .root
            .get_exposed_dir()
            .open3(dir, flags, &fio::Options::default(), server.into_channel())
            .expect("open failed");
        dev
    }

    pub async fn check_fs_type(&self, dir: &str, fs_type: u32) {
        let (status, info) =
            self.dir(dir, fio::Flags::empty()).query_filesystem().await.expect("query failed");
        assert_eq!(zx::Status::from_raw(status), zx::Status::OK);
        assert!(info.is_some());
        let info_type = info.unwrap().fs_type;
        assert_eq!(info_type, fs_type, "{:#08x} != {:#08x}", info_type, fs_type);
    }

    pub async fn check_test_blob(&self, use_fxblob: bool) {
        let expected_blob_hash = fuchsia_merkle::from_slice(&disk_builder::BLOB_CONTENTS).root();
        if use_fxblob {
            let reader = connect_to_protocol_at_dir_root::<BlobReaderMarker>(
                self.realm.root.get_exposed_dir(),
            )
            .expect("failed to connect to the BlobReader");
            let _vmo = reader.get_vmo(&expected_blob_hash.into()).await.unwrap().unwrap();
        } else {
            let (blob, server_end) =
                create_proxy::<fio::FileMarker>().expect("create_proxy failed");
            let path = &format!("{}", expected_blob_hash);
            self.dir("blob", fio::PERM_READABLE)
                .open3(
                    path,
                    fio::PERM_READABLE,
                    &fio::Options::default(),
                    server_end.into_channel(),
                )
                .expect("open failed");
            blob.query().await.expect("open file failed");
        }
    }

    /// Check for the existence of a well-known set of test files in the data volume. These files
    /// are placed by the disk builder if it formats the filesystem beforehand.
    pub async fn check_test_data_file(&self) {
        let (file, server) = create_proxy::<fio::NodeMarker>().unwrap();
        self.dir("data", fio::PERM_READABLE)
            .open3(".testdata", fio::PERM_READABLE, &fio::Options::default(), server.into_channel())
            .expect("open failed");
        file.get_attr().await.expect("get_attr failed");

        let data = self.dir("data", fio::PERM_READABLE);
        fuchsia_fs::directory::open_file(&data, ".testdata", fio::PERM_READABLE).await.unwrap();

        fuchsia_fs::directory::open_directory(&data, "ssh", fio::PERM_READABLE).await.unwrap();
        fuchsia_fs::directory::open_directory(&data, "ssh/config", fio::PERM_READABLE)
            .await
            .unwrap();
        fuchsia_fs::directory::open_directory(&data, "problems", fio::PERM_READABLE).await.unwrap();

        let authorized_keys =
            fuchsia_fs::directory::open_file(&data, "ssh/authorized_keys", fio::PERM_READABLE)
                .await
                .unwrap();
        assert_eq!(
            &fuchsia_fs::file::read_to_string(&authorized_keys).await.unwrap(),
            "public key!"
        );
    }

    /// Checks for the absence of the .testdata marker file, indicating the data filesystem was
    /// reformatted.
    pub async fn check_test_data_file_absent(&self) {
        let (file, server) = create_proxy::<fio::NodeMarker>().unwrap();
        self.dir("data", fio::PERM_READABLE)
            .open3(".testdata", fio::PERM_READABLE, &fio::Options::default(), server.into_channel())
            .expect("open failed");
        file.get_attr().await.expect_err(".testdata should be absent");
    }

    pub fn ramdisk_vmo(&self) -> Option<&zx::Vmo> {
        self.ramdisk_vmo.as_ref()
    }

    pub async fn add_ramdisk(&mut self, vmo: zx::Vmo) {
        let mut ramdisk = pin!(if self.storage_host {
            RamdiskClientBuilder::new_with_vmo(vmo, Some(512)).use_v2().publish().ramdisk_service(
                self.dir(framdisk::ServiceMarker::SERVICE_NAME, fio::Flags::empty()),
            )
        } else {
            RamdiskClientBuilder::new_with_vmo(vmo, Some(512))
                .dev_root(self.dir("dev-topological", fio::Flags::empty()))
        }
        .build()
        .fuse());

        let ramdisk = futures::select_biased!(
            res = ramdisk => res,
            _ = fasync::Timer::new(Duration::from_secs(120))
                .fuse() => panic!("Timed out waiting for RamdiskClient"),
        )
        .unwrap();
        self.ramdisks.push(ramdisk);
    }

    /// This must be called if any crash reports are expected, since spurious reports will cause a
    /// failure in TestFixture::tear_down.
    pub async fn wait_for_crash_reports(
        &mut self,
        count: usize,
        expected_program: &'_ str,
        expected_signature: &'_ str,
    ) {
        tracing::info!("Waiting for {count} crash reports");
        for _ in 0..count {
            let report = self.crash_reports.next().await.expect("Sender closed");
            assert_eq!(report.program_name.as_deref(), Some(expected_program));
            assert_eq!(report.crash_signature.as_deref(), Some(expected_signature));
        }
        if count > 0 {
            let selector =
                format!("realm_builder\\:{}/test-fshost:root", self.realm.root.child_name());
            tracing::info!("Checking inspect for corruption event, selector={selector}");
            let tree = ArchiveReader::new()
                .add_selector(selector)
                .snapshot::<Inspect>()
                .await
                .unwrap()
                .into_iter()
                .next()
                .and_then(|result| result.payload)
                .expect("expected one inspect hierarchy");

            let format = || expected_program.to_string();

            assert_data_tree!(tree, root: contains {
                corruption_events: contains {
                    format() => 1u64,
                }
            });
        }
    }

    /// Ignore crash reports when tearing down the fixture.
    pub fn ignore_crash_reports(&mut self) {
        self.ignore_crash_reports = true;
    }
}
