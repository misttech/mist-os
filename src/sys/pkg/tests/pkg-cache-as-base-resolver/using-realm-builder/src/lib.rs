// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use assert_matches::assert_matches;
use blobfs_ramdisk::BlobfsRamdisk;
use fidl::endpoints::DiscoverableProtocolMarker as _;
use fuchsia_component_test::{Capability, ChildOptions, RealmBuilder, RealmInstance, Ref, Route};
use futures::future::{BoxFuture, FutureExt as _};
use futures::stream::TryStreamExt as _;
use std::sync::Arc;
use vfs::directory::entry_container::Directory;
use vfs::execution_scope::ExecutionScope;
use {
    fidl_fuchsia_boot as fboot, fidl_fuchsia_component_decl as fcomponent_decl,
    fidl_fuchsia_component_resolution as fcomponent_resolution, fidl_fuchsia_io as fio,
    fidl_fuchsia_pkg as fpkg, fidl_fuchsia_space as fspace,
};

static PKGFS_BOOT_ARG_KEY: &'static str = "zircon.system.pkgfs.cmd";
static PKGFS_BOOT_ARG_VALUE_PREFIX: &'static str = "bin/pkgsvr+";

// When this feature is enabled, the base-resolver integration tests will start Fxblob.
#[cfg(feature = "use_fxblob")]
static BLOB_IMPLEMENTATION: blobfs_ramdisk::Implementation = blobfs_ramdisk::Implementation::Fxblob;

// When this feature is not enabled, the base-resolver integration tests will start cpp Blobfs.
#[cfg(not(feature = "use_fxblob"))]
static BLOB_IMPLEMENTATION: blobfs_ramdisk::Implementation =
    blobfs_ramdisk::Implementation::CppBlobfs;

trait BootArgumentsStreamHandler: Send + Sync {
    fn handle_stream(&self, stream: fboot::ArgumentsRequestStream) -> BoxFuture<'static, ()>;
}

struct TestEnvBuilder {
    blobfs: BlobfsRamdisk,
    system_image_builder: fuchsia_pkg_testing::SystemImageBuilder,
    enable_upgradable_packages: Option<bool>,
}

impl TestEnvBuilder {
    async fn new() -> Self {
        Self {
            blobfs: BlobfsRamdisk::builder()
                .implementation(BLOB_IMPLEMENTATION)
                .start()
                .await
                .unwrap(),
            system_image_builder: fuchsia_pkg_testing::SystemImageBuilder::new(),
            enable_upgradable_packages: None,
        }
    }

    fn enable_upgradable_packages(self, enable_upgradable_packages: bool) -> Self {
        assert_eq!(self.enable_upgradable_packages, None);
        Self { enable_upgradable_packages: Some(enable_upgradable_packages), ..self }
    }

    async fn static_packages(self, static_packages: &[&fuchsia_pkg_testing::Package]) -> Self {
        for pkg in static_packages {
            let () = pkg.write_to_blobfs(&self.blobfs).await;
        }
        Self {
            system_image_builder: self.system_image_builder.static_packages(static_packages),
            ..self
        }
    }

    async fn cache_packages(self, cache_packages: &[&fuchsia_pkg_testing::Package]) -> Self {
        for pkg in cache_packages {
            let () = pkg.write_to_blobfs(&self.blobfs).await;
        }
        Self {
            system_image_builder: self.system_image_builder.cache_packages(cache_packages),
            ..self
        }
    }

    async fn build(self) -> TestEnv {
        let blobfs = self.blobfs;
        let blobfs_dir = vfs::remote::remote_dir(blobfs.root_dir_proxy().unwrap());

        let system_image = self.system_image_builder.build().await;
        let () = system_image.write_to_blobfs(&blobfs).await;
        let boot_args = Arc::new(BootArgsFixedHash::new(*system_image.hash()));

        let builder = RealmBuilder::new().await.unwrap();

        let pkg_cache = builder
            .add_child("pkg_cache", "#meta/pkg-cache.cm", ChildOptions::new())
            .await
            .unwrap();
        let pkg_cache_config = builder
            .add_child("pkg_cache_config", "#meta/pkg-cache-config.cm", ChildOptions::new())
            .await
            .unwrap();
        builder
            .add_route(
                Route::new()
                    .capability(Capability::configuration("fuchsia.pkgcache.AllPackagesExecutable"))
                    .capability(Capability::configuration("fuchsia.pkgcache.UseSystemImage"))
                    .from(&pkg_cache_config)
                    .to(&pkg_cache),
            )
            .await
            .unwrap();

        if let Some(enable_upgradable_packages) = self.enable_upgradable_packages {
            builder
                .add_capability(cm_rust::CapabilityDecl::Config(cm_rust::ConfigurationDecl {
                    name: "fuchsia.pkgcache.EnableUpgradablePackages".parse().unwrap(),
                    value: enable_upgradable_packages.into(),
                }))
                .await
                .unwrap();
        }
        builder
            .add_route(
                Route::new()
                    .capability(Capability::configuration(
                        "fuchsia.pkgcache.EnableUpgradablePackages",
                    ))
                    .from(if self.enable_upgradable_packages.is_some() {
                        Ref::self_()
                    } else {
                        (&pkg_cache_config).into()
                    })
                    .to(&pkg_cache),
            )
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

        builder.init_mutable_config_from_package(&pkg_cache).await.unwrap();
        match BLOB_IMPLEMENTATION {
            blobfs_ramdisk::Implementation::Fxblob => {
                builder
                    .add_capability(cm_rust::CapabilityDecl::Config(cm_rust::ConfigurationDecl {
                        name: "fuchsia.pkgcache.UseFxblob".parse().unwrap(),
                        value: true.into(),
                    }))
                    .await
                    .unwrap();

                let svc_dir = vfs::remote::remote_dir(blobfs.svc_dir().unwrap().unwrap());
                let service_reflector = builder
                    .add_local_child(
                        "service_reflector",
                        move |handles| {
                            let out_dir = vfs::pseudo_directory! {
                                "blob-svc" => svc_dir.clone(),
                            };
                            let scope = vfs::execution_scope::ExecutionScope::new();
                            let () = out_dir.open(
                                scope.clone(),
                                fio::OpenFlags::RIGHT_READABLE
                                    | fio::OpenFlags::RIGHT_WRITABLE
                                    | fio::OpenFlags::RIGHT_EXECUTABLE,
                                vfs::path::Path::dot(),
                                handles.outgoing_dir.into_channel().into(),
                            );
                            async move {
                                scope.wait().await;
                                Ok(())
                            }
                            .boxed()
                        },
                        ChildOptions::new(),
                    )
                    .await
                    .unwrap();

                builder
                    .add_route(
                        Route::new()
                            .capability(
                                Capability::protocol::<fidl_fuchsia_fxfs::BlobCreatorMarker>()
                                    .path(format!(
                                        "/blob-svc/{}",
                                        fidl_fuchsia_fxfs::BlobCreatorMarker::PROTOCOL_NAME
                                    )),
                            )
                            .capability(
                                Capability::protocol::<fidl_fuchsia_fxfs::BlobReaderMarker>().path(
                                    format!(
                                        "/blob-svc/{}",
                                        fidl_fuchsia_fxfs::BlobReaderMarker::PROTOCOL_NAME
                                    ),
                                ),
                            )
                            .from(&service_reflector)
                            .to(&pkg_cache),
                    )
                    .await
                    .unwrap();
            }
            blobfs_ramdisk::Implementation::CppBlobfs => {
                builder
                    .add_capability(cm_rust::CapabilityDecl::Config(cm_rust::ConfigurationDecl {
                        name: "fuchsia.pkgcache.UseFxblob".parse().unwrap(),
                        value: false.into(),
                    }))
                    .await
                    .unwrap();
            }
        }
        builder
            .add_route(
                Route::new()
                    .capability(Capability::configuration("fuchsia.pkgcache.UseFxblob"))
                    .from(Ref::self_())
                    .to(&pkg_cache),
            )
            .await
            .unwrap();

        builder
            .add_route(
                Route::new()
                    .capability(
                        Capability::directory("blob-exec")
                            .path("/blob")
                            .rights(fio::RW_STAR_DIR | fio::Operations::EXECUTE),
                    )
                    .capability(Capability::protocol::<fboot::ArgumentsMarker>())
                    .from(&local_mocks)
                    .to(&pkg_cache),
            )
            .await
            .unwrap();

        builder
            .add_route(
                Route::new()
                    .capability(Capability::protocol::<fidl_fuchsia_logger::LogSinkMarker>())
                    .from(Ref::parent())
                    .to(&pkg_cache),
            )
            .await
            .unwrap();

        builder
            .add_route(
                Route::new()
                    .capability(Capability::protocol::<fpkg::PackageCacheMarker>())
                    .capability(Capability::protocol::<fpkg::RetainedPackagesMarker>())
                    .capability(Capability::protocol::<fspace::ManagerMarker>())
                    .capability(Capability::protocol::<fpkg::PackageResolverMarker>())
                    .capability(Capability::protocol::<fcomponent_resolution::ResolverMarker>())
                    .capability(Capability::directory("pkgfs"))
                    .capability(Capability::directory("system"))
                    .capability(Capability::directory("pkgfs-packages"))
                    .from(&pkg_cache)
                    .to(Ref::parent()),
            )
            .await
            .unwrap();

        TestEnv { realm_instance: builder.build().await.unwrap(), blobfs }
    }
}

struct TestEnv {
    realm_instance: RealmInstance,
    blobfs: BlobfsRamdisk,
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
        let (package, package_server_end) = fidl::endpoints::create_proxy();
        let context = self.package_resolver().resolve(url, package_server_end).await.unwrap()?;
        Ok((package, context))
    }

    async fn resolve_and_verify_package(&self, pkg: &fuchsia_pkg_testing::Package) {
        let (resolved, _) = self
            .resolve_package(&format!("fuchsia-pkg://fuchsia.com/{}", pkg.name()))
            .await
            .unwrap();
        let () = pkg.verify_contents(&resolved).await.unwrap();
    }

    async fn resolve_with_context_package(
        &self,
        url: &str,
        in_context: fpkg::ResolutionContext,
    ) -> Result<(fio::DirectoryProxy, fpkg::ResolutionContext), fpkg::ResolveError> {
        let (package, package_server_end) = fidl::endpoints::create_proxy();
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

    fn package_cache(&self) -> fpkg::PackageCacheProxy {
        self.realm_instance
            .root
            .connect_to_protocol_at_exposed_dir::<fpkg::PackageCacheMarker>()
            .unwrap()
    }

    async fn set_upgradable_urls(
        &self,
        urls: impl IntoIterator<Item = impl std::fmt::Display>,
    ) -> Result<(), fpkg::SetUpgradableUrlsError> {
        let urls: Vec<_> =
            urls.into_iter().map(|url| fpkg::PackageUrl { url: url.to_string() }).collect();
        self.package_cache().set_upgradable_urls(&urls).await.unwrap()
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
    let env = TestEnvBuilder::new().await.static_packages(&[&base_pkg]).await.build().await;

    let () = env.resolve_and_verify_package(&base_pkg).await;
}

#[fuchsia::test]
async fn resolve_system_image() {
    let env = TestEnvBuilder::new().await.build().await;

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
    let env = TestEnvBuilder::new().await.static_packages(&[&base_pkg]).await.build().await;

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
    let env = TestEnvBuilder::new().await.static_packages(&[&super_pkg]).await.build().await;
    let (_dir, context) =
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
    let env = TestEnvBuilder::new().await.static_packages(&[&super_pkg]).await.build().await;
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
    let manifest = fidl::persist(&fcomponent_decl::Component {
        config: Some(fcomponent_decl::ConfigSchema {
            value_source: Some(fcomponent_decl::ConfigValueSource::PackagePath(
                "meta/config-data.cvf".to_string(),
            )),
            ..Default::default()
        }),
        ..Default::default()
    })
    .unwrap();
    let config_data = fidl::persist(&fcomponent_decl::ConfigValuesData::default()).unwrap();
    let base_pkg = fuchsia_pkg_testing::PackageBuilder::new_with_abi_revision(
        "a-base-package",
        0x601665c5b1a89c7f.into(),
    )
    .add_resource_at("meta/manifest.cm", &*manifest)
    .add_resource_at("meta/config-data.cvf", &*config_data)
    .build()
    .await
    .unwrap();
    let env = TestEnvBuilder::new().await.static_packages(&[&base_pkg]).await.build().await;

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
    let () = base_pkg.verify_contents(&directory.unwrap().into_proxy()).await.unwrap();
    assert_eq!(mem_util::bytes_from_data(config_values.as_ref().unwrap()).unwrap(), config_data);
    assert!(resolution_context.is_some());
    assert_eq!(abi_revision, Some(0x601665c5b1a89c7f));
}

#[fuchsia::test]
async fn resolve_with_context_component() {
    let manifest = fidl::persist(&fcomponent_decl::Component::default().clone()).unwrap();
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
    let env = TestEnvBuilder::new().await.static_packages(&[&base_pkg]).await.build().await;

    let component = env
        .resolve_with_context_component(
            "fuchsia-pkg://fuchsia.com/a-base-package/0#meta/manifest.cm",
            fcomponent_resolution::Context { bytes: vec![] },
        )
        .await
        .unwrap();
    let component = env
        .resolve_with_context_component(
            "sub-package-url#meta/manifest.cm",
            component.resolution_context.unwrap(),
        )
        .await
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
        .resolve_with_context_component(
            "sub-sub-package-url#meta/manifest.cm",
            component.resolution_context.unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(url.unwrap(), "sub-sub-package-url#meta/manifest.cm");
    assert_eq!(mem_util::bytes_from_data(decl.as_ref().unwrap()).unwrap(), manifest);
    let fcomponent_resolution::Package { url, directory, .. } = package.unwrap();
    assert_eq!(url.unwrap(), "sub-sub-package-url");
    let () = sub_sub_pkg.verify_contents(&directory.unwrap().into_proxy()).await.unwrap();
    assert_eq!(config_values, None);
    assert!(resolution_context.is_some());
    assert_eq!(abi_revision, Some(0xeccea2f70acd6fc0));
}

#[fuchsia::test]
async fn resolve_upgradable_package() {
    let upgradable_pkg =
        fuchsia_pkg_testing::PackageBuilder::new("upgradable-package").build().await.unwrap();
    let upgradable_pkg2 =
        fuchsia_pkg_testing::PackageBuilder::new("upgradable-package2").build().await.unwrap();
    let env = TestEnvBuilder::new()
        .await
        .enable_upgradable_packages(true)
        .cache_packages(&[&upgradable_pkg, &upgradable_pkg2])
        .await
        .build()
        .await;

    let persisted_upgradable_pkg = fuchsia_pkg_testing::PackageBuilder::new("upgradable-package")
        .add_resource_at("blob", &b"content"[..])
        .build()
        .await
        .unwrap();
    let () = persisted_upgradable_pkg.write_to_blobfs(&env.blobfs).await;

    let () = env.set_upgradable_urls([persisted_upgradable_pkg.fuchsia_url()]).await.unwrap();
    let () = env.resolve_and_verify_package(&persisted_upgradable_pkg).await;
    // cache fallback
    let () = env.resolve_and_verify_package(&upgradable_pkg2).await;

    let new_upgradable_pkg2 = fuchsia_pkg_testing::PackageBuilder::new("upgradable-package2")
        .add_resource_at("blob2", &b"content2"[..])
        .build()
        .await
        .unwrap();
    let () = new_upgradable_pkg2.write_to_blobfs(&env.blobfs).await;
    let () = env.set_upgradable_urls([new_upgradable_pkg2.fuchsia_url()]).await.unwrap();
    let () = env.resolve_and_verify_package(&new_upgradable_pkg2).await;
}

#[fuchsia::test]
async fn set_upgradable_urls_does_not_block_base_packages() {
    let base_pkg =
        fuchsia_pkg_testing::PackageBuilder::new("a-base-package").build().await.unwrap();
    let env = TestEnvBuilder::new()
        .await
        .enable_upgradable_packages(true)
        .static_packages(&[&base_pkg])
        .await
        .build()
        .await;
    // base packages can be resolved before `set_upgradable_urls` is called
    let () = env.resolve_and_verify_package(&base_pkg).await;
}

#[fuchsia::test]
async fn set_upgradable_urls_ignore_base_packages_and_invalid_urls() {
    let base_pkg =
        fuchsia_pkg_testing::PackageBuilder::new("a-base-package").build().await.unwrap();
    let upgradable_pkg =
        fuchsia_pkg_testing::PackageBuilder::new("upgradable-package").build().await.unwrap();
    let env = TestEnvBuilder::new()
        .await
        .enable_upgradable_packages(true)
        .static_packages(&[&base_pkg])
        .await
        .cache_packages(&[&upgradable_pkg])
        .await
        .build()
        .await;
    assert_matches!(
        env.set_upgradable_urls([
            base_pkg.fuchsia_url().to_string(),
            "".into(),
            "http://fuchsia.com/wrong-scheme".into(),
            "fuchsia-pkg://fuchsia.com/unpinned".into()
        ])
        .await,
        Err(fpkg::SetUpgradableUrlsError::PartialSet)
    );
    // resolving upgradable packages are unblocked even if set_upgradable_urls returned an error
    let () = env.resolve_and_verify_package(&upgradable_pkg).await;
}
