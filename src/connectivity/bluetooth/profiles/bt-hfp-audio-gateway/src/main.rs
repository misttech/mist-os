// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
#![recursion_limit = "1024"]

use anyhow::{Context, Error};
use battery_client::BatteryClient;
use fidl_fuchsia_media as media;
use fuchsia_component::server::ServiceFs;
use fuchsia_inspect_derive::Inspect;
use futures::{channel::mpsc, future, pin_mut};
use std::collections::HashSet;
use tracing::{debug, error, info, warn};

use crate::{
    config::AudioGatewayFeatureSupport, features::CodecId, fidl_service::run_services, hfp::Hfp,
    profile::register_audio_gateway, sco_connector::ScoConnector,
};

mod a2dp;
mod audio;
mod config;
mod error;
mod features;
mod fidl_service;
mod hfp;
mod inspect;
mod peer;
mod profile;
mod sco_connector;
mod service_definitions;

fn controller_codecs(config: &hfp_profile_config::Config) -> HashSet<features::CodecId> {
    let mut controller_codecs = HashSet::new();
    if config.controller_encoding_cvsd {
        let _ = controller_codecs.insert(CodecId::CVSD);
    }
    if config.controller_encoding_msbc {
        let _ = controller_codecs.insert(CodecId::MSBC);
    }
    controller_codecs
}

#[fuchsia::main(logging_tags = ["hfp-ag"])]
async fn main() -> Result<(), Error> {
    let fs = ServiceFs::new();

    let inspector = fuchsia_inspect::Inspector::default();
    let _inspect_server_task =
        inspect_runtime::publish(&inspector, inspect_runtime::PublishOptions::default());

    let config = hfp_profile_config::Config::take_from_startup_handle();
    let controller_codecs = controller_codecs(&config);
    let feature_support = AudioGatewayFeatureSupport::load_from_config(config);
    debug!(?feature_support, "Starting HFP Audio Gateway");
    let (profile_client, profile_svc) = register_audio_gateway(feature_support)?;

    // Power integration is optional - the HFP component will function even power integration is
    // unavailable.
    let battery_client = match BatteryClient::create() {
        Err(e) => {
            info!("Power integration unavailable: {:?}", e);
            None
        }
        Ok(batt) => Some(batt),
    };

    let (call_manager_sender, call_manager_receiver) = mpsc::channel(1);
    let (test_request_sender, test_request_receiver) = mpsc::channel(1);

    let audio_proxy =
        fuchsia_component::client::connect_to_protocol::<media::AudioDeviceEnumeratorMarker>()
            .with_context(|| format!("Error connecting to audio_core"))?;
    let audio =
        Box::new(audio::CodecAudioControl::setup(audio_proxy, controller_codecs.clone()).await?);
    let sco_connector = ScoConnector::build(profile_svc.clone(), controller_codecs);

    let mut hfp = Hfp::new(
        profile_client,
        profile_svc,
        battery_client,
        audio,
        call_manager_receiver,
        feature_support,
        sco_connector,
        test_request_receiver,
    );
    if let Err(e) = hfp.iattach(&inspector.root(), "hfp") {
        warn!("Couldn't attach inspect to HFP: {:?}", e);
    }

    let hfp = hfp.run();
    pin_mut!(hfp);

    let services = run_services(fs, call_manager_sender, test_request_sender);
    pin_mut!(services);

    info!("HFP Audio Gateway running.");

    match future::select(services, hfp).await {
        future::Either::Left((Ok(()), _)) => {
            warn!("Service FS directory handle closed. Exiting.");
        }
        future::Either::Left((Err(e), _)) => {
            error!("Error encountered running Service FS: {e}. Exiting");
        }
        future::Either::Right((Ok(()), _)) => {
            warn!("All HFP related connections to this component have been disconnected. Exiting.");
        }
        future::Either::Right((Err(e), _)) => {
            error!("Error encountered running main HFP loop: {e}. Exiting.");
        }
    }

    Ok(())
}
