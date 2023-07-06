// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Test authenticator is a test-only authenticator that produces fake authentication
//! events to be consumed by identity components during tests.

mod storage_unlock;
mod test_interaction;

use {
    crate::storage_unlock::StorageUnlockMechanism,
    anyhow::Error,
    fuchsia_async as fasync,
    fuchsia_component::server::ServiceFs,
    futures::StreamExt,
    tracing::{error, info},
};

#[fuchsia::main(logging_tags = ["identity", "test_authenticator"])]
async fn main() -> Result<(), Error> {
    info!("Starting test authenticator");

    let mut fs = ServiceFs::new();
    fs.dir("svc").add_fidl_service(move |stream| {
        fasync::Task::spawn(async move {
            let mechanism = StorageUnlockMechanism::new();
            mechanism
                .handle_requests_from_stream(stream)
                .await
                .unwrap_or_else(|err| error!(?err, "Error handling storage unlock stream"));
        })
        .detach();
    });

    fs.take_and_serve_directory_handle().expect("Failed to serve directory");

    fs.collect::<()>().await;
    Ok(())
}
