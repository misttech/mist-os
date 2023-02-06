// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::fidl_server::{FidlServer, OutgoingService},
    crate::hypervisor::FuchsiaHypervisor,
    anyhow::Context,
    fuchsia_component::server,
};

mod address;
mod device_manager;
mod fidl_server;
mod hw;
mod hypervisor;
mod memory;
mod vcpu;
mod virtual_machine;

#[fuchsia::main(logging = true, threads = 1)]
async fn main() -> Result<(), anyhow::Error> {
    let mut fs = server::ServiceFs::new();
    fs.dir("svc")
        .add_fidl_service(OutgoingService::Guest)
        .add_fidl_service(OutgoingService::GuestLifecycle);
    fs.take_and_serve_directory_handle().context("Error starting server")?;
    let hypervisor = FuchsiaHypervisor::new().await.map_err(|e| {
        tracing::error!("Failed to create hypervisor: {}", e);
        e
    })?;
    let mut server = FidlServer::new(hypervisor);
    server.run(fs).await;
    Ok(())
}
