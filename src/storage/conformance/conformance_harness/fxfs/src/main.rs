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
                    name: Some(name),
                    entries,
                    ..
                }) => {
                    let new_dir = open_dir(&dest, FLAGS | fio::OpenFlags::DIRECTORY, &name)
                        .await
                        .context(format!("failed to create directory {name}"))?;
                    if let Some(entries) = entries {
                        queue.push((new_dir, entries));
                    }
                }
                io_test::DirectoryEntry::File(io_test::File {
                    name: Some(name), contents, ..
                }) => {
                    let file = open_file(&dest, FLAGS, &name)
                        .await
                        .context(format!("failed to create file {name}"))?;
                    if let Some(contents) = contents {
                        fuchsia_fs::file::write(&file, contents)
                            .await
                            .context(format!("failed to write contents for {name}"))?;
                    }
                }
                io_test::DirectoryEntry::VmoFile(io_test::VmoFile {
                    name: Some(name),
                    vmo: Some(vmo),
                    ..
                }) => {
                    let file = open_file(&dest, FLAGS, &name)
                        .await
                        .context(format!("failed to create file {name}"))?;
                    fuchsia_fs::file::write(&file, &vmo.read_to_vec(0, vmo.get_content_size()?)?)
                        .await
                        .context(format!("failed to write contents for {name}"))?;
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
                    mutable_file: Some(true),
                    supports_create: Some(true),
                    supports_executable_file: Some(false),
                    supports_vmo_file: Some(false),
                    supports_remote_dir: Some(false),
                    supports_get_backing_memory: Some(true),
                    supports_rename: Some(true),
                    supports_link: Some(true),
                    supports_set_attr: Some(true),
                    supports_get_token: Some(true),
                    conformant_path_handling: Some(true),
                    supports_unlink: Some(true),
                    ..Default::default()
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
                if let Some(entries) = root.entries {
                    add_entries(
                        fuchsia_fs::directory::clone_no_describe(&dir, None).expect("clone failed"),
                        entries,
                    )
                    .await
                    .expect("add_entries failed");
                }
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

#[fuchsia::main]
async fn main() -> Result<(), Error> {
    let mut fs = ServiceFs::new_local();
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
