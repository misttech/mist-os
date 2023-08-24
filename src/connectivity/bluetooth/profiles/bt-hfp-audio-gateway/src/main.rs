// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
#![recursion_limit = "1024"]

use anyhow::{format_err, Context, Error};
use battery_client::BatteryClient;
use fidl_fuchsia_media as media;
use fuchsia_component::server::ServiceFs;
use fuchsia_inspect_derive::Inspect;
use futures::{channel::mpsc, future, pin_mut};
use tracing::{debug, error, info, warn};

use crate::{
    audio::AudioControl, config::AudioGatewayFeatureSupport, fidl_service::run_services, hfp::Hfp,
    profile::register_audio_gateway,
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

async fn setup_audio(
    in_band_sco: bool,
    audio_proxy: media::AudioDeviceEnumeratorProxy,
) -> Result<Box<dyn AudioControl>, audio::AudioError> {
    if in_band_sco {
        Ok(Box::from(audio::InbandAudioControl::create(audio_proxy)?))
    } else {
        Ok(Box::from(audio::DaiAudioControl::discover(audio_proxy).await?))
    }
}

#[fuchsia::main(logging_tags = ["hfp-ag"])]
async fn main() -> Result<(), Error> {
    let fs = ServiceFs::new();

    let inspector = fuchsia_inspect::Inspector::default();
    let _inspect_server_task =
        inspect_runtime::publish(&inspector, inspect_runtime::PublishOptions::default());

    let config = hfp_profile_config::Config::take_from_startup_handle();
    let in_band_sco = config.in_band_sco;
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
    let audio = match setup_audio(in_band_sco, audio_proxy).await {
        Err(e) => {
            error!(?e, "Couldn't setup audio");
            return Err(format_err!("Audio setup failed: {e:?}"));
        }
        Ok(audio) => audio,
    };
    let mut hfp = Hfp::new(
        profile_client,
        profile_svc,
        battery_client,
        audio,
        call_manager_receiver,
        feature_support,
        in_band_sco,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[fuchsia::test]
    async fn setup_audio_chooses_wisely() {
        // This should not panic, inband audio doesn't need to connect to anything other than the
        // proxy.
        // Dai Audio does, and will panic this test.
        let proxy =
            fidl::endpoints::create_proxy::<media::AudioDeviceEnumeratorMarker>().unwrap().0;
        let _ = setup_audio(true, proxy).await.unwrap();
    }
}
