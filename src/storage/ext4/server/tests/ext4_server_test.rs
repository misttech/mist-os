// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Error;
use fidl::endpoints::DiscoverableProtocolMarker as _;
use fidl_fuchsia_hardware_block_volume::VolumeMarker;
use fidl_fuchsia_io as fio;
use fuchsia_component_test::{Capability, ChildOptions, RealmBuilder, Ref, Route};
use futures::FutureExt as _;
use maplit::hashmap;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::{Read as _, Seek as _};
use std::sync::Arc;
use std::{fs, io};
use test_case::test_case;
use vmo_backed_block_server::{VmoBackedServer, VmoBackedServerTestingExt as _};

#[test_case(
    "/pkg/data/extents.img",
    hashmap!{
        "largefile".to_string() => "de2cf635ae4e0e727f1e412f978001d6a70d2386dc798d4327ec8c77a8e4895d".to_string(),
        "smallfile".to_string() => "5891b5b522d5df086d0ff0b110fbd9d21bb4fc7163af34d08286a2e846f6be03".to_string(),
        "sparsefile".to_string() => "3f411e42c1417cd8845d7144679812be3e120318d843c8c6e66d8b2c47a700e9".to_string(),
        "a/multi/dir/path/within/this/crowded/extents/test/img/empty".to_string() => "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855".to_string(),
    };
    "fs with multiple files with multiple extents")]
#[test_case(
    "/pkg/data/1file.img",
    hashmap!{
        "file1".to_string() => "6bc35bfb2ca96c75a1fecde205693c19a827d4b04e90ace330048f3e031487dd".to_string(),
    };
    "fs with one small file")]
#[test_case(
    "/pkg/data/nest.img",
    hashmap!{
        "file1".to_string() => "6bc35bfb2ca96c75a1fecde205693c19a827d4b04e90ace330048f3e031487dd".to_string(),
        "inner/file2".to_string() => "215ca145cbac95c9e2a6f5ff91ca1887c837b18e5f58fd2a7a16e2e5a3901e10".to_string(),
    };
    "fs with a single directory")]
#[fuchsia::test]
async fn ext4_server_mounts_block_device_from_namespace(
    ext4_path: &str,
    file_hashes: HashMap<String, String>,
) -> Result<(), Error> {
    let mut file_buf = io::BufReader::new(fs::File::open(ext4_path)?);
    let size = file_buf.seek(io::SeekFrom::End(0))?;
    file_buf.seek(io::SeekFrom::Start(0))?;

    let mut temp_buf = vec![0u8; size as usize];
    {
        file_buf.read_exact(&mut temp_buf)?;
    }
    let vmo = zx::Vmo::create(size)?;
    vmo.write(&temp_buf, 0)?;
    let server = Arc::new(VmoBackedServer::from_vmo(512, vmo));

    let builder = RealmBuilder::new().await.expect("Failed to create RealmBuilder");
    let block = builder
        .add_local_child(
            "block",
            move |handles: fuchsia_component_test::LocalComponentHandles| {
                let server = server.clone();
                let scope = vfs::ExecutionScope::new();
                let outgoing = vfs::pseudo_directory! {
                    "block" => vfs::pseudo_directory! {
                        VolumeMarker::PROTOCOL_NAME =>
                            vfs::service::host(move |requests| {
                                let server_clone = server.clone();
                                async move {
                                    let _ = server_clone.serve(requests).await;
                                }
                            }),
                    },
                };

                vfs::directory::serve_on(
                    outgoing,
                    fio::PERM_READABLE,
                    scope.clone(),
                    handles.outgoing_dir,
                );
                async move {
                    scope.wait().await;
                    Ok(())
                }
                .boxed()
            },
            ChildOptions::new().eager(),
        )
        .await
        .unwrap();
    let ext4_server = builder
        .add_child("ext4_server", "#meta/ext4_readonly.cm", ChildOptions::new())
        .await
        .unwrap();

    builder
        .add_route(
            Route::new()
                .capability(Capability::directory("block").path("/block").rights(fio::R_STAR_DIR))
                .from(&block)
                .to(&ext4_server),
        )
        .await
        .unwrap();
    builder
        .add_route(
            Route::new()
                .capability(Capability::directory("root").rights(fio::R_STAR_DIR))
                .from(&ext4_server)
                .to(Ref::parent()),
        )
        .await
        .unwrap();
    builder
        .add_route(
            Route::new()
                .capability(Capability::dictionary("diagnostics"))
                .from(Ref::parent())
                .to(&ext4_server),
        )
        .await
        .unwrap();

    let realm = builder.build().await.expect("Failed to build realm");

    let fs_root = fuchsia_fs::directory::open_directory(
        realm.root.get_exposed_dir(),
        "root",
        fio::PERM_READABLE,
    )
    .await
    .expect("Failed to open fs root");

    for (file_path, expected_hash) in &file_hashes {
        let file =
            fuchsia_fs::directory::open_file(&fs_root, file_path, fio::PERM_READABLE).await?;
        let mut hasher = Sha256::new();
        hasher.update(&fuchsia_fs::file::read(&file).await?);
        assert_eq!(*expected_hash, hex::encode(hasher.finalize()));
    }

    Ok(())
}
