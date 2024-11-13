// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl_fuchsia_io as fio;
use io_conformance_util::test_harness::TestHarness;
use io_conformance_util::*;

const CONTENTS: &[u8] = b"abcdef";

struct Fixture {
    _harness: TestHarness,
    dir: fio::DirectoryProxy,
    file: fio::FileProxy,
}

impl Fixture {
    async fn new(rights: fio::OpenFlags) -> Option<Self> {
        let harness = TestHarness::new().await;
        if !harness.config.supports_link_into || !harness.config.supports_get_token {
            return None;
        }

        let entries = vec![file(TEST_FILE, CONTENTS.to_vec()), file("existing", vec![])];
        let dir = harness.get_directory(entries, harness.dir_rights.all_flags());

        let file = open_file_with_flags(&dir, rights, TEST_FILE).await;

        Some(Self { _harness: harness, dir, file })
    }

    async fn get_token(&self) -> zx::Event {
        io_conformance_util::get_token(&self.dir).await.into()
    }
}

#[fuchsia::test]
async fn file_link_into() {
    let Some(fixture) =
        Fixture::new(fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE).await
    else {
        return;
    };

    fixture
        .file
        .link_into(fixture.get_token().await, "linked")
        .await
        .expect("link_into (FIDL) failed")
        .expect("link_into failed");

    assert_eq!(read_file(&fixture.dir, "linked").await, CONTENTS);
}

#[fuchsia::test]
async fn file_link_into_bad_name() {
    let Some(fixture) =
        Fixture::new(fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE).await
    else {
        return;
    };

    for bad_name in ["/linked", ".", "..", "\0"] {
        assert_eq!(
            fixture
                .file
                .link_into(fixture.get_token().await, bad_name)
                .await
                .expect("link_into (FIDL) failed")
                .expect_err("link_into succeeded"),
            zx::Status::INVALID_ARGS.into_raw()
        );
    }
}

#[fuchsia::test]
async fn file_link_into_insufficient_rights() {
    for rights in [fio::OpenFlags::RIGHT_READABLE, fio::OpenFlags::RIGHT_WRITABLE] {
        let Some(fixture) = Fixture::new(rights).await else { return };
        assert_eq!(
            fixture
                .file
                .link_into(fixture.get_token().await, "linked")
                .await
                .expect("link_into (FIDL) failed")
                .expect_err("link_into succeeded"),
            zx::Status::ACCESS_DENIED.into_raw()
        );
    }
}

#[fuchsia::test]
async fn file_link_into_for_unlinked_file() {
    let Some(fixture) =
        Fixture::new(fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE).await
    else {
        return;
    };

    fixture
        .dir
        .unlink(TEST_FILE, &fio::UnlinkOptions::default())
        .await
        .expect("unlink (FIDL) failed")
        .expect("unlink failed");

    assert_eq!(
        fixture
            .file
            .link_into(fixture.get_token().await, "linked")
            .await
            .expect("link_into (FIDL) failed")
            .expect_err("link_into succeeded"),
        zx::Status::NOT_FOUND.into_raw()
    );
}

#[fuchsia::test]
async fn file_link_into_existing() {
    let Some(fixture) =
        Fixture::new(fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE).await
    else {
        return;
    };

    assert_eq!(
        fixture
            .file
            .link_into(fixture.get_token().await, "existing")
            .await
            .expect("link_into (FIDL) failed")
            .expect_err("link_into succeeded"),
        zx::Status::ALREADY_EXISTS.into_raw()
    );
}

#[fuchsia::test]
async fn file_link_into_target_unlinked_dir() {
    let Some(fixture) =
        Fixture::new(fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE).await
    else {
        return;
    };

    let target_dir = open_dir_with_flags(
        &fixture.dir,
        fio::OpenFlags::CREATE | fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE,
        "dir",
    )
    .await;

    let token = get_token(&target_dir).await.into();

    fixture
        .dir
        .unlink("dir", &fio::UnlinkOptions::default())
        .await
        .expect("unlink (FIDL) failed")
        .expect("unlink failed");

    assert_eq!(
        fixture
            .file
            .link_into(token, "existing")
            .await
            .expect("link_into (FIDL) failed")
            .expect_err("link_into succeeded"),
        zx::Status::ACCESS_DENIED.into_raw()
    );
}
