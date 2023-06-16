// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::{Context, Error},
    fidl_fuchsia_wlan_wlanix as fidl_wlanix, fuchsia_async as fasync,
    fuchsia_component::server::ServiceFs,
    futures::StreamExt,
    tracing::{error, info, warn},
};

async fn handle_wifi_sta_iface_request(req: fidl_wlanix::WifiStaIfaceRequest) -> Result<(), Error> {
    match req {
        fidl_wlanix::WifiStaIfaceRequest::GetName { responder } => {
            info!("fidl_wlanix::WifiStaIfaceRequest::GetName");
            responder.send("sta-iface-name").context("send GetName response")?;
        }
        fidl_wlanix::WifiStaIfaceRequest::_UnknownMethod { ordinal, .. } => {
            warn!("Unknown WifiStaIfaceRequest ordinal: {}", ordinal);
        }
    }
    Ok(())
}

async fn serve_wifi_sta_iface(reqs: fidl_wlanix::WifiStaIfaceRequestStream) {
    reqs.for_each_concurrent(None, |req| async {
        match req {
            Ok(req) => {
                if let Err(e) = handle_wifi_sta_iface_request(req).await {
                    warn!("Failed to handle WifiStaIfaceRequest: {}", e);
                }
            }
            Err(e) => {
                error!("Wifi sta iface request stream failed: {}", e);
            }
        }
    })
    .await;
}

async fn handle_wifi_chip_request(req: fidl_wlanix::WifiChipRequest) -> Result<(), Error> {
    match req {
        fidl_wlanix::WifiChipRequest::CreateStaIface { iface, responder, .. } => {
            info!("fidl_wlanix::WifiChipRequest::CreateStaIface");
            let reqs = iface.into_stream().context("create WifiStaIface stream")?;
            responder.send(Ok(())).context("send CreateStaIface response")?;
            serve_wifi_sta_iface(reqs).await;
        }
        fidl_wlanix::WifiChipRequest::_UnknownMethod { ordinal, .. } => {
            warn!("Unknown WifiChipRequest ordinal: {}", ordinal);
        }
    }
    Ok(())
}

async fn serve_wifi_chip(_chip_id: u32, reqs: fidl_wlanix::WifiChipRequestStream) {
    reqs.for_each_concurrent(None, |req| async {
        match req {
            Ok(req) => {
                if let Err(e) = handle_wifi_chip_request(req).await {
                    warn!("Failed to handle WifiChipRequest: {}", e);
                }
            }
            Err(e) => {
                error!("Wifi chip request stream failed: {}", e);
            }
        }
    })
    .await;
}

async fn handle_wifi_request(req: fidl_wlanix::WifiRequest) -> Result<(), Error> {
    match req {
        fidl_wlanix::WifiRequest::GetChip { chip_id, chip, responder } => {
            info!("fidl_wlanix::WifiRequest::GetChip - chip_id {}", chip_id);
            let chip_stream = chip.into_stream().context("create WifiChip stream")?;
            responder.send(Ok(())).context("send GetChip response")?;
            serve_wifi_chip(chip_id, chip_stream).await;
        }
        fidl_wlanix::WifiRequest::_UnknownMethod { ordinal, .. } => {
            warn!("Unknown WifiRequest ordinal: {}", ordinal);
        }
    }
    Ok(())
}

async fn serve_wifi(reqs: fidl_wlanix::WifiRequestStream) {
    reqs.for_each_concurrent(None, |req| async {
        match req {
            Ok(req) => {
                if let Err(e) = handle_wifi_request(req).await {
                    warn!("Failed to handle WifiRequest: {}", e);
                }
            }
            Err(e) => {
                error!("Wifi request stream failed: {}", e);
            }
        }
    })
    .await;
}

async fn handle_wlanix_request(req: fidl_wlanix::WlanixRequest) -> Result<(), Error> {
    match req {
        fidl_wlanix::WlanixRequest::GetWifi { wifi, responder } => {
            info!("fidl_wlanix::WlanixRequest::GetWifi");
            let wifi_stream = wifi.into_stream().context("create Wifi stream")?;
            responder.send(Ok(())).context("send GetWifi response")?;
            serve_wifi(wifi_stream).await;
        }
        fidl_wlanix::WlanixRequest::_UnknownMethod { ordinal, .. } => {
            warn!("Unknown WlanixRequest ordinal: {}", ordinal);
        }
    }
    Ok(())
}

async fn serve_wlanix(reqs: fidl_wlanix::WlanixRequestStream) {
    reqs.for_each_concurrent(None, |req| async {
        match req {
            Ok(req) => {
                if let Err(e) = handle_wlanix_request(req).await {
                    warn!("Failed to handle WlanixRequest: {}", e);
                }
            }
            Err(e) => {
                error!("Wlanix request stream failed: {}", e);
            }
        }
    })
    .await;
}

async fn serve_fidl() -> Result<(), Error> {
    let mut fs = ServiceFs::new();
    let _ = fs.dir("svc").add_fidl_service(move |reqs| serve_wlanix(reqs));
    fs.take_and_serve_directory_handle()?;
    fs.for_each_concurrent(None, |t| async { t.await }).await;
    Ok(())
}

#[fasync::run_singlethreaded]
async fn main() {
    diagnostics_log::initialize(
        diagnostics_log::PublishOptions::default()
            .tags(&["wlan", "wlanix"])
            .enable_metatag(diagnostics_log::Metatag::Target),
    )
    .expect("Failed to initialize wlanix logs");
    info!("Starting Wlanix");
    match serve_fidl().await {
        Ok(()) => info!("Wlanix exiting cleanly"),
        Err(e) => error!("Wlanix exiting with error: {}", e),
    }
}
