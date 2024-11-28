// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{format_err, Context as _, Error};
use async_utils::hanging_get::client::HangingGetStream;
use fidl_fuchsia_bluetooth_map::{MessagingClientMarker, MessagingClientProxy};
use fuchsia_async as fasync;
use fuchsia_component::client::connect_to_protocol;
use futures::{pin_mut, FutureExt, TryStreamExt};
use std::io;
use tracing::{info, warn};

mod accessor;
mod commands;
mod repl;

use crate::accessor::AccessorClient;
use crate::repl::start_accessor_loop;

#[fasync::run_singlethreaded]
async fn main() -> Result<(), Error> {
    tracing_subscriber::fmt()
        .compact()
        .with_max_level(tracing::Level::INFO)
        .with_writer(io::stderr)
        .init();

    // Connect to message client service.
    let svc = connect_to_protocol::<MessagingClientMarker>()
        .context("Failed to connect to MessagingClient FIDL interface")?;

    let mut stream = HangingGetStream::new(svc.clone(), MessagingClientProxy::watch_accessor);
    info!("Waiting for a peer...");
    while let Some(item) = stream.try_next().await? {
        match item {
            Ok(response) => {
                let client = AccessorClient::new(
                    response.peer_id.unwrap(),
                    response.accessor.unwrap().into_proxy(),
                );
                let accessor_loop = start_accessor_loop(client.clone()).fuse();
                pin_mut!(accessor_loop);
                if let Err(e) = accessor_loop.await {
                    warn!(?e, "MESSAGE_ACCESSOR REPL for peer {:?} closed", client.peer_id());
                }
            }
            Err(e) => {
                warn!(?e, "Error while getting an Accessor FIDL");
                return Err(format_err!("{e:?}"));
            }
        }
    }
    Ok(())
}
