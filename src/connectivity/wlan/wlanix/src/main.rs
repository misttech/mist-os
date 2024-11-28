// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{bail, Context, Error};
use fuchsia_component::server::ServiceFs;
use fuchsia_sync::Mutex;
use futures::future::OptionFuture;
use futures::{FutureExt, StreamExt};
use ieee80211::{Bssid, MacAddrBytes};
use netlink_packet_core::{NetlinkDeserializable, NetlinkHeader, NetlinkSerializable};
use netlink_packet_generic::GenlMessage;
use std::convert::{TryFrom, TryInto};
use std::sync::Arc;
use tracing::{debug, error, info, warn};
use wlan_common::bss::BssDescription;
use wlan_common::channel::{Cbw, Channel};
use wlan_telemetry::{self, TelemetryEvent, TelemetrySender};
use {
    fidl_fuchsia_wlan_ieee80211 as fidl_ieee80211, fidl_fuchsia_wlan_sme as fidl_sme,
    fidl_fuchsia_wlan_wlanix as fidl_wlanix, fuchsia_async as fasync, fuchsia_inspect,
};

mod bss_scorer;
mod default_drop;
mod ifaces;
mod nl80211;
mod security;

use default_drop::{DefaultDrop, WithDefaultDrop};
use ifaces::{ClientIface, ConnectResult, IfaceManager, ScanEnd};
use nl80211::{Nl80211, Nl80211Attr, Nl80211BandAttr, Nl80211Cmd, Nl80211FrequencyAttr};

// TODO(https://fxbug.dev/368005870): Need to reconsider the consequences of using
// the same iface name, even when an iface is recreated.
const IFACE_NAME: &str = "wlan";

async fn handle_wifi_sta_iface_request(req: fidl_wlanix::WifiStaIfaceRequest) -> Result<(), Error> {
    match req {
        fidl_wlanix::WifiStaIfaceRequest::GetName { responder } => {
            info!("fidl_wlanix::WifiStaIfaceRequest::GetName");
            let response = fidl_wlanix::WifiStaIfaceGetNameResponse {
                iface_name: Some(IFACE_NAME.to_string()),
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

async fn serve_wifi_sta_iface(iface_id: u16, reqs: fidl_wlanix::WifiStaIfaceRequestStream) {
    reqs.for_each_concurrent(None, |req| async {
        match req {
            Ok(req) => {
                if let Err(e) = handle_wifi_sta_iface_request(req).await {
                    warn!("Failed to handle WifiStaIfaceRequest for iface {}: {}", iface_id, e);
                }
            }
            Err(e) => {
                error!("Wifi sta iface {} request stream failed: {}", iface_id, e);
            }
        }
    })
    .await;
}

async fn handle_wifi_chip_request<I: IfaceManager>(
    req: fidl_wlanix::WifiChipRequest,
    chip_id: u16,
    iface_manager: Arc<I>,
    telemetry_sender: TelemetrySender,
) -> Result<(), Error> {
    match req {
        fidl_wlanix::WifiChipRequest::CreateStaIface { payload, responder, .. } => {
            info!("fidl_wlanix::WifiChipRequest::CreateStaIface");
            match payload.iface {
                Some(iface) => {
                    let reqs = iface.into_stream();
                    let iface_id = iface_manager.create_client_iface(chip_id).await?;
                    telemetry_sender.send(TelemetryEvent::ClientIfaceCreated { iface_id });
                    responder.send(Ok(())).context("send CreateStaIface response")?;
                    serve_wifi_sta_iface(iface_id, reqs).await;
                }
                None => {
                    responder
                        .send(Err(zx::sys::ZX_ERR_INVALID_ARGS))
                        .context("send CreateStaIface response")?;
                }
            }
        }
        fidl_wlanix::WifiChipRequest::GetStaIfaceNames { responder } => {
            // TODO(b/323586414): Unit test once we actually support this.
            info!("fidl_wlanix::WifiChipRequest::GetStaIfaceNames");
            let ifaces = iface_manager.list_ifaces();
            // TODO(b/298030634): Serve actual interface names.
            let response = fidl_wlanix::WifiChipGetStaIfaceNamesResponse {
                iface_names: Some(vec![IFACE_NAME.to_string(); ifaces.len()]),
                ..Default::default()
            };
            responder.send(&response).context("send GetStaIfaceNames response")?;
        }
        fidl_wlanix::WifiChipRequest::GetStaIface { payload, responder } => {
            // TODO(b/323586414): Unit test once we actually support this.
            info!("fidl_wlanix::WifiChipRequest::GetStaIface");
            match payload.iface {
                Some(iface) => {
                    // TODO(b/298030634): Use the iface name to identify the correct iface here.
                    let reqs = iface.into_stream();
                    let ifaces = iface_manager.list_ifaces();
                    if ifaces.is_empty() {
                        warn!("No iface available for GetStaIface.");
                        responder
                            .send(Err(zx::sys::ZX_ERR_INVALID_ARGS))
                            .context("send GetStaIface response")?;
                    } else {
                        responder.send(Ok(())).context("send GetStaIface response")?;
                        serve_wifi_sta_iface(ifaces[0], reqs).await;
                    }
                }
                None => {
                    responder
                        .send(Err(zx::sys::ZX_ERR_INVALID_ARGS))
                        .context("send GetStaIface response")?;
                }
            }
        }
        fidl_wlanix::WifiChipRequest::RemoveStaIface { payload: _, responder, .. } => {
            info!("fidl_wlanix::WifiChipRequest::RemoveStaIface");
            // TODO(b/298030634): Use the iface name to identify the correct iface here.
            let ifaces = iface_manager.list_ifaces();
            if ifaces.is_empty() {
                warn!("No iface available to remove.");
                responder
                    .send(Err(zx::sys::ZX_ERR_INVALID_ARGS))
                    .context("send RemoveStaIface response")?;
            } else {
                info!("Removing iface {}", ifaces[0]);
                match iface_manager.destroy_iface(ifaces[0]).await {
                    Ok(()) => {
                        telemetry_sender
                            .send(TelemetryEvent::ClientIfaceDestroyed { iface_id: ifaces[0] });
                        responder.send(Ok(())).context("send RemoveStaIface response")?;
                    }
                    Err(e) => {
                        error!("Failed to remove iface: {}", e);
                        responder
                            .send(Err(zx::sys::ZX_ERR_NOT_SUPPORTED))
                            .context("send RemoveStaIface response")?;
                    }
                }
            }
        }
        fidl_wlanix::WifiChipRequest::SetCountryCode { payload: _, responder } => {
            info!("fidl_wlanix::WifiChipRequest::SetCountryCode");
            responder.send(Ok(())).context("send SetCountryCode response")?;
        }
        // TODO(https://fxbug.dev/366027488): GetAvailableModes is hardcoded.
        fidl_wlanix::WifiChipRequest::GetAvailableModes { responder } => {
            info!("fidl_wlanix::WifiChipRequest::GetAvailableModes");
            let response = fidl_wlanix::WifiChipGetAvailableModesResponse {
                chip_modes: Some(vec![fidl_wlanix::ChipMode {
                    id: Some(chip_id as u32),
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
        fidl_wlanix::WifiChipRequest::GetId { responder } => {
            info!("fidl_wlanix::WifiChipRequest::GetId");
            let response = fidl_wlanix::WifiChipGetIdResponse {
                id: Some(chip_id as u32),
                ..Default::default()
            };
            responder.send(&response).context("send GetId response")?;
        }
        // TODO(https://fxbug.dev/366028666): GetMode is hardcoded.
        fidl_wlanix::WifiChipRequest::GetMode { responder } => {
            info!("fidl_wlanix::WifiChipRequest::GetMode");
            let response =
                fidl_wlanix::WifiChipGetModeResponse { mode: Some(0), ..Default::default() };
            responder.send(&response).context("send GetMode response")?;
        }
        // TODO(https://fxbug.dev/366027491): GetCapabilities is hardcoded.
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

async fn serve_wifi_chip<I: IfaceManager>(
    chip_id: u16,
    reqs: fidl_wlanix::WifiChipRequestStream,
    iface_manager: Arc<I>,
    telemetry_sender: TelemetrySender,
) {
    reqs.for_each_concurrent(None, |req| async {
        match req {
            Ok(req) => {
                if let Err(e) = handle_wifi_chip_request(
                    req,
                    chip_id,
                    Arc::clone(&iface_manager),
                    telemetry_sender.clone(),
                )
                .await
                {
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

fn run_callbacks<T>(
    callback_fn: impl Fn(&T) -> Result<(), fidl::Error>,
    callbacks: &[T],
    ctx: &'static str,
) {
    let mut failed_callbacks = 0u32;
    for callback in callbacks {
        if let Err(_e) = callback_fn(callback) {
            failed_callbacks += 1;
        }
    }
    if failed_callbacks > 0 {
        warn!("Failed sending {} event to {} subscribers", ctx, failed_callbacks);
    }
}

#[derive(Default)]
struct WifiState {
    started: bool,
    callbacks: Vec<fidl_wlanix::WifiEventCallbackProxy>,
    scan_multicast_proxy: Option<fidl_wlanix::Nl80211MulticastProxy>,
    mlme_multicast_proxy: Option<fidl_wlanix::Nl80211MulticastProxy>,
}

async fn handle_wifi_request<I: IfaceManager>(
    req: fidl_wlanix::WifiRequest,
    state: Arc<Mutex<WifiState>>,
    iface_manager: Arc<I>,
    telemetry_sender: TelemetrySender,
) -> Result<(), Error> {
    match req {
        fidl_wlanix::WifiRequest::RegisterEventCallback { payload, .. } => {
            info!("fidl_wlanix::WifiRequest::RegisterEventCallback");
            if let Some(callback) = payload.callback {
                state.lock().callbacks.push(callback.into_proxy());
            }
        }
        fidl_wlanix::WifiRequest::Start { responder } => {
            info!("fidl_wlanix::WifiRequest::Start");
            let mut state = state.lock();
            state.started = true;
            responder.send(Ok(())).context("send Start response")?;
            run_callbacks(
                fidl_wlanix::WifiEventCallbackProxy::on_start,
                &state.callbacks[..],
                "OnStart",
            );

            let event = wlan_telemetry::ClientConnectionsToggleEvent::Enabled;
            telemetry_sender.send(TelemetryEvent::ClientConnectionsToggle { event });
        }
        fidl_wlanix::WifiRequest::Stop { responder } => {
            info!("fidl_wlanix::WifiRequest::Stop");
            // This should look like a reset of the chip. Tear down all ifaces.
            for iface in iface_manager.list_ifaces() {
                if let Err(e) = iface_manager.destroy_iface(iface).await {
                    error!(
                        "Failed to destroy iface {} in response to WifiRequest::Stop: {}",
                        iface, e
                    );
                }
            }
            let mut state = state.lock();
            state.started = false;
            run_callbacks(
                fidl_wlanix::WifiEventCallbackProxy::on_stop,
                &state.callbacks[..],
                "OnStop",
            );

            let event = wlan_telemetry::ClientConnectionsToggleEvent::Disabled;
            telemetry_sender.send(TelemetryEvent::ClientConnectionsToggle { event });
            responder.send(Ok(())).context("send Stop response")?;
        }
        fidl_wlanix::WifiRequest::GetState { responder } => {
            info!("fidl_wlanix::WifiRequest::GetState");
            let response = fidl_wlanix::WifiGetStateResponse {
                is_started: Some(state.lock().started),
                ..Default::default()
            };
            responder.send(&response).context("send GetState response")?;
        }
        fidl_wlanix::WifiRequest::GetChipIds { responder } => {
            info!("fidl_wlanix::WifiRequest::GetChipIds");
            let phy_ids = iface_manager.list_phys().await?;
            let response = fidl_wlanix::WifiGetChipIdsResponse {
                chip_ids: Some(phy_ids.into_iter().map(Into::into).collect()),
                ..Default::default()
            };
            responder.send(&response).context("send GetChipIds response")?;
        }
        fidl_wlanix::WifiRequest::GetChip { payload, responder } => {
            info!("fidl_wlanix::WifiRequest::GetChip - chip_id {:?}", payload.chip_id);
            match (payload.chip_id, payload.chip) {
                (Some(chip_id), Some(chip)) => {
                    let chip_stream = chip.into_stream();
                    match u16::try_from(chip_id) {
                        Ok(chip_id) => {
                            responder.send(Ok(())).context("send GetChip response")?;
                            serve_wifi_chip(chip_id, chip_stream, iface_manager, telemetry_sender)
                                .await;
                        }
                        Err(_e) => {
                            warn!("fidl_wlanix::WifiRequest::GetChip chip_id > u16::MAX");
                            responder
                                .send(Err(zx::sys::ZX_ERR_INVALID_ARGS))
                                .context("send GetChip response")?;
                        }
                    }
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

async fn serve_wifi<I: IfaceManager>(
    reqs: fidl_wlanix::WifiRequestStream,
    state: Arc<Mutex<WifiState>>,
    iface_manager: Arc<I>,
    telemetry_sender: TelemetrySender,
) {
    reqs.for_each_concurrent(None, |req| async {
        match req {
            Ok(req) => {
                if let Err(e) = handle_wifi_request(
                    req,
                    Arc::clone(&state),
                    Arc::clone(&iface_manager),
                    telemetry_sender.clone(),
                )
                .await
                {
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

#[derive(Default)]
struct SupplicantStaNetworkState {
    ssid: Option<Vec<u8>>,
    passphrase: Option<Vec<u8>>,
    bssid: Option<Bssid>,
}

struct SupplicantStaIfaceState {
    callbacks: Vec<fidl_wlanix::SupplicantStaIfaceCallbackProxy>,
}

struct ConnectionContext {
    stream: fidl_sme::ConnectTransactionEventStream,
    original_bss_desc: Box<BssDescription>,
    most_recent_connect_time: fasync::MonotonicInstant,
    current_rssi_dbm: i8,
    current_snr_db: i8,
    current_channel: Channel,
}

fn send_disconnect_event<C: ClientIface>(
    source: &fidl_sme::DisconnectSource,
    ctx: &ConnectionContext,
    sta_iface_state: &SupplicantStaIfaceState,
    wifi_state: &WifiState,
    iface: &C,
    iface_id: u16,
) {
    // We expect both an OnDisconnected and an OnStateChanged event.
    let (locally_generated, reason_code) = match source {
        fidl_sme::DisconnectSource::Ap(cause) => (false, cause.reason_code),
        fidl_sme::DisconnectSource::Mlme(cause) => (true, cause.reason_code),
        fidl_sme::DisconnectSource::User(user_reason) => {
            warn!("Disconnected by user with reason: {:?}", user_reason);
            (true, fidl_fuchsia_wlan_ieee80211::ReasonCode::UnspecifiedReason)
        }
    };
    let disconnected_event = fidl_wlanix::SupplicantStaIfaceCallbackOnDisconnectedRequest {
        bssid: Some(ctx.original_bss_desc.bssid.to_array()),
        locally_generated: Some(locally_generated),
        reason_code: Some(reason_code),
        ..Default::default()
    };
    run_callbacks(
        |callback_proxy| callback_proxy.on_disconnected(&disconnected_event),
        &sta_iface_state.callbacks[..],
        "on_disconnected",
    );
    let state_changed_event = fidl_wlanix::SupplicantStaIfaceCallbackOnStateChangedRequest {
        new_state: Some(fidl_wlanix::StaIfaceCallbackState::Disconnected),
        bssid: Some(ctx.original_bss_desc.bssid.to_array()),
        // TODO(b/316034688): do we need to keep track of actual id?
        id: Some(1),
        ssid: Some(ctx.original_bss_desc.ssid.to_vec()),
        ..Default::default()
    };
    run_callbacks(
        |callback_proxy| callback_proxy.on_state_changed(&state_changed_event),
        &sta_iface_state.callbacks[..],
        "on_state_changed",
    );
    // Also communicate the state change via nl80211.
    if let Some(proxy) = &wifi_state.mlme_multicast_proxy {
        let res = proxy.message(fidl_wlanix::Nl80211MulticastMessageRequest {
            message: Some(build_nl80211_message(
                Nl80211Cmd::Disconnect,
                vec![
                    Nl80211Attr::IfaceIndex(iface_id.into()),
                    Nl80211Attr::Mac(ctx.original_bss_desc.bssid.to_array()),
                ],
            )),
            ..Default::default()
        });
        if let Err(e) = res {
            error!("Failed to notify nl80211 mlme group of disconnect: {}", e);
        }
    }

    // Let iface know about disconnect so it clears any intermediate state
    iface.on_disconnect(source);
}

async fn handle_client_connect_transactions<C: ClientIface>(
    mut ctx: ConnectionContext,
    sta_iface_state: Arc<Mutex<SupplicantStaIfaceState>>,
    wifi_state: Arc<Mutex<WifiState>>,
    telemetry_sender: TelemetrySender,
    iface: Arc<C>,
    iface_id: u16,
) {
    // If we receive a disconnect but attempt to reconnect, we will deliver the
    // disconnect event later if the reconnect attempt fails.
    let mut disconnect_with_ongoing_reconnect: Option<fidl_sme::DisconnectSource> = None;

    loop {
        // The transaction stream will exit cleanly when the connection has fully terminated.
        let req = match ctx.stream.next().await {
            Some(req) => req,
            None => return,
        };
        match req {
            Ok(fidl_sme::ConnectTransactionEvent::OnConnectResult { result }) => {
                match (disconnect_with_ongoing_reconnect.as_ref(), result.is_reconnect) {
                    (Some(info), true) => {
                        if result.code == fidl_fuchsia_wlan_ieee80211::StatusCode::Success {
                            ctx.most_recent_connect_time = fasync::MonotonicInstant::now();
                            info!("Successfully reconnected after disconnect");
                        } else {
                            send_disconnect_event(
                                info,
                                &ctx,
                                &sta_iface_state.lock(),
                                &wifi_state.lock(),
                                &*iface,
                                iface_id,
                            );
                        }
                        disconnect_with_ongoing_reconnect = None;
                    }
                    (Some(_), false) => {
                        error!("Received non-reconnect connect result while reconnecting")
                    }

                    (None, true) => error!("Received reconnect result while not reconnecting"),
                    (None, false) => error!(
                        "Received unexpected connect result after connection already established."
                    ),
                }
            }
            Ok(fidl_sme::ConnectTransactionEvent::OnRoamResult { result }) => {
                match result.status_code {
                    fidl_fuchsia_wlan_ieee80211::StatusCode::Success => {
                        info!("Connection roamed successfully");
                        // TODO(https://fxbug.dev/352557875): Plumb SME RoamResult into wlanix.
                    }
                    _ => {
                        info!("Connection failed to roam");
                        let source = match result.disconnect_info {
                            Some(disconnect_info) => disconnect_info.disconnect_source,
                            None => {
                                error!("RoamResult indicates failure, but disconnect source is missing");
                                fidl_sme::DisconnectSource::Mlme(fidl_sme::DisconnectCause {
                                    mlme_event_name:
                                        fidl_sme::DisconnectMlmeEventName::RoamResultIndication,
                                    reason_code:
                                        fidl_fuchsia_wlan_ieee80211::ReasonCode::UnspecifiedReason,
                                })
                            }
                        };
                        send_disconnect_event(
                            &source,
                            &ctx,
                            &sta_iface_state.lock(),
                            &wifi_state.lock(),
                            &*iface,
                            iface_id,
                        );
                    }
                }
            }
            Ok(fidl_sme::ConnectTransactionEvent::OnDisconnect { info }) => {
                let connected_duration =
                    fasync::MonotonicInstant::now() - ctx.most_recent_connect_time;
                telemetry_sender.send(TelemetryEvent::Disconnect {
                    info: wlan_telemetry::DisconnectInfo {
                        connected_duration: connected_duration,
                        is_sme_reconnecting: info.is_sme_reconnecting,
                        disconnect_source: info.disconnect_source,
                        original_bss_desc: ctx.original_bss_desc.clone(),
                        current_rssi_dbm: ctx.current_rssi_dbm,
                        current_snr_db: ctx.current_snr_db,
                        current_channel: ctx.current_channel,
                    },
                });
                if info.is_sme_reconnecting {
                    info!("Connection interrupted, awaiting reconnect: {:?}", info);
                    disconnect_with_ongoing_reconnect = Some(info.disconnect_source);
                } else {
                    info!(
                        "Connection terminated by disconnect, lasted {:?}: {:?}",
                        connected_duration, info
                    );
                    send_disconnect_event(
                        &info.disconnect_source,
                        &ctx,
                        &sta_iface_state.lock(),
                        &wifi_state.lock(),
                        &*iface,
                        iface_id,
                    );
                    disconnect_with_ongoing_reconnect = None;
                }
            }
            Ok(fidl_sme::ConnectTransactionEvent::OnSignalReport { ind }) => {
                ctx.current_rssi_dbm = ind.rssi_dbm;
                ctx.current_snr_db = ind.snr_db;
                iface.on_signal_report(ind);
            }
            Ok(fidl_sme::ConnectTransactionEvent::OnChannelSwitched { info }) => {
                ctx.current_channel.primary = info.new_channel;
                info!("Connection switching to channel {}", info.new_channel);
            }
            Err(e) => {
                error!("Error on connect transaction event stream: {}", e);
            }
        }
    }
}

async fn handle_supplicant_sta_network_request<C: ClientIface>(
    telemetry_sender: TelemetrySender,
    req: fidl_wlanix::SupplicantStaNetworkRequest,
    sta_network_state: Arc<Mutex<SupplicantStaNetworkState>>,
    sta_iface_state: Arc<Mutex<SupplicantStaIfaceState>>,
    state: Arc<Mutex<WifiState>>,
    iface: Arc<C>,
    iface_id: u16,
) -> Result<(), Error> {
    match req {
        fidl_wlanix::SupplicantStaNetworkRequest::SetBssid { payload, .. } => {
            info!("fidl_wlanix::SupplicantStaNetworkRequest::SetBssid");
            if let Some(bssid) = payload.bssid {
                sta_network_state.lock().bssid.replace(Bssid::from(bssid));
            }
        }
        fidl_wlanix::SupplicantStaNetworkRequest::ClearBssid { .. } => {
            info!("fidl_wlanix::SupplicantStaNetworkRequest::ClearBssid");
            sta_network_state.lock().bssid.take();
        }
        fidl_wlanix::SupplicantStaNetworkRequest::SetSsid { payload, .. } => {
            info!("fidl_wlanix::SupplicantStaNetworkRequest::SetSsid");
            if let Some(ssid) = payload.ssid {
                sta_network_state.lock().ssid.replace(ssid);
            }
        }
        fidl_wlanix::SupplicantStaNetworkRequest::SetPskPassphrase { payload, .. } => {
            info!("fidl_wlanix::SupplicantStaNetworkRequest::SetPskPassphrase");
            if let Some(passphrase) = payload.passphrase {
                sta_network_state.lock().passphrase.replace(passphrase);
            }
        }
        fidl_wlanix::SupplicantStaNetworkRequest::Select { responder } => {
            info!("fidl_wlanix::SupplicantStaNetworkRequest::Select");
            let (ssid, passphrase, bssid) = {
                let state = sta_network_state.lock();
                (state.ssid.clone(), state.passphrase.clone(), state.bssid.clone())
            };
            let (result, connection_ctx) = match ssid {
                Some(ssid) => match iface.connect_to_network(&ssid[..], passphrase, bssid).await {
                    Ok(ConnectResult::Success(connected)) => {
                        info!("Connected to requested network");
                        telemetry_sender.send(TelemetryEvent::ConnectResult {
                            result: fidl_ieee80211::StatusCode::Success,
                            bss: connected.bss.clone(),
                        });
                        let event = fidl_wlanix::SupplicantStaIfaceCallbackOnStateChangedRequest {
                            new_state: Some(fidl_wlanix::StaIfaceCallbackState::Completed),
                            bssid: Some(connected.bss.bssid.to_array()),
                            // TODO(b/316034688): do we need to keep track of actual id?
                            id: Some(1),
                            ssid: Some(connected.bss.ssid.clone().into()),
                            ..Default::default()
                        };
                        run_callbacks(
                            |callback_proxy| callback_proxy.on_state_changed(&event),
                            &sta_iface_state.lock().callbacks[..],
                            "on_state_changed",
                        );
                        (
                            Ok(()),
                            Some(ConnectionContext {
                                stream: connected.transaction_stream,
                                original_bss_desc: connected.bss.clone(),
                                most_recent_connect_time: fasync::MonotonicInstant::now(),
                                current_rssi_dbm: connected.bss.rssi_dbm,
                                current_snr_db: connected.bss.snr_db,
                                current_channel: connected.bss.channel.clone(),
                            }),
                        )
                    }
                    Ok(ConnectResult::Fail(fail)) => {
                        warn!("Connection failed with status code: {:?}", fail.status_code);
                        telemetry_sender.send(TelemetryEvent::ConnectResult {
                            result: fail.status_code,
                            bss: fail.bss.clone(),
                        });
                        let event =
                            fidl_wlanix::SupplicantStaIfaceCallbackOnAssociationRejectedRequest {
                                ssid: Some(fail.bss.ssid.to_vec()),
                                bssid: Some(fail.bss.bssid.to_array()),
                                status_code: Some(fail.status_code),
                                timed_out: Some(fail.timed_out),
                                ..Default::default()
                            };
                        run_callbacks(
                            |callback_proxy| callback_proxy.on_association_rejected(&event),
                            &sta_iface_state.lock().callbacks[..],
                            "on_association_rejected",
                        );
                        (Err(zx::sys::ZX_ERR_INTERNAL), None)
                    }
                    Err(e) => {
                        error!("Error while connecting to network: {}", e);
                        (Err(zx::sys::ZX_ERR_INTERNAL), None)
                    }
                },
                None => {
                    warn!("No SSID set. fidl_wlanix::SupplicantStaNetworkRequest::Select ignored");
                    (Err(zx::sys::ZX_ERR_BAD_STATE), None)
                }
            };
            responder.send(result).context("send Select response")?;
            if let Some(proxy) = state.lock().mlme_multicast_proxy.as_ref() {
                let status_code = match result {
                    Ok(()) => 0,
                    Err(_) => 1,
                };
                proxy
                    .message(fidl_wlanix::Nl80211MulticastMessageRequest {
                        message: Some(build_nl80211_message(
                            Nl80211Cmd::Connect,
                            vec![
                                Nl80211Attr::IfaceIndex(iface_id.into()),
                                // TODO(b/316035583): Do we need to send the actual station MAC?
                                Nl80211Attr::Mac([0u8; 6]),
                                Nl80211Attr::StatusCode(status_code),
                            ],
                        )),
                        ..Default::default()
                    })
                    .context("Failed to send nl80211 Connect")?;
            }
            if let Some(ctx) = connection_ctx {
                // Continue to process connection updates until the connection terminates.
                // We can do this here because calls to this function are all executed
                // concurrently, so it doesn't block other requests.
                handle_client_connect_transactions(
                    ctx,
                    Arc::clone(&sta_iface_state),
                    Arc::clone(&state),
                    telemetry_sender,
                    Arc::clone(&iface),
                    iface_id,
                )
                .await;
            }
        }
        fidl_wlanix::SupplicantStaNetworkRequest::_UnknownMethod { ordinal, .. } => {
            warn!("Unknown SupplicantStaNetworkRequest ordinal: {}", ordinal);
        }
    }
    Ok(())
}

async fn serve_supplicant_sta_network<C: ClientIface>(
    telemetry_sender: TelemetrySender,
    reqs: fidl_wlanix::SupplicantStaNetworkRequestStream,
    sta_iface_state: Arc<Mutex<SupplicantStaIfaceState>>,
    state: Arc<Mutex<WifiState>>,
    iface: Arc<C>,
    iface_id: u16,
) {
    let sta_network_state = Arc::new(Mutex::new(SupplicantStaNetworkState::default()));
    reqs.for_each_concurrent(None, |req| async {
        match req {
            Ok(req) => {
                if let Err(e) = handle_supplicant_sta_network_request(
                    telemetry_sender.clone(),
                    req,
                    Arc::clone(&sta_network_state),
                    Arc::clone(&sta_iface_state),
                    Arc::clone(&state),
                    Arc::clone(&iface),
                    iface_id,
                )
                .await
                {
                    warn!("Failed to handle SupplicantStaNetwork: {}", e);
                }
            }
            Err(e) => {
                error!("SupplicantStaNetwork request stream failed: {}", e);
            }
        }
    })
    .await;
}

async fn handle_supplicant_sta_iface_request<C: ClientIface>(
    telemetry_sender: TelemetrySender,
    req: fidl_wlanix::SupplicantStaIfaceRequest,
    sta_iface_state: Arc<Mutex<SupplicantStaIfaceState>>,
    state: Arc<Mutex<WifiState>>,
    iface: Arc<C>,
    iface_id: u16,
) -> Result<(), Error> {
    match req {
        fidl_wlanix::SupplicantStaIfaceRequest::RegisterCallback { payload, .. } => {
            info!("fidl_wlanix::SupplicantStaIfaceRequest::RegisterCallback");
            if let Some(callback) = payload.callback {
                sta_iface_state.lock().callbacks.push(callback.into_proxy());
            } else {
                warn!("Empty callback field in received RegisterCallback request.")
            }
        }
        fidl_wlanix::SupplicantStaIfaceRequest::AddNetwork { payload, .. } => {
            info!("fidl_wlanix::SupplicantStaIfaceRequest::AddNetwork");
            if let Some(supplicant_sta_network) = payload.network {
                let supplicant_sta_network_stream = supplicant_sta_network.into_stream();
                // TODO(https://fxbug.dev/316035436): Should we return NetworkAdded event?
                serve_supplicant_sta_network(
                    telemetry_sender,
                    supplicant_sta_network_stream,
                    sta_iface_state,
                    state,
                    iface,
                    iface_id,
                )
                .await;
            }
        }
        fidl_wlanix::SupplicantStaIfaceRequest::Disconnect { responder } => {
            info!("fidl_wlanix::SupplicantStaIfaceRequest::Disconnect");
            if let Err(e) = iface.disconnect().await {
                warn!("iface.disconnect() error: {}", e);
            }
            if let Err(e) = responder.send() {
                warn!("Failed to send disconnect response: {}", e);
            }
        }
        fidl_wlanix::SupplicantStaIfaceRequest::SetPowerSave { payload, responder } => {
            info!("fidl_wlanix::SupplicantStaIfaceRequest::SetPowerSave");
            match payload.enable {
                Some(enable) => match iface.set_power_save_mode(enable).await {
                    Ok(()) => info!("Set power save mode to {}", enable),
                    Err(e) => warn!("Failed to set power save mode: {:?}", e),
                },
                None => error!("Got SetPowerSave without a payload"),
            }
            if let Err(e) = responder.send() {
                warn!("Failed to send disconnect response: {}", e);
            }
        }
        fidl_wlanix::SupplicantStaIfaceRequest::SetSuspendModeEnabled { payload, responder } => {
            info!("fidl_wlanix::SupplicantStaIfaceRequest::SetSuspendModeEnabled");
            match payload.enable {
                Some(enable) => match iface.set_suspend_mode(enable).await {
                    Ok(()) => info!("Set suspend mode to {}", enable),
                    Err(e) => warn!("Failed to set suspend mode: {:?}", e),
                },
                None => error!("Got SetSuspendModeEnabled without a payload"),
            }
            if let Err(e) = responder.send() {
                warn!("Failed to send disconnect response: {}", e);
            }
        }
        fidl_wlanix::SupplicantStaIfaceRequest::SetStaCountryCode { payload: _, responder } => {
            info!("fidl_wlanix::SupplicantStaIfaceRequest::SetStaCountryCode");
            responder.send(Ok(())).context("send SetStaCountryCode response")?;
        }
        fidl_wlanix::SupplicantStaIfaceRequest::_UnknownMethod { ordinal, .. } => {
            warn!("Unknown SupplicantStaIfaceRequest ordinal: {}", ordinal);
        }
    }
    Ok(())
}

async fn serve_supplicant_sta_iface<C: ClientIface>(
    telemetry_sender: TelemetrySender,
    reqs: fidl_wlanix::SupplicantStaIfaceRequestStream,
    state: Arc<Mutex<WifiState>>,
    iface: Arc<C>,
    iface_id: u16,
) {
    let sta_iface_state = Arc::new(Mutex::new(SupplicantStaIfaceState { callbacks: vec![] }));
    reqs.for_each_concurrent(None, |req| async {
        match req {
            Ok(req) => {
                if let Err(e) = handle_supplicant_sta_iface_request(
                    telemetry_sender.clone(),
                    req,
                    Arc::clone(&sta_iface_state),
                    Arc::clone(&state),
                    Arc::clone(&iface),
                    iface_id,
                )
                .await
                {
                    warn!("Failed to handle SupplicantRequest: {}", e);
                }
            }
            Err(e) => {
                error!("SupplicantStaIface request stream failed: {}", e);
            }
        }
    })
    .await;
}

async fn handle_supplicant_request<I: IfaceManager>(
    req: fidl_wlanix::SupplicantRequest,
    iface_manager: Arc<I>,
    telemetry_sender: TelemetrySender,
    state: Arc<Mutex<WifiState>>,
) -> Result<(), Error> {
    match req {
        fidl_wlanix::SupplicantRequest::AddStaInterface { payload, .. } => {
            info!("fidl_wlanix::SupplicantRequest::AddStaInterface");
            // TODO(b/299349496): Check that the iface name matches the request.
            if let Some(supplicant_sta_iface) = payload.iface {
                let ifaces = iface_manager.list_ifaces();
                if ifaces.is_empty() {
                    bail!("AddStaInterface but no interfaces exist.");
                } else {
                    let client_iface = iface_manager.get_client_iface(ifaces[0]).await?;
                    let supplicant_sta_iface_stream = supplicant_sta_iface.into_stream();
                    serve_supplicant_sta_iface(
                        telemetry_sender,
                        supplicant_sta_iface_stream,
                        state,
                        client_iface,
                        ifaces[0],
                    )
                    .await;
                }
            }
        }
        fidl_wlanix::SupplicantRequest::_UnknownMethod { ordinal, .. } => {
            warn!("Unknown SupplicantRequest ordinal: {}", ordinal);
        }
    }
    Ok(())
}

async fn serve_supplicant<I: IfaceManager>(
    reqs: fidl_wlanix::SupplicantRequestStream,
    iface_manager: Arc<I>,
    telemetry_sender: TelemetrySender,
    state: Arc<Mutex<WifiState>>,
) {
    reqs.for_each_concurrent(None, |req| async {
        match req {
            Ok(req) => {
                if let Err(e) = handle_supplicant_request(
                    req,
                    Arc::clone(&iface_manager),
                    telemetry_sender.clone(),
                    Arc::clone(&state),
                )
                .await
                {
                    warn!("Failed to handle SupplicantRequest: {}", e);
                }
            }
            Err(e) => {
                error!("Supplicant request stream failed: {}", e);
            }
        }
    })
    .await;
}

fn nl80211_message_resp(
    responses: Vec<fidl_wlanix::Nl80211Message>,
) -> fidl_wlanix::Nl80211MessageResponse {
    fidl_wlanix::Nl80211MessageResponse { responses: Some(responses), ..Default::default() }
}

fn build_nl80211_message(cmd: Nl80211Cmd, attrs: Vec<Nl80211Attr>) -> fidl_wlanix::Nl80211Message {
    let resp = GenlMessage::from_payload(Nl80211 { cmd, attrs });
    let mut buffer = vec![0u8; resp.buffer_len()];
    resp.serialize(&mut buffer);
    fidl_wlanix::Nl80211Message {
        message_type: Some(fidl_wlanix::Nl80211MessageType::Message),
        payload: Some(buffer),
        ..Default::default()
    }
}

fn build_nl80211_ack() -> fidl_wlanix::Nl80211Message {
    fidl_wlanix::Nl80211Message {
        message_type: Some(fidl_wlanix::Nl80211MessageType::Ack),
        payload: None,
        ..Default::default()
    }
}

fn build_nl80211_err() -> fidl_wlanix::Nl80211Message {
    // TODO(https://fxbug.dev/369154198): This should probably contain an error value.
    fidl_wlanix::Nl80211Message {
        message_type: Some(fidl_wlanix::Nl80211MessageType::Error),
        payload: None,
        ..Default::default()
    }
}

fn build_nl80211_done() -> fidl_wlanix::Nl80211Message {
    fidl_wlanix::Nl80211Message {
        message_type: Some(fidl_wlanix::Nl80211MessageType::Done),
        payload: None,
        ..Default::default()
    }
}

fn get_supported_frequencies() -> Vec<Vec<Nl80211FrequencyAttr>> {
    // TODO(b/316037008): Reevaluate this list later. This does not reflect
    // actual support. We should instead get supported frequencies from the phy.
    #[rustfmt::skip]
    let channels = vec![
        // 2.4 GHz
        1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11,
        // 5 GHz
        36, 40, 44, 48, 52, 56, 60, 64,
        100, 104, 108, 112, 116, 120, 124, 128, 132, 136, 140, 144,
        149, 153, 157, 161, 165,
    ];
    channels
        .into_iter()
        .map(|channel_idx| {
            // We report the frequency of the beacon, which is always 20MHz on the primary channel.
            let freq = Channel::new(channel_idx, Cbw::Cbw20).get_center_freq().unwrap();
            vec![Nl80211FrequencyAttr::Frequency(freq.into())]
        })
        .collect()
}

async fn handle_nl80211_message<I: IfaceManager>(
    netlink_message: fidl_wlanix::Nl80211Message,
    responder: WithDefaultDrop<fidl_wlanix::Nl80211MessageResponder>,
    state: Arc<Mutex<WifiState>>,
    iface_manager: Arc<I>,
) -> Result<(), Error> {
    let payload = match netlink_message {
        fidl_wlanix::Nl80211Message {
            message_type: Some(fidl_wlanix::Nl80211MessageType::Message),
            payload: Some(p),
            ..
        } => p,
        _ => return Ok(()),
    };
    let deserialized = GenlMessage::<Nl80211>::deserialize(&NetlinkHeader::default(), &payload[..]);
    let Ok(message) = deserialized else {
        responder
            .take()
            .send(Err(zx::sys::ZX_ERR_INTERNAL))
            .context("sending error status on failing to parse nl80211 message")?;
        bail!("Failed to parse nl80211 message: {}", deserialized.unwrap_err())
    };
    match message.payload.cmd {
        Nl80211Cmd::GetWiphy => {
            info!("Nl80211Cmd::GetWiphy");
            let phys = iface_manager.list_phys().await?;
            let mut resp = vec![];
            for phy_id in phys {
                resp.push(build_nl80211_message(
                    Nl80211Cmd::NewWiphy,
                    vec![
                        // Phy ID
                        Nl80211Attr::Wiphy(phy_id as u32),
                        // Supported bands
                        Nl80211Attr::WiphyBands(vec![vec![Nl80211BandAttr::Frequencies(
                            get_supported_frequencies(),
                        )]]),
                        // Scan capabilities
                        Nl80211Attr::MaxScanSsids(32),
                        Nl80211Attr::MaxScheduledScanSsids(32),
                        Nl80211Attr::MaxMatchSets(32),
                        // Feature flags
                        Nl80211Attr::FeatureFlags(0),
                        Nl80211Attr::ExtendedFeatures(vec![]),
                    ],
                ));
            }
            responder
                .take()
                .send(Ok(nl80211_message_resp(resp)))
                .context("Failed to send NewWiphy")?;
        }
        Nl80211Cmd::GetInterface => {
            info!("Nl80211Cmd::GetInterface");
            let ifaces = iface_manager.list_ifaces();
            let mut resp = vec![];
            for iface in ifaces {
                let iface_info = iface_manager.query_iface(iface).await?;
                resp.push(build_nl80211_message(
                    Nl80211Cmd::NewInterface,
                    vec![
                        Nl80211Attr::IfaceIndex(iface.into()),
                        Nl80211Attr::IfaceName(IFACE_NAME.to_string()),
                        Nl80211Attr::Mac(iface_info.sta_addr),
                    ],
                ));
            }
            resp.push(build_nl80211_done());
            responder
                .take()
                .send(Ok(nl80211_message_resp(resp)))
                .context("Failed to send scan results")?;
        }
        Nl80211Cmd::GetStation => {
            debug!("Nl80211Cmd::GetStation");
            use crate::nl80211::Nl80211StaInfoAttr;
            // GetStation also has a MAC address attribute. We don't check whether it
            // matches the connected network BSSID and simply assume that it does.
            match get_client_iface_and_id(&message.payload.attrs[..], &iface_manager).await {
                Ok((client_iface, _)) => {
                    const INVALID_RSSI: i8 = -127;
                    let rssi = client_iface.get_connected_network_rssi().unwrap_or(INVALID_RSSI);
                    responder
                        .take()
                        .send(Ok(nl80211_message_resp(vec![build_nl80211_message(
                            Nl80211Cmd::NewStation,
                            vec![Nl80211Attr::StaInfo(vec![
                                // TX packet counters don't seem to be used, so just set
                                // them to 0
                                Nl80211StaInfoAttr::TxPackets(0),
                                Nl80211StaInfoAttr::TxFailed(0),
                                Nl80211StaInfoAttr::Signal(rssi),
                                // NL80211_STA_INFO_TX_BITRATE and NL80211_STA_INFO_RX_BITRATE
                                // can also be included. We don't have those information, so
                                // we are not including them here.
                            ])],
                        )])))
                        .context("Failed to send GetStation")?;
                }
                Err(e) => {
                    responder.take().send(Err(e)).context("sending error status for GetStation")?;
                    bail!("Could not get a client iface for GetStation")
                }
            }
        }
        Nl80211Cmd::GetProtocolFeatures => {
            info!("Nl80211Cmd::GetProtocolFeatures");
            responder
                .take()
                .send(Ok(nl80211_message_resp(vec![build_nl80211_message(
                    Nl80211Cmd::GetProtocolFeatures,
                    vec![Nl80211Attr::ProtocolFeatures(0)],
                )])))
                .context("Failed to send GetProtocolFeatures")?;
        }
        Nl80211Cmd::TriggerScan => {
            info!("Nl80211Cmd::TriggerScan");
            match get_client_iface_and_id(&message.payload.attrs[..], &iface_manager).await {
                Ok((client_iface, iface_id)) => {
                    responder
                        .take()
                        .send(Ok(nl80211_message_resp(vec![build_nl80211_ack()])))
                        .context("Failed to ack TriggerScan")?;
                    match client_iface.trigger_scan().await {
                        Ok(ScanEnd::Complete) => {
                            info!("Passive scan completed successfully");
                            if let Some(proxy) = state.lock().scan_multicast_proxy.as_ref() {
                                proxy
                                    .message(fidl_wlanix::Nl80211MulticastMessageRequest {
                                        message: Some(build_nl80211_message(
                                            Nl80211Cmd::NewScanResults,
                                            vec![Nl80211Attr::IfaceIndex(iface_id)],
                                        )),
                                        ..Default::default()
                                    })
                                    .context("Failed to send NewScanResults")?;
                            }
                        }
                        Ok(ScanEnd::Cancelled) => {
                            info!("Passive scan terminated");
                            if let Some(proxy) = state.lock().scan_multicast_proxy.as_ref() {
                                proxy
                                    .message(fidl_wlanix::Nl80211MulticastMessageRequest {
                                        message: Some(build_nl80211_message(
                                            Nl80211Cmd::ScanAborted,
                                            vec![Nl80211Attr::IfaceIndex(iface_id)],
                                        )),
                                        ..Default::default()
                                    })
                                    .context("Failed to send ScanAborted after scan cancelled")?;
                            }
                        }
                        Err(e) => {
                            error!("Failed to run passive scan: {:?}", e);
                            if let Some(proxy) = state.lock().scan_multicast_proxy.as_ref() {
                                proxy
                                    .message(fidl_wlanix::Nl80211MulticastMessageRequest {
                                        message: Some(build_nl80211_message(
                                            Nl80211Cmd::ScanAborted,
                                            vec![Nl80211Attr::IfaceIndex(iface_id)],
                                        )),
                                        ..Default::default()
                                    })
                                    .context("Failed to send ScanAborted after scan error")?;
                            }
                        }
                    }
                }
                Err(e) => {
                    responder
                        .take()
                        .send(Err(e))
                        .context("sending error status for TriggerScan")?;
                    bail!("Could not get a client iface for TriggerScan")
                }
            }
        }
        Nl80211Cmd::AbortScan => {
            info!("Nl80211Cmd::AbortScan");
            match get_client_iface_and_id(&message.payload.attrs[..], &iface_manager).await {
                Ok((client_iface, _)) => match client_iface.abort_scan().await {
                    Ok(()) => {
                        info!("Aborted scan successfully");
                        responder
                            .take()
                            .send(Ok(nl80211_message_resp(vec![build_nl80211_ack()])))
                            .context("Failed to ack AbortScan")?;
                    }
                    Err(e) => {
                        error!("Failed to abort scan: {:?}", e);
                        responder
                            .take()
                            .send(Ok(nl80211_message_resp(vec![build_nl80211_err()])))
                            .context("Failed to ack AbortScan")?;
                    }
                },
                Err(e) => {
                    responder.take().send(Err(e)).context("sending error status for AbortScan")?;
                    bail!("Could not get a client iface for AbortScan")
                }
            }
        }
        Nl80211Cmd::GetScan => {
            info!("Nl80211Cmd::GetScan");
            match get_client_iface_and_id(&message.payload.attrs[..], &iface_manager).await {
                Ok((client_iface, iface_id)) => {
                    let results = client_iface.get_last_scan_results();
                    info!("Processing {} scan results", results.len());
                    let mut resp = vec![];
                    for result in results {
                        resp.push(build_nl80211_message(
                            Nl80211Cmd::NewScanResults,
                            vec![Nl80211Attr::IfaceIndex(iface_id), convert_scan_result(result)],
                        ));
                    }
                    resp.push(build_nl80211_done());
                    responder
                        .take()
                        .send(Ok(nl80211_message_resp(resp)))
                        .context("Failed to send scan results")?;
                }
                Err(e) => {
                    responder.take().send(Err(e)).context("sending error status for GetScan")?;
                    bail!("Could not get a client iface for GetScan");
                }
            }
        }
        Nl80211Cmd::GetReg => {
            info!("Nl80211Cmd::GetReg");
            match find_phy_id(&message.payload.attrs[..]) {
                Some(phy_id) => match iface_manager.get_country(phy_id.try_into()?).await {
                    Ok(country) => {
                        let resp = build_nl80211_message(
                            Nl80211Cmd::GetReg,
                            vec![Nl80211Attr::RegulatoryRegionAlpha2(country)],
                        );
                        responder
                            .take()
                            .send(Ok(nl80211_message_resp(vec![resp])))
                            .context("Failed to respond to GetReg")?;
                    }
                    Err(e) => {
                        error!("Failed to get regulatory region from phy: {:?}", e);
                        responder
                            .take()
                            .send(Err(zx::sys::ZX_ERR_INTERNAL))
                            .context("Failed to respond to GetReg with error")?;
                    }
                },
                None => {
                    responder
                        .take()
                        .send(Err(zx::sys::ZX_ERR_INVALID_ARGS))
                        .context("sending error status due to missing iface id on GetReg")?;
                    bail!("GetReg did not include a phy id");
                }
            }
        }
        _ => {
            warn!("Dropping nl80211 message: {:?}", message);
            responder
                .take()
                .send(Ok(nl80211_message_resp(vec![])))
                .context("Failed to respond to unhandled message")?;
        }
    }
    Ok(())
}

impl DefaultDrop for fidl_wlanix::Nl80211MessageResponder {
    fn default_drop(self) {
        error!("Dropped Nl80211MessageResponder without responding.");
        if let Err(e) = self.send(Err(zx::sys::ZX_ERR_INTERNAL)) {
            error!("Failed to send internal error response: {}", e);
        }
    }
}

fn convert_scan_result(result: fidl_sme::ScanResult) -> Nl80211Attr {
    use crate::nl80211::{ChainSignalAttr, Nl80211BssAttr};
    let channel = Channel::new(result.bss_description.channel.primary, Cbw::Cbw20);
    let center_freq = channel.get_center_freq().expect("Failed to get center freq").into();
    Nl80211Attr::Bss(vec![
        Nl80211BssAttr::Bssid(result.bss_description.bssid),
        Nl80211BssAttr::Frequency(center_freq),
        Nl80211BssAttr::InformationElement(result.bss_description.ies),
        Nl80211BssAttr::LastSeenBoottime(fasync::MonotonicInstant::now().into_nanos() as u64),
        Nl80211BssAttr::SignalMbm(result.bss_description.rssi_dbm as i32 * 100),
        Nl80211BssAttr::Capability(result.bss_description.capability_info),
        Nl80211BssAttr::Status(0),
        // TODO(b/316038074): Determine whether we should provide real chain signals.
        Nl80211BssAttr::ChainSignal(vec![ChainSignalAttr {
            id: 0,
            rssi: result.bss_description.rssi_dbm.into(),
        }]),
    ])
}

fn find_phy_id(attrs: &[Nl80211Attr]) -> Option<u32> {
    attrs
        .iter()
        .filter_map(|attr| match attr {
            Nl80211Attr::Wiphy(idx) => Some(idx),
            _ => None,
        })
        .cloned()
        .next()
}

fn find_iface_id(attrs: &[Nl80211Attr]) -> Option<u32> {
    attrs
        .iter()
        .filter_map(|attr| match attr {
            Nl80211Attr::IfaceIndex(idx) => Some(idx),
            _ => None,
        })
        .cloned()
        .next()
}

async fn get_client_iface_and_id<I: IfaceManager>(
    attrs: &[Nl80211Attr],
    iface_manager: &Arc<I>,
) -> Result<(Arc<I::Client>, u32), i32> {
    match find_iface_id(attrs) {
        Some(iface_id) => {
            let iface_id_u16 = u16::try_from(iface_id).map_err(|_| zx::sys::ZX_ERR_BAD_STATE)?;
            iface_manager
                .get_client_iface(iface_id_u16)
                .await
                .map(|iface| (iface, iface_id))
                .map_err(|_| zx::sys::ZX_ERR_NOT_FOUND)
        }
        None => Err(zx::sys::ZX_ERR_INVALID_ARGS),
    }
}

async fn serve_nl80211<I: IfaceManager>(
    mut reqs: fidl_wlanix::Nl80211RequestStream,
    state: Arc<Mutex<WifiState>>,
    iface_manager: Arc<I>,
) {
    loop {
        let Some(req) = reqs.next().await else {
            warn!("Nl80211 stream terminated. Should only happen during shutdown.");
            return;
        };
        match req {
            Ok(fidl_wlanix::Nl80211Request::Message { payload, responder, .. }) => {
                if let Some(message) = payload.message {
                    if let Err(e) = handle_nl80211_message(
                        message,
                        WithDefaultDrop::new(responder),
                        Arc::clone(&state),
                        Arc::clone(&iface_manager),
                    )
                    .await
                    {
                        error!("Failed to handle Nl80211 message: {}", e);
                    }
                }
            }
            Ok(fidl_wlanix::Nl80211Request::GetMulticast { payload, .. }) => {
                if let Some(multicast) = payload.multicast {
                    if payload.group == Some("scan".to_string()) {
                        state.lock().scan_multicast_proxy.replace(multicast.into_proxy());
                    } else if payload.group == Some("mlme".to_string()) {
                        state.lock().mlme_multicast_proxy.replace(multicast.into_proxy());
                    } else {
                        warn!(
                            "Dropping channel for unsupported multicast group {:?}",
                            payload.group
                        );
                    }
                }
            }
            Ok(fidl_wlanix::Nl80211Request::_UnknownMethod { ordinal, .. }) => {
                warn!("Unknown Nl80211Request ordinal: {}", ordinal);
            }
            Err(e) => {
                error!("Nl80211 request stream failed: {}", e);
                return;
            }
        }
    }
}

async fn handle_wlanix_request<I: IfaceManager>(
    req: fidl_wlanix::WlanixRequest,
    state: Arc<Mutex<WifiState>>,
    iface_manager: Arc<I>,
    telemetry_sender: TelemetrySender,
) -> Result<(), Error> {
    match req {
        fidl_wlanix::WlanixRequest::GetWifi { payload, .. } => {
            info!("fidl_wlanix::WlanixRequest::GetWifi");
            if let Some(wifi) = payload.wifi {
                let wifi_stream = wifi.into_stream();
                serve_wifi(
                    wifi_stream,
                    Arc::clone(&state),
                    Arc::clone(&iface_manager),
                    telemetry_sender,
                )
                .await;
            }
        }
        fidl_wlanix::WlanixRequest::GetSupplicant { payload, .. } => {
            info!("fidl_wlanix::WlanixRequest::GetSupplicant");
            if let Some(supplicant) = payload.supplicant {
                let supplicant_stream = supplicant.into_stream();
                serve_supplicant(
                    supplicant_stream,
                    Arc::clone(&iface_manager),
                    telemetry_sender,
                    Arc::clone(&state),
                )
                .await;
            }
        }
        fidl_wlanix::WlanixRequest::GetNl80211 { payload, .. } => {
            info!("fidl_wlanix::WlanixRequest::GetNl80211");
            if let Some(nl80211) = payload.nl80211 {
                let nl80211_stream = nl80211.into_stream();
                serve_nl80211(nl80211_stream, Arc::clone(&state), Arc::clone(&iface_manager)).await;
            }
        }
        fidl_wlanix::WlanixRequest::_UnknownMethod { ordinal, .. } => {
            warn!("Unknown WlanixRequest ordinal: {}", ordinal);
        }
    }
    Ok(())
}

async fn serve_wlanix<I: IfaceManager>(
    reqs: fidl_wlanix::WlanixRequestStream,
    state: Arc<Mutex<WifiState>>,
    iface_manager: Arc<I>,
    telemetry_sender: TelemetrySender,
) {
    reqs.for_each_concurrent(None, |req| async {
        match req {
            Ok(req) => {
                if let Err(e) = handle_wlanix_request(
                    req,
                    Arc::clone(&state),
                    Arc::clone(&iface_manager),
                    telemetry_sender.clone(),
                )
                .await
                {
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

async fn serve_fidl<I: IfaceManager>(
    iface_manager: Arc<I>,
    telemetry_sender: TelemetrySender,
) -> Result<(), Error> {
    let mut fs = ServiceFs::new();
    let _inspect_server_task = inspect_runtime::publish(
        fuchsia_inspect::component::inspector(),
        inspect_runtime::PublishOptions::default(),
    );
    let state = Arc::new(Mutex::new(WifiState::default()));
    let _ = fs.dir("svc").add_fidl_service(move |reqs| {
        serve_wlanix(reqs, Arc::clone(&state), Arc::clone(&iface_manager), telemetry_sender.clone())
    });
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

    let iface_manager = ifaces::DeviceMonitorIfaceManager::new()
        .expect("Failed to connect wlanix to wlandevicemonitor");

    // Setup telemetry module
    let cobalt_logger = wlan_telemetry::setup_cobalt_proxy().await.unwrap_or_else(|err| {
        error!("Cobalt service unavailable, will discard all metrics: {}", err);
        // This will only happen if Cobalt is very broken in some way. Using the disconnected
        // proxy will result in log spam as metrics fail to send, but it's preferable to run rather
        // than panicking, so the user still has the option of wireless connectivity (e.g. to OTA).
        wlan_telemetry::setup_disconnected_cobalt_proxy()
            .expect("Failed to create any FIDL channels, panicking")
    });
    let (persistence_sender, persistence_fut) = wlan_telemetry::setup_persistence_req_sender()
        .map(|(sender, future)| (sender, OptionFuture::from(Some(future))))
        .unwrap_or_else(|err| {
            error!("Inspect data will not persist across reboot: {}", err);
            // To keep the code paths identical, create an empty OptionFuture that can run in the
            // `try_join!()`.
            (wlan_telemetry::setup_disconnected_persistence_req_sender(), OptionFuture::from(None))
        });
    const CLIENT_STATS_NODE_NAME: &'static str = "client_stats";
    let (telemetry_sender, serve_telemetry_fut) = wlan_telemetry::serve_telemetry(
        cobalt_logger,
        iface_manager.clone_device_monitor_svc(),
        fuchsia_inspect::component::inspector().root().create_child(CLIENT_STATS_NODE_NAME),
        &format!("root/{CLIENT_STATS_NODE_NAME}"),
        persistence_sender,
    );

    let res = futures::try_join!(
        serve_telemetry_fut,
        persistence_fut.map(Ok),
        serve_fidl(Arc::new(iface_manager), telemetry_sender)
    );
    match res {
        Ok(_) => info!("Wlanix exiting cleanly"),
        Err(e) => error!("Wlanix exiting with error: {}", e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::format_err;
    use fidl::endpoints::{create_proxy, create_proxy_and_stream, create_request_stream, Proxy};
    use futures::channel::mpsc;
    use futures::task::Poll;
    use futures::Future;
    use ieee80211::Ssid;
    use ifaces::test_utils::{ClientIfaceCall, TestIfaceManager, FAKE_IFACE_RESPONSE};
    use rand::Rng as _;
    use std::pin::{pin, Pin};
    use test_case::test_case;
    use wlan_common::assert_variant;
    use {
        fidl_fuchsia_wlan_ieee80211 as fidl_ieee80211, fidl_fuchsia_wlan_internal as fidl_internal,
    };

    const CHIP_ID: u32 = 1;

    // This will only work if the message is a parseable nl80211 message. Some
    // attributes are currently write only in our NL80211 implementation. If a
    // write-only attribute is included, this function will panic.
    fn expect_nl80211_message(message: &fidl_wlanix::Nl80211Message) -> GenlMessage<Nl80211> {
        assert_eq!(message.message_type, Some(fidl_wlanix::Nl80211MessageType::Message));
        GenlMessage::deserialize(
            &NetlinkHeader::default(),
            &message.payload.as_ref().expect("Message should always have a payload")[..],
        )
        .expect("Failed to deserialize genetlink message")
    }

    #[test]
    fn test_wifi_get_state_is_started() {
        let (mut test_helper, mut test_fut) = setup_wifi_test();

        let get_state_fut = test_helper.wifi_proxy.get_state();
        let mut get_state_fut = pin!(get_state_fut);
        assert_variant!(test_helper.exec.run_until_stalled(&mut get_state_fut), Poll::Pending);
        assert_variant!(test_helper.exec.run_until_stalled(&mut test_fut), Poll::Pending);
        let response = assert_variant!(
            test_helper.exec.run_until_stalled(&mut get_state_fut),
            Poll::Ready(Ok(response)) => response
        );
        assert_eq!(response.is_started, Some(false));
    }

    #[test]
    fn test_wifi_start() {
        let (mut test_helper, mut test_fut) = setup_wifi_test();

        let start_fut = test_helper.wifi_proxy.start();
        let mut start_fut = pin!(start_fut);
        assert_variant!(test_helper.exec.run_until_stalled(&mut start_fut), Poll::Pending);

        let get_state_fut = test_helper.wifi_proxy.get_state();
        let mut get_state_fut = pin!(get_state_fut);
        assert_variant!(test_helper.exec.run_until_stalled(&mut get_state_fut), Poll::Pending);
        assert_variant!(test_helper.exec.run_until_stalled(&mut test_fut), Poll::Pending);
        let response = assert_variant!(
            test_helper.exec.run_until_stalled(&mut get_state_fut),
            Poll::Ready(Ok(response)) => response
        );
        assert_eq!(response.is_started, Some(true));

        assert_variant!(
            test_helper.telemetry_receiver.try_next(),
            Ok(Some(TelemetryEvent::ClientConnectionsToggle {
                event: wlan_telemetry::ClientConnectionsToggleEvent::Enabled
            }))
        );
    }

    #[test]
    fn test_wifi_stop() {
        let (mut test_helper, mut test_fut) = setup_wifi_test();

        let start_fut = test_helper.wifi_proxy.start();
        let mut start_fut = pin!(start_fut);
        assert_variant!(test_helper.exec.run_until_stalled(&mut start_fut), Poll::Pending);

        let stop_fut = test_helper.wifi_proxy.stop();
        let mut stop_fut = pin!(stop_fut);
        assert_variant!(test_helper.exec.run_until_stalled(&mut stop_fut), Poll::Pending);

        let get_state_fut = test_helper.wifi_proxy.get_state();
        let mut get_state_fut = pin!(get_state_fut);
        assert_variant!(test_helper.exec.run_until_stalled(&mut get_state_fut), Poll::Pending);
        assert_variant!(test_helper.exec.run_until_stalled(&mut test_fut), Poll::Pending);
        let response = assert_variant!(
            test_helper.exec.run_until_stalled(&mut get_state_fut),
            Poll::Ready(Ok(response)) => response
        );
        assert_eq!(response.is_started, Some(false));

        // On stop, we shut down all remaining ifaces.
        let calls = test_helper.iface_manager.calls.lock();
        assert!(!calls.is_empty());
        assert_variant!(
            &calls[calls.len() - 1],
            ifaces::test_utils::IfaceManagerCall::DestroyIface(_)
        );

        // There was a start and a stop, so expect enabled and disabled mesages.
        assert_variant!(
            test_helper.telemetry_receiver.try_next(),
            Ok(Some(TelemetryEvent::ClientConnectionsToggle {
                event: wlan_telemetry::ClientConnectionsToggleEvent::Enabled
            }))
        );
        assert_variant!(
            test_helper.telemetry_receiver.try_next(),
            Ok(Some(TelemetryEvent::ClientConnectionsToggle {
                event: wlan_telemetry::ClientConnectionsToggleEvent::Disabled
            }))
        );
        assert!(test_helper.telemetry_receiver.try_next().is_err());
    }

    #[test]
    fn test_wifi_get_chip_ids() {
        let (mut test_helper, mut test_fut) = setup_wifi_test();

        let get_chip_ids_fut = test_helper.wifi_proxy.get_chip_ids();
        let mut get_chip_ids_fut = pin!(get_chip_ids_fut);
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
        let (mut test_helper, mut test_fut) = setup_wifi_test();

        let get_available_modes_fut = test_helper.wifi_chip_proxy.get_available_modes();
        let mut get_available_modes_fut = pin!(get_available_modes_fut);
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
                id: Some(CHIP_ID),
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
    fn test_wifi_chip_get_id() {
        let (mut test_helper, mut test_fut) = setup_wifi_test();

        let get_id_fut = test_helper.wifi_chip_proxy.get_id();
        let mut get_id_fut = pin!(get_id_fut);
        assert_variant!(test_helper.exec.run_until_stalled(&mut get_id_fut), Poll::Pending);
        assert_variant!(test_helper.exec.run_until_stalled(&mut test_fut), Poll::Pending);
        let response = assert_variant!(test_helper.exec.run_until_stalled(&mut get_id_fut), Poll::Ready(Ok(response)) => response);
        assert_eq!(response.id, Some(CHIP_ID));
    }

    #[test]
    fn test_wifi_chip_get_mode() {
        let (mut test_helper, mut test_fut) = setup_wifi_test();

        let get_mode_fut = test_helper.wifi_chip_proxy.get_mode();
        let mut get_mode_fut = pin!(get_mode_fut);
        assert_variant!(test_helper.exec.run_until_stalled(&mut get_mode_fut), Poll::Pending);
        assert_variant!(test_helper.exec.run_until_stalled(&mut test_fut), Poll::Pending);
        let response = assert_variant!(test_helper.exec.run_until_stalled(&mut get_mode_fut), Poll::Ready(Ok(response)) => response);
        assert_eq!(response.mode, Some(0));
    }

    #[test]
    fn test_wifi_chip_get_capabilities() {
        let (mut test_helper, mut test_fut) = setup_wifi_test();

        let get_capabilities_fut = test_helper.wifi_chip_proxy.get_capabilities();
        let mut get_capabilities_fut = pin!(get_capabilities_fut);
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
    fn test_wifi_chip_remove_sta_iface() {
        let (mut test_helper, mut test_fut) = setup_wifi_test();

        let remove_sta_iface_fut = test_helper.wifi_chip_proxy.remove_sta_iface(
            fidl_wlanix::WifiChipRemoveStaIfaceRequest {
                iface_name: Some("some_iface_name".to_string()), // iface name doesn't matter
                ..Default::default()
            },
        );
        let mut remove_sta_iface_fut = pin!(remove_sta_iface_fut);
        assert_variant!(
            test_helper.exec.run_until_stalled(&mut remove_sta_iface_fut),
            Poll::Pending
        );
        assert_eq!(test_helper.exec.run_until_stalled(&mut test_fut), Poll::Pending);
        assert_variant!(
            test_helper.exec.run_until_stalled(&mut remove_sta_iface_fut),
            Poll::Ready(Ok(Ok(())))
        );

        assert_variant!(
            test_helper.telemetry_receiver.try_next(),
            Ok(Some(TelemetryEvent::ClientIfaceDestroyed { iface_id })) => {
                assert_eq!(iface_id, FAKE_IFACE_RESPONSE.id);
            }
        );
    }

    #[test]
    fn test_wifi_chip_get_sta_iface_names() {
        let (mut test_helper, mut test_fut) = setup_wifi_test();

        // We observe the iface created by setup_wifi_test.
        let get_iface_names_fut = test_helper.wifi_chip_proxy.get_sta_iface_names();
        let mut get_iface_names_fut = pin!(get_iface_names_fut);
        assert_variant!(test_helper.exec.run_until_stalled(&mut test_fut), Poll::Pending);
        let result = assert_variant!(test_helper.exec.run_until_stalled(&mut get_iface_names_fut), Poll::Ready(Ok(result)) => result);
        assert_eq!(result.iface_names, Some(vec![IFACE_NAME.to_string()]));
    }

    #[test]
    fn test_wifi_chip_get_sta_iface_names_no_ifaces() {
        let (mut test_helper, mut test_fut) = setup_wifi_test();

        // Remove the iface from setup.
        let _ = test_helper.iface_manager.client_iface.lock().take();

        // No ifaces show up.
        let get_iface_names_fut = test_helper.wifi_chip_proxy.get_sta_iface_names();
        let mut get_iface_names_fut = pin!(get_iface_names_fut);
        assert_variant!(test_helper.exec.run_until_stalled(&mut test_fut), Poll::Pending);
        let result = assert_variant!(test_helper.exec.run_until_stalled(&mut get_iface_names_fut), Poll::Ready(Ok(result)) => result);
        assert_eq!(result.iface_names, Some(vec![]));
    }

    #[test]
    fn test_wifi_chip_get_sta_iface() {
        let (mut test_helper, mut test_fut) = setup_wifi_test();

        let (wifi_sta_iface_proxy, wifi_sta_iface_server_end) =
            create_proxy::<fidl_wlanix::WifiStaIfaceMarker>();
        let mut get_sta_iface_fut =
            test_helper.wifi_chip_proxy.get_sta_iface(fidl_wlanix::WifiChipGetStaIfaceRequest {
                iface_name: Some("FOO".to_string()),
                iface: Some(wifi_sta_iface_server_end),
                ..Default::default()
            });

        assert_variant!(test_helper.exec.run_until_stalled(&mut test_fut), Poll::Pending);
        let result = assert_variant!(test_helper.exec.run_until_stalled(&mut get_sta_iface_fut), Poll::Ready(Ok(result)) => result);
        assert!(result.is_ok());

        let get_name_fut = wifi_sta_iface_proxy.get_name();
        let mut get_name_fut = pin!(get_name_fut);
        assert_variant!(test_helper.exec.run_until_stalled(&mut get_name_fut), Poll::Pending);
        assert_variant!(test_helper.exec.run_until_stalled(&mut test_fut), Poll::Pending);
        let response = assert_variant!(
            test_helper.exec.run_until_stalled(&mut get_name_fut),
            Poll::Ready(Ok(response)) => response
        );
        assert_eq!(response.iface_name, Some(IFACE_NAME.to_string()));
    }

    #[test]
    fn test_wifi_chip_get_sta_iface_no_iface() {
        let (mut test_helper, mut test_fut) = setup_wifi_test();
        let _ = test_helper.iface_manager.client_iface.lock().take();

        let (_wifi_sta_iface_proxy, wifi_sta_iface_server_end) =
            create_proxy::<fidl_wlanix::WifiStaIfaceMarker>();
        let mut get_sta_iface_fut =
            test_helper.wifi_chip_proxy.get_sta_iface(fidl_wlanix::WifiChipGetStaIfaceRequest {
                iface_name: Some("FOO".to_string()),
                iface: Some(wifi_sta_iface_server_end),
                ..Default::default()
            });

        assert_variant!(test_helper.exec.run_until_stalled(&mut test_fut), Poll::Pending);
        let result = assert_variant!(test_helper.exec.run_until_stalled(&mut get_sta_iface_fut), Poll::Ready(Ok(result)) => result);
        assert!(result.is_err());
    }

    #[test]
    fn test_wifi_sta_iface_get_name() {
        let (mut test_helper, mut test_fut) = setup_wifi_test();

        let get_name_fut = test_helper.wifi_sta_iface_proxy.get_name();
        let mut get_name_fut = pin!(get_name_fut);
        assert_variant!(test_helper.exec.run_until_stalled(&mut get_name_fut), Poll::Pending);
        assert_variant!(test_helper.exec.run_until_stalled(&mut test_fut), Poll::Pending);
        let response = assert_variant!(
            test_helper.exec.run_until_stalled(&mut get_name_fut),
            Poll::Ready(Ok(response)) => response
        );
        assert_eq!(response.iface_name, Some(IFACE_NAME.to_string()));
    }

    struct WifiTestHelper {
        _wlanix_proxy: fidl_wlanix::WlanixProxy,
        wifi_proxy: fidl_wlanix::WifiProxy,
        wifi_chip_proxy: fidl_wlanix::WifiChipProxy,
        wifi_sta_iface_proxy: fidl_wlanix::WifiStaIfaceProxy,
        telemetry_receiver: mpsc::Receiver<TelemetryEvent>,
        iface_manager: Arc<TestIfaceManager>,

        // Note: keep the executor field last in the struct so it gets dropped last.
        exec: fasync::TestExecutor,
    }

    fn setup_wifi_test() -> (WifiTestHelper, Pin<Box<impl Future<Output = ()>>>) {
        let mut exec = fasync::TestExecutor::new_with_fake_time();
        exec.set_fake_time(fasync::MonotonicInstant::from_nanos(0));

        let (wlanix_proxy, wlanix_stream) = create_proxy_and_stream::<fidl_wlanix::WlanixMarker>();
        let (wifi_proxy, wifi_server_end) = create_proxy::<fidl_wlanix::WifiMarker>();
        let result = wlanix_proxy.get_wifi(fidl_wlanix::WlanixGetWifiRequest {
            wifi: Some(wifi_server_end),
            ..Default::default()
        });
        assert_variant!(result, Ok(()));

        let (wifi_chip_proxy, wifi_chip_server_end) = create_proxy::<fidl_wlanix::WifiChipMarker>();
        let get_chip_fut = wifi_proxy.get_chip(fidl_wlanix::WifiGetChipRequest {
            chip_id: Some(CHIP_ID),
            chip: Some(wifi_chip_server_end),
            ..Default::default()
        });
        let mut get_chip_fut = pin!(get_chip_fut);
        assert_variant!(exec.run_until_stalled(&mut get_chip_fut), Poll::Pending);

        let (wifi_sta_iface_proxy, wifi_sta_iface_server_end) =
            create_proxy::<fidl_wlanix::WifiStaIfaceMarker>();
        let create_sta_iface_fut =
            wifi_chip_proxy.create_sta_iface(fidl_wlanix::WifiChipCreateStaIfaceRequest {
                iface: Some(wifi_sta_iface_server_end),
                ..Default::default()
            });
        let mut create_sta_iface_fut = pin!(create_sta_iface_fut);
        assert_variant!(exec.run_until_stalled(&mut create_sta_iface_fut), Poll::Pending);

        let wifi_state = Arc::new(Mutex::new(WifiState::default()));
        let iface_manager = Arc::new(TestIfaceManager::new());
        let (telemetry_sender, mut telemetry_receiver) = mpsc::channel::<TelemetryEvent>(100);
        let test_fut = serve_wlanix(
            wlanix_stream,
            wifi_state,
            Arc::clone(&iface_manager),
            TelemetrySender::new(telemetry_sender),
        );
        let mut test_fut = Box::pin(test_fut);
        assert_eq!(exec.run_until_stalled(&mut test_fut), Poll::Pending);

        assert_variant!(exec.run_until_stalled(&mut get_chip_fut), Poll::Ready(Ok(Ok(()))));
        assert_variant!(exec.run_until_stalled(&mut create_sta_iface_fut), Poll::Ready(Ok(Ok(()))));

        assert_variant!(telemetry_receiver.try_next(), Ok(Some(TelemetryEvent::ClientIfaceCreated { iface_id })) => {
            assert_eq!(iface_id, FAKE_IFACE_RESPONSE.id);
        });

        // Quick check that telemetry event queue is now empty
        assert_variant!(telemetry_receiver.try_next(), Err(_));

        let test_helper = WifiTestHelper {
            _wlanix_proxy: wlanix_proxy,
            wifi_proxy,
            wifi_chip_proxy,
            wifi_sta_iface_proxy,
            telemetry_receiver,
            iface_manager,
            exec,
        };
        (test_helper, test_fut)
    }

    #[test]
    fn test_supplicant_sta_iface_disconnect() {
        let (mut test_helper, mut test_fut) = setup_supplicant_test();

        let mut disconnect_fut = test_helper.supplicant_sta_iface_proxy.disconnect();
        assert_variant!(test_helper.exec.run_until_stalled(&mut disconnect_fut), Poll::Pending);
        assert_variant!(test_helper.exec.run_until_stalled(&mut test_fut), Poll::Pending);
        let iface_calls = test_helper.iface_manager.get_iface_call_history();
        assert_variant!(&iface_calls.lock()[0], ClientIfaceCall::Disconnect);
        assert_variant!(
            test_helper.exec.run_until_stalled(&mut disconnect_fut),
            Poll::Ready(Ok(()))
        );
    }

    #[test]
    fn test_supplicant_sta_open_network_connect_flow() {
        let (mut test_helper, mut test_fut) = setup_supplicant_test();

        let mut mcast_stream = get_nl80211_mcast(&test_helper.nl80211_proxy, "mlme");
        let next_mcast = next_mcast_message(&mut mcast_stream);
        let mut next_mcast = pin!(next_mcast);
        assert_variant!(test_helper.exec.run_until_stalled(&mut test_fut), Poll::Pending);
        assert_variant!(test_helper.exec.run_until_stalled(&mut next_mcast), Poll::Pending);

        let result = test_helper.supplicant_sta_network_proxy.set_ssid(
            &fidl_wlanix::SupplicantStaNetworkSetSsidRequest {
                ssid: Some(vec![b'f', b'o', b'o']),
                ..Default::default()
            },
        );
        assert_variant!(result, Ok(()));
        assert_variant!(test_helper.supplicant_sta_network_proxy.clear_bssid(), Ok(()));

        let mut network_select_fut = test_helper.supplicant_sta_network_proxy.select();
        assert_variant!(test_helper.exec.run_until_stalled(&mut network_select_fut), Poll::Pending);
        assert_variant!(test_helper.exec.run_until_stalled(&mut test_fut), Poll::Pending);
        assert_variant!(
            test_helper.exec.run_until_stalled(&mut network_select_fut),
            Poll::Ready(Ok(Ok(())))
        );

        let iface_calls = test_helper.iface_manager.get_iface_call_history();
        let (ssid, passphrase, bssid) = assert_variant!(
            iface_calls.lock()[0].clone(),
            ClientIfaceCall::ConnectToNetwork { ssid, passphrase, bssid } => (ssid, passphrase, bssid)
        );
        assert_eq!(ssid, vec![b'f', b'o', b'o']);
        assert_eq!(passphrase, None);
        assert_eq!(bssid, None);
        let mut next_callback_fut = test_helper.supplicant_sta_iface_callback_stream.next();
        let on_state_changed = assert_variant!(test_helper.exec.run_until_stalled(&mut next_callback_fut), Poll::Ready(Some(Ok(fidl_wlanix::SupplicantStaIfaceCallbackRequest::OnStateChanged { payload, .. }))) => payload);
        assert_eq!(on_state_changed.new_state, Some(fidl_wlanix::StaIfaceCallbackState::Completed));
        assert_eq!(on_state_changed.bssid, Some([42, 42, 42, 42, 42, 42]));
        assert_eq!(on_state_changed.id, Some(1));
        assert_eq!(on_state_changed.ssid, Some(vec![b'f', b'o', b'o']));

        let mcast_msg = assert_variant!(test_helper.exec.run_until_stalled(&mut next_mcast), Poll::Ready(msg) => msg);
        assert_eq!(mcast_msg.payload.cmd, Nl80211Cmd::Connect);

        assert_variant!(
            test_helper.telemetry_receiver.try_next(),
            Ok(Some(TelemetryEvent::ConnectResult { result, bss })) => {
                assert_eq!(result, fidl_ieee80211::StatusCode::Success);
                assert_eq!(bss.ssid, Ssid::try_from("foo").unwrap());
                assert_eq!(bss.bssid, Bssid::from([42, 42, 42, 42, 42, 42]));
            }
        );
    }

    #[test]
    fn test_supplicant_sta_protected_network_connect_flow() {
        let (mut test_helper, mut test_fut) = setup_supplicant_test();

        let mut mcast_stream = get_nl80211_mcast(&test_helper.nl80211_proxy, "mlme");
        let next_mcast = next_mcast_message(&mut mcast_stream);
        let mut next_mcast = pin!(next_mcast);
        assert_variant!(test_helper.exec.run_until_stalled(&mut test_fut), Poll::Pending);
        assert_variant!(test_helper.exec.run_until_stalled(&mut next_mcast), Poll::Pending);

        let result = test_helper.supplicant_sta_network_proxy.set_ssid(
            &fidl_wlanix::SupplicantStaNetworkSetSsidRequest {
                ssid: Some(vec![b'f', b'o', b'o']),
                ..Default::default()
            },
        );
        assert_variant!(result, Ok(()));

        let result = test_helper.supplicant_sta_network_proxy.set_psk_passphrase(
            &fidl_wlanix::SupplicantStaNetworkSetPskPassphraseRequest {
                passphrase: Some(vec![b'p', b'a', b's', b's']),
                ..Default::default()
            },
        );
        assert_variant!(result, Ok(()));
        assert_variant!(test_helper.supplicant_sta_network_proxy.clear_bssid(), Ok(()));

        let mut network_select_fut = test_helper.supplicant_sta_network_proxy.select();
        assert_variant!(test_helper.exec.run_until_stalled(&mut network_select_fut), Poll::Pending);
        assert_variant!(test_helper.exec.run_until_stalled(&mut test_fut), Poll::Pending);
        assert_variant!(
            test_helper.exec.run_until_stalled(&mut network_select_fut),
            Poll::Ready(Ok(Ok(())))
        );

        let iface_calls = test_helper.iface_manager.get_iface_call_history();
        let (ssid, passphrase, bssid) = assert_variant!(
            iface_calls.lock()[0].clone(),
            ClientIfaceCall::ConnectToNetwork { ssid, passphrase, bssid } => (ssid, passphrase, bssid)
        );
        assert_eq!(ssid, vec![b'f', b'o', b'o']);
        assert_eq!(passphrase, Some(vec![b'p', b'a', b's', b's']));
        assert_eq!(bssid, None);
        let mut next_callback_fut = test_helper.supplicant_sta_iface_callback_stream.next();
        let on_state_changed = assert_variant!(test_helper.exec.run_until_stalled(&mut next_callback_fut), Poll::Ready(Some(Ok(fidl_wlanix::SupplicantStaIfaceCallbackRequest::OnStateChanged { payload, .. }))) => payload);
        assert_eq!(on_state_changed.new_state, Some(fidl_wlanix::StaIfaceCallbackState::Completed));

        let mcast_msg = assert_variant!(test_helper.exec.run_until_stalled(&mut next_mcast), Poll::Ready(msg) => msg);
        assert_eq!(mcast_msg.payload.cmd, Nl80211Cmd::Connect);
    }

    #[test]
    fn test_supplicant_sta_network_connect_flow_with_bssid_set() {
        let (mut test_helper, mut test_fut) = setup_supplicant_test();

        let mut mcast_stream = get_nl80211_mcast(&test_helper.nl80211_proxy, "mlme");
        let next_mcast = next_mcast_message(&mut mcast_stream);
        let mut next_mcast = pin!(next_mcast);
        assert_variant!(test_helper.exec.run_until_stalled(&mut test_fut), Poll::Pending);
        assert_variant!(test_helper.exec.run_until_stalled(&mut next_mcast), Poll::Pending);

        let result = test_helper.supplicant_sta_network_proxy.set_ssid(
            &fidl_wlanix::SupplicantStaNetworkSetSsidRequest {
                ssid: Some(vec![b'f', b'o', b'o']),
                ..Default::default()
            },
        );
        assert_variant!(result, Ok(()));

        let result = test_helper.supplicant_sta_network_proxy.set_bssid(
            &fidl_wlanix::SupplicantStaNetworkSetBssidRequest {
                bssid: Some([1, 2, 3, 4, 5, 6]),
                ..Default::default()
            },
        );
        assert_variant!(result, Ok(()));

        let mut network_select_fut = test_helper.supplicant_sta_network_proxy.select();
        assert_variant!(test_helper.exec.run_until_stalled(&mut network_select_fut), Poll::Pending);
        assert_variant!(test_helper.exec.run_until_stalled(&mut test_fut), Poll::Pending);
        assert_variant!(
            test_helper.exec.run_until_stalled(&mut network_select_fut),
            Poll::Ready(Ok(Ok(())))
        );

        let iface_calls = test_helper.iface_manager.get_iface_call_history();
        let (ssid, passphrase, bssid) = assert_variant!(
            iface_calls.lock()[0].clone(),
            ClientIfaceCall::ConnectToNetwork { ssid, passphrase, bssid } => (ssid, passphrase, bssid)
        );
        assert_eq!(ssid, vec![b'f', b'o', b'o']);
        assert_eq!(passphrase, None);
        assert_eq!(bssid, Some(Bssid::from([1, 2, 3, 4, 5, 6])));
        let mut next_callback_fut = test_helper.supplicant_sta_iface_callback_stream.next();
        let on_state_changed = assert_variant!(test_helper.exec.run_until_stalled(&mut next_callback_fut), Poll::Ready(Some(Ok(fidl_wlanix::SupplicantStaIfaceCallbackRequest::OnStateChanged { payload, .. }))) => payload);
        assert_eq!(on_state_changed.new_state, Some(fidl_wlanix::StaIfaceCallbackState::Completed));
        assert_eq!(on_state_changed.bssid, Some([1, 2, 3, 4, 5, 6]));
        assert_eq!(on_state_changed.id, Some(1));
        assert_eq!(on_state_changed.ssid, Some(vec![b'f', b'o', b'o']));

        let mcast_msg = assert_variant!(test_helper.exec.run_until_stalled(&mut next_mcast), Poll::Ready(msg) => msg);
        assert_eq!(mcast_msg.payload.cmd, Nl80211Cmd::Connect);
    }

    #[test]
    fn test_supplicant_sta_network_connect_flow_with_bssid_set_and_cleared() {
        let (mut test_helper, mut test_fut) = setup_supplicant_test();

        let mut mcast_stream = get_nl80211_mcast(&test_helper.nl80211_proxy, "mlme");
        let next_mcast = next_mcast_message(&mut mcast_stream);
        let mut next_mcast = pin!(next_mcast);
        assert_variant!(test_helper.exec.run_until_stalled(&mut test_fut), Poll::Pending);
        assert_variant!(test_helper.exec.run_until_stalled(&mut next_mcast), Poll::Pending);

        let result = test_helper.supplicant_sta_network_proxy.set_ssid(
            &fidl_wlanix::SupplicantStaNetworkSetSsidRequest {
                ssid: Some(vec![b'f', b'o', b'o']),
                ..Default::default()
            },
        );
        assert_variant!(result, Ok(()));

        let result = test_helper.supplicant_sta_network_proxy.set_bssid(
            &fidl_wlanix::SupplicantStaNetworkSetBssidRequest {
                bssid: Some([1, 2, 3, 4, 5, 6]),
                ..Default::default()
            },
        );
        assert_variant!(result, Ok(()));
        assert_variant!(test_helper.supplicant_sta_network_proxy.clear_bssid(), Ok(()));

        let mut network_select_fut = test_helper.supplicant_sta_network_proxy.select();
        assert_variant!(test_helper.exec.run_until_stalled(&mut network_select_fut), Poll::Pending);
        assert_variant!(test_helper.exec.run_until_stalled(&mut test_fut), Poll::Pending);
        assert_variant!(
            test_helper.exec.run_until_stalled(&mut network_select_fut),
            Poll::Ready(Ok(Ok(())))
        );

        let iface_calls = test_helper.iface_manager.get_iface_call_history();
        let bssid = assert_variant!(
            iface_calls.lock()[0].clone(),
            ClientIfaceCall::ConnectToNetwork { bssid, .. } => bssid
        );
        assert_eq!(bssid, None);
        let mut next_callback_fut = test_helper.supplicant_sta_iface_callback_stream.next();
        let on_state_changed = assert_variant!(test_helper.exec.run_until_stalled(&mut next_callback_fut), Poll::Ready(Some(Ok(fidl_wlanix::SupplicantStaIfaceCallbackRequest::OnStateChanged { payload, .. }))) => payload);
        assert_eq!(on_state_changed.new_state, Some(fidl_wlanix::StaIfaceCallbackState::Completed));
        assert_eq!(on_state_changed.bssid, Some([42, 42, 42, 42, 42, 42]));
    }

    fn establish_open_connection(
        test_helper: &mut SupplicantTestHelper,
        test_fut: &mut Pin<Box<impl Future<Output = ()>>>,
        mcast_stream: &mut fidl_wlanix::Nl80211MulticastRequestStream,
    ) {
        let result = test_helper.supplicant_sta_network_proxy.set_ssid(
            &fidl_wlanix::SupplicantStaNetworkSetSsidRequest {
                ssid: Some(vec![b'f', b'o', b'o']),
                ..Default::default()
            },
        );
        assert_variant!(result, Ok(()));
        assert_variant!(test_helper.supplicant_sta_network_proxy.clear_bssid(), Ok(()));

        let mut network_select_fut = test_helper.supplicant_sta_network_proxy.select();
        assert_variant!(test_helper.exec.run_until_stalled(&mut network_select_fut), Poll::Pending);
        assert_variant!(test_helper.exec.run_until_stalled(test_fut), Poll::Pending);
        assert_variant!(
            test_helper.exec.run_until_stalled(&mut network_select_fut),
            Poll::Ready(Ok(Ok(())))
        );

        {
            let next_mcast = next_mcast_message(mcast_stream);
            let mut next_mcast = pin!(next_mcast);
            let mcast_msg = assert_variant!(test_helper.exec.run_until_stalled(&mut next_mcast), Poll::Ready(msg) => msg);
            assert_eq!(mcast_msg.payload.cmd, Nl80211Cmd::Connect);
        }

        let mut next_callback_fut = test_helper.supplicant_sta_iface_callback_stream.next();
        let on_state_changed = assert_variant!(test_helper.exec.run_until_stalled(&mut next_callback_fut), Poll::Ready(Some(Ok(fidl_wlanix::SupplicantStaIfaceCallbackRequest::OnStateChanged { payload, .. }))) => payload);
        assert_eq!(on_state_changed.new_state, Some(fidl_wlanix::StaIfaceCallbackState::Completed));
    }

    #[test]
    fn test_supplicant_sta_network_connect_flow_failure() {
        let (mut test_helper, mut test_fut) = setup_supplicant_test();

        // Configure our iface manager to send a connect failure.
        *test_helper.iface_manager.get_client_iface().connect_success.lock() = false;

        let mut mcast_stream = get_nl80211_mcast(&test_helper.nl80211_proxy, "mlme");
        let next_mcast = next_mcast_message(&mut mcast_stream);
        let mut next_mcast = pin!(next_mcast);
        assert_variant!(test_helper.exec.run_until_stalled(&mut test_fut), Poll::Pending);
        assert_variant!(test_helper.exec.run_until_stalled(&mut next_mcast), Poll::Pending);

        let result = test_helper.supplicant_sta_network_proxy.set_ssid(
            &fidl_wlanix::SupplicantStaNetworkSetSsidRequest {
                ssid: Some(vec![b'f', b'o', b'o']),
                ..Default::default()
            },
        );
        assert_variant!(result, Ok(()));
        assert_variant!(test_helper.supplicant_sta_network_proxy.clear_bssid(), Ok(()));

        let mut network_select_fut = test_helper.supplicant_sta_network_proxy.select();
        assert_variant!(test_helper.exec.run_until_stalled(&mut test_fut), Poll::Pending);
        assert_variant!(
            test_helper.exec.run_until_stalled(&mut network_select_fut),
            Poll::Ready(Ok(Err(zx::sys::ZX_ERR_INTERNAL)))
        );

        let iface_calls = test_helper.iface_manager.get_iface_call_history();
        assert_variant!(iface_calls.lock()[0].clone(), ClientIfaceCall::ConnectToNetwork { .. });
        let mut next_callback_fut = test_helper.supplicant_sta_iface_callback_stream.next();
        let reject = assert_variant!(test_helper.exec.run_until_stalled(&mut next_callback_fut), Poll::Ready(Some(Ok(fidl_wlanix::SupplicantStaIfaceCallbackRequest::OnAssociationRejected { payload, .. }))) => payload);
        assert_eq!(reject.bssid, Some([42, 42, 42, 42, 42, 42]));
        assert_eq!(reject.ssid, Some(vec![b'f', b'o', b'o']));
        assert_eq!(reject.status_code, Some(fidl_ieee80211::StatusCode::RefusedReasonUnspecified));
        assert_eq!(reject.timed_out, Some(false));
    }

    #[test]
    fn test_supplicant_sta_disconnect_signal() {
        let (mut test_helper, mut test_fut) = setup_supplicant_test();
        let mut mcast_stream = get_nl80211_mcast(&test_helper.nl80211_proxy, "mlme");

        establish_open_connection(&mut test_helper, &mut test_fut, &mut mcast_stream);
        // Metrics: for this test, we don't care about the contents of the ConnectResult
        assert_variant!(
            test_helper.telemetry_receiver.try_next(),
            Ok(Some(TelemetryEvent::ConnectResult { result: _, bss: _ }))
        );

        let connection_length_nanos: u16 = rand::thread_rng().gen();
        test_helper
            .exec
            .set_fake_time(fasync::MonotonicInstant::from_nanos(connection_length_nanos.into()));

        let mocked_disconnect_source = fidl_sme::DisconnectSource::Ap(fidl_sme::DisconnectCause {
            mlme_event_name: fidl_sme::DisconnectMlmeEventName::DeauthenticateIndication,
            reason_code: fidl_fuchsia_wlan_ieee80211::ReasonCode::ReasonInactivity,
        });
        let mocked_is_sme_reconnecting = false;
        {
            let client_iface = test_helper.iface_manager.get_client_iface();
            let transaction_handle = client_iface.transaction_handle.lock();
            let control_handle = transaction_handle.as_ref().expect("No control handle found");
            control_handle
                .send_on_disconnect(&fidl_sme::DisconnectInfo {
                    is_sme_reconnecting: mocked_is_sme_reconnecting,
                    disconnect_source: mocked_disconnect_source.clone(),
                })
                .expect("Failed to send OnDisconnect");
        }

        assert_variant!(test_helper.exec.run_until_stalled(&mut test_fut), Poll::Pending);
        let mut next_callback_fut = test_helper.supplicant_sta_iface_callback_stream.next();

        assert_variant!(
            test_helper.exec.run_until_stalled(&mut next_callback_fut),
            Poll::Ready(Some(Ok(
                fidl_wlanix::SupplicantStaIfaceCallbackRequest::OnDisconnected { .. }
            )))
        );
        let mut next_callback_fut = test_helper.supplicant_sta_iface_callback_stream.next();
        let on_state_changed = assert_variant!(
            test_helper.exec.run_until_stalled(&mut next_callback_fut),
            Poll::Ready(Some(Ok(
                fidl_wlanix::SupplicantStaIfaceCallbackRequest::OnStateChanged { payload, .. }
            ))) => payload);
        assert_eq!(
            on_state_changed.new_state,
            Some(fidl_wlanix::StaIfaceCallbackState::Disconnected)
        );

        let next_mcast = next_mcast_message(&mut mcast_stream);
        let mut next_mcast = pin!(next_mcast);
        let mcast_msg = assert_variant!(test_helper.exec.run_until_stalled(&mut next_mcast), Poll::Ready(msg) => msg);
        assert_eq!(mcast_msg.payload.cmd, Nl80211Cmd::Disconnect);

        let iface_calls = test_helper.iface_manager.get_iface_call_history();
        let disconnect_info = assert_variant!(
            iface_calls.lock().pop().expect("iface call history should not be empty"),
            ClientIfaceCall::OnDisconnect { info } => info
        );
        assert_eq!(disconnect_info, mocked_disconnect_source);

        assert_variant!(
            test_helper.telemetry_receiver.try_next(),
            Ok(Some(TelemetryEvent::Disconnect { info })) => {
                assert_eq!(info.connected_duration, zx::MonotonicDuration::from_nanos(connection_length_nanos.into()));
                assert_eq!(info.is_sme_reconnecting, mocked_is_sme_reconnecting);
                assert_eq!(info.disconnect_source, mocked_disconnect_source);
                assert_eq!(info.original_bss_desc.ssid, Ssid::try_from("foo").unwrap());
                assert_eq!(info.original_bss_desc.bssid, Bssid::from([42, 42, 42, 42, 42, 42]));
            }
        );
    }

    #[test_case(fidl_sme::ConnectResult {
        code: fidl_fuchsia_wlan_ieee80211::StatusCode::Success,
        is_credential_rejected: false,
        is_reconnect: true,
    }, false; "Successful reconnect")]
    #[test_case(fidl_sme::ConnectResult {
        code: fidl_fuchsia_wlan_ieee80211::StatusCode::RefusedReasonUnspecified,
        is_credential_rejected: false,
        is_reconnect: true,
    }, true; "Failed reconnect")]
    #[test_case(fidl_sme::ConnectResult {
        code: fidl_fuchsia_wlan_ieee80211::StatusCode::RefusedReasonUnspecified,
        is_credential_rejected: false,
        is_reconnect: false,
    }, false; "Ignore non-reconnect result")]
    fn test_supplicant_sta_sme_reconnect(
        reconnect_result: fidl_sme::ConnectResult,
        expect_disconnect: bool,
    ) {
        let (mut test_helper, mut test_fut) = setup_supplicant_test();
        let mut mcast_stream = get_nl80211_mcast(&test_helper.nl80211_proxy, "mlme");

        establish_open_connection(&mut test_helper, &mut test_fut, &mut mcast_stream);
        // Metrics: for this test, we don't care about the contents of the ConnectResult
        assert_variant!(
            test_helper.telemetry_receiver.try_next(),
            Ok(Some(TelemetryEvent::ConnectResult { result: _, bss: _ }))
        );

        let connection_length_nanos: u16 = rand::thread_rng().gen();
        test_helper
            .exec
            .set_fake_time(fasync::MonotonicInstant::from_nanos(connection_length_nanos.into()));

        let mocked_disconnect_source = fidl_sme::DisconnectSource::Ap(fidl_sme::DisconnectCause {
            mlme_event_name: fidl_sme::DisconnectMlmeEventName::DeauthenticateIndication,
            reason_code: fidl_fuchsia_wlan_ieee80211::ReasonCode::ReasonInactivity,
        });
        let mocked_is_sme_reconnecting = true;
        {
            let client_iface = test_helper.iface_manager.get_client_iface();
            let transaction_handle = client_iface.transaction_handle.lock();
            let control_handle = transaction_handle.as_ref().expect("No control handle found");
            control_handle
                .send_on_disconnect(&fidl_sme::DisconnectInfo {
                    is_sme_reconnecting: mocked_is_sme_reconnecting,
                    disconnect_source: mocked_disconnect_source.clone(),
                })
                .expect("Failed to send OnDisconnect");
        }

        // No callbacks for disconnect, since we're awaiting a reconnect result.
        assert_variant!(test_helper.exec.run_until_stalled(&mut test_fut), Poll::Pending);
        let mut next_callback_fut = test_helper.supplicant_sta_iface_callback_stream.next();
        assert_variant!(test_helper.exec.run_until_stalled(&mut next_callback_fut), Poll::Pending);
        let next_mcast = next_mcast_message(&mut mcast_stream);
        let mut next_mcast = pin!(next_mcast);
        assert_variant!(test_helper.exec.run_until_stalled(&mut next_mcast), Poll::Pending);

        // We should always log a disconnect to the metrics module, even if reconnect is pending
        assert_variant!(
            test_helper.telemetry_receiver.try_next(),
            Ok(Some(TelemetryEvent::Disconnect { info })) => {
                assert_eq!(info.connected_duration, zx::MonotonicDuration::from_nanos(connection_length_nanos.into()));
                assert_eq!(info.is_sme_reconnecting, mocked_is_sme_reconnecting);
                assert_eq!(info.disconnect_source, mocked_disconnect_source);
                assert_eq!(info.original_bss_desc.ssid, Ssid::try_from("foo").unwrap());
                assert_eq!(info.original_bss_desc.bssid, Bssid::from([42, 42, 42, 42, 42, 42]));
            }
        );

        // Send and process the reconnect result.
        let client_iface = test_helper.iface_manager.get_client_iface();
        let locked_handle = client_iface.transaction_handle.lock();
        let handle = locked_handle.as_ref().unwrap();
        handle
            .send_on_connect_result(&reconnect_result)
            .expect("Failed to send ConnectResult for reconnect");
        assert_variant!(test_helper.exec.run_until_stalled(&mut test_fut), Poll::Pending);

        if expect_disconnect {
            assert_variant!(
                test_helper.exec.run_until_stalled(&mut next_callback_fut),
                Poll::Ready(Some(Ok(
                    fidl_wlanix::SupplicantStaIfaceCallbackRequest::OnDisconnected { .. }
                )))
            );
            let on_state_changed = assert_variant!(
            test_helper.exec.run_until_stalled(&mut next_callback_fut),
            Poll::Ready(Some(Ok(
                fidl_wlanix::SupplicantStaIfaceCallbackRequest::OnStateChanged { payload, .. }
            ))) => payload);
            assert_eq!(
                on_state_changed.new_state,
                Some(fidl_wlanix::StaIfaceCallbackState::Disconnected)
            );

            let mcast_msg = assert_variant!(test_helper.exec.run_until_stalled(&mut next_mcast), Poll::Ready(msg) => msg);
            assert_eq!(mcast_msg.payload.cmd, Nl80211Cmd::Disconnect);

            let iface_calls = test_helper.iface_manager.get_iface_call_history();
            let disconnect_info = assert_variant!(
                iface_calls.lock().pop().expect("iface call history should not be empty"),
                ClientIfaceCall::OnDisconnect { info } => info
            );
            assert_eq!(disconnect_info, mocked_disconnect_source);
        } else {
            // Still no messages, since the reconnect was successful.
            assert_variant!(
                test_helper.exec.run_until_stalled(&mut next_callback_fut),
                Poll::Pending
            );
            assert_variant!(test_helper.exec.run_until_stalled(&mut next_mcast), Poll::Pending);
        }

        // Metrics: no further messages expected, regardless of if reconnect is successful
        assert_variant!(test_helper.telemetry_receiver.try_next(), Err(_));
    }

    #[test]
    fn test_supplicant_sta_signal_report() {
        let (mut test_helper, mut test_fut) = setup_supplicant_test();
        let mut mcast_stream = get_nl80211_mcast(&test_helper.nl80211_proxy, "mlme");

        establish_open_connection(&mut test_helper, &mut test_fut, &mut mcast_stream);

        let mocked_signal_report =
            fidl_internal::SignalReportIndication { rssi_dbm: -35, snr_db: 20 };
        {
            let client_iface = test_helper.iface_manager.get_client_iface();
            let transaction_handle = client_iface.transaction_handle.lock();
            let control_handle = transaction_handle.as_ref().expect("No control handle found");
            control_handle
                .send_on_signal_report(&mocked_signal_report)
                .expect("Failed to send OnDisconnect");
        }

        assert_variant!(test_helper.exec.run_until_stalled(&mut test_fut), Poll::Pending);

        let iface_calls = test_helper.iface_manager.get_iface_call_history();
        let signal_report_ind = assert_variant!(
            iface_calls.lock().pop().expect("iface call history should not be empty"),
            ClientIfaceCall::OnSignalReport { ind } => ind
        );
        assert_eq!(signal_report_ind, mocked_signal_report);
    }

    #[test_case(true)]
    #[test_case(false)]
    #[fuchsia::test(add_test_attr = false)]
    fn test_supplicant_set_power_save_mode(desired: bool) {
        let (mut test_helper, mut test_fut) = setup_supplicant_test();

        let mut set_fut: fidl::client::QueryResponseFut<()> = test_helper
            .supplicant_sta_iface_proxy
            .set_power_save(fidl_fuchsia_wlan_wlanix::SupplicantStaIfaceSetPowerSaveRequest {
                enable: Some(desired),
                ..Default::default()
            });
        assert_variant!(test_helper.exec.run_until_stalled(&mut set_fut), Poll::Pending);
        assert_variant!(test_helper.exec.run_until_stalled(&mut test_fut), Poll::Pending);
        let iface_calls = test_helper.iface_manager.get_iface_call_history();
        assert_variant!(&iface_calls.lock().last().unwrap(), ClientIfaceCall::SetPowerSaveMode(setting) => assert_eq!(*setting, desired));
        assert_variant!(test_helper.exec.run_until_stalled(&mut set_fut), Poll::Ready(Ok(())));
    }

    #[test_case(true)]
    #[test_case(false)]
    #[fuchsia::test(add_test_attr = false)]
    fn test_supplicant_set_suspend_mode(desired: bool) {
        let (mut test_helper, mut test_fut) = setup_supplicant_test();

        let mut set_fut: fidl::client::QueryResponseFut<()> =
            test_helper.supplicant_sta_iface_proxy.set_suspend_mode_enabled(
                fidl_fuchsia_wlan_wlanix::SupplicantStaIfaceSetSuspendModeEnabledRequest {
                    enable: Some(desired),
                    ..Default::default()
                },
            );
        assert_variant!(test_helper.exec.run_until_stalled(&mut set_fut), Poll::Pending);
        assert_variant!(test_helper.exec.run_until_stalled(&mut test_fut), Poll::Pending);
        let iface_calls = test_helper.iface_manager.get_iface_call_history();
        assert_variant!(&iface_calls.lock().last().unwrap(), ClientIfaceCall::SetSuspendMode(setting)  => assert_eq!(*setting, desired));
        assert_variant!(test_helper.exec.run_until_stalled(&mut set_fut), Poll::Ready(Ok(())));
    }

    struct SupplicantTestHelper {
        _wlanix_proxy: fidl_wlanix::WlanixProxy,
        _supplicant_proxy: fidl_wlanix::SupplicantProxy,
        supplicant_sta_iface_proxy: fidl_wlanix::SupplicantStaIfaceProxy,
        nl80211_proxy: fidl_wlanix::Nl80211Proxy,
        supplicant_sta_network_proxy: fidl_wlanix::SupplicantStaNetworkProxy,
        supplicant_sta_iface_callback_stream: fidl_wlanix::SupplicantStaIfaceCallbackRequestStream,
        telemetry_receiver: mpsc::Receiver<TelemetryEvent>,
        iface_manager: Arc<TestIfaceManager>,

        // Note: keep the executor field last in the struct so it gets dropped last.
        exec: fasync::TestExecutor,
    }

    fn setup_supplicant_test() -> (SupplicantTestHelper, Pin<Box<impl Future<Output = ()>>>) {
        let mut exec = fasync::TestExecutor::new_with_fake_time();
        exec.set_fake_time(fasync::MonotonicInstant::from_nanos(0));

        let (wlanix_proxy, wlanix_stream) = create_proxy_and_stream::<fidl_wlanix::WlanixMarker>();
        let (supplicant_proxy, supplicant_server_end) =
            create_proxy::<fidl_wlanix::SupplicantMarker>();
        let result = wlanix_proxy.get_supplicant(fidl_wlanix::WlanixGetSupplicantRequest {
            supplicant: Some(supplicant_server_end),
            ..Default::default()
        });
        assert_variant!(result, Ok(()));

        let (nl80211_proxy, nl80211_server_end) = create_proxy::<fidl_wlanix::Nl80211Marker>();
        let result = wlanix_proxy.get_nl80211(fidl_wlanix::WlanixGetNl80211Request {
            nl80211: Some(nl80211_server_end),
            ..Default::default()
        });
        assert_variant!(result, Ok(()));

        let (supplicant_sta_iface_proxy, supplicant_sta_iface_server_end) =
            create_proxy::<fidl_wlanix::SupplicantStaIfaceMarker>();
        let result =
            supplicant_proxy.add_sta_interface(fidl_wlanix::SupplicantAddStaInterfaceRequest {
                iface: Some(supplicant_sta_iface_server_end),
                iface_name: Some("fake-iface-name".to_string()),
                ..Default::default()
            });
        assert_variant!(result, Ok(()));

        let (supplicant_sta_iface_callback_client_end, supplicant_sta_iface_callback_stream) =
            create_request_stream::<fidl_wlanix::SupplicantStaIfaceCallbackMarker>();
        let result = supplicant_sta_iface_proxy.register_callback(
            fidl_wlanix::SupplicantStaIfaceRegisterCallbackRequest {
                callback: Some(supplicant_sta_iface_callback_client_end),
                ..Default::default()
            },
        );
        assert_variant!(result, Ok(()));

        let (supplicant_sta_network_proxy, supplicant_sta_network_server_end) =
            create_proxy::<fidl_wlanix::SupplicantStaNetworkMarker>();
        let result = supplicant_sta_iface_proxy.add_network(
            fidl_wlanix::SupplicantStaIfaceAddNetworkRequest {
                network: Some(supplicant_sta_network_server_end),
                ..Default::default()
            },
        );
        assert_variant!(result, Ok(()));

        let wifi_state = Arc::new(Mutex::new(WifiState::default()));
        let iface_manager = Arc::new(TestIfaceManager::new_with_client());
        let (telemetry_sender, telemetry_receiver) = mpsc::channel::<TelemetryEvent>(100);
        let test_fut = serve_wlanix(
            wlanix_stream,
            wifi_state,
            Arc::clone(&iface_manager),
            TelemetrySender::new(telemetry_sender),
        );
        let mut test_fut = Box::pin(test_fut);
        assert_eq!(exec.run_until_stalled(&mut test_fut), Poll::Pending);

        let test_helper = SupplicantTestHelper {
            _wlanix_proxy: wlanix_proxy,
            _supplicant_proxy: supplicant_proxy,
            supplicant_sta_iface_proxy,
            nl80211_proxy,
            supplicant_sta_network_proxy,
            supplicant_sta_iface_callback_stream,
            telemetry_receiver,
            iface_manager,
            exec,
        };
        (test_helper, test_fut)
    }

    fn get_nl80211_mcast(
        nl80211_proxy: &fidl_wlanix::Nl80211Proxy,
        group: &str,
    ) -> fidl_wlanix::Nl80211MulticastRequestStream {
        let (mcast_client, mcast_stream) =
            create_request_stream::<fidl_wlanix::Nl80211MulticastMarker>();
        nl80211_proxy
            .get_multicast(fidl_wlanix::Nl80211GetMulticastRequest {
                group: Some(group.to_string()),
                multicast: Some(mcast_client),
                ..Default::default()
            })
            .expect("Failed to get multicast");
        mcast_stream
    }

    async fn next_mcast_message(
        stream: &mut fidl_wlanix::Nl80211MulticastRequestStream,
    ) -> GenlMessage<Nl80211> {
        let req = stream
            .next()
            .await
            .expect("Failed to request multicast message")
            .expect("Multicast message stream terminated");
        let mcast_msg = assert_variant!(req, fidl_wlanix::Nl80211MulticastRequest::Message {
            payload: fidl_wlanix::Nl80211MulticastMessageRequest {message: Some(m), .. }, ..} => m);
        expect_nl80211_message(&mcast_msg)
    }

    #[test]
    fn get_nl80211() {
        let mut exec = fasync::TestExecutor::new();
        let (proxy, stream) = create_proxy_and_stream::<fidl_wlanix::WlanixMarker>();
        let state = Arc::new(Mutex::new(WifiState::default()));
        let iface_manager = Arc::new(TestIfaceManager::new());
        let (telemetry_sender, _telemetry_receiver) = mpsc::channel::<TelemetryEvent>(100);
        let wlanix_fut =
            serve_wlanix(stream, state, iface_manager, TelemetrySender::new(telemetry_sender));
        let mut wlanix_fut = pin!(wlanix_fut);
        let (nl_proxy, nl_server) = create_proxy::<fidl_wlanix::Nl80211Marker>();
        proxy
            .get_nl80211(fidl_wlanix::WlanixGetNl80211Request {
                nl80211: Some(nl_server),
                ..Default::default()
            })
            .expect("Failed to get Nl80211");
        assert_variant!(exec.run_until_stalled(&mut wlanix_fut), Poll::Pending);
        assert!(!nl_proxy.is_closed());
    }

    #[test]
    fn unsupported_mcast_group() {
        let mut exec = fasync::TestExecutor::new();
        let (proxy, stream) = create_proxy_and_stream::<fidl_wlanix::Nl80211Marker>();

        let state = Arc::new(Mutex::new(WifiState::default()));
        let iface_manager = Arc::new(TestIfaceManager::new());
        let nl80211_fut = serve_nl80211(stream, state, iface_manager);
        let mut nl80211_fut = pin!(nl80211_fut);

        let mut mcast_stream = get_nl80211_mcast(&proxy, "doesnt_exist");
        assert_variant!(exec.run_until_stalled(&mut nl80211_fut), Poll::Pending);

        // The stream should immediately terminate.
        let next_mcast = mcast_stream.next();
        let mut next_mcast = pin!(next_mcast);
        assert_variant!(exec.run_until_stalled(&mut next_mcast), Poll::Ready(None));

        // serve_nl80211 should complete successfully.
        drop(proxy);
        assert_variant!(exec.run_until_stalled(&mut nl80211_fut), Poll::Ready(()));
    }

    #[test]
    fn unsupported_nl80211_command() {
        #[derive(Debug)]
        struct TestNl80211 {
            cmd: u8,
            attrs: Vec<Nl80211Attr>,
        }

        impl netlink_packet_generic::GenlFamily for TestNl80211 {
            fn family_name() -> &'static str {
                "nl80211"
            }

            fn command(&self) -> u8 {
                self.cmd
            }

            fn version(&self) -> u8 {
                1
            }
        }

        impl netlink_packet_utils::Emitable for TestNl80211 {
            fn emit(&self, buffer: &mut [u8]) {
                self.attrs.as_slice().emit(buffer)
            }

            fn buffer_len(&self) -> usize {
                self.attrs.as_slice().buffer_len()
            }
        }

        let mut exec = fasync::TestExecutor::new();
        let (proxy, stream) = create_proxy_and_stream::<fidl_wlanix::Nl80211Marker>();

        let state = Arc::new(Mutex::new(WifiState::default()));
        let iface_manager = Arc::new(TestIfaceManager::new_with_client());
        let nl80211_fut = serve_nl80211(stream, state, iface_manager);
        let mut nl80211_fut = pin!(nl80211_fut);

        // Create an nl80211 message with invalid command
        let genl_message = GenlMessage::from_payload(TestNl80211 { cmd: 255, attrs: vec![] });
        let mut buffer = vec![0u8; genl_message.buffer_len()];
        genl_message.serialize(&mut buffer);
        let invalid_message = fidl_wlanix::Nl80211Message {
            message_type: Some(fidl_wlanix::Nl80211MessageType::Message),
            payload: Some(buffer),
            ..Default::default()
        };

        let query_resp_fut = proxy.message(fidl_wlanix::Nl80211MessageRequest {
            message: Some(invalid_message),
            ..Default::default()
        });
        let mut query_resp_fut = pin!(query_resp_fut);
        assert_variant!(exec.run_until_stalled(&mut nl80211_fut), Poll::Pending);
        assert_variant!(
            exec.run_until_stalled(&mut query_resp_fut),
            Poll::Ready(Ok(Err(zx::sys::ZX_ERR_INTERNAL))),
        );
    }

    #[test]
    fn get_interface() {
        let mut exec = fasync::TestExecutor::new();
        let (proxy, stream) = create_proxy_and_stream::<fidl_wlanix::Nl80211Marker>();

        let state = Arc::new(Mutex::new(WifiState::default()));
        let iface_manager = Arc::new(TestIfaceManager::new_with_client());
        let nl80211_fut = serve_nl80211(stream, state, iface_manager);
        let mut nl80211_fut = pin!(nl80211_fut);

        let get_interface_message = build_nl80211_message(Nl80211Cmd::GetInterface, vec![]);
        let get_interface_fut = proxy.message(fidl_wlanix::Nl80211MessageRequest {
            message: Some(get_interface_message),
            ..Default::default()
        });
        let mut get_interface_fut = pin!(get_interface_fut);
        assert_variant!(exec.run_until_stalled(&mut nl80211_fut), Poll::Pending);
        let responses = assert_variant!(
            exec.run_until_stalled(&mut get_interface_fut),
            Poll::Ready(Ok(Ok(fidl_wlanix::Nl80211MessageResponse{responses: Some(r), ..}))) => r,
        );

        assert_eq!(responses.len(), 2);
        let message = expect_nl80211_message(&responses[0]);
        assert_eq!(message.payload.cmd, Nl80211Cmd::NewInterface);
        assert!(message.payload.attrs.iter().any(|attr| *attr
            == Nl80211Attr::IfaceIndex(ifaces::test_utils::FAKE_IFACE_RESPONSE.id.into())));
        assert!(message.payload.attrs.iter().any(
            |attr| *attr == Nl80211Attr::Mac(ifaces::test_utils::FAKE_IFACE_RESPONSE.sta_addr)
        ));
        assert_eq!(responses[1].message_type, Some(fidl_wlanix::Nl80211MessageType::Done));
    }

    #[test]
    fn get_station() {
        let mut exec = fasync::TestExecutor::new();
        let (proxy, stream) = create_proxy_and_stream::<fidl_wlanix::Nl80211Marker>();

        let state = Arc::new(Mutex::new(WifiState::default()));
        let iface_manager = Arc::new(TestIfaceManager::new_with_client());
        let nl80211_fut = serve_nl80211(stream, state, iface_manager);
        let mut nl80211_fut = pin!(nl80211_fut);

        let get_station_message = build_nl80211_message(
            Nl80211Cmd::GetStation,
            vec![Nl80211Attr::IfaceIndex(ifaces::test_utils::FAKE_IFACE_RESPONSE.id.into())],
        );
        let get_station_fut = proxy.message(fidl_wlanix::Nl80211MessageRequest {
            message: Some(get_station_message),
            ..Default::default()
        });

        let mut get_station_fut = pin!(get_station_fut);
        assert_variant!(exec.run_until_stalled(&mut nl80211_fut), Poll::Pending);
        let responses = assert_variant!(
            exec.run_until_stalled(&mut get_station_fut),
            Poll::Ready(Ok(Ok(fidl_wlanix::Nl80211MessageResponse{responses: Some(r), ..}))) => r,
        );
        assert_eq!(responses.len(), 1);
        assert_eq!(responses[0].message_type, Some(fidl_wlanix::Nl80211MessageType::Message));
    }

    #[test]
    fn trigger_scan() {
        let mut exec = fasync::TestExecutor::new();
        let (proxy, stream) = create_proxy_and_stream::<fidl_wlanix::Nl80211Marker>();

        let state = Arc::new(Mutex::new(WifiState::default()));
        let iface_manager = Arc::new(TestIfaceManager::new_with_client());
        let nl80211_fut = serve_nl80211(stream, state, iface_manager);
        let mut nl80211_fut = pin!(nl80211_fut);

        let mut mcast_stream = get_nl80211_mcast(&proxy, "scan");
        assert_variant!(exec.run_until_stalled(&mut nl80211_fut), Poll::Pending);

        let next_mcast = next_mcast_message(&mut mcast_stream);
        let mut next_mcast = pin!(next_mcast);
        assert_variant!(exec.run_until_stalled(&mut next_mcast), Poll::Pending);

        let trigger_scan_message = build_nl80211_message(
            Nl80211Cmd::TriggerScan,
            vec![Nl80211Attr::IfaceIndex(ifaces::test_utils::FAKE_IFACE_RESPONSE.id.into())],
        );
        let trigger_scan_fut = proxy.message(fidl_wlanix::Nl80211MessageRequest {
            message: Some(trigger_scan_message),
            ..Default::default()
        });

        let mut trigger_scan_fut = pin!(trigger_scan_fut);
        assert_variant!(exec.run_until_stalled(&mut nl80211_fut), Poll::Pending);
        let responses = assert_variant!(
            exec.run_until_stalled(&mut trigger_scan_fut),
            Poll::Ready(Ok(Ok(fidl_wlanix::Nl80211MessageResponse{responses: Some(r), ..}))) => r,
        );
        assert_eq!(responses.len(), 1);
        assert_eq!(responses[0].message_type, Some(fidl_wlanix::Nl80211MessageType::Ack));

        // With our faked scan results we expect an immediate multicast notification.
        let mcast_msg =
            assert_variant!(exec.run_until_stalled(&mut next_mcast), Poll::Ready(msg) => msg);
        assert_eq!(mcast_msg.payload.cmd, Nl80211Cmd::NewScanResults);
    }

    #[test]
    fn trigger_scan_no_iface_arg() {
        let mut exec = fasync::TestExecutor::new();
        let (proxy, stream) = create_proxy_and_stream::<fidl_wlanix::Nl80211Marker>();

        let state = Arc::new(Mutex::new(WifiState::default()));
        let iface_manager = Arc::new(TestIfaceManager::new_with_client());
        let nl80211_fut = serve_nl80211(stream, state, iface_manager);
        let mut nl80211_fut = pin!(nl80211_fut);

        let trigger_scan_message = build_nl80211_message(Nl80211Cmd::TriggerScan, vec![]);
        let trigger_scan_fut = proxy.message(fidl_wlanix::Nl80211MessageRequest {
            message: Some(trigger_scan_message),
            ..Default::default()
        });

        let mut trigger_scan_fut = pin!(trigger_scan_fut);
        assert_variant!(exec.run_until_stalled(&mut nl80211_fut), Poll::Pending);
        assert_variant!(
            exec.run_until_stalled(&mut trigger_scan_fut),
            Poll::Ready(Ok(Err(zx::sys::ZX_ERR_INVALID_ARGS))),
        );
    }

    #[test]
    fn trigger_scan_invalid_iface() {
        let mut exec = fasync::TestExecutor::new();
        let (proxy, stream) = create_proxy_and_stream::<fidl_wlanix::Nl80211Marker>();

        let state = Arc::new(Mutex::new(WifiState::default()));
        let iface_manager = Arc::new(TestIfaceManager::new_with_client());
        let nl80211_fut = serve_nl80211(stream, state, iface_manager);
        let mut nl80211_fut = pin!(nl80211_fut);

        let trigger_scan_message =
            build_nl80211_message(Nl80211Cmd::TriggerScan, vec![Nl80211Attr::IfaceIndex(123)]);
        let trigger_scan_fut = proxy.message(fidl_wlanix::Nl80211MessageRequest {
            message: Some(trigger_scan_message),
            ..Default::default()
        });

        let mut trigger_scan_fut = pin!(trigger_scan_fut);
        assert_variant!(exec.run_until_stalled(&mut nl80211_fut), Poll::Pending);
        assert_variant!(
            exec.run_until_stalled(&mut trigger_scan_fut),
            Poll::Ready(Ok(Err(zx::sys::ZX_ERR_NOT_FOUND))),
        );
    }

    #[test_case(Ok(ScanEnd::Cancelled); "Scan cancelled by user")]
    #[test_case(Err(format_err!("scan ended unexpectedly")); "Scan fails with error")]
    fn scan_abort(scan_result: Result<ScanEnd, Error>) {
        let mut exec = fasync::TestExecutor::new();
        let (proxy, stream) = create_proxy_and_stream::<fidl_wlanix::Nl80211Marker>();

        let state = Arc::new(Mutex::new(WifiState::default()));
        let (iface_manager, scan_end_sender) =
            TestIfaceManager::new_with_client_and_scan_end_sender();
        let iface_manager = Arc::new(iface_manager);
        let nl80211_fut = serve_nl80211(stream, state, iface_manager);
        let mut nl80211_fut = pin!(nl80211_fut);

        let mut mcast_stream = get_nl80211_mcast(&proxy, "scan");
        assert_variant!(exec.run_until_stalled(&mut nl80211_fut), Poll::Pending);

        let next_mcast = next_mcast_message(&mut mcast_stream);
        let mut next_mcast = pin!(next_mcast);
        assert_variant!(exec.run_until_stalled(&mut next_mcast), Poll::Pending);

        let trigger_scan_message = build_nl80211_message(
            Nl80211Cmd::TriggerScan,
            vec![Nl80211Attr::IfaceIndex(ifaces::test_utils::FAKE_IFACE_RESPONSE.id.into())],
        );
        let trigger_scan_fut = proxy.message(fidl_wlanix::Nl80211MessageRequest {
            message: Some(trigger_scan_message),
            ..Default::default()
        });
        let mut trigger_scan_fut = pin!(trigger_scan_fut);
        assert_variant!(exec.run_until_stalled(&mut nl80211_fut), Poll::Pending);
        assert_variant!(exec.run_until_stalled(&mut trigger_scan_fut), Poll::Ready(_));
        assert_variant!(exec.run_until_stalled(&mut next_mcast), Poll::Pending);

        // After ending the scan we expect wlanix to broadcast the scan abort.
        scan_end_sender.send(scan_result).expect("Failed to send scan result");
        assert_variant!(exec.run_until_stalled(&mut nl80211_fut), Poll::Pending);
        let message = assert_variant!(exec.run_until_stalled(&mut next_mcast), Poll::Ready(message) => message);
        assert_eq!(message.payload.cmd, Nl80211Cmd::ScanAborted);
    }

    #[test]
    fn get_scan_results() {
        let mut exec = fasync::TestExecutor::new();
        let (proxy, stream) = create_proxy_and_stream::<fidl_wlanix::Nl80211Marker>();

        let state = Arc::new(Mutex::new(WifiState::default()));
        let iface_manager = Arc::new(TestIfaceManager::new_with_client());
        let nl80211_fut = serve_nl80211(stream, state, iface_manager);
        let mut nl80211_fut = pin!(nl80211_fut);

        let get_scan_message = build_nl80211_message(
            Nl80211Cmd::GetScan,
            vec![Nl80211Attr::IfaceIndex(ifaces::test_utils::FAKE_IFACE_RESPONSE.id.into())],
        );
        let get_scan_fut = proxy.message(fidl_wlanix::Nl80211MessageRequest {
            message: Some(get_scan_message),
            ..Default::default()
        });

        let mut get_scan_fut = pin!(get_scan_fut);
        assert_variant!(exec.run_until_stalled(&mut nl80211_fut), Poll::Pending);
        let responses = assert_variant!(
            exec.run_until_stalled(&mut get_scan_fut),
            Poll::Ready(Ok(Ok(fidl_wlanix::Nl80211MessageResponse{responses: Some(r), ..}))) => r,
        );
        assert_eq!(responses.len(), 2);
        assert_eq!(responses[0].message_type, Some(fidl_wlanix::Nl80211MessageType::Message));
        assert_eq!(responses[1].message_type, Some(fidl_wlanix::Nl80211MessageType::Done));
    }

    #[test]
    fn get_scan_results_no_iface_args() {
        let mut exec = fasync::TestExecutor::new();
        let (proxy, stream) = create_proxy_and_stream::<fidl_wlanix::Nl80211Marker>();

        let state = Arc::new(Mutex::new(WifiState::default()));
        let iface_manager = Arc::new(TestIfaceManager::new_with_client());
        let nl80211_fut = serve_nl80211(stream, state, iface_manager);
        let mut nl80211_fut = pin!(nl80211_fut);

        let get_scan_message = build_nl80211_message(Nl80211Cmd::GetScan, vec![]);
        let get_scan_fut = proxy.message(fidl_wlanix::Nl80211MessageRequest {
            message: Some(get_scan_message),
            ..Default::default()
        });

        let mut get_scan_fut = pin!(get_scan_fut);
        assert_variant!(exec.run_until_stalled(&mut nl80211_fut), Poll::Pending);
        assert_variant!(
            exec.run_until_stalled(&mut get_scan_fut),
            Poll::Ready(Ok(Err(zx::sys::ZX_ERR_INVALID_ARGS))),
        );
    }

    #[test]
    fn get_reg() {
        let mut exec = fasync::TestExecutor::new();
        let (proxy, stream) = create_proxy_and_stream::<fidl_wlanix::Nl80211Marker>();

        let state = Arc::new(Mutex::new(WifiState::default()));
        let iface_manager = Arc::new(TestIfaceManager::new_with_client());
        let nl80211_fut = serve_nl80211(stream, state, iface_manager);
        let mut nl80211_fut = pin!(nl80211_fut);

        let get_reg_message =
            build_nl80211_message(Nl80211Cmd::GetReg, vec![Nl80211Attr::Wiphy(123)]);
        let get_reg_fut = proxy.message(fidl_wlanix::Nl80211MessageRequest {
            message: Some(get_reg_message),
            ..Default::default()
        });

        let mut get_reg_fut = pin!(get_reg_fut);
        assert_variant!(exec.run_until_stalled(&mut nl80211_fut), Poll::Pending);
        let responses = assert_variant!(
            exec.run_until_stalled(&mut get_reg_fut),
            Poll::Ready(Ok(Ok(fidl_wlanix::Nl80211MessageResponse{responses: Some(r), ..}))) => r,
        );
        assert_eq!(responses.len(), 1);
        let message = expect_nl80211_message(&responses[0]);
        assert_eq!(message.payload.cmd, Nl80211Cmd::GetReg);
        assert!(message
            .payload
            .attrs
            .iter()
            .any(|attr| *attr == Nl80211Attr::RegulatoryRegionAlpha2([b'W', b'W'])));
    }
}
