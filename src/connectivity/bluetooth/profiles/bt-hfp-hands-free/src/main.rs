// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{Context, Error};
use bt_hfp::codec_id::CodecId;
use bt_hfp::{audio, sco};
use fidl_fuchsia_bluetooth_hfp as fidl_hfp;
use fuchsia_component::client::connect_to_protocol;
use fuchsia_component::server::{ServiceFs, ServiceObj};
use futures::StreamExt;
use log::{debug, error, info};
use std::collections::HashSet;

use crate::config::HandsFreeFeatureSupport;
use crate::hfp::Hfp;

mod config;
mod features;
mod hfp;
mod one_to_one;
mod peer;
mod profile;
mod service_definition;

type HandsFreeFidlService = ServiceFs<ServiceObj<'static, fidl_hfp::HandsFreeRequestStream>>;

#[fuchsia::main(logging_tags = ["bt-hfp-hf"])]
async fn main() -> Result<(), Error> {
    debug!("Starting HFP Hands Free");

    let mut fs = ServiceFs::new();

    let _inspect_server_task = start_inspect();

    let feature_support = HandsFreeFeatureSupport::load()?;
    let (profile_client, profile_proxy) = profile::register_hands_free(feature_support)?;

    let controller_codecs = controller_codecs();
    let sco_connector = sco::Connector::build(profile_proxy.clone(), controller_codecs.clone());

    let provider_proxy = connect_to_protocol::<fidl_fuchsia_audio_device::ProviderMarker>()?;
    // TODO(b/416737099) Support other audio connection types.
    let audio = Box::new(audio::CodecControl::new(provider_proxy));

    serve_fidl(&mut fs)?;

    let hfp =
        Hfp::new(feature_support, profile_client, profile_proxy, sco_connector, audio, fs.fuse());
    let result = hfp.run().await;

    match result {
        Ok(()) => {
            info!("Profile client connection closed. Exiting.");
        }
        Err(e) => {
            error!("Error encountered running main HFP loop: {e}. Exiting.");
        }
    }

    Ok(())
}

fn start_inspect() -> Option<inspect_runtime::PublishedInspectController> {
    // TODO(https://fxbug.dev/136817): Add inspect.
    let inspector = fuchsia_inspect::Inspector::default();
    inspect_runtime::publish(&inspector, inspect_runtime::PublishOptions::default())
}

fn serve_fidl(fs: &mut HandsFreeFidlService) -> Result<(), Error> {
    let _ = fs.dir("svc").add_fidl_service(|s: fidl_hfp::HandsFreeRequestStream| s);
    let _ = fs.take_and_serve_directory_handle().context("Failed to serve ServiceFs directory")?;

    Ok(())
}

fn controller_codecs() -> HashSet<CodecId> {
    let mut controller_codecs = HashSet::new();

    // TODO(b/323584284) Read support for these codecs from config.
    let _ = controller_codecs.insert(CodecId::CVSD);
    let _ = controller_codecs.insert(CodecId::MSBC);

    controller_codecs
}
