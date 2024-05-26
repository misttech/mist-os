// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    fidl::endpoints::create_proxy,
    fidl_fuchsia_io as fio,
    fuchsia_fs::directory::{WatchEvent, WatchMessage, Watcher},
    fuchsia_zircon as zx,
    futures::StreamExt,
    io_conformance_util::{test_harness::TestHarness, *},
    std::path::PathBuf,
};

#[fuchsia::test]
async fn watch_dir_existing() {
    let harness = TestHarness::new().await;
    if !harness.config.supports_directory_watchers {
        return;
    }

    let root = root_directory(vec![file("foo", b"test".to_vec())]);
    let root_dir = harness.get_directory(root, harness.dir_rights.all());

    let mut watcher = Watcher::new(&root_dir).await.expect("making watcher");
    assert_eq!(
        watcher.next().await.expect("watcher stream empty").expect("watch message error"),
        WatchMessage { event: WatchEvent::EXISTING, filename: PathBuf::from(".") },
    );
    assert_eq!(
        watcher.next().await.expect("watcher stream empty").expect("watch message error"),
        WatchMessage { event: WatchEvent::EXISTING, filename: PathBuf::from("foo") },
    );
    assert_eq!(
        watcher.next().await.expect("watcher stream empty").expect("watch message error"),
        WatchMessage { event: WatchEvent::IDLE, filename: PathBuf::new() },
    );
}

#[fuchsia::test]
async fn watch_dir_added_removed() {
    let harness = TestHarness::new().await;
    if !harness.config.supports_directory_watchers {
        return;
    }
    if !harness.config.supports_create {
        return;
    }

    let root = root_directory(vec![]);
    let root_dir = harness.get_directory(root, harness.dir_rights.all());

    let mut watcher = Watcher::new(&root_dir).await.expect("making watcher");
    assert_eq!(
        watcher.next().await.expect("watcher stream empty").expect("watch message error"),
        WatchMessage { event: WatchEvent::EXISTING, filename: PathBuf::from(".") },
    );
    assert_eq!(
        watcher.next().await.expect("watcher stream empty").expect("watch message error"),
        WatchMessage { event: WatchEvent::IDLE, filename: PathBuf::new() },
    );

    let _ = open_dir_with_flags(
        &root_dir,
        fio::OpenFlags::RIGHT_WRITABLE | fio::OpenFlags::CREATE,
        "foo",
    )
    .await;
    assert_eq!(
        watcher.next().await.expect("watcher stream empty").expect("watch message error"),
        WatchMessage { event: WatchEvent::ADD_FILE, filename: PathBuf::from("foo") },
    );

    let _ = open_dir_with_flags(
        &root_dir,
        fio::OpenFlags::RIGHT_WRITABLE | fio::OpenFlags::CREATE | fio::OpenFlags::DIRECTORY,
        "dir",
    )
    .await;
    assert_eq!(
        watcher.next().await.expect("watcher stream empty").expect("watch message error"),
        WatchMessage { event: WatchEvent::ADD_FILE, filename: PathBuf::from("dir") },
    );

    root_dir
        .unlink("foo", &fio::UnlinkOptions::default())
        .await
        .expect("fidl error")
        .expect("unlink error");
    assert_eq!(
        watcher.next().await.expect("watcher stream empty").expect("watch message error"),
        WatchMessage { event: WatchEvent::REMOVE_FILE, filename: PathBuf::from("foo") },
    );
}

#[fuchsia::test]
async fn watch_dir_existing_file_create_does_not_generate_new_event() {
    let harness = TestHarness::new().await;
    if !harness.config.supports_directory_watchers {
        return;
    }
    if !harness.config.supports_create {
        return;
    }

    let root = root_directory(vec![]);
    let root_dir = harness.get_directory(root, harness.dir_rights.all());

    let mut watcher = Watcher::new(&root_dir).await.expect("making watcher");
    assert_eq!(
        watcher.next().await.expect("watcher stream empty").expect("watch message error"),
        WatchMessage { event: WatchEvent::EXISTING, filename: PathBuf::from(".") },
    );
    assert_eq!(
        watcher.next().await.expect("watcher stream empty").expect("watch message error"),
        WatchMessage { event: WatchEvent::IDLE, filename: PathBuf::new() },
    );

    let _ = open_dir_with_flags(
        &root_dir,
        fio::OpenFlags::RIGHT_WRITABLE | fio::OpenFlags::CREATE,
        "foo",
    )
    .await;
    assert_eq!(
        watcher.next().await.expect("watcher stream empty").expect("watch message error"),
        WatchMessage { event: WatchEvent::ADD_FILE, filename: PathBuf::from("foo") },
    );
    {
        let (client, server) = create_proxy::<fio::NodeMarker>().expect("Cannot create proxy.");
        root_dir
            .open(
                fio::OpenFlags::RIGHT_WRITABLE | fio::OpenFlags::CREATE | fio::OpenFlags::DESCRIBE,
                fio::ModeType::empty(),
                "foo",
                server,
            )
            .expect("Cannot open file");
        // Open should succeed - CREATE is fine if the file already exists.
        assert_eq!(get_open_status(&client).await, zx::Status::OK);
    }
    // Since we are testing that the previous open does _not_ generate an event, do something else
    // that will generate a different event and make sure that is the next event.
    root_dir
        .unlink("foo", &fio::UnlinkOptions::default())
        .await
        .expect("fidl error")
        .expect("unlink error");
    assert_eq!(
        watcher.next().await.expect("watcher stream empty").expect("watch message error"),
        WatchMessage { event: WatchEvent::REMOVE_FILE, filename: PathBuf::from("foo") },
    );
}

#[fuchsia::test]
async fn watch_dir_rename() {
    let harness = TestHarness::new().await;
    if !harness.config.supports_directory_watchers {
        return;
    }
    if !harness.config.supports_create {
        return;
    }
    if !harness.config.supports_rename {
        return;
    }

    let root = root_directory(vec![]);
    let root_dir = harness.get_directory(root, harness.dir_rights.all());

    let mut watcher = Watcher::new(&root_dir).await.expect("making watcher");
    assert_eq!(
        watcher.next().await.expect("watcher stream empty").expect("watch message error"),
        WatchMessage { event: WatchEvent::EXISTING, filename: PathBuf::from(".") },
    );
    assert_eq!(
        watcher.next().await.expect("watcher stream empty").expect("watch message error"),
        WatchMessage { event: WatchEvent::IDLE, filename: PathBuf::new() },
    );

    let _ = open_dir_with_flags(
        &root_dir,
        fio::OpenFlags::RIGHT_WRITABLE | fio::OpenFlags::CREATE,
        "foo",
    )
    .await;
    assert_eq!(
        watcher.next().await.expect("watcher stream empty").expect("watch message error"),
        WatchMessage { event: WatchEvent::ADD_FILE, filename: PathBuf::from("foo") },
    );

    let (status, token) = root_dir.get_token().await.unwrap();
    assert_eq!(zx::Status::from_raw(status), zx::Status::OK);
    let token = token.unwrap();
    root_dir.rename("foo", token.into(), "bar").await.expect("fidl error").expect("rename error");
    assert_eq!(
        watcher.next().await.expect("watcher stream empty").expect("watch message error"),
        WatchMessage { event: WatchEvent::REMOVE_FILE, filename: PathBuf::from("foo") },
    );
    assert_eq!(
        watcher.next().await.expect("watcher stream empty").expect("watch message error"),
        WatchMessage { event: WatchEvent::ADD_FILE, filename: PathBuf::from("bar") },
    );

    root_dir
        .unlink("bar", &fio::UnlinkOptions::default())
        .await
        .expect("fidl error")
        .expect("unlink error");
    assert_eq!(
        watcher.next().await.expect("watcher stream empty").expect("watch message error"),
        WatchMessage { event: WatchEvent::REMOVE_FILE, filename: PathBuf::from("bar") },
    );
}
