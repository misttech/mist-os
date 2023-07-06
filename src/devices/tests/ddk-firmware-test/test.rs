// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::Error,
    fidl::endpoints::ServerEnd,
    fidl::endpoints::{create_proxy, ClientEnd, Proxy},
    fidl_fuchsia_component_decl as fdecl, fidl_fuchsia_component_resolution as fresolution,
    fidl_fuchsia_driver_test as fdt, fidl_fuchsia_io as fio, fidl_fuchsia_mem as fmem,
    fuchsia_async::Task,
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

type Directory =
    Arc<vfs::directory::simple::Simple<vfs::directory::immutable::connection::ImmutableConnection>>;

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
struct FakeBaseComponentResolver {
    base_packages: HashMap<String, fio::DirectoryProxy>,
}

impl FakeBaseComponentResolver {
    pub fn new(base_packages: HashMap<String, fio::DirectoryProxy>) -> Self {
        Self { base_packages }
    }

    pub async fn handle_request_stream(
        self: Arc<Self>,
        mut stream: fresolution::ResolverRequestStream,
    ) {
        while let Some(req) = stream
            .try_next()
            .await
            .expect("read fuchsia.component.resolution/Resolver request stream")
        {
            match req {
                fresolution::ResolverRequest::Resolve { component_url, responder } => {
                    let pkg = match self.base_packages.get(&component_url) {
                        Some(proxy) => {
                            let proxy_clone = fuchsia_fs::directory::clone_no_describe(proxy, None)
                                .expect("failed to clone");
                            ClientEnd::new(proxy_clone.into_channel().unwrap().into_zx_channel())
                        }
                        None => panic!(
                            "FakeBaseComponentResolver resolve unknown component {component_url}"
                        ),
                    };
                    responder
                        .send(Ok(fresolution::Component {
                            decl: Some(fmem::Data::Bytes(
                                fidl::persist(&fdecl::Component::default()).unwrap(),
                            )),
                            package: Some(fresolution::Package {
                                directory: Some(pkg),
                                ..Default::default()
                            }),
                            ..Default::default()
                        }))
                        .expect("error sending response");
                }
                req => panic!("unexpected fuchsia.component.resolution/Resolver request {req:?}"),
            }
        }
    }
}

fn serve_vfs_dir(
    root: Arc<impl DirectoryEntry>,
    server_end: ServerEnd<fio::NodeMarker>,
) -> Task<()> {
    let fs_scope = ExecutionScope::new();
    root.open(
        fs_scope.clone(),
        fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_EXECUTABLE,
        vfs::path::Path::dot(),
        server_end,
    );

    Task::spawn(async move { fs_scope.wait().await })
}

async fn serve_fake_filesystem(
    base_packages: HashMap<String, fio::DirectoryProxy>,
    handles: LocalComponentHandles,
) -> Result<(), anyhow::Error> {
    let root: Directory = vfs::pseudo_directory! {
        "svc" => vfs::pseudo_directory! {
            "fuchsia.component.resolution.Resolver-base" => vfs::service::host(move |stream| {
                let base_packages = base_packages.clone();
                Arc::new(FakeBaseComponentResolver::new(base_packages)).handle_request_stream(stream)
            }
            ),
        },
        "boot" => vfs::pseudo_directory! {
            "meta" => vfs::pseudo_directory! {},
        },
    };
    serve_vfs_dir(root, ServerEnd::new(handles.outgoing_dir.into_channel())).await;
    Ok::<(), anyhow::Error>(())
}

async fn create_realm(
    base_packages: HashMap<String, fio::DirectoryProxy>,
) -> Result<fuchsia_component_test::RealmInstance, Error> {
    let builder = RealmBuilder::new().await?;

    let fake_filesystem = builder
        .add_local_child(
            "fake_filesystem",
            move |h: LocalComponentHandles| serve_fake_filesystem(base_packages.clone(), h).boxed(),
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
                .capability(Capability::protocol_by_name(
                    "fuchsia.component.resolution.Resolver-base",
                ))
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

    let (config_client, config_server) = create_proxy::<fio::DirectoryMarker>().unwrap();
    let _config_task = serve_vfs_dir(
        vfs::pseudo_directory! {
            "config" => vfs::pseudo_directory! { "base-driver-manifest.json" => base_manifest },
        },
        ServerEnd::new(config_server.into_channel()),
    );

    let (pkg_client, pkg_server) = create_proxy::<fio::DirectoryMarker>().unwrap();
    let _pkg_task = serve_vfs_dir(my_package.into(), ServerEnd::new(pkg_server.into_channel()));
    let base_packages = HashMap::from([
        ("fuchsia-pkg://fuchsia.com/driver-manager-base-config".to_owned(), config_client),
        ("fuchsia-pkg://fuchsia.com/my-package".to_owned(), pkg_client),
    ]);

    let instance = create_realm(base_packages).await?;

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
