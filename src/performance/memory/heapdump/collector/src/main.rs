// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl_fuchsia_memory_heapdump_process as fheapdump_process;
use fuchsia_component::server::ServiceFs;
use futures::StreamExt;
use tracing::warn;

mod process;
mod process_v1;
mod registry;

use registry::Registry;

/// All FIDL services that are exposed by this component's ServiceFs.
enum Service {
    /// The `fuchsia.memory.heapdump.process.Registry` protocol.
    Process(fheapdump_process::RegistryRequestStream),
}

#[fuchsia::main]
async fn main() -> Result<(), anyhow::Error> {
    let registry = Registry::new();

    let mut service_fs = ServiceFs::new();
    service_fs.dir("svc").add_fidl_service(Service::Process);
    service_fs.take_and_serve_directory_handle()?;

    service_fs
        .for_each_concurrent(None, |stream| async {
            match stream {
                Service::Process(stream) => {
                    if let Err(error) = registry.serve_process_stream(stream).await {
                        warn!(%error, "Error while serving process");
                    }
                }
            }
        })
        .await;

    Ok(())
}
