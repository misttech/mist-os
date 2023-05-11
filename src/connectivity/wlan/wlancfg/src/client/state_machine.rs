// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fasync::TimeoutExt;

use {
    crate::{
        client::{bss_selection, types},
        config_management::{PastConnectionData, SavedNetworksManagerApi},
        mode_management::{Defect, IfaceFailure},
        telemetry::{DisconnectInfo, TelemetryEvent, TelemetrySender},
        util::{
            listener::{
                ClientListenerMessageSender, ClientNetworkState, ClientStateUpdate,
                Message::NotifyListeners,
            },
            state_machine::{self, ExitReason, IntoStateExt},
        },
    },
    anyhow::format_err,
    fidl::endpoints::create_proxy,
    fidl_fuchsia_wlan_ieee80211 as fidl_ieee80211, fidl_fuchsia_wlan_internal as fidl_internal,
    fidl_fuchsia_wlan_policy as fidl_policy, fidl_fuchsia_wlan_sme as fidl_sme,
    fuchsia_async::{self as fasync, DurationExt},
    fuchsia_zircon as zx,
    futures::{
        channel::{mpsc, oneshot},
        future::FutureExt,
        select,
        stream::{self, StreamExt, TryStreamExt},
    },
    std::{
        convert::{Infallible, TryFrom},
        sync::Arc,
    },
    tracing::{debug, error, info, warn},
    wlan_common::{
        bss::BssDescription, energy::DecibelMilliWatt, sequestered::Sequestered,
        stats::SignalStrengthAverage,
    },
};

const MAX_CONNECTION_ATTEMPTS: u8 = 4; // arbitrarily chosen until we have some data
const CONNECT_TIMEOUT: zx::Duration = zx::Duration::from_seconds(30);
type State = state_machine::State<ExitReason>;
type ReqStream = stream::Fuse<mpsc::Receiver<ManualRequest>>;

pub trait ClientApi {
    fn connect(&mut self, selection: types::ConnectSelection) -> Result<(), anyhow::Error>;
    fn disconnect(
        &mut self,
        reason: types::DisconnectReason,
        responder: oneshot::Sender<()>,
    ) -> Result<(), anyhow::Error>;

    /// Queries the liveness of the channel used to control the client state machine.  If the
    /// channel is not alive, this indicates that the client state machine has exited.
    fn is_alive(&self) -> bool;
}

pub struct Client {
    req_sender: mpsc::Sender<ManualRequest>,
}

impl Client {
    pub fn new(req_sender: mpsc::Sender<ManualRequest>) -> Self {
        Self { req_sender }
    }
}

impl ClientApi for Client {
    fn connect(&mut self, selection: types::ConnectSelection) -> Result<(), anyhow::Error> {
        self.req_sender
            .try_send(ManualRequest::Connect(selection))
            .map_err(|e| format_err!("failed to send connect selection: {:?}", e))
    }

    fn disconnect(
        &mut self,
        reason: types::DisconnectReason,
        responder: oneshot::Sender<()>,
    ) -> Result<(), anyhow::Error> {
        self.req_sender
            .try_send(ManualRequest::Disconnect((reason, responder)))
            .map_err(|e| format_err!("failed to send disconnect request: {:?}", e))
    }

    fn is_alive(&self) -> bool {
        !self.req_sender.is_closed()
    }
}

pub enum ManualRequest {
    Connect(types::ConnectSelection),
    Disconnect((types::DisconnectReason, oneshot::Sender<()>)),
}

fn send_listener_state_update(
    sender: &ClientListenerMessageSender,
    network_update: Option<ClientNetworkState>,
) {
    let mut networks = vec![];
    if let Some(network) = network_update {
        networks.push(network)
    }

    let updates =
        ClientStateUpdate { state: fidl_policy::WlanClientState::ConnectionsEnabled, networks };
    match sender.clone().unbounded_send(NotifyListeners(updates)) {
        Ok(_) => (),
        Err(e) => error!("failed to send state update: {:?}", e),
    };
}

pub async fn serve(
    iface_id: u16,
    proxy: fidl_sme::ClientSmeProxy,
    sme_event_stream: fidl_sme::ClientSmeEventStream,
    req_stream: mpsc::Receiver<ManualRequest>,
    update_sender: ClientListenerMessageSender,
    saved_networks_manager: Arc<dyn SavedNetworksManagerApi>,
    connect_selection: Option<types::ConnectSelection>,
    telemetry_sender: TelemetrySender,
    stats_sender: ConnectionStatsSender,
    defect_sender: mpsc::UnboundedSender<Defect>,
) {
    let next_network = connect_selection
        .map(|selection| ConnectingOptions { connect_selection: selection, attempt_counter: 0 });
    let disconnect_options = DisconnectingOptions {
        disconnect_responder: None,
        previous_network: None,
        next_network,
        reason: types::DisconnectReason::Startup,
    };
    let common_options = CommonStateOptions {
        proxy,
        req_stream: req_stream.fuse(),
        update_sender,
        saved_networks_manager,
        telemetry_sender,
        iface_id,
        stats_sender,
        defect_sender,
    };
    let state_machine =
        disconnecting_state(common_options, disconnect_options).into_state_machine();
    let removal_watcher = sme_event_stream.map_ok(|_| ()).try_collect::<()>();
    select! {
        state_machine = state_machine.fuse() => {
            match state_machine {
                Ok(v) => {
                    // This should never happen because the `Infallible` type should be impossible
                    // to create.
                    let _: Infallible = v;
                    unreachable!()
                }
                Err(ExitReason(Err(e))) => error!("Client state machine for iface #{} terminated with an error: {:?}",
                    iface_id, e),
                Err(ExitReason(Ok(_))) => info!("Client state machine for iface #{} exited gracefully",
                    iface_id,),
            }
        }
        removal_watcher = removal_watcher.fuse() => if let Err(e) = removal_watcher {
            info!("Error reading from Client SME channel of iface #{}: {:?}",
                iface_id, e);
        },
    }
}

/// Common parameters passed to all states
struct CommonStateOptions {
    proxy: fidl_sme::ClientSmeProxy,
    req_stream: ReqStream,
    update_sender: ClientListenerMessageSender,
    saved_networks_manager: Arc<dyn SavedNetworksManagerApi>,
    telemetry_sender: TelemetrySender,
    iface_id: u16,
    /// Used to send periodic connection stats used to determine whether or not to roam.
    stats_sender: mpsc::UnboundedSender<PeriodicConnectionStats>,
    defect_sender: mpsc::UnboundedSender<Defect>,
}

/// Data that is periodically gathered for determining whether to roam
pub struct PeriodicConnectionStats {
    /// ID and BSSID of the current connection, to exclude it when comparing available networks.
    pub id: types::NetworkIdentifier,
    /// Iface ID that the connection is on.
    pub iface_id: u16,
    pub quality_data: bss_selection::BssQualityData,
}

pub type ConnectionStatsSender = mpsc::UnboundedSender<PeriodicConnectionStats>;
pub type ConnectionStatsReceiver = mpsc::UnboundedReceiver<PeriodicConnectionStats>;

fn handle_none_request() -> Result<State, ExitReason> {
    return Err(ExitReason(Err(format_err!("The stream of requests ended unexpectedly"))));
}

// These functions were introduced to resolve the following error:
// ```
// error[E0391]: cycle detected when evaluating trait selection obligation
// `impl core::future::future::Future: std::marker::Send`
// ```
// which occurs when two functions that return an `impl Trait` call each other
// in a cycle. (e.g. this case `connecting_state` calling `disconnecting_state`,
// which calls `connecting_state`)
fn to_disconnecting_state(
    common_options: CommonStateOptions,
    disconnecting_options: DisconnectingOptions,
) -> State {
    disconnecting_state(common_options, disconnecting_options).into_state()
}
fn to_connecting_state(
    common_options: CommonStateOptions,
    connecting_options: ConnectingOptions,
) -> State {
    connecting_state(common_options, connecting_options).into_state()
}

struct DisconnectingOptions {
    disconnect_responder: Option<oneshot::Sender<()>>,
    /// Information about the previously connected network, if there was one. Used to send out
    /// listener updates.
    previous_network: Option<(types::NetworkIdentifier, types::DisconnectStatus)>,
    /// Configuration for the next network to connect to, after the disconnect is complete. If not
    /// present, the state machine will proceed to IDLE.
    next_network: Option<ConnectingOptions>,
    reason: types::DisconnectReason,
}
/// The DISCONNECTING state requests an SME disconnect, then transitions to either:
/// - the CONNECTING state if options.next_network is present
/// - exit otherwise
async fn disconnecting_state(
    common_options: CommonStateOptions,
    options: DisconnectingOptions,
) -> Result<State, ExitReason> {
    // Log a message with the disconnect reason
    match options.reason {
        types::DisconnectReason::FailedToConnect
        | types::DisconnectReason::Startup
        | types::DisconnectReason::DisconnectDetectedFromSme => {
            // These are either just noise or have separate logging, so keep the level at debug.
            debug!("Disconnected due to {:?}", options.reason);
        }
        reason => {
            info!("Disconnected due to {:?}", reason);
        }
    }

    // TODO(fxbug.dev/53505): either make this fire-and-forget in the SME, or spawn a thread for this,
    // so we don't block on it
    common_options
        .proxy
        .disconnect(types::convert_to_sme_disconnect_reason(options.reason))
        .await
        .map_err(|e| {
            ExitReason(Err(format_err!("Failed to send command to wlanstack: {:?}", e)))
        })?;

    // Notify listeners if a disconnect request was sent, or ensure that listeners know client
    // connections are enabled.
    let networks =
        options.previous_network.map(|(network_identifier, status)| ClientNetworkState {
            id: network_identifier,
            state: types::ConnectionState::Disconnected,
            status: Some(status),
        });
    send_listener_state_update(&common_options.update_sender, networks);

    // Notify the caller that disconnect was sent to the SME once the final disconnected update has
    // been sent.  This ensures that there will not be a race when the IfaceManager sends out a
    // ConnectionsDisabled update.
    match options.disconnect_responder {
        Some(responder) => responder.send(()).unwrap_or_else(|_| ()),
        None => (),
    }

    // Transition to next state
    match options.next_network {
        Some(next_network) => Ok(to_connecting_state(common_options, next_network)),
        None => Err(ExitReason(Ok(()))),
    }
}

fn connect_txn_event_name(event: &fidl_sme::ConnectTransactionEvent) -> &'static str {
    match event {
        fidl_sme::ConnectTransactionEvent::OnConnectResult { .. } => "OnConnectResult",
        fidl_sme::ConnectTransactionEvent::OnDisconnect { .. } => "OnDisconnect",
        fidl_sme::ConnectTransactionEvent::OnSignalReport { .. } => "OnSignalReport",
        fidl_sme::ConnectTransactionEvent::OnChannelSwitched { .. } => "OnChannelSwitched",
    }
}

struct ConnectingOptions {
    connect_selection: types::ConnectSelection,
    /// Count of previous consecutive failed connection attempts to this same network.
    attempt_counter: u8,
}

async fn handle_connecting_error_and_retry(
    common_options: CommonStateOptions,
    options: ConnectingOptions,
) -> Result<State, ExitReason> {
    // Check if the limit for connection attempts to this network has been
    // exceeded.
    let new_attempt_count = options.attempt_counter + 1;
    if new_attempt_count >= MAX_CONNECTION_ATTEMPTS {
        info!("Exceeded maximum connection attempts, will not retry");
        send_listener_state_update(
            &common_options.update_sender,
            Some(ClientNetworkState {
                id: options.connect_selection.target.network,
                state: types::ConnectionState::Failed,
                status: Some(types::DisconnectStatus::ConnectionFailed),
            }),
        );
        return Err(ExitReason(Ok(())));
    } else {
        // Limit not exceeded, retry after backing off.
        let backoff_time = 400_i64 * i64::from(new_attempt_count);
        info!("Will attempt to reconnect after {}ms backoff", backoff_time);
        fasync::Timer::new(zx::Duration::from_millis(backoff_time).after_now()).await;

        let next_connecting_options = ConnectingOptions {
            connect_selection: types::ConnectSelection {
                reason: types::ConnectReason::RetryAfterFailedConnectAttempt,
                ..options.connect_selection
            },
            attempt_counter: new_attempt_count,
        };
        let disconnecting_options = DisconnectingOptions {
            disconnect_responder: None,
            previous_network: None,
            next_network: Some(next_connecting_options),
            reason: types::DisconnectReason::FailedToConnect,
        };
        return Ok(to_disconnecting_state(common_options, disconnecting_options));
    }
}

/// Wait until stream returns an OnConnectResult event or None. Ignore other event types.
async fn wait_for_connect_result(
    mut stream: fidl_sme::ConnectTransactionEventStream,
) -> Result<fidl_sme::ConnectResult, ExitReason> {
    loop {
        let stream_fut = stream.try_next();
        match stream_fut.await.map_err(|e| {
            ExitReason(Err(format_err!("Failed to receive connect result from sme: {:?}", e)))
        })? {
            Some(fidl_sme::ConnectTransactionEvent::OnConnectResult { result }) => {
                return Ok(result)
            }
            Some(other) => {
                info!(
                    "Expected ConnectTransactionEvent::OnConnectResult, got {}. Ignoring.",
                    connect_txn_event_name(&other)
                );
            }
            None => {
                return Err(ExitReason(Err(format_err!(
                    "Server closed the ConnectTransaction channel before sending a response"
                ))));
            }
        };
    }
}

/// The CONNECTING state requests an SME connect. It handles the SME connect response:
/// - for a successful connection, transition to CONNECTED state
/// - for a failed connection, retry connection by passing a next_network to the
///       DISCONNECTING state, as long as there haven't been too many connection attempts
/// During this time, incoming ManualRequests are also monitored for:
/// - duplicate connect requests are deduped
/// - different connect requests are serviced by passing a next_network to the DISCONNECTING state
/// - disconnect requests cause a transition to DISCONNECTING state
async fn connecting_state<'a>(
    common_options: CommonStateOptions,
    options: ConnectingOptions,
) -> Result<State, ExitReason> {
    debug!("Entering connecting state");

    if options.attempt_counter > 0 {
        info!(
            "Retrying connection, {} attempts remaining",
            MAX_CONNECTION_ATTEMPTS - options.attempt_counter
        );
    }

    // Send a "Connecting" update to listeners, unless this is a retry
    if options.attempt_counter == 0 {
        send_listener_state_update(
            &common_options.update_sender,
            Some(ClientNetworkState {
                id: options.connect_selection.target.network.clone(),
                state: types::ConnectionState::Connecting,
                status: None,
            }),
        );
    };

    // Release the sequestered BSS description. While considered a "black box" elsewhere, the state
    // machine uses this by design to construct its AP state and to report telemetry.
    let bss_description =
        Sequestered::release(options.connect_selection.target.bss.bss_description.clone());
    let ap_state = types::ApState::from(
        BssDescription::try_from(bss_description.clone()).map_err(|error| {
            // This only occurs if an invalid `BssDescription` is received from SME, which should
            // never happen.
            ExitReason(Err(
                format_err!("Failed to convert BSS description from FIDL: {:?}", error,),
            ))
        })?,
    );

    // Send a connect request to the SME.
    let (connect_txn, remote) = create_proxy()
        .map_err(|e| ExitReason(Err(format_err!("Failed to create proxy: {:?}", e))))?;
    let sme_connect_request = fidl_sme::ConnectRequest {
        ssid: options.connect_selection.target.network.ssid.to_vec(),
        bss_description,
        multiple_bss_candidates: options.connect_selection.target.network_has_multiple_bss,
        authentication: options.connect_selection.target.authenticator.clone().into(),
        deprecated_scan_type: fidl_fuchsia_wlan_common::ScanType::Active,
    };
    common_options.proxy.connect(&sme_connect_request, Some(remote)).map_err(|e| {
        ExitReason(Err(format_err!("Failed to send command to wlanstack: {:?}", e)))
    })?;
    let start_time = fasync::Time::now();

    // Wait for connect result or timeout.
    let stream = connect_txn.take_event_stream();
    let sme_result = wait_for_connect_result(stream)
        .on_timeout(CONNECT_TIMEOUT, || {
            Err(ExitReason(Err(format_err!("Timed out waiting for connect result from SME."))))
        })
        .await?;

    // Report the connect result to the saved networks manager.
    common_options
        .saved_networks_manager
        .record_connect_result(
            options.connect_selection.target.network.clone(),
            &options.connect_selection.target.credential,
            ap_state.original().bssid,
            sme_result,
            options.connect_selection.target.bss.observation,
        )
        .await;

    // Log the connect result for metrics.
    common_options.telemetry_sender.send(TelemetryEvent::ConnectResult {
        ap_state: ap_state.clone(),
        result: sme_result,
        policy_connect_reason: Some(options.connect_selection.reason),
        multiple_bss_candidates: options.connect_selection.target.network_has_multiple_bss,
        iface_id: common_options.iface_id,
    });

    match (sme_result.code, sme_result.is_credential_rejected) {
        (fidl_ieee80211::StatusCode::Success, _) => {
            info!("Successfully connected to network");
            send_listener_state_update(
                &common_options.update_sender,
                Some(ClientNetworkState {
                    id: options.connect_selection.target.network.clone(),
                    state: types::ConnectionState::Connected,
                    status: None,
                }),
            );
            let connected_options = ConnectedOptions {
                currently_fulfilled_connection: options.connect_selection.clone(),
                connect_txn_stream: connect_txn.take_event_stream(),
                ap_state: Box::new(ap_state),
                multiple_bss_candidates: options.connect_selection.target.network_has_multiple_bss,
                connection_attempt_time: start_time,
                time_to_connect: fasync::Time::now() - start_time,
            };
            return Ok(connected_state(common_options, connected_options).into_state());
        }
        (code, true) => {
            info!("Failed to connect: {:?}. Will not retry because of credential error.", code);
            send_listener_state_update(
                &common_options.update_sender,
                Some(ClientNetworkState {
                    id: options.connect_selection.target.network,
                    state: types::ConnectionState::Failed,
                    status: Some(types::DisconnectStatus::CredentialsFailed),
                }),
            );
            return Err(ExitReason(Ok(())));
        }
        (code, _) => {
            info!("Failed to connect: {:?}", code);

            // Defects should be logged for connection failures that are not due to
            // bad credentials.
            if let Err(e) = common_options.defect_sender.unbounded_send(Defect::Iface(
                IfaceFailure::ConnectionFailure { iface_id: common_options.iface_id },
            )) {
                warn!("Failed to log connection failure: {}", e);
            }

            return handle_connecting_error_and_retry(common_options, options).await;
        }
    };
}

struct ConnectedOptions {
    // Keep track of the BSSID we are connected in order to record connection information for
    // future network selection.
    ap_state: Box<types::ApState>,
    multiple_bss_candidates: bool,
    currently_fulfilled_connection: types::ConnectSelection,
    connect_txn_stream: fidl_sme::ConnectTransactionEventStream,
    /// Time at which connect was first attempted, historical data for network scoring.
    pub connection_attempt_time: fasync::Time,
    /// Duration from connection attempt to success, historical data for network scoring.
    pub time_to_connect: zx::Duration,
}

/// The CONNECTED state monitors the SME status. It handles the SME status response:
/// - if still connected to the correct network, no action
/// - if disconnected, retry connection by passing a next_network to the
///       DISCONNECTING state
/// During this time, incoming ManualRequests are also monitored for:
/// - duplicate connect requests are deduped
/// - different connect requests are serviced by passing a next_network to the DISCONNECTING state
/// - disconnect requests cause a transition to DISCONNECTING state
async fn connected_state(
    mut common_options: CommonStateOptions,
    mut options: ConnectedOptions,
) -> Result<State, ExitReason> {
    debug!("Entering connected state");
    let mut connect_start_time = fasync::Time::now();

    // Initialize connection data
    let past_connections = common_options
        .saved_networks_manager
        .get_past_connections(
            &options.currently_fulfilled_connection.target.network,
            &options.currently_fulfilled_connection.target.credential,
            &options.ap_state.original().bssid,
        )
        .await;
    let mut bss_quality_data = bss_selection::BssQualityData::new(
        bss_selection::SignalData::new(
            options.ap_state.tracked.signal.rssi_dbm,
            options.ap_state.tracked.signal.snr_db,
            bss_selection::EWMA_SMOOTHING_FACTOR,
            bss_selection::EWMA_VELOCITY_SMOOTHING_FACTOR,
        ),
        options.ap_state.tracked.channel,
        past_connections,
    );

    // Keep track of the connection's average signal strength for future scoring.
    let mut avg_rssi = SignalStrengthAverage::new();

    loop {
        select! {
            event = options.connect_txn_stream.next() => match event {
                Some(Ok(event)) => {
                    let is_sme_idle = match event {
                        fidl_sme::ConnectTransactionEvent::OnDisconnect { info: fidl_info } => {
                            // Log a disconnect in Cobalt
                            let now = fasync::Time::now();
                            let info = DisconnectInfo {
                                connected_duration: now - connect_start_time,
                                is_sme_reconnecting: fidl_info.is_sme_reconnecting,
                                disconnect_source: fidl_info.disconnect_source,
                                previous_connect_reason: options.currently_fulfilled_connection.reason,
                                ap_state: (*options.ap_state).clone(),
                            };
                            common_options.telemetry_sender.send(TelemetryEvent::Disconnected { track_subsequent_downtime: true, info });

                            // Record data about the connection and disconnect for future network
                            // selection.
                            record_disconnect(
                                &common_options,
                                &options,
                                connect_start_time,
                                types::DisconnectReason::DisconnectDetectedFromSme,
                                bss_quality_data.signal_data,
                            ).await;

                            !fidl_info.is_sme_reconnecting
                            }
                        fidl_sme::ConnectTransactionEvent::OnConnectResult { result } => {
                            let connected = result.code == fidl_ieee80211::StatusCode::Success;
                            if connected {
                                // This OnConnectResult should be for reconnecting to the same AP,
                                // so keep the same SignalData but reset the connect start time
                                // to track as a new connection.
                                connect_start_time = fasync::Time::now();
                            }
                            common_options.telemetry_sender.send(TelemetryEvent::ConnectResult {
                                iface_id: common_options.iface_id,
                                result,
                                policy_connect_reason: None,
                                // It's not necessarily true that there are still multiple BSS
                                // candidates in the network at this point in time, but we use the
                                // heuristic that if previously there were multiple BSS's, then
                                // it likely remains the same.
                                multiple_bss_candidates: options.multiple_bss_candidates,
                                ap_state: (*options.ap_state).clone(),
                            });
                            !connected
                        }
                        fidl_sme::ConnectTransactionEvent::OnSignalReport { ind } => {
                            // Update connection data
                            options.ap_state.tracked.signal.rssi_dbm = ind.rssi_dbm;
                            options.ap_state.tracked.signal.snr_db = ind.snr_db;
                            bss_quality_data.signal_data.update_with_new_measurement(ind.rssi_dbm, ind.snr_db);
                            avg_rssi.add(DecibelMilliWatt(ind.rssi_dbm));
                            let current_connection = &options.currently_fulfilled_connection.target;
                            handle_connection_stats(
                                &mut common_options.telemetry_sender,
                                &mut common_options.stats_sender,
                                common_options.iface_id,
                                current_connection.network.clone(),
                                ind,
                                bss_quality_data.clone()
                            ).await;

                            // Evaluate current BSS, and determine if roaming future should be
                            // triggered.
                            let (_bss_score, roam_reasons) = bss_selection::evaluate_current_bss(bss_quality_data.clone());
                            if !roam_reasons.is_empty() {
                                common_options.telemetry_sender.send(TelemetryEvent::RoamingScan);
                                // TODO(haydennix): Trigger roaming future, which must be idempotent
                                // since repeated calls are likely.
                            }
                            false
                        }
                        fidl_sme::ConnectTransactionEvent::OnChannelSwitched { info } => {
                            options.ap_state.tracked.channel.primary = info.new_channel;
                            common_options.telemetry_sender.send(TelemetryEvent::OnChannelSwitched { info });
                            false
                        }
                    };

                    if is_sme_idle {
                        info!("Idle sme detected.");
                        let options = DisconnectingOptions {
                            disconnect_responder: None,
                            previous_network: Some((options.currently_fulfilled_connection.target.network.clone(), types::DisconnectStatus::ConnectionFailed)),
                            next_network: None,
                            reason: types::DisconnectReason::DisconnectDetectedFromSme,
                        };
                        return Ok(disconnecting_state(common_options, options).into_state());
                    }
                }
                _ => {
                    info!("SME dropped ConnectTransaction channel. Exiting state machine");
                    return Err(ExitReason(Err(format_err!("Failed to receive ConnectTransactionEvent for SME status"))));
                }
            },
            req = common_options.req_stream.next() => {
                let now = fasync::Time::now();
                match req {
                    Some(ManualRequest::Disconnect((reason, responder))) => {
                        debug!("Disconnect requested");
                        record_disconnect(
                            &common_options,
                            &options,
                            connect_start_time,
                            reason,
                            bss_quality_data.signal_data
                        ).await;
                        let ap_state = options.ap_state;
                        let info = DisconnectInfo {
                            connected_duration: now - connect_start_time,
                            is_sme_reconnecting: false,
                            disconnect_source: fidl_sme::DisconnectSource::User(types::convert_to_sme_disconnect_reason(reason)),
                            previous_connect_reason: options.currently_fulfilled_connection.reason,
                            ap_state: *ap_state,
                        };
                        let options = DisconnectingOptions {
                            disconnect_responder: Some(responder),
                            previous_network: Some((options.currently_fulfilled_connection.target.network, types::DisconnectStatus::ConnectionStopped)),
                            next_network: None,
                            reason,
                        };
                        common_options.telemetry_sender.send(TelemetryEvent::Disconnected { track_subsequent_downtime: false, info });
                        return Ok(disconnecting_state(common_options, options).into_state());
                    }
                    Some(ManualRequest::Connect(new_connect_selection)) => {
                        // Check if it's the same network as we're currently connected to. If yes, reply immediately
                        if new_connect_selection.target.network == options.currently_fulfilled_connection.target.network {
                            info!("Received connection request for current network, deduping");
                        } else {
                            let disconnect_reason = convert_manual_connect_to_disconnect_reason(&new_connect_selection.reason).unwrap_or_else(|_| {
                                error!("Unexpected connection reason: {:?}", new_connect_selection.reason);
                                types::DisconnectReason::Unknown
                            });

                            record_disconnect(
                                &common_options,
                                &options,
                                connect_start_time,
                                disconnect_reason,
                                bss_quality_data.signal_data
                            ).await;


                            let next_connecting_options = ConnectingOptions {
                                connect_selection: new_connect_selection.clone(),
                                attempt_counter: 0,
                            };
                            let ap_state = options.ap_state;
                            let info = DisconnectInfo {
                                connected_duration: now - connect_start_time,
                                is_sme_reconnecting: false,
                                disconnect_source: fidl_sme::DisconnectSource::User(types::convert_to_sme_disconnect_reason(disconnect_reason)),
                                previous_connect_reason: options.currently_fulfilled_connection.reason,
                                ap_state: *ap_state,
                            };
                            let options = DisconnectingOptions {
                                disconnect_responder: None,
                                previous_network: Some((options.currently_fulfilled_connection.target.network, types::DisconnectStatus::ConnectionStopped)),
                                next_network: Some(next_connecting_options),
                                reason: disconnect_reason,
                            };
                            info!("Connection to new network requested, disconnecting from current network");
                            common_options.telemetry_sender.send(TelemetryEvent::Disconnected { track_subsequent_downtime: false, info });
                            return Ok(disconnecting_state(common_options, options).into_state())
                        }
                    }
                    None => return handle_none_request(),
                };
            }
        }
    }
}

/// Update IfaceManager with the updated connection quality data.
async fn handle_connection_stats(
    telemetry_sender: &mut TelemetrySender,
    stats_sender: &mut ConnectionStatsSender,
    iface_id: u16,
    id: types::NetworkIdentifier,
    ind: fidl_internal::SignalReportIndication,
    bss_quality_data: bss_selection::BssQualityData,
) {
    let connection_stats =
        PeriodicConnectionStats { id, iface_id, quality_data: bss_quality_data.clone() };
    stats_sender.unbounded_send(connection_stats).unwrap_or_else(|e| {
        error!("Failed to send periodic connection stats from the connected state: {}", e);
    });
    // Send RSSI and RSSI velocity metrics
    telemetry_sender.send(TelemetryEvent::OnSignalReport {
        ind,
        rssi_velocity: bss_quality_data.signal_data.ewma_rssi_velocity.get().round() as i8,
    });
}

async fn record_disconnect(
    common_options: &CommonStateOptions,
    options: &ConnectedOptions,
    connect_start_time: fasync::Time,
    reason: types::DisconnectReason,
    signal_data: bss_selection::SignalData,
) {
    let curr_time = fasync::Time::now();
    let uptime = curr_time - connect_start_time;
    let data = PastConnectionData::new(
        options.ap_state.original().bssid,
        options.connection_attempt_time,
        options.time_to_connect,
        curr_time,
        uptime,
        reason,
        signal_data,
        // TODO: record average phy rate over connection once available
        0,
    );
    common_options
        .saved_networks_manager
        .record_disconnect(
            &options.currently_fulfilled_connection.target.network.clone(),
            &options.currently_fulfilled_connection.target.credential,
            data,
        )
        .await;
}

/// Get the disconnect reason corresponding to the connect reason. Return an error if the connect
/// reason does not correspond to a manual connect.
pub fn convert_manual_connect_to_disconnect_reason(
    reason: &types::ConnectReason,
) -> Result<types::DisconnectReason, ()> {
    match reason {
        types::ConnectReason::FidlConnectRequest => Ok(types::DisconnectReason::FidlConnectRequest),
        types::ConnectReason::ProactiveNetworkSwitch => {
            Ok(types::DisconnectReason::ProactiveNetworkSwitch)
        }
        types::ConnectReason::RetryAfterDisconnectDetected
        | types::ConnectReason::RetryAfterFailedConnectAttempt
        | types::ConnectReason::RegulatoryChangeReconnect
        | types::ConnectReason::IdleInterfaceAutoconnect
        | types::ConnectReason::NewSavedNetworkAutoconnect => Err(()),
    }
}

#[cfg(test)]
mod tests {

    use {
        super::*,
        crate::{
            config_management::{
                network_config::{self, AddAndGetRecent, Credential, FailureReason},
                PastConnectionList, SavedNetworksManager,
            },
            telemetry::{TelemetryEvent, TelemetrySender},
            util::{
                listener,
                testing::{
                    generate_connect_selection, generate_disconnect_info, poll_sme_req,
                    random_connection_data, ConnectResultRecord, ConnectionRecord,
                    FakeSavedNetworksManager,
                },
            },
        },
        fidl::endpoints::create_proxy_and_stream,
        fidl::prelude::*,
        fidl_fuchsia_stash as fidl_stash, fidl_fuchsia_wlan_policy as fidl_policy,
        fuchsia_zircon::prelude::*,
        futures::{task::Poll, Future},
        lazy_static::lazy_static,
        pin_utils::pin_mut,
        std::convert::TryFrom,
        test_util::{assert_gt, assert_lt},
        wlan_common::{assert_variant, sequestered::Sequestered},
        wlan_metrics_registry::PolicyDisconnectionMigratedMetricDimensionReason,
    };

    lazy_static! {
        pub static ref TEST_PASSWORD: Credential = Credential::Password(b"password".to_vec());
        pub static ref TEST_WEP_PSK: Credential = Credential::Password(b"five0".to_vec());
    }

    struct TestValues {
        common_options: CommonStateOptions,
        sme_req_stream: fidl_sme::ClientSmeRequestStream,
        saved_networks_manager: Arc<FakeSavedNetworksManager>,
        client_req_sender: mpsc::Sender<ManualRequest>,
        update_receiver: mpsc::UnboundedReceiver<listener::ClientListenerMessage>,
        telemetry_receiver: mpsc::Receiver<TelemetryEvent>,
        stats_receiver: mpsc::UnboundedReceiver<PeriodicConnectionStats>,
        defect_receiver: mpsc::UnboundedReceiver<Defect>,
    }

    fn test_setup() -> TestValues {
        let (client_req_sender, client_req_stream) = mpsc::channel(1);
        let (update_sender, update_receiver) = mpsc::unbounded();
        let (sme_proxy, sme_server) =
            create_proxy::<fidl_sme::ClientSmeMarker>().expect("failed to create an sme channel");
        let sme_req_stream = sme_server.into_stream().expect("could not create SME request stream");
        let saved_networks = FakeSavedNetworksManager::new();
        let saved_networks_manager = Arc::new(saved_networks);
        let (telemetry_sender, telemetry_receiver) = mpsc::channel::<TelemetryEvent>(100);
        let telemetry_sender = TelemetrySender::new(telemetry_sender);
        let (stats_sender, stats_receiver) = mpsc::unbounded();
        let (defect_sender, defect_receiver) = mpsc::unbounded();

        TestValues {
            common_options: CommonStateOptions {
                proxy: sme_proxy,
                req_stream: client_req_stream.fuse(),
                update_sender,
                saved_networks_manager: saved_networks_manager.clone(),
                telemetry_sender,
                iface_id: 1,
                stats_sender,
                defect_sender,
            },
            sme_req_stream,
            saved_networks_manager,
            client_req_sender,
            update_receiver,
            telemetry_receiver,
            stats_receiver,
            defect_receiver,
        }
    }

    async fn run_state_machine(
        fut: impl Future<Output = Result<State, ExitReason>> + Send + 'static,
    ) {
        let state_machine = fut.into_state_machine();
        select! {
            _state_machine = state_machine.fuse() => return,
        }
    }

    /// Move stash requests forward so that a save request can progress.
    fn process_stash_write(
        exec: &mut fasync::TestExecutor,
        stash_server: &mut fidl_stash::StoreAccessorRequestStream,
    ) {
        assert_variant!(
            exec.run_until_stalled(&mut stash_server.try_next()),
            Poll::Ready(Ok(Some(fidl_stash::StoreAccessorRequest::SetValue { .. })))
        );
        assert_variant!(
            exec.run_until_stalled(&mut stash_server.try_next()),
            Poll::Ready(Ok(Some(fidl_stash::StoreAccessorRequest::Flush{responder}))) => {
                responder.send(&mut Ok(())).expect("failed to send stash response");
            }
        );
    }

    #[fuchsia::test]
    fn wait_for_connect_result_ignores_other_events() {
        let mut exec = fasync::TestExecutor::new();
        let (connect_txn, remote) = create_proxy::<fidl_sme::ConnectTransactionMarker>().unwrap();
        let request_handle = remote.into_stream().unwrap().control_handle();
        let response_stream = connect_txn.take_event_stream();

        let fut = wait_for_connect_result(response_stream);

        pin_mut!(fut);
        assert_variant!(exec.run_until_stalled(&mut fut), Poll::Pending);

        // Send some unexpected response
        let ind = fidl_internal::SignalReportIndication { rssi_dbm: -20, snr_db: 25 };
        request_handle.send_on_signal_report(&ind).unwrap();

        // Future should still be waiting for OnConnectResult event
        assert_variant!(exec.run_until_stalled(&mut fut), Poll::Pending);

        // Send expected ConnectResult response
        let sme_result = fidl_sme::ConnectResult {
            code: fidl_ieee80211::StatusCode::Success,
            is_credential_rejected: false,
            is_reconnect: false,
        };
        request_handle.send_on_connect_result(&sme_result).unwrap();
        assert_variant!(exec.run_until_stalled(&mut fut), Poll::Ready(Ok(response)) => {
            assert_eq!(sme_result, response);
        });
    }

    #[fuchsia::test]
    fn wait_for_connect_result_error() {
        let mut exec = fasync::TestExecutor::new();
        let (connect_txn, remote) = create_proxy::<fidl_sme::ConnectTransactionMarker>().unwrap();
        let response_stream = connect_txn.take_event_stream();

        let fut = wait_for_connect_result(response_stream);

        pin_mut!(fut);
        assert_variant!(exec.run_until_stalled(&mut fut), Poll::Pending);

        // Drop server end, and verify future completes with error
        drop(remote);
        assert_variant!(exec.run_until_stalled(&mut fut), Poll::Ready(Err(ExitReason(_))));
    }

    #[fuchsia::test]
    fn connecting_state_successfully_connects() {
        let mut exec = fasync::TestExecutor::new();
        let mut test_values = test_setup();
        // Do SavedNetworksManager set up manually to get functionality and stash server
        let (saved_networks, mut stash_server) =
            exec.run_singlethreaded(SavedNetworksManager::new_and_stash_server());
        let saved_networks_manager = Arc::new(saved_networks);
        test_values.common_options.saved_networks_manager = saved_networks_manager.clone();

        let connect_selection = generate_connect_selection();
        let bss_description =
            Sequestered::release(connect_selection.target.bss.bss_description.clone());

        // Store the network in the saved_networks_manager, so we can record connection success
        let save_fut = saved_networks_manager.store(
            connect_selection.target.network.clone(),
            connect_selection.target.credential.clone(),
        );
        pin_mut!(save_fut);
        assert_variant!(exec.run_until_stalled(&mut save_fut), Poll::Pending);
        process_stash_write(&mut exec, &mut stash_server);
        assert_variant!(exec.run_until_stalled(&mut save_fut), Poll::Ready(Ok(None)));

        // Check that the saved networks manager has the expected initial data
        let saved_networks = exec.run_singlethreaded(
            saved_networks_manager.lookup(&connect_selection.target.network.clone()),
        );
        assert_eq!(false, saved_networks[0].has_ever_connected);
        assert!(saved_networks[0].hidden_probability > 0.0);

        let connecting_options =
            ConnectingOptions { connect_selection: connect_selection.clone(), attempt_counter: 0 };
        let initial_state = connecting_state(test_values.common_options, connecting_options);
        let fut = run_state_machine(initial_state);
        pin_mut!(fut);
        let sme_fut = test_values.sme_req_stream.into_future();
        pin_mut!(sme_fut);

        // Run the state machine
        assert_variant!(exec.run_until_stalled(&mut fut), Poll::Pending);

        // Ensure a connect request is sent to the SME
        let connect_txn_handle = assert_variant!(
            poll_sme_req(&mut exec, &mut sme_fut),
            Poll::Ready(fidl_sme::ClientSmeRequest::Connect{ req, txn, control_handle: _ }) => {
                assert_eq!(req.ssid, connect_selection.target.network.ssid.clone().to_vec());
                assert_eq!(req.bss_description, bss_description);
                assert_eq!(req.deprecated_scan_type, fidl_fuchsia_wlan_common::ScanType::Active);
                assert_eq!(req.multiple_bss_candidates, connect_selection.target.network_has_multiple_bss);
                // Send connection response.
                let (_stream, ctrl) = txn.expect("connect txn unused")
                    .into_stream_and_control_handle().expect("error accessing control handle");
                ctrl
            }
        );
        connect_txn_handle
            .send_on_connect_result(&fake_successful_connect_result())
            .expect("failed to send connection completion");

        // Check for a connecting update
        let client_state_update = ClientStateUpdate {
            state: fidl_policy::WlanClientState::ConnectionsEnabled,
            networks: vec![ClientNetworkState {
                id: connect_selection.target.network.clone(),
                state: fidl_policy::ConnectionState::Connecting,
                status: None,
            }],
        };
        assert_variant!(
            test_values.update_receiver.try_next(),
            Ok(Some(listener::Message::NotifyListeners(updates))) => {
            assert_eq!(updates, client_state_update);
        });

        // Progress the state machine
        assert_variant!(exec.run_until_stalled(&mut fut), Poll::Pending);
        process_stash_write(&mut exec, &mut stash_server);
        assert_variant!(exec.run_until_stalled(&mut fut), Poll::Pending);

        // Check for a connect update
        let client_state_update = ClientStateUpdate {
            state: fidl_policy::WlanClientState::ConnectionsEnabled,
            networks: vec![ClientNetworkState {
                id: connect_selection.target.network.clone(),
                state: fidl_policy::ConnectionState::Connected,
                status: None,
            }],
        };
        assert_variant!(
            test_values.update_receiver.try_next(),
            Ok(Some(listener::Message::NotifyListeners(updates))) => {
            assert_eq!(updates, client_state_update);
        });

        // Check that the saved networks manager has the connection recorded
        let saved_networks = exec.run_singlethreaded(
            saved_networks_manager.lookup(&connect_selection.target.network.clone()),
        );
        assert_eq!(true, saved_networks[0].has_ever_connected);

        // Progress the state machine
        assert_variant!(exec.run_until_stalled(&mut fut), Poll::Pending);

        // Ensure no further updates were sent to listeners
        assert_variant!(
            exec.run_until_stalled(&mut test_values.update_receiver.into_future()),
            Poll::Pending
        );
    }

    #[fuchsia::test]
    fn connecting_state_times_out() {
        let mut exec = fasync::TestExecutor::new();
        let mut test_values = test_setup();
        // Do SavedNetworksManager set up manually to get functionality and stash server
        let (saved_networks, mut stash_server) =
            exec.run_singlethreaded(SavedNetworksManager::new_and_stash_server());
        let saved_networks_manager = Arc::new(saved_networks);
        test_values.common_options.saved_networks_manager = saved_networks_manager.clone();

        let connect_selection = generate_connect_selection();
        let bss_description =
            Sequestered::release(connect_selection.target.bss.bss_description.clone());

        // Store the network in the saved_networks_manager
        let save_fut = saved_networks_manager.store(
            connect_selection.target.network.clone(),
            connect_selection.target.credential.clone(),
        );
        pin_mut!(save_fut);
        assert_variant!(exec.run_until_stalled(&mut save_fut), Poll::Pending);
        process_stash_write(&mut exec, &mut stash_server);
        assert_variant!(exec.run_until_stalled(&mut save_fut), Poll::Ready(Ok(None)));

        // Prepare state machine
        let connecting_options =
            ConnectingOptions { connect_selection: connect_selection.clone(), attempt_counter: 0 };
        let initial_state = connecting_state(test_values.common_options, connecting_options);
        let fut = run_state_machine(initial_state);
        pin_mut!(fut);
        let sme_fut = test_values.sme_req_stream.into_future();
        pin_mut!(sme_fut);

        // Run the state machine
        assert_variant!(exec.run_until_stalled(&mut fut), Poll::Pending);

        // Ensure a connect request is sent to the SME
        let connect_txn_handle = assert_variant!(
            poll_sme_req(&mut exec, &mut sme_fut),
            Poll::Ready(fidl_sme::ClientSmeRequest::Connect{ req, txn, control_handle: _ }) => {
                assert_eq!(req.ssid, connect_selection.target.network.ssid.clone().to_vec());
                assert_eq!(req.bss_description, bss_description);
                assert_eq!(req.deprecated_scan_type, fidl_fuchsia_wlan_common::ScanType::Active);
                assert_eq!(req.multiple_bss_candidates, connect_selection.target.network_has_multiple_bss);
                let (_stream, ctrl) = txn.expect("connect txn unused")
                    .into_stream_and_control_handle().expect("error accessing control handle");
                ctrl
            }
        );

        // Check for a connecting update
        let client_state_update = ClientStateUpdate {
            state: fidl_policy::WlanClientState::ConnectionsEnabled,
            networks: vec![ClientNetworkState {
                id: connect_selection.target.network.clone(),
                state: fidl_policy::ConnectionState::Connecting,
                status: None,
            }],
        };
        assert_variant!(
            test_values.update_receiver.try_next(),
            Ok(Some(listener::Message::NotifyListeners(updates))) => {
            assert_eq!(updates, client_state_update);
        });

        // Respond with a SignalReport, which should not unblock connecting_state
        connect_txn_handle
            .send_on_signal_report(&fidl_internal::SignalReportIndication {
                rssi_dbm: -25,
                snr_db: 30,
            })
            .expect("failed to send singal report");

        // Run the state machine. Should still be pending
        assert_variant!(exec.run_until_stalled(&mut fut), Poll::Pending);

        // Wake up the next timer, which is the timeout for the connect request.
        assert!(exec.wake_next_timer().is_some());

        // State machine should exit.
        assert_variant!(exec.run_until_stalled(&mut fut), Poll::Ready(()));
    }

    #[fuchsia::test]
    fn connecting_state_successfully_scans_and_connects() {
        let mut exec = fasync::TestExecutor::new_with_fake_time();
        exec.set_fake_time(fasync::Time::from_nanos(123));
        let mut test_values = test_setup();

        let connection_attempt_time = fasync::Time::now();

        let connect_selection = generate_connect_selection();
        let bss_description =
            Sequestered::release(connect_selection.target.bss.bss_description.clone());

        // Set how the SavedNetworksManager should respond to lookup_compatible for the scan.
        let expected_config = network_config::NetworkConfig::new(
            connect_selection.target.network.clone(),
            connect_selection.target.credential.clone(),
            connect_selection.target.saved_network_info.has_ever_connected,
        )
        .expect("failed to create network config");
        test_values.saved_networks_manager.set_lookup_compatible_response(vec![expected_config]);

        let connecting_options =
            ConnectingOptions { connect_selection: connect_selection.clone(), attempt_counter: 0 };
        let initial_state = connecting_state(test_values.common_options, connecting_options);
        let fut = run_state_machine(initial_state);
        pin_mut!(fut);

        // Run the state machine
        assert_variant!(exec.run_until_stalled(&mut fut), Poll::Pending);

        // Ensure a connect request is sent to the SME
        let sme_fut = test_values.sme_req_stream.into_future();
        pin_mut!(sme_fut);
        let time_to_connect = 30.seconds();
        let connect_txn_handle = assert_variant!(
            poll_sme_req(&mut exec, &mut sme_fut),
            Poll::Ready(fidl_sme::ClientSmeRequest::Connect{ req, txn, control_handle: _ }) => {
                assert_eq!(req.ssid, connect_selection.target.network.ssid.clone().to_vec());
                assert_eq!(req.bss_description, bss_description.clone());
                assert_eq!(req.deprecated_scan_type, fidl_fuchsia_wlan_common::ScanType::Active);
                assert_eq!(req.multiple_bss_candidates, connect_selection.target.network_has_multiple_bss);
                // Send connection response.
                exec.set_fake_time(fasync::Time::after(time_to_connect));
                let (_stream, ctrl) = txn.expect("connect txn unused")
                    .into_stream_and_control_handle().expect("error accessing control handle");
                ctrl
            }
        );
        connect_txn_handle
            .send_on_connect_result(&fake_successful_connect_result())
            .expect("failed to send connection completion");

        // Check for a connecting update
        let client_state_update = ClientStateUpdate {
            state: fidl_policy::WlanClientState::ConnectionsEnabled,
            networks: vec![ClientNetworkState {
                id: connect_selection.target.network.clone(),
                state: fidl_policy::ConnectionState::Connecting,
                status: None,
            }],
        };
        assert_variant!(
            test_values.update_receiver.try_next(),
            Ok(Some(listener::Message::NotifyListeners(updates))) => {
            assert_eq!(updates, client_state_update);
        });

        // Progress the state machine
        assert_variant!(exec.run_until_stalled(&mut fut), Poll::Pending);

        // Check for a connect update
        let client_state_update = ClientStateUpdate {
            state: fidl_policy::WlanClientState::ConnectionsEnabled,
            networks: vec![ClientNetworkState {
                id: connect_selection.target.network.clone(),
                state: fidl_policy::ConnectionState::Connected,
                status: None,
            }],
        };
        assert_variant!(
            test_values.update_receiver.try_next(),
            Ok(Some(listener::Message::NotifyListeners(updates))) => {
            assert_eq!(updates, client_state_update);
        });

        // Check that the saved networks manager has the connection result recorded
        assert_variant!(test_values.saved_networks_manager.get_recorded_connect_reslts().as_slice(), [data] => {
            let expected_connect_result = ConnectResultRecord {
                 id: connect_selection.target.network.clone(),
                 credential: connect_selection.target.credential.clone(),
                 bssid: types::Bssid(bss_description.bssid),
                 connect_result: fake_successful_connect_result(),
                 scan_type: connect_selection.target.bss.observation,
            };
            assert_eq!(data, &expected_connect_result);
        });

        // Check that connected telemetry event is sent
        assert_variant!(
            test_values.telemetry_receiver.try_next(),
            Ok(Some(TelemetryEvent::ConnectResult { iface_id: 1, policy_connect_reason, result, multiple_bss_candidates, ap_state })) => {
                assert_eq!(bss_description, ap_state.original().clone().into());
                assert_eq!(multiple_bss_candidates, connect_selection.target.network_has_multiple_bss);
                assert_eq!(policy_connect_reason, Some(connect_selection.reason));
                assert_eq!(result, fake_successful_connect_result());
            }
        );

        // Progress the state machine
        assert_variant!(exec.run_until_stalled(&mut fut), Poll::Pending);

        // Ensure no further updates were sent to listeners
        assert_variant!(
            exec.run_until_stalled(&mut test_values.update_receiver.into_future()),
            Poll::Pending
        );

        // Send a disconnect and check that the connection data is correctly recorded
        let is_sme_reconnecting = false;
        let fidl_disconnect_info = generate_disconnect_info(is_sme_reconnecting);
        connect_txn_handle
            .send_on_disconnect(&fidl_disconnect_info)
            .expect("failed to send disconnection event");
        assert_variant!(exec.run_until_stalled(&mut fut), Poll::Pending);

        let expected_recorded_connection = ConnectionRecord {
            id: connect_selection.target.network.clone(),
            credential: connect_selection.target.credential.clone(),
            data: PastConnectionData {
                bssid: types::Bssid(bss_description.bssid),
                connection_attempt_time,
                time_to_connect,
                disconnect_time: fasync::Time::now(),
                connection_uptime: zx::Duration::from_minutes(0),
                disconnect_reason: types::DisconnectReason::DisconnectDetectedFromSme,
                signal_data_at_disconnect: bss_selection::SignalData::new(
                    bss_description.rssi_dbm,
                    bss_description.snr_db,
                    bss_selection::EWMA_SMOOTHING_FACTOR,
                    bss_selection::EWMA_VELOCITY_SMOOTHING_FACTOR,
                ),
                // TODO: record average phy rate over connection once available
                average_tx_rate: 0,
            },
        };
        assert_variant!(test_values.saved_networks_manager.get_recorded_past_connections().as_slice(), [data] => {
            assert_eq!(data, &expected_recorded_connection);
        });
    }

    #[fuchsia::test]
    fn connecting_state_fails_to_connect_and_retries() {
        let mut exec = fasync::TestExecutor::new();
        let mut test_values = test_setup();

        let connect_selection = generate_connect_selection();
        let bss_description =
            Sequestered::release(connect_selection.target.bss.bss_description.clone());

        let connecting_options =
            ConnectingOptions { connect_selection: connect_selection.clone(), attempt_counter: 0 };
        let initial_state = connecting_state(test_values.common_options, connecting_options);
        let fut = run_state_machine(initial_state);
        pin_mut!(fut);
        let sme_fut = test_values.sme_req_stream.into_future();
        pin_mut!(sme_fut);

        // Run the state machine
        assert_variant!(exec.run_until_stalled(&mut fut), Poll::Pending);

        // Ensure a connect request is sent to the SME
        let mut connect_txn_handle = assert_variant!(
            poll_sme_req(&mut exec, &mut sme_fut),
            Poll::Ready(fidl_sme::ClientSmeRequest::Connect{ req, txn, control_handle: _ }) => {
                assert_eq!(req.ssid, connect_selection.target.network.ssid.to_vec());
                 // Send connection response.
                let (_stream, ctrl) = txn.expect("connect txn unused")
                    .into_stream_and_control_handle().expect("error accessing control handle");
                ctrl
            }
        );
        let connect_result = fidl_sme::ConnectResult {
            code: fidl_ieee80211::StatusCode::RefusedReasonUnspecified,
            ..fake_successful_connect_result()
        };
        connect_txn_handle
            .send_on_connect_result(&connect_result)
            .expect("failed to send connection completion");

        // Check for a connecting update
        let client_state_update = ClientStateUpdate {
            state: fidl_policy::WlanClientState::ConnectionsEnabled,
            networks: vec![ClientNetworkState {
                id: types::NetworkIdentifier {
                    ssid: connect_selection.target.network.ssid.clone(),
                    security_type: types::SecurityType::Wpa2,
                },
                state: fidl_policy::ConnectionState::Connecting,
                status: None,
            }],
        };
        assert_variant!(
            test_values.update_receiver.try_next(),
            Ok(Some(listener::Message::NotifyListeners(updates))) => {
            assert_eq!(updates, client_state_update);
        });

        // Progress the state machine
        assert_variant!(exec.run_until_stalled(&mut fut), Poll::Pending);
        assert!(exec.wake_next_timer().is_some());
        assert_variant!(exec.run_until_stalled(&mut fut), Poll::Pending);

        // Check that connect result telemetry event is sent
        assert_variant!(
            test_values.telemetry_receiver.try_next(),
            Ok(Some(TelemetryEvent::ConnectResult { iface_id: 1, policy_connect_reason, result, multiple_bss_candidates, ap_state })) => {
                assert_eq!(bss_description, ap_state.original().clone().into());
                assert_eq!(multiple_bss_candidates, connect_selection.target.network_has_multiple_bss);
                assert_eq!(policy_connect_reason, Some(connect_selection.reason));
                assert_eq!(result, connect_result);

            }
        );

        // Ensure a disconnect request is sent to the SME
        assert_variant!(
            poll_sme_req(&mut exec, &mut sme_fut),
            Poll::Ready(fidl_sme::ClientSmeRequest::Disconnect{ responder, reason: fidl_sme::UserDisconnectReason::FailedToConnect }) => {
                responder.send().expect("could not send sme response");
            }
        );

        // Progress the state machine
        assert_variant!(exec.run_until_stalled(&mut fut), Poll::Pending);

        // Ensure a connect request is sent to the SME
        connect_txn_handle = assert_variant!(
            poll_sme_req(&mut exec, &mut sme_fut),
            Poll::Ready(fidl_sme::ClientSmeRequest::Connect{ req, txn, control_handle: _ }) => {
                assert_eq!(req.ssid, connect_selection.target.network.ssid.to_vec());
                assert_eq!(req.bss_description, Sequestered::release(connect_selection.target.bss.bss_description));
                assert_eq!(req.multiple_bss_candidates, connect_selection.target.network_has_multiple_bss);
                 // Send connection response.
                let (_stream, ctrl) = txn.expect("connect txn unused")
                    .into_stream_and_control_handle().expect("error accessing control handle");
                ctrl
            }
        );
        let connect_result = fake_successful_connect_result();
        connect_txn_handle
            .send_on_connect_result(&connect_result)
            .expect("failed to send connection completion");

        // Progress the state machine
        assert_variant!(exec.run_until_stalled(&mut fut), Poll::Pending);

        // Empty update sent to NotifyListeners (which in this case, will not actually be sent.)
        assert_variant!(
            test_values.update_receiver.try_next(),
            Ok(Some(listener::Message::NotifyListeners(ClientStateUpdate {
                state: fidl_policy::WlanClientState::ConnectionsEnabled,
                networks
            }))) => {
                assert!(networks.is_empty());
            }
        );

        // A defect should be logged.
        assert_variant!(
            test_values.defect_receiver.try_next(),
            Ok(Some(Defect::Iface(IfaceFailure::ConnectionFailure { iface_id: 1 })))
        );

        // Check for a connected update
        let client_state_update = ClientStateUpdate {
            state: fidl_policy::WlanClientState::ConnectionsEnabled,
            networks: vec![ClientNetworkState {
                id: types::NetworkIdentifier {
                    ssid: connect_selection.target.network.ssid.clone(),
                    security_type: types::SecurityType::Wpa2,
                },
                state: fidl_policy::ConnectionState::Connected,
                status: None,
            }],
        };
        assert_variant!(
            test_values.update_receiver.try_next(),
            Ok(Some(listener::Message::NotifyListeners(updates))) => {
            assert_eq!(updates, client_state_update);
        });

        // Progress the state machine
        assert_variant!(exec.run_until_stalled(&mut fut), Poll::Pending);

        // Ensure no further updates were sent to listeners
        assert_variant!(
            exec.run_until_stalled(&mut test_values.update_receiver.into_future()),
            Poll::Pending
        );
    }

    #[fuchsia::test]
    fn connecting_state_fails_to_connect_at_max_retries() {
        let mut exec = fasync::TestExecutor::new();
        // Don't use test_values() because of issue with KnownEssStore
        let (update_sender, mut update_receiver) = mpsc::unbounded();
        let (sme_proxy, sme_server) =
            create_proxy::<fidl_sme::ClientSmeMarker>().expect("failed to create an sme channel");
        let sme_req_stream = sme_server.into_stream().expect("could not create SME request stream");
        let saved_networks_manager = Arc::new(
            exec.run_singlethreaded(SavedNetworksManager::new_for_test())
                .expect("Failed to create saved networks manager"),
        );
        let (_client_req_sender, client_req_stream) = mpsc::channel(1);
        let (telemetry_sender, _telemetry_receiver) = mpsc::channel::<TelemetryEvent>(100);
        let telemetry_sender = TelemetrySender::new(telemetry_sender);
        let (stats_sender, _stats_receiver) = mpsc::unbounded();
        let (defect_sender, mut defect_receiver) = mpsc::unbounded();
        let common_options = CommonStateOptions {
            proxy: sme_proxy,
            req_stream: client_req_stream.fuse(),
            update_sender,
            saved_networks_manager: saved_networks_manager.clone(),
            telemetry_sender,
            iface_id: 1,
            stats_sender,
            defect_sender,
        };

        let connect_selection = generate_connect_selection();
        let bss_description =
            Sequestered::release(connect_selection.target.bss.bss_description.clone());

        // save network to check that failed connect is recorded
        assert!(exec
            .run_singlethreaded(saved_networks_manager.store(
                connect_selection.target.network.clone(),
                connect_selection.target.credential.clone()
            ),)
            .expect("Failed to save network")
            .is_none());
        let before_recording = fasync::Time::now();

        let connecting_options = ConnectingOptions {
            connect_selection: connect_selection.clone(),
            attempt_counter: MAX_CONNECTION_ATTEMPTS - 1,
        };
        let initial_state = connecting_state(common_options, connecting_options);
        let fut = run_state_machine(initial_state);
        pin_mut!(fut);
        let sme_fut = sme_req_stream.into_future();
        pin_mut!(sme_fut);

        // Run the state machine
        assert_variant!(exec.run_until_stalled(&mut fut), Poll::Pending);

        // Ensure a connect request is sent to the SME
        assert_variant!(
            poll_sme_req(&mut exec, &mut sme_fut),
            Poll::Ready(fidl_sme::ClientSmeRequest::Connect{ req, txn, control_handle: _ }) => {
                assert_eq!(req.ssid, connect_selection.target.network.ssid.clone().to_vec());
                assert_eq!(req.bss_description, bss_description.clone());
                assert_eq!(req.deprecated_scan_type, fidl_fuchsia_wlan_common::ScanType::Active);
                assert_eq!(req.multiple_bss_candidates, connect_selection.target.network_has_multiple_bss);
                 // Send connection response.
                let (_stream, ctrl) = txn.expect("connect txn unused")
                    .into_stream_and_control_handle().expect("error accessing control handle");
                let connect_result = fidl_sme::ConnectResult {
                    code: fidl_ieee80211::StatusCode::RefusedReasonUnspecified,
                    ..fake_successful_connect_result()
                };
                ctrl
                    .send_on_connect_result(&connect_result)
                    .expect("failed to send connection completion");
            }
        );

        // After failing to reconnect, the state machine should exit so that the state machine
        // monitor can attempt to reconnect the interface.
        assert_variant!(exec.run_until_stalled(&mut fut), Poll::Ready(()));

        // Check for a connect update
        let client_state_update = ClientStateUpdate {
            state: fidl_policy::WlanClientState::ConnectionsEnabled,
            networks: vec![ClientNetworkState {
                id: connect_selection.target.network.clone(),
                state: fidl_policy::ConnectionState::Failed,
                status: Some(fidl_policy::DisconnectStatus::ConnectionFailed),
            }],
        };
        assert_variant!(
            update_receiver.try_next(),
            Ok(Some(listener::Message::NotifyListeners(updates))) => {
            assert_eq!(updates, client_state_update);
        });

        // Check that failure was recorded in SavedNetworksManager
        let mut configs = exec.run_singlethreaded(
            saved_networks_manager.lookup(&connect_selection.target.network.clone()),
        );
        let network_config = configs.pop().expect("Failed to get saved network");
        let mut failures =
            network_config.perf_stats.connect_failures.get_recent_for_network(before_recording);
        let connect_failure = failures.pop().expect("Saved network is missing failure reason");
        assert_eq!(connect_failure.reason, FailureReason::GeneralFailure);

        // A defect should be logged.
        assert_variant!(
            defect_receiver.try_next(),
            Ok(Some(Defect::Iface(IfaceFailure::ConnectionFailure { iface_id: 1 })))
        );
    }

    #[fuchsia::test]
    fn connecting_state_fails_to_connect_with_bad_credentials() {
        let mut exec = fasync::TestExecutor::new();
        // Don't use test_values() because of issue with KnownEssStore
        let (update_sender, mut update_receiver) = mpsc::unbounded();
        let (sme_proxy, sme_server) =
            create_proxy::<fidl_sme::ClientSmeMarker>().expect("failed to create an sme channel");
        let sme_req_stream = sme_server.into_stream().expect("could not create SME request stream");
        let saved_networks_manager = Arc::new(
            exec.run_singlethreaded(SavedNetworksManager::new_for_test())
                .expect("Failed to create saved networks manager"),
        );
        let (_client_req_sender, client_req_stream) = mpsc::channel(1);
        let (telemetry_sender, _telemetry_receiver) = mpsc::channel::<TelemetryEvent>(100);
        let telemetry_sender = TelemetrySender::new(telemetry_sender);
        let (stats_sender, _stats_receiver) = mpsc::unbounded();
        let (defect_sender, mut defect_receiver) = mpsc::unbounded();

        let common_options = CommonStateOptions {
            proxy: sme_proxy,
            req_stream: client_req_stream.fuse(),
            update_sender,
            saved_networks_manager: saved_networks_manager.clone(),
            telemetry_sender,
            iface_id: 1,
            stats_sender,
            defect_sender,
        };

        let connect_selection = generate_connect_selection();
        let bss_description =
            Sequestered::release(connect_selection.target.bss.bss_description.clone());

        // save network to check that failed connect is recorded
        let saved_networks_manager = common_options.saved_networks_manager.clone();
        assert!(exec
            .run_singlethreaded(saved_networks_manager.store(
                connect_selection.target.network.clone(),
                connect_selection.target.credential.clone()
            ),)
            .expect("Failed to save network")
            .is_none());
        let before_recording = fasync::Time::now();

        let connecting_options = ConnectingOptions {
            connect_selection: connect_selection.clone(),
            attempt_counter: MAX_CONNECTION_ATTEMPTS - 1,
        };
        let initial_state = connecting_state(common_options, connecting_options);
        let fut = run_state_machine(initial_state);
        pin_mut!(fut);
        let sme_fut = sme_req_stream.into_future();
        pin_mut!(sme_fut);

        // Run the state machine
        assert_variant!(exec.run_until_stalled(&mut fut), Poll::Pending);

        // Ensure a connect request is sent to the SME
        assert_variant!(
            poll_sme_req(&mut exec, &mut sme_fut),
            Poll::Ready(fidl_sme::ClientSmeRequest::Connect{ req, txn, control_handle: _ }) => {
                assert_eq!(req.ssid, connect_selection.target.network.ssid.clone().to_vec());
                assert_eq!(req.bss_description, bss_description.clone());
                assert_eq!(req.deprecated_scan_type, fidl_fuchsia_wlan_common::ScanType::Active);
                assert_eq!(req.multiple_bss_candidates, connect_selection.target.network_has_multiple_bss);
                 // Send connection response.
                let (_stream, ctrl) = txn.expect("connect txn unused")
                    .into_stream_and_control_handle().expect("error accessing control handle");
                let connect_result = fidl_sme::ConnectResult {
                    code: fidl_ieee80211::StatusCode::RefusedReasonUnspecified,
                    is_credential_rejected: true,
                    ..fake_successful_connect_result()
                };
                ctrl
                    .send_on_connect_result(&connect_result)
                    .expect("failed to send connection completion");
            }
        );

        // The state machine should exit when bad credentials are detected so that the state
        // machine monitor can try to connect to another network.
        assert_variant!(exec.run_until_stalled(&mut fut), Poll::Ready(()));

        // Check for a connect update
        let client_state_update = ClientStateUpdate {
            state: fidl_policy::WlanClientState::ConnectionsEnabled,
            networks: vec![ClientNetworkState {
                id: connect_selection.target.network.clone(),
                state: fidl_policy::ConnectionState::Failed,
                status: Some(fidl_policy::DisconnectStatus::CredentialsFailed),
            }],
        };
        assert_variant!(
            update_receiver.try_next(),
            Ok(Some(listener::Message::NotifyListeners(updates))) => {
            assert_eq!(updates, client_state_update);
        });

        // Check that failure was recorded in SavedNetworksManager
        let mut configs = exec.run_singlethreaded(
            saved_networks_manager.lookup(&connect_selection.target.network.clone()),
        );
        let network_config = configs.pop().expect("Failed to get saved network");
        let mut failures =
            network_config.perf_stats.connect_failures.get_recent_for_network(before_recording);
        let connect_failure = failures.pop().expect("Saved network is missing failure reason");
        assert_eq!(connect_failure.reason, FailureReason::CredentialRejected);

        // No defect should have been observed.
        assert_variant!(defect_receiver.try_next(), Ok(None));
    }

    #[fuchsia::test]
    fn connecting_state_gets_duplicate_connect_selection() {
        let mut exec = fasync::TestExecutor::new();
        let mut test_values = test_setup();

        let connect_selection = generate_connect_selection();
        let bss_description =
            Sequestered::release(connect_selection.target.bss.bss_description.clone());

        let connecting_options =
            ConnectingOptions { connect_selection: connect_selection.clone(), attempt_counter: 0 };
        let initial_state = connecting_state(test_values.common_options, connecting_options);
        let fut = run_state_machine(initial_state);
        pin_mut!(fut);
        let sme_fut = test_values.sme_req_stream.into_future();
        pin_mut!(sme_fut);

        // Run the state machine
        assert_variant!(exec.run_until_stalled(&mut fut), Poll::Pending);

        // Check for a connecting update
        let client_state_update = ClientStateUpdate {
            state: fidl_policy::WlanClientState::ConnectionsEnabled,
            networks: vec![ClientNetworkState {
                id: types::NetworkIdentifier {
                    ssid: connect_selection.target.network.ssid.clone(),
                    security_type: types::SecurityType::Wpa2,
                },
                state: fidl_policy::ConnectionState::Connecting,
                status: None,
            }],
        };
        assert_variant!(
            test_values.update_receiver.try_next(),
            Ok(Some(listener::Message::NotifyListeners(updates))) => {
            assert_eq!(updates, client_state_update);
        });

        // Send a duplicate connect request
        let mut client = Client::new(test_values.client_req_sender);
        let duplicate_request = types::ConnectSelection {
            // this incoming request should be deduped regardless of the reason
            reason: types::ConnectReason::ProactiveNetworkSwitch,
            ..connect_selection.clone()
        };
        client.connect(duplicate_request).expect("failed to make request");

        // Progress the state machine
        assert_variant!(exec.run_until_stalled(&mut fut), Poll::Pending);

        // Ensure a connect request is sent to the SME
        let connect_txn_handle = assert_variant!(
            poll_sme_req(&mut exec, &mut sme_fut),
            Poll::Ready(fidl_sme::ClientSmeRequest::Connect{ req, txn, control_handle: _ }) => {
                assert_eq!(req.ssid, connect_selection.target.network.ssid.clone().to_vec());
                assert_eq!(req.deprecated_scan_type, fidl_fuchsia_wlan_common::ScanType::Active);
                assert_eq!(req.bss_description, bss_description);
                assert_eq!(req.multiple_bss_candidates, connect_selection.target.network_has_multiple_bss);
                 // Send connection response.
                let (_stream, ctrl) = txn.expect("connect txn unused")
                    .into_stream_and_control_handle().expect("error accessing control handle");
                ctrl
            }
        );
        connect_txn_handle
            .send_on_connect_result(&fake_successful_connect_result())
            .expect("failed to send connection completion");

        // Progress the state machine
        assert_variant!(exec.run_until_stalled(&mut fut), Poll::Pending);

        // Check for a connect update
        let client_state_update = ClientStateUpdate {
            state: fidl_policy::WlanClientState::ConnectionsEnabled,
            networks: vec![ClientNetworkState {
                id: connect_selection.target.network.clone(),
                state: fidl_policy::ConnectionState::Connected,
                status: None,
            }],
        };
        assert_variant!(
            test_values.update_receiver.try_next(),
            Ok(Some(listener::Message::NotifyListeners(updates))) => {
            assert_eq!(updates, client_state_update);
        });

        // Progress the state machine
        assert_variant!(exec.run_until_stalled(&mut fut), Poll::Pending);

        // Ensure no further updates were sent to listeners
        assert_variant!(
            exec.run_until_stalled(&mut test_values.update_receiver.into_future()),
            Poll::Pending
        );
    }

    #[fuchsia::test]
    fn connecting_state_has_broken_sme() {
        let mut exec = fasync::TestExecutor::new();
        let test_values = test_setup();

        let connect_selection = generate_connect_selection();

        let connecting_options =
            ConnectingOptions { connect_selection: connect_selection.clone(), attempt_counter: 0 };
        let initial_state = connecting_state(test_values.common_options, connecting_options);
        let fut = run_state_machine(initial_state);
        pin_mut!(fut);

        // Break the SME by dropping the server end of the SME stream, so it causes an error
        drop(test_values.sme_req_stream);

        // Ensure the state machine exits
        assert_variant!(exec.run_until_stalled(&mut fut), Poll::Ready(()));
    }

    #[fuchsia::test]
    fn connected_state_gets_disconnect_request() {
        let mut exec = fasync::TestExecutor::new_with_fake_time();
        exec.set_fake_time(fasync::Time::from_nanos(0));

        let mut test_values = test_setup();
        let mut telemetry_receiver = test_values.telemetry_receiver;
        let connect_selection = generate_connect_selection();
        let bss_description =
            Sequestered::release(connect_selection.target.bss.bss_description.clone());
        let ap_state =
            types::ApState::from(BssDescription::try_from(bss_description.clone()).unwrap());

        let (connect_txn_proxy, _connect_txn_stream) =
            create_proxy_and_stream::<fidl_sme::ConnectTransactionMarker>()
                .expect("failed to create a connect txn channel");
        let connection_attempt_time = fasync::Time::now();
        let time_to_connect = zx::Duration::from_seconds(10);
        let options = ConnectedOptions {
            currently_fulfilled_connection: connect_selection.clone(),
            multiple_bss_candidates: connect_selection.target.network_has_multiple_bss,
            ap_state: Box::new(ap_state.clone()),
            connect_txn_stream: connect_txn_proxy.take_event_stream(),
            connection_attempt_time,
            time_to_connect,
        };
        let initial_state = connected_state(test_values.common_options, options);
        let fut = run_state_machine(initial_state);
        pin_mut!(fut);
        let sme_fut = test_values.sme_req_stream.into_future();
        pin_mut!(sme_fut);

        // Run the state machine
        assert_variant!(exec.run_until_stalled(&mut fut), Poll::Pending);

        let disconnect_time = fasync::Time::after(12.hours());
        exec.set_fake_time(disconnect_time);

        // Send a disconnect request
        let mut client = Client::new(test_values.client_req_sender);
        let (sender, mut receiver) = oneshot::channel();
        client
            .disconnect(types::DisconnectReason::FidlStopClientConnectionsRequest, sender)
            .expect("failed to make request");

        // Run the state machine
        assert_variant!(exec.run_until_stalled(&mut fut), Poll::Pending);

        // Respond to the SME disconnect
        assert_variant!(
            poll_sme_req(&mut exec, &mut sme_fut),
            Poll::Ready(fidl_sme::ClientSmeRequest::Disconnect{ responder, reason: fidl_sme::UserDisconnectReason::FidlStopClientConnectionsRequest }) => {
                responder.send().expect("could not send sme response");
            }
        );

        // Once the disconnect is processed, the state machine should exit.
        assert_variant!(exec.run_until_stalled(&mut fut), Poll::Ready(()));

        // Check for a disconnect update and the responder
        let client_state_update = ClientStateUpdate {
            state: fidl_policy::WlanClientState::ConnectionsEnabled,
            networks: vec![ClientNetworkState {
                id: connect_selection.target.network.clone(),
                state: fidl_policy::ConnectionState::Disconnected,
                status: Some(fidl_policy::DisconnectStatus::ConnectionStopped),
            }],
        };
        assert_variant!(
            test_values.update_receiver.try_next(),
            Ok(Some(listener::Message::NotifyListeners(updates))) => {
            assert_eq!(updates, client_state_update);
        });
        assert_variant!(exec.run_until_stalled(&mut receiver), Poll::Ready(Ok(())));

        // Disconnect telemetry event sent
        assert_variant!(telemetry_receiver.try_next(), Ok(Some(event)) => {
            assert_variant!(event, TelemetryEvent::Disconnected { track_subsequent_downtime, info } => {
                assert!(!track_subsequent_downtime);
                assert_eq!(info, DisconnectInfo {
                    connected_duration: 12.hours(),
                    is_sme_reconnecting: false,
                    disconnect_source: fidl_sme::DisconnectSource::User(fidl_sme::UserDisconnectReason::FidlStopClientConnectionsRequest),
                    previous_connect_reason: connect_selection.reason,
                    ap_state: ap_state.clone(),
                });
            });
        });

        // The disconnect should have been recorded for the saved network config.
        let expected_recorded_connection = ConnectionRecord {
            id: connect_selection.target.network.clone(),
            credential: connect_selection.target.credential.clone(),
            data: PastConnectionData {
                bssid: ap_state.original().bssid,
                connection_attempt_time,
                time_to_connect,
                disconnect_time,
                connection_uptime: zx::Duration::from_hours(12),
                disconnect_reason: types::DisconnectReason::FidlStopClientConnectionsRequest,
                signal_data_at_disconnect: bss_selection::SignalData::new(
                    bss_description.rssi_dbm,
                    bss_description.snr_db,
                    bss_selection::EWMA_SMOOTHING_FACTOR,
                    bss_selection::EWMA_VELOCITY_SMOOTHING_FACTOR,
                ),
                // TODO: record average phy rate over connection once available
                average_tx_rate: 0,
            },
        };
        assert_variant!(test_values.saved_networks_manager.get_recorded_past_connections().as_slice(), [connection_data] => {
            assert_eq!(connection_data, &expected_recorded_connection);
        });
    }

    #[fuchsia::test]
    fn connected_state_records_unexpected_disconnect() {
        let mut exec = fasync::TestExecutor::new_with_fake_time();
        exec.set_fake_time(fasync::Time::from_nanos(0));

        let test_values = test_setup();
        let mut telemetry_receiver = test_values.telemetry_receiver;

        let connect_selection = generate_connect_selection();
        let bss_description =
            Sequestered::release(connect_selection.target.bss.bss_description.clone());
        let ap_state =
            types::ApState::from(BssDescription::try_from(bss_description.clone()).unwrap());

        // Save the network in order to later record the disconnect to it.
        let save_fut = test_values.saved_networks_manager.store(
            connect_selection.target.network.clone(),
            connect_selection.target.credential.clone(),
        );
        pin_mut!(save_fut);
        assert_variant!(exec.run_until_stalled(&mut save_fut), Poll::Ready(Ok(None)));

        let (connect_txn_proxy, connect_txn_stream) =
            create_proxy_and_stream::<fidl_sme::ConnectTransactionMarker>()
                .expect("failed to create a connect txn channel");
        let connect_txn_handle = connect_txn_stream.control_handle();
        let connection_attempt_time = fasync::Time::now();
        let time_to_connect = zx::Duration::from_seconds(10);
        let options = ConnectedOptions {
            currently_fulfilled_connection: connect_selection.clone(),
            multiple_bss_candidates: connect_selection.target.network_has_multiple_bss,
            ap_state: Box::new(ap_state.clone()),
            connect_txn_stream: connect_txn_proxy.take_event_stream(),
            connection_attempt_time,
            time_to_connect,
        };

        // Start the state machine in the connected state.
        let initial_state = connected_state(test_values.common_options, options);
        let fut = run_state_machine(initial_state);
        pin_mut!(fut);
        assert_variant!(exec.run_until_stalled(&mut fut), Poll::Pending);

        let disconnect_time = fasync::Time::after(12.hours());
        exec.set_fake_time(disconnect_time);

        // SME notifies Policy of disconnection
        let is_sme_reconnecting = false;
        let fidl_disconnect_info = generate_disconnect_info(is_sme_reconnecting);
        connect_txn_handle
            .send_on_disconnect(&fidl_disconnect_info)
            .expect("failed to send disconnection event");
        assert_variant!(exec.run_until_stalled(&mut fut), Poll::Pending);

        // The disconnect should have been recorded for the saved network config.
        let expected_recorded_connection = ConnectionRecord {
            id: connect_selection.target.network.clone(),
            credential: connect_selection.target.credential.clone(),
            data: PastConnectionData {
                bssid: ap_state.original().bssid,
                connection_attempt_time,
                time_to_connect,
                disconnect_time,
                connection_uptime: zx::Duration::from_hours(12),
                disconnect_reason: types::DisconnectReason::DisconnectDetectedFromSme,
                signal_data_at_disconnect: bss_selection::SignalData::new(
                    bss_description.rssi_dbm,
                    bss_description.snr_db,
                    bss_selection::EWMA_SMOOTHING_FACTOR,
                    bss_selection::EWMA_VELOCITY_SMOOTHING_FACTOR,
                ),
                // TODO: record average phy rate over connection once available
                average_tx_rate: 0,
            },
        };
        assert_variant!(test_values.saved_networks_manager.get_recorded_past_connections().as_slice(), [connection_data] => {
            assert_eq!(connection_data, &expected_recorded_connection);
        });

        assert_variant!(telemetry_receiver.try_next(), Ok(Some(event)) => {
            // Disconnect telemetry event sent
            assert_variant!(event, TelemetryEvent::Disconnected { track_subsequent_downtime, info } => {
                assert!(track_subsequent_downtime);
                assert_eq!(info, DisconnectInfo {
                    connected_duration: 12.hours(),
                    is_sme_reconnecting,
                    disconnect_source: fidl_disconnect_info.disconnect_source,
                    previous_connect_reason: connect_selection.reason,
                    ap_state: ap_state.clone(),
                });
            });
        });
    }

    #[fuchsia::test]
    fn connected_state_reconnect_resets_connected_duration() {
        let mut exec = fasync::TestExecutor::new_with_fake_time();
        exec.set_fake_time(fasync::Time::from_nanos(0));

        let test_values = test_setup();
        let mut telemetry_receiver = test_values.telemetry_receiver;

        let connect_selection = generate_connect_selection();
        let bss_description =
            Sequestered::release(connect_selection.target.bss.bss_description.clone());
        let ap_state =
            types::ApState::from(BssDescription::try_from(bss_description.clone()).unwrap());

        let (connect_txn_proxy, connect_txn_stream) =
            create_proxy_and_stream::<fidl_sme::ConnectTransactionMarker>()
                .expect("failed to create a connect txn channel");
        let connect_txn_handle = connect_txn_stream.control_handle();
        let options = ConnectedOptions {
            currently_fulfilled_connection: connect_selection.clone(),
            ap_state: Box::new(ap_state.clone()),
            multiple_bss_candidates: connect_selection.target.network_has_multiple_bss,
            connect_txn_stream: connect_txn_proxy.take_event_stream(),
            connection_attempt_time: fasync::Time::now(),
            time_to_connect: zx::Duration::from_seconds(10),
        };
        let initial_state = connected_state(test_values.common_options, options);
        let fut = run_state_machine(initial_state);
        pin_mut!(fut);

        // Run the state machine
        assert_variant!(exec.run_until_stalled(&mut fut), Poll::Pending);

        exec.set_fake_time(fasync::Time::after(12.hours()));

        // SME notifies Policy of disconnection with SME-initiated reconnect
        let is_sme_reconnecting = true;
        let fidl_disconnect_info = generate_disconnect_info(is_sme_reconnecting);
        connect_txn_handle
            .send_on_disconnect(&fidl_disconnect_info)
            .expect("failed to send disconnection event");
        assert_variant!(exec.run_until_stalled(&mut fut), Poll::Pending);

        // Disconnect telemetry event sent
        assert_variant!(telemetry_receiver.try_next(), Ok(Some(event)) => {
            assert_variant!(event, TelemetryEvent::Disconnected { info, .. } => {
                assert_eq!(info.connected_duration, 12.hours());
            });
        });

        // SME notifies Policy of reconnection successful
        exec.set_fake_time(fasync::Time::after(1.second()));
        let connect_result =
            fidl_sme::ConnectResult { is_reconnect: true, ..fake_successful_connect_result() };
        connect_txn_handle
            .send_on_connect_result(&connect_result)
            .expect("failed to send connect result event");
        assert_variant!(exec.run_until_stalled(&mut fut), Poll::Pending);

        assert_variant!(
            telemetry_receiver.try_next(),
            Ok(Some(TelemetryEvent::ConnectResult { .. }))
        );

        // SME notifies Policy of another disconnection
        exec.set_fake_time(fasync::Time::after(2.hours()));
        let is_sme_reconnecting = false;
        let fidl_disconnect_info = generate_disconnect_info(is_sme_reconnecting);
        connect_txn_handle
            .send_on_disconnect(&fidl_disconnect_info)
            .expect("failed to send disconnection event");
        assert_variant!(exec.run_until_stalled(&mut fut), Poll::Pending);

        // Another disconnect telemetry event sent
        assert_variant!(telemetry_receiver.try_next(), Ok(Some(event)) => {
            assert_variant!(event, TelemetryEvent::Disconnected { info, .. } => {
                assert_eq!(info.connected_duration, 2.hours());
            });
        });
    }

    #[fuchsia::test]
    fn connected_state_records_unexpected_disconnect_unspecified_bss() {
        let mut exec = fasync::TestExecutor::new_with_fake_time();
        let connection_attempt_time = fasync::Time::from_nanos(0);
        exec.set_fake_time(connection_attempt_time);
        let test_values = test_setup();

        let connect_selection = generate_connect_selection();
        let bss_description =
            Sequestered::release(connect_selection.target.bss.bss_description.clone());

        // Setup for network selection in the connecting state to select the intended network.
        let expected_config = network_config::NetworkConfig::new(
            connect_selection.target.network.clone(),
            connect_selection.target.credential.clone(),
            false,
        )
        .expect("failed to create network config");
        test_values.saved_networks_manager.set_lookup_compatible_response(vec![expected_config]);

        let connecting_options =
            ConnectingOptions { connect_selection: connect_selection.clone(), attempt_counter: 0 };
        let initial_state = connecting_state(test_values.common_options, connecting_options);
        let state_fut = run_state_machine(initial_state);
        pin_mut!(state_fut);
        let sme_fut = test_values.sme_req_stream.into_future();
        pin_mut!(sme_fut);

        // Run the state machine
        assert_variant!(exec.run_until_stalled(&mut state_fut), Poll::Pending);

        let time_to_connect = 10.seconds();
        exec.set_fake_time(fasync::Time::after(time_to_connect));

        // Process connect request sent to SME
        let connect_txn_handle = assert_variant!(
            poll_sme_req(&mut exec, &mut sme_fut),
            Poll::Ready(fidl_sme::ClientSmeRequest::Connect{ req: _, txn, control_handle: _ }) => {
                 // Send connection response.
                let (_stream, ctrl) = txn.expect("connect txn unused")
                    .into_stream_and_control_handle().expect("error accessing control handle");
                ctrl
            }
        );
        connect_txn_handle
            .send_on_connect_result(&fake_successful_connect_result())
            .expect("failed to send connection completion");
        assert_variant!(exec.run_until_stalled(&mut state_fut), Poll::Pending);

        // SME notifies Policy of disconnection.
        let disconnect_time = fasync::Time::after(5.hours());
        exec.set_fake_time(disconnect_time);
        let is_sme_reconnecting = false;
        connect_txn_handle
            .send_on_disconnect(&generate_disconnect_info(is_sme_reconnecting))
            .expect("failed to send disconnection event");
        assert_variant!(exec.run_until_stalled(&mut state_fut), Poll::Pending);

        // The connection data should have been recorded at disconnect.
        let expected_recorded_connection = ConnectionRecord {
            id: connect_selection.target.network.clone(),
            credential: connect_selection.target.credential.clone(),
            data: PastConnectionData {
                bssid: types::Bssid(bss_description.bssid),
                connection_attempt_time,
                time_to_connect,
                disconnect_time,
                connection_uptime: zx::Duration::from_hours(5),
                disconnect_reason: types::DisconnectReason::DisconnectDetectedFromSme,
                signal_data_at_disconnect: bss_selection::SignalData::new(
                    bss_description.rssi_dbm,
                    bss_description.snr_db,
                    bss_selection::EWMA_SMOOTHING_FACTOR,
                    bss_selection::EWMA_VELOCITY_SMOOTHING_FACTOR,
                ),
                average_tx_rate: 0,
            },
        };
        assert_variant!(test_values.saved_networks_manager.get_recorded_past_connections().as_slice(), [connection_data] => {
            assert_eq!(connection_data, &expected_recorded_connection);
        });
    }

    #[fuchsia::test]
    fn connected_state_gets_duplicate_connect_selection() {
        let mut exec = fasync::TestExecutor::new_with_fake_time();
        exec.set_fake_time(fasync::Time::from_nanos(0));
        let test_values = test_setup();
        let mut telemetry_receiver = test_values.telemetry_receiver;

        let connect_selection = generate_connect_selection();
        let bss_description =
            Sequestered::release(connect_selection.target.bss.bss_description.clone());
        let ap_state =
            types::ApState::from(BssDescription::try_from(bss_description.clone()).unwrap());

        let (connect_txn_proxy, _connect_txn_stream) =
            create_proxy_and_stream::<fidl_sme::ConnectTransactionMarker>()
                .expect("failed to create a connect txn channel");
        let options = ConnectedOptions {
            currently_fulfilled_connection: connect_selection.clone(),
            ap_state: Box::new(ap_state.clone()),
            multiple_bss_candidates: connect_selection.target.network_has_multiple_bss,
            connect_txn_stream: connect_txn_proxy.take_event_stream(),
            connection_attempt_time: fasync::Time::now(),
            time_to_connect: zx::Duration::from_seconds(10),
        };
        let initial_state = connected_state(test_values.common_options, options);
        let fut = run_state_machine(initial_state);
        pin_mut!(fut);
        let sme_fut = test_values.sme_req_stream.into_future();
        pin_mut!(sme_fut);

        // Send another duplicate request
        let mut client = Client::new(test_values.client_req_sender);
        client.connect(connect_selection.clone()).expect("failed to make request");

        // Run the state machine
        assert_variant!(exec.run_until_stalled(&mut fut), Poll::Pending);

        // Ensure nothing was sent to the SME
        assert_variant!(poll_sme_req(&mut exec, &mut sme_fut), Poll::Pending);

        // No telemetry event is sent
        assert_variant!(telemetry_receiver.try_next(), Err(_));
    }

    #[fuchsia::test]
    fn connected_state_gets_different_connect_selection() {
        let mut exec = fasync::TestExecutor::new_with_fake_time();
        exec.set_fake_time(fasync::Time::from_nanos(0));

        let mut test_values = test_setup();
        let mut telemetry_receiver = test_values.telemetry_receiver;

        let first_connect_selection = generate_connect_selection();
        let first_bss_desc =
            Sequestered::release(first_connect_selection.target.bss.bss_description.clone());
        let first_ap_state =
            types::ApState::from(BssDescription::try_from(first_bss_desc.clone()).unwrap());
        let second_connect_selection = types::ConnectSelection {
            reason: types::ConnectReason::ProactiveNetworkSwitch,
            ..generate_connect_selection()
        };

        let (connect_txn_proxy, _connect_txn_stream) =
            create_proxy_and_stream::<fidl_sme::ConnectTransactionMarker>()
                .expect("failed to create a connect txn channel");
        let connection_attempt_time = fasync::Time::now();
        let time_to_connect = zx::Duration::from_seconds(10);
        let options = ConnectedOptions {
            currently_fulfilled_connection: first_connect_selection.clone(),
            ap_state: Box::new(first_ap_state.clone()),
            multiple_bss_candidates: first_connect_selection.target.network_has_multiple_bss,
            connect_txn_stream: connect_txn_proxy.take_event_stream(),
            connection_attempt_time,
            time_to_connect,
        };
        let initial_state = connected_state(test_values.common_options, options);
        let fut = run_state_machine(initial_state);
        pin_mut!(fut);
        let sme_fut = test_values.sme_req_stream.into_future();
        pin_mut!(sme_fut);

        // Run the state machine
        assert_variant!(exec.run_until_stalled(&mut fut), Poll::Pending);

        let disconnect_time = fasync::Time::after(12.hours());
        exec.set_fake_time(disconnect_time);

        // Send a different connect request
        let mut client = Client::new(test_values.client_req_sender);
        client.connect(second_connect_selection.clone()).expect("failed to make request");

        // Run the state machine
        assert_variant!(exec.run_until_stalled(&mut fut), Poll::Pending);

        // There should be 2 requests to the SME stacked up
        // First SME request: disconnect
        assert_variant!(
            poll_sme_req(&mut exec, &mut sme_fut),
            Poll::Ready(fidl_sme::ClientSmeRequest::Disconnect{ responder, reason: fidl_sme::UserDisconnectReason::ProactiveNetworkSwitch }) => {
                responder.send().expect("could not send sme response");
            }
        );
        // Progress the state machine
        // TODO(fxbug.dev/53505): remove this once the disconnect request is fire-and-forget
        assert_variant!(exec.run_until_stalled(&mut fut), Poll::Pending);
        // Second SME request: connect to the second network
        let connect_txn_handle = assert_variant!(
            poll_sme_req(&mut exec, &mut sme_fut),
            Poll::Ready(fidl_sme::ClientSmeRequest::Connect{ req, txn, control_handle: _ }) => {
                assert_eq!(req.ssid, second_connect_selection.target.network.ssid.clone().to_vec());
                 // Send connection response.
                let (_stream, ctrl) = txn.expect("connect txn unused")
                    .into_stream_and_control_handle().expect("error accessing control handle");
                ctrl
            }
        );
        connect_txn_handle
            .send_on_connect_result(&fake_successful_connect_result())
            .expect("failed to send connection completion");
        // Progress the state machine
        assert_variant!(exec.run_until_stalled(&mut fut), Poll::Pending);

        // Check for a disconnect update
        let client_state_update = ClientStateUpdate {
            state: fidl_policy::WlanClientState::ConnectionsEnabled,
            networks: vec![ClientNetworkState {
                id: first_connect_selection.target.network.clone(),
                state: fidl_policy::ConnectionState::Disconnected,
                status: Some(fidl_policy::DisconnectStatus::ConnectionStopped),
            }],
        };
        assert_variant!(
            test_values.update_receiver.try_next(),
            Ok(Some(listener::Message::NotifyListeners(updates))) => {
            assert_eq!(updates, client_state_update);
        });

        // Disconnect telemetry event sent
        assert_variant!(telemetry_receiver.try_next(), Ok(Some(event)) => {
            assert_variant!(event, TelemetryEvent::Disconnected { track_subsequent_downtime, info } => {
                assert!(!track_subsequent_downtime);
                assert_eq!(info, DisconnectInfo {
                    connected_duration: 12.hours(),
                    is_sme_reconnecting: false,
                    disconnect_source: fidl_sme::DisconnectSource::User(fidl_sme::UserDisconnectReason::ProactiveNetworkSwitch),
                    previous_connect_reason: first_connect_selection.reason,
                    ap_state: first_ap_state.clone(),
                });
            });
        });

        // Check for a connecting update
        let client_state_update = ClientStateUpdate {
            state: fidl_policy::WlanClientState::ConnectionsEnabled,
            networks: vec![ClientNetworkState {
                id: types::NetworkIdentifier {
                    ssid: second_connect_selection.target.network.ssid.clone(),
                    security_type: types::SecurityType::Wpa2,
                },
                state: fidl_policy::ConnectionState::Connecting,
                status: None,
            }],
        };
        assert_variant!(
            test_values.update_receiver.try_next(),
            Ok(Some(listener::Message::NotifyListeners(updates))) => {
            assert_eq!(updates, client_state_update);
        });
        // Check for a connected update
        let client_state_update = ClientStateUpdate {
            state: fidl_policy::WlanClientState::ConnectionsEnabled,
            networks: vec![ClientNetworkState {
                id: types::NetworkIdentifier {
                    ssid: second_connect_selection.target.network.ssid.clone(),
                    security_type: types::SecurityType::Wpa2,
                },
                state: fidl_policy::ConnectionState::Connected,
                status: None,
            }],
        };
        assert_variant!(
            test_values.update_receiver.try_next(),
            Ok(Some(listener::Message::NotifyListeners(updates))) => {
            assert_eq!(updates, client_state_update);
        });

        // Progress the state machine
        assert_variant!(exec.run_until_stalled(&mut fut), Poll::Pending);

        // Ensure no further updates were sent to listeners
        assert_variant!(
            exec.run_until_stalled(&mut test_values.update_receiver.into_future()),
            Poll::Pending
        );

        // Check that the first connection was recorded
        let expected_recorded_connection = ConnectionRecord {
            id: first_connect_selection.target.network.clone(),
            credential: first_connect_selection.target.credential.clone(),
            data: PastConnectionData {
                bssid: types::Bssid(first_bss_desc.bssid),
                connection_attempt_time,
                time_to_connect,
                disconnect_time,
                connection_uptime: zx::Duration::from_hours(12),
                disconnect_reason: types::DisconnectReason::ProactiveNetworkSwitch,
                signal_data_at_disconnect: bss_selection::SignalData::new(
                    first_bss_desc.rssi_dbm,
                    first_bss_desc.snr_db,
                    bss_selection::EWMA_SMOOTHING_FACTOR,
                    bss_selection::EWMA_VELOCITY_SMOOTHING_FACTOR,
                ),
                // TODO: record average phy rate over connection once available
                average_tx_rate: 0,
            },
        };
        assert_variant!(test_values.saved_networks_manager.get_recorded_past_connections().as_slice(), [connection_data] => {
            assert_eq!(connection_data, &expected_recorded_connection);
        });
    }

    #[fuchsia::test]
    fn connected_state_notified_of_network_disconnect_no_sme_reconnect_short_uptime_no_retry() {
        let mut exec = fasync::TestExecutor::new_with_fake_time();
        let test_values = test_setup();

        let connect_selection = generate_connect_selection();
        let bss_description =
            Sequestered::release(connect_selection.target.bss.bss_description.clone());
        let ap_state =
            types::ApState::from(BssDescription::try_from(bss_description.clone()).unwrap());

        let (connect_txn_proxy, connect_txn_stream) =
            create_proxy_and_stream::<fidl_sme::ConnectTransactionMarker>()
                .expect("failed to create a connect txn channel");
        let connect_txn_handle = connect_txn_stream.control_handle();
        let options = ConnectedOptions {
            currently_fulfilled_connection: connect_selection.clone(),
            ap_state: Box::new(ap_state.clone()),
            multiple_bss_candidates: connect_selection.target.network_has_multiple_bss,
            connect_txn_stream: connect_txn_proxy.take_event_stream(),
            connection_attempt_time: fasync::Time::now(),
            time_to_connect: zx::Duration::from_seconds(10),
        };
        let initial_state = connected_state(test_values.common_options, options);
        let fut = run_state_machine(initial_state);
        pin_mut!(fut);
        let sme_fut = test_values.sme_req_stream.into_future();
        pin_mut!(sme_fut);

        // Run the state machine
        assert_variant!(exec.run_until_stalled(&mut fut), Poll::Pending);

        // SME notifies Policy of disconnection.
        let is_sme_reconnecting = false;
        connect_txn_handle
            .send_on_disconnect(&generate_disconnect_info(is_sme_reconnecting))
            .expect("failed to send disconnection event");

        // Run the state machine
        assert_variant!(exec.run_until_stalled(&mut fut), Poll::Pending);

        // Check for a disconnect request to SME
        assert_variant!(
            poll_sme_req(&mut exec, &mut sme_fut),
            Poll::Ready(fidl_sme::ClientSmeRequest::Disconnect{ responder, reason: fidl_sme::UserDisconnectReason::DisconnectDetectedFromSme }) => {
                responder.send().expect("could not send sme response");
            }
        );

        // The state machine should exit since there is no attempt to reconnect.
        assert_variant!(exec.run_until_stalled(&mut fut), Poll::Ready(()));
    }

    #[fuchsia::test]
    fn connected_state_notified_of_network_disconnect_sme_reconnect_successfully() {
        let mut exec = fasync::TestExecutor::new();
        let mut test_values = test_setup();

        let connect_selection = generate_connect_selection();
        let bss_description =
            Sequestered::release(connect_selection.target.bss.bss_description.clone());
        let ap_state =
            types::ApState::from(BssDescription::try_from(bss_description.clone()).unwrap());

        let (connect_txn_proxy, connect_txn_stream) =
            create_proxy_and_stream::<fidl_sme::ConnectTransactionMarker>()
                .expect("failed to create a connect txn channel");
        let connect_txn_handle = connect_txn_stream.control_handle();
        let options = ConnectedOptions {
            currently_fulfilled_connection: connect_selection.clone(),
            ap_state: Box::new(ap_state.clone()),
            multiple_bss_candidates: connect_selection.target.network_has_multiple_bss,
            connect_txn_stream: connect_txn_proxy.take_event_stream(),
            connection_attempt_time: fasync::Time::now(),
            time_to_connect: zx::Duration::from_seconds(10),
        };
        let initial_state = connected_state(test_values.common_options, options);
        let fut = run_state_machine(initial_state);
        pin_mut!(fut);

        // Run the state machine
        assert_variant!(exec.run_until_stalled(&mut fut), Poll::Pending);

        // SME notifies Policy of disconnection
        let is_sme_reconnecting = true;
        connect_txn_handle
            .send_on_disconnect(&generate_disconnect_info(is_sme_reconnecting))
            .expect("failed to send disconnection event");

        // Run the state machine
        assert_variant!(exec.run_until_stalled(&mut fut), Poll::Pending);

        // SME notifies Policy that reconnects succeeds
        let connect_result =
            fidl_sme::ConnectResult { is_reconnect: true, ..fake_successful_connect_result() };
        connect_txn_handle
            .send_on_connect_result(&connect_result)
            .expect("failed to send reconnection result");

        // Run the state machine
        assert_variant!(exec.run_until_stalled(&mut fut), Poll::Pending);

        // Check there were no state updates
        assert_variant!(test_values.update_receiver.try_next(), Err(_));
    }

    #[fuchsia::test]
    fn connected_state_notified_of_network_disconnect_sme_reconnect_unsuccessfully() {
        let mut exec = fasync::TestExecutor::new_with_fake_time();
        let mut test_values = test_setup();
        let connect_selection = generate_connect_selection();
        let bss_description =
            Sequestered::release(connect_selection.target.bss.bss_description.clone());
        let ap_state =
            types::ApState::from(BssDescription::try_from(bss_description.clone()).unwrap());

        // Set the start time of the connection
        let start_time = fasync::Time::now();
        exec.set_fake_time(start_time);

        let (connect_txn_proxy, connect_txn_stream) =
            create_proxy_and_stream::<fidl_sme::ConnectTransactionMarker>()
                .expect("failed to create a connect txn channel");
        let connect_txn_handle = connect_txn_stream.control_handle();
        let options = ConnectedOptions {
            currently_fulfilled_connection: connect_selection.clone(),
            ap_state: Box::new(ap_state),
            multiple_bss_candidates: connect_selection.target.network_has_multiple_bss,
            connect_txn_stream: connect_txn_proxy.take_event_stream(),
            connection_attempt_time: fasync::Time::now(),
            time_to_connect: zx::Duration::from_seconds(10),
        };
        let initial_state = connected_state(test_values.common_options, options);
        let fut = run_state_machine(initial_state);
        pin_mut!(fut);
        let sme_fut = test_values.sme_req_stream.into_future();
        pin_mut!(sme_fut);

        // Run the state machine
        assert_variant!(exec.run_until_stalled(&mut fut), Poll::Pending);

        // Set time to indicate a decent uptime before the disconnect so the AP is retried
        exec.set_fake_time(start_time + fasync::Duration::from_hours(24));

        // SME notifies Policy of disconnection
        let is_sme_reconnecting = true;
        connect_txn_handle
            .send_on_disconnect(&generate_disconnect_info(is_sme_reconnecting))
            .expect("failed to send disconnection event");

        // Run the state machine
        assert_variant!(exec.run_until_stalled(&mut fut), Poll::Pending);

        // SME notifies Policy that reconnects fails
        let connect_result = fidl_sme::ConnectResult {
            code: fidl_ieee80211::StatusCode::RefusedReasonUnspecified,
            is_reconnect: true,
            ..fake_successful_connect_result()
        };
        connect_txn_handle
            .send_on_connect_result(&connect_result)
            .expect("failed to send reconnection result");

        // Run the state machine
        assert_variant!(exec.run_until_stalled(&mut fut), Poll::Pending);

        // Check for an SME disconnect request
        assert_variant!(
            poll_sme_req(&mut exec, &mut sme_fut),
            Poll::Ready(fidl_sme::ClientSmeRequest::Disconnect{ responder, reason: fidl_sme::UserDisconnectReason::DisconnectDetectedFromSme }) => {
                responder.send().expect("could not send sme response");
            }
        );

        // The state machine should exit since there is no policy attempt to reconnect.
        assert_variant!(exec.run_until_stalled(&mut fut), Poll::Ready(()));

        // Check for a disconnect update
        let client_state_update = ClientStateUpdate {
            state: fidl_policy::WlanClientState::ConnectionsEnabled,
            networks: vec![ClientNetworkState {
                id: connect_selection.target.network.clone(),
                state: fidl_policy::ConnectionState::Disconnected,
                status: Some(fidl_policy::DisconnectStatus::ConnectionFailed),
            }],
        };
        assert_variant!(
            test_values.update_receiver.try_next(),
            Ok(Some(listener::Message::NotifyListeners(updates))) => {
            assert_eq!(updates, client_state_update);
        });
    }

    #[fuchsia::test]
    fn connected_state_on_signal_report() {
        let mut exec = fasync::TestExecutor::new_with_fake_time();
        exec.set_fake_time(fasync::Time::from_nanos(0));

        let mut test_values = test_setup();
        let mut telemetry_receiver = test_values.telemetry_receiver;

        // Set initial RSSI and SNR values
        let mut connect_selection = generate_connect_selection();
        let init_rssi = -40;
        let init_snr = 30;
        connect_selection.target.bss.rssi = init_rssi;
        connect_selection.target.bss.snr_db = init_snr;

        let mut bss_description =
            Sequestered::release(connect_selection.target.bss.bss_description.clone());
        bss_description.rssi_dbm = init_rssi;
        bss_description.snr_db = init_snr;
        connect_selection.target.bss.bss_description = bss_description.clone().into();

        let ap_state =
            types::ApState::from(BssDescription::try_from(bss_description.clone()).unwrap());

        // Add a PastConnectionData for the connected network to be send in BSS quality data.
        let mut past_connections = PastConnectionList::new();
        let mut past_connection_data = random_connection_data();
        past_connection_data.bssid = ieee80211::Bssid(bss_description.bssid);
        past_connections.add(past_connection_data);
        let mut saved_networks_manager = FakeSavedNetworksManager::new();
        saved_networks_manager.past_connections_response = past_connections.clone();
        test_values.common_options.saved_networks_manager = Arc::new(saved_networks_manager);

        // Set up the state machine, starting at the connected state.
        let (connect_txn_proxy, connect_txn_stream) =
            create_proxy_and_stream::<fidl_sme::ConnectTransactionMarker>()
                .expect("failed to create a connect txn channel");
        let options = ConnectedOptions {
            currently_fulfilled_connection: connect_selection.clone(),
            ap_state: Box::new(ap_state),
            multiple_bss_candidates: connect_selection.target.network_has_multiple_bss,
            connect_txn_stream: connect_txn_proxy.take_event_stream(),
            connection_attempt_time: fasync::Time::now(),
            time_to_connect: zx::Duration::from_seconds(10),
        };
        let initial_state = connected_state(test_values.common_options, options);

        let connect_txn_handle = connect_txn_stream.control_handle();
        let fut = run_state_machine(initial_state);
        pin_mut!(fut);
        let sme_fut = test_values.sme_req_stream.into_future();
        pin_mut!(sme_fut);

        // Send the first signal report from SME
        let rssi_1 = -50;
        let snr_1 = 25;
        let fidl_signal_report =
            fidl_internal::SignalReportIndication { rssi_dbm: rssi_1, snr_db: snr_1 };
        connect_txn_handle
            .send_on_signal_report(&fidl_signal_report)
            .expect("failed to send signal report");
        assert_variant!(exec.run_until_stalled(&mut fut), Poll::Pending);

        // Verify telemetry event for signal report data then RSSI data.
        assert_variant!(telemetry_receiver.try_next(), Ok(Some(TelemetryEvent::OnSignalReport {ind, rssi_velocity})) => {
            assert_eq!(ind, fidl_signal_report);
            // verify that RSSI velocity is negative since the signal report RSSI is lower.
            assert_lt!(rssi_velocity, 0);
        });

        // Do a quick check that state machine does not exist and there's no disconnect to SME
        assert_variant!(poll_sme_req(&mut exec, &mut sme_fut), Poll::Pending);

        // Verify that connection stats are sent out
        let stats = test_values
            .stats_receiver
            .try_next()
            .expect("failed to get connection stats")
            .expect("next connection stats is missing");
        // Test setup always use iface ID 1.
        assert_eq!(stats.iface_id, 1);
        assert_eq!(stats.id, connect_selection.target.network.clone());
        // EWMA RSSI and SNR should be between the initial and the newest values.
        let ewma_rssi_1 = stats.quality_data.signal_data.ewma_rssi;
        assert_lt!(ewma_rssi_1.get(), init_rssi as f64);
        assert_gt!(ewma_rssi_1.get(), rssi_1 as f64);
        let ewma_snr_1 = stats.quality_data.signal_data.ewma_snr;
        assert_lt!(ewma_snr_1.get(), init_snr as f64);
        assert_gt!(ewma_snr_1.get(), snr_1 as f64);
        // Check that RSSI velocity is negative.
        let rssi_velocity_1 = stats.quality_data.signal_data.ewma_rssi_velocity.get();
        assert_lt!(rssi_velocity_1, 0.0);
        // Check that the BssQualityData includes the past connection data.
        assert_eq!(stats.quality_data.past_connections_list, past_connections.clone());
        // Check that the channel is included.
        assert_eq!(
            stats.quality_data.channel,
            wlan_common::channel::Channel::try_from(bss_description.channel).unwrap()
        );

        // Send a second signal report with higher RSSI and SNR than the previous reports.
        let rssi_1 = -30;
        let snr_1 = 35;
        let fidl_signal_report =
            fidl_internal::SignalReportIndication { rssi_dbm: rssi_1, snr_db: snr_1 };
        connect_txn_handle
            .send_on_signal_report(&fidl_signal_report)
            .expect("failed to send signal report");
        assert_variant!(exec.run_until_stalled(&mut fut), Poll::Pending);

        // Verify that a telemetry event is sent
        assert_variant!(telemetry_receiver.try_next(), Ok(Some(event)) => {
            assert_variant!(event, TelemetryEvent::OnSignalReport { ind, rssi_velocity } => {
                assert_eq!(ind, fidl_signal_report);
                assert_gt!(rssi_velocity, 0);
            });
        });

        // Verify that the new EWMA values are higher than the previous values.
        let stats = test_values
            .stats_receiver
            .try_next()
            .expect("failed to get connection stats")
            .expect("next connection stats is missing");
        assert_eq!(stats.iface_id, 1);
        assert_eq!(stats.id, connect_selection.target.network.clone());
        // Check that EWMA RSSI and SNR values are greater than the previous values.
        assert_gt!(stats.quality_data.signal_data.ewma_rssi.get(), ewma_rssi_1.get());
        assert_gt!(stats.quality_data.signal_data.ewma_snr.get(), ewma_snr_1.get());
        // Check that RSSI velocity is greater than the previous velocity.
        assert_gt!(stats.quality_data.signal_data.ewma_rssi_velocity.get(), rssi_velocity_1);
        // Check that the BssQualityData includes the past connection data.
        assert_eq!(stats.quality_data.past_connections_list, past_connections);
        // Check that the channel is included.
        assert_eq!(
            stats.quality_data.channel,
            wlan_common::channel::Channel::try_from(bss_description.channel).unwrap()
        );
    }

    #[fuchsia::test]
    fn connected_state_should_roam() {
        let mut exec = fasync::TestExecutor::new_with_fake_time();
        exec.set_fake_time(fasync::Time::from_nanos(0));

        let test_values = test_setup();
        let mut telemetry_receiver = test_values.telemetry_receiver;

        let mut connect_selection = generate_connect_selection();
        let init_rssi = -75;
        let init_snr = 30;

        // Set initial RSSI and SNR values
        connect_selection.target.bss.rssi = init_rssi;
        connect_selection.target.bss.snr_db = init_snr;

        // Set initial RSSI and SNR values in bss description
        let mut bss_description =
            Sequestered::release(connect_selection.target.bss.bss_description.clone());
        bss_description.rssi_dbm = init_rssi;
        bss_description.snr_db = init_snr;
        connect_selection.target.bss.bss_description = bss_description.clone().into();

        let ap_state =
            types::ApState::from(BssDescription::try_from(bss_description.clone()).unwrap());

        // Set up the state machine, starting at the connected state.
        let (connect_txn_proxy, connect_txn_stream) =
            create_proxy_and_stream::<fidl_sme::ConnectTransactionMarker>()
                .expect("failed to create a connect txn channel");
        let options = ConnectedOptions {
            currently_fulfilled_connection: connect_selection.clone(),
            ap_state: Box::new(ap_state),
            multiple_bss_candidates: connect_selection.target.network_has_multiple_bss,
            connect_txn_stream: connect_txn_proxy.take_event_stream(),
            connection_attempt_time: fasync::Time::now(),
            time_to_connect: zx::Duration::from_seconds(10),
        };
        let initial_state = connected_state(test_values.common_options, options);

        let connect_txn_handle = connect_txn_stream.control_handle();
        let fut = run_state_machine(initial_state);
        pin_mut!(fut);

        // Send a signal report indicating the connection is weak.
        let rssi_1 = -90;
        let snr_1 = 25;
        let fidl_signal_report =
            fidl_internal::SignalReportIndication { rssi_dbm: rssi_1, snr_db: snr_1 };
        connect_txn_handle
            .send_on_signal_report(&fidl_signal_report)
            .expect("failed to send signal report");
        assert_variant!(exec.run_until_stalled(&mut fut), Poll::Pending);

        // Verify that telemetry events are sent for the signal report and a Roam Scan.
        assert_variant!(
            telemetry_receiver.try_next(),
            Ok(Some(TelemetryEvent::OnSignalReport {ind, rssi_velocity: _})) => {
                assert_eq!(ind, fidl_signal_report);
            }
        );
        assert_variant!(telemetry_receiver.try_next(), Ok(Some(TelemetryEvent::RoamingScan {})));
    }

    #[fuchsia::test]
    fn connected_state_on_channel_switched() {
        let mut exec = fasync::TestExecutor::new_with_fake_time();
        exec.set_fake_time(fasync::Time::from_nanos(0));

        let test_values = test_setup();
        let mut telemetry_receiver = test_values.telemetry_receiver;

        let connect_selection = generate_connect_selection();
        let bss_description =
            Sequestered::release(connect_selection.target.bss.bss_description.clone());
        let ap_state =
            types::ApState::from(BssDescription::try_from(bss_description.clone()).unwrap());

        // Set up the state machine, starting at the connected state.
        let (connect_txn_proxy, connect_txn_stream) =
            create_proxy_and_stream::<fidl_sme::ConnectTransactionMarker>()
                .expect("failed to create a connect txn channel");
        let options = ConnectedOptions {
            currently_fulfilled_connection: connect_selection.clone(),
            ap_state: Box::new(ap_state),
            multiple_bss_candidates: connect_selection.target.network_has_multiple_bss,
            connect_txn_stream: connect_txn_proxy.take_event_stream(),
            connection_attempt_time: fasync::Time::now(),
            time_to_connect: zx::Duration::from_seconds(10),
        };
        let initial_state = connected_state(test_values.common_options, options);

        let connect_txn_handle = connect_txn_stream.control_handle();
        let fut = run_state_machine(initial_state);
        pin_mut!(fut);

        // Run the state machine
        assert_variant!(exec.run_until_stalled(&mut fut), Poll::Pending);

        let channel_switch_info = fidl_internal::ChannelSwitchInfo { new_channel: 10 };
        connect_txn_handle
            .send_on_channel_switched(&channel_switch_info)
            .expect("failed to send signal report");
        assert_variant!(exec.run_until_stalled(&mut fut), Poll::Pending);

        // Verify telemetry event
        assert_variant!(telemetry_receiver.try_next(), Ok(Some(event)) => {
            assert_variant!(event, TelemetryEvent::OnChannelSwitched { info } => {
                assert_eq!(info, channel_switch_info);
            });
        });

        // Have SME notify Policy of disconnection so we can see whether the channel in the
        // BssDescription has changed.
        let is_sme_reconnecting = false;
        let fidl_disconnect_info = generate_disconnect_info(is_sme_reconnecting);
        connect_txn_handle
            .send_on_disconnect(&fidl_disconnect_info)
            .expect("failed to send disconnection event");
        assert_variant!(exec.run_until_stalled(&mut fut), Poll::Pending);

        // Verify telemetry event
        assert_variant!(telemetry_receiver.try_next(), Ok(Some(event)) => {
            assert_variant!(event, TelemetryEvent::Disconnected { info, .. } => {
                assert_eq!(info.ap_state.tracked.channel.primary, 10);
            });
        });
    }

    #[fuchsia::test]
    fn disconnecting_state_completes_and_exits() {
        let mut exec = fasync::TestExecutor::new();
        let mut test_values = test_setup();

        let (sender, _) = oneshot::channel();
        let disconnecting_options = DisconnectingOptions {
            disconnect_responder: Some(sender),
            previous_network: None,
            next_network: None,
            reason: types::DisconnectReason::RegulatoryRegionChange,
        };
        let initial_state = disconnecting_state(test_values.common_options, disconnecting_options);
        let fut = run_state_machine(initial_state);
        pin_mut!(fut);
        let sme_fut = test_values.sme_req_stream.into_future();
        pin_mut!(sme_fut);

        // Run the state machine
        assert_variant!(exec.run_until_stalled(&mut fut), Poll::Pending);

        // Ensure a disconnect request is sent to the SME
        assert_variant!(
            poll_sme_req(&mut exec, &mut sme_fut),
            Poll::Ready(fidl_sme::ClientSmeRequest::Disconnect{ responder, reason: fidl_sme::UserDisconnectReason::RegulatoryRegionChange }) => {
                responder.send().expect("could not send sme response");
            }
        );

        // Ensure the state machine exits once the disconnect is processed.
        assert_variant!(exec.run_until_stalled(&mut fut), Poll::Ready(()));

        // The state machine should have sent a listener update
        assert_variant!(
            test_values.update_receiver.try_next(),
            Ok(Some(listener::Message::NotifyListeners(ClientStateUpdate {
                state: fidl_policy::WlanClientState::ConnectionsEnabled,
                networks
            }))) => {
                assert!(networks.is_empty());
            }
        );
    }

    #[fuchsia::test]
    fn disconnecting_state_completes_disconnect_to_connecting() {
        let mut exec = fasync::TestExecutor::new();
        let mut test_values = test_setup();

        let previous_connect_selection = generate_connect_selection();
        let next_connect_selection = generate_connect_selection();

        let bss_description =
            Sequestered::release(next_connect_selection.target.bss.bss_description.clone());

        let (disconnect_sender, mut disconnect_receiver) = oneshot::channel();
        let connecting_options = ConnectingOptions {
            connect_selection: next_connect_selection.clone(),
            attempt_counter: 0,
        };
        // Include both a "previous" and "next" network
        let disconnecting_options = DisconnectingOptions {
            disconnect_responder: Some(disconnect_sender),
            previous_network: Some((
                previous_connect_selection.target.network.clone(),
                fidl_policy::DisconnectStatus::ConnectionStopped,
            )),
            next_network: Some(connecting_options),
            reason: types::DisconnectReason::ProactiveNetworkSwitch,
        };
        let initial_state = disconnecting_state(test_values.common_options, disconnecting_options);
        let fut = run_state_machine(initial_state);
        pin_mut!(fut);
        let sme_fut = test_values.sme_req_stream.into_future();
        pin_mut!(sme_fut);

        // Run the state machine
        assert_variant!(exec.run_until_stalled(&mut fut), Poll::Pending);

        // Ensure a disconnect request is sent to the SME
        assert_variant!(
            poll_sme_req(&mut exec, &mut sme_fut),
            Poll::Ready(fidl_sme::ClientSmeRequest::Disconnect{ responder, reason: fidl_sme::UserDisconnectReason::ProactiveNetworkSwitch }) => {
                responder.send().expect("could not send sme response");
            }
        );

        // Progress the state machine
        assert_variant!(exec.run_until_stalled(&mut fut), Poll::Pending);

        // Check for a disconnect update and the disconnect responder
        let client_state_update = ClientStateUpdate {
            state: fidl_policy::WlanClientState::ConnectionsEnabled,
            networks: vec![ClientNetworkState {
                id: previous_connect_selection.target.network.clone(),
                state: fidl_policy::ConnectionState::Disconnected,
                status: Some(fidl_policy::DisconnectStatus::ConnectionStopped),
            }],
        };
        assert_variant!(
            test_values.update_receiver.try_next(),
            Ok(Some(listener::Message::NotifyListeners(updates))) => {
            assert_eq!(updates, client_state_update);
        });

        assert_variant!(exec.run_until_stalled(&mut disconnect_receiver), Poll::Ready(Ok(())));

        // Ensure a connect request is sent to the SME
        assert_variant!(
            poll_sme_req(&mut exec, &mut sme_fut),
            Poll::Ready(fidl_sme::ClientSmeRequest::Connect{ req, txn, control_handle: _ }) => {
                assert_eq!(req.ssid, next_connect_selection.target.network.ssid.clone().to_vec());
                assert_eq!(req.deprecated_scan_type, fidl_fuchsia_wlan_common::ScanType::Active);
                assert_eq!(req.bss_description, bss_description.clone());
                assert_eq!(req.multiple_bss_candidates, next_connect_selection.target.network_has_multiple_bss);
                 // Send connection response.
                let (_stream, ctrl) = txn.expect("connect txn unused")
                    .into_stream_and_control_handle().expect("error accessing control handle");
                ctrl
                    .send_on_connect_result(&fake_successful_connect_result())
                    .expect("failed to send connection completion");
            }
        );
    }

    #[fuchsia::test]
    fn disconnecting_state_has_broken_sme() {
        let mut exec = fasync::TestExecutor::new();
        let test_values = test_setup();

        let (sender, mut receiver) = oneshot::channel();
        let disconnecting_options = DisconnectingOptions {
            disconnect_responder: Some(sender),
            previous_network: None,
            next_network: None,
            reason: types::DisconnectReason::NetworkConfigUpdated,
        };
        let initial_state = disconnecting_state(test_values.common_options, disconnecting_options);
        let fut = run_state_machine(initial_state);
        pin_mut!(fut);

        // Break the SME by dropping the server end of the SME stream, so it causes an error
        drop(test_values.sme_req_stream);

        // Ensure the state machine exits
        assert_variant!(exec.run_until_stalled(&mut fut), Poll::Ready(()));

        // Expect the responder to have an error
        assert_variant!(exec.run_until_stalled(&mut receiver), Poll::Ready(Err(_)));
    }

    #[fuchsia::test]
    fn serve_loop_handles_startup() {
        let mut exec = fasync::TestExecutor::new();
        let test_values = test_setup();
        let sme_proxy = test_values.common_options.proxy;
        let sme_event_stream = sme_proxy.take_event_stream();
        let (_client_req_sender, client_req_stream) = mpsc::channel(1);
        let sme_fut = test_values.sme_req_stream.into_future();
        pin_mut!(sme_fut);

        // Create a connect request so that the state machine does not immediately exit.
        let connect_selection = generate_connect_selection();

        let fut = serve(
            0,
            sme_proxy,
            sme_event_stream,
            client_req_stream,
            test_values.common_options.update_sender,
            test_values.common_options.saved_networks_manager,
            Some(connect_selection),
            test_values.common_options.telemetry_sender,
            test_values.common_options.stats_sender,
            test_values.common_options.defect_sender,
        );
        pin_mut!(fut);

        // Run the state machine so it sends the initial SME disconnect request.
        assert_variant!(exec.run_until_stalled(&mut fut), Poll::Pending);
        assert_variant!(
            poll_sme_req(&mut exec, &mut sme_fut),
            Poll::Ready(fidl_sme::ClientSmeRequest::Disconnect{ responder, reason: fidl_sme::UserDisconnectReason::Startup }) => {
                responder.send().expect("could not send sme response");
            }
        );

        // Run the future again and ensure that it has not exited after receiving the response.
        assert_variant!(exec.run_until_stalled(&mut fut), Poll::Pending);
    }

    #[fuchsia::test]
    fn serve_loop_handles_sme_disappearance() {
        let mut exec = fasync::TestExecutor::new();
        let test_values = test_setup();
        let (_client_req_sender, client_req_stream) = mpsc::channel(1);

        // Make our own SME proxy for this test
        let (sme_proxy, sme_server) =
            create_proxy::<fidl_sme::ClientSmeMarker>().expect("failed to create an sme channel");
        let (sme_req_stream, sme_control_handle) = sme_server
            .into_stream_and_control_handle()
            .expect("could not create SME request stream");

        let sme_fut = sme_req_stream.into_future();
        pin_mut!(sme_fut);

        let sme_event_stream = sme_proxy.take_event_stream();

        // Create a connect request so that the state machine does not immediately exit.
        let connect_selection = generate_connect_selection();

        let fut = serve(
            0,
            sme_proxy,
            sme_event_stream,
            client_req_stream,
            test_values.common_options.update_sender,
            test_values.common_options.saved_networks_manager,
            Some(connect_selection),
            test_values.common_options.telemetry_sender,
            test_values.common_options.stats_sender,
            test_values.common_options.defect_sender,
        );
        pin_mut!(fut);

        // Run the state machine so it sends the initial SME disconnect request.
        assert_variant!(exec.run_until_stalled(&mut fut), Poll::Pending);
        assert_variant!(
            poll_sme_req(&mut exec, &mut sme_fut),
            Poll::Ready(fidl_sme::ClientSmeRequest::Disconnect{ responder, reason: fidl_sme::UserDisconnectReason::Startup }) => {
                responder.send().expect("could not send sme response");
            }
        );

        // Run the future again and ensure that it has not exited after receiving the response.
        assert_variant!(exec.run_until_stalled(&mut fut), Poll::Pending);

        sme_control_handle.shutdown_with_epitaph(zx::Status::UNAVAILABLE);

        // Ensure the state machine has no further actions and is exited
        assert_variant!(exec.run_until_stalled(&mut fut), Poll::Ready(()));
    }

    #[fuchsia::test]
    fn serve_loop_handles_disconnect() {
        let mut exec = fasync::TestExecutor::new();
        let test_values = test_setup();
        let sme_proxy = test_values.common_options.proxy;
        let sme_event_stream = sme_proxy.take_event_stream();
        let (client_req_sender, client_req_stream) = mpsc::channel(1);
        let sme_fut = test_values.sme_req_stream.into_future();
        pin_mut!(sme_fut);

        // Create a connect request so that the state machine does not immediately exit.
        let connect_selection = generate_connect_selection();
        let fut = serve(
            0,
            sme_proxy,
            sme_event_stream,
            client_req_stream,
            test_values.common_options.update_sender,
            test_values.common_options.saved_networks_manager,
            Some(connect_selection),
            test_values.common_options.telemetry_sender,
            test_values.common_options.stats_sender,
            test_values.common_options.defect_sender,
        );
        pin_mut!(fut);

        // Run the state machine so it sends the initial SME disconnect request.
        assert_variant!(exec.run_until_stalled(&mut fut), Poll::Pending);
        assert_variant!(
            poll_sme_req(&mut exec, &mut sme_fut),
            Poll::Ready(fidl_sme::ClientSmeRequest::Disconnect{ responder, reason: fidl_sme::UserDisconnectReason::Startup }) => {
                responder.send().expect("could not send sme response");
            }
        );

        // Run the future again and ensure that it has not exited after receiving the response.
        assert_variant!(exec.run_until_stalled(&mut fut), Poll::Pending);

        // Absorb the connect request.
        let connect_txn_handle = assert_variant!(
            poll_sme_req(&mut exec, &mut sme_fut),
            Poll::Ready(fidl_sme::ClientSmeRequest::Connect{ req: _, txn, control_handle: _ }) => {
                // Send connection response.
                let (_stream, ctrl) = txn.expect("connect txn unused")
                    .into_stream_and_control_handle().expect("error accessing control handle");
                ctrl
            }
        );
        connect_txn_handle
            .send_on_connect_result(&fake_successful_connect_result())
            .expect("failed to send connection completion");
        assert_variant!(exec.run_until_stalled(&mut fut), Poll::Pending);

        // Send a disconnect request
        let mut client = Client::new(client_req_sender);
        let (sender, mut receiver) = oneshot::channel();
        client
            .disconnect(
                PolicyDisconnectionMigratedMetricDimensionReason::NetworkConfigUpdated,
                sender,
            )
            .expect("failed to make request");

        // Run the state machine so that it handles the disconnect message.
        assert_variant!(exec.run_until_stalled(&mut fut), Poll::Pending);
        assert_variant!(
            poll_sme_req(&mut exec, &mut sme_fut),
            Poll::Ready(fidl_sme::ClientSmeRequest::Disconnect{ responder, reason: fidl_sme::UserDisconnectReason::NetworkConfigUpdated }) => {
                responder.send().expect("could not send sme response");
            }
        );

        // The state machine should exit following the disconnect request.
        assert_variant!(exec.run_until_stalled(&mut fut), Poll::Ready(()));

        // Expect the responder to be acknowledged
        assert_variant!(exec.run_until_stalled(&mut receiver), Poll::Ready(Ok(())));
    }

    #[fuchsia::test]
    fn serve_loop_handles_state_machine_error() {
        let mut exec = fasync::TestExecutor::new();
        let test_values = test_setup();
        let sme_proxy = test_values.common_options.proxy;
        let sme_event_stream = sme_proxy.take_event_stream();
        let (_client_req_sender, client_req_stream) = mpsc::channel(1);

        // Create a connect request so that the state machine does not immediately exit.
        let connect_selection = generate_connect_selection();

        let fut = serve(
            0,
            sme_proxy,
            sme_event_stream,
            client_req_stream,
            test_values.common_options.update_sender,
            test_values.common_options.saved_networks_manager,
            Some(connect_selection),
            test_values.common_options.telemetry_sender,
            test_values.common_options.stats_sender,
            test_values.common_options.defect_sender,
        );
        pin_mut!(fut);

        // Drop the server end of the SME stream, so it causes an error
        drop(test_values.sme_req_stream);

        // Ensure the state machine exits
        assert_variant!(exec.run_until_stalled(&mut fut), Poll::Ready(()));
    }

    fn fake_successful_connect_result() -> fidl_sme::ConnectResult {
        fidl_sme::ConnectResult {
            code: fidl_ieee80211::StatusCode::Success,
            is_credential_rejected: false,
            is_reconnect: false,
        }
    }
}
