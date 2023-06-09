// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::base_packages::BasePackages,
    crate::index::{
        fulfill_meta_far_blob, CompleteInstallError, FulfillMetaFarError, PackageIndex,
    },
    anyhow::{anyhow, Error},
    cobalt_sw_delivery_registry as metrics,
    fidl::endpoints::ServerEnd,
    fidl::prelude::*,
    fidl_contrib::protocol_connector::ProtocolSender,
    fidl_fuchsia_io as fio,
    fidl_fuchsia_metrics::MetricEvent,
    fidl_fuchsia_pkg::{
        self as fpkg, NeededBlobsMarker, NeededBlobsRequest, NeededBlobsRequestStream,
        PackageCacheRequest, PackageCacheRequestStream, PackageIndexEntry,
        PackageIndexIteratorRequestStream,
    },
    fidl_fuchsia_pkg_ext::{
        serve_fidl_iterator_from_slice, serve_fidl_iterator_from_stream, BlobId, BlobInfo,
    },
    fuchsia_async::Task,
    fuchsia_cobalt_builders::MetricEventExt,
    fuchsia_hash::Hash,
    fuchsia_inspect::{self as finspect, NumericProperty, Property, StringProperty},
    fuchsia_trace as ftrace, fuchsia_zircon as zx,
    fuchsia_zircon::Status,
    futures::{FutureExt as _, TryFutureExt as _, TryStreamExt as _},
    std::{
        collections::HashSet,
        sync::{
            atomic::{AtomicU32, Ordering},
            Arc,
        },
    },
    tracing::{error, warn},
    vfs::directory::entry::DirectoryEntry as _,
};

mod missing_blobs;

// This encodes a host to interpolate when responding to BasePackageIndex requests.
const BASE_PACKAGE_HOST: &str = "fuchsia.com";

#[allow(clippy::too_many_arguments)]
pub(crate) async fn serve(
    package_index: Arc<async_lock::RwLock<PackageIndex>>,
    blobfs: blobfs::Client,
    base_packages: Arc<BasePackages>,
    cache_packages: Option<Arc<system_image::CachePackages>>,
    executability_restrictions: system_image::ExecutabilityRestrictions,
    non_static_allow_list: Arc<system_image::NonStaticAllowList>,
    scope: package_directory::ExecutionScope,
    stream: PackageCacheRequestStream,
    cobalt_sender: ProtocolSender<MetricEvent>,
    serve_id: Arc<AtomicU32>,
    get_node: Arc<finspect::Node>,
) -> Result<(), Error> {
    stream
        .map_err(anyhow::Error::new)
        .try_for_each_concurrent(None, |event| async {
            let cobalt_sender = cobalt_sender.clone();
            match event {
                PackageCacheRequest::Get { meta_far_blob, needed_blobs, dir, responder } => {
                    let id = serve_id.fetch_add(1, Ordering::SeqCst);
                    let meta_far_blob: BlobInfo = meta_far_blob.into();
                    let node = get_node.create_child(id.to_string());
                    let trace_id = ftrace::Id::random();
                    let guard = ftrace::async_enter!(
                        trace_id,
                        "app",
                        "cache_get",
                        "meta_far_blob_id" => meta_far_blob.blob_id.to_string().as_str(),
                        // An async duration cannot have multiple concurrent child async durations
                        // so we include the id as metadata to manually determine the
                        // relationship.
                        "trace_id" => u64::from(trace_id)
                    );
                    let response = get(
                        package_index.as_ref(),
                        base_packages.as_ref(),
                        executability_restrictions,
                        non_static_allow_list.as_ref(),
                        &blobfs,
                        meta_far_blob,
                        needed_blobs,
                        dir.map(|dir| (dir, scope.clone())),
                        cobalt_sender,
                        &node,
                    )
                    .await;
                    guard.end(&[ftrace::ArgValue::of(
                        "status",
                        Status::from(response).to_string().as_str(),
                    )]);
                    drop(node);
                    responder.send(response.map_err(|status| status.into_raw()))?;
                }
                PackageCacheRequest::Open { meta_far_blob_id, dir, responder } => {
                    let meta_far: Hash = BlobId::from(meta_far_blob_id).into();
                    let trace_id = ftrace::Id::random();
                    let guard = ftrace::async_enter!(
                        trace_id,
                        "app",
                        "cache_open",
                        "meta_far_blob_id" => meta_far.to_string().as_str()
                    );
                    let response = open(
                        package_index.as_ref(),
                        base_packages.as_ref(),
                        executability_restrictions,
                        non_static_allow_list.as_ref(),
                        scope.clone(),
                        &blobfs,
                        meta_far,
                        dir,
                        cobalt_sender,
                    )
                    .await;
                    guard.end(&[ftrace::ArgValue::of(
                        "status",
                        Status::from(response).to_string().as_str(),
                    )]);
                    responder.send(response.map_err(|status| status.into_raw()))?;
                }
                PackageCacheRequest::BasePackageIndex { iterator, control_handle: _ } => {
                    let stream = iterator.into_stream()?;
                    serve_base_package_index(BASE_PACKAGE_HOST, Arc::clone(&base_packages), stream)
                        .await;
                }
                PackageCacheRequest::CachePackageIndex { iterator, control_handle: _ } => {
                    let stream = iterator.into_stream()?;
                    serve_cache_package_index(cache_packages.clone(), stream).await;
                }
                PackageCacheRequest::Sync { responder } => {
                    responder.send(blobfs.sync().await.map_err(|e| {
                        error!("error syncing blobfs: {:#}", anyhow!(e));
                        Status::INTERNAL.into_raw()
                    }))?;
                }
            }

            Ok(())
        })
        .await
}

pub(crate) enum PackageStatus {
    Base,
    Active(fuchsia_pkg::PackageName),
    Other,
}

pub(crate) async fn get_package_status(
    base_packages: &BasePackages,
    package_index: &async_lock::RwLock<PackageIndex>,
    package: &fuchsia_hash::Hash,
) -> PackageStatus {
    if base_packages.is_base_package(*package) {
        return PackageStatus::Base;
    }

    match package_index.read().await.get_name_if_active(package) {
        Some(name) => PackageStatus::Active(name.clone()),
        None => PackageStatus::Other,
    }
}

pub(crate) enum ExecutabilityStatus {
    Allowed,
    Forbidden,
}

pub(crate) fn executability_status(
    executability_restrictions: system_image::ExecutabilityRestrictions,
    package_status: &PackageStatus,
    non_static_allow_list: &system_image::NonStaticAllowList,
) -> ExecutabilityStatus {
    use {system_image::ExecutabilityRestrictions::*, ExecutabilityStatus::*, PackageStatus::*};
    match (executability_restrictions, package_status) {
        (Enforce, Base) => Allowed,
        (Enforce, Active(name)) => {
            if non_static_allow_list.allows(name) {
                Allowed
            } else {
                Forbidden
            }
        }
        (Enforce, Other) => Forbidden,
        (DoNotEnforce, _) => Allowed,
    }
}

fn make_pkgdir_flags(executability_status: ExecutabilityStatus) -> fio::OpenFlags {
    use ExecutabilityStatus::*;
    fio::OpenFlags::RIGHT_READABLE
        | match executability_status {
            Allowed => fio::OpenFlags::RIGHT_EXECUTABLE,
            Forbidden => fio::OpenFlags::empty(),
        }
}

#[allow(clippy::too_many_arguments)]
/// Fetch a package, and optionally open it.
async fn get(
    package_index: &async_lock::RwLock<PackageIndex>,
    base_packages: &BasePackages,
    executability_restrictions: system_image::ExecutabilityRestrictions,
    non_static_allow_list: &system_image::NonStaticAllowList,
    blobfs: &blobfs::Client,
    meta_far_blob: BlobInfo,
    needed_blobs: ServerEnd<NeededBlobsMarker>,
    dir_and_scope: Option<(ServerEnd<fio::DirectoryMarker>, package_directory::ExecutionScope)>,
    mut cobalt_sender: ProtocolSender<MetricEvent>,
    node: &finspect::Node,
) -> Result<(), Status> {
    let _time_prop = node.create_int("started-time", zx::Time::get_monotonic().into_nanos());
    let _id_prop = node.create_string("meta-far-id", meta_far_blob.blob_id.to_string());
    let _length_prop = node.create_uint("meta-far-length", meta_far_blob.length);

    let needed_blobs = needed_blobs.into_stream().map_err(|_| Status::INTERNAL)?;

    let (root_dir, package_status) =
        match get_package_status(base_packages, package_index, &meta_far_blob.blob_id.into()).await
        {
            ps @ PackageStatus::Base | ps @ PackageStatus::Active(_) => {
                let () = needed_blobs.control_handle().shutdown_with_epitaph(Status::OK);
                (None, ps)
            }
            PackageStatus::Other => {
                let (root_dir, name) =
                    serve_needed_blobs(needed_blobs, meta_far_blob, package_index, blobfs, node)
                        .await
                        .map_err(|e| {
                            error!(
                                "error while caching package {}: {:#}",
                                meta_far_blob.blob_id,
                                anyhow!(e)
                            );
                            cobalt_sender.send(
                                MetricEvent::builder(metrics::PKG_CACHE_OPEN_MIGRATED_METRIC_ID)
                                    .with_event_codes(
                                        metrics::PkgCacheOpenMigratedMetricDimensionResult::Io,
                                    )
                                    .as_occurrence(1),
                            );
                            Status::UNAVAILABLE
                        })?;
                let package_status = if let Some(name) = name {
                    PackageStatus::Active(name)
                } else {
                    PackageStatus::Other
                };
                (Some(root_dir), package_status)
            }
        };

    if let Some((dir, scope)) = dir_and_scope {
        let root_dir = if let Some(root_dir) = root_dir {
            root_dir
        } else {
            package_directory::RootDir::new(blobfs.clone(), meta_far_blob.blob_id.into())
                .await
                .map_err(|e| {
                    error!("get: creating RootDir {}: {:#}", meta_far_blob.blob_id, anyhow!(e));
                    cobalt_sender.send(
                        MetricEvent::builder(metrics::PKG_CACHE_OPEN_MIGRATED_METRIC_ID)
                            .with_event_codes(
                                metrics::PkgCacheOpenMigratedMetricDimensionResult::Io,
                            )
                            .as_occurrence(1),
                    );
                    Status::INTERNAL
                })?
        };
        let () = Arc::new(root_dir).open(
            scope,
            make_pkgdir_flags(executability_status(
                executability_restrictions,
                &package_status,
                non_static_allow_list,
            )),
            vfs::path::Path::dot(),
            dir.into_channel().into(),
        );
    }

    cobalt_sender.send(
        MetricEvent::builder(metrics::PKG_CACHE_OPEN_MIGRATED_METRIC_ID)
            .with_event_codes(metrics::PkgCacheOpenMigratedMetricDimensionResult::Success)
            .as_occurrence(1),
    );
    Ok(())
}

#[allow(clippy::too_many_arguments)]
/// Open a package directory.
async fn open(
    package_index: &async_lock::RwLock<PackageIndex>,
    base_packages: &BasePackages,
    executability_restrictions: system_image::ExecutabilityRestrictions,
    non_static_allow_list: &system_image::NonStaticAllowList,
    scope: package_directory::ExecutionScope,
    blobfs: &blobfs::Client,
    meta_far: Hash,
    dir_request: ServerEnd<fio::DirectoryMarker>,
    mut cobalt_sender: ProtocolSender<MetricEvent>,
) -> Result<(), Status> {
    let package_status = match get_package_status(base_packages, package_index, &meta_far).await {
        PackageStatus::Other => {
            cobalt_sender.send(
                MetricEvent::builder(metrics::PKG_CACHE_OPEN_MIGRATED_METRIC_ID)
                    .with_event_codes(metrics::PkgCacheOpenMigratedMetricDimensionResult::NotFound)
                    .as_occurrence(1),
            );
            return Err(Status::NOT_FOUND);
        }
        ps @ PackageStatus::Base | ps @ PackageStatus::Active(_) => ps,
    };

    let () = package_directory::serve(
        scope,
        blobfs.clone(),
        meta_far,
        make_pkgdir_flags(executability_status(
            executability_restrictions,
            &package_status,
            non_static_allow_list,
        )),
        dir_request,
    )
    .map_err(|e| {
        error!("open: error serving package {}: {:#}", meta_far, anyhow!(e));
        cobalt_sender.send(
            MetricEvent::builder(metrics::PKG_CACHE_OPEN_MIGRATED_METRIC_ID)
                .with_event_codes(metrics::PkgCacheOpenMigratedMetricDimensionResult::Io)
                .as_occurrence(1),
        );
        Status::INTERNAL
    })
    .await?;

    cobalt_sender.send(
        MetricEvent::builder(metrics::PKG_CACHE_OPEN_MIGRATED_METRIC_ID)
            .with_event_codes(metrics::PkgCacheOpenMigratedMetricDimensionResult::Success)
            .as_occurrence(1),
    );
    Ok(())
}

#[derive(thiserror::Error, Debug)]
enum ServeNeededBlobsError {
    #[error("protocol violation: request stream terminated unexpectedly in {0}")]
    UnexpectedClose(&'static str),

    #[error("protocol violation: expected {expected} request, got {received}")]
    UnexpectedRequest { received: &'static str, expected: &'static str },

    #[error("protocol violation: while reading next request")]
    ReceiveRequest(#[source] fidl::Error),

    #[error("protocol violation: while responding to last request")]
    SendResponse(#[source] fidl::Error),

    #[error("the blob {0} is not needed")]
    BlobNotNeeded(Hash),

    #[error("the operation was aborted by the caller")]
    Aborted,

    #[error("while updating package index install state")]
    CompleteInstall(#[from] CompleteInstallError),

    #[error("while updating package index with meta far info")]
    FulfillMetaFar(#[from] FulfillMetaFarError),

    #[error("while adding needed content blobs to the iterator")]
    SendNeededContentBlobs(#[source] futures::channel::mpsc::SendError),

    #[error("while adding needed subpackage meta.fars to the iterator")]
    SendNeededSubpackageBlobs(#[source] futures::channel::mpsc::SendError),

    #[error("while creating a RootDir for a subpackage")]
    CreateSubpackageRootDir(#[source] package_directory::Error),

    #[error("while reading the subpackages of a package")]
    ReadSubpackages(#[source] package_directory::SubpackagesError),

    #[error(
        "handle_open_blobs finished writing all the needed blobs but still had {count} \
             outstanding blob write futures. This should be impossible"
    )]
    OutstandingBlobWritesWhenHandleOpenBlobsFinished { count: usize },

    #[error("while recording some of a package's subpackage blobs")]
    RecordingSubpackageBlobs(#[source] anyhow::Error),

    #[error("while recording some of a package's content blobs")]
    RecordingContentBlobs(#[source] anyhow::Error),

    #[error("client signaled blob {0} was written before client opened said blob")]
    BlobWrittenBeforeOpened(BlobId),

    #[error("client signaled blob {0} was written but blobfs does not have it")]
    BlobWrittenButMissing(BlobId),

    #[error("client signaled blob {wrong_blob} was written but meta.far was {meta_far}")]
    WrongMetaFarBlobWritten { wrong_blob: BlobId, meta_far: BlobId },
}

/// Adds all of a package's content and subpackage blobs (discovered during the caching process by
/// MissingBlobs) to the PackageIndex, protecting them from GC. MissingBlobs waits for the Future
/// returned by `record` to complete (so waits until the blobs have been added to the PackageIndex)
/// before sending the blobs out over the paired receiver, which occurs before the blobs are sent
/// out over the missing blobs iterator, which occurs before the blobs are written via
/// NeededBlobs.OpenBlob, so the blobs of a package being cached should always be protected from
/// GC.
#[derive(Debug)]
struct IndexBlobRecorder<'a> {
    package_index: &'a async_lock::RwLock<PackageIndex>,
    meta_far: Hash,
}

impl<'a> missing_blobs::BlobRecorder for IndexBlobRecorder<'a> {
    fn record(
        &self,
        blobs: HashSet<Hash>,
    ) -> futures::future::BoxFuture<'_, Result<(), anyhow::Error>> {
        async move { Ok(self.package_index.write().await.add_blobs(self.meta_far, blobs)?) }.boxed()
    }
}

/// Implements the fuchsia.pkg.NeededBlobs protocol, which represents the transaction for caching a
/// particular package.
///
/// Clients should start by requesting to `OpenMetaBlob()`, and fetch and write the metadata blob
/// if needed. Once written, `GetMissingBlobs()` should be used to determine which content blobs
/// need fetched and written using `OpenBlob()`. Violating the expected protocol state will result
/// in the channel being closed by the package cache with a `ZX_ERR_BAD_STATE` epitaph and aborting
/// the package cache operation.
///
/// Once all needed blobs are written by the client, the package cache will complete the pending
/// [`PackageCache.Get`] request and close this channel with a `ZX_OK` epitaph.
///
/// Returns the package's name if the package was activated in the dynamic index.
async fn serve_needed_blobs(
    mut stream: NeededBlobsRequestStream,
    meta_far_info: BlobInfo,
    package_index: &async_lock::RwLock<PackageIndex>,
    blobfs: &blobfs::Client,
    node: &finspect::Node,
) -> Result<
    (package_directory::RootDir<blobfs::Client>, Option<fuchsia_pkg::PackageName>),
    ServeNeededBlobsError,
> {
    let state = node.create_string("state", "need-meta-far");
    let res = async {
        // Step 1: Open and write the meta.far, or determine it is not needed.
        let root_dir =
            handle_open_meta_blob(&mut stream, meta_far_info, blobfs, package_index, &state)
                .await?;

        // TODO(fxbug.dev/112579) Move the fulfill_meta_far_blob call out of handle_open_meta_blob
        // to avoid setting the content blobs twice in the package index.
        let (missing_blobs, missing_blobs_recv) = missing_blobs::MissingBlobs::new(
            blobfs.clone(),
            &root_dir,
            Box::new(IndexBlobRecorder { package_index, meta_far: meta_far_info.blob_id.into() }),
        )
        .await?;

        // Step 2: Determine which data blobs are needed and report them to the client.
        let serve_iterator = handle_get_missing_blobs(&mut stream, missing_blobs_recv).await?;

        state.set("need-content-blobs");

        // Step 3: Open and write all needed data blobs.
        let () = handle_open_blobs(&mut stream, missing_blobs, blobfs, node).await?;

        let () = serve_iterator.await;
        Ok(root_dir)
    }
    .await;

    let res = match res {
        Ok(root_dir) => Ok((
            root_dir,
            package_index.write().await.complete_install(meta_far_info.blob_id.into())?,
        )),
        Err(e) => {
            package_index.write().await.cancel_install(&meta_far_info.blob_id.into());
            Err(e)
        }
    };

    // TODO in the Err(_) case, a responder was likely dropped, which would have already shutdown
    // the stream without our custom epitaph value.  Need to find a nice way to always shutdown
    // with a custom epitaph without copy/pasting something to every return site.

    let epitaph = match res {
        Ok(_) => Status::OK,
        Err(_) => Status::BAD_STATE,
    };
    stream.control_handle().shutdown_with_epitaph(epitaph);

    res
}

async fn handle_open_meta_blob(
    stream: &mut NeededBlobsRequestStream,
    meta_far_info: BlobInfo,
    blobfs: &blobfs::Client,
    package_index: &async_lock::RwLock<PackageIndex>,
    state: &StringProperty,
) -> Result<package_directory::RootDir<blobfs::Client>, ServeNeededBlobsError> {
    let hash = meta_far_info.blob_id.into();
    package_index.write().await.start_install(hash);
    let mut opened = false;

    loop {
        let () = match stream.try_next().await.map_err(ServeNeededBlobsError::ReceiveRequest)? {
            Some(NeededBlobsRequest::OpenMetaBlob { blob_type, responder }) => {
                // Do not fail if already opened to allow retries.
                opened = true;
                match open_blob(responder, blobfs, hash, blob_type).await? {
                    OpenBlobSuccess::AlreadyCached => break,
                    OpenBlobSuccess::Needed => Ok(()),
                }
            }
            Some(NeededBlobsRequest::BlobWritten { blob_id, responder }) => {
                let blob_id = BlobId::from(blob_id);
                if blob_id != meta_far_info.blob_id {
                    let _: Result<(), _> =
                        responder.send(Err(fpkg::BlobWrittenError::UnopenedBlob));
                    return Err(ServeNeededBlobsError::WrongMetaFarBlobWritten {
                        wrong_blob: blob_id,
                        meta_far: meta_far_info.blob_id,
                    });
                }
                if !opened {
                    let _: Result<(), _> =
                        responder.send(Err(fpkg::BlobWrittenError::UnopenedBlob));
                    return Err(ServeNeededBlobsError::BlobWrittenBeforeOpened(
                        meta_far_info.blob_id,
                    ));
                }
                if !blobfs.has_blob(&blob_id.into()).await {
                    let _: Result<(), _> = responder.send(Err(fpkg::BlobWrittenError::NotWritten));
                    return Err(ServeNeededBlobsError::BlobWrittenButMissing(
                        meta_far_info.blob_id,
                    ));
                }
                responder.send(Ok(())).map_err(ServeNeededBlobsError::SendResponse)?;
                break;
            }
            Some(NeededBlobsRequest::Abort { responder: _ }) => Err(ServeNeededBlobsError::Aborted),
            Some(other) => Err(ServeNeededBlobsError::UnexpectedRequest {
                received: other.method_name(),
                expected: if opened { "blob_written" } else { "open_meta_blob" },
            }),
            None => Err(ServeNeededBlobsError::UnexpectedClose("handle_open_meta_blob")),
        }?;
    }

    state.set("enumerate-missing-blobs");

    Ok(fulfill_meta_far_blob(package_index, blobfs, hash).await?)
}

async fn handle_get_missing_blobs(
    stream: &mut NeededBlobsRequestStream,
    missing_blobs: futures::channel::mpsc::UnboundedReceiver<Vec<fpkg::BlobInfo>>,
) -> Result<Task<()>, ServeNeededBlobsError> {
    let iterator = match stream.try_next().await.map_err(ServeNeededBlobsError::ReceiveRequest)? {
        Some(NeededBlobsRequest::GetMissingBlobs { iterator, control_handle: _ }) => Ok(iterator),
        Some(NeededBlobsRequest::Abort { responder: _ }) => Err(ServeNeededBlobsError::Aborted),
        Some(other) => Err(ServeNeededBlobsError::UnexpectedRequest {
            received: other.method_name(),
            expected: "get_missing_blobs",
        }),
        None => Err(ServeNeededBlobsError::UnexpectedClose("handle_get_missing_blobs")),
    }?;

    let iter_stream = iterator.into_stream().map_err(ServeNeededBlobsError::ReceiveRequest)?;

    // Start serving the iterator in the background and internally move on to the next state. If
    // this foreground task decides to bail out, this spawned task will be dropped which will abort
    // the iterator serving task.
    Ok(Task::spawn(
        serve_fidl_iterator_from_stream(
            iter_stream,
            missing_blobs,
            // Unlikely that more than 10 Vec<BlobInfo> (e.g. 5 RootDirs with subpackages)
            // will be written to missing_blobs between calls to Iterator::Next by the FIDL client,
            // so no need to increase this which would use (a tiny amount) more memory.
            10,
        )
        .unwrap_or_else(|e| {
            error!("error serving BlobInfoIteratorRequestStream: {:#}", anyhow!(e))
        }),
    ))
}

async fn handle_open_blobs(
    stream: &mut NeededBlobsRequestStream,
    mut missing_blobs: missing_blobs::MissingBlobs<'_>,
    blobfs: &blobfs::Client,
    node: &finspect::Node,
) -> Result<(), ServeNeededBlobsError> {
    let known_remaining_counter =
        node.create_uint("known_remaining", missing_blobs.count_not_cached() as u64);
    let mut open_blobs = HashSet::new();
    let open_counter = node.create_uint("open", 0);
    let written_counter = node.create_uint("written", 0);

    while missing_blobs.count_not_cached() != 0 {
        match stream.try_next().await.map_err(ServeNeededBlobsError::ReceiveRequest)? {
            Some(NeededBlobsRequest::OpenBlob { blob_id, blob_type, responder }) => {
                let blob_id = Hash::from(BlobId::from(blob_id));
                if !missing_blobs.should_cache(&blob_id) {
                    return Err(ServeNeededBlobsError::BlobNotNeeded(blob_id));
                }
                match open_blob(responder, blobfs, blob_id, blob_type).await {
                    Ok(OpenBlobSuccess::AlreadyCached) => {
                        // A prior call to OpenBlob may have added the blob to the set.
                        open_blobs.remove(&blob_id);
                        open_counter.set(open_blobs.len() as u64);
                        let () = missing_blobs.cache(&blob_id).await?;
                        known_remaining_counter.set(missing_blobs.count_not_cached() as u64);
                    }
                    Ok(OpenBlobSuccess::Needed) => {
                        open_blobs.insert(blob_id);
                        open_counter.set(open_blobs.len() as u64);
                    }
                    Err(e) => {
                        warn!("Error while opening content blob: {} {:#}", blob_id, anyhow!(e))
                    }
                }
            }
            Some(NeededBlobsRequest::BlobWritten { blob_id, responder }) => {
                let blob_id = Hash::from(BlobId::from(blob_id));
                if !open_blobs.remove(&blob_id) {
                    let _: Result<(), _> =
                        responder.send(Err(fpkg::BlobWrittenError::UnopenedBlob));
                    return Err(ServeNeededBlobsError::BlobWrittenBeforeOpened(blob_id.into()));
                }
                open_counter.set(open_blobs.len() as u64);
                if !blobfs.has_blob(&blob_id).await {
                    let _: Result<(), _> = responder.send(Err(fpkg::BlobWrittenError::NotWritten));
                    return Err(ServeNeededBlobsError::BlobWrittenButMissing(blob_id.into()));
                }
                let () = missing_blobs.cache(&blob_id).await?;
                known_remaining_counter.set(missing_blobs.count_not_cached() as u64);
                written_counter.add(1);
                responder.send(Ok(())).map_err(ServeNeededBlobsError::SendResponse)?;
            }
            Some(NeededBlobsRequest::Abort { responder }) => {
                drop(responder);
                return Err(ServeNeededBlobsError::Aborted);
            }
            Some(other) => {
                return Err(ServeNeededBlobsError::UnexpectedRequest {
                    received: other.method_name(),
                    expected: if open_blobs.is_empty() {
                        "open_blob"
                    } else {
                        "open_blob or blob_written"
                    },
                })
            }
            None => {
                return Err(ServeNeededBlobsError::UnexpectedClose("handle_open_blobs"));
            }
        }
    }

    if !open_blobs.is_empty() {
        Err(ServeNeededBlobsError::OutstandingBlobWritesWhenHandleOpenBlobsFinished {
            count: open_blobs.len(),
        })
    } else {
        Ok(())
    }
}

// Allow a function to generically respond to either an OpenMetaBlob or OpenBlob request.
type OpenBlobResponse =
    Result<Option<fidl::endpoints::ClientEnd<fio::FileMarker>>, fpkg::OpenBlobError>;
trait OpenBlobResponder {
    fn send(self, res: OpenBlobResponse) -> Result<(), fidl::Error>;
}
impl OpenBlobResponder for fpkg::NeededBlobsOpenBlobResponder {
    fn send(self, res: OpenBlobResponse) -> Result<(), fidl::Error> {
        self.send(res)
    }
}
impl OpenBlobResponder for fpkg::NeededBlobsOpenMetaBlobResponder {
    fn send(self, res: OpenBlobResponse) -> Result<(), fidl::Error> {
        self.send(res)
    }
}

#[derive(Debug)]
enum OpenBlobSuccess {
    AlreadyCached,
    Needed,
}

async fn open_blob(
    responder: impl OpenBlobResponder,
    blobfs: &blobfs::Client,
    blob_id: Hash,
    blob_type: fpkg::BlobType,
) -> Result<OpenBlobSuccess, ServeNeededBlobsError> {
    let create_res = blobfs.open_blob_for_write(&blob_id, blob_type).await;
    let is_readable = match &create_res {
        Err(blobfs::CreateError::AlreadyExists) => {
            // The blob may exist and be readable, or it may be in the process of being written.
            // Ensure we only indicate the blob is already present if we can actually open it for
            // read.
            blobfs.has_blob(&blob_id).await
        }
        _ => false,
    };

    use {blobfs::CreateError::*, fpkg::OpenBlobError as fErr, OpenBlobSuccess::*};
    let (fidl_resp, fn_ret) = match create_res {
        Ok(blob) => (Ok(Some(blob)), Ok(Needed)),
        Err(AlreadyExists) if is_readable => (Ok(None), Ok(AlreadyCached)),
        Err(AlreadyExists) => (Err(fErr::ConcurrentWrite), Ok(Needed)),
        Err(Io(_)) | Err(ConvertToClientEnd) => (Err(fErr::UnspecifiedIo), Ok(Needed)),
    };
    let () = responder.send(fidl_resp).map_err(ServeNeededBlobsError::SendResponse)?;
    fn_ret
}

/// Serves the `PackageIndexIteratorRequestStream` with as many base package index entries per
/// request as will fit in a fidl message.
async fn serve_base_package_index(
    package_host: &'static str,
    base_packages: Arc<BasePackages>,
    stream: PackageIndexIteratorRequestStream,
) {
    let mut package_entries = base_packages
        .root_paths_and_hashes()
        .map(|(path, hash)| PackageIndexEntry {
            package_url: fpkg::PackageUrl {
                url: format!("fuchsia-pkg://{}/{}", package_host, path.name()),
            },
            meta_far_blob_id: BlobId::from(*hash).into(),
        })
        .collect::<Vec<PackageIndexEntry>>();
    package_entries.sort_unstable_by(|a, b| a.package_url.url.cmp(&b.package_url.url));
    serve_fidl_iterator_from_slice(stream, package_entries).await.unwrap_or_else(|e| {
        error!("error serving PackageIndexIteratorRequestStream protocol: {:#}", anyhow!(e))
    })
}

/// Serves the `PackageIndexIteratorRequestStream` with as many cache package index entries per
/// request as will fit in a fidl message.
async fn serve_cache_package_index(
    cache_packages: Option<Arc<system_image::CachePackages>>,
    stream: PackageIndexIteratorRequestStream,
) {
    let package_entries = match cache_packages {
        Some(cache_packages) => cache_packages
            .contents()
            .map(|package_url| PackageIndexEntry {
                package_url: fpkg::PackageUrl {
                    url: package_url.as_unpinned().clone().clear_variant().to_string(),
                },
                meta_far_blob_id: BlobId::from(package_url.hash()).into(),
            })
            .collect::<Vec<PackageIndexEntry>>(),
        None => vec![],
    };
    serve_fidl_iterator_from_slice(stream, package_entries).await.unwrap_or_else(|e| {
        error!("error serving PackageIndexIteratorRequestStream protocol: {:#}", anyhow!(e))
    })
}

#[cfg(test)]
mod serve_needed_blobs_tests {
    use {
        super::*,
        assert_matches::assert_matches,
        fidl_fuchsia_pkg::{BlobInfoIteratorMarker, BlobInfoIteratorProxy, NeededBlobsProxy},
        fuchsia_hash::HashRangeFull,
        fuchsia_inspect as finspect,
        futures::{future, stream, stream::StreamExt as _},
    };

    #[fuchsia_async::run_singlethreaded(test)]
    async fn start_stop() {
        let (_, stream) = fidl::endpoints::create_proxy_and_stream::<NeededBlobsMarker>().unwrap();

        let meta_blob_info = BlobInfo { blob_id: [0; 32].into(), length: 0 };

        let (blobfs, _) = blobfs::Client::new_test();
        let inspector = finspect::Inspector::default();
        let package_index = Arc::new(async_lock::RwLock::new(PackageIndex::new(
            inspector.root().create_child("test_does_not_use_inspect "),
        )));

        assert_matches!(
            serve_needed_blobs(
                stream,
                meta_blob_info,
                &package_index,
                &blobfs,
                &inspector.root().create_child("test-node-name"),
            )
            .await,
            Err(ServeNeededBlobsError::UnexpectedClose("handle_open_meta_blob"))
        );
    }

    fn spawn_serve_needed_blobs_with_mocks(
        meta_blob_info: BlobInfo,
    ) -> (Task<Result<(), ServeNeededBlobsError>>, NeededBlobsProxy, blobfs::Mock) {
        let (proxy, stream) =
            fidl::endpoints::create_proxy_and_stream::<NeededBlobsMarker>().unwrap();

        let (blobfs, blobfs_mock) = blobfs::Client::new_mock();
        let inspector = finspect::Inspector::default();
        let package_index = Arc::new(async_lock::RwLock::new(PackageIndex::new(
            inspector.root().create_child("test_does_not_use_inspect "),
        )));

        (
            Task::spawn(async move {
                serve_needed_blobs(
                    stream,
                    meta_blob_info,
                    &package_index,
                    &blobfs,
                    &inspector.root().create_child("test-node-name"),
                )
                .await
                .map(|_| ())
            }),
            proxy,
            blobfs_mock,
        )
    }

    struct FakeOpenBlobResponse(Option<OpenBlobResponse>);

    struct FakeOpenBlobResponder<'a> {
        // Response is written to through send(). It is never intended to read.
        #[allow(dead_code)]
        response: &'a mut FakeOpenBlobResponse,
    }

    impl FakeOpenBlobResponse {
        fn new() -> Self {
            Self(None)
        }
        fn responder(&mut self) -> FakeOpenBlobResponder<'_> {
            FakeOpenBlobResponder { response: self }
        }
        fn take(self) -> OpenBlobResponse {
            self.0.unwrap()
        }
    }

    impl OpenBlobResponder for FakeOpenBlobResponder<'_> {
        fn send(self, res: OpenBlobResponse) -> Result<(), fidl::Error> {
            self.response.0 = Some(res);
            Ok(())
        }
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn open_blob_handles_io_open_error() {
        // Provide open_write_blob a closed blobfs and file stream to trigger a PEER_CLOSED IO
        // error.
        let (blobfs, _) = blobfs::Client::new_test();

        let mut response = FakeOpenBlobResponse::new();
        let res =
            open_blob(response.responder(), &blobfs, [0; 32].into(), fpkg::BlobType::Uncompressed)
                .await;

        // The operation should succeed, to allow retries, but it should report the failure to the
        // fidl responder.
        assert_matches!(res, Ok(OpenBlobSuccess::Needed));
        assert_eq!(response.take(), Err(fpkg::OpenBlobError::UnspecifiedIo));
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn expects_open_meta_blob() {
        let meta_blob_info = BlobInfo { blob_id: [0; 32].into(), length: 0 };

        let (task, proxy, blobfs) = spawn_serve_needed_blobs_with_mocks(meta_blob_info);

        let (iter, iter_server_end) =
            fidl::endpoints::create_proxy::<BlobInfoIteratorMarker>().unwrap();
        proxy.get_missing_blobs(iter_server_end).unwrap();
        assert_matches!(
            iter.next().await,
            Err(fidl::Error::ClientChannelClosed { status: Status::PEER_CLOSED, .. })
        );

        assert_matches!(
            task.await,
            Err(ServeNeededBlobsError::UnexpectedRequest {
                received: "get_missing_blobs",
                expected: "open_meta_blob"
            })
        );
        blobfs.expect_done().await;
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn expects_open_meta_blob_before_blob_written() {
        let meta_blob_info = BlobInfo { blob_id: [0; 32].into(), length: 0 };
        let (task, proxy, blobfs) = spawn_serve_needed_blobs_with_mocks(meta_blob_info);

        assert_matches!(
            proxy.blob_written(&BlobId::from([0; 32]).into()).await.unwrap(),
            Err(fpkg::BlobWrittenError::UnopenedBlob)
        );

        assert_matches!(
            task.await,
            Err(ServeNeededBlobsError::BlobWrittenBeforeOpened(hash)) if hash == [0; 32].into()
        );
        blobfs.expect_done().await;
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn expects_open_meta_blob_once() {
        let meta_blob_info = BlobInfo { blob_id: [0; 32].into(), length: 4 };
        let (serve_needed_task, proxy, mut blobfs) =
            spawn_serve_needed_blobs_with_mocks(meta_blob_info);

        // Open a needed meta FAR blob and write it.
        let (serve_meta_task, ()) = future::join(
            // serve_meta_task does not complete until later.
            #[allow(clippy::async_yields_async)]
            async {
                blobfs.expect_create_blob([0; 32].into()).await.expect_payload(b"test").await;
                blobfs.expect_readable_missing_checks(&[[0; 32].into()], &[]).await;

                // serve_needed_blobs parses the meta far after it is written.  Feed that logic a
                // valid, minimal far that doesn't actually correlate to what we just wrote.
                serve_minimal_far(&mut blobfs, [0; 32].into()).await
            },
            async {
                let blob = proxy
                    .open_meta_blob(fpkg::BlobType::Uncompressed)
                    .await
                    .expect("open_meta_blob failed")
                    .expect("open_meta_blob error")
                    .expect("meta blob not cached")
                    .into_proxy()
                    .unwrap();

                let () = blob
                    .resize(4)
                    .await
                    .expect("resize failed")
                    .map_err(Status::from_raw)
                    .expect("resize error");
                let _: u64 = blob
                    .write(b"test")
                    .await
                    .expect("write failed")
                    .map_err(Status::from_raw)
                    .expect("write error");
                let () = blob
                    .close()
                    .await
                    .expect("close failed")
                    .map_err(Status::from_raw)
                    .expect("close error");
                drop(blob);

                let () = proxy
                    .blob_written(&BlobId::from([0; 32]).into())
                    .await
                    .expect("blob_written failed")
                    .expect("blob_written error");
            },
        )
        .await;

        // Trying to open the meta FAR blob again after writing it successfully is a protocol
        // violation.
        assert_matches!(
            proxy.open_meta_blob(fpkg::BlobType::Uncompressed).await,
            Err(fidl::Error::ClientChannelClosed { status: Status::PEER_CLOSED, .. })
        );

        assert_matches!(
            serve_needed_task.await,
            Err(ServeNeededBlobsError::UnexpectedRequest {
                received: "open_meta_blob",
                expected: "get_missing_blobs"
            })
        );
        let () = serve_meta_task.await;
        blobfs.expect_done().await;
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn meta_far_blob_written_wrong_hash() {
        let meta_blob_info = BlobInfo { blob_id: [0; 32].into(), length: 4 };
        let (serve_needed_task, proxy, mut blobfs) =
            spawn_serve_needed_blobs_with_mocks(meta_blob_info);

        let ((), ()) = future::join(
            async {
                blobfs.expect_create_blob([0; 32].into()).await.expect_payload(b"test").await;
            },
            async {
                let blob = proxy
                    .open_meta_blob(fpkg::BlobType::Uncompressed)
                    .await
                    .expect("open_meta_blob failed")
                    .expect("open_meta_blob error")
                    .expect("meta blob not cached")
                    .into_proxy()
                    .unwrap();

                let () = blob
                    .resize(4)
                    .await
                    .expect("resize failed")
                    .map_err(Status::from_raw)
                    .expect("resize error");
                let _: u64 = blob
                    .write(b"test")
                    .await
                    .expect("write failed")
                    .map_err(Status::from_raw)
                    .expect("write error");
                let () = blob
                    .close()
                    .await
                    .expect("close failed")
                    .map_err(Status::from_raw)
                    .expect("close error");
                drop(blob);

                assert_matches!(
                    proxy
                        .blob_written(&BlobId::from([1; 32]).into())
                        .await
                        .expect("blob_written failed"),
                    Err(fpkg::BlobWrittenError::UnopenedBlob)
                );
            },
        )
        .await;

        // Calling BlobWritten for the wrong hash should close the channel, so calling BlobWritten
        // for the correct hash now should fail.
        assert_matches!(
            proxy.blob_written(&BlobId::from([0; 32]).into()).await,
            Err(fidl::Error::ClientChannelClosed { status: Status::BAD_STATE, .. })
        );

        assert_matches!(
            serve_needed_task.await,
            Err(ServeNeededBlobsError::WrongMetaFarBlobWritten {
                wrong_blob,
                meta_far
            }) if wrong_blob == [1; 32].into() && meta_far == [0; 32].into()
        );
        blobfs.expect_done().await;
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn meta_far_blob_written_but_not_in_blobfs() {
        let meta_blob_info = BlobInfo { blob_id: [0; 32].into(), length: 4 };
        let (serve_needed_task, proxy, mut blobfs) =
            spawn_serve_needed_blobs_with_mocks(meta_blob_info);

        let ((), ()) = future::join(
            async {
                blobfs.expect_create_blob([0; 32].into()).await.expect_close().await;
                blobfs.expect_readable_missing_checks(&[], &[[0; 32].into()]).await;
            },
            async {
                let _: fidl::endpoints::ClientEnd<fio::FileMarker> = proxy
                    .open_meta_blob(fpkg::BlobType::Uncompressed)
                    .await
                    .expect("open_meta_blob failed")
                    .expect("open_meta_blob error")
                    .expect("meta blob not cached");

                assert_matches!(
                    proxy
                        .blob_written(&BlobId::from([0; 32]).into())
                        .await
                        .expect("blob_written failed"),
                    Err(fpkg::BlobWrittenError::NotWritten)
                );
            },
        )
        .await;

        // The invalid BlobWritten call should close the channel, so trying to open the meta far
        // again should fail.
        assert_matches!(
            proxy.open_meta_blob(fpkg::BlobType::Uncompressed).await,
            Err(fidl::Error::ClientChannelClosed { status: Status::BAD_STATE, .. })
        );

        assert_matches!(
            serve_needed_task.await,
            Err(ServeNeededBlobsError::BlobWrittenButMissing(hash)) if hash == [0; 32].into()
        );
        blobfs.expect_done().await;
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn handles_present_meta_blob() {
        let meta_blob_info = BlobInfo { blob_id: [0; 32].into(), length: 0 };
        let (serve_needed_task, proxy, mut blobfs) =
            spawn_serve_needed_blobs_with_mocks(meta_blob_info);

        // Try to open the meta FAR blob, but report it is no longer needed.
        let (serve_meta_task, ()) = future::join(
            // serve_meta_task does not complete until later.
            #[allow(clippy::async_yields_async)]
            async {
                blobfs.expect_create_blob([0; 32].into()).await.fail_open_with_already_exists();
                blobfs
                    .expect_open_blob([0; 32].into())
                    .await
                    .succeed_open_with_blob_readable()
                    .await;

                // serve_needed_blobs parses the meta far after it is written.  Feed that logic a
                // valid, minimal far that doesn't actually correlate to what we just wrote.
                serve_minimal_far(&mut blobfs, [0; 32].into()).await
            },
            async {
                assert_eq!(
                    proxy
                        .open_meta_blob(fpkg::BlobType::Uncompressed)
                        .await
                        .expect("open_meta_blob failed")
                        .expect("open_meta_blob error"),
                    None
                );
            },
        )
        .await;

        // Trying to open the meta FAR blob again after being told it is not needed is a protocol
        // violation.
        assert_matches!(
            proxy.open_meta_blob(fpkg::BlobType::Uncompressed).await,
            Err(fidl::Error::ClientChannelClosed { status: Status::PEER_CLOSED, .. })
        );

        assert_matches!(
            serve_needed_task.await,
            Err(ServeNeededBlobsError::UnexpectedRequest {
                received: "open_meta_blob",
                expected: "get_missing_blobs"
            })
        );
        let () = serve_meta_task.await;
        blobfs.expect_done().await;
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn allows_retrying_nonfatal_open_meta_blob_errors() {
        let meta_blob_info = BlobInfo { blob_id: [0; 32].into(), length: 1 };
        let (serve_needed_task, proxy, mut blobfs) =
            spawn_serve_needed_blobs_with_mocks(meta_blob_info);

        // Try to open the meta FAR blob, but report it is already being written concurrently.
        let ((), ()) = future::join(
            async {
                blobfs.expect_create_blob([0; 32].into()).await.fail_open_with_already_exists();
                blobfs.expect_open_blob([0; 32].into()).await.fail_open_with_not_readable().await;
            },
            async {
                assert_matches!(
                    proxy.open_meta_blob(fpkg::BlobType::Uncompressed).await,
                    Ok(Err(fidl_fuchsia_pkg::OpenBlobError::ConcurrentWrite))
                );
            },
        )
        .await;

        // Try to write the meta FAR blob, but report the written contents are corrupt.
        let ((), ()) = future::join(
            async {
                blobfs.expect_create_blob([0; 32].into()).await.fail_write_with_corrupt().await;
            },
            async {
                let blob = proxy
                    .open_meta_blob(fpkg::BlobType::Uncompressed)
                    .await
                    .expect("open_meta_blob failed")
                    .expect("open_meta_blob error")
                    .expect("blob already cached")
                    .into_proxy()
                    .unwrap();

                let () = blob
                    .resize(1)
                    .await
                    .expect("resize failed")
                    .map_err(Status::from_raw)
                    .expect("resize error");
                let result =
                    blob.write(&[0]).await.expect("write failed").map_err(Status::from_raw);
                assert_eq!(result, Err(Status::IO_DATA_INTEGRITY));
                assert_matches!(
                    blob.close().await,
                    Err(fidl::Error::ClientChannelClosed { status: Status::PEER_CLOSED, .. })
                );
            },
        )
        .await;

        // Open the meta FAR blob for write, but then close it (a non-fatal error)
        let ((), ()) = future::join(
            async {
                blobfs.expect_create_blob([0; 32].into()).await.expect_close().await;
            },
            async {
                let blob = proxy
                    .open_meta_blob(fpkg::BlobType::Uncompressed)
                    .await
                    .expect("open_meta_blob failed")
                    .expect("open_meta_blob error")
                    .expect("blob already cached")
                    .into_proxy()
                    .unwrap();

                let () = blob
                    .close()
                    .await
                    .expect("close failed")
                    .map_err(Status::from_raw)
                    .expect("close error");
            },
        )
        .await;

        // Operation succeeds after blobfs cooperates.
        let (serve_meta_task, ()) = future::join(
            // serve_meta_task does not complete until later.
            #[allow(clippy::async_yields_async)]
            async {
                blobfs.expect_create_blob([0; 32].into()).await.expect_payload(&[0]).await;
                blobfs.expect_readable_missing_checks(&[[0; 32].into()], &[]).await;

                // serve_needed_blobs parses the meta far after it is written.  Feed that logic a
                // valid, minimal far that doesn't actually correlate to what we just wrote.
                serve_minimal_far(&mut blobfs, [0; 32].into()).await
            },
            async {
                let blob = proxy
                    .open_meta_blob(fpkg::BlobType::Uncompressed)
                    .await
                    .unwrap()
                    .unwrap()
                    .unwrap()
                    .into_proxy()
                    .unwrap();

                let () = blob
                    .resize(1)
                    .await
                    .expect("resize failed")
                    .map_err(Status::from_raw)
                    .expect("resize error");
                let _: u64 = blob
                    .write(&[0])
                    .await
                    .expect("write failed")
                    .map_err(Status::from_raw)
                    .expect("write error");
                let () = blob
                    .close()
                    .await
                    .expect("close failed")
                    .map_err(Status::from_raw)
                    .expect("close error");
                drop(blob);

                let () = proxy
                    .blob_written(&BlobId::from([0; 32]).into())
                    .await
                    .expect("blob_written failed")
                    .expect("blob_written error");
            },
        )
        .await;

        // Task moves to next state after retried write operation succeeds.
        assert_matches!(
            proxy.open_meta_blob(fpkg::BlobType::Uncompressed).await,
            Err(fidl::Error::ClientChannelClosed { status: Status::PEER_CLOSED, .. })
        );
        assert_matches!(
            serve_needed_task.await,
            Err(ServeNeededBlobsError::UnexpectedRequest {
                received: "open_meta_blob",
                expected: "get_missing_blobs"
            })
        );
        let () = serve_meta_task.await;
        blobfs.expect_done().await;
    }

    /// The returned task completes when the connection to the meta blob closes.
    pub(super) async fn serve_minimal_far(blobfs: &mut blobfs::Mock, meta_hash: Hash) -> Task<()> {
        let far_data = crate::test_utils::get_meta_far("fake-package", [], []);

        let blob = blobfs.expect_open_blob(meta_hash).await;
        Task::spawn(async move { blob.serve_contents(&far_data[..]).await })
    }

    /// The returned task completes when the connection to the meta blob closes, which is normally
    /// when the task serving the NeededBlobs stream completes.
    pub(super) async fn write_meta_blob(
        proxy: &NeededBlobsProxy,
        blobfs: &mut blobfs::Mock,
        meta_blob_info: BlobInfo,
        needed_blobs: impl IntoIterator<Item = Hash>,
    ) -> Task<()> {
        let far_data = crate::test_utils::get_meta_far("fake-package", needed_blobs, []);

        let (serve_contents, ()) = future::join(
            // serve_contents does not complete until later.
            #[allow(clippy::async_yields_async)]
            async {
                // Fail the create request, then succeed an open request that checks if the blob is
                // readable. The already_exists error could indicate that the blob is being
                // written, so pkg-cache needs to disambiguate the 2 cases.
                blobfs
                    .expect_create_blob(meta_blob_info.blob_id.into())
                    .await
                    .fail_open_with_already_exists();
                blobfs
                    .expect_open_blob(meta_blob_info.blob_id.into())
                    .await
                    .succeed_open_with_blob_readable()
                    .await;

                let blob = blobfs.expect_open_blob(meta_blob_info.blob_id.into()).await;

                // the serving task does not complete until later.
                #[allow(clippy::async_yields_async)]
                Task::spawn(async move { blob.serve_contents(&far_data[..]).await })
            },
            async {
                assert_matches!(
                    proxy.open_meta_blob(fpkg::BlobType::Uncompressed).await,
                    Ok(Ok(None))
                );
            },
        )
        .await;
        serve_contents
    }

    async fn collect_blob_info_iterator(proxy: BlobInfoIteratorProxy) -> Vec<BlobInfo> {
        let mut res = vec![];

        loop {
            let chunk = proxy.next().await.unwrap();

            if chunk.is_empty() {
                break;
            }

            res.extend(chunk.into_iter().map(BlobInfo::from));
        }

        res
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn discovers_and_reports_missing_blobs() {
        let meta_blob_info = BlobInfo { blob_id: [0; 32].into(), length: 0 };
        let (serve_needed_task, proxy, mut blobfs) =
            spawn_serve_needed_blobs_with_mocks(meta_blob_info);

        let expected = HashRangeFull::default().skip(1).take(2000).collect::<Vec<_>>();

        let serve_meta_task =
            write_meta_blob(&proxy, &mut blobfs, meta_blob_info, expected.iter().copied()).await;

        let ((), ()) = future::join(
            async {
                blobfs
                    .expect_filter_to_missing_blobs_with_readable_missing_ids(&[], &expected[..])
                    .await;
            },
            async {
                let (missing_blobs_iter, missing_blobs_iter_server_end) =
                    fidl::endpoints::create_proxy::<BlobInfoIteratorMarker>().unwrap();

                assert_matches!(proxy.get_missing_blobs(missing_blobs_iter_server_end), Ok(()));

                let missing_blobs = collect_blob_info_iterator(missing_blobs_iter).await;

                let expected = expected
                    .iter()
                    .cloned()
                    .map(|hash| BlobInfo { blob_id: hash.into(), length: 0 })
                    .collect::<Vec<_>>();
                assert_eq!(missing_blobs, expected);
            },
        )
        .await;

        drop(proxy);
        assert_matches!(
            serve_needed_task.await,
            Err(ServeNeededBlobsError::UnexpectedClose("handle_open_blobs"))
        );
        let () = serve_meta_task.await;
        blobfs.expect_done().await;
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn handles_no_missing_blobs() {
        let meta_blob_info = BlobInfo { blob_id: [0; 32].into(), length: 0 };
        let (serve_needed_task, proxy, mut blobfs) =
            spawn_serve_needed_blobs_with_mocks(meta_blob_info);

        let serve_meta_task = write_meta_blob(&proxy, &mut blobfs, meta_blob_info, vec![]).await;

        let (missing_blobs_iter, missing_blobs_iter_server_end) =
            fidl::endpoints::create_proxy::<BlobInfoIteratorMarker>().unwrap();
        assert_matches!(proxy.get_missing_blobs(missing_blobs_iter_server_end), Ok(()));
        let missing_blobs = collect_blob_info_iterator(missing_blobs_iter).await;
        assert_eq!(missing_blobs, vec![]);

        assert_matches!(serve_needed_task.await, Ok(()));
        assert_matches!(
            proxy.take_event_stream().next().await,
            Some(Err(fidl::Error::ClientChannelClosed { status: Status::OK, .. }))
        );
        let () = serve_meta_task.await;
        blobfs.expect_done().await;
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn fails_on_invalid_meta_far() {
        let meta_blob_info = BlobInfo { blob_id: [0; 32].into(), length: 0 };
        let (task, proxy, mut blobfs) = spawn_serve_needed_blobs_with_mocks(meta_blob_info);

        let bogus_far_data = b"this is not a far file";

        let ((), ()) = future::join(
            async {
                // Fail the create request, then succeed an open request that checks if the blob is
                // readable. The already_exists error could indicate that the blob is being
                // written, so pkg-cache need to disambiguate the 2 cases.
                blobfs
                    .expect_create_blob(meta_blob_info.blob_id.into())
                    .await
                    .fail_open_with_already_exists();
                blobfs
                    .expect_open_blob(meta_blob_info.blob_id.into())
                    .await
                    .succeed_open_with_blob_readable()
                    .await;

                blobfs
                    .expect_open_blob(meta_blob_info.blob_id.into())
                    .await
                    .serve_contents(&bogus_far_data[..])
                    .await;
            },
            async {
                assert_matches!(
                    proxy.open_meta_blob(fpkg::BlobType::Uncompressed).await,
                    Ok(Ok(None))
                );
            },
        )
        .await;

        drop(proxy);
        assert_matches!(
            task.await,
            Err(ServeNeededBlobsError::FulfillMetaFar(FulfillMetaFarError::CreateRootDir(
                package_directory::Error::ArchiveReader(fuchsia_archive::Error::InvalidMagic(_))
            )))
        );
        blobfs.expect_done().await;
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn dropping_needed_blobs_stops_missing_blob_iterator() {
        let meta_blob_info = BlobInfo { blob_id: [0; 32].into(), length: 0 };
        let (serve_needed_task, proxy, mut blobfs) =
            spawn_serve_needed_blobs_with_mocks(meta_blob_info);

        let missing = HashRangeFull::default().take(10).collect::<Vec<_>>();
        let serve_meta_task =
            write_meta_blob(&proxy, &mut blobfs, meta_blob_info, missing.iter().copied()).await;

        let ((), ()) = future::join(
            async {
                blobfs
                    .expect_filter_to_missing_blobs_with_readable_missing_ids(&[], &missing[..])
                    .await;
            },
            async {
                let (missing_blobs_iter, missing_blobs_iter_server_end) =
                    fidl::endpoints::create_proxy::<BlobInfoIteratorMarker>().unwrap();

                assert_matches!(proxy.get_missing_blobs(missing_blobs_iter_server_end), Ok(()));

                // Closing the needed blobs request stream terminates any spawned tasks.
                drop(proxy);
                assert_matches!(
                    missing_blobs_iter.next().await,
                    Err(fidl::Error::ClientChannelClosed { status: Status::PEER_CLOSED, .. })
                );
            },
        )
        .await;

        assert_matches!(
            serve_needed_task.await,
            Err(ServeNeededBlobsError::UnexpectedClose("handle_open_blobs"))
        );
        let () = serve_meta_task.await;
        blobfs.expect_done().await;
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn expects_get_missing_blobs_once() {
        let meta_blob_info = BlobInfo { blob_id: [0; 32].into(), length: 0 };
        let (serve_needed_task, proxy, mut blobfs) =
            spawn_serve_needed_blobs_with_mocks(meta_blob_info);

        let missing = HashRangeFull::default().take(10).collect::<Vec<_>>();
        let serve_meta_task =
            write_meta_blob(&proxy, &mut blobfs, meta_blob_info, missing.iter().copied()).await;

        // Enumerate the needs successfully once.
        let ((), ()) = future::join(
            async {
                blobfs
                    .expect_filter_to_missing_blobs_with_readable_missing_ids(&[], &missing[..])
                    .await;
            },
            async {
                let (missing_blobs_iter, missing_blobs_iter_server_end) =
                    fidl::endpoints::create_proxy::<BlobInfoIteratorMarker>().unwrap();

                assert_matches!(proxy.get_missing_blobs(missing_blobs_iter_server_end), Ok(()));

                collect_blob_info_iterator(missing_blobs_iter).await;
            },
        )
        .await;

        // Trying to enumerate the missing blobs again is a protocol violation.
        let (_missing_blobs_iter, missing_blobs_iter_server_end) =
            fidl::endpoints::create_proxy::<BlobInfoIteratorMarker>().unwrap();
        assert_matches!(proxy.get_missing_blobs(missing_blobs_iter_server_end), Ok(()));

        assert_matches!(
            serve_needed_task.await,
            Err(ServeNeededBlobsError::UnexpectedRequest {
                received: "get_missing_blobs",
                expected: "open_blob"
            })
        );
        let () = serve_meta_task.await;
        blobfs.expect_done().await;
    }

    pub(super) async fn enumerate_readable_missing_blobs(
        proxy: &NeededBlobsProxy,
        blobfs: &mut blobfs::Mock,
        readable: impl Iterator<Item = Hash>,
        missing: impl Iterator<Item = Hash>,
    ) {
        let readable = readable.collect::<Vec<_>>();
        let missing = missing.collect::<Vec<_>>();

        let ((), ()) = future::join(
            async {
                blobfs
                    .expect_filter_to_missing_blobs_with_readable_missing_ids(
                        &readable[..],
                        &missing[..],
                    )
                    .await;
            },
            async {
                let (missing_blobs_iter, missing_blobs_iter_server_end) =
                    fidl::endpoints::create_proxy::<BlobInfoIteratorMarker>().unwrap();

                assert_matches!(proxy.get_missing_blobs(missing_blobs_iter_server_end), Ok(()));

                let infos = collect_blob_info_iterator(missing_blobs_iter).await;
                let mut actual =
                    infos.into_iter().map(|info| info.blob_id.into()).collect::<Vec<Hash>>();
                actual.sort_unstable();
                assert_eq!(missing, actual);
            },
        )
        .await;
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn single_need() {
        let meta_blob_info = BlobInfo { blob_id: [1; 32].into(), length: 0 };
        let (serve_needed_task, proxy, mut blobfs) =
            spawn_serve_needed_blobs_with_mocks(meta_blob_info);

        let serve_meta_task =
            write_meta_blob(&proxy, &mut blobfs, meta_blob_info, vec![[2; 32].into()]).await;
        enumerate_readable_missing_blobs(
            &proxy,
            &mut blobfs,
            std::iter::empty(),
            vec![[2; 32].into()].into_iter(),
        )
        .await;

        let payload = b"single blob";

        let ((), ()) = future::join(
            async {
                blobfs.expect_create_blob([2; 32].into()).await.expect_payload(payload).await;
                blobfs.expect_readable_missing_checks(&[[2; 32].into()], &[]).await;
            },
            async {
                let blob = proxy
                    .open_blob(&BlobId::from([2; 32]).into(), fpkg::BlobType::Uncompressed)
                    .await
                    .expect("open_blob failed")
                    .expect("open_blob error")
                    .expect("blob not cached")
                    .into_proxy()
                    .unwrap();

                let () = blob
                    .resize(payload.len() as u64)
                    .await
                    .expect("resize failed")
                    .map_err(Status::from_raw)
                    .expect("resize error");
                let _: u64 = blob
                    .write(payload)
                    .await
                    .expect("write failed")
                    .map_err(Status::from_raw)
                    .expect("write error");
                let () = blob
                    .close()
                    .await
                    .expect("close failed")
                    .map_err(Status::from_raw)
                    .expect("close error");

                drop(blob);

                let () = proxy
                    .blob_written(&BlobId::from([2; 32]).into())
                    .await
                    .expect("blob_written failed")
                    .expect("blob_written error");
            },
        )
        .await;

        assert_matches!(serve_needed_task.await, Ok(()));
        assert_matches!(
            proxy.take_event_stream().next().await,
            Some(Err(fidl::Error::ClientChannelClosed { status: Status::OK, .. }))
        );
        let () = serve_meta_task.await;
        blobfs.expect_done().await;
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn open_blob_blob_present_on_second_call() {
        let meta_blob_info = BlobInfo { blob_id: [1; 32].into(), length: 0 };
        let (serve_needed_task, proxy, mut blobfs) =
            spawn_serve_needed_blobs_with_mocks(meta_blob_info);

        let serve_meta_task =
            write_meta_blob(&proxy, &mut blobfs, meta_blob_info, vec![[2; 32].into()]).await;
        enumerate_readable_missing_blobs(
            &proxy,
            &mut blobfs,
            std::iter::empty(),
            vec![[2; 32].into()].into_iter(),
        )
        .await;

        let ((), ()) = future::join(
            async {
                blobfs.expect_create_blob([2; 32].into()).await.expect_close().await;
                blobfs.expect_create_blob([2; 32].into()).await.fail_open_with_already_exists();
                blobfs
                    .expect_open_blob([2; 32].into())
                    .await
                    .succeed_open_with_blob_readable()
                    .await;
            },
            async {
                let _: fidl::endpoints::ClientEnd<fio::FileMarker> = proxy
                    .open_blob(&BlobId::from([2; 32]).into(), fpkg::BlobType::Uncompressed)
                    .await
                    .expect("open_blob failed")
                    .expect("open_blob error")
                    .expect("blob not cached");

                assert_eq!(
                    proxy
                        .open_blob(&BlobId::from([2; 32]).into(), fpkg::BlobType::Uncompressed)
                        .await
                        .expect("open_blob failed")
                        .expect("open_blob error"),
                    None
                );
            },
        )
        .await;

        assert_matches!(serve_needed_task.await, Ok(()));
        assert_matches!(
            proxy.take_event_stream().next().await,
            Some(Err(fidl::Error::ClientChannelClosed { status: Status::OK, .. }))
        );
        let () = serve_meta_task.await;
        blobfs.expect_done().await;
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn handles_many_content_blobs_that_need_written() {
        let meta_blob_info = BlobInfo { blob_id: [0; 32].into(), length: 0 };
        let (serve_needed_task, proxy, mut blobfs) =
            spawn_serve_needed_blobs_with_mocks(meta_blob_info);

        let content_blobs = || HashRangeFull::default().skip(1).take(100);

        let serve_meta_task =
            write_meta_blob(&proxy, &mut blobfs, meta_blob_info, content_blobs()).await;
        enumerate_readable_missing_blobs(&proxy, &mut blobfs, std::iter::empty(), content_blobs())
            .await;

        fn payload(hash: Hash) -> Vec<u8> {
            let hash_bytes = || hash.as_bytes().iter().copied();
            let len = hash_bytes().map(|n| n as usize).sum();
            assert!(len <= fio::MAX_BUF as usize);

            std::iter::repeat(hash_bytes()).flatten().take(len).collect()
        }

        let ((), ()) = future::join(
            async {
                for hash in content_blobs() {
                    blobfs.expect_create_blob(hash).await.expect_payload(&payload(hash)).await;
                }
                blobfs
                    .expect_readable_missing_checks(
                        content_blobs().collect::<Vec<_>>().as_slice(),
                        &[],
                    )
                    .await;
            },
            async {
                let () = stream::iter(content_blobs())
                    .for_each_concurrent(None, |hash| {
                        let open_fut = proxy
                            .open_blob(&BlobId::from(hash).into(), fpkg::BlobType::Uncompressed);
                        let proxy = &proxy;

                        async move {
                            let blob =
                                open_fut.await.unwrap().unwrap().unwrap().into_proxy().unwrap();

                            let payload = payload(hash);
                            let () = blob
                                .resize(payload.len() as u64)
                                .await
                                .expect("resize failed")
                                .map_err(Status::from_raw)
                                .expect("resize error");
                            let _: u64 = blob
                                .write(&payload)
                                .await
                                .expect("write failed")
                                .map_err(Status::from_raw)
                                .expect("write error");
                            let () = blob
                                .close()
                                .await
                                .expect("close failed")
                                .map_err(Status::from_raw)
                                .expect("close error");
                            drop(blob);

                            let () = proxy
                                .blob_written(&BlobId::from(hash).into())
                                .await
                                .expect("blob_written failed")
                                .expect("blob_written error");
                        }
                    })
                    .await;
            },
        )
        .await;

        assert_matches!(serve_needed_task.await, Ok(()));
        assert_matches!(
            proxy.take_event_stream().next().await,
            Some(Err(fidl::Error::ClientChannelClosed { status: Status::OK, .. }))
        );
        let () = serve_meta_task.await;
        blobfs.expect_done().await;
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn handles_many_content_blobs_that_are_already_present() {
        let meta_blob_info = BlobInfo { blob_id: [0; 32].into(), length: 0 };
        let (serve_needed_task, proxy, mut blobfs) =
            spawn_serve_needed_blobs_with_mocks(meta_blob_info);

        let content_blobs = || HashRangeFull::default().skip(1).take(100);

        let serve_meta_task =
            write_meta_blob(&proxy, &mut blobfs, meta_blob_info, content_blobs()).await;
        enumerate_readable_missing_blobs(&proxy, &mut blobfs, std::iter::empty(), content_blobs())
            .await;

        let ((), ()) = future::join(
            async {
                for hash in content_blobs() {
                    blobfs.expect_create_blob(hash).await.fail_open_with_already_exists();
                    blobfs.expect_open_blob(hash).await.succeed_open_with_blob_readable().await;
                }
            },
            async {
                let () = stream::iter(content_blobs())
                    .for_each(|hash| {
                        let open_fut = proxy
                            .open_blob(&BlobId::from(hash).into(), fpkg::BlobType::Uncompressed);

                        async move {
                            assert_eq!(open_fut.await.unwrap().unwrap(), None);
                        }
                    })
                    .await;
            },
        )
        .await;

        assert_matches!(serve_needed_task.await, Ok(()));
        assert_matches!(
            proxy.take_event_stream().next().await,
            Some(Err(fidl::Error::ClientChannelClosed { status: Status::OK, .. }))
        );
        let () = serve_meta_task.await;
        blobfs.expect_done().await;
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn content_blob_written_but_not_in_blobfs() {
        let meta_blob_info = BlobInfo { blob_id: [1; 32].into(), length: 0 };
        let (serve_needed_task, proxy, mut blobfs) =
            spawn_serve_needed_blobs_with_mocks(meta_blob_info);

        let serve_meta_task =
            write_meta_blob(&proxy, &mut blobfs, meta_blob_info, vec![[2; 32].into()]).await;
        enumerate_readable_missing_blobs(
            &proxy,
            &mut blobfs,
            std::iter::empty(),
            vec![[2; 32].into()].into_iter(),
        )
        .await;

        let ((), ()) = future::join(
            async {
                blobfs.expect_create_blob([2; 32].into()).await.expect_close().await;
                blobfs.expect_readable_missing_checks(&[], &[[2; 32].into()]).await;
            },
            async {
                let _: fidl::endpoints::ClientEnd<fio::FileMarker> = proxy
                    .open_blob(&BlobId::from([2; 32]).into(), fpkg::BlobType::Uncompressed)
                    .await
                    .expect("open_blob failed")
                    .expect("open_blob error")
                    .expect("blob not cached");

                assert_matches!(
                    proxy
                        .blob_written(&BlobId::from([2; 32]).into())
                        .await
                        .expect("blob_written failed"),
                    Err(fpkg::BlobWrittenError::NotWritten)
                );
            },
        )
        .await;

        assert_matches!(
            proxy.take_event_stream().next().await,
            Some(Err(fidl::Error::ClientChannelClosed { status: Status::BAD_STATE, .. }))
        );
        assert_matches!(
            serve_needed_task.await,
            Err(ServeNeededBlobsError::BlobWrittenButMissing(hash)) if hash == [2; 32].into()
        );
        let () = serve_meta_task.await;
        blobfs.expect_done().await;
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn content_blob_written_before_open_blob() {
        let meta_blob_info = BlobInfo { blob_id: [0; 32].into(), length: 0 };
        let (serve_needed_task, proxy, mut blobfs) =
            spawn_serve_needed_blobs_with_mocks(meta_blob_info);

        let serve_meta_task =
            write_meta_blob(&proxy, &mut blobfs, meta_blob_info, vec![[2; 32].into()]).await;
        enumerate_readable_missing_blobs(
            &proxy,
            &mut blobfs,
            std::iter::empty(),
            vec![[2; 32].into()].into_iter(),
        )
        .await;

        assert_matches!(
            proxy.blob_written(&BlobId::from([2; 32]).into()).await.expect("blob_written failed"),
            Err(fpkg::BlobWrittenError::UnopenedBlob)
        );

        assert_matches!(
            proxy.take_event_stream().next().await,
            Some(Err(fidl::Error::ClientChannelClosed { status: Status::BAD_STATE, .. }))
        );
        assert_matches!(
            serve_needed_task.await,
            Err(ServeNeededBlobsError::BlobWrittenBeforeOpened(hash)) if hash == [2; 32].into()
        );
        let () = serve_meta_task.await;
        blobfs.expect_done().await;
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn allows_retrying_nonfatal_open_blob_errors() {
        let meta_blob_info = BlobInfo { blob_id: [0; 32].into(), length: 0 };
        let (serve_needed_task, proxy, mut blobfs) =
            spawn_serve_needed_blobs_with_mocks(meta_blob_info);

        let content_blob = Hash::from([1; 32]);

        let serve_meta_task =
            write_meta_blob(&proxy, &mut blobfs, meta_blob_info, vec![content_blob]).await;
        enumerate_readable_missing_blobs(
            &proxy,
            &mut blobfs,
            std::iter::empty(),
            vec![content_blob].into_iter(),
        )
        .await;

        // Try to open the blob, but report it is already being written concurrently.
        let ((), ()) = future::join(
            async {
                blobfs.expect_create_blob(content_blob).await.fail_open_with_already_exists();
                blobfs.expect_open_blob(content_blob).await.fail_open_with_not_readable().await;
            },
            async {
                assert_matches!(
                    proxy
                        .open_blob(&BlobId::from(content_blob).into(), fpkg::BlobType::Uncompressed)
                        .await,
                    Ok(Err(fpkg::OpenBlobError::ConcurrentWrite))
                );
            },
        )
        .await;

        // Try to write the blob, but report the written contents are corrupt.
        let ((), ()) = future::join(
            async {
                blobfs.expect_create_blob(content_blob).await.fail_write_with_corrupt().await;
            },
            async {
                let blob = proxy
                    .open_blob(&BlobId::from(content_blob).into(), fpkg::BlobType::Uncompressed)
                    .await
                    .expect("open_blob failed")
                    .expect("open_blob error")
                    .expect("blob not cached")
                    .into_proxy()
                    .unwrap();

                let () = blob
                    .resize(1)
                    .await
                    .expect("resize failed")
                    .map_err(Status::from_raw)
                    .expect("resize error");
                let result =
                    blob.write(&[0]).await.expect("write failed").map_err(Status::from_raw);
                assert_eq!(result, Err(Status::IO_DATA_INTEGRITY));
                assert_matches!(
                    blob.close().await,
                    Err(fidl::Error::ClientChannelClosed { status: Status::PEER_CLOSED, .. })
                );
            },
        )
        .await;

        // Open the blob for write, but then close it (a non-fatal error)
        let ((), ()) = future::join(
            async {
                blobfs.expect_create_blob(content_blob).await.expect_close().await;
            },
            async {
                let blob = proxy
                    .open_blob(&BlobId::from(content_blob).into(), fpkg::BlobType::Uncompressed)
                    .await
                    .unwrap()
                    .unwrap()
                    .unwrap()
                    .into_proxy()
                    .unwrap();

                let () = blob
                    .close()
                    .await
                    .expect("close failed")
                    .map_err(Status::from_raw)
                    .expect("close error");
            },
        )
        .await;

        // Operation succeeds after blobfs cooperates.
        let ((), ()) = future::join(
            async {
                blobfs.expect_create_blob(content_blob).await.expect_payload(&[0]).await;
                blobfs.expect_readable_missing_checks(&[content_blob], &[]).await;
            },
            async {
                let blob = proxy
                    .open_blob(&BlobId::from(content_blob).into(), fpkg::BlobType::Uncompressed)
                    .await
                    .unwrap()
                    .unwrap()
                    .unwrap()
                    .into_proxy()
                    .unwrap();

                let () = blob
                    .resize(1)
                    .await
                    .expect("resize failed")
                    .map_err(Status::from_raw)
                    .expect("resize error");
                let _: u64 = blob
                    .write(&[0])
                    .await
                    .expect("write failed")
                    .map_err(Status::from_raw)
                    .expect("write error");
                let () = blob
                    .close()
                    .await
                    .expect("close failed")
                    .map_err(Status::from_raw)
                    .expect("close error");
                drop(blob);

                let () = proxy
                    .blob_written(&BlobId::from(content_blob).into())
                    .await
                    .expect("blob_written failed")
                    .expect("blob_written error");
            },
        )
        .await;

        // That was the only data blob, so the operation is now done.
        assert_matches!(serve_needed_task.await, Ok(()));
        let () = serve_meta_task.await;
        blobfs.expect_done().await;
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn abort_aborts_while_waiting_for_open_meta_blob() {
        let meta_blob_info = BlobInfo { blob_id: [0; 32].into(), length: 0 };
        let (task, proxy, blobfs) = spawn_serve_needed_blobs_with_mocks(meta_blob_info);

        let abort_fut = proxy.abort();

        assert_matches!(task.await, Err(ServeNeededBlobsError::Aborted));
        assert_matches!(
            abort_fut.await,
            Err(fidl::Error::ClientChannelClosed { status: Status::PEER_CLOSED, .. })
        );
        blobfs.expect_done().await;
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn abort_aborts_while_waiting_for_get_missing_blobs() {
        let meta_blob_info = BlobInfo { blob_id: [0; 32].into(), length: 0 };
        let (serve_needed_task, proxy, mut blobfs) =
            spawn_serve_needed_blobs_with_mocks(meta_blob_info);

        let serve_meta_task = write_meta_blob(&proxy, &mut blobfs, meta_blob_info, vec![]).await;

        let abort_fut = proxy.abort();

        assert_matches!(serve_needed_task.await, Err(ServeNeededBlobsError::Aborted));
        assert_matches!(
            abort_fut.await,
            Err(fidl::Error::ClientChannelClosed { status: Status::PEER_CLOSED, .. })
        );
        let () = serve_meta_task.await;
        blobfs.expect_done().await;
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn abort_aborts_while_waiting_for_open_blobs() {
        let meta_blob_info = BlobInfo { blob_id: [0; 32].into(), length: 0 };
        let (serve_needed_task, proxy, mut blobfs) =
            spawn_serve_needed_blobs_with_mocks(meta_blob_info);

        let serve_meta_task =
            write_meta_blob(&proxy, &mut blobfs, meta_blob_info, vec![[2; 32].into()]).await;
        enumerate_readable_missing_blobs(
            &proxy,
            &mut blobfs,
            std::iter::empty(),
            vec![[2; 32].into()].into_iter(),
        )
        .await;

        let abort_fut = proxy.abort();

        assert_matches!(serve_needed_task.await, Err(ServeNeededBlobsError::Aborted));
        assert_matches!(
            abort_fut.await,
            Err(fidl::Error::ClientChannelClosed { status: Status::PEER_CLOSED, .. })
        );
        let () = serve_meta_task.await;
        blobfs.expect_done().await;
    }
}

#[cfg(test)]
mod get_handler_tests {
    use {
        super::*,
        crate::{CobaltConnectedService, ProtocolConnector, COBALT_CONNECTOR_BUFFER_SIZE},
        std::collections::HashSet,
    };

    #[fuchsia_async::run_singlethreaded(test)]
    async fn everything_closed() {
        let (_, stream) = fidl::endpoints::create_proxy::<NeededBlobsMarker>().unwrap();
        let meta_blob_info = BlobInfo { blob_id: [0; 32].into(), length: 0 };
        let (blobfs, _) = blobfs::Client::new_test();
        let inspector = fuchsia_inspect::Inspector::default();
        let package_index = Arc::new(async_lock::RwLock::new(PackageIndex::new(
            inspector.root().create_child("test_does_not_use_inspect "),
        )));

        assert_matches::assert_matches!(
            get(
                &package_index,
                &BasePackages::new_test_only(HashSet::new(), vec![]),
                system_image::ExecutabilityRestrictions::DoNotEnforce,
                &system_image::NonStaticAllowList::empty(),
                &blobfs,
                meta_blob_info,
                stream,
                None,
                ProtocolConnector::new_with_buffer_size(
                    CobaltConnectedService,
                    COBALT_CONNECTOR_BUFFER_SIZE,
                )
                .serve_and_log_errors()
                .0,
                &inspector.root().create_child("get"),
            )
            .await,
            Err(Status::UNAVAILABLE)
        );
    }
}

#[cfg(test)]
mod serve_base_package_index_tests {
    use {
        super::*, fidl_fuchsia_pkg::PackageIndexIteratorMarker, fuchsia_pkg::PackagePath,
        std::collections::HashSet,
    };

    #[fuchsia_async::run_singlethreaded(test)]
    async fn base_packages_entries_converted_correctly() {
        let base_packages = BasePackages::new_test_only(
            HashSet::new(),
            [
                (
                    PackagePath::from_name_and_variant(
                        "name0".parse().unwrap(),
                        "0".parse().unwrap(),
                    ),
                    Hash::from([0u8; 32]),
                ),
                (
                    PackagePath::from_name_and_variant(
                        "name1".parse().unwrap(),
                        "1".parse().unwrap(),
                    ),
                    Hash::from([1u8; 32]),
                ),
            ],
        );

        let (proxy, stream) =
            fidl::endpoints::create_proxy_and_stream::<PackageIndexIteratorMarker>().unwrap();
        let task =
            Task::local(serve_base_package_index("fuchsia.test", Arc::new(base_packages), stream));

        let entries = proxy.next().await.unwrap();
        assert_eq!(
            entries,
            vec![
                fpkg::PackageIndexEntry {
                    package_url: fpkg::PackageUrl {
                        url: "fuchsia-pkg://fuchsia.test/name0".to_string(),
                    },
                    meta_far_blob_id: fpkg::BlobId { merkle_root: [0u8; 32] }
                },
                fpkg::PackageIndexEntry {
                    package_url: fpkg::PackageUrl {
                        url: "fuchsia-pkg://fuchsia.test/name1".to_string(),
                    },
                    meta_far_blob_id: fpkg::BlobId { merkle_root: [1u8; 32] }
                }
            ]
        );

        let entries = proxy.next().await.unwrap();
        assert_eq!(entries, vec![]);

        let () = task.await;
    }
}

#[cfg(test)]
mod serve_cache_package_index_tests {
    use {
        super::*, fidl_fuchsia_pkg::PackageIndexIteratorMarker,
        fuchsia_url::PinnedAbsolutePackageUrl,
    };

    #[fuchsia_async::run_singlethreaded(test)]
    async fn cache_packages_entries_converted_correctly() {
        let cache_packages = system_image::CachePackages::from_entries(vec![
            PinnedAbsolutePackageUrl::new(
                "fuchsia-pkg://fuchsia.test".parse().unwrap(),
                "name0".parse().unwrap(),
                Some(fuchsia_url::PackageVariant::zero()),
                Hash::from([0u8; 32]),
            ),
            PinnedAbsolutePackageUrl::new(
                "fuchsia-pkg://fuchsia.test".parse().unwrap(),
                "name1".parse().unwrap(),
                Some("1".parse().unwrap()),
                Hash::from([1u8; 32]),
            ),
        ]);

        let (proxy, stream) =
            fidl::endpoints::create_proxy_and_stream::<PackageIndexIteratorMarker>().unwrap();
        let task = Task::local(serve_cache_package_index(Some(Arc::new(cache_packages)), stream));

        let entries = proxy.next().await.unwrap();
        assert_eq!(
            entries,
            vec![
                fpkg::PackageIndexEntry {
                    package_url: fpkg::PackageUrl {
                        url: "fuchsia-pkg://fuchsia.test/name0".to_string()
                    },
                    meta_far_blob_id: fpkg::BlobId { merkle_root: [0u8; 32] }
                },
                fpkg::PackageIndexEntry {
                    package_url: fpkg::PackageUrl {
                        url: "fuchsia-pkg://fuchsia.test/name1".to_string()
                    },
                    meta_far_blob_id: fpkg::BlobId { merkle_root: [1u8; 32] }
                }
            ]
        );

        let entries = proxy.next().await.unwrap();
        assert_eq!(entries, vec![]);

        let () = task.await;
    }
}
