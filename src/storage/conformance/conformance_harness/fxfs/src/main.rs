// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! fuchsia io conformance testing harness for Fxfs

use {
    anyhow::{Context as _, Error},
    fidl_fuchsia_io as fio,
    fidl_fuchsia_io_test::{
        self as io_test, Io1Config, Io1HarnessRequest, Io1HarnessRequestStream,
    },
    fuchsia_component::server::ServiceFs,
    futures::prelude::*,
    fxfs_testing::{open_dir, open_file, TestFixture},
    std::sync::atomic::{AtomicU64, Ordering},
    tracing::error,
};

struct Harness(Io1HarnessRequestStream);

const FLAGS: fio::OpenFlags = fio::OpenFlags::CREATE
    .union(fio::OpenFlags::RIGHT_READABLE)
    .union(fio::OpenFlags::RIGHT_WRITABLE);

async fn add_entries(
    dest: fio::DirectoryProxy,
    entries: Vec<Option<Box<io_test::DirectoryEntry>>>,
) -> Result<(), Error> {
    let mut queue = vec![(dest, entries)];
    while let Some((dest, entries)) = queue.pop() {
        for entry in entries {
            match *entry.unwrap() {
                io_test::DirectoryEntry::Directory(io_test::Directory {
                    name, entries, ..
                }) => {
                    let new_dir = open_dir(&dest, FLAGS | fio::OpenFlags::DIRECTORY, &name)
                        .await
                        .context(format!("failed to create directory {name}"))?;
                    queue.push((new_dir, entries));
                }
                io_test::DirectoryEntry::File(io_test::File { name, contents, .. }) => {
                    let file = open_file(&dest, FLAGS, &name)
                        .await
                        .context(format!("failed to create file {name}"))?;
                    if !contents.is_empty() {
                        fuchsia_fs::file::write(&file, contents)
                            .await
                            .context(format!("failed to write contents for {name}"))?;
                    }
                }
                _ => panic!("Not supported"),
            }
        }
    }
    Ok(())
}

async fn run(mut stream: Io1HarnessRequestStream, fixture: &TestFixture) -> Result<(), Error> {
    static COUNTER: AtomicU64 = AtomicU64::new(0);

    while let Some(request) = stream.try_next().await.context("error running harness server")? {
        match request {
            Io1HarnessRequest::GetConfig { responder } => {
                responder.send(&Io1Config {
                    supports_create: true,
                    supports_executable_file: false,
                    supports_get_backing_memory: true,
                    supports_remote_dir: false,
                    supports_rename: true,
                    supports_link: true,
                    supports_get_token: true,
                    supports_unlink: true,
                    supports_open2: true,
                    supports_get_attributes: true,
                    supports_update_attributes: true,
                    supports_link_into: true,
                    supports_directory_watchers: true,
                    supports_append: true,
                    supported_attributes: fio::NodeAttributesQuery::PROTOCOLS
                        | fio::NodeAttributesQuery::ABILITIES
                        | fio::NodeAttributesQuery::CONTENT_SIZE
                        | fio::NodeAttributesQuery::STORAGE_SIZE
                        | fio::NodeAttributesQuery::LINK_COUNT
                        | fio::NodeAttributesQuery::ID
                        | fio::NodeAttributesQuery::CREATION_TIME
                        | fio::NodeAttributesQuery::MODIFICATION_TIME
                        | fio::NodeAttributesQuery::MODE
                        | fio::NodeAttributesQuery::UID
                        | fio::NodeAttributesQuery::GID
                        | fio::NodeAttributesQuery::RDEV
                        | fio::NodeAttributesQuery::ACCESS_TIME,
                })?;
            }
            Io1HarnessRequest::GetDirectory {
                root,
                flags,
                directory_request,
                control_handle: _,
            } => {
                let counter = COUNTER.fetch_add(1, Ordering::SeqCst);
                let dir = open_dir(
                    fixture.root(),
                    FLAGS | fio::OpenFlags::DIRECTORY,
                    &format!("test.{}", counter),
                )
                .await
                .unwrap();
                add_entries(
                    fuchsia_fs::directory::clone_no_describe(&dir, None).expect("clone failed"),
                    root.entries,
                )
                .await
                .expect("add_entries failed");
                dir.open(
                    flags,
                    fio::ModeType::empty(),
                    ".",
                    directory_request.into_channel().into(),
                )
                .unwrap();
            }
        };
    }

    Ok(())
}

#[fuchsia::main(threads = 4)]
async fn main() -> Result<(), Error> {
    let mut fs = ServiceFs::new();
    fs.dir("svc").add_fidl_service(Harness);
    fs.take_and_serve_directory_handle()?;

    let fixture = TestFixture::new().await;

    fs.for_each_concurrent(10_000, |Harness(stream)| {
        run(stream, &fixture).unwrap_or_else(|e| error!("Error processing request: {e:?}"))
    })
    .await;

    fixture.close().await;

    Ok(())
}
