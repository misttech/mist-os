// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl::endpoints::create_proxy;
use fidl_fuchsia_io as fio;
use fuchsia_fs::directory::{WatchEvent, WatchMessage, Watcher};
use futures::StreamExt;
use io_conformance_util::test_harness::TestHarness;
use io_conformance_util::*;
use std::path::PathBuf;

#[fuchsia::test]
async fn watch_dir_existing() {
    let harness = TestHarness::new().await;

    let entries = vec![file("foo", b"test".to_vec())];
    let dir = harness.get_directory(entries, harness.dir_rights.all_flags());

    let mut watcher = Watcher::new(&dir).await.expect("making watcher");
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
    if !harness.config.supports_modify_directory {
        return;
    }

    let dir = harness.get_directory(vec![], harness.dir_rights.all_flags());

    let mut watcher = Watcher::new(&dir).await.expect("making watcher");
    assert_eq!(
        watcher.next().await.expect("watcher stream empty").expect("watch message error"),
        WatchMessage { event: WatchEvent::EXISTING, filename: PathBuf::from(".") },
    );
    assert_eq!(
        watcher.next().await.expect("watcher stream empty").expect("watch message error"),
        WatchMessage { event: WatchEvent::IDLE, filename: PathBuf::new() },
    );

    let _ =
        open_dir_with_flags(&dir, fio::OpenFlags::RIGHT_WRITABLE | fio::OpenFlags::CREATE, "foo")
            .await;
    assert_eq!(
        watcher.next().await.expect("watcher stream empty").expect("watch message error"),
        WatchMessage { event: WatchEvent::ADD_FILE, filename: PathBuf::from("foo") },
    );

    let _ = open_dir_with_flags(
        &dir,
        fio::OpenFlags::RIGHT_WRITABLE | fio::OpenFlags::CREATE | fio::OpenFlags::DIRECTORY,
        "dir",
    )
    .await;
    assert_eq!(
        watcher.next().await.expect("watcher stream empty").expect("watch message error"),
        WatchMessage { event: WatchEvent::ADD_FILE, filename: PathBuf::from("dir") },
    );

    dir.unlink("foo", &fio::UnlinkOptions::default())
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
    if !harness.config.supports_modify_directory {
        return;
    }

    let dir = harness.get_directory(vec![], harness.dir_rights.all_flags());

    let mut watcher = Watcher::new(&dir).await.expect("making watcher");
    assert_eq!(
        watcher.next().await.expect("watcher stream empty").expect("watch message error"),
        WatchMessage { event: WatchEvent::EXISTING, filename: PathBuf::from(".") },
    );
    assert_eq!(
        watcher.next().await.expect("watcher stream empty").expect("watch message error"),
        WatchMessage { event: WatchEvent::IDLE, filename: PathBuf::new() },
    );

    let _ =
        open_dir_with_flags(&dir, fio::OpenFlags::RIGHT_WRITABLE | fio::OpenFlags::CREATE, "foo")
            .await;
    assert_eq!(
        watcher.next().await.expect("watcher stream empty").expect("watch message error"),
        WatchMessage { event: WatchEvent::ADD_FILE, filename: PathBuf::from("foo") },
    );
    {
        let (client, server) = create_proxy::<fio::NodeMarker>();
        dir.open(
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
    dir.unlink("foo", &fio::UnlinkOptions::default())
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
    if !harness.config.supports_modify_directory {
        return;
    }

    let dir = harness.get_directory(vec![], harness.dir_rights.all_flags());

    let mut watcher = Watcher::new(&dir).await.expect("making watcher");
    assert_eq!(
        watcher.next().await.expect("watcher stream empty").expect("watch message error"),
        WatchMessage { event: WatchEvent::EXISTING, filename: PathBuf::from(".") },
    );
    assert_eq!(
        watcher.next().await.expect("watcher stream empty").expect("watch message error"),
        WatchMessage { event: WatchEvent::IDLE, filename: PathBuf::new() },
    );

    let _ =
        open_dir_with_flags(&dir, fio::OpenFlags::RIGHT_WRITABLE | fio::OpenFlags::CREATE, "foo")
            .await;
    assert_eq!(
        watcher.next().await.expect("watcher stream empty").expect("watch message error"),
        WatchMessage { event: WatchEvent::ADD_FILE, filename: PathBuf::from("foo") },
    );

    let (status, token) = dir.get_token().await.unwrap();
    assert_eq!(zx::Status::from_raw(status), zx::Status::OK);
    let token = token.unwrap();
    dir.rename("foo", token.into(), "bar").await.expect("fidl error").expect("rename error");
    assert_eq!(
        watcher.next().await.expect("watcher stream empty").expect("watch message error"),
        WatchMessage { event: WatchEvent::REMOVE_FILE, filename: PathBuf::from("foo") },
    );
    assert_eq!(
        watcher.next().await.expect("watcher stream empty").expect("watch message error"),
        WatchMessage { event: WatchEvent::ADD_FILE, filename: PathBuf::from("bar") },
    );

    dir.unlink("bar", &fio::UnlinkOptions::default())
        .await
        .expect("fidl error")
        .expect("unlink error");
    assert_eq!(
        watcher.next().await.expect("watcher stream empty").expect("watch message error"),
        WatchMessage { event: WatchEvent::REMOVE_FILE, filename: PathBuf::from("bar") },
    );
}
