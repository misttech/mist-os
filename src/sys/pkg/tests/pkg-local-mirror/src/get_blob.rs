// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

/// This module tests calls to the get_blob API.
use {
    super::*, assert_matches::assert_matches, fidl::endpoints::create_proxy,
    fidl_fuchsia_io as fio, fidl_fuchsia_pkg_ext::BlobId, fuchsia_zircon::Status,
    vfs::file::vmo::read_only,
};

async fn verify_get_blob_with_read_success(env: &TestEnv, blob: &str, file_contents: &str) {
    let (file_proxy, server_end) = create_proxy().unwrap();

    let res =
        env.local_mirror_proxy().get_blob(&BlobId::parse(blob).unwrap().into(), server_end).await;

    assert_eq!(res.unwrap(), Ok(()));
    assert_matches!(
        file_proxy.take_event_stream().next().await,
        Some(Ok(fio::FileEvent::OnOpen_{s, info: Some(_)})) if Status::ok(s) == Ok(())
    );
    assert_eq!(
        fuchsia_fs::file::read_to_string(&file_proxy).await.unwrap(),
        file_contents.to_owned()
    );
}

#[fuchsia::test]
async fn success() {
    let env = TestEnv::builder()
        .usb_dir(pseudo_directory! {
            "0" => pseudo_directory! {
                "fuchsia_pkg" => pseudo_directory! {
                    "blobs" => pseudo_directory! {
                        "00" => pseudo_directory! {
                            "00000000000000000000000000000000000000000000000000000000000000" =>
                                read_only("ben"),
                            "11111111111111111111111111111111111111111111111111111111111111" =>
                                read_only("dan"),
                        },
                        "aa" => pseudo_directory! {
                            "bbccddeeff00112233445566778899aabbccddeeff00112233445566778899" =>
                                read_only("kevin"),
                        },
                    },
                    "repository_metadata" => pseudo_directory! {},
                },
            },
        })
        .build()
        .await;

    verify_get_blob_with_read_success(
        &env,
        "0000000000000000000000000000000000000000000000000000000000000000",
        "ben",
    )
    .await;
    verify_get_blob_with_read_success(
        &env,
        "0011111111111111111111111111111111111111111111111111111111111111",
        "dan",
    )
    .await;
    verify_get_blob_with_read_success(
        &env,
        "aabbccddeeff00112233445566778899aabbccddeeff00112233445566778899",
        "kevin",
    )
    .await;
}

#[fuchsia::test]
async fn missing_blob() {
    let env = TestEnv::builder().build().await;
    let (file_proxy, server_end) = create_proxy().unwrap();

    let res = env
        .local_mirror_proxy()
        .get_blob(
            &BlobId::parse("0000000000000000000000000000000000000000000000000000000000000000")
                .unwrap()
                .into(),
            server_end,
        )
        .await;

    assert_eq!(res.unwrap(), Ok(()));
    assert_matches!(
        file_proxy.take_event_stream().next().await,
        Some(Ok(fio::FileEvent::OnOpen_{s, info: None})) if  Status::from_raw(s) == Status::NOT_FOUND
    );
    assert_matches!(fuchsia_fs::file::read_to_string(&file_proxy).await, Err(_));
}

#[fuchsia::test]
async fn blobs_directory_closed() {
    let env = TestEnv::builder()
        .usb_dir(pseudo_directory! {
            "0" => pseudo_directory! {
                "fuchsia_pkg" => pseudo_directory! {
                    "blobs" => DropAfterOpen::new(),
                    "repository_metadata" => pseudo_directory! {},
                }
            }
        })
        .build()
        .await;

    let (file_proxy, server_end) = create_proxy().unwrap();
    let res = env
        .local_mirror_proxy()
        .get_blob(
            &BlobId::parse("0000000000000000000000000000000000000000000000000000000000000000")
                .unwrap()
                .into(),
            server_end,
        )
        .await;

    assert_eq!(res.unwrap(), Ok(()));
    assert_matches!(file_proxy.take_event_stream().next().await, None);
    assert_matches!(fuchsia_fs::file::read_to_string(&file_proxy).await, Err(_));
}
