// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

/// This module tests calls to the get_metadata API.
use {
    super::*, assert_matches::assert_matches, fidl::endpoints::create_proxy,
    fidl_fuchsia_io as fio, fidl_fuchsia_pkg::RepositoryUrl, fuchsia_zircon::Status,
    vfs::file::vmo::read_only,
};

async fn verify_get_metadata_with_read_success(env: &TestEnv, path: &str, file_contents: &str) {
    let (file_proxy, server_end) = create_proxy().unwrap();

    let res = env
        .local_mirror_proxy()
        .get_metadata(&RepositoryUrl { url: repo_url().to_string() }, path, server_end)
        .await;

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
                    "blobs" => pseudo_directory! {},
                    "repository_metadata" => pseudo_directory! {
                        repo_url().host() => pseudo_directory! {
                            "1.root.json" => read_only("beep"),
                            "2.root.json" => read_only("boop"),
                        },
                    },
                },
            },
        })
        .build()
        .await;

    verify_get_metadata_with_read_success(&env, "1.root.json", "beep").await;
    verify_get_metadata_with_read_success(&env, "2.root.json", "boop").await;
}

#[fuchsia::test]
async fn success_multiple_path_segments() {
    let env = TestEnv::builder()
        .usb_dir(pseudo_directory! {
            "0" => pseudo_directory! {
                "fuchsia_pkg" => pseudo_directory! {
                    "blobs" => pseudo_directory! {},
                    "repository_metadata" => pseudo_directory! {
                        repo_url().host() => pseudo_directory! {
                            "foo" => pseudo_directory! {
                                "bar" => pseudo_directory! {
                                    "1.root.json" => read_only("beep"),
                                },
                            },
                            "baz" => pseudo_directory! {
                                "2.root.json" => read_only("boop"),
                            }
                        },
                    },
                },
            },
        })
        .build()
        .await;

    verify_get_metadata_with_read_success(&env, "foo/bar/1.root.json", "beep").await;
    verify_get_metadata_with_read_success(&env, "baz/2.root.json", "boop").await;
}

async fn verify_get_metadata_with_on_open_failure_status(
    env: &TestEnv,
    path: &str,
    status: Status,
) {
    let (file_proxy, server_end) = create_proxy().unwrap();

    let res = env
        .local_mirror_proxy()
        .get_metadata(&RepositoryUrl { url: repo_url().to_string() }, path, server_end)
        .await;

    assert_eq!(res.unwrap(), Ok(()));
    assert_matches!(
        file_proxy.take_event_stream().next().await,
        Some(Ok(fio::FileEvent::OnOpen_{s, info: None})) if  Status::from_raw(s) == status
    );
    assert_matches!(fuchsia_fs::file::read_to_string(&file_proxy).await, Err(_));
}

#[fuchsia::test]
async fn missing_repo_url_directory() {
    let env = TestEnv::builder().build().await;

    verify_get_metadata_with_on_open_failure_status(&env, "1.root.json", Status::NOT_FOUND).await;
}

#[fuchsia::test]
async fn missing_metadata_file() {
    let env = TestEnv::builder()
        .usb_dir(pseudo_directory! {
            "0" => pseudo_directory! {
                "fuchsia_pkg" => pseudo_directory! {
                    "blobs" => pseudo_directory! {},
                    "repository_metadata" => pseudo_directory! {
                        repo_url().host() => pseudo_directory! {
                            "2.root.json" => read_only("boop"),
                        },
                    },
                },
            },
        })
        .build()
        .await;

    verify_get_metadata_with_on_open_failure_status(&env, "1.root.json", Status::NOT_FOUND).await;
}

#[fuchsia::test]
async fn metadata_directory_closed() {
    let env = TestEnv::builder()
        .usb_dir(pseudo_directory! {
            "0" => pseudo_directory! {
                "fuchsia_pkg" => pseudo_directory! {
                    "blobs" => pseudo_directory! {},
                    "repository_metadata" => DropAfterOpen::new(),
                }
            }
        })
        .build()
        .await;

    let (file_proxy, server_end) = create_proxy().unwrap();
    let res = env
        .local_mirror_proxy()
        .get_metadata(&RepositoryUrl { url: repo_url().to_string() }, "1.root.json", server_end)
        .await;

    assert_eq!(res.unwrap(), Ok(()));
    assert_matches!(file_proxy.take_event_stream().next().await, None);
    assert_matches!(fuchsia_fs::file::read_to_string(&file_proxy).await, Err(_));
}
