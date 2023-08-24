// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Error;
use fuchsia_async::pin_mut;
use fuchsia_component::server::ServiceFs;
use futures::channel::mpsc;
use futures::future;
use tracing::{debug, error, warn};

use crate::config::HandsFreeFeatureSupport;
use crate::hfp::Hfp;

mod config;
mod features;
mod fidl_service;
mod hfp;
mod peer;
mod profile;
mod service_definition;

#[fuchsia::main(logging_tags = ["hfp-hf"])]
async fn main() -> Result<(), Error> {
    debug!("Starting HFP Hands Free");

    let fs = ServiceFs::new();

    let _inspect_server_task = start_inspect();

    let feature_support = HandsFreeFeatureSupport::load()?;
    let (profile_client, profile_proxy) = profile::register_hands_free(feature_support)?;

    let (call_client_sender, call_client_receiver) = mpsc::channel(0);

    let hfp = Hfp::new(profile_client, profile_proxy, feature_support, call_client_receiver);
    let hfp_fut = hfp.run();
    pin_mut!(hfp_fut);

    let fidl_service_fut = fidl_service::serve(fs, call_client_sender);
    pin_mut!(fidl_service_fut);

    let result = future::select(fidl_service_fut, hfp_fut).await;
    match result {
        future::Either::Left((Ok(()), _)) => {
            warn!("Service FS directory handle closed. Exiting.");
        }
        future::Either::Left((Err(e), _)) => {
            error!("Error encountered running Service FS: {e}. Exiting");
        }
        future::Either::Right((Ok(()), _)) => {
            warn!("Profile client connection closed. Exiting.");
        }
        future::Either::Right((Err(e), _)) => {
            error!("Error encountered running main HFP loop: {e}. Exiting.");
        }
    }

    Ok(())
}

fn start_inspect() -> Option<fuchsia_async::Task<()>> {
    let inspector = fuchsia_inspect::Inspector::default();
    inspect_runtime::publish(&inspector, inspect_runtime::PublishOptions::default())
}
