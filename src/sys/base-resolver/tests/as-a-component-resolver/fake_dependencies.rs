// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    fidl::endpoints::DiscoverableProtocolMarker as _, fidl_fuchsia_boot as fboot,
    fidl_fuchsia_io as fio, futures::stream::TryStreamExt as _, tracing::info,
    vfs::directory::entry::DirectoryEntry as _,
};

static PKGFS_BOOT_ARG_KEY: &'static str = "zircon.system.pkgfs.cmd";
static PKGFS_BOOT_ARG_VALUE_PREFIX: &'static str = "bin/pkgsvr+";

#[fuchsia::main]
async fn main() {
    info!("started");
    let this_pkg = fuchsia_pkg_testing::Package::identity().await.unwrap();
    let static_packages = system_image::StaticPackages::from_entries(vec![(
        "mock-package/0".parse().unwrap(),
        *this_pkg.meta_far_merkle_root(),
    )]);
    let mut static_packages_bytes = vec![];
    static_packages.serialize(&mut static_packages_bytes).expect("write static_packages");
    let system_image = fuchsia_pkg_testing::PackageBuilder::new("system_image")
        .add_resource_at("data/static_packages", static_packages_bytes.as_slice())
        .build()
        .await
        .expect("build system_image package");
    let blobfs = blobfs_ramdisk::BlobfsRamdisk::start().await.expect("start blobfs");
    let () = system_image.write_to_blobfs(&blobfs).await;
    let () = this_pkg.write_to_blobfs_ignore_subpackages(&blobfs).await;
    let the_subpackage = fuchsia_pkg_testing::Package::from_dir("/the-subpackage").await.unwrap();
    let () = the_subpackage.write_to_blobfs(&blobfs).await;

    // Use VFS because ServiceFs does not support OPEN_RIGHT_EXECUTABLE, but /blob needs it.
    let system_image_hash = *system_image.meta_far_merkle_root();
    let out_dir = vfs::pseudo_directory! {
        "svc" => vfs::pseudo_directory! {
            fboot::ArgumentsMarker::PROTOCOL_NAME =>
                vfs::service::host(move |stream: fboot::ArgumentsRequestStream| {
                    serve_boot_args(stream, system_image_hash)
                }),
        },
        "blob" =>
            vfs::remote::remote_dir(blobfs.root_dir_proxy().expect("get blobfs root dir")),
    };

    let scope = vfs::execution_scope::ExecutionScope::new();
    out_dir.open(
        scope.clone(),
        fio::OpenFlags::RIGHT_READABLE
            | fio::OpenFlags::RIGHT_WRITABLE
            | fio::OpenFlags::RIGHT_EXECUTABLE,
        vfs::path::Path::dot(),
        fuchsia_runtime::take_startup_handle(fuchsia_runtime::HandleType::DirectoryRequest.into())
            .unwrap()
            .into(),
    );
    let () = scope.wait().await;
}

async fn serve_boot_args(mut stream: fboot::ArgumentsRequestStream, hash: fuchsia_hash::Hash) {
    while let Some(request) = stream.try_next().await.unwrap() {
        match request {
            fboot::ArgumentsRequest::GetString { key, responder } => {
                assert_eq!(key, PKGFS_BOOT_ARG_KEY);
                responder.send(Some(&format!("{}{}", PKGFS_BOOT_ARG_VALUE_PREFIX, hash))).unwrap();
            }
            req => panic!("unexpected request {:?}", req),
        }
    }
}
