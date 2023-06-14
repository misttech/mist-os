// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::Error,
    fidl::endpoints::ServerEnd,
    fidl_fuchsia_driver_test as fdt, fidl_fuchsia_io as fio, fidl_fuchsia_pkg as fpkg,
    fuchsia_component_test::{
        Capability, ChildOptions, LocalComponentHandles, RealmBuilder, Ref, Route,
    },
    fuchsia_driver_test::{DriverTestRealmBuilder, DriverTestRealmInstance},
    futures::{FutureExt as _, TryStreamExt as _},
    std::{collections::HashMap, sync::Arc},
    vfs::{
        directory::entry::{DirectoryEntry, EntryInfo},
        execution_scope::ExecutionScope,
        file::vmo::VmoFile,
    },
};

type Directory = Arc<
    vfs::directory::simple::Simple<vfs::directory::immutable::connection::io1::ImmutableConnection>,
>;

struct FakePackageVariant {
    dir: Directory,
    meta_file: Arc<VmoFile>,
}

impl FakePackageVariant {
    /// Creates a new struct that acts a directory serving `dir`. If the "meta"
    /// node within the directory is read as a file then `meta_file` is served
    /// as the contents of the file.
    pub fn new(dir: Directory, meta_file: Arc<VmoFile>) -> Self {
        Self { dir, meta_file }
    }
}

impl DirectoryEntry for FakePackageVariant {
    fn open(
        self: Arc<Self>,
        scope: ExecutionScope,
        flags: fio::OpenFlags,
        path: vfs::path::Path,
        server_end: ServerEnd<fio::NodeMarker>,
    ) {
        fn open_as_file(flags: fio::OpenFlags) -> bool {
            !flags.intersects(fio::OpenFlags::DIRECTORY | fio::OpenFlags::NODE_REFERENCE)
        }

        // vfs::path::Path::as_str() is an object relative path expression [1],
        // except that it may:
        //   1. have a trailing "/"
        //   2. be exactly "."
        //   3. be longer than 4,095 bytes
        // The .is_empty() check above rules out "." and the following line
        // removes the possible trailing "/".
        // [1] https://fuchsia.dev/fuchsia-src/concepts/process/namespaces?hl=en#object_relative_path_expressions
        let canonical_path = path.as_ref().strip_suffix("/").unwrap_or_else(|| path.as_ref());

        if canonical_path == "meta" && open_as_file(flags) {
            let meta_file = Arc::clone(&self.meta_file);
            meta_file.open(scope, flags, vfs::path::Path::dot(), server_end);
        } else {
            let dir = Arc::clone(&self.dir);
            dir.open(scope, flags, path, server_end);
        }
    }

    fn entry_info(&self) -> EntryInfo {
        EntryInfo::new(fio::INO_UNKNOWN, fio::DirentType::Directory)
    }
}

struct FakeBasePackageResolver {
    base_packages: HashMap<String, Arc<dyn DirectoryEntry>>,
}

impl FakeBasePackageResolver {
    pub fn new(base_packages: HashMap<String, Arc<dyn DirectoryEntry>>) -> Self {
        Self { base_packages }
    }

    pub async fn handle_request_stream(
        self: Arc<Self>,
        mut stream: fpkg::PackageResolverRequestStream,
    ) {
        while let Some(req) =
            stream.try_next().await.expect("read fuchsia.pkg/PackageResolver request stream")
        {
            match req {
                fpkg::PackageResolverRequest::Resolve { package_url, dir, responder } => {
                    let Some(pkg) = self.base_packages.get(&package_url) else {
                        panic!("FakeBasePackageResolver resolve unknown package {package_url}");
                    };
                    let () = pkg.clone().open(
                        ExecutionScope::new(),
                        fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_EXECUTABLE,
                        vfs::path::Path::dot(),
                        dir.into_channel().into(),
                    );
                    let () = responder
                        .send(Ok(&fpkg::ResolutionContext { bytes: vec![] }))
                        .expect("send resolve response");
                }
                req => panic!("unexpected fuchsia.pkg/PackageResolver request {req:?}"),
            }
        }
    }
}

async fn serve_fake_filesystem(
    base_manifest: Arc<VmoFile>,
    base_packages: HashMap<String, Arc<dyn DirectoryEntry>>,
    handles: LocalComponentHandles,
) -> Result<(), anyhow::Error> {
    let fs_scope = vfs::execution_scope::ExecutionScope::new();
    let root: Directory = vfs::pseudo_directory! {
        "svc" => vfs::pseudo_directory! {
            "fuchsia.pkg.PackageResolver-base" => vfs::service::host(move |stream| {
                let base_packages = base_packages.clone();
                Arc::new(FakeBasePackageResolver::new(base_packages)).handle_request_stream(stream)
            }
            ),
        },
        "boot" => vfs::pseudo_directory! {
            "meta" => vfs::pseudo_directory! {},
            "config" => vfs::pseudo_directory! {
              "driver_index" => vfs::pseudo_directory! {
                "base_driver_manifest" => base_manifest
              }
            },
        },
    };
    root.open(
        fs_scope.clone(),
        fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_EXECUTABLE,
        vfs::path::Path::dot(),
        ServerEnd::new(handles.outgoing_dir.into_channel()),
    );
    fs_scope.wait().await;
    Ok::<(), anyhow::Error>(())
}

async fn create_realm(
    base_manifest: Arc<VmoFile>,
    base_packages: HashMap<String, Arc<dyn DirectoryEntry>>,
) -> Result<fuchsia_component_test::RealmInstance, Error> {
    let builder = RealmBuilder::new().await?;

    let fake_filesystem = builder
        .add_local_child(
            "fake_filesystem",
            move |h: LocalComponentHandles| {
                serve_fake_filesystem(base_manifest.clone(), base_packages.clone(), h).boxed()
            },
            ChildOptions::new().eager(),
        )
        .await
        .expect("mock component added");

    let driver_manager = builder
        .add_child(
            "driver_manager",
            "fuchsia-pkg://fuchsia.com/ddk-firmware-test#meta/driver-manager-realm.cm",
            ChildOptions::new(),
        )
        .await?;

    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol_by_name("fuchsia.pkg.PackageResolver-base"))
                .capability(Capability::directory("boot").path("/boot").rights(fio::R_STAR_DIR))
                .from(&fake_filesystem)
                .to(&driver_manager),
        )
        .await?;
    builder
        .add_route(
            Route::new()
                .capability(Capability::directory("dev-topological"))
                .capability(Capability::protocol_by_name("fuchsia.device.manager.Administrator"))
                .capability(Capability::protocol_by_name("fuchsia.driver.test.Realm"))
                .from(&driver_manager)
                .to(Ref::parent()),
        )
        .await?;
    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol_by_name("fuchsia.logger.LogSink"))
                .capability(Capability::protocol_by_name("fuchsia.process.Launcher"))
                .from(Ref::parent())
                .to(&driver_manager),
        )
        .await?;
    let realm = builder.build().await?;
    realm.driver_test_realm_start(fdt::RealmArgs::default()).await?;
    Ok(realm)
}

// TODO(fxbug.dev/116950): Re-enable after merging test into driver test realm proper.
#[ignore]
#[fuchsia::test]
async fn load_package_firmware_test() -> Result<(), Error> {
    let firmware_file = vfs::file::vmo::read_only(b"this is some firmware\n");
    let driver_dir = vfs::remote::remote_dir(fuchsia_fs::directory::open_in_namespace(
        "/pkg/driver",
        fuchsia_fs::OpenFlags::RIGHT_READABLE | fuchsia_fs::OpenFlags::RIGHT_EXECUTABLE,
    )?);
    let meta_dir = vfs::remote::remote_dir(fuchsia_fs::directory::open_in_namespace(
        "/pkg/meta",
        fuchsia_fs::OpenFlags::RIGHT_READABLE,
    )?);
    let bind_dir = vfs::remote::remote_dir(fuchsia_fs::directory::open_in_namespace(
        "/pkg/bind",
        fuchsia_fs::OpenFlags::RIGHT_READABLE,
    )?);
    let base_manifest = vfs::file::vmo::read_only(
        r#"[{"driver_url": "fuchsia-pkg://fuchsia.com/my-package#meta/ddk-firmware-test-driver.cm"}]"#,
    );
    let my_package = FakePackageVariant::new(
        vfs::pseudo_directory! {
            "driver" => driver_dir,
            "meta" => meta_dir,
            "bind" => bind_dir,
                "lib" => vfs::pseudo_directory! {
                    "firmware" => vfs::pseudo_directory! {
                        "package-firmware" => firmware_file,
                    },
                },
        },
        // Hash is arbitrary and is not read in tests, however, some value needs
        // to be provided, otherwise, the driver will fail to load.
        vfs::file::vmo::read_only(
            "0102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f20",
        ),
    );

    let base_packages = HashMap::from([(
        "fuchsia-pkg://fuchsia.com/my-package".to_owned(),
        Arc::new(my_package) as Arc<dyn DirectoryEntry>,
    )]);

    let instance = create_realm(base_manifest, base_packages).await?;

    // This is unused but connecting to it causes DriverManager to start.
    let _admin = instance
        .root
        .connect_to_protocol_at_exposed_dir::<fidl_fuchsia_device_manager::AdministratorMarker>()?;

    let out_dir = instance.root.get_exposed_dir();
    let driver_proxy = device_watcher::recursive_wait_and_open::<
        fidl_fuchsia_device_firmware_test::TestDeviceMarker,
    >(&out_dir, "dev-topological/sys/test/ddk-firmware-test-device-0")
    .await?;

    // Check that we can load firmware out of /boot.
    driver_proxy.load_firmware("test-firmware").await?.unwrap();

    // Check that we can load firmware from our package.
    driver_proxy.load_firmware("package-firmware").await?.unwrap();

    // Check that loading unknown name fails.
    assert!(
        driver_proxy.load_firmware("test-bad").await? == Err(fuchsia_zircon::sys::ZX_ERR_NOT_FOUND)
    );
    Ok(())
}

#[fuchsia::test]
async fn load_package_firmware_test_dfv2() -> Result<(), Error> {
    // Create the RealmBuilder.
    let builder = RealmBuilder::new().await?;
    builder.driver_test_realm_setup().await?;
    let instance = builder.build().await?;

    // Start DriverTestRealm
    let args = fdt::RealmArgs {
        use_driver_framework_v2: Some(true),
        root_driver: Some("fuchsia-boot:///#meta/test-parent-sys.cm".to_string()),
        ..Default::default()
    };
    instance.driver_test_realm_start(args).await?;

    // Connect to our driver.
    let dev = instance.driver_test_realm_connect_to_dev()?;
    let driver_proxy = device_watcher::recursive_wait_and_open::<
        fidl_fuchsia_device_firmware_test::TestDeviceMarker,
    >(&dev, "sys/test/ddk-firmware-test-device-0")
    .await?;

    // Check that we can load firmware from our package.
    driver_proxy.load_firmware("test-firmware").await?.unwrap();

    // Check that loading unknown name fails.
    assert_eq!(
        driver_proxy.load_firmware("test-bad").await?,
        Err(fuchsia_zircon::sys::ZX_ERR_NOT_FOUND)
    );
    Ok(())
}
