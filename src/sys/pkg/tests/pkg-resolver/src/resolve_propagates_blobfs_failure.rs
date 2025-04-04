// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

/// This module tests the property that pkg_resolver propagates blobfs errors when
/// servicing fuchsia.pkg.PackageResolver.Resolve FIDL requests.
///
/// Long term, these tests should be moved/adapted to the pkg-cache integration tests, and similar
/// resolve_propagates_pkgcache_failure.rs tests should be added for pkg-resolver.
use {
    assert_matches::assert_matches,
    blobfs_ramdisk::BlobfsRamdisk,
    fidl::endpoints::{ClientEnd, RequestStream, ServerEnd},
    fidl_fuchsia_io as fio,
    fuchsia_async::{self as fasync, Task},
    fuchsia_merkle::Hash,
    fuchsia_pkg_testing::{Package, RepositoryBuilder, SystemImageBuilder},
    futures::{future::BoxFuture, prelude::*},
    lib::{extra_blob_contents, make_pkg_with_extra_blobs, TestEnvBuilder, EMPTY_REPO_PATH},
    std::sync::{atomic::AtomicU64, Arc},
    vfs::ObjectRequest,
    zx::Status,
};

trait FileStreamHandler: Send + Sync + 'static {
    fn handle_file_stream(
        &self,
        call_count: Arc<AtomicU64>,
        stream: fio::FileRequestStream,
        ch: fio::FileControlHandle,
    ) -> BoxFuture<'static, ()>;
}

impl<Func, Fut> FileStreamHandler for Func
where
    Func: Fn(Arc<AtomicU64>, fio::FileRequestStream, fio::FileControlHandle) -> Fut,
    Func: Unpin + Send + Sync + Clone + 'static,
    Fut: Future<Output = ()> + Send + Sync + 'static,
{
    fn handle_file_stream(
        &self,
        call_count: Arc<AtomicU64>,
        stream: fio::FileRequestStream,
        ch: fio::FileControlHandle,
    ) -> BoxFuture<'static, ()> {
        (self)(call_count, stream, ch).boxed()
    }
}

struct BlobFsWithFileCreateOverride {
    // The underlying blobfs ramdisk.
    wrapped: BlobfsRamdisk,

    target: (String, FakeFile),

    system_image: Hash,
}

impl lib::Blobfs for BlobFsWithFileCreateOverride {
    fn root_dir_handle(&self) -> ClientEnd<fio::DirectoryMarker> {
        let inner = self.wrapped.root_dir_handle().unwrap().into_proxy();
        let (client, server) = fidl::endpoints::create_request_stream::<fio::DirectoryMarker>();
        DirectoryWithFileCreateOverride { inner, target: self.target.clone() }.spawn(server);
        client
    }
    fn svc_dir(&self) -> fio::DirectoryProxy {
        panic!("BlobFsWithFileCreateOverride does not have a service dir")
    }
}

#[derive(Clone)]
struct DirectoryWithFileCreateOverride {
    inner: fio::DirectoryProxy,
    target: (String, FakeFile),
}

impl DirectoryWithFileCreateOverride {
    fn spawn(self, stream: fio::DirectoryRequestStream) {
        Task::spawn(self.serve(stream)).detach();
    }

    async fn serve(self, mut stream: fio::DirectoryRequestStream) {
        while let Some(req) = stream.next().await {
            match req.unwrap() {
                fio::DirectoryRequest::Open { path, flags, options, object, control_handle: _ } => {
                    ObjectRequest::new(flags, &options, object).handle(|request| {
                        if path == "." {
                            let stream = fio::NodeRequestStream::from_channel(
                                fasync::Channel::from_channel(request.take().into_channel()),
                            )
                            .cast_stream();
                            self.clone().spawn(stream);
                        } else if path == self.target.0
                            && flags.intersects(fio::Flags::FLAG_MAYBE_CREATE)
                        {
                            let server_end =
                                ServerEnd::<fio::FileMarker>::new(request.take().into_channel());
                            let handler = self.target.1.clone();

                            Task::spawn(
                                async move { handler.handle_file_stream(server_end).await },
                            )
                            .detach();
                        } else {
                            // The channel will be dropped and closed if the wire call fails.
                            let _ = self.inner.open(
                                &path,
                                flags,
                                &request.options(),
                                request.take().into_channel(),
                            );
                        }
                        Ok(())
                    });
                }
                request => panic!("Unhandled fuchsia.io/Directory request: {request:?}"),
            }
        }
    }
}

#[derive(Clone)]
struct FakeFile {
    stream_handler: Arc<dyn FileStreamHandler>,
    call_count: Arc<AtomicU64>,
}

impl FakeFile {
    fn new_and_call_count(stream_handler: impl FileStreamHandler) -> (Self, Arc<AtomicU64>) {
        let call_count = Arc::new(AtomicU64::new(0));
        (
            Self { stream_handler: Arc::new(stream_handler), call_count: call_count.clone() },
            call_count,
        )
    }

    async fn handle_file_stream(self, server_end: ServerEnd<fio::FileMarker>) {
        let (stream, ch) = server_end.into_stream_and_control_handle();

        self.stream_handler.handle_file_stream(self.call_count, stream, ch).await;
    }
}

async fn handle_file_stream_fail_on_open(
    call_count: Arc<AtomicU64>,
    mut stream: fio::FileRequestStream,
    ch: fio::FileControlHandle,
) {
    ch.send_on_open_(Status::NO_MEMORY.into_raw(), None).expect("send on open");
    call_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    while let Some(req) = stream.next().await {
        handle_file_req_panic(req.expect("file request unpack")).await;
    }
}

async fn handle_file_stream_fail_truncate(
    call_count: Arc<AtomicU64>,
    mut stream: fio::FileRequestStream,
    ch: fio::FileControlHandle,
) {
    ch.send_on_open_(
        Status::OK.into_raw(),
        Some(fio::NodeInfoDeprecated::File(fio::FileObject { event: None, stream: None })),
    )
    .expect("send on open");
    while let Some(req) = stream.next().await {
        handle_file_req_fail_truncate(call_count.clone(), req.expect("file request unpack")).await;
    }
}

async fn handle_file_stream_fail_write(
    call_count: Arc<AtomicU64>,
    mut stream: fio::FileRequestStream,
    ch: fio::FileControlHandle,
) {
    ch.send_on_open_(
        Status::OK.into_raw(),
        Some(fio::NodeInfoDeprecated::File(fio::FileObject { event: None, stream: None })),
    )
    .expect("send on open");
    while let Some(req) = stream.next().await {
        handle_file_req_fail_write(call_count.clone(), req.expect("file request unpack")).await;
    }
}
async fn handle_file_req_panic(_req: fio::FileRequest) {
    panic!("should not be called");
}

async fn handle_file_req_fail_truncate(call_count: Arc<AtomicU64>, req: fio::FileRequest) {
    match req {
        fio::FileRequest::Resize { length: _length, responder } => {
            call_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            responder.send(Err(Status::NO_MEMORY.into_raw())).expect("send truncate response");
        }
        fio::FileRequest::Close { responder } => {
            let _ = responder.send(Ok(()));
        }
        req => panic!("unexpected request: {:?}", req),
    }
}

async fn handle_file_req_fail_write(call_count: Arc<AtomicU64>, req: fio::FileRequest) {
    match req {
        fio::FileRequest::Resize { length: _length, responder } => {
            responder.send(Ok(())).expect("send resize response");
        }
        fio::FileRequest::Close { responder } => {
            let _ = responder.send(Ok(()));
        }
        fio::FileRequest::Write { data: _data, responder } => {
            call_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            responder.send(Err(Status::NO_MEMORY.into_raw())).expect("send write response");
        }
        req => panic!("unexpected request: {:?}", req),
    }
}

async fn make_blobfs_with_minimal_system_image() -> (BlobfsRamdisk, Hash) {
    let blobfs = BlobfsRamdisk::start().await.unwrap();
    let system_image = SystemImageBuilder::new().build().await;
    system_image.write_to_blobfs(&blobfs).await;
    (blobfs, *system_image.hash())
}

async fn make_pkg_for_mock_blobfs_tests(package_name: &str) -> (Package, String, String) {
    let pkg = make_pkg_with_extra_blobs(package_name, 1).await;
    let pkg_merkle = pkg.hash().to_string();
    let blob_merkle =
        fuchsia_merkle::from_slice(&extra_blob_contents(package_name, 0)).root().to_string();
    (pkg, pkg_merkle, blob_merkle)
}

async fn make_mock_blobfs_with_failing_install_pkg<StreamHandler>(
    package_name: &str,
    file_request_stream_handler: StreamHandler,
) -> (BlobFsWithFileCreateOverride, Package, Arc<AtomicU64>)
where
    StreamHandler: FileStreamHandler,
{
    let (blobfs, system_image) = make_blobfs_with_minimal_system_image().await;
    let (failing_file, call_count) = FakeFile::new_and_call_count(file_request_stream_handler);
    let (pkg, pkg_merkle, _) = make_pkg_for_mock_blobfs_tests(package_name).await;

    (
        BlobFsWithFileCreateOverride {
            wrapped: blobfs,
            target: (delivery_blob::delivery_blob_path(pkg_merkle), failing_file),
            system_image,
        },
        pkg,
        call_count,
    )
}

async fn make_mock_blobfs_with_failing_install_blob<StreamHandler>(
    package_name: &str,
    file_request_stream_handler: StreamHandler,
) -> (BlobFsWithFileCreateOverride, Package, Arc<AtomicU64>)
where
    StreamHandler: FileStreamHandler,
{
    let (blobfs, system_image) = make_blobfs_with_minimal_system_image().await;
    let (failing_file, call_count) = FakeFile::new_and_call_count(file_request_stream_handler);
    let (pkg, _pkg_merkle, blob_merkle) = make_pkg_for_mock_blobfs_tests(package_name).await;

    (
        BlobFsWithFileCreateOverride {
            wrapped: blobfs,
            target: (delivery_blob::delivery_blob_path(blob_merkle), failing_file),
            system_image,
        },
        pkg,
        call_count,
    )
}

async fn assert_resolve_package_with_failing_blobfs_fails(
    blobfs: BlobFsWithFileCreateOverride,
    pkg: Package,
    failing_file_call_count: Arc<AtomicU64>,
    expected_failing_file_call_count: u64,
) {
    let system_image = blobfs.system_image;
    let env = TestEnvBuilder::new()
        .blobfs_and_system_image_hash(blobfs, Some(system_image))
        .build()
        .await;
    let repo = RepositoryBuilder::from_template_dir(EMPTY_REPO_PATH)
        .add_package(&pkg)
        .build()
        .await
        .unwrap();
    let served_repository = Arc::new(repo).server().start().unwrap();
    let repo_url = "fuchsia-pkg://test".parse().unwrap();
    let repo_config = served_repository.make_repo_config(repo_url);
    let () = env
        .proxies
        .repo_manager
        .add(&repo_config.into())
        .await
        .unwrap()
        .map_err(Status::from_raw)
        .unwrap();
    let res = env.resolve_package(format!("fuchsia-pkg://test/{}", pkg.name()).as_str()).await;

    assert_matches!(res, Err(fidl_fuchsia_pkg::ResolveError::Io));
    assert_eq!(
        failing_file_call_count.load(std::sync::atomic::Ordering::SeqCst),
        expected_failing_file_call_count
    );
}

#[fuchsia::test]
async fn fails_on_open_far_in_install_pkg() {
    let (blobfs, pkg, failing_file_call_count) = make_mock_blobfs_with_failing_install_pkg(
        "fails_on_open_far_in_install_pkg",
        handle_file_stream_fail_on_open,
    )
    .await;

    // pkg-resolver tries to open the meta.far first outside of the fetch queue to see if it can
    // skip fetching the blob to avoid blocking resolves of fully-cached packages, which results
    // in two open attempts being made on the meta.far.
    assert_resolve_package_with_failing_blobfs_fails(blobfs, pkg, failing_file_call_count, 2).await
}

#[fuchsia::test]
async fn fails_truncate_far_in_install_pkg() {
    let (blobfs, pkg, failing_file_call_count) = make_mock_blobfs_with_failing_install_pkg(
        "fails_truncate_far_in_install_pkg",
        handle_file_stream_fail_truncate,
    )
    .await;

    assert_resolve_package_with_failing_blobfs_fails(blobfs, pkg, failing_file_call_count, 1).await
}

#[fuchsia::test]
async fn fails_write_far_in_install_pkg() {
    let (blobfs, pkg, failing_file_call_count) = make_mock_blobfs_with_failing_install_pkg(
        "fails_write_far_in_install_pkg",
        handle_file_stream_fail_write,
    )
    .await;

    assert_resolve_package_with_failing_blobfs_fails(blobfs, pkg, failing_file_call_count, 1).await
}

#[fuchsia::test]
async fn fails_on_open_blob_in_install_blob() {
    let (blobfs, pkg, failing_file_call_count) = make_mock_blobfs_with_failing_install_blob(
        "fails_on_open_blob_in_install_blob",
        handle_file_stream_fail_on_open,
    )
    .await;

    assert_resolve_package_with_failing_blobfs_fails(blobfs, pkg, failing_file_call_count, 1).await
}

#[fuchsia::test]
async fn fails_truncate_blob_in_install_blob() {
    let (blobfs, pkg, failing_file_call_count) = make_mock_blobfs_with_failing_install_blob(
        "fails_truncate_blob_in_install_blob",
        handle_file_stream_fail_truncate,
    )
    .await;

    assert_resolve_package_with_failing_blobfs_fails(blobfs, pkg, failing_file_call_count, 1).await
}

#[fuchsia::test]
async fn fails_write_blob_in_install_blob() {
    let (blobfs, pkg, failing_file_call_count) = make_mock_blobfs_with_failing_install_blob(
        "fails_write_blob_in_install_blob",
        handle_file_stream_fail_write,
    )
    .await;

    assert_resolve_package_with_failing_blobfs_fails(blobfs, pkg, failing_file_call_count, 1).await
}
