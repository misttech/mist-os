// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Error;

/// Represents the sandboxed package resolver.
pub struct Resolver {
    _pkg_resolver_proxy: fidl_fuchsia_pkg::PackageResolverProxy,
}

impl Resolver {
    pub fn new_with_proxy(
        pkg_resolver_proxy: fidl_fuchsia_pkg::PackageResolverProxy,
    ) -> Result<Self, Error> {
        Ok(Self { _pkg_resolver_proxy: pkg_resolver_proxy })
    }

    pub fn new() -> Result<Self, Error> {
        Ok(Self {
            _pkg_resolver_proxy: fuchsia_component::client::connect_to_protocol::<
                fidl_fuchsia_pkg::PackageResolverMarker,
            >()?,
        })
    }
}

#[cfg(test)]
pub(crate) mod for_tests {
    use {
        super::*,
        crate::cache::for_tests::CacheForTest,
        anyhow::{anyhow, Context, Error},
        blobfs_ramdisk::BlobfsRamdisk,
        fidl_fuchsia_boot::{ArgumentsRequest, ArgumentsRequestStream},
        fidl_fuchsia_io as fio, fidl_fuchsia_pkg_ext as pkg,
        fidl_fuchsia_pkg_ext::RepositoryConfigs,
        fuchsia_component_test::ChildRef,
        fuchsia_component_test::{
            Capability, ChildOptions, DirectoryContents, RealmBuilder, RealmInstance, Ref, Route,
        },
        fuchsia_pkg_testing::serve::ServedRepository,
        fuchsia_url::RepositoryUrl,
        futures::StreamExt,
        futures::TryStreamExt,
        std::sync::Arc,
    };

    const SSL_TEST_CERTS_PATH: &str = "/pkg/data/ssl/cert.pem";
    const SSL_CERT_FILE_NAME: &str = "cert.pem";
    pub const EMPTY_REPO_PATH: &str = "/pkg/empty-repo";

    /// This wraps the `Resolver` in order to reduce test boilerplate.
    pub struct ResolverForTest {
        pub cache: CacheForTest,
        pub resolver: Arc<Resolver>,
        _served_repo: Arc<ServedRepository>,
    }

    pub struct ResolverRealm {
        pub resolver: ChildRef,
        pub cache: ChildRef,
    }

    impl ResolverForTest {
        async fn serve_boot_args(mut stream: ArgumentsRequestStream, channel: Option<String>) {
            while let Some(req) = stream.try_next().await.unwrap() {
                match req {
                    ArgumentsRequest::GetString { key, responder } => {
                        if key == "tuf_repo_config" {
                            responder.send(channel.as_deref()).unwrap();
                        } else {
                            eprintln!("Unexpected arguments GetString: {key}, closing channel.");
                        }
                    }
                    _ => eprintln!("Unexpected arguments request, closing channel."),
                }
            }
        }

        async fn run_mocks(
            handles: fuchsia_component_test::LocalComponentHandles,
            channel: Option<String>,
        ) -> Result<(), Error> {
            let mut fs = fuchsia_component::server::ServiceFs::new();
            fs.dir("svc")
                .add_fidl_service(move |stream| Self::serve_boot_args(stream, channel.clone()));
            fs.serve_connection(handles.outgoing_dir)?;
            let () = fs.for_each_concurrent(None, |req| req).await;
            Ok(())
        }

        pub async fn realm_setup(
            realm_builder: &RealmBuilder,
            served_repo: Arc<ServedRepository>,
            repo_url: RepositoryUrl,
            channel: Option<String>,
            blobfs: &BlobfsRamdisk,
        ) -> Result<ResolverRealm, Error> {
            let cache_ref =
                CacheForTest::realm_setup(realm_builder, blobfs).await.expect("setting up cache");

            let repo_config =
                RepositoryConfigs::Version1(vec![served_repo.make_repo_config(repo_url)]);

            let cert_bytes = std::fs::read(std::path::Path::new(SSL_TEST_CERTS_PATH)).unwrap();

            let pkg_resolver = realm_builder
                .add_child("pkg-resolver", "#meta/pkg-resolver.cm", ChildOptions::new())
                .await
                .unwrap();
            if channel.is_some() {
                realm_builder.init_mutable_config_from_package(&pkg_resolver).await.unwrap();
                realm_builder
                    .set_config_value_bool(&pkg_resolver, "create_ota_channel_rewrite_rule", true)
                    .await
                    .unwrap();
            }

            realm_builder
                .read_only_directory(
                    "config-data",
                    vec![&pkg_resolver],
                    DirectoryContents::new().add_file(
                        "repositories/test.json",
                        serde_json::to_string(&repo_config).unwrap(),
                    ),
                )
                .await
                .unwrap();

            realm_builder
                .read_only_directory(
                    "root-ssl-certificates",
                    vec![&pkg_resolver],
                    DirectoryContents::new().add_file(SSL_CERT_FILE_NAME, cert_bytes),
                )
                .await
                .unwrap();

            let local_mocks = realm_builder
                .add_local_child(
                    "boot_arguments",
                    move |handles| Box::pin(Self::run_mocks(handles, channel.clone())),
                    ChildOptions::new(),
                )
                .await
                .unwrap();

            realm_builder
                .add_route(
                    Route::new()
                        .capability(Capability::protocol_by_name("fuchsia.logger.LogSink"))
                        .from(Ref::parent())
                        .to(&pkg_resolver),
                )
                .await
                .unwrap();

            realm_builder
                .add_route(
                    Route::new()
                        .capability(Capability::protocol_by_name("fuchsia.net.name.Lookup"))
                        .capability(Capability::protocol_by_name("fuchsia.posix.socket.Provider"))
                        .from(Ref::parent())
                        .to(&pkg_resolver),
                )
                .await
                .unwrap();

            realm_builder
                .add_route(
                    Route::new()
                        .capability(Capability::protocol_by_name("fuchsia.pkg.PackageResolver"))
                        .from(&pkg_resolver)
                        .to(Ref::parent()),
                )
                .await
                .unwrap();

            realm_builder
                .add_route(
                    Route::new()
                        .capability(Capability::protocol_by_name("fuchsia.pkg.PackageCache"))
                        .capability(Capability::protocol_by_name("fuchsia.space.Manager"))
                        .from(&cache_ref)
                        .to(&pkg_resolver),
                )
                .await
                .unwrap();

            realm_builder
                .add_route(
                    Route::new()
                        .capability(Capability::protocol_by_name("fuchsia.boot.Arguments"))
                        .from(&local_mocks)
                        .to(&pkg_resolver),
                )
                .await
                .unwrap();

            Ok(ResolverRealm { resolver: pkg_resolver, cache: cache_ref })
        }

        pub async fn new(
            realm_instance: &RealmInstance,
            blobfs: BlobfsRamdisk,
            served_repo: Arc<ServedRepository>,
        ) -> Result<Self, Error> {
            let cache = CacheForTest::new(realm_instance, blobfs).await.unwrap();

            let resolver_proxy = realm_instance
                .root
                .connect_to_protocol_at_exposed_dir::<fidl_fuchsia_pkg::PackageResolverMarker>()
                .expect("connect to pkg resolver");

            let resolver = Resolver::new_with_proxy(resolver_proxy).unwrap();

            let cache_proxy = cache.cache.package_cache_proxy().unwrap();

            assert_eq!(cache_proxy.sync().await.unwrap(), Ok(()));

            Ok(ResolverForTest { cache, resolver: Arc::new(resolver), _served_repo: served_repo })
        }

        /// Resolve a package using the resolver, returning the root directory of the package,
        /// and the context for resolving relative package URLs.
        pub async fn resolve_package(
            &self,
            url: &str,
        ) -> Result<(fio::DirectoryProxy, pkg::ResolutionContext), Error> {
            let proxy = &self.resolver._pkg_resolver_proxy;
            let (package, package_remote) =
                fidl::endpoints::create_proxy().context("creating package directory endpoints")?;
            let resolved_context = proxy
                .resolve(url, package_remote)
                .await
                .unwrap()
                .map_err(|e| anyhow!("Package resolver error: {:?}", e))?;
            Ok((package, (&resolved_context).try_into().expect("resolver returns valid context")))
        }
    }
}

#[cfg(test)]
pub mod tests {
    use {
        super::for_tests::{ResolverForTest, EMPTY_REPO_PATH},
        super::*,
        anyhow::Context,
        fuchsia_async as fasync,
        fuchsia_component_test::RealmBuilder,
        fuchsia_pkg_testing::{PackageBuilder, RepositoryBuilder},
        std::sync::Arc,
    };

    const TEST_REPO_URL: &str = "fuchsia-pkg://test";

    #[fasync::run_singlethreaded(test)]
    pub async fn test_resolver() -> Result<(), Error> {
        let name = "test-resolver";
        let package = PackageBuilder::new(name)
            .add_resource_at("data/file1", "hello".as_bytes())
            .add_resource_at("data/file2", "hello two".as_bytes())
            .build()
            .await
            .unwrap();
        let repo = Arc::new(
            RepositoryBuilder::from_template_dir(EMPTY_REPO_PATH)
                .add_package(&package)
                .delivery_blob_type(1)
                .build()
                .await
                .context("Building repo")
                .unwrap(),
        );

        let realm_builder = RealmBuilder::new().await.unwrap();
        let served_repo = Arc::new(Arc::clone(&repo).server().start().unwrap());
        let blobfs =
            blobfs_ramdisk::BlobfsRamdisk::start().await.context("starting blobfs").unwrap();
        let _resolver_realm = ResolverForTest::realm_setup(
            &realm_builder,
            Arc::clone(&served_repo),
            TEST_REPO_URL.parse().unwrap(),
            None,
            &blobfs,
        )
        .await
        .unwrap();

        let realm_instance = realm_builder.build().await.unwrap();

        let resolver = ResolverForTest::new(&realm_instance, blobfs, served_repo)
            .await
            .context("launching resolver")?;
        let (root_dir, _resolved_context) =
            resolver.resolve_package(&format!("{TEST_REPO_URL}/{name}")).await.unwrap();

        package.verify_contents(&root_dir).await.unwrap();
        Ok(())
    }

    #[fasync::run_singlethreaded(test)]
    pub async fn test_resolver_with_channel() -> Result<(), Error> {
        let name = "test-resolver-channel";
        let package = PackageBuilder::new(name)
            .add_resource_at("data/file1", "hello".as_bytes())
            .add_resource_at("data/file2", "hello two".as_bytes())
            .build()
            .await
            .unwrap();
        let repo = Arc::new(
            RepositoryBuilder::from_template_dir(EMPTY_REPO_PATH)
                .add_package(&package)
                .delivery_blob_type(1)
                .build()
                .await
                .context("Building repo")
                .unwrap(),
        );

        let realm_builder = RealmBuilder::new().await.unwrap();
        let served_repo = Arc::new(Arc::clone(&repo).server().start().unwrap());
        let blobfs =
            blobfs_ramdisk::BlobfsRamdisk::start().await.context("starting blobfs").unwrap();
        let _resolver_realm = ResolverForTest::realm_setup(
            &realm_builder,
            Arc::clone(&served_repo),
            "fuchsia-pkg://x64.resolver-test-channel.fuchsia.com".parse().unwrap(),
            Some("resolver-test-channel".to_owned()),
            &blobfs,
        )
        .await
        .unwrap();

        let realm_instance = realm_builder.build().await.unwrap();
        let resolver = ResolverForTest::new(&realm_instance, blobfs, served_repo)
            .await
            .context("launching resolver")?;
        let (root_dir, _resolved_context) =
            resolver.resolve_package(&format!("fuchsia-pkg://fuchsia.com/{name}")).await.unwrap();

        package.verify_contents(&root_dir).await.unwrap();
        Ok(())
    }
}
