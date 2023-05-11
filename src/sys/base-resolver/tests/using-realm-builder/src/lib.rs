// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    assert_matches::assert_matches,
    blobfs_ramdisk::BlobfsRamdisk,
    fidl::endpoints::DiscoverableProtocolMarker as _,
    fidl_fuchsia_boot as fboot, fidl_fuchsia_component_decl as fcomponent_decl,
    fidl_fuchsia_component_resolution as fcomponent_resolution, fidl_fuchsia_io as fio,
    fidl_fuchsia_pkg as fpkg,
    fuchsia_component_test::{Capability, ChildOptions, RealmBuilder, RealmInstance, Ref, Route},
    futures::{
        future::{BoxFuture, FutureExt as _},
        stream::TryStreamExt as _,
    },
    std::sync::Arc,
    vfs::{directory::entry::DirectoryEntry, execution_scope::ExecutionScope},
};

static PKGFS_BOOT_ARG_KEY: &'static str = "zircon.system.pkgfs.cmd";
static PKGFS_BOOT_ARG_VALUE_PREFIX: &'static str = "bin/pkgsvr+";
static SHELL_COMMANDS_BIN_PATH: &'static str = "shell-commands-bin";

trait BootArgumentsStreamHandler: Send + Sync {
    fn handle_stream(&self, stream: fboot::ArgumentsRequestStream) -> BoxFuture<'static, ()>;
}

struct TestEnvBuilder {
    blobfs: Option<BlobfsRamdisk>,
    boot_args: Option<Arc<dyn BootArgumentsStreamHandler>>,
}

impl TestEnvBuilder {
    fn new() -> Self {
        Self { blobfs: None, boot_args: None }
    }

    async fn static_packages(self, static_packages: &[&fuchsia_pkg_testing::Package]) -> Self {
        assert!(self.blobfs.is_none());
        assert!(self.boot_args.is_none());

        let system_image = fuchsia_pkg_testing::SystemImageBuilder::new()
            .static_packages(static_packages)
            .build()
            .await;

        let blobfs = BlobfsRamdisk::start().await.unwrap();
        let root_dir = blobfs.root_dir().unwrap();
        let () = system_image.write_to_blobfs_dir(&root_dir);
        for pkg in static_packages {
            let () = pkg.write_to_blobfs_dir(&root_dir);
        }

        Self {
            blobfs: Some(blobfs),
            boot_args: Some(Arc::new(BootArgsFixedHash::new(*system_image.meta_far_merkle_root()))),
        }
    }

    async fn build(self) -> TestEnv {
        let blobfs = self.blobfs.unwrap();
        let blobfs_dir = vfs::remote::remote_dir(blobfs.root_dir_proxy().unwrap());
        let boot_args = self.boot_args.unwrap();

        let builder = RealmBuilder::new().await.unwrap();

        let resolver = builder
            .add_child("resolver", "#meta/base-resolver.cm", ChildOptions::new())
            .await
            .unwrap();

        let local_mocks = builder
            .add_local_child(
                "local_mocks",
                move |handles| {
                    let blobfs_dir = blobfs_dir.clone();
                    let boot_args_clone = boot_args.clone();
                    let out_dir = vfs::pseudo_directory! {
                        "blob" => blobfs_dir,
                        "svc" => vfs::pseudo_directory! {
                            fboot::ArgumentsMarker::PROTOCOL_NAME =>
                                vfs::service::host(move |stream|
                                    boot_args_clone.handle_stream(stream)
                                ),
                        },
                    };
                    let scope = ExecutionScope::new();
                    let () = out_dir.open(
                        scope.clone(),
                        fio::OpenFlags::RIGHT_READABLE
                            | fio::OpenFlags::RIGHT_WRITABLE
                            | fio::OpenFlags::RIGHT_EXECUTABLE,
                        vfs::path::Path::dot(),
                        handles.outgoing_dir.into_channel().into(),
                    );
                    async move { Ok(scope.wait().await) }.boxed()
                },
                ChildOptions::new(),
            )
            .await
            .unwrap();

        builder
            .add_route(
                Route::new()
                    .capability(
                        Capability::directory("blob-exec").path("/blob").rights(fio::RX_STAR_DIR),
                    )
                    .capability(Capability::protocol::<fboot::ArgumentsMarker>())
                    .from(&local_mocks)
                    .to(&resolver),
            )
            .await
            .unwrap();

        builder
            .add_route(
                Route::new()
                    .capability(Capability::protocol::<fidl_fuchsia_logger::LogSinkMarker>())
                    .from(Ref::parent())
                    .to(&resolver),
            )
            .await
            .unwrap();

        builder
            .add_route(
                Route::new()
                    .capability(Capability::protocol::<fpkg::PackageResolverMarker>())
                    .capability(Capability::protocol::<fcomponent_resolution::ResolverMarker>())
                    .capability(Capability::directory(SHELL_COMMANDS_BIN_PATH))
                    .from(&resolver)
                    .to(Ref::parent()),
            )
            .await
            .unwrap();

        TestEnv { realm_instance: builder.build().await.unwrap(), _blobfs: blobfs }
    }
}

struct TestEnv {
    realm_instance: RealmInstance,
    _blobfs: BlobfsRamdisk,
}

impl TestEnv {
    fn package_resolver(&self) -> fpkg::PackageResolverProxy {
        self.realm_instance
            .root
            .connect_to_protocol_at_exposed_dir::<fpkg::PackageResolverMarker>()
            .unwrap()
    }

    async fn resolve_package(
        &self,
        url: &str,
    ) -> Result<(fio::DirectoryProxy, fpkg::ResolutionContext), fpkg::ResolveError> {
        let (package, package_server_end) = fidl::endpoints::create_proxy().unwrap();
        let context = self.package_resolver().resolve(url, package_server_end).await.unwrap()?;
        Ok((package, context))
    }

    async fn resolve_with_context_package(
        &self,
        url: &str,
        in_context: fpkg::ResolutionContext,
    ) -> Result<(fio::DirectoryProxy, fpkg::ResolutionContext), fpkg::ResolveError> {
        let (package, package_server_end) = fidl::endpoints::create_proxy().unwrap();
        let out_context = self
            .package_resolver()
            .resolve_with_context(url, &in_context, package_server_end)
            .await
            .unwrap()?;
        Ok((package, out_context))
    }

    fn component_resolver(&self) -> fcomponent_resolution::ResolverProxy {
        self.realm_instance
            .root
            .connect_to_protocol_at_exposed_dir::<fcomponent_resolution::ResolverMarker>()
            .unwrap()
    }

    async fn resolve_component(
        &self,
        url: &str,
    ) -> Result<fcomponent_resolution::Component, fcomponent_resolution::ResolverError> {
        self.component_resolver().resolve(url).await.unwrap()
    }

    async fn resolve_with_context_component(
        &self,
        url: &str,
        context: fcomponent_resolution::Context,
    ) -> Result<fcomponent_resolution::Component, fcomponent_resolution::ResolverError> {
        self.component_resolver().resolve_with_context(url, &context).await.unwrap()
    }

    fn shell_commands_bin(&self) -> fio::DirectoryProxy {
        fuchsia_fs::directory::open_directory_no_describe(
            self.realm_instance.root.get_exposed_dir(),
            SHELL_COMMANDS_BIN_PATH,
            fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_EXECUTABLE,
        )
        .expect("open shell-commands-bin")
    }
}

// Responds to requests for "zircon.system.pkgfs.cmd" with the provided hash.
struct BootArgsFixedHash {
    hash: fuchsia_hash::Hash,
}

impl BootArgsFixedHash {
    fn new(hash: fuchsia_hash::Hash) -> Self {
        Self { hash }
    }
}

impl BootArgumentsStreamHandler for BootArgsFixedHash {
    fn handle_stream(&self, mut stream: fboot::ArgumentsRequestStream) -> BoxFuture<'static, ()> {
        let hash = self.hash;
        async move {
            while let Some(request) = stream.try_next().await.unwrap() {
                match request {
                    fboot::ArgumentsRequest::GetString { key, responder } => {
                        assert_eq!(key, PKGFS_BOOT_ARG_KEY);
                        responder
                            .send(Some(&format!("{}{}", PKGFS_BOOT_ARG_VALUE_PREFIX, hash)))
                            .unwrap();
                    }
                    req => panic!("unexpected request {:?}", req),
                }
            }
        }
        .boxed()
    }
}

#[fuchsia::test]
async fn resolve_static_package() {
    let base_pkg =
        fuchsia_pkg_testing::PackageBuilder::new("a-base-package").build().await.unwrap();
    let env = TestEnvBuilder::new().static_packages(&[&base_pkg]).await.build().await;

    let (resolved, _) =
        env.resolve_package("fuchsia-pkg://fuchsia.com/a-base-package/0").await.unwrap();

    let () = base_pkg.verify_contents(&resolved).await.unwrap();
}

#[fuchsia::test]
async fn resolve_system_image() {
    let env = TestEnvBuilder::new().static_packages(&[]).await.build().await;

    let (resolved, _) =
        env.resolve_package("fuchsia-pkg://fuchsia.com/system_image/0").await.unwrap();

    assert_eq!(
        fuchsia_pkg::PackageDirectory::from_proxy(resolved)
            .meta_package()
            .await
            .unwrap()
            .into_path(),
        system_image::SystemImage::package_path()
    );
}

#[fuchsia::test]
async fn resolve_with_context_absolute_url_package() {
    let base_pkg =
        fuchsia_pkg_testing::PackageBuilder::new("a-base-package").build().await.unwrap();
    let env = TestEnvBuilder::new().static_packages(&[&base_pkg]).await.build().await;

    let (resolved, _) = env
        .resolve_with_context_package(
            "fuchsia-pkg://fuchsia.com/a-base-package/0",
            fpkg::ResolutionContext { bytes: vec![] },
        )
        .await
        .unwrap();

    let () = base_pkg.verify_contents(&resolved).await.unwrap();
}

#[fuchsia::test]
async fn resolve_with_context_relative_url_package() {
    let sub_sub_pkg =
        fuchsia_pkg_testing::PackageBuilder::new("sub-sub-package").build().await.unwrap();
    let sub_pkg = fuchsia_pkg_testing::PackageBuilder::new("sub-package")
        .add_subpackage("sub-sub-package-url", &sub_sub_pkg)
        .build()
        .await
        .unwrap();
    let super_pkg = fuchsia_pkg_testing::PackageBuilder::new("super-package")
        .add_subpackage("sub-package-url", &sub_pkg)
        .build()
        .await
        .unwrap();
    let env = TestEnvBuilder::new().static_packages(&[&super_pkg]).await.build().await;
    let (_, context) =
        env.resolve_package("fuchsia-pkg://fuchsia.com/super-package/0").await.unwrap();

    let (resolved, context) =
        env.resolve_with_context_package("sub-package-url", context).await.unwrap();
    let () = sub_pkg.verify_contents(&resolved).await.unwrap();

    let (resolved, _) =
        env.resolve_with_context_package("sub-sub-package-url", context).await.unwrap();
    let () = sub_sub_pkg.verify_contents(&resolved).await.unwrap();
}

#[fuchsia::test]
async fn manipulated_context_rejected() {
    let sub_pkg = fuchsia_pkg_testing::PackageBuilder::new("sub-package").build().await.unwrap();
    let super_pkg = fuchsia_pkg_testing::PackageBuilder::new("super-package")
        .add_subpackage("sub-package-url", &sub_pkg)
        .build()
        .await
        .unwrap();
    let env = TestEnvBuilder::new().static_packages(&[&super_pkg]).await.build().await;
    let (_, mut context) =
        env.resolve_package("fuchsia-pkg://fuchsia.com/super-package/0").await.unwrap();
    context.bytes[0] = !context.bytes[0];

    assert_matches!(
        env.resolve_with_context_package("sub-package-url", context).await,
        Err(fpkg::ResolveError::InvalidContext)
    );
}

#[fuchsia::test]
async fn resolve_component() {
    let manifest = fidl::encoding::persist(&fcomponent_decl::Component {
        config: Some(fcomponent_decl::ConfigSchema {
            value_source: Some(fcomponent_decl::ConfigValueSource::PackagePath(
                "meta/config-data.cvf".to_string(),
            )),
            ..Default::default()
        }),
        ..Default::default()
    })
    .unwrap();
    let config_data =
        fidl::encoding::persist(&fcomponent_decl::ConfigValuesData::default()).unwrap();
    let base_pkg = fuchsia_pkg_testing::PackageBuilder::new("a-base-package")
        .abi_revision(version_history::AbiRevision::new(0x601665c5b1a89c7f))
        .add_resource_at("meta/manifest.cm", &*manifest)
        .add_resource_at("meta/config-data.cvf", &*config_data)
        .build()
        .await
        .unwrap();
    let env = TestEnvBuilder::new().static_packages(&[&base_pkg]).await.build().await;

    let fcomponent_resolution::Component {
        url,
        decl,
        package,
        config_values,
        resolution_context,
        abi_revision,
        ..
    } = env
        .resolve_component("fuchsia-pkg://fuchsia.com/a-base-package/0#meta/manifest.cm")
        .await
        .unwrap();

    assert_eq!(url.unwrap(), "fuchsia-pkg://fuchsia.com/a-base-package/0#meta/manifest.cm");
    assert_eq!(mem_util::bytes_from_data(decl.as_ref().unwrap()).unwrap(), manifest);
    let fcomponent_resolution::Package { url, directory, .. } = package.unwrap();
    assert_eq!(url.unwrap(), "fuchsia-pkg://fuchsia.com/a-base-package/0");
    let () = base_pkg.verify_contents(&directory.unwrap().into_proxy().unwrap()).await.unwrap();
    assert_eq!(mem_util::bytes_from_data(config_values.as_ref().unwrap()).unwrap(), config_data);
    assert!(resolution_context.is_some());
    assert_eq!(abi_revision, Some(0x601665c5b1a89c7f));
}

#[fuchsia::test]
async fn resolve_with_context_component() {
    let manifest = fidl::encoding::persist(&fcomponent_decl::Component::default().clone()).unwrap();
    let sub_sub_pkg = fuchsia_pkg_testing::PackageBuilder::new("a-sub-sub-package")
        .add_resource_at("meta/manifest.cm", &*manifest)
        .build()
        .await
        .unwrap();
    let sub_pkg = fuchsia_pkg_testing::PackageBuilder::new("a-sub-package")
        .add_resource_at("meta/manifest.cm", &*manifest)
        .add_subpackage("sub-sub-package-url", &sub_sub_pkg)
        .build()
        .await
        .unwrap();
    let base_pkg = fuchsia_pkg_testing::PackageBuilder::new("a-base-package")
        .add_resource_at("meta/manifest.cm", &*manifest)
        .add_subpackage("sub-package-url", &sub_pkg)
        .build()
        .await
        .unwrap();
    let env = TestEnvBuilder::new().static_packages(&[&base_pkg]).await.build().await;

    let context = env
        .resolve_with_context_component(
            "fuchsia-pkg://fuchsia.com/a-base-package/0#meta/manifest.cm",
            fcomponent_resolution::Context { bytes: vec![] },
        )
        .await
        .unwrap()
        .resolution_context
        .unwrap();
    let context = env
        .resolve_with_context_component("sub-package-url#meta/manifest.cm", context)
        .await
        .unwrap()
        .resolution_context
        .unwrap();

    let fcomponent_resolution::Component {
        url,
        decl,
        package,
        config_values,
        resolution_context,
        abi_revision,
        ..
    } = env
        .resolve_with_context_component("sub-sub-package-url#meta/manifest.cm", context)
        .await
        .unwrap();

    assert_eq!(url.unwrap(), "sub-sub-package-url#meta/manifest.cm");
    assert_eq!(mem_util::bytes_from_data(decl.as_ref().unwrap()).unwrap(), manifest);
    let fcomponent_resolution::Package { url, directory, .. } = package.unwrap();
    assert_eq!(url.unwrap(), "sub-sub-package-url");
    let () = sub_sub_pkg.verify_contents(&directory.unwrap().into_proxy().unwrap()).await.unwrap();
    assert_eq!(config_values, None);
    assert!(resolution_context.is_some());
    assert_eq!(abi_revision, Some(0xeccea2f70acd6fc0));
}

#[fuchsia::test]
async fn shell_commands_bin_dir() {
    let shell_commands = fuchsia_pkg_testing::PackageBuilder::new("shell-commands")
        .add_resource_at("bin/a-file", &b"the-content"[..])
        .build()
        .await
        .unwrap();
    let env = TestEnvBuilder::new().static_packages(&[&shell_commands]).await.build().await;

    assert_eq!(
        fuchsia_fs::file::read(
            &fuchsia_fs::directory::open_file(
                &env.shell_commands_bin(),
                "a-file",
                fio::OpenFlags::RIGHT_READABLE
            )
            .await
            .unwrap()
        )
        .await
        .unwrap(),
        b"the-content".to_vec()
    );
}
