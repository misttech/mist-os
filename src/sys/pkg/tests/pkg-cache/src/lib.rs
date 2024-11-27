// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![allow(clippy::let_unit_value)]
#![allow(clippy::bool_assert_comparison)]
#![cfg(test)]

use anyhow::anyhow;
use assert_matches::assert_matches;
use blobfs_ramdisk::BlobfsRamdisk;
use diagnostics_assertions::TreeAssertion;
use fidl::endpoints::DiscoverableProtocolMarker as _;
use fuchsia_component_test::{Capability, ChildOptions, RealmBuilder, RealmInstance, Ref, Route};
use fuchsia_inspect::reader::DiagnosticsHierarchy;
use fuchsia_merkle::Hash;
use fuchsia_pkg_testing::{get_inspect_hierarchy, BlobContents, Package};
use fuchsia_sync::Mutex;
use futures::future::BoxFuture;
use futures::prelude::*;
use mock_boot_arguments::MockBootArgumentsService;
use mock_metrics::MockMetricEventLoggerFactory;
use mock_paver::{MockPaverService, MockPaverServiceBuilder};
use mock_reboot::{MockRebootService, RebootReason};
use mock_verifier::MockVerifierService;
use std::collections::HashMap;
use std::sync::Arc;
use vfs::directory::entry_container::Directory as _;
use vfs::directory::helper::DirectlyMutable as _;
use zx::{self as zx, Status};
use {
    fidl_fuchsia_boot as fboot, fidl_fuchsia_component_resolution as fcomponent_resolution,
    fidl_fuchsia_fxfs as ffxfs, fidl_fuchsia_io as fio, fidl_fuchsia_metrics as fmetrics,
    fidl_fuchsia_pkg as fpkg, fidl_fuchsia_pkg_ext as fpkg_ext, fidl_fuchsia_space as fspace,
    fidl_fuchsia_update as fupdate, fidl_fuchsia_update_verify as fupdate_verify,
    fuchsia_async as fasync,
};

mod base_pkg_index;
mod cache_pkg_index;
mod cobalt;
mod executability_enforcement;
mod get;
mod inspect;
mod pkgfs;
mod retained_packages;
mod space;
mod startup;
mod sync;

static SHELL_COMMANDS_BIN_PATH: &str = "shell-commands-bin";
// Sleep duration while waiting for pkg-cache to update inspect state. Chosen arbitrarily.
// Do not just sleep once and assume good, loop until expected state occurs.
const INSPECT_WAIT: std::time::Duration = std::time::Duration::from_millis(10);

#[derive(Debug)]
enum WriteBlobError {
    File(zx::Status),
    Writer(blob_writer::WriteError),
}

impl WriteBlobError {
    fn assert_out_of_space(&self) {
        match self {
            Self::File(s) => assert_eq!(*s, zx::Status::NO_SPACE),
            Self::Writer(e) => assert_matches!(
                e,
                blob_writer::WriteError::BytesReady(s) if *s == zx::Status::NO_SPACE
            ),
        }
    }
}

/// Writes `contents` to `blob`. If `blob` was opened as a `Delivery` blob, then contents should
/// already be compressed.
async fn write_blob(contents: &[u8], blob: fpkg::BlobWriter) -> Result<(), WriteBlobError> {
    match blob {
        fpkg::BlobWriter::File(file) => {
            let file = file.into_proxy();
            let () = file
                .resize(contents.len() as u64)
                .await
                .unwrap()
                .map_err(zx::Status::from_raw)
                .unwrap();
            fuchsia_fs::file::write(&file, contents)
                .await
                .map_err(|e| match e {
                    fuchsia_fs::file::WriteError::WriteError(s) => s,
                    _ => zx::Status::INTERNAL,
                })
                .map_err(WriteBlobError::File)?;
            let () = file.close().await.unwrap().map_err(zx::Status::from_raw).unwrap();
        }
        fpkg::BlobWriter::Writer(writer) => {
            let () = blob_writer::BlobWriter::create(
                writer.into_proxy(),
                contents.len().try_into().unwrap(),
            )
            .await
            .unwrap()
            .write(contents)
            .await
            .map_err(WriteBlobError::Writer)?;
        }
    }

    Ok(())
}

/// Compresses `contents` then writes the compressed bytes to `blob`. `blob` should have been
/// opened as a `Delivery` blob.
async fn compress_and_write_blob(
    contents: &[u8],
    blob: fpkg::BlobWriter,
) -> Result<(), WriteBlobError> {
    write_blob(
        &delivery_blob::Type1Blob::generate(contents, delivery_blob::CompressionMode::Attempt),
        blob,
    )
    .await
}

// Calls fuchsia.pkg/NeededBlobs.GetMissingBlobs and reads from the iterator until it ends.
// Will block unless the package being cached does not have any uncached subpackage meta.fars (or
// another process is writing the subpackage meta.fars via NeededBlobs.OpenBlob), since pkg-cache
// can't determine which blobs are required to cache a package without reading the meta.fars of all
// the subpackages.
async fn get_missing_blobs(proxy: &fpkg::NeededBlobsProxy) -> Vec<fpkg::BlobInfo> {
    let (blob_iterator, blob_iterator_server_end) =
        fidl::endpoints::create_proxy::<fpkg::BlobInfoIteratorMarker>();
    let () = proxy.get_missing_blobs(blob_iterator_server_end).unwrap();

    let mut res = vec![];
    loop {
        let chunk = blob_iterator.next().await.unwrap();
        if chunk.is_empty() {
            break;
        }
        res.extend(chunk);
    }
    res
}

// Verifies that:
//   1. all requested blobs are actually needed by the package
//   2. no blob is requested more than once
//   3. after the Get, all the package's blobs are in blobfs
async fn get_and_verify_package(
    package_cache: &fpkg::PackageCacheProxy,
    gc_protection: fpkg::GcProtection,
    pkg: &Package,
) -> fio::DirectoryProxy {
    let meta_blob_info =
        fpkg::BlobInfo { blob_id: fpkg_ext::BlobId::from(*pkg.hash()).into(), length: 0 };

    let (needed_blobs, needed_blobs_server_end) =
        fidl::endpoints::create_proxy::<fpkg::NeededBlobsMarker>();
    let (dir, dir_server_end) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>();
    let get_fut = package_cache
        .get(&meta_blob_info, gc_protection, needed_blobs_server_end, dir_server_end)
        .map_ok(|res| res.map_err(zx::Status::from_raw));

    let (meta_far, _) = pkg.contents();
    let available_blobs = pkg.content_and_subpackage_blobs().unwrap();

    let () = write_meta_far(&needed_blobs, meta_far).await;
    let () = write_needed_blobs(&needed_blobs, available_blobs).await;

    let () = get_fut.await.unwrap().unwrap();
    let () = pkg.verify_contents(&dir).await.unwrap();
    dir
}

pub async fn write_meta_far(needed_blobs: &fpkg::NeededBlobsProxy, meta_far: BlobContents) {
    let meta_blob = needed_blobs.open_meta_blob().await.unwrap().unwrap().unwrap();
    let () = compress_and_write_blob(&meta_far.contents, *meta_blob).await.unwrap();
    let () = blob_written(needed_blobs, meta_far.merkle).await;
}

pub async fn blob_written(needed_blobs: &fpkg::NeededBlobsProxy, hash: Hash) {
    let () =
        needed_blobs.blob_written(&fpkg_ext::BlobId::from(hash).into()).await.unwrap().unwrap();
}

pub async fn write_needed_blobs(
    needed_blobs: &fpkg::NeededBlobsProxy,
    mut available_blobs: HashMap<Hash, Vec<u8>>,
) {
    let (blob_iterator, blob_iterator_server_end) =
        fidl::endpoints::create_proxy::<fpkg::BlobInfoIteratorMarker>();
    let () = needed_blobs.get_missing_blobs(blob_iterator_server_end).unwrap();

    loop {
        let chunk = blob_iterator.next().await.unwrap();
        if chunk.is_empty() {
            break;
        }
        for blob_info in chunk {
            let blob_proxy =
                needed_blobs.open_blob(&blob_info.blob_id).await.unwrap().unwrap().unwrap();
            let () = compress_and_write_blob(
                available_blobs
                    .remove(&fpkg_ext::BlobId::from(blob_info.blob_id).into())
                    .unwrap()
                    .as_slice(),
                *blob_proxy,
            )
            .await
            .unwrap();
            let () =
                blob_written(needed_blobs, fpkg_ext::BlobId::from(blob_info.blob_id).into()).await;
        }
    }
}

// Calls PackageCache.Get and verifies the package directory for each element of `packages`
// concurrently.
// PackageCache.Get requires that the caller not write the same blob concurrently across
// separate calls and this fn does not enforce that, so this fn should not be called with
// packages that share blobs.
// Uses OpenPackageTracking
async fn get_and_verify_packages(proxy: &fpkg::PackageCacheProxy, packages: &[Package]) {
    let () = futures::stream::iter(packages)
        .for_each_concurrent(None, move |pkg| {
            get_and_verify_package(proxy, fpkg::GcProtection::OpenPackageTracking, pkg).map(|_| {})
        })
        .await;
}

// Verifies that:
//   1. `pkg` can be opened via PackageCache.Get without needing to write any blobs.
//   2. `pkg` matches the package directory obtained from PackageCache.Get.
//
// Does *not* verify that `pkg`'s subpackages were cached.
//
// Returns the package directory obtained from PackageCache.Get.
async fn verify_package_cached(
    proxy: &fpkg::PackageCacheProxy,
    pkg: &Package,
) -> fio::DirectoryProxy {
    let meta_blob_info =
        fpkg::BlobInfo { blob_id: fpkg_ext::BlobId::from(*pkg.hash()).into(), length: 0 };

    let (needed_blobs, needed_blobs_server_end) =
        fidl::endpoints::create_proxy::<fpkg::NeededBlobsMarker>();

    let (dir, dir_server_end) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>();

    let get_fut = proxy
        .get(
            &meta_blob_info,
            fpkg::GcProtection::OpenPackageTracking,
            needed_blobs_server_end,
            dir_server_end,
        )
        .map_ok(|res| res.map_err(Status::from_raw));

    // If the package is in base, cache, or currently open, the server will send a `ZX_OK` epitaph
    // and then close the channel.
    // If all the blobs are cached but the package is not in base, cache, or already open, then the
    // server will reply with `Ok(None)`, meaning that the metadata blob is cached and
    // GetMissingBlobs needs to be performed (but the iterator obtained with GetMissingBlobs should
    // be empty).
    let epitaph_received = match needed_blobs.open_meta_blob().await {
        Err(fidl::Error::ClientChannelClosed { status: Status::OK, .. }) => true,
        Ok(Ok(None)) => false,
        Ok(r) => {
            panic!("Meta blob not cached: unexpected response {r:?}")
        }
        Err(e) => {
            panic!("Meta blob not cached: unexpected FIDL error {e:?}")
        }
    };

    let (blob_iterator, blob_iterator_server_end) =
        fidl::endpoints::create_proxy::<fpkg::BlobInfoIteratorMarker>();
    let () = needed_blobs.get_missing_blobs(blob_iterator_server_end).unwrap();
    let chunk = blob_iterator.next().await;

    if epitaph_received {
        // The server closed the channel, so the iterator gets closed too.
        assert_matches!(
            chunk,
            Err(fidl::Error::ClientChannelClosed { status: Status::PEER_CLOSED, .. })
        );
    } else {
        // All subpackage meta.fars and content blobs should be cached, so iterator should be empty.
        assert_eq!(chunk.unwrap(), vec![]);
    }

    let () = get_fut.await.unwrap().unwrap();

    // `dir` is resolved to package directory.
    let () = pkg.verify_contents(&dir).await.unwrap();

    dir
}

// Verifies that:
//   1. `pkg` can be opened via PackageCache.GetSubpackage.
//   2. `pkg` matches the package directory obtained from PackageCache.GetSubpackage.
//
// Does *not* verify that `pkg`'s subpackages can be obtained.
//
// Returns the package directory obtained from PackageCache.GetSubpackage.
async fn verify_get_subpackage(
    proxy: &fpkg::PackageCacheProxy,
    superpackage: Hash,
    url: String,
    pkg: &Package,
) -> fio::DirectoryProxy {
    let (dir, dir_server_end) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>();
    let () = proxy
        .get_subpackage(
            &fpkg_ext::BlobId::from(superpackage).into(),
            &fpkg::PackageUrl { url },
            dir_server_end,
        )
        .await
        .unwrap()
        .unwrap();
    let () = pkg.verify_contents(&dir).await.unwrap();
    dir
}

pub async fn replace_retained_packages(
    proxy: &fpkg::RetainedPackagesProxy,
    packages: &[fpkg_ext::BlobId],
) {
    let packages = packages.iter().cloned().map(Into::into).collect::<Vec<_>>();
    let (iterator_client_end, iterator_stream) =
        fidl::endpoints::create_request_stream::<fpkg::BlobIdIteratorMarker>().unwrap();
    let serve_iterator_fut = async {
        fpkg_ext::serve_fidl_iterator_from_slice(iterator_stream, packages).await.unwrap();
    };
    let (replace_retained_result, ()) =
        futures::join!(proxy.replace(iterator_client_end), serve_iterator_fut);
    assert_matches!(replace_retained_result, Ok(()));
}

async fn verify_packages_cached(proxy: &fpkg::PackageCacheProxy, packages: &[Package]) {
    let () = futures::stream::iter(packages)
        .for_each_concurrent(None, move |pkg| verify_package_cached(proxy, pkg).map(|_| ()))
        .await;
}

trait Blobfs {
    fn root_proxy(&self) -> fio::DirectoryProxy;
    fn svc_dir(&self) -> fio::DirectoryProxy;
    fn blob_creator_proxy(&self) -> Option<ffxfs::BlobCreatorProxy>;
    fn blob_reader_proxy(&self) -> Option<ffxfs::BlobReaderProxy>;
}

impl Blobfs for BlobfsRamdisk {
    fn root_proxy(&self) -> fio::DirectoryProxy {
        self.root_dir_proxy().unwrap()
    }
    fn svc_dir(&self) -> fio::DirectoryProxy {
        self.svc_dir().unwrap().unwrap()
    }
    fn blob_creator_proxy(&self) -> Option<ffxfs::BlobCreatorProxy> {
        self.blob_creator_proxy().unwrap()
    }
    fn blob_reader_proxy(&self) -> Option<ffxfs::BlobReaderProxy> {
        self.blob_reader_proxy().unwrap()
    }
}

struct TestEnvBuilder<BlobfsAndSystemImageFut> {
    paver_service_builder: Option<MockPaverServiceBuilder>,
    blobfs_and_system_image:
        Box<dyn FnOnce(blobfs_ramdisk::Implementation) -> BlobfsAndSystemImageFut>,
    ignore_system_image: bool,
    enable_upgradable_packages: bool,
    blob_implementation: Option<blobfs_ramdisk::Implementation>,
    bootfs_blobs: HashMap<Hash, Vec<u8>>,
}

impl TestEnvBuilder<BoxFuture<'static, (BlobfsRamdisk, Option<Hash>)>> {
    fn new() -> Self {
        Self {
            blobfs_and_system_image: Box::new(|blob_impl| {
                async move {
                    let system_image_package =
                        fuchsia_pkg_testing::SystemImageBuilder::new().build().await;
                    let blobfs =
                        BlobfsRamdisk::builder().implementation(blob_impl).start().await.unwrap();
                    let () = system_image_package.write_to_blobfs(&blobfs).await;
                    (blobfs, Some(*system_image_package.hash()))
                }
                .boxed()
            }),
            paver_service_builder: None,
            ignore_system_image: false,
            enable_upgradable_packages: false,
            blob_implementation: None,
            bootfs_blobs: HashMap::new(),
        }
    }
}

impl<BlobfsAndSystemImageFut, ConcreteBlobfs> TestEnvBuilder<BlobfsAndSystemImageFut>
where
    BlobfsAndSystemImageFut: Future<Output = (ConcreteBlobfs, Option<Hash>)>,
    ConcreteBlobfs: Blobfs,
{
    fn paver_service_builder(self, paver_service_builder: MockPaverServiceBuilder) -> Self {
        Self { paver_service_builder: Some(paver_service_builder), ..self }
    }

    fn blobfs_and_system_image_hash<OtherBlobfs>(
        self,
        blobfs: OtherBlobfs,
        system_image: Option<Hash>,
    ) -> TestEnvBuilder<future::Ready<(OtherBlobfs, Option<Hash>)>>
    where
        OtherBlobfs: Blobfs + 'static,
    {
        TestEnvBuilder {
            blobfs_and_system_image: Box::new(move |_| future::ready((blobfs, system_image))),
            paver_service_builder: self.paver_service_builder,
            ignore_system_image: self.ignore_system_image,
            enable_upgradable_packages: self.enable_upgradable_packages,
            blob_implementation: self.blob_implementation,
            bootfs_blobs: self.bootfs_blobs,
        }
    }

    /// Creates a BlobfsRamdisk loaded with, and configures pkg-cache to use, the supplied
    /// `system_image` package.
    async fn blobfs_from_system_image(
        self,
        system_image: &Package,
    ) -> TestEnvBuilder<future::Ready<(BlobfsRamdisk, Option<Hash>)>> {
        self.blobfs_from_system_image_and_extra_packages(system_image, &[]).await
    }

    /// Creates a BlobfsRamdisk loaded with the supplied packages and configures the system to use
    /// the supplied `system_image` package.
    async fn blobfs_from_system_image_and_extra_packages(
        self,
        system_image: &Package,
        extra_packages: &[&Package],
    ) -> TestEnvBuilder<future::Ready<(BlobfsRamdisk, Option<Hash>)>> {
        assert_eq!(self.blob_implementation, None);
        let blobfs = BlobfsRamdisk::builder().impl_from_env().start().await.unwrap();
        let () = system_image.write_to_blobfs(&blobfs).await;
        for pkg in extra_packages {
            let () = pkg.write_to_blobfs(&blobfs).await;
        }
        let system_image_hash = *system_image.hash();

        TestEnvBuilder::<_> {
            blobfs_and_system_image: Box::new(move |_| {
                future::ready((blobfs, Some(system_image_hash)))
            }),
            paver_service_builder: self.paver_service_builder,
            ignore_system_image: self.ignore_system_image,
            enable_upgradable_packages: self.enable_upgradable_packages,
            blob_implementation: Some(blobfs_ramdisk::Implementation::from_env()),
            bootfs_blobs: self.bootfs_blobs,
        }
    }

    fn ignore_system_image(self) -> Self {
        assert_eq!(self.ignore_system_image, false);
        Self { ignore_system_image: true, ..self }
    }

    fn enable_upgradable_packages(self) -> Self {
        assert_eq!(self.enable_upgradable_packages, false);
        Self { enable_upgradable_packages: true, ..self }
    }

    fn fxblob(self) -> Self {
        assert_eq!(self.blob_implementation, None);
        Self { blob_implementation: Some(blobfs_ramdisk::Implementation::Fxblob), ..self }
    }

    fn cpp_blobfs(self) -> Self {
        assert_eq!(self.blob_implementation, None);
        Self { blob_implementation: Some(blobfs_ramdisk::Implementation::CppBlobfs), ..self }
    }

    fn blobfs_impl(self, impl_: blobfs_ramdisk::Implementation) -> Self {
        assert_eq!(self.blob_implementation, None);
        Self { blob_implementation: Some(impl_), ..self }
    }

    fn add_bootfs_blob(mut self, hash: Hash, contents: Vec<u8>) -> Self {
        assert_matches!(self.bootfs_blobs.insert(hash, contents), None);
        self
    }

    async fn build(self) -> TestEnv<ConcreteBlobfs> {
        let (blob_implementation, blob_implementation_overridden) = match self.blob_implementation {
            Some(blob_implementation) => (blob_implementation, true),
            None => (blobfs_ramdisk::Implementation::from_env(), false),
        };
        let (blobfs, system_image) = (self.blobfs_and_system_image)(blob_implementation).await;
        let local_child_svc_dir = vfs::pseudo_directory! {};

        // Cobalt mocks so we can assert that we emit the correct events
        let logger_factory = Arc::new(MockMetricEventLoggerFactory::new());
        {
            let logger_factory = Arc::clone(&logger_factory);
            local_child_svc_dir
                .add_entry(
                    fmetrics::MetricEventLoggerFactoryMarker::PROTOCOL_NAME,
                    vfs::service::host(move |stream| {
                        Arc::clone(&logger_factory).run_logger_factory(stream)
                    }),
                )
                .unwrap();
        }

        let reboot_reasons = Arc::new(Mutex::new(vec![]));
        let reboot_service = Arc::new(MockRebootService::new(Box::new({
            let reboot_reasons = Arc::clone(&reboot_reasons);
            move |reason| {
                reboot_reasons.lock().push(reason);
                Ok(())
            }
        })));
        {
            let reboot_service = Arc::clone(&reboot_service);
            local_child_svc_dir
                .add_entry(
                    fidl_fuchsia_hardware_power_statecontrol::AdminMarker::PROTOCOL_NAME,
                    vfs::service::host(move |stream| {
                        Arc::clone(&reboot_service).run_reboot_service(stream).unwrap_or_else(|e| {
                            panic!("error running reboot service: {:#}", anyhow!(e))
                        })
                    }),
                )
                .unwrap();
        }

        // Paver service, so we can verify that we submit the expected requests and so that
        // we can verify if the paver service returns errors, that we handle them correctly.
        let paver_service = Arc::new(
            self.paver_service_builder.unwrap_or_else(MockPaverServiceBuilder::new).build(),
        );
        {
            let paver_service = Arc::clone(&paver_service);
            local_child_svc_dir
                .add_entry(
                    fidl_fuchsia_paver::PaverMarker::PROTOCOL_NAME,
                    vfs::service::host(move |stream| {
                        Arc::clone(&paver_service).run_paver_service(stream).unwrap_or_else(|e| {
                            panic!("error running paver service: {:#}", anyhow!(e))
                        })
                    }),
                )
                .unwrap();
        }

        // Set up verifier service so we can verify that we reject GC until after the verifier
        // commits this boot/slot as successful, lest we break rollbacks.
        let verifier_service = Arc::new(MockVerifierService::new(|_| Ok(())));
        {
            let verifier_service = Arc::clone(&verifier_service);
            local_child_svc_dir
                .add_entry(
                    fupdate_verify::BlobfsVerifierMarker::PROTOCOL_NAME,
                    vfs::service::host(move |stream| {
                        Arc::clone(&verifier_service).run_blobfs_verifier_service(stream)
                    }),
                )
                .unwrap();
        }
        {
            let verifier_service = Arc::clone(&verifier_service);
            local_child_svc_dir
                .add_entry(
                    fupdate_verify::NetstackVerifierMarker::PROTOCOL_NAME,
                    vfs::service::host(move |stream| {
                        Arc::clone(&verifier_service).run_netstack_verifier_service(stream)
                    }),
                )
                .unwrap();
        }

        // fuchsia.boot/Arguments service to supply the hash of the system_image package.
        let mut arguments_service = MockBootArgumentsService::new(HashMap::new());
        if let Some(hash) = system_image {
            arguments_service.insert_pkgfs_boot_arg(hash);
        }
        let arguments_service = Arc::new(arguments_service);
        {
            let arguments_service = Arc::clone(&arguments_service);
            local_child_svc_dir
                .add_entry(
                    fboot::ArgumentsMarker::PROTOCOL_NAME,
                    vfs::service::host(move |stream| {
                        Arc::clone(&arguments_service).handle_request_stream(stream)
                    }),
                )
                .unwrap();
        }

        let bootfs_blobs = {
            // The capability is optional, so if there are no bootfs blobs give pkg-cache a broken
            // proxy.
            if self.bootfs_blobs.is_empty() {
                vfs::remote::remote_dir(fidl::endpoints::create_proxy::<fio::DirectoryMarker>().0)
            } else {
                let dir = vfs::directory::immutable::simple();
                for (hash, contents) in self.bootfs_blobs {
                    let () =
                        dir.add_entry(hash.to_string(), vfs::file::read_only(contents)).unwrap();
                }
                dir as Arc<dyn vfs::directory::entry::DirectoryEntry>
            }
        };

        let local_child_out_dir = vfs::pseudo_directory! {
            "blob" => vfs::remote::remote_dir(blobfs.root_proxy()),
            "bootfs-blobs" => bootfs_blobs,
            "svc" => local_child_svc_dir,
        };
        if matches!(blob_implementation, blobfs_ramdisk::Implementation::Fxblob) {
            local_child_out_dir
                .add_entry("blob-svc", vfs::remote::remote_dir(blobfs.svc_dir()))
                .unwrap();
        }

        let local_child_out_dir = Mutex::new(Some(local_child_out_dir));

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
                    .from(&pkg_cache_config)
                    .to(&pkg_cache),
            )
            .await
            .unwrap();
        if self.ignore_system_image {
            builder
                .add_capability(cm_rust::CapabilityDecl::Config(cm_rust::ConfigurationDecl {
                    name: "fuchsia.pkgcache.UseSystemImage".parse().unwrap(),
                    value: false.into(),
                }))
                .await
                .unwrap();
        }
        builder
            .add_route(
                Route::new()
                    .capability(Capability::configuration("fuchsia.pkgcache.UseSystemImage"))
                    .from(if self.ignore_system_image {
                        Ref::self_()
                    } else {
                        (&pkg_cache_config).into()
                    })
                    .to(&pkg_cache),
            )
            .await
            .unwrap();
        if self.enable_upgradable_packages {
            builder
                .add_capability(cm_rust::CapabilityDecl::Config(cm_rust::ConfigurationDecl {
                    name: "fuchsia.pkgcache.EnableUpgradablePackages".parse().unwrap(),
                    value: true.into(),
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
                    .from(if self.enable_upgradable_packages {
                        Ref::self_()
                    } else {
                        (&pkg_cache_config).into()
                    })
                    .to(&pkg_cache),
            )
            .await
            .unwrap();
        if blob_implementation_overridden {
            builder
                .add_capability(cm_rust::CapabilityDecl::Config(cm_rust::ConfigurationDecl {
                    name: "fuchsia.pkgcache.UseFxblob".parse().unwrap(),
                    value: matches!(blob_implementation, blobfs_ramdisk::Implementation::Fxblob)
                        .into(),
                }))
                .await
                .unwrap();
        }
        builder
            .add_route(
                Route::new()
                    .capability(Capability::configuration("fuchsia.pkgcache.UseFxblob"))
                    .from(if blob_implementation_overridden {
                        Ref::self_()
                    } else {
                        (&pkg_cache_config).into()
                    })
                    .to(&pkg_cache),
            )
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
        builder
            .add_route(
                Route::new()
                    .capability(Capability::protocol::<fidl_fuchsia_logger::LogSinkMarker>())
                    .capability(Capability::dictionary("diagnostics"))
                    .from(Ref::parent())
                    .to(&pkg_cache)
                    .to(&service_reflector)
                    .to(&system_update_committer),
            )
            .await
            .unwrap();
        builder
            .add_route(
                Route::new()
                    .capability(Capability::protocol::<fmetrics::MetricEventLoggerFactoryMarker>())
                    .capability(Capability::protocol::<fboot::ArgumentsMarker>())
                    .capability(
                        Capability::protocol::<fidl_fuchsia_tracing_provider::RegistryMarker>(),
                    )
                    .capability(Capability::protocol::<
                        fidl_fuchsia_hardware_power_statecontrol::AdminMarker,
                    >())
                    .capability(
                        Capability::directory("blob-exec")
                            .path("/blob")
                            .rights(fio::RW_STAR_DIR | fio::Operations::EXECUTE),
                    )
                    .capability(
                        Capability::directory("bootfs-blobs")
                            .path("/bootfs-blobs")
                            .rights(fio::RX_STAR_DIR),
                    )
                    .from(&service_reflector)
                    .to(&pkg_cache),
            )
            .await
            .unwrap();
        if matches!(blob_implementation, blobfs_ramdisk::Implementation::Fxblob) {
            builder
                .add_route(
                    Route::new()
                        .capability(
                            Capability::protocol::<ffxfs::BlobCreatorMarker>().path(format!(
                                "/blob-svc/{}",
                                ffxfs::BlobCreatorMarker::PROTOCOL_NAME
                            )),
                        )
                        .capability(
                            Capability::protocol::<ffxfs::BlobReaderMarker>().path(format!(
                                "/blob-svc/{}",
                                ffxfs::BlobReaderMarker::PROTOCOL_NAME
                            )),
                        )
                        .from(&service_reflector)
                        .to(&pkg_cache),
                )
                .await
                .unwrap();
        }
        builder
            .add_route(
                Route::new()
                    .capability(Capability::protocol::<fidl_fuchsia_paver::PaverMarker>())
                    .capability(Capability::protocol::<fupdate_verify::BlobfsVerifierMarker>())
                    .capability(Capability::protocol::<fupdate_verify::NetstackVerifierMarker>())
                    .from(&service_reflector)
                    .to(&system_update_committer),
            )
            .await
            .unwrap();
        builder
            .add_route(
                Route::new()
                    .capability(Capability::protocol::<fupdate::CommitStatusProviderMarker>())
                    .from(&system_update_committer)
                    .to(&pkg_cache) // offer
                    .to(Ref::parent()), // expose
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
                    .capability(Capability::directory(SHELL_COMMANDS_BIN_PATH))
                    .capability(Capability::directory("pkgfs"))
                    .capability(Capability::directory("system"))
                    .from(&pkg_cache)
                    .to(Ref::parent()),
            )
            .await
            .unwrap();

        let realm_instance = builder.build().await.unwrap();

        let proxies = Proxies {
            commit_status_provider: realm_instance
                .root
                .connect_to_protocol_at_exposed_dir::<fupdate::CommitStatusProviderMarker>()
                .expect("connect to commit status provider"),
            space_manager: realm_instance
                .root
                .connect_to_protocol_at_exposed_dir::<fspace::ManagerMarker>()
                .expect("connect to space manager"),
            package_cache: realm_instance
                .root
                .connect_to_protocol_at_exposed_dir::<fpkg::PackageCacheMarker>()
                .expect("connect to package cache"),
            retained_packages: realm_instance
                .root
                .connect_to_protocol_at_exposed_dir::<fpkg::RetainedPackagesMarker>()
                .expect("connect to retained packages"),
            pkgfs: fuchsia_fs::directory::open_directory_async(
                realm_instance.root.get_exposed_dir(),
                "pkgfs",
                fio::PERM_READABLE | fio::PERM_WRITABLE | fio::PERM_EXECUTABLE,
            )
            .expect("open pkgfs"),
        };

        TestEnv {
            apps: Apps { realm_instance },
            blobfs,
            system_image,
            reboot_reasons,
            proxies,
            mocks: Mocks {
                logger_factory,
                reboot_service,
                _paver_service: paver_service,
                _verifier_service: verifier_service,
            },
        }
    }
}

struct Proxies {
    commit_status_provider: fupdate::CommitStatusProviderProxy,
    space_manager: fspace::ManagerProxy,
    package_cache: fpkg::PackageCacheProxy,
    retained_packages: fpkg::RetainedPackagesProxy,
    pkgfs: fio::DirectoryProxy,
}

pub struct Mocks {
    pub logger_factory: Arc<MockMetricEventLoggerFactory>,
    pub reboot_service: Arc<MockRebootService>,
    _paver_service: Arc<MockPaverService>,
    _verifier_service: Arc<MockVerifierService>,
}

struct Apps {
    realm_instance: RealmInstance,
}

struct TestEnv<B = BlobfsRamdisk> {
    apps: Apps,
    blobfs: B,
    system_image: Option<Hash>,
    reboot_reasons: Arc<Mutex<Vec<RebootReason>>>,
    proxies: Proxies,
    pub mocks: Mocks,
}

impl TestEnv<BlobfsRamdisk> {
    // workaround for https://fxbug.dev/42113882
    async fn stop(self) {
        // Tear down the environment in reverse order, ending with the storage.
        drop(self.proxies);
        drop(self.apps);
        self.blobfs.stop().await.unwrap();
    }

    fn builder() -> TestEnvBuilder<BoxFuture<'static, (BlobfsRamdisk, Option<Hash>)>> {
        TestEnvBuilder::new()
    }
}

impl<B: Blobfs> TestEnv<B> {
    async fn inspect_hierarchy(&self) -> DiagnosticsHierarchy {
        let nested_environment_label = format!(
            "pkg_cache_integration_test/realm_builder\\:{}",
            self.apps.realm_instance.root.child_name()
        );

        get_inspect_hierarchy(&nested_environment_label, "pkg_cache").await
    }

    pub async fn get_already_cached(
        &self,
        merkle: &str,
    ) -> Result<fio::DirectoryProxy, fpkg_ext::cache::GetAlreadyCachedError> {
        fpkg_ext::cache::Client::from_proxy(self.proxies.package_cache.clone())
            .get_already_cached(merkle.parse().unwrap())
            .await
            .map(|pd| pd.into_proxy())
    }

    async fn block_until_started(&self) {
        // Wait until the server is responding to FIDL requests, the result is irrelevant.
        let (_, needed_blobs) = fidl::endpoints::create_endpoints();
        let _ = self
            .proxies
            .package_cache
            .get(
                &fpkg::BlobInfo { blob_id: fpkg::BlobId { merkle_root: [0; 32] }, length: 0 },
                fpkg::GcProtection::OpenPackageTracking,
                needed_blobs,
                fidl::endpoints::create_endpoints().1,
            )
            .await
            .unwrap();

        // Also, make sure the system-update-committer starts to prevent race conditions
        // where the system-update-commiter drops before the paver.
        let _ = self.proxies.commit_status_provider.is_current_system_committed().await.unwrap();
    }

    /// Wait until pkg-cache inspect state satisfies `desired_state`, return the satisfying state.
    pub async fn wait_for_and_return_inspect_state(
        &self,
        desired_state: TreeAssertion<String>,
    ) -> DiagnosticsHierarchy {
        loop {
            let hierarchy = self.inspect_hierarchy().await;
            if desired_state.run(&hierarchy).is_ok() {
                break hierarchy;
            }
            let () = fasync::Timer::new(INSPECT_WAIT).await;
        }
    }

    /// Wait for pkg-cache to notice that all of the connections to `pkg` have closed and stop
    /// protecting it from GC.
    pub async fn wait_for_package_to_close(&self, pkg: &Hash) {
        let pkg = pkg.to_string();
        loop {
            if self
                .inspect_hierarchy()
                .await
                .get_child("open-packages")
                .unwrap()
                .get_child(&pkg)
                .is_none()
            {
                break;
            }
            let () = fasync::Timer::new(INSPECT_WAIT).await;
        }
    }

    pub fn client(&self) -> fidl_fuchsia_pkg_ext::cache::Client {
        fidl_fuchsia_pkg_ext::cache::Client::from_proxy(self.proxies.package_cache.clone())
    }

    /// Get a DirectoryProxy to pkg-cache's exposed /system directory.
    /// This proxy is not stored in Proxies because the directory is not served when there is no
    /// system_image package.
    async fn system_dir(&self) -> fio::DirectoryProxy {
        fuchsia_fs::directory::open_directory(
            self.apps.realm_instance.root.get_exposed_dir(),
            "system",
            fio::PERM_READABLE | fio::PERM_EXECUTABLE,
        )
        .await
        .expect("open system")
    }

    async fn write_to_blobfs(&self, hash: &Hash, contents: &[u8]) {
        let blobfs = blobfs::Client::new(
            self.blobfs.root_proxy(),
            self.blobfs.blob_creator_proxy(),
            self.blobfs.blob_reader_proxy(),
            None,
        )
        .unwrap();
        let () = compress_and_write_blob(contents, blobfs.open_blob_for_write(hash).await.unwrap())
            .await
            .unwrap();
    }

    fn take_reboot_reasons(&self) -> Vec<RebootReason> {
        std::mem::take(&mut *self.reboot_reasons.lock())
    }
}
