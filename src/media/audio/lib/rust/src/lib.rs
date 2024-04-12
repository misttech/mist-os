// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{anyhow, Context, Error};
use fidl::endpoints::ServerEnd;
use fidl_fuchsia_audio_controller as fac;
use futures::TryStreamExt;
use std::sync::atomic::{AtomicBool, Ordering};

pub mod device;
pub mod format;
pub mod format_set;
pub mod registry;

pub use format::{parse_duration, str_to_clock, Format};
pub use registry::Registry;

pub async fn stop_listener(
    canceler: ServerEnd<fac::RecordCancelerMarker>,
    stop_signal: &AtomicBool,
) -> Result<(), Error> {
    let mut stream = canceler
        .into_stream()
        .map_err(|e| anyhow!("Error turning canceler server into stream {}", e))?;

    let item = stream.try_next().await;
    stop_signal.store(true, Ordering::SeqCst);

    match item {
        Ok(Some(request)) => match request {
            fac::RecordCancelerRequest::Cancel { responder } => {
                responder.send(Ok(())).context("FIDL error with stop request")
            }
            _ => Err(anyhow!("Unimplemented method on canceler")),
        },
        Ok(None) => Ok(()),
        Err(e) => Err(anyhow!("FIDL error with stop request: {e}")),
    }
}
