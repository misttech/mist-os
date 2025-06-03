// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Error;
use fuchsia_async as fasync;
use fuchsia_component::server::ServiceFs;
use futures::stream::StreamExt;
use log::error;
use std::sync::Arc;

mod audio_facade;
mod input_worker;
mod output_worker;
mod server;
mod util;

use crate::audio_facade::AudioFacade;

#[fuchsia::main]
async fn main() -> Result<(), Error> {
    let mut fs = ServiceFs::new_local();
    let audio_facade = Arc::new(AudioFacade::new().await?);

    let facade_one = audio_facade.clone();
    fs.dir("svc").add_fidl_service(move |stream| {
        let audio_facade_clone = facade_one.clone();
        fasync::Task::spawn(async move {
            server::handle_injection_request(audio_facade_clone, stream)
                .await
                .unwrap_or_else(|e| error!("Error handling injection channel: {:?}", e))
        })
        .detach();
    });

    fs.dir("svc").add_fidl_service(move |stream| {
        let audio_facade_clone = audio_facade.clone();
        fasync::Task::spawn(async move {
            server::handle_capture_request(audio_facade_clone, stream)
                .await
                .unwrap_or_else(|e| error!("Error handling recording channel: {:?}", e))
        })
        .detach();
    });

    fs.take_and_serve_directory_handle()?;
    fs.collect::<()>().await;

    Ok(())
}
