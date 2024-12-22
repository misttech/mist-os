// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! fuchsia io conformance testing harness for the rust pseudo-fs-mt library

use anyhow::{anyhow, Context as _, Error};
use fidl::endpoints::{create_endpoints, DiscoverableProtocolMarker as _};
use fidl_fuchsia_io as fio;
use fidl_fuchsia_io_test::{
    self as io_test, HarnessConfig, TestHarnessRequest, TestHarnessRequestStream,
};
use fidl_test_placeholders::{EchoMarker, EchoRequest, EchoRequestStream};
use fuchsia_component::server::ServiceFs;
use futures::prelude::*;
use std::sync::Arc;
use tracing::error;
use vfs::directory::entry_container::Directory;
use vfs::directory::helper::DirectlyMutable;
use vfs::directory::immutable::{simple, Simple};
use vfs::execution_scope::ExecutionScope;
use vfs::file::vmo;
use vfs::path::Path;
use vfs::remote::remote_dir;

struct Harness(TestHarnessRequestStream);

const HARNESS_EXEC_PATH: &'static str = "/pkg/bin/io_conformance_harness_rustvfs";

/// Creates and returns a Rust VFS VmoFile-backed executable file using the contents of the
/// conformance test harness binary itself.
fn new_executable_file() -> Result<Arc<vmo::VmoFile>, Error> {
    let file = fdio::open_fd(HARNESS_EXEC_PATH, fio::PERM_READABLE | fio::PERM_EXECUTABLE)?;
    let exec_vmo = fdio::get_vmo_exec_from_file(&file)?;
    let exec_file = vmo::VmoFile::new(
        exec_vmo, /*readable*/ true, /*writable*/ false, /*executable*/ true,
    );
    Ok(exec_file)
}

fn add_entry(entry: io_test::DirectoryEntry, dest: &Arc<Simple>) -> Result<(), Error> {
    match entry {
        io_test::DirectoryEntry::Directory(io_test::Directory { name, entries, .. }) => {
            let new_dir = simple();
            for entry in entries {
                let entry = *entry.expect("Directory entries must not be null");
                add_entry(entry, &new_dir)?;
            }
            dest.add_entry(name, new_dir)?;
        }
        io_test::DirectoryEntry::RemoteDirectory(io_test::RemoteDirectory {
            name,
            remote_client,
            ..
        }) => {
            dest.add_entry(name, remote_dir(remote_client.into_proxy()))?;
        }
        io_test::DirectoryEntry::File(io_test::File { name, contents, .. }) => {
            let new_file = vmo::read_only(contents);
            dest.add_entry(name, new_file)?;
        }
        io_test::DirectoryEntry::ExecutableFile(io_test::ExecutableFile { name, .. }) => {
            let executable_file = new_executable_file()?;
            dest.add_entry(name, executable_file)?;
        }
    }
    Ok(())
}

async fn run(mut stream: TestHarnessRequestStream) -> Result<(), Error> {
    while let Some(request) = stream.try_next().await.context("error running harness server")? {
        match request {
            TestHarnessRequest::GetConfig { responder } => {
                let config = HarnessConfig {
                    // Supported options:
                    supports_executable_file: true,
                    supports_get_backing_memory: true,
                    supports_remote_dir: true,
                    supported_attributes: fio::NodeAttributesQuery::PROTOCOLS
                        | fio::NodeAttributesQuery::ABILITIES
                        | fio::NodeAttributesQuery::CONTENT_SIZE
                        | fio::NodeAttributesQuery::STORAGE_SIZE
                        | fio::NodeAttributesQuery::ID,
                    supports_services: true,
                    // Unsupported options:
                    supports_link_into: false,
                    supports_get_token: false,
                    supports_append: false,
                    supports_truncate: false,
                    supports_modify_directory: false,
                    supports_mutable_file: false,
                };
                responder.send(&config)?;
            }
            TestHarnessRequest::CreateDirectory {
                contents,
                flags,
                object_request,
                control_handle: _,
            } => {
                let dir = simple();
                for entry in contents {
                    if let Some(entry) = entry {
                        add_entry(*entry, &dir)?;
                    }
                }
                let mut object_request = vfs::ObjectRequest::new(
                    flags,
                    &Default::default(),
                    object_request.into_channel(),
                );
                dir.open3(ExecutionScope::new(), Path::dot(), flags, &mut object_request)
                    .expect("failed to open directory");
            }
            TestHarnessRequest::OpenServiceDirectory { responder } => {
                const FLAGS: fio::Flags = fio::PERM_READABLE;
                let svc_dir = simple();
                let service = vfs::service::host(run_echo_server);
                svc_dir.add_entry(EchoMarker::PROTOCOL_NAME, service).unwrap();
                let (svc_client, svc_server) = create_endpoints::<fio::DirectoryMarker>();
                let mut object_request =
                    vfs::ObjectRequest::new(FLAGS, &Default::default(), svc_server.into_channel());
                svc_dir
                    .open3(ExecutionScope::new(), Path::dot(), FLAGS, &mut object_request)
                    .unwrap();
                responder.send(svc_client).unwrap();
            }
        }
    }

    Ok(())
}

async fn run_echo_server(stream: EchoRequestStream) {
    stream
        .map(|result| result.context("failed request"))
        .try_for_each(|request| async move {
            match request {
                EchoRequest::EchoString { value, responder } => {
                    responder
                        .send(value.as_ref().map(String::as_str))
                        .context("error sending response")?;
                }
            }
            Ok(())
        })
        .await
        .unwrap_or_else(|e| error!("Error processing request: {:?}", anyhow!(e)));
}

#[fuchsia::main]
async fn main() -> Result<(), Error> {
    let mut fs = ServiceFs::new_local();
    fs.dir("svc").add_fidl_service(Harness);
    fs.take_and_serve_directory_handle()?;

    let fut = fs.for_each_concurrent(10_000, |Harness(stream)| {
        run(stream).unwrap_or_else(|e| error!("Error processing request: {:?}", anyhow!(e)))
    });

    fut.await;
    Ok(())
}
