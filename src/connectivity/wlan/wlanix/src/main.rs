// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::{Context, Error},
    fidl_fuchsia_wlan_wlanix as fidl_wlanix, fuchsia_async as fasync,
    fuchsia_component::server::ServiceFs,
    fuchsia_zircon as zx,
    futures::StreamExt,
    parking_lot::Mutex,
    std::sync::Arc,
    tracing::{error, info, warn},
};

#[allow(unused)]
mod nl80211;

const FAKE_CHIP_ID: u32 = 1;

async fn handle_wifi_sta_iface_request(req: fidl_wlanix::WifiStaIfaceRequest) -> Result<(), Error> {
    match req {
        fidl_wlanix::WifiStaIfaceRequest::GetName { responder } => {
            info!("fidl_wlanix::WifiStaIfaceRequest::GetName");
            let response = fidl_wlanix::WifiStaIfaceGetNameResponse {
                iface_name: Some("sta-iface-name".to_string()),
                ..Default::default()
            };
            responder.send(&response).context("send GetName response")?;
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
        fidl_wlanix::WifiChipRequest::CreateStaIface { payload, responder, .. } => {
            info!("fidl_wlanix::WifiChipRequest::CreateStaIface");
            match payload.iface {
                Some(iface) => {
                    let reqs = iface.into_stream().context("create WifiStaIface stream")?;
                    responder.send(Ok(())).context("send CreateStaIface response")?;
                    serve_wifi_sta_iface(reqs).await;
                }
                None => {
                    responder
                        .send(Err(zx::sys::ZX_ERR_INVALID_ARGS))
                        .context("send CreateStaIface response")?;
                }
            }
        }
        fidl_wlanix::WifiChipRequest::GetAvailableModes { responder } => {
            info!("fidl_wlanix::WifiChipRequest::GetAvailableModes");
            let response = fidl_wlanix::WifiChipGetAvailableModesResponse {
                chip_modes: Some(vec![fidl_wlanix::ChipMode {
                    id: Some(1),
                    available_combinations: Some(vec![fidl_wlanix::ChipConcurrencyCombination {
                        limits: Some(vec![fidl_wlanix::ChipConcurrencyCombinationLimit {
                            types: Some(vec![fidl_wlanix::IfaceConcurrencyType::Sta]),
                            max_ifaces: Some(1),
                            ..Default::default()
                        }]),
                        ..Default::default()
                    }]),
                    ..Default::default()
                }]),
                ..Default::default()
            };
            responder.send(&response).context("send GetAvailableModes response")?;
        }
        fidl_wlanix::WifiChipRequest::GetMode { responder } => {
            info!("fidl_wlanix::WifiChipRequest::GetMode");
            let response =
                fidl_wlanix::WifiChipGetModeResponse { mode: Some(0), ..Default::default() };
            responder.send(&response).context("send GetMode response")?;
        }
        fidl_wlanix::WifiChipRequest::GetCapabilities { responder } => {
            info!("fidl_wlanix::WifiChipRequest::GetCapabilities");
            let response = fidl_wlanix::WifiChipGetCapabilitiesResponse {
                capabilities_mask: Some(0),
                ..Default::default()
            };
            responder.send(&response).context("send GetCapabilities response")?;
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

#[derive(Default)]
struct WifiState {
    started: bool,
    callbacks: Vec<fidl_wlanix::WifiEventCallbackProxy>,
}

impl WifiState {
    fn run_callbacks(
        &self,
        callback_fn: fn(&fidl_wlanix::WifiEventCallbackProxy) -> Result<(), fidl::Error>,
        ctx: &'static str,
    ) {
        let mut failed_callbacks = 0u32;
        for callback in &self.callbacks {
            if let Err(_e) = callback_fn(callback) {
                failed_callbacks += 1;
            }
        }
        if failed_callbacks > 0 {
            warn!("Failed sending {} event to {} subscribers", ctx, failed_callbacks);
        }
    }
}

async fn handle_wifi_request(
    req: fidl_wlanix::WifiRequest,
    state: Arc<Mutex<WifiState>>,
) -> Result<(), Error> {
    match req {
        fidl_wlanix::WifiRequest::RegisterEventCallback { payload, .. } => {
            if let Some(callback) = payload.callback {
                state.lock().callbacks.push(callback.into_proxy()?);
            }
        }
        fidl_wlanix::WifiRequest::Start { responder } => {
            let mut state = state.lock();
            state.started = true;
            responder.send(Ok(())).context("send Start response")?;
            state.run_callbacks(fidl_wlanix::WifiEventCallbackProxy::on_start, "OnStart");
        }
        fidl_wlanix::WifiRequest::Stop { responder } => {
            let mut state = state.lock();
            state.started = false;
            responder.send(Ok(())).context("send Stop response")?;
            state.run_callbacks(fidl_wlanix::WifiEventCallbackProxy::on_stop, "OnStop");
        }
        fidl_wlanix::WifiRequest::GetState { responder } => {
            let response = fidl_wlanix::WifiGetStateResponse {
                is_started: Some(state.lock().started),
                ..Default::default()
            };
            responder.send(&response).context("send GetState response")?;
        }
        fidl_wlanix::WifiRequest::GetChipIds { responder } => {
            info!("fidl_wlanix::WifiRequest::GetChipIds");
            let response = fidl_wlanix::WifiGetChipIdsResponse {
                chip_ids: Some(vec![FAKE_CHIP_ID]),
                ..Default::default()
            };
            responder.send(&response).context("send GetChipIds response")?;
        }
        fidl_wlanix::WifiRequest::GetChip { payload, responder } => {
            info!("fidl_wlanix::WifiRequest::GetChip - chip_id {:?}", payload.chip_id);
            match (payload.chip_id, payload.chip) {
                (Some(chip_id), Some(chip)) => {
                    let chip_stream = chip.into_stream().context("create WifiChip stream")?;
                    responder.send(Ok(())).context("send GetChip response")?;
                    serve_wifi_chip(chip_id, chip_stream).await;
                }
                _ => {
                    warn!("No chip_id or chip in fidl_wlanix::WifiRequest::GetChip");
                    responder
                        .send(Err(zx::sys::ZX_ERR_INVALID_ARGS))
                        .context("send GetChip response")?;
                }
            }
        }
        fidl_wlanix::WifiRequest::_UnknownMethod { ordinal, .. } => {
            warn!("Unknown WifiRequest ordinal: {}", ordinal);
        }
    }
    Ok(())
}

async fn serve_wifi(reqs: fidl_wlanix::WifiRequestStream, state: Arc<Mutex<WifiState>>) {
    reqs.for_each_concurrent(None, |req| async {
        match req {
            Ok(req) => {
                if let Err(e) = handle_wifi_request(req, Arc::clone(&state)).await {
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

async fn handle_wlanix_request(
    req: fidl_wlanix::WlanixRequest,
    state: Arc<Mutex<WifiState>>,
) -> Result<(), Error> {
    match req {
        fidl_wlanix::WlanixRequest::GetWifi { payload, .. } => {
            info!("fidl_wlanix::WlanixRequest::GetWifi");
            if let Some(wifi) = payload.wifi {
                let wifi_stream = wifi.into_stream().context("create Wifi stream")?;
                serve_wifi(wifi_stream, Arc::clone(&state)).await;
            }
        }
        fidl_wlanix::WlanixRequest::_UnknownMethod { ordinal, .. } => {
            warn!("Unknown WlanixRequest ordinal: {}", ordinal);
        }
    }
    Ok(())
}

async fn serve_wlanix(reqs: fidl_wlanix::WlanixRequestStream, state: Arc<Mutex<WifiState>>) {
    reqs.for_each_concurrent(None, |req| async {
        match req {
            Ok(req) => {
                if let Err(e) = handle_wlanix_request(req, Arc::clone(&state)).await {
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
    let state = Arc::new(Mutex::new(WifiState::default()));
    let _ = fs.dir("svc").add_fidl_service(move |reqs| serve_wlanix(reqs, Arc::clone(&state)));
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

#[cfg(test)]
mod tests {
    use {
        super::*,
        fidl::endpoints::{create_proxy, create_proxy_and_stream},
        futures::{pin_mut, task::Poll, Future},
        std::pin::Pin,
        wlan_common::assert_variant,
    };

    #[test]
    fn test_wifi_get_state_is_started_false_at_beginning() {
        let (mut test_helper, mut test_fut) = setup_test();

        let get_state_fut = test_helper.wifi_proxy.get_state();
        pin_mut!(get_state_fut);
        assert_variant!(test_helper.exec.run_until_stalled(&mut get_state_fut), Poll::Pending);
        assert_variant!(test_helper.exec.run_until_stalled(&mut test_fut), Poll::Pending);
        let response = assert_variant!(
            test_helper.exec.run_until_stalled(&mut get_state_fut),
            Poll::Ready(Ok(response)) => response
        );
        assert_eq!(response.is_started, Some(false));
    }

    #[test]
    fn test_wifi_get_state_is_started_true_after_start() {
        let (mut test_helper, mut test_fut) = setup_test();

        let start_fut = test_helper.wifi_proxy.start();
        pin_mut!(start_fut);
        assert_variant!(test_helper.exec.run_until_stalled(&mut start_fut), Poll::Pending);

        let get_state_fut = test_helper.wifi_proxy.get_state();
        pin_mut!(get_state_fut);
        assert_variant!(test_helper.exec.run_until_stalled(&mut get_state_fut), Poll::Pending);
        assert_variant!(test_helper.exec.run_until_stalled(&mut test_fut), Poll::Pending);
        let response = assert_variant!(
            test_helper.exec.run_until_stalled(&mut get_state_fut),
            Poll::Ready(Ok(response)) => response
        );
        assert_eq!(response.is_started, Some(true));
    }

    #[test]
    fn test_wifi_get_state_is_started_false_after_stop() {
        let (mut test_helper, mut test_fut) = setup_test();

        let start_fut = test_helper.wifi_proxy.start();
        pin_mut!(start_fut);
        assert_variant!(test_helper.exec.run_until_stalled(&mut start_fut), Poll::Pending);

        let stop_fut = test_helper.wifi_proxy.stop();
        pin_mut!(stop_fut);
        assert_variant!(test_helper.exec.run_until_stalled(&mut stop_fut), Poll::Pending);

        let get_state_fut = test_helper.wifi_proxy.get_state();
        pin_mut!(get_state_fut);
        assert_variant!(test_helper.exec.run_until_stalled(&mut get_state_fut), Poll::Pending);
        assert_variant!(test_helper.exec.run_until_stalled(&mut test_fut), Poll::Pending);
        let response = assert_variant!(
            test_helper.exec.run_until_stalled(&mut get_state_fut),
            Poll::Ready(Ok(response)) => response
        );
        assert_eq!(response.is_started, Some(false));
    }

    #[test]
    fn test_wifi_get_chip_ids() {
        let (mut test_helper, mut test_fut) = setup_test();

        let get_chip_ids_fut = test_helper.wifi_proxy.get_chip_ids();
        pin_mut!(get_chip_ids_fut);
        assert_variant!(test_helper.exec.run_until_stalled(&mut get_chip_ids_fut), Poll::Pending);
        assert_variant!(test_helper.exec.run_until_stalled(&mut test_fut), Poll::Pending);
        let response = assert_variant!(
            test_helper.exec.run_until_stalled(&mut get_chip_ids_fut),
            Poll::Ready(Ok(response)) => response
        );
        assert_eq!(response.chip_ids, Some(vec![1]));
    }

    #[test]
    fn test_wifi_chip_get_available_modes() {
        let (mut test_helper, mut test_fut) = setup_test();

        let get_available_modes_fut = test_helper.wifi_chip_proxy.get_available_modes();
        pin_mut!(get_available_modes_fut);
        assert_variant!(
            test_helper.exec.run_until_stalled(&mut get_available_modes_fut),
            Poll::Pending
        );
        assert_variant!(test_helper.exec.run_until_stalled(&mut test_fut), Poll::Pending);
        let response = assert_variant!(
            test_helper.exec.run_until_stalled(&mut get_available_modes_fut),
            Poll::Ready(Ok(response)) => response
        );
        let expected_response = fidl_wlanix::WifiChipGetAvailableModesResponse {
            chip_modes: Some(vec![fidl_wlanix::ChipMode {
                id: Some(1),
                available_combinations: Some(vec![fidl_wlanix::ChipConcurrencyCombination {
                    limits: Some(vec![fidl_wlanix::ChipConcurrencyCombinationLimit {
                        types: Some(vec![fidl_wlanix::IfaceConcurrencyType::Sta]),
                        max_ifaces: Some(1),
                        ..Default::default()
                    }]),
                    ..Default::default()
                }]),
                ..Default::default()
            }]),
            ..Default::default()
        };
        assert_eq!(response, expected_response);
    }

    #[test]
    fn test_wifi_chip_get_mode() {
        let (mut test_helper, mut test_fut) = setup_test();

        let get_mode_fut = test_helper.wifi_chip_proxy.get_mode();
        pin_mut!(get_mode_fut);
        assert_variant!(test_helper.exec.run_until_stalled(&mut get_mode_fut), Poll::Pending);
        assert_variant!(test_helper.exec.run_until_stalled(&mut test_fut), Poll::Pending);
        let response = assert_variant!(test_helper.exec.run_until_stalled(&mut get_mode_fut), Poll::Ready(Ok(response)) => response);
        assert_eq!(response.mode, Some(0));
    }

    #[test]
    fn test_wifi_chip_get_capabilities() {
        let (mut test_helper, mut test_fut) = setup_test();

        let get_capabilities_fut = test_helper.wifi_chip_proxy.get_capabilities();
        pin_mut!(get_capabilities_fut);
        assert_variant!(
            test_helper.exec.run_until_stalled(&mut get_capabilities_fut),
            Poll::Pending
        );
        assert_variant!(test_helper.exec.run_until_stalled(&mut test_fut), Poll::Pending);
        let response = assert_variant!(
            test_helper.exec.run_until_stalled(&mut get_capabilities_fut),
            Poll::Ready(Ok(response)) => response,
        );
        assert_eq!(response.capabilities_mask, Some(0));
    }

    #[test]
    fn test_wifi_sta_iface_get_name() {
        let (mut test_helper, mut test_fut) = setup_test();

        let get_name_fut = test_helper.wifi_sta_iface_proxy.get_name();
        pin_mut!(get_name_fut);
        assert_variant!(test_helper.exec.run_until_stalled(&mut get_name_fut), Poll::Pending);
        assert_variant!(test_helper.exec.run_until_stalled(&mut test_fut), Poll::Pending);
        let response = assert_variant!(
            test_helper.exec.run_until_stalled(&mut get_name_fut),
            Poll::Ready(Ok(response)) => response
        );
        assert_eq!(response.iface_name, Some("sta-iface-name".to_string()));
    }

    struct TestHelper {
        _wlanix_proxy: fidl_wlanix::WlanixProxy,
        wifi_proxy: fidl_wlanix::WifiProxy,
        wifi_chip_proxy: fidl_wlanix::WifiChipProxy,
        wifi_sta_iface_proxy: fidl_wlanix::WifiStaIfaceProxy,

        // Note: keep the executor field last in the struct so it gets dropped last.
        exec: fasync::TestExecutor,
    }

    fn setup_test() -> (TestHelper, Pin<Box<impl Future<Output = ()>>>) {
        let mut exec = fasync::TestExecutor::new_with_fake_time();
        exec.set_fake_time(fasync::Time::from_nanos(0));

        let (wlanix_proxy, wlanix_stream) = create_proxy_and_stream::<fidl_wlanix::WlanixMarker>()
            .expect("create Wlanix proxy should succeed");
        let (wifi_proxy, wifi_server_end) =
            create_proxy::<fidl_wlanix::WifiMarker>().expect("create Wifi proxy should succeed");
        let result = wlanix_proxy.get_wifi(fidl_wlanix::WlanixGetWifiRequest {
            wifi: Some(wifi_server_end),
            ..Default::default()
        });
        assert_variant!(result, Ok(()));

        let (wifi_chip_proxy, wifi_chip_server_end) = create_proxy::<fidl_wlanix::WifiChipMarker>()
            .expect("create WifiChip proxy should succeed");
        let get_chip_fut = wifi_proxy.get_chip(fidl_wlanix::WifiGetChipRequest {
            chip_id: Some(1),
            chip: Some(wifi_chip_server_end),
            ..Default::default()
        });
        pin_mut!(get_chip_fut);
        assert_variant!(exec.run_until_stalled(&mut get_chip_fut), Poll::Pending);

        let (wifi_sta_iface_proxy, wifi_sta_iface_server_end) =
            create_proxy::<fidl_wlanix::WifiStaIfaceMarker>()
                .expect("create WifiStaIface proxy should succeed");
        let create_sta_iface_fut =
            wifi_chip_proxy.create_sta_iface(fidl_wlanix::WifiChipCreateStaIfaceRequest {
                iface: Some(wifi_sta_iface_server_end),
                ..Default::default()
            });
        pin_mut!(create_sta_iface_fut);
        assert_variant!(exec.run_until_stalled(&mut create_sta_iface_fut), Poll::Pending);

        let state = Arc::new(Mutex::new(WifiState::default()));
        let test_fut = serve_wlanix(wlanix_stream, state);
        let mut test_fut = Box::pin(test_fut);
        assert_eq!(exec.run_until_stalled(&mut test_fut), Poll::Pending);

        assert_variant!(exec.run_until_stalled(&mut get_chip_fut), Poll::Ready(Ok(Ok(()))));
        assert_variant!(exec.run_until_stalled(&mut create_sta_iface_fut), Poll::Ready(Ok(Ok(()))));

        let test_helper = TestHelper {
            _wlanix_proxy: wlanix_proxy,
            wifi_proxy,
            wifi_chip_proxy,
            wifi_sta_iface_proxy,
            exec,
        };
        (test_helper, test_fut)
    }
}
