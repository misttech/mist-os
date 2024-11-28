// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::framebuffer::{DetectResult, DisplayInfo, Framebuffer};
use anyhow::{Context, Error};
use fidl::endpoints;
use fidl_fuchsia_hardware_display::{
    CoordinatorListenerMarker, CoordinatorMarker, Info, ProviderSynchronousProxy,
};

use futures::executor::block_on;
use futures::{future, TryStreamExt};
use serde_json::json;

fn get_display_coordinator_path() -> anyhow::Result<String> {
    // TODO(liyl): This assumes that a display-coordinator device is ready before the test binary
    // starts. Consider switching to `fuchsia_fs::directory::Watcher` if this flakes.
    const DEVICE_CLASS_PATH: &'static str = "/dev/class/display-coordinator";
    let entries = std::fs::read_dir(DEVICE_CLASS_PATH).context("read directory")?;
    let entry = entries
        .into_iter()
        .next()
        .context("no valid display-coordinator")?
        .context("entry invalid")?;
    Ok(String::from(entry.path().to_string_lossy()))
}

fn convert_info(info: &Info) -> DisplayInfo {
    DisplayInfo {
        id: format!("[mfgr: '{}', model: '{}']", info.manufacturer_name, info.monitor_name),
        width: info.modes[0].horizontal_resolution,
        height: info.modes[0].vertical_resolution,
    }
}

async fn read_info() -> Result<DetectResult, Error> {
    // Connect to the display coordinator.
    let provider = {
        let (client_end, server_end) = zx::Channel::create();
        let display_coordinator_path =
            get_display_coordinator_path().context("get display coordinator path")?;
        fuchsia_component::client::connect_channel_to_protocol_at_path(
            server_end,
            &display_coordinator_path,
        )?;
        ProviderSynchronousProxy::new(client_end)
    };

    let (_coordinator, listener_requests) = {
        let (dc_proxy, dc_server) = endpoints::create_sync_proxy::<CoordinatorMarker>();
        let (listener_client, listener_requests) =
            endpoints::create_request_stream::<CoordinatorListenerMarker>();
        let payload =
            fidl_fuchsia_hardware_display::ProviderOpenCoordinatorWithListenerForPrimaryRequest {
                coordinator: Some(dc_server),
                coordinator_listener: Some(listener_client),
                __source_breaking: fidl::marker::SourceBreaking,
            };
        provider
            .open_coordinator_with_listener_for_primary(payload, zx::MonotonicInstant::INFINITE)?
            .map_err(zx::Status::from_raw)?;
        (dc_proxy, listener_requests)
    };

    let mut stream = listener_requests.try_filter_map(|event| match event {
        fidl_fuchsia_hardware_display::CoordinatorListenerRequest::OnDisplaysChanged {
            added,
            removed: _,
            control_handle: _,
        } => future::ok(Some(added)),
        _ => future::ok(None),
    });
    let displays = &mut stream.try_next().await?.context("failed to get display streams")?;

    Ok(DetectResult {
        displays: displays.iter().map(convert_info).collect(),
        details: json!(format!("{:#?}", displays)),
        ..Default::default()
    })
}

fn read_info_from_display_coordinator() -> DetectResult {
    block_on(read_info()).unwrap_or_else(DetectResult::from_error)
}

pub struct ZirconFramebuffer;

impl Framebuffer for ZirconFramebuffer {
    fn detect_displays(&self) -> DetectResult {
        read_info_from_display_coordinator()
    }
}
