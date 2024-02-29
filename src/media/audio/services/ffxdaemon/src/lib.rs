// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use {
    anyhow::{Context, Error},
    fidl_fuchsia_audio_controller::{RecordCancelerMarker, RecordCancelerRequest},
    futures::TryStreamExt,
};

pub async fn stop_listener(
    canceler: fidl::endpoints::ServerEnd<RecordCancelerMarker>,
    stop_signal: &std::sync::atomic::AtomicBool,
) -> Result<(), Error> {
    let mut stream = canceler
        .into_stream()
        .map_err(|e| anyhow::anyhow!("Error turning canceler server into stream {}", e))?;

    let item = stream.try_next().await;
    stop_signal.store(true, std::sync::atomic::Ordering::SeqCst);

    match item {
        Ok(Some(request)) => match request {
            RecordCancelerRequest::Cancel { responder } => {
                responder.send(Ok(())).context("FIDL error with stop request")
            }
            _ => Err(anyhow::anyhow!("Unimplemented method on canceler")),
        },
        Ok(None) => Ok(()),
        Err(e) => Err(anyhow::anyhow!("FIDL error with stop request: {e}")),
    }
}
