// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

mod gtest;
mod helpers;
mod runner;
mod test_suite;

use anyhow::{anyhow, Error};
use fidl::endpoints::DiscoverableProtocolMarker;
use fidl_fuchsia_component_runner as frunner;
use fidl_fuchsia_io as fio;
use fuchsia_zircon as zx;
use vfs::directory::{entry::DirectoryEntry, helper::DirectlyMutable};

#[fuchsia::main(logging_tags=["starnix_test_runner"])]
async fn main() -> Result<(), Error> {
    // The name of the directory capability that is offered to the starnix_kernel instances.
    const KERNELS_DIRECTORY: &str = "kernels";
    const SVC_DIRECTORY: &str = "svc";

    let outgoing_dir_handle =
        fuchsia_runtime::take_startup_handle(fuchsia_runtime::HandleType::DirectoryRequest.into())
            .ok_or(anyhow!("Failed to get startup handle"))?;
    let outgoing_dir_server_end =
        fidl::endpoints::ServerEnd::new(zx::Channel::from(outgoing_dir_handle));

    let outgoing_dir = vfs::directory::immutable::simple();
    let kernels_dir = vfs::directory::immutable::simple();
    // Add the directory that is offered to starnix_kernel instances, to read their configuration
    // from.
    outgoing_dir.add_entry(KERNELS_DIRECTORY, kernels_dir.clone())?;

    let svc_dir = vfs::directory::immutable::simple();
    svc_dir.add_entry(
        frunner::ComponentRunnerMarker::PROTOCOL_NAME,
        vfs::service::host(move |requests| {
            let kernels_dir = kernels_dir.clone();
            async move {
                runner::handle_runner_requests(kernels_dir.clone(), requests)
                    .await
                    .expect("Error serving runner requests.")
            }
        }),
    )?;
    outgoing_dir.add_entry(SVC_DIRECTORY, svc_dir.clone())?;

    let execution_scope = vfs::execution_scope::ExecutionScope::new();
    outgoing_dir.open(
        execution_scope.clone(),
        fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE,
        vfs::path::Path::dot(),
        outgoing_dir_server_end,
    );
    execution_scope.wait().await;

    Ok(())
}
