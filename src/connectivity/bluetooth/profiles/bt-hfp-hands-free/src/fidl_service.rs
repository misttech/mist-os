// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{Context, Error};
use fidl_fuchsia_bluetooth_hfp as fidl_hfp;
use fuchsia_component::server::{ServiceFs, ServiceObj};
use futures::channel::mpsc;
use futures::{SinkExt, StreamExt};
use tracing::info;

pub async fn serve(
    mut fs: ServiceFs<ServiceObj<'_, fidl_hfp::HandsFreeRequestStream>>,
    mut hfp_stream_sender: mpsc::Sender<fidl_hfp::HandsFreeRequestStream>,
) -> Result<(), Error> {
    let _ = fs.dir("svc").add_fidl_service(|s: fidl_hfp::HandsFreeRequestStream| s);
    let _ = fs.take_and_serve_directory_handle().context("Failed to serve ServiceFs directory")?;

    while let Some(stream) = fs.next().await {
        info!("New HFP Hands-Free client connection");
        hfp_stream_sender.send(stream).await.context("Unable to send hands free stream")?;
    }

    Ok(())
}
