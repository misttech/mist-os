// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use {
    anyhow::{anyhow, Context, Error},
    fidl_fuchsia_paver::Configuration,
    fidl_fuchsia_paver::{BootManagerMarker, PaverMarker, PaverProxy},
    fidl_fuchsia_update_installer::{InstallerMarker, InstallerProxy, RebootControllerMarker},
    fidl_fuchsia_update_installer_ext::{
        options::{Initiator, Options},
        start_update, UpdateAttempt,
    },
    fuchsia_zircon as zx,
    futures::prelude::*,
};

pub const DEFAULT_UPDATE_PACKAGE_URL: &str = "fuchsia-pkg://fuchsia.com/update";

pub struct Updater {
    proxy: InstallerProxy,
    paver_proxy: PaverProxy,
}

impl Updater {
    pub fn new_with_proxies(proxy: InstallerProxy, paver_proxy: PaverProxy) -> Self {
        Self { proxy, paver_proxy }
    }

    pub fn new() -> Result<Self, Error> {
        Ok(Self::new_with_proxies(
            fuchsia_component::client::connect_to_protocol::<InstallerMarker>()?,
            fuchsia_component::client::connect_to_protocol::<PaverMarker>()?,
        ))
    }

    /// Perform an update, skipping the final reboot.
    /// If `update_package` is Some, use the given package URL as the URL for the update package.
    /// Otherwise, `system-updater` uses the default URL.
    /// This will not install any images to the recovery partitions.
    pub async fn install_update(
        &mut self,
        update_package: Option<&fuchsia_url::AbsolutePackageUrl>,
    ) -> Result<(), Error> {
        let update_package = match update_package {
            Some(url) => url.to_owned(),
            None => DEFAULT_UPDATE_PACKAGE_URL.parse().unwrap(),
        };

        let (reboot_controller, reboot_controller_server_end) =
            fidl::endpoints::create_proxy::<RebootControllerMarker>()
                .context("creating reboot controller proxy")?;
        let () = reboot_controller.detach().context("disabling automatic reboot")?;

        let attempt = start_update(
            &update_package,
            Options {
                initiator: Initiator::User,
                allow_attach_to_existing_attempt: false,
                should_write_recovery: false,
            },
            &self.proxy,
            Some(reboot_controller_server_end),
        )
        .await
        .context("starting system update")?;

        let () = Self::monitor_update_attempt(attempt).await.context("monitoring installation")?;

        let () = Self::activate_installed_slot(&self.paver_proxy)
            .await
            .context("activating installed slot")?;

        Ok(())
    }

    async fn monitor_update_attempt(mut attempt: UpdateAttempt) -> Result<(), Error> {
        while let Some(state) = attempt.try_next().await.context("fetching next update state")? {
            tracing::info!("Install: {:?}", state);
            if state.is_success() {
                return Ok(());
            } else if state.is_failure() {
                return Err(anyhow!("update attempt failed in state {:?}", state));
            }
        }

        Err(anyhow!("unexpected end of update attempt"))
    }

    async fn activate_installed_slot(paver: &PaverProxy) -> Result<(), Error> {
        let (boot_manager, remote) = fidl::endpoints::create_proxy::<BootManagerMarker>()
            .context("Creating boot manager proxy")?;

        paver.find_boot_manager(remote).context("finding boot manager")?;

        let result = boot_manager.query_active_configuration().await;
        if let Err(fidl::Error::ClientChannelClosed { status: zx::Status::NOT_SUPPORTED, .. }) =
            result
        {
            // board does not actually support ABR, so return.
            tracing::info!("ABR not supported, not configuring slots.");
            return Ok(());
        }
        let result = result?;
        if result.is_ok() {
            // active slot is valid - assume that system-updater handled this for us.
            return Ok(());
        }

        // In recovery, the paver will return ZX_ERR_NOT_SUPPORTED to query_active_configuration(),
        // even on devices which support ABR. Handle this manually in case it is actually
        // supported.
        zx::Status::ok(
            boot_manager
                .set_configuration_active(Configuration::A)
                .await
                .context("Sending set active configuration request")?,
        )
        .context("Setting A to active configuration")?;
        Ok(())
    }
}

#[cfg(test)]
pub(crate) mod for_tests {
    use {
        super::*,
        crate::resolver::for_tests::{ResolverForTest, EMPTY_REPO_PATH},
        anyhow::Context,
        blobfs_ramdisk::BlobfsRamdisk,
        fidl_fuchsia_metrics as fmetrics,
        fidl_fuchsia_paver::PaverRequestStream,
        fuchsia_async as fasync,
        fuchsia_component_test::{
            Capability, ChildOptions, DirectoryContents, RealmBuilder, RealmInstance, Ref, Route,
        },
        fuchsia_merkle::Hash,
        fuchsia_pkg_testing::serve::ServedRepository,
        fuchsia_pkg_testing::{Package, PackageBuilder, RepositoryBuilder, SystemImageBuilder},
        mock_paver::{MockPaverService, MockPaverServiceBuilder, PaverEvent},
        std::collections::HashMap,
        std::sync::Arc,
    };

    pub const TEST_REPO_URL: &str = "fuchsia-pkg://fuchsia.com";
    pub struct UpdaterBuilder {
        paver_builder: MockPaverServiceBuilder,
        packages: Vec<Package>,
        images: HashMap<String, Vec<u8>>,
        repo_url: fuchsia_url::RepositoryUrl,
    }

    impl UpdaterBuilder {
        /// Construct a new UpdateBuilder. Initially, this contains no images and an empty system
        /// image package.
        pub async fn new() -> UpdaterBuilder {
            UpdaterBuilder {
                paver_builder: MockPaverServiceBuilder::new(),
                packages: vec![SystemImageBuilder::new().build().await],
                images: HashMap::new(),
                repo_url: TEST_REPO_URL.parse().unwrap(),
            }
        }

        /// Add a package to the update package this builder will generate.
        pub fn add_package(mut self, package: Package) -> Self {
            self.packages.push(package);
            self
        }

        /// Add an image to the update package this builder will generate.
        pub fn add_image(mut self, name: &str, contents: &[u8]) -> Self {
            self.images.insert(name.to_owned(), contents.to_owned());
            self
        }

        /// Mutate the `MockPaverServiceBuilder` contained in this UpdaterBuilder.
        pub fn paver<F>(mut self, f: F) -> Self
        where
            F: FnOnce(MockPaverServiceBuilder) -> MockPaverServiceBuilder,
        {
            self.paver_builder = f(self.paver_builder);
            self
        }

        pub fn repo_url(mut self, url: &str) -> Self {
            self.repo_url = url.parse().expect("Valid URL supplied to repo_url()");
            self
        }

        fn serve_mock_paver(stream: PaverRequestStream, paver: Arc<MockPaverService>) {
            let paver_clone = Arc::clone(&paver);
            fasync::Task::spawn(
                Arc::clone(&paver_clone)
                    .run_paver_service(stream)
                    .unwrap_or_else(|e| panic!("Failed to run mock paver: {e:?}")),
            )
            .detach();
        }

        async fn run_mock_paver(
            handles: fuchsia_component_test::LocalComponentHandles,
            paver: Arc<MockPaverService>,
        ) -> Result<(), Error> {
            let mut fs = fuchsia_component::server::ServiceFs::new();
            fs.dir("svc")
                .add_fidl_service(move |stream| Self::serve_mock_paver(stream, Arc::clone(&paver)));
            fs.serve_connection(handles.outgoing_dir)?;
            let () = fs.for_each_concurrent(None, |req| async move { req }).await;
            Ok(())
        }

        /// Create an UpdateForTest from this UpdaterBuilder.
        /// This will construct an update package containing all packages and images added to the
        /// builder, create a repository containing the packages, and create a MockPaver.
        pub async fn build(self) -> UpdaterForTest {
            let mut update = PackageBuilder::new("update").add_resource_at(
                "packages.json",
                generate_packages_json(&self.packages, &self.repo_url.to_string()).as_bytes(),
            );
            for (name, data) in self.images.iter() {
                update = update.add_resource_at(name, data.as_slice());
            }

            let update = update.build().await.expect("Building update package");

            let repo = Arc::new(
                self.packages
                    .iter()
                    .fold(
                        RepositoryBuilder::from_template_dir(EMPTY_REPO_PATH)
                            .add_package(&update)
                            .delivery_blob_type(1),
                        |repo, package| repo.add_package(package),
                    )
                    .build()
                    .await
                    .expect("Building repo"),
            );

            let realm_builder = RealmBuilder::new().await.unwrap();
            let blobfs = BlobfsRamdisk::start().await.context("starting blobfs").unwrap();

            let served_repo = Arc::new(Arc::clone(&repo).server().start().unwrap());

            let resolver_realm = ResolverForTest::realm_setup(
                &realm_builder,
                Arc::clone(&served_repo),
                self.repo_url.clone(),
                &blobfs,
            )
            .await
            .unwrap();

            let system_updater = realm_builder
                .add_child("system-updater", "#meta/system-updater.cm", ChildOptions::new())
                .await
                .unwrap();

            let service_reflector = realm_builder
                .add_local_child(
                    "system_updater_service_reflector",
                    move |handles| {
                        let mut fs = fuchsia_component::server::ServiceFs::new();
                        // Not necessary for updates, but without this system-updater will wait 30
                        // seconds trying to flush cobalt logs before logging an attempt error,
                        // and the test is torn down before then, so the error is lost. Also
                        // prevents spam of irrelevant error logs.
                        fs.dir("svc").add_fidl_service(move |stream| {
                            fasync::Task::spawn(
                                Arc::new(mock_metrics::MockMetricEventLoggerFactory::new())
                                    .run_logger_factory(stream),
                            )
                            .detach()
                        });
                        async move {
                            fs.serve_connection(handles.outgoing_dir).unwrap();
                            let () = fs.collect().await;
                            Ok(())
                        }
                        .boxed()
                    },
                    ChildOptions::new(),
                )
                .await
                .unwrap();

            realm_builder
                .add_route(
                    Route::new()
                        .capability(
                            Capability::protocol::<fmetrics::MetricEventLoggerFactoryMarker>(),
                        )
                        .from(&service_reflector)
                        .to(&system_updater),
                )
                .await
                .unwrap();

            // Set up paver and routes
            let paver = Arc::new(self.paver_builder.build());
            let paver_clone = Arc::clone(&paver);
            let mock_paver = realm_builder
                .add_local_child(
                    "paver",
                    move |handles| Box::pin(Self::run_mock_paver(handles, Arc::clone(&paver))),
                    ChildOptions::new(),
                )
                .await
                .unwrap();

            realm_builder
                .add_route(
                    Route::new()
                        .capability(Capability::protocol_by_name("fuchsia.paver.Paver"))
                        .from(&mock_paver)
                        .to(&system_updater),
                )
                .await
                .unwrap();
            realm_builder
                .add_route(
                    Route::new()
                        .capability(Capability::protocol_by_name("fuchsia.paver.Paver"))
                        .from(&mock_paver)
                        .to(Ref::parent()),
                )
                .await
                .unwrap();

            // Set up build-info and routes
            realm_builder
                .read_only_directory(
                    "build-info",
                    vec![&system_updater],
                    DirectoryContents::new().add_file("board", "test".as_bytes()),
                )
                .await
                .unwrap();

            // Set up pkg-resolver and pkg-cache routes
            realm_builder
                .add_route(
                    Route::new()
                        .capability(Capability::protocol_by_name("fuchsia.pkg.PackageResolver"))
                        .from(&resolver_realm.resolver)
                        .to(&system_updater),
                )
                .await
                .unwrap();

            realm_builder
                .add_route(
                    Route::new()
                        .capability(Capability::protocol_by_name("fuchsia.pkg.PackageCache"))
                        .capability(Capability::protocol_by_name("fuchsia.pkg.RetainedPackages"))
                        .capability(Capability::protocol_by_name("fuchsia.space.Manager"))
                        .from(&resolver_realm.cache)
                        .to(&system_updater),
                )
                .await
                .unwrap();

            // Make sure the component under test can log.
            realm_builder
                .add_route(
                    Route::new()
                        .capability(Capability::protocol_by_name("fuchsia.logger.LogSink"))
                        .from(Ref::parent())
                        .to(&system_updater),
                )
                .await
                .unwrap();

            // Expose system_updater to the parent
            realm_builder
                .add_route(
                    Route::new()
                        .capability(Capability::protocol_by_name(
                            "fuchsia.update.installer.Installer",
                        ))
                        .from(&system_updater)
                        .to(Ref::parent()),
                )
                .await
                .unwrap();

            // Expose pkg-cache to these tests, for use by verify_packages
            realm_builder
                .add_route(
                    Route::new()
                        .capability(Capability::protocol_by_name("fuchsia.pkg.PackageCache"))
                        .from(&resolver_realm.cache)
                        .to(Ref::parent()),
                )
                .await
                .unwrap();

            let realm_instance = realm_builder.build().await.unwrap();

            let installer_proxy = realm_instance
                .root
                .connect_to_protocol_at_exposed_dir::<InstallerMarker>()
                .unwrap();
            let paver_proxy =
                realm_instance.root.connect_to_protocol_at_exposed_dir::<PaverMarker>().unwrap();

            let updater = Updater::new_with_proxies(installer_proxy, paver_proxy);

            let resolver = ResolverForTest::new(&realm_instance, blobfs, Arc::clone(&served_repo))
                .await
                .unwrap();

            UpdaterForTest {
                served_repo,
                paver: paver_clone,
                packages: self.packages,
                update_merkle_root: *update.meta_far_merkle_root(),
                repo_url: self.repo_url,
                updater,
                resolver,
                realm_instance,
            }
        }

        #[cfg(test)]
        pub async fn build_and_run(self) -> UpdaterResult {
            self.build().await.run().await
        }
    }

    pub fn generate_packages_json(packages: &[Package], repo_url: &str) -> String {
        let package_urls: Vec<String> = packages
            .iter()
            .map(|p| format!("{}/{}/0?hash={}", repo_url, p.name(), p.meta_far_merkle_root()))
            .collect();

        let packages_json = serde_json::json!({
            "version": "1",
            "content": package_urls
        });
        serde_json::to_string(&packages_json).unwrap()
    }

    /// This wraps the `Updater` in order to reduce test boilerplate.
    /// Should be constructed using `UpdaterBuilder`.
    pub struct UpdaterForTest {
        pub served_repo: Arc<ServedRepository>,
        pub paver: Arc<MockPaverService>,
        pub packages: Vec<Package>,
        pub update_merkle_root: Hash,
        pub repo_url: fuchsia_url::RepositoryUrl,
        pub resolver: ResolverForTest,
        pub updater: Updater,
        pub realm_instance: RealmInstance,
    }

    impl UpdaterForTest {
        /// Run the system update, returning an `UpdaterResult` containing information about the
        /// result of the update.
        pub async fn run(mut self) -> UpdaterResult {
            let () = self.updater.install_update(None).await.expect("installing update");

            UpdaterResult {
                paver_events: self.paver.take_events(),
                resolver: self.resolver,
                packages: self.packages,
                realm_instance: self.realm_instance,
            }
        }
    }

    /// Contains information about the state of the system after the updater was run.
    pub struct UpdaterResult {
        /// All paver events received by the MockPaver during the update.
        pub paver_events: Vec<PaverEvent>,
        /// The resolver used by the updater.
        pub resolver: ResolverForTest,
        /// All the packages that should have been resolved by the update.
        pub packages: Vec<Package>,
        // The RealmInstance used to run this update, for introspection into component states.
        pub realm_instance: RealmInstance,
    }

    impl UpdaterResult {
        /// Verify that all packages that should have been resolved by the update
        /// were resolved.
        pub async fn verify_packages(&self) -> Result<(), Error> {
            let blobfs_client = self.resolver.cache.blobfs.client();
            for package in self.packages.iter() {
                // We deliberately avoid the package resolver here, as we want
                // to make sure that the system-updater retrieved all the
                // correct blobs.
                // We also want to avoid pkg-cache, since the packages we
                // installed are listed in the retained but not dynamic indices,
                // and will not be returned by PackageCache.Open. So, go
                // straight to blobfs.
                let blobs = package.content_and_subpackage_blobs().unwrap();
                for (merkle, _bytes) in blobs.iter() {
                    assert!(blobfs_client.has_blob(merkle).await);
                }
            }
            Ok(())
        }
    }
}

#[cfg(test)]
pub mod tests {
    use {
        super::for_tests::UpdaterBuilder,
        super::*,
        anyhow::Context,
        fidl_fuchsia_paver::{Asset, Configuration},
        fuchsia_async as fasync,
        fuchsia_pkg_testing::{make_current_epoch_json, PackageBuilder},
        mock_paver::PaverEvent,
    };

    #[fasync::run_singlethreaded(test)]
    pub async fn test_updater() -> Result<(), Error> {
        let data = "hello world!".as_bytes();
        let test_package = PackageBuilder::new("test_package")
            .add_resource_at("bin/hello", "this is a test".as_bytes())
            .add_resource_at("data/file", "this is a file".as_bytes())
            .add_resource_at("meta/test_package.cm", "{}".as_bytes())
            .build()
            .await
            .context("Building test_package")?;
        let updater = UpdaterBuilder::new()
            .await
            .paver(|p| {
                // Emulate ABR not being supported
                p.boot_manager_close_with_epitaph(zx::Status::NOT_SUPPORTED)
            })
            .add_package(test_package)
            .add_image("zbi.signed", data)
            .add_image("fuchsia.vbmeta", data)
            .add_image("recovery", data)
            .add_image("epoch.json", make_current_epoch_json().as_bytes())
            .add_image("recovery.vbmeta", data);
        let result = updater.build_and_run().await;

        assert_eq!(
            result.paver_events,
            vec![
                PaverEvent::WriteAsset {
                    configuration: Configuration::A,
                    asset: Asset::Kernel,
                    payload: data.to_vec()
                },
                PaverEvent::WriteAsset {
                    configuration: Configuration::B,
                    asset: Asset::Kernel,
                    payload: data.to_vec()
                },
                PaverEvent::WriteAsset {
                    configuration: Configuration::A,
                    asset: Asset::VerifiedBootMetadata,
                    payload: data.to_vec()
                },
                PaverEvent::WriteAsset {
                    configuration: Configuration::B,
                    asset: Asset::VerifiedBootMetadata,
                    payload: data.to_vec()
                },
                PaverEvent::DataSinkFlush,
            ]
        );

        result
            .verify_packages()
            .await
            .context("Verifying packages were correctly installed")
            .unwrap();
        Ok(())
    }
}
