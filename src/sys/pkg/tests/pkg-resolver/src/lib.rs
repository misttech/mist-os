// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![warn(clippy::all)]
#![allow(clippy::let_unit_value)]

use {
    assert_matches::assert_matches,
    blobfs_ramdisk::BlobfsRamdisk,
    cobalt_client::traits::AsEventCodes,
    diagnostics_hierarchy::{testing::TreeAssertion, DiagnosticsHierarchy},
    diagnostics_reader::{ArchiveReader, ComponentSelector, Inspect},
    fidl::endpoints::{ClientEnd, DiscoverableProtocolMarker as _},
    fidl::persist,
    fidl_fuchsia_boot as fboot, fidl_fuchsia_io as fio,
    fidl_fuchsia_metrics::{self as fmetrics, MetricEvent, MetricEventPayload},
    fidl_fuchsia_pkg::{
        self as fpkg, CupMarker, CupProxy, GetInfoError, PackageCacheMarker, PackageResolverMarker,
        PackageResolverProxy, RepositoryManagerMarker, RepositoryManagerProxy, WriteError,
    },
    fidl_fuchsia_pkg_ext::{
        self as pkg, RepositoryConfig, RepositoryConfigBuilder, RepositoryConfigs,
    },
    fidl_fuchsia_pkg_internal::{PersistentEagerPackage, PersistentEagerPackages},
    fidl_fuchsia_pkg_rewrite as fpkg_rewrite,
    fidl_fuchsia_pkg_rewrite_ext::{Rule, RuleConfig},
    fidl_fuchsia_sys2 as fsys2, fuchsia_async as fasync,
    fuchsia_component_test::{
        Capability, ChildOptions, RealmBuilder, RealmInstance, Ref, Route, ScopedInstance,
    },
    fuchsia_merkle::{Hash, MerkleTree},
    fuchsia_pkg_testing::{serve::ServedRepository, Package, PackageBuilder, Repository},
    fuchsia_url::{PinnedAbsolutePackageUrl, RepositoryUrl},
    fuchsia_zircon::{self as zx, Status},
    futures::prelude::*,
    mock_boot_arguments::MockBootArgumentsService,
    mock_metrics::MockMetricEventLoggerFactory,
    parking_lot::Mutex,
    serde::Serialize,
    std::{
        collections::HashMap,
        convert::TryInto,
        fs::File,
        io::{self, BufWriter, Write},
        path::{Path, PathBuf},
        sync::Arc,
        time::Duration,
    },
    tempfile::TempDir,
    vfs::directory::{entry::DirectoryEntry as _, helper::DirectlyMutable as _},
};

// If the body of an https response is not large enough, hyper will download the body
// along with the header in the initial fuchsia_hyper::HttpsClient.request(). This means
// that even if the body is implemented with a stream that sends some bytes and then fails
// before the transfer is complete, the error will occur on the initial request instead
// of when looping over the Response body bytes.
// This value probably just needs to be larger than the Hyper buffer, which defaults to 400 kB
// https://docs.rs/hyper/0.13.10/hyper/client/struct.Builder.html#method.http1_max_buf_size
pub const FILE_SIZE_LARGE_ENOUGH_TO_TRIGGER_HYPER_BATCHING: usize = 600_000;

static PKG_RESOLVER_CHILD_NAME: &str = "pkg_resolver";

pub mod mock_filesystem;

pub trait Blobfs {
    fn root_dir_handle(&self) -> ClientEnd<fio::DirectoryMarker>;
}

impl Blobfs for BlobfsRamdisk {
    fn root_dir_handle(&self) -> ClientEnd<fio::DirectoryMarker> {
        self.root_dir_handle().unwrap()
    }
}

pub struct Mounts {
    pkg_resolver_data: DirOrProxy,
    pkg_resolver_config_data: DirOrProxy,
}

#[derive(Serialize)]
pub struct EnableDynamicConfig {
    pub enable_dynamic_configuration: bool,
}

#[derive(Serialize)]
pub struct PersistedReposConfig {
    pub persisted_repos_dir: String,
}

#[derive(Default)]
pub struct MountsBuilder {
    pkg_resolver_data: Option<DirOrProxy>,
    pkg_resolver_config_data: Option<DirOrProxy>,
    enable_dynamic_config: Option<EnableDynamicConfig>,
    static_repository: Option<RepositoryConfig>,
    dynamic_rewrite_rules: Option<RuleConfig>,
    dynamic_repositories: Option<RepositoryConfigs>,
    custom_config_data: Vec<(PathBuf, String)>,
    persisted_repos_config: Option<PersistedReposConfig>,
    eager_packages: Vec<(PinnedAbsolutePackageUrl, pkg::CupData)>,
}

impl MountsBuilder {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn pkg_resolver_data(mut self, pkg_resolver_data: DirOrProxy) -> Self {
        self.pkg_resolver_data = Some(pkg_resolver_data);
        self
    }
    pub fn pkg_resolver_config_data(mut self, pkg_resolver_config_data: DirOrProxy) -> Self {
        self.pkg_resolver_config_data = Some(pkg_resolver_config_data);
        self
    }
    pub fn enable_dynamic_config(mut self, config: EnableDynamicConfig) -> Self {
        self.enable_dynamic_config = Some(config);
        self
    }
    pub fn persisted_repos_config(mut self, config: PersistedReposConfig) -> Self {
        self.persisted_repos_config = Some(config);
        self
    }
    pub fn static_repository(mut self, static_repository: RepositoryConfig) -> Self {
        self.static_repository = Some(static_repository);
        self
    }
    pub fn dynamic_rewrite_rules(mut self, dynamic_rewrite_rules: RuleConfig) -> Self {
        self.dynamic_rewrite_rules = Some(dynamic_rewrite_rules);
        self
    }
    pub fn dynamic_repositories(mut self, dynamic_repositories: RepositoryConfigs) -> Self {
        self.dynamic_repositories = Some(dynamic_repositories);
        self
    }
    /// Injects a file with custom contents into /config/data. Panics if file already exists.
    pub fn custom_config_data(mut self, path: impl Into<PathBuf>, data: impl Into<String>) -> Self {
        self.custom_config_data.push((path.into(), data.into()));
        self
    }
    pub fn eager_packages(
        mut self,
        package_urls: Vec<(PinnedAbsolutePackageUrl, pkg::CupData)>,
    ) -> Self {
        assert!(self.eager_packages.is_empty());
        self.eager_packages = package_urls;
        self
    }

    pub fn build(self) -> Mounts {
        let mounts = Mounts {
            pkg_resolver_data: self
                .pkg_resolver_data
                .unwrap_or_else(|| DirOrProxy::Dir(tempfile::tempdir().expect("/tmp to exist"))),
            pkg_resolver_config_data: self
                .pkg_resolver_config_data
                .unwrap_or_else(|| DirOrProxy::Dir(tempfile::tempdir().expect("/tmp to exist"))),
        };
        if let Some(config) = self.enable_dynamic_config {
            mounts.add_enable_dynamic_config(&config);
        }
        if let Some(config) = self.persisted_repos_config {
            mounts.add_persisted_repos_config(&config);
        }
        if let Some(config) = self.static_repository {
            mounts.add_static_repository(config);
        }
        if let Some(config) = self.dynamic_rewrite_rules {
            mounts.add_dynamic_rewrite_rules(&config);
        }
        if let Some(config) = self.dynamic_repositories {
            mounts.add_dynamic_repositories(&config);
        }
        for (path, data) in self.custom_config_data {
            mounts.add_custom_config_data(path, data);
        }
        if !self.eager_packages.is_empty() {
            mounts.add_eager_packages(self.eager_packages);
        }
        mounts
    }
}

impl Mounts {
    fn add_enable_dynamic_config(&self, config: &EnableDynamicConfig) {
        if let DirOrProxy::Dir(ref d) = self.pkg_resolver_config_data {
            let mut f = BufWriter::new(File::create(d.path().join("config.json")).unwrap());
            serde_json::to_writer(&mut f, &config).unwrap();
            f.flush().unwrap();
        } else {
            panic!("not supported");
        }
    }

    fn add_persisted_repos_config(&self, config: &PersistedReposConfig) {
        if let DirOrProxy::Dir(ref d) = self.pkg_resolver_config_data {
            let mut f =
                BufWriter::new(File::create(d.path().join("persisted_repos_dir.json")).unwrap());
            serde_json::to_writer(&mut f, &config).unwrap();
            f.flush().unwrap();
        } else {
            panic!("not supported");
        }
    }

    fn add_static_repository(&self, config: RepositoryConfig) {
        if let DirOrProxy::Dir(ref d) = self.pkg_resolver_config_data {
            let static_repo_path = d.path().join("repositories");
            if !static_repo_path.exists() {
                std::fs::create_dir(&static_repo_path).unwrap();
            }
            let mut f = BufWriter::new(
                File::create(static_repo_path.join(format!("{}.json", config.repo_url().host())))
                    .unwrap(),
            );
            serde_json::to_writer(&mut f, &RepositoryConfigs::Version1(vec![config])).unwrap();
            f.flush().unwrap();
        } else {
            panic!("not supported");
        }
    }

    fn add_dynamic_rewrite_rules(&self, rule_config: &RuleConfig) {
        if let DirOrProxy::Dir(ref d) = self.pkg_resolver_data {
            let mut f = BufWriter::new(File::create(d.path().join("rewrites.json")).unwrap());
            serde_json::to_writer(&mut f, rule_config).unwrap();
            f.flush().unwrap();
        } else {
            panic!("not supported");
        }
    }
    fn add_dynamic_repositories(&self, repo_configs: &RepositoryConfigs) {
        if let DirOrProxy::Dir(ref d) = self.pkg_resolver_data {
            let mut f = BufWriter::new(File::create(d.path().join("repositories.json")).unwrap());
            serde_json::to_writer(&mut f, repo_configs).unwrap();
            f.flush().unwrap();
        } else {
            panic!("not supported");
        }
    }

    fn add_custom_config_data(&self, path: impl AsRef<Path>, data: String) {
        if let DirOrProxy::Dir(ref d) = self.pkg_resolver_config_data {
            let path = d.path().join(path);
            assert!(!path.exists());
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, data).unwrap();
        } else {
            panic!("not supported");
        }
    }

    fn add_eager_packages(&self, packages: Vec<(PinnedAbsolutePackageUrl, pkg::CupData)>) {
        if let DirOrProxy::Dir(ref d) = self.pkg_resolver_data {
            let mut f = BufWriter::new(File::create(d.path().join("eager_packages.pf")).unwrap());

            let packages = PersistentEagerPackages {
                packages: Some(
                    packages
                        .into_iter()
                        .map(|(url, cup)| {
                            let pkg_url = fpkg::PackageUrl { url: url.as_unpinned().to_string() };
                            PersistentEagerPackage {
                                url: Some(pkg_url),
                                cup: Some(cup.into()),
                                ..Default::default()
                            }
                        })
                        .collect(),
                ),
                ..Default::default()
            };

            let data = persist(&packages).unwrap();
            f.write_all(&data).unwrap();
            f.flush().unwrap();
        } else {
            panic!("not supported");
        }
    }
}

pub enum DirOrProxy {
    Dir(TempDir),
    Proxy(fio::DirectoryProxy),
}

impl DirOrProxy {
    fn to_proxy(&self, rights: fio::OpenFlags) -> fio::DirectoryProxy {
        match &self {
            DirOrProxy::Dir(d) => {
                fuchsia_fs::directory::open_in_namespace(d.path().to_str().unwrap(), rights)
                    .unwrap()
            }
            DirOrProxy::Proxy(p) => clone_directory_proxy(p, rights),
        }
    }
}

pub fn clone_directory_proxy(
    proxy: &fio::DirectoryProxy,
    rights: fio::OpenFlags,
) -> fio::DirectoryProxy {
    let (client, server) = fidl::endpoints::create_endpoints();
    proxy.clone(rights, server).unwrap();
    ClientEnd::<fio::DirectoryMarker>::new(client.into_channel()).into_proxy().unwrap()
}

pub struct TestEnvBuilder<BlobfsAndSystemImageFut, MountsFn> {
    blobfs_and_system_image: BlobfsAndSystemImageFut,
    mounts: MountsFn,
    local_mirror_repo: Option<(Arc<Repository>, RepositoryUrl)>,
    fetch_delivery_blob: Option<bool>,
    delivery_blob_fallback: Option<bool>,
    allow_local_mirror: Option<bool>,
    tuf_metadata_timeout_seconds: Option<u32>,
    blob_network_header_timeout_seconds: Option<u32>,
    blob_network_body_timeout_seconds: Option<u32>,
    blob_download_resumption_attempts_limit: Option<u32>,
}

impl TestEnvBuilder<future::BoxFuture<'static, (BlobfsRamdisk, Option<Hash>)>, fn() -> Mounts> {
    #![allow(clippy::new_without_default)]
    pub fn new() -> Self {
        Self {
            blobfs_and_system_image: async {
                let system_image_package =
                    fuchsia_pkg_testing::SystemImageBuilder::new().build().await;
                let blobfs = BlobfsRamdisk::start().await.unwrap();
                let () = system_image_package.write_to_blobfs_dir(&blobfs.root_dir().unwrap());
                (blobfs, Some(*system_image_package.meta_far_merkle_root()))
            }
            .boxed(),
            // If it's not overridden, the default state of the mounts allows for dynamic
            // configuration. We do this because in the majority of tests, we'll want to use
            // dynamic repos and rewrite rules.
            // Note: this means that we'll produce different envs from
            // TestEnvBuilder::new().build().await
            // vs TestEnvBuilder::new().mounts(MountsBuilder::new().build()).build()
            mounts: || {
                MountsBuilder::new()
                    .enable_dynamic_config(EnableDynamicConfig {
                        enable_dynamic_configuration: true,
                    })
                    .build()
            },
            local_mirror_repo: None,
            fetch_delivery_blob: None,
            delivery_blob_fallback: None,
            allow_local_mirror: None,
            tuf_metadata_timeout_seconds: None,
            blob_network_header_timeout_seconds: None,
            blob_network_body_timeout_seconds: None,
            blob_download_resumption_attempts_limit: None,
        }
    }
}

impl<BlobfsAndSystemImageFut, ConcreteBlobfs, MountsFn>
    TestEnvBuilder<BlobfsAndSystemImageFut, MountsFn>
where
    BlobfsAndSystemImageFut: Future<Output = (ConcreteBlobfs, Option<Hash>)>,
    ConcreteBlobfs: Blobfs,
    MountsFn: FnOnce() -> Mounts,
{
    pub fn blobfs_and_system_image_hash<OtherBlobfs>(
        self,
        blobfs: OtherBlobfs,
        system_image: Option<Hash>,
    ) -> TestEnvBuilder<future::Ready<(OtherBlobfs, Option<Hash>)>, MountsFn>
    where
        OtherBlobfs: Blobfs,
    {
        TestEnvBuilder::<_, MountsFn> {
            blobfs_and_system_image: future::ready((blobfs, system_image)),
            mounts: self.mounts,
            local_mirror_repo: self.local_mirror_repo,
            fetch_delivery_blob: self.fetch_delivery_blob,
            delivery_blob_fallback: self.delivery_blob_fallback,
            allow_local_mirror: self.allow_local_mirror,
            tuf_metadata_timeout_seconds: self.tuf_metadata_timeout_seconds,
            blob_network_header_timeout_seconds: self.blob_network_header_timeout_seconds,
            blob_network_body_timeout_seconds: self.blob_network_body_timeout_seconds,
            blob_download_resumption_attempts_limit: self.blob_download_resumption_attempts_limit,
        }
    }

    /// Creates a BlobfsRamdisk loaded with the supplied packages and configures the system to use
    /// the supplied `system_image` package.
    pub async fn system_image_and_extra_packages(
        self,
        system_image: &Package,
        extra_packages: &[&Package],
    ) -> TestEnvBuilder<future::Ready<(BlobfsRamdisk, Option<Hash>)>, MountsFn> {
        let blobfs = BlobfsRamdisk::start().await.unwrap();
        let root_dir = blobfs.root_dir().unwrap();
        let () = system_image.write_to_blobfs_dir(&root_dir);
        for pkg in extra_packages {
            let () = pkg.write_to_blobfs_dir(&root_dir);
        }

        TestEnvBuilder::<_, MountsFn> {
            blobfs_and_system_image: future::ready((
                blobfs,
                Some(*system_image.meta_far_merkle_root()),
            )),
            mounts: self.mounts,
            local_mirror_repo: self.local_mirror_repo,
            fetch_delivery_blob: self.fetch_delivery_blob,
            delivery_blob_fallback: self.delivery_blob_fallback,
            allow_local_mirror: self.allow_local_mirror,
            tuf_metadata_timeout_seconds: self.tuf_metadata_timeout_seconds,
            blob_network_header_timeout_seconds: self.blob_network_header_timeout_seconds,
            blob_network_body_timeout_seconds: self.blob_network_body_timeout_seconds,
            blob_download_resumption_attempts_limit: self.blob_download_resumption_attempts_limit,
        }
    }

    pub fn mounts(
        self,
        mounts: Mounts,
    ) -> TestEnvBuilder<BlobfsAndSystemImageFut, impl FnOnce() -> Mounts> {
        TestEnvBuilder::<_, _> {
            blobfs_and_system_image: self.blobfs_and_system_image,
            mounts: || mounts,
            local_mirror_repo: self.local_mirror_repo,
            fetch_delivery_blob: self.fetch_delivery_blob,
            delivery_blob_fallback: self.delivery_blob_fallback,
            allow_local_mirror: self.allow_local_mirror,
            tuf_metadata_timeout_seconds: self.tuf_metadata_timeout_seconds,
            blob_network_header_timeout_seconds: self.blob_network_header_timeout_seconds,
            blob_network_body_timeout_seconds: self.blob_network_body_timeout_seconds,
            blob_download_resumption_attempts_limit: self.blob_download_resumption_attempts_limit,
        }
    }

    pub fn local_mirror_repo(mut self, repo: &Arc<Repository>, hostname: RepositoryUrl) -> Self {
        self.local_mirror_repo = Some((repo.clone(), hostname));
        self
    }

    pub fn allow_local_mirror(mut self, allow: bool) -> Self {
        assert_eq!(self.allow_local_mirror, None);
        self.allow_local_mirror = Some(allow);
        self
    }

    pub fn tuf_metadata_timeout_seconds(mut self, seconds: u32) -> Self {
        assert_eq!(self.tuf_metadata_timeout_seconds, None);
        self.tuf_metadata_timeout_seconds = Some(seconds);
        self
    }

    pub fn blob_network_header_timeout_seconds(mut self, seconds: u32) -> Self {
        assert_eq!(self.blob_network_header_timeout_seconds, None);
        self.blob_network_header_timeout_seconds = Some(seconds);
        self
    }

    pub fn blob_network_body_timeout_seconds(mut self, seconds: u32) -> Self {
        assert_eq!(self.blob_network_body_timeout_seconds, None);
        self.blob_network_body_timeout_seconds = Some(seconds);
        self
    }

    pub fn blob_download_resumption_attempts_limit(mut self, limit: u32) -> Self {
        assert_eq!(self.blob_download_resumption_attempts_limit, None);
        self.blob_download_resumption_attempts_limit = Some(limit);
        self
    }

    pub fn fetch_delivery_blob(mut self, fetch_delivery_blob: bool) -> Self {
        assert_eq!(self.fetch_delivery_blob, None);
        self.fetch_delivery_blob = Some(fetch_delivery_blob);
        self
    }

    pub fn delivery_blob_fallback(mut self, delivery_blob_fallback: bool) -> Self {
        assert_eq!(self.delivery_blob_fallback, None);
        self.delivery_blob_fallback = Some(delivery_blob_fallback);
        self
    }

    pub async fn build(self) -> TestEnv<ConcreteBlobfs> {
        let (blobfs, system_image) = self.blobfs_and_system_image.await;
        let mounts = (self.mounts)();

        let local_child_svc_dir = vfs::pseudo_directory! {};

        let mut boot_arguments_service = MockBootArgumentsService::new(HashMap::new());
        if let Some(hash) = &system_image {
            boot_arguments_service.insert_pkgfs_boot_arg(*hash)
        }
        let boot_arguments_service = Arc::new(boot_arguments_service);
        local_child_svc_dir
            .add_entry(
                fboot::ArgumentsMarker::PROTOCOL_NAME,
                vfs::service::host(move |stream| {
                    Arc::clone(&boot_arguments_service).handle_request_stream(stream)
                }),
            )
            .unwrap();

        let logger_factory = Arc::new(MockMetricEventLoggerFactory::new());
        let logger_factory_clone = Arc::clone(&logger_factory);
        local_child_svc_dir
            .add_entry(
                fmetrics::MetricEventLoggerFactoryMarker::PROTOCOL_NAME,
                vfs::service::host(move |stream| {
                    Arc::clone(&logger_factory_clone).run_logger_factory(stream)
                }),
            )
            .unwrap();

        let local_child_out_dir = vfs::pseudo_directory! {
            "blob" => vfs::remote::remote_dir(
                blobfs.root_dir_handle().into_proxy().unwrap()
            ),
            "data" => vfs::remote::remote_dir(
                mounts.pkg_resolver_data.to_proxy(fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE)
            ),
            "config" => vfs::pseudo_directory! {
                "data" => vfs::remote::remote_dir(
                    mounts.pkg_resolver_config_data.to_proxy(fio::OpenFlags::RIGHT_READABLE)
                ),
                "ssl" => vfs::remote::remote_dir(
                    fuchsia_fs::directory::open_in_namespace(
                        "/pkg/data/ssl",
                        fio::OpenFlags::RIGHT_READABLE
                    ).unwrap()
                ),
            },
            "svc" => local_child_svc_dir,
        };

        let local_mirror_dir = tempfile::tempdir().unwrap();
        if let Some((repo, url)) = self.local_mirror_repo {
            let proxy = fuchsia_fs::directory::open_in_namespace(
                local_mirror_dir.path().to_str().unwrap(),
                fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE,
            )
            .unwrap();
            let () = repo.copy_local_repository_to_dir(&proxy, &url).await;
            let () = local_child_out_dir
                .add_entry(
                    "usb",
                    vfs::pseudo_directory! {
                        "0" => vfs::pseudo_directory! {
                            "fuchsia_pkg" => vfs::remote::remote_dir(proxy),
                        },
                    },
                )
                .unwrap();
        }

        let local_child_out_dir = Mutex::new(Some(local_child_out_dir));

        let builder = RealmBuilder::new().await.unwrap();
        let pkg_cache = builder
            .add_child("pkg_cache", "#meta/pkg-cache.cm", ChildOptions::new())
            .await
            .unwrap();
        let system_update_committer = builder
            .add_child(
                "system_update_committer",
                "#meta/system-update-committer.cm",
                ChildOptions::new(),
            )
            .await
            .unwrap();
        let service_reflector = builder
            .add_local_child(
                "service_reflector",
                move |handles| {
                    let local_child_out_dir = local_child_out_dir
                        .lock()
                        .take()
                        .expect("mock component should only be launched once");
                    let scope = vfs::execution_scope::ExecutionScope::new();
                    let () = local_child_out_dir.open(
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
        let local_mirror = builder
            .add_child("local_mirror", "#meta/pkg-local-mirror.cm", ChildOptions::new())
            .await
            .unwrap();

        let pkg_resolver = builder
            .add_child(PKG_RESOLVER_CHILD_NAME, "#meta/pkg-resolver.cm", ChildOptions::new())
            .await
            .unwrap();

        if self.fetch_delivery_blob.is_some()
            || self.delivery_blob_fallback.is_some()
            || self.allow_local_mirror.is_some()
            || self.tuf_metadata_timeout_seconds.is_some()
            || self.blob_network_header_timeout_seconds.is_some()
            || self.blob_network_body_timeout_seconds.is_some()
            || self.blob_download_resumption_attempts_limit.is_some()
        {
            builder.init_mutable_config_from_package(&pkg_resolver).await.unwrap();
            if let Some(fetch_delivery_blob) = self.fetch_delivery_blob {
                builder
                    .set_config_value_bool(
                        &pkg_resolver,
                        "fetch_delivery_blob",
                        fetch_delivery_blob,
                    )
                    .await
                    .unwrap();
            }
            if let Some(delivery_blob_fallback) = self.delivery_blob_fallback {
                builder
                    .set_config_value_bool(
                        &pkg_resolver,
                        "delivery_blob_fallback",
                        delivery_blob_fallback,
                    )
                    .await
                    .unwrap();
            }
            if let Some(allow_local_mirror) = self.allow_local_mirror {
                builder
                    .set_config_value_bool(&pkg_resolver, "allow_local_mirror", allow_local_mirror)
                    .await
                    .unwrap();
            }
            if let Some(tuf_metadata_timeout_seconds) = self.tuf_metadata_timeout_seconds {
                builder
                    .set_config_value_uint32(
                        &pkg_resolver,
                        "tuf_metadata_timeout_seconds",
                        tuf_metadata_timeout_seconds,
                    )
                    .await
                    .unwrap();
            }
            if let Some(blob_network_header_timeout_seconds) =
                self.blob_network_header_timeout_seconds
            {
                builder
                    .set_config_value_uint32(
                        &pkg_resolver,
                        "blob_network_header_timeout_seconds",
                        blob_network_header_timeout_seconds,
                    )
                    .await
                    .unwrap();
            }
            if let Some(blob_network_body_timeout_seconds) = self.blob_network_body_timeout_seconds
            {
                builder
                    .set_config_value_uint32(
                        &pkg_resolver,
                        "blob_network_body_timeout_seconds",
                        blob_network_body_timeout_seconds,
                    )
                    .await
                    .unwrap();
            }
            if let Some(blob_download_resumption_attempts_limit) =
                self.blob_download_resumption_attempts_limit
            {
                builder
                    .set_config_value_uint32(
                        &pkg_resolver,
                        "blob_download_resumption_attempts_limit",
                        blob_download_resumption_attempts_limit,
                    )
                    .await
                    .unwrap();
            }
        }

        if system_image.is_none() {
            let () = builder.init_mutable_config_from_package(&pkg_cache).await.unwrap();
            let () =
                builder.set_config_value_bool(&pkg_cache, "use_system_image", false).await.unwrap();
        }

        builder
            .add_route(
                Route::new()
                    .capability(Capability::protocol::<fidl_fuchsia_logger::LogSinkMarker>())
                    .from(Ref::parent())
                    .to(&pkg_cache)
                    .to(&system_update_committer)
                    .to(&local_mirror)
                    .to(&pkg_resolver),
            )
            .await
            .unwrap();

        // Make sure pkg_resolver has network access as required by the hyper client shard
        builder
            .add_route(
                Route::new()
                    .capability(Capability::protocol::<fidl_fuchsia_posix_socket::ProviderMarker>())
                    .capability(Capability::protocol::<fidl_fuchsia_net_name::LookupMarker>())
                    .from(Ref::parent())
                    .to(&pkg_resolver),
            )
            .await
            .unwrap();

        // Fill out the rest of the `use` stanzas for pkg_resolver and pkg_cache
        builder
            .add_route(
                Route::new()
                    .capability(Capability::protocol::<fmetrics::MetricEventLoggerFactoryMarker>())
                    .capability(
                        Capability::protocol::<fidl_fuchsia_tracing_provider::RegistryMarker>(),
                    )
                    .from(&service_reflector)
                    .to(&pkg_cache)
                    .to(&pkg_resolver),
            )
            .await
            .unwrap();
        builder
            .add_route(
                Route::new()
                    .capability(Capability::protocol::<fpkg::LocalMirrorMarker>())
                    .from(&local_mirror)
                    .to(&pkg_resolver),
            )
            .await
            .unwrap();
        builder
            .add_route(
                Route::new()
                    .capability(Capability::protocol::<fpkg::PackageCacheMarker>())
                    .from(&pkg_cache)
                    .to(&pkg_resolver)
                    .to(Ref::parent()),
            )
            .await
            .unwrap();

        builder
            .add_route(
                Route::new()
                    .capability(Capability::protocol::<fpkg::PackageResolverMarker>())
                    .capability(Capability::protocol::<fpkg::RepositoryManagerMarker>())
                    .capability(Capability::protocol::<fpkg_rewrite::EngineMarker>())
                    .capability(Capability::protocol::<fpkg::CupMarker>())
                    .from(&pkg_resolver)
                    .to(Ref::parent()),
            )
            .await
            .unwrap();

        builder
            .add_route(
                Route::new()
                    .capability(Capability::protocol::<fsys2::LifecycleControllerMarker>())
                    .from(Ref::framework())
                    .to(Ref::parent()),
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
                    .from(&service_reflector)
                    .to(&pkg_cache),
            )
            .await
            .unwrap();

        builder
            .add_route(
                Route::new()
                    .capability(Capability::directory("usb").path("/usb").rights(fio::RW_STAR_DIR))
                    .from(&service_reflector)
                    .to(&local_mirror),
            )
            .await
            .unwrap();

        builder
            .add_route(
                Route::new()
                    .capability(
                        Capability::directory("config-data")
                            .path("/config/data")
                            .rights(fio::R_STAR_DIR),
                    )
                    .capability(
                        Capability::directory("root-ssl-certificates")
                            .path("/config/ssl")
                            .rights(fio::R_STAR_DIR),
                    )
                    // TODO(fxbug.dev/75658): Change to storage once convenient.
                    .capability(
                        Capability::directory("data").path("/data").rights(fio::RW_STAR_DIR),
                    )
                    .from(&service_reflector)
                    .to(&pkg_resolver),
            )
            .await
            .unwrap();

        let realm_instance = builder.build().await.unwrap();

        TestEnv {
            blobfs,
            proxies: Proxies::from_instance(&realm_instance.root),
            realm_instance,
            _mounts: mounts,
            mocks: Mocks { logger_factory },
            local_mirror_dir,
        }
    }
}

pub struct Proxies {
    pub resolver: PackageResolverProxy,
    pub repo_manager: RepositoryManagerProxy,
    pub rewrite_engine: fpkg_rewrite::EngineProxy,
    pub cup: CupProxy,
}

impl Proxies {
    fn from_instance(realm: &ScopedInstance) -> Proxies {
        Proxies {
            resolver: realm
                .connect_to_protocol_at_exposed_dir::<PackageResolverMarker>()
                .expect("connect to package resolver"),
            repo_manager: realm
                .connect_to_protocol_at_exposed_dir::<RepositoryManagerMarker>()
                .expect("connect to repository manager"),
            rewrite_engine: realm
                .connect_to_protocol_at_exposed_dir::<fpkg_rewrite::EngineMarker>()
                .expect("connect to rewrite engine"),
            cup: realm.connect_to_protocol_at_exposed_dir::<CupMarker>().expect("connect to cup"),
        }
    }
}

pub struct Mocks {
    pub logger_factory: Arc<MockMetricEventLoggerFactory>,
}

pub struct TestEnv<B = BlobfsRamdisk> {
    pub blobfs: B,
    pub realm_instance: RealmInstance,
    pub proxies: Proxies,
    pub _mounts: Mounts,
    pub mocks: Mocks,
    pub local_mirror_dir: TempDir,
}

impl TestEnv<BlobfsRamdisk> {
    pub fn add_slice_to_blobfs(&self, slice: &[u8]) {
        let merkle = MerkleTree::from_reader(slice).expect("merkle slice").root().to_string();
        let mut blob = self
            .blobfs
            .root_dir()
            .expect("blobfs has root dir")
            .write_file(merkle, 0)
            .expect("create file in blobfs");
        blob.set_len(slice.len() as u64).expect("set_len");
        io::copy(&mut &slice[..], &mut blob).expect("copy from slice to blob");
    }

    pub fn add_file_with_hash_to_blobfs(&self, mut file: File, hash: &Hash) {
        let mut blob = self
            .blobfs
            .root_dir()
            .expect("blobfs has root dir")
            .write_file(hash.to_string(), 0)
            .expect("create file in blobfs");
        blob.set_len(file.metadata().expect("file has metadata").len()).expect("set_len");
        io::copy(&mut file, &mut blob).expect("copy file to blobfs");
    }

    pub async fn stop(self) {
        // Tear down the environment in reverse order, ending with the storage.
        drop(self.proxies);
        drop(self.realm_instance);
        self.blobfs.stop().await.expect("blobfs to stop gracefully");
    }
}

impl<B: Blobfs> TestEnv<B> {
    pub async fn register_repo(&self, repo: &ServedRepository) {
        self.register_repo_at_url(repo, "fuchsia-pkg://test").await;
    }

    pub async fn register_repo_at_url<R>(&self, repo: &ServedRepository, url: R)
    where
        R: TryInto<RepositoryUrl>,
        R::Error: std::fmt::Debug,
    {
        let repo_config = repo.make_repo_config(url.try_into().unwrap());
        let () = self.proxies.repo_manager.add(&repo_config.into()).await.unwrap().unwrap();
    }

    pub async fn restart_pkg_resolver(&mut self) {
        let lifecycle_controller = self
            .realm_instance
            .root
            .connect_to_protocol_at_exposed_dir::<fsys2::LifecycleControllerMarker>()
            .unwrap();
        let () = lifecycle_controller
            .stop_instance(&format!("./{PKG_RESOLVER_CHILD_NAME}"))
            .await
            .unwrap()
            .unwrap();
        let (_, binder_server) = fidl::endpoints::create_endpoints();
        lifecycle_controller
            .start_instance(&format!("./{PKG_RESOLVER_CHILD_NAME}"), binder_server)
            .await
            .unwrap()
            .unwrap();
        self.proxies = Proxies::from_instance(&self.realm_instance.root);
        self.wait_for_pkg_resolver_to_start().await;
    }

    pub async fn wait_for_pkg_resolver_to_start(&self) {
        self.proxies
            .rewrite_engine
            .test_apply("fuchsia-pkg://test.com/name")
            .await
            .expect("fidl call succeeds")
            .expect("test apply result is ok");
    }

    /// Obtain a new proxy, different than self.proxies.resolver, to issue concurrent requests.
    pub fn connect_to_resolver(&self) -> PackageResolverProxy {
        self.realm_instance
            .root
            .connect_to_protocol_at_exposed_dir::<PackageResolverMarker>()
            .expect("connect to package resolver")
    }

    pub fn resolve_package(
        &self,
        url: &str,
    ) -> impl Future<
        Output = Result<
            (fio::DirectoryProxy, pkg::ResolutionContext),
            fidl_fuchsia_pkg::ResolveError,
        >,
    > {
        resolve_package(&self.proxies.resolver, url)
    }

    pub fn resolve_with_context(
        &self,
        url: &str,
        context: pkg::ResolutionContext,
    ) -> impl Future<
        Output = Result<
            (fio::DirectoryProxy, pkg::ResolutionContext),
            fidl_fuchsia_pkg::ResolveError,
        >,
    > {
        resolve_with_context(&self.proxies.resolver, url, context)
    }

    pub fn get_hash(
        &self,
        url: impl Into<String>,
    ) -> impl Future<Output = Result<pkg::BlobId, Status>> {
        let fut = self.proxies.resolver.get_hash(&fpkg::PackageUrl { url: url.into() });
        async move { fut.await.unwrap().map(|blob_id| blob_id.into()).map_err(Status::from_raw) }
    }

    pub async fn open_cached_package(
        &self,
        hash: pkg::BlobId,
    ) -> Result<fio::DirectoryProxy, zx::Status> {
        let cache_service = self
            .realm_instance
            .root
            .connect_to_protocol_at_exposed_dir::<PackageCacheMarker>()
            .unwrap();
        let (proxy, server_end) = fidl::endpoints::create_proxy().unwrap();
        let () = cache_service
            .open(&hash.into(), server_end)
            .await
            .unwrap()
            .map_err(zx::Status::from_raw)?;
        Ok(proxy)
    }

    pub async fn pkg_resolver_inspect_hierarchy(&self) -> DiagnosticsHierarchy {
        let data = ArchiveReader::new()
            .add_selector(ComponentSelector::new(vec![
                format!("realm_builder\\:{}", self.realm_instance.root.child_name()),
                PKG_RESOLVER_CHILD_NAME.into(),
            ]))
            .snapshot::<Inspect>()
            .await
            .expect("read inspect hierarchy")
            .into_iter()
            .next()
            .expect("one result");

        if data.payload.is_none() {
            tracing::error!(?data, "Unexpected empty payload");
        }

        data.payload.unwrap()
    }

    /// Wait until pkg-resolver inspect state satisfies `desired_state`.
    pub async fn wait_for_pkg_resolver_inspect_state(&self, desired_state: TreeAssertion<String>) {
        while desired_state.run(&self.pkg_resolver_inspect_hierarchy().await).is_err() {
            fasync::Timer::new(Duration::from_millis(10)).await;
        }
    }
    /// Wait until at least `expected_event_codes.len()` events of metric id `expected_metric_id`
    /// are received, then assert that the event codes of the received events correspond, in order,
    /// to the event codes in `expected_event_codes`.
    pub async fn assert_count_events(
        &self,
        expected_metric_id: u32,
        expected_event_codes: Vec<impl AsEventCodes>,
    ) {
        let actual_events = self
            .mocks
            .logger_factory
            .wait_for_at_least_n_events_with_metric_id(
                expected_event_codes.len(),
                expected_metric_id,
            )
            .await;
        assert_eq!(
            actual_events.len(),
            expected_event_codes.len(),
            "event count different than expected, actual_events: {actual_events:?}"
        );

        for (event, expected_codes) in actual_events
            .into_iter()
            .zip(expected_event_codes.into_iter().map(|c| c.as_event_codes()))
        {
            assert_matches!(
                event,
                MetricEvent {
                    metric_id,
                    event_codes,
                    payload: MetricEventPayload::Count(1),
                } if metric_id == expected_metric_id && event_codes == expected_codes
            )
        }
    }

    pub async fn cup_write(
        &self,
        url: impl Into<String>,
        cup: pkg::CupData,
    ) -> Result<(), WriteError> {
        self.proxies.cup.write(&fpkg::PackageUrl { url: url.into() }, &cup.into()).await.unwrap()
    }

    pub async fn cup_get_info(
        &self,
        url: impl Into<String>,
    ) -> Result<(String, String), GetInfoError> {
        self.proxies.cup.get_info(&fpkg::PackageUrl { url: url.into() }).await.unwrap()
    }
}

pub const EMPTY_REPO_PATH: &str = "/pkg/empty-repo";

// The following functions generate unique test package dummy content. Callers are recommended
// to pass in the name of the test case.
pub fn test_package_bin(s: &str) -> Vec<u8> {
    return format!("!/boot/bin/sh\n{s}").as_bytes().to_owned();
}

pub fn test_package_cml(s: &str) -> Vec<u8> {
    return format!("{{program:{{runner:\"elf\",binary:\"bin/{s}\"}}}}").as_bytes().to_owned();
}

pub fn extra_blob_contents(s: &str, i: u32) -> Vec<u8> {
    format!("contents of file {s}-{i}").as_bytes().to_owned()
}

pub async fn make_pkg_with_extra_blobs(s: &str, n: u32) -> Package {
    let mut pkg = PackageBuilder::new(s)
        .add_resource_at(format!("bin/{s}"), &test_package_bin(s)[..])
        .add_resource_at(format!("meta/{s}.cml"), &test_package_cml(s)[..]);
    for i in 0..n {
        pkg = pkg.add_resource_at(format!("data/{s}-{i}"), extra_blob_contents(s, i).as_slice());
    }
    pkg.build().await.unwrap()
}

pub fn resolve_package(
    resolver: &PackageResolverProxy,
    url: &str,
) -> impl Future<
    Output = Result<(fio::DirectoryProxy, pkg::ResolutionContext), fidl_fuchsia_pkg::ResolveError>,
> {
    let (package, package_server_end) = fidl::endpoints::create_proxy().unwrap();
    let response_fut = resolver.resolve(url, package_server_end);
    async move {
        let resolved_context = response_fut.await.unwrap()?;
        Ok((package, (&resolved_context).try_into().unwrap()))
    }
}

pub fn resolve_with_context(
    resolver: &PackageResolverProxy,
    url: &str,
    context: pkg::ResolutionContext,
) -> impl Future<
    Output = Result<(fio::DirectoryProxy, pkg::ResolutionContext), fidl_fuchsia_pkg::ResolveError>,
> {
    let (package, package_server_end) = fidl::endpoints::create_proxy().unwrap();
    let response_fut = resolver.resolve_with_context(url, &context.into(), package_server_end);
    async move {
        let resolved_context = response_fut.await.unwrap()?;
        Ok((package, (&resolved_context).try_into().unwrap()))
    }
}

pub fn make_repo_config(repo: &RepositoryConfig) -> RepositoryConfigs {
    RepositoryConfigs::Version1(vec![repo.clone()])
}

pub fn make_repo() -> RepositoryConfig {
    RepositoryConfigBuilder::new("fuchsia-pkg://example.com".parse().unwrap()).build()
}

pub async fn get_repos(repository_manager: &RepositoryManagerProxy) -> Vec<RepositoryConfig> {
    let (repo_iterator, repo_iterator_server) =
        fidl::endpoints::create_proxy().expect("create repo iterator proxy");
    repository_manager.list(repo_iterator_server).expect("list repos");
    let mut ret = vec![];
    loop {
        let repos = repo_iterator.next().await.expect("advance repo iterator");
        if repos.is_empty() {
            return ret;
        }
        ret.extend(repos.into_iter().map(|r| r.try_into().unwrap()))
    }
}

pub async fn get_rules(rewrite_engine: &fpkg_rewrite::EngineProxy) -> Vec<Rule> {
    let (rule_iterator, rule_iterator_server) =
        fidl::endpoints::create_proxy().expect("create rule iterator proxy");
    rewrite_engine.list(rule_iterator_server).expect("list rules");
    let mut ret = vec![];
    loop {
        let rules = rule_iterator.next().await.expect("advance rule iterator");
        if rules.is_empty() {
            return ret;
        }
        ret.extend(rules.into_iter().map(|r| r.try_into().unwrap()))
    }
}

pub fn get_cup_response_with_name(package_url: &PinnedAbsolutePackageUrl) -> Vec<u8> {
    let response = serde_json::json!({"response":{
      "server": "prod",
      "protocol": "3.0",
      "app": [{
        "appid": "appid",
        "cohortname": "stable",
        "status": "ok",
        "updatecheck": {
          "status": "ok",
          "urls":{
            "url":[
                {"codebase": format!("{}/", package_url.repository()) },
            ]
          },
          "manifest": {
            "version": "1.2.3.4",
            "actions": {
              "action": [],
            },
            "packages": {
              "package": [
                {
                 "name": format!("{}?hash={}", package_url.name(), package_url.hash()),
                 "required": true,
                 "fp": "",
                }
              ],
            },
          }
        }
      }],
    }});
    serde_json::to_vec(&response).unwrap()
}
