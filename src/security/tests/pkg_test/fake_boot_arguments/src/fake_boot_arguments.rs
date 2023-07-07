// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::Context as _,
    fidl::endpoints::DiscoverableProtocolMarker,
    fidl_fuchsia_io as fio, fuchsia_zircon as zx,
    futures::stream::{StreamExt as _, TryStreamExt as _},
    std::sync::Arc,
    tracing::{error, info},
};

static PKGFS_BOOT_ARG_KEY: &'static str = "zircon.system.pkgfs.cmd";
static PKGFS_BOOT_ARG_VALUE_PREFIX: &'static str = "bin/pkgsvr+";

/// Flags for fake_boot_arguments.
#[derive(argh::FromArgs, Debug, PartialEq)]
pub struct Args {
    /// absolute path to system_image package file.
    #[argh(option)]
    system_image_path: String,
}

enum BootServices {
    Arguments(fidl_fuchsia_boot::ArgumentsRequestStream),
    Items(fidl_fuchsia_boot::ItemsRequestStream),
}

#[fuchsia::main]
async fn main() {
    info!("Starting fake_boot_arguments...");
    let args @ Args { system_image_path } = &argh::from_env();
    info!(?args, "Initalizing fake_boot_arguments");

    let system_image = fuchsia_fs::file::read(
        &fuchsia_fs::file::open_in_namespace(
            system_image_path.as_str(),
            fio::OpenFlags::RIGHT_READABLE,
        )
        .unwrap(),
    )
    .await
    .unwrap();

    let system_image_merkle =
        fuchsia_merkle::MerkleTree::from_reader(system_image.as_slice()).unwrap().root();
    let pkgfs_boot_arg_value = format!("{}{}", PKGFS_BOOT_ARG_VALUE_PREFIX, system_image_merkle);

    let mut fs = fuchsia_component::server::ServiceFs::new();
    fs.dir("svc").add_fidl_service(BootServices::Arguments);
    fs.dir("svc").add_fidl_service(BootServices::Items);
    fs.take_and_serve_directory_handle().unwrap();

    fs.for_each_concurrent(None, |stream| async {
        match stream {
            BootServices::Arguments(stream) => {
                let () = serve(stream, pkgfs_boot_arg_value.as_str()).await.unwrap_or_else(|err| {
                    error!("error handling fuchsia.boot/Arguments stream: {:#}", err)
                });
            }
            BootServices::Items(stream) => {
                // The VMO provided here would be for the recovery case only, which isn't of interest for pkg_test.
                run_boot_items(stream, None).await
            }
        }
    })
    .await;
}

/// Identifier for ramdisk storage. Defined in sdk/lib/zbi-format/include/lib/zbi-format/zbi.h.
const ZBI_TYPE_STORAGE_RAMDISK: u32 = 0x4b534452;

async fn serve(
    mut stream: fidl_fuchsia_boot::ArgumentsRequestStream,
    pkgfs_boot_arg_value: &str,
) -> anyhow::Result<()> {
    while let Some(request) = stream.try_next().await.context("getting next request")? {
        match request {
            fidl_fuchsia_boot::ArgumentsRequest::GetString { key, responder } => {
                let value = if key == PKGFS_BOOT_ARG_KEY {
                    Some(pkgfs_boot_arg_value)
                } else {
                    // fshost may depend on using this interface, but not care about the return value.
                    None
                };
                responder.send(value).unwrap();
            }
            fidl_fuchsia_boot::ArgumentsRequest::GetStrings { keys, responder } => {
                responder.send(&vec![None; keys.len()]).unwrap();
            }
            fidl_fuchsia_boot::ArgumentsRequest::GetBool { key: _, defaultval, responder } => {
                responder.send(defaultval).unwrap();
            }
            fidl_fuchsia_boot::ArgumentsRequest::GetBools { keys, responder } => {
                let vec: Vec<_> = keys.iter().map(|bool_pair| bool_pair.defaultval).collect();
                responder.send(&vec).unwrap();
            }
            fidl_fuchsia_boot::ArgumentsRequest::Collect { .. } => {
                // This seems to be deprecated. Either way, fshost doesn't use it.
                panic!(
                    "unexpectedly called Collect on {}",
                    fidl_fuchsia_boot::ArgumentsMarker::PROTOCOL_NAME
                );
            }
        }
    }
    Ok(())
}

// Mocks for fshost, from https://cs.opensource.google/fuchsia/fuchsia/+/main:src/storage/fshost/integration/src/mocks.rs
// fshost uses exactly one boot item - it checks to see if there is an item of type
// ZBI_TYPE_STORAGE_RAMDISK. If it's there, it's a vmo that represents a ramdisk version of the
// fvm, and fshost creates a ramdisk from the vmo so it can go through the normal device matching.
async fn run_boot_items(
    mut stream: fidl_fuchsia_boot::ItemsRequestStream,
    vmo: Option<Arc<zx::Vmo>>,
) {
    while let Some(request) = stream.next().await {
        match request.unwrap() {
            fidl_fuchsia_boot::ItemsRequest::Get { type_, extra, responder } => {
                assert_eq!(type_, ZBI_TYPE_STORAGE_RAMDISK);
                assert_eq!(extra, 0);
                let response_vmo = vmo.as_ref().map(|vmo| {
                    vmo.create_child(zx::VmoChildOptions::SLICE, 0, vmo.get_size().unwrap())
                        .unwrap()
                });
                responder.send(response_vmo, 0).unwrap();
            }
            fidl_fuchsia_boot::ItemsRequest::Get2 { type_, extra, responder } => {
                assert_eq!(type_, ZBI_TYPE_STORAGE_RAMDISK);
                assert_eq!((*extra.unwrap()).n, 0);
                responder.send(Ok(Vec::new())).unwrap();
            }
            fidl_fuchsia_boot::ItemsRequest::GetBootloaderFile { .. } => {
                panic!(
                    "unexpectedly called GetBootloaderFile on {}",
                    fidl_fuchsia_boot::ItemsMarker::PROTOCOL_NAME
                );
            }
        }
    }
}
