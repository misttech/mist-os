// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

mod convert;
mod inspect_time_series;
mod windowed_stats;

use crate::client;
use crate::mode_management::{Defect, IfaceFailure};
use crate::telemetry::inspect_time_series::TimeSeriesStats;
use crate::telemetry::windowed_stats::WindowedStats;
use crate::util::historical_list::{HistoricalList, Timestamped};
use crate::util::pseudo_energy::{EwmaSignalData, RssiVelocity};
use anyhow::{format_err, Context, Error};
use cobalt_client::traits::AsEventCode;
use fidl_fuchsia_metrics::{MetricEvent, MetricEventPayload};
use fuchsia_async::{self as fasync, TimeoutExt};
use fuchsia_inspect::{
    ArrayProperty, InspectType, Inspector, LazyNode, Node as InspectNode, NumericProperty,
    Property, UintProperty,
};
use fuchsia_inspect_auto_persist::{self as auto_persist, AutoPersist};
use fuchsia_inspect_contrib::inspectable::{InspectableBool, InspectableU64};
use fuchsia_inspect_contrib::log::{InspectBytes, InspectList};
use fuchsia_inspect_contrib::nodes::BoundedListNode;
use fuchsia_inspect_contrib::{inspect_insert, inspect_log, make_inspect_loggable};
use fuchsia_sync::Mutex;
use futures::channel::{mpsc, oneshot};
use futures::{select, Future, FutureExt, StreamExt};
use ieee80211::OuiFmt;
use log::{error, info, warn};
use num_traits::SaturatingAdd;
use static_assertions::const_assert_eq;
use std::cmp::{max, min, Reverse};
use std::collections::{HashMap, HashSet};
use std::ops::Add;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use {
    fidl_fuchsia_wlan_ieee80211 as fidl_ieee80211, fidl_fuchsia_wlan_internal as fidl_internal,
    fidl_fuchsia_wlan_sme as fidl_sme, wlan_metrics_registry as metrics,
};

// Include a timeout on stats calls so that if the driver deadlocks, telemtry doesn't get stuck.
const GET_IFACE_STATS_TIMEOUT: zx::MonotonicDuration = zx::MonotonicDuration::from_seconds(5);
// If there are commands to turn off then turn on client connections within this amount of time
// through the policy API, it is likely that a user intended to restart WLAN connections.
const USER_RESTART_TIME_THRESHOLD: zx::MonotonicDuration = zx::MonotonicDuration::from_seconds(5);
// Short duration connection for metrics purposes.
pub const METRICS_SHORT_CONNECT_DURATION: zx::MonotonicDuration =
    zx::MonotonicDuration::from_seconds(90);
// Minimum connection duration for logging average connection score deltas.
pub const AVERAGE_SCORE_DELTA_MINIMUM_DURATION: zx::MonotonicDuration =
    zx::MonotonicDuration::from_seconds(30);
// Maximum value of reason code accepted by cobalt metrics (set by max_event_code)
pub const COBALT_REASON_CODE_MAX: u16 = 1000;
// Time between cobalt error reports to prevent cluttering up the syslog.
pub const MINUTES_BETWEEN_COBALT_SYSLOG_WARNINGS: i64 = 60;
/// Number of previous RSSI measurements to exponentially weigh into average.
/// TODO(https://fxbug.dev/42165706): Tune smoothing factor.
pub const EWMA_SMOOTHING_FACTOR_FOR_METRICS: usize = 10;

#[allow(non_camel_case_types)]
// Temporary metric ids while renaming. See go/tq-cobalt-registry-changes#renaming-entities.
const ROAMING_DISCONNECT_PER_DAY_CONNECTED_METRIC_ID: u32 = 2008;
const NON_ROAMING_DISCONNECT_PER_DAY_CONNECTED_METRIC_ID: u32 = 2009;
const CONNECTED_DURATION_BEFORE_ROAMING_DISCONNECT_METRIC_ID: u32 = 4030;
const NETWORK_ROAMING_DISCONNECT_COUNTS_METRIC_ID: u32 = 4027;
const CONNECTED_DURATION_BEFORE_NON_ROAMING_DISCONNECT_METRIC_ID: u32 = 4031;
const NETWORK_NON_ROAMING_DISCONNECT_COUNTS_METRIC_ID: u32 = 4028;
const ROAMING_RECONNECT_DURATION_METRIC_ID: u32 = 4035;
const NON_ROAMING_RECONNECT_DURATION_METRIC_ID: u32 = 4036;
const POLICY_PROACTIVE_ROAMING_SCAN_COUNTS_METRIC_ID: u32 = 4026;

#[derive(Clone, Debug, PartialEq)]
// Connection score and the time at which it was calculated.
pub struct TimestampedConnectionScore {
    pub score: u8,
    pub time: fasync::MonotonicInstant,
}
impl TimestampedConnectionScore {
    pub fn new(score: u8, time: fasync::MonotonicInstant) -> Self {
        Self { score, time }
    }
}
impl Timestamped for TimestampedConnectionScore {
    fn time(&self) -> fasync::MonotonicInstant {
        self.time
    }
}

#[derive(Clone)]
#[cfg_attr(test, derive(Debug))]
pub struct TelemetrySender {
    sender: Arc<Mutex<mpsc::Sender<TelemetryEvent>>>,
    sender_is_blocked: Arc<AtomicBool>,
}

impl TelemetrySender {
    pub fn new(sender: mpsc::Sender<TelemetryEvent>) -> Self {
        Self {
            sender: Arc::new(Mutex::new(sender)),
            sender_is_blocked: Arc::new(AtomicBool::new(false)),
        }
    }

    // Send telemetry event. Log an error if it fails
    pub fn send(&self, event: TelemetryEvent) {
        match self.sender.lock().try_send(event) {
            Ok(_) => {
                // If sender has been blocked before, set bool to false and log message
                if self
                    .sender_is_blocked
                    .compare_exchange(true, false, Ordering::SeqCst, Ordering::SeqCst)
                    .is_ok()
                {
                    info!("TelemetrySender recovered and resumed sending");
                }
            }
            Err(_) => {
                // If sender has not been blocked before, set bool to true and log error message
                if self
                    .sender_is_blocked
                    .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                    .is_ok()
                {
                    warn!("TelemetrySender dropped a msg: either buffer is full or no receiver is waiting");
                }
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct DisconnectInfo {
    pub connected_duration: zx::MonotonicDuration,
    pub is_sme_reconnecting: bool,
    pub disconnect_source: fidl_sme::DisconnectSource,
    pub previous_connect_reason: client::types::ConnectReason,
    pub ap_state: client::types::ApState,
    pub signals: HistoricalList<client::types::TimestampedSignal>,
}

pub trait DisconnectSourceExt {
    fn inspect_string(&self) -> String;
    fn flattened_reason_code(&self) -> u32;
    fn cobalt_reason_code(&self) -> u16;
    fn locally_initiated(&self) -> bool;
}

impl DisconnectSourceExt for fidl_sme::DisconnectSource {
    fn inspect_string(&self) -> String {
        match self {
            fidl_sme::DisconnectSource::User(reason) => {
                format!("source: user, reason: {:?}", reason)
            }
            fidl_sme::DisconnectSource::Ap(cause) => format!(
                "source: ap, reason: {:?}, mlme_event_name: {:?}",
                cause.reason_code, cause.mlme_event_name
            ),
            fidl_sme::DisconnectSource::Mlme(cause) => format!(
                "source: mlme, reason: {:?}, mlme_event_name: {:?}",
                cause.reason_code, cause.mlme_event_name
            ),
        }
    }

    /// If disconnect comes from AP, then get the 802.11 reason code.
    /// If disconnect comes from MLME, return (1u32 << 17) + reason code.
    /// If disconnect comes from user, return (1u32 << 16) + user disconnect reason.
    /// This is mainly used for metric.
    fn flattened_reason_code(&self) -> u32 {
        match self {
            fidl_sme::DisconnectSource::Ap(cause) => cause.reason_code.into_primitive() as u32,
            fidl_sme::DisconnectSource::User(reason) => (1u32 << 16) + *reason as u32,
            fidl_sme::DisconnectSource::Mlme(cause) => {
                (1u32 << 17) + (cause.reason_code.into_primitive() as u32)
            }
        }
    }

    fn cobalt_reason_code(&self) -> u16 {
        match self {
            // Cobalt metrics expects reason_code value to be less than COBALT_REASON_CODE_MAX.
            fidl_sme::DisconnectSource::Ap(cause) => {
                std::cmp::min(cause.reason_code.into_primitive(), COBALT_REASON_CODE_MAX)
            }
            fidl_sme::DisconnectSource::User(reason) => {
                std::cmp::min(*reason as u16, COBALT_REASON_CODE_MAX)
            }
            fidl_sme::DisconnectSource::Mlme(cause) => {
                std::cmp::min(cause.reason_code.into_primitive(), COBALT_REASON_CODE_MAX)
            }
        }
    }

    fn locally_initiated(&self) -> bool {
        match self {
            fidl_sme::DisconnectSource::Ap(..) => false,
            fidl_sme::DisconnectSource::Mlme(..) | fidl_sme::DisconnectSource::User(..) => true,
        }
    }
}

#[derive(Debug, PartialEq)]
pub struct ScanEventInspectData {
    pub unknown_protection_ies: Vec<String>,
}

impl Default for ScanEventInspectData {
    fn default() -> Self {
        Self::new()
    }
}

impl ScanEventInspectData {
    pub fn new() -> Self {
        Self { unknown_protection_ies: vec![] }
    }
}

#[cfg_attr(test, derive(Debug))]
pub enum TelemetryEvent {
    /// Request telemetry for the latest status
    QueryStatus {
        sender: oneshot::Sender<QueryStatusResult>,
    },
    /// Notify the telemetry event loop that the process of establishing connection is started
    StartEstablishConnection {
        /// If set to true, use the current time as the start time of the establish connection
        /// process. If set to false, then use the start time initialized from the previous
        /// StartEstablishConnection event, or use the current time if there isn't an existing
        /// start time.
        reset_start_time: bool,
    },
    /// Clear any existing start time of establish connection process tracked by telemetry.
    ClearEstablishConnectionStartTime,
    /// Notify the telemetry event loop of an active scan being requested.
    ActiveScanRequested {
        num_ssids_requested: usize,
    },
    /// Notify the telemetry event loop of an active scan being requested via Policy API.
    ActiveScanRequestedViaApi {
        num_ssids_requested: usize,
    },
    /// Notify the telemetry event loop that network selection is complete.
    NetworkSelectionDecision {
        /// Type of network selection. If it's undirected and no candidate network is found,
        /// telemetry will toggle the "no saved neighbor" flag.
        network_selection_type: NetworkSelectionType,
        /// When there's a scan error, `num_candidates` should be Err.
        /// When `num_candidates` is `Ok(0)` for an undirected network selection, telemetry
        /// will toggle the "no saved neighbor" flag.  If the event loop is tracking downtime,
        /// the subsequent downtime period will also be used to increment the,
        /// `downtime_no_saved_neighbor_duration` counter. This counter is used to
        /// adjust the raw downtime.
        num_candidates: Result<usize, ()>,
        /// Count of number of networks selected. This will be 0 if there are no candidates selected
        /// including if num_candidates is Ok(0) or Err. However, this will only be logged to
        /// Cobalt is num_candidates is not Err and is greater than 0.
        selected_count: usize,
    },
    /// Notify the telemetry event loop of connection result.
    /// If connection result is successful, telemetry will move its internal state to
    /// connected. Subsequently, the telemetry event loop will increment the `connected_duration`
    /// counter periodically.
    ConnectResult {
        iface_id: u16,
        policy_connect_reason: Option<client::types::ConnectReason>,
        result: fidl_sme::ConnectResult,
        multiple_bss_candidates: bool,
        ap_state: client::types::ApState,
        network_is_likely_hidden: bool,
    },
    /// Notify the telemetry event loop of roam result.
    /// If roam result is unsuccessful, telemetry will move its internal state to
    /// disconnected.
    RoamResult {
        iface_id: u16,
        result: fidl_sme::RoamResult,
        ap_state: client::types::ApState,
    },
    /// Notify the telemetry event loop that the client has disconnected.
    /// Subsequently, the telemetry event loop will increment the downtime counters periodically
    /// if TelemetrySender has requested downtime to be tracked via `track_subsequent_downtime`
    /// flag.
    Disconnected {
        /// Indicates whether subsequent period should be used to increment the downtime counters.
        track_subsequent_downtime: bool,
        info: DisconnectInfo,
    },
    OnSignalReport {
        ind: fidl_internal::SignalReportIndication,
    },
    OnSignalVelocityUpdate {
        rssi_velocity: f64,
    },
    OnChannelSwitched {
        info: fidl_internal::ChannelSwitchInfo,
    },
    /// Notify telemetry that there was a decision to look for networks to roam to after evaluating
    /// the existing connection.
    RoamingScan,
    /// Proactive roams do not happen yet, but we want to analyze metrics for when they would
    /// happen. Roams are set up to log metrics when disconnects happen to roam, so this event
    /// covers when roams would happen but no actual disconnect happens.
    WouldRoamConnect,
    /// Counts of saved networks and count of configurations for each of those networks, to be
    /// recorded periodically.
    SavedNetworkCount {
        saved_network_count: usize,
        config_count_per_saved_network: Vec<usize>,
    },
    /// Record the time since the last network selection scan
    NetworkSelectionScanInterval {
        time_since_last_scan: zx::MonotonicDuration,
    },
    /// Statistics about networks observed in scan results for Connection Selection
    ConnectionSelectionScanResults {
        saved_network_count: usize,
        bss_count_per_saved_network: Vec<usize>,
        saved_network_count_found_by_active_scan: usize,
    },
    PostConnectionSignals {
        connect_time: fasync::MonotonicInstant,
        signal_at_connect: client::types::Signal,
        signals: HistoricalList<client::types::TimestampedSignal>,
    },
    /// Notify telemetry of an API request to start client connections.
    StartClientConnectionsRequest,
    /// Notify telemetry of an API request to stop client connections.
    StopClientConnectionsRequest,
    /// Notify telemetry of when AP is stopped, and how long it was started.
    StopAp {
        enabled_duration: zx::MonotonicDuration,
    },
    /// Notify telemetry of the result of a create iface request.
    IfaceCreationResult(Result<(), ()>),
    /// Notify telemetry of the result of destroying an interface.
    IfaceDestructionResult(Result<(), ()>),
    /// Notify telemetry of the result of a StartAp request.
    StartApResult(Result<(), ()>),
    /// Record scan fulfillment time
    ScanRequestFulfillmentTime {
        duration: zx::MonotonicDuration,
        reason: client::scan::ScanReason,
    },
    /// Record scan queue length upon scan completion
    ScanQueueStatistics {
        fulfilled_requests: usize,
        remaining_requests: usize,
    },
    /// Record the results of a completed BSS selection
    BssSelectionResult {
        reason: client::types::ConnectReason,
        scored_candidates: Vec<(client::types::ScannedCandidate, i16)>,
        selected_candidate: Option<(client::types::ScannedCandidate, i16)>,
    },
    ScanEvent {
        inspect_data: ScanEventInspectData,
        scan_defects: Vec<ScanIssue>,
    },
    LongDurationSignals {
        signals: Vec<client::types::TimestampedSignal>,
    },
    /// Record recovery events and store recovery-related metadata so that the
    /// efficacy of the recovery mechanism can be evaluated later.
    RecoveryEvent {
        reason: RecoveryReason,
    },
    /// Get the TimeSeries held by telemetry loop. Intended for test only.
    GetTimeSeries {
        sender: oneshot::Sender<Arc<Mutex<TimeSeriesStats>>>,
    },
    SmeTimeout {
        source: TimeoutSource,
    },
}

#[derive(Clone, Debug)]
pub struct QueryStatusResult {
    connection_state: ConnectionStateInfo,
}

// TODO(https://fxbug.dev/324167674): fix.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug)]
pub enum ConnectionStateInfo {
    Idle,
    Disconnected,
    Connected {
        iface_id: u16,
        ap_state: client::types::ApState,
        telemetry_proxy: Option<fidl_fuchsia_wlan_sme::TelemetryProxy>,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub enum NetworkSelectionType {
    /// Looking for the best BSS from any saved networks
    Undirected,
    /// Looking for the best BSS for a particular network
    Directed,
}

#[derive(Debug, PartialEq)]
pub enum ScanIssue {
    ScanFailure,
    AbortedScan,
    EmptyScanResults,
}

impl ScanIssue {
    fn as_metric_id(&self) -> u32 {
        match self {
            ScanIssue::ScanFailure => metrics::CLIENT_SCAN_FAILURE_METRIC_ID,
            ScanIssue::AbortedScan => metrics::ABORTED_SCAN_METRIC_ID,
            ScanIssue::EmptyScanResults => metrics::EMPTY_SCAN_RESULTS_METRIC_ID,
        }
    }
}

pub type ClientRecoveryMechanism = metrics::ConnectivityWlanMetricDimensionClientRecoveryMechanism;
pub type ApRecoveryMechanism = metrics::ConnectivityWlanMetricDimensionApRecoveryMechanism;
pub type TimeoutRecoveryMechanism =
    metrics::ConnectivityWlanMetricDimensionTimeoutRecoveryMechanism;

#[derive(Copy, Clone, Debug, PartialEq)]
pub enum PhyRecoveryMechanism {
    PhyReset = 0,
}

#[derive(Copy, Clone, Debug, PartialEq)]
pub enum RecoveryReason {
    CreateIfaceFailure(PhyRecoveryMechanism),
    DestroyIfaceFailure(PhyRecoveryMechanism),
    Timeout(TimeoutRecoveryMechanism),
    ConnectFailure(ClientRecoveryMechanism),
    StartApFailure(ApRecoveryMechanism),
    ScanFailure(ClientRecoveryMechanism),
    ScanCancellation(ClientRecoveryMechanism),
    ScanResultsEmpty(ClientRecoveryMechanism),
}

struct RecoveryRecord {
    scan_failure: Option<RecoveryReason>,
    scan_cancellation: Option<RecoveryReason>,
    scan_results_empty: Option<RecoveryReason>,
    connect_failure: Option<RecoveryReason>,
    start_ap_failure: Option<RecoveryReason>,
    create_iface_failure: Option<RecoveryReason>,
    destroy_iface_failure: Option<RecoveryReason>,
    timeout: Option<RecoveryReason>,
}

impl RecoveryRecord {
    fn new() -> Self {
        RecoveryRecord {
            scan_failure: None,
            scan_cancellation: None,
            scan_results_empty: None,
            connect_failure: None,
            start_ap_failure: None,
            create_iface_failure: None,
            destroy_iface_failure: None,
            timeout: None,
        }
    }

    fn record_recovery_attempt(&mut self, reason: RecoveryReason) {
        match reason {
            RecoveryReason::ScanFailure(_) => self.scan_failure = Some(reason),
            RecoveryReason::ScanCancellation(_) => self.scan_cancellation = Some(reason),
            RecoveryReason::ScanResultsEmpty(_) => self.scan_results_empty = Some(reason),
            RecoveryReason::ConnectFailure(_) => self.connect_failure = Some(reason),
            RecoveryReason::StartApFailure(_) => self.start_ap_failure = Some(reason),
            RecoveryReason::CreateIfaceFailure(_) => self.create_iface_failure = Some(reason),
            RecoveryReason::DestroyIfaceFailure(_) => self.destroy_iface_failure = Some(reason),
            RecoveryReason::Timeout(_) => self.timeout = Some(reason),
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq)]
pub enum TimeoutSource {
    Scan,
    Connect,
    Disconnect,
    ClientStatus,
    WmmStatus,
    ApStart,
    ApStop,
    ApStatus,
    GetCounterStats,
    GetHistogramStats,
}

pub type RecoveryOutcome = metrics::ConnectivityWlanMetricDimensionResult;

/// Capacity of "first come, first serve" slots available to clients of
/// the mpsc::Sender<TelemetryEvent>.
const TELEMETRY_EVENT_BUFFER_SIZE: usize = 100;
/// How often to request RSSI stats and dispatcher packet counts from MLME.
const TELEMETRY_QUERY_INTERVAL: zx::MonotonicDuration = zx::MonotonicDuration::from_seconds(15);

/// Create a struct for sending TelemetryEvent, and a future representing the telemetry loop.
///
/// Every 15 seconds, the telemetry loop will query for MLME/PHY stats and update various
/// time-interval stats. The telemetry loop also handles incoming TelemetryEvent to update
/// the appropriate stats.
pub fn serve_telemetry(
    monitor_svc_proxy: fidl_fuchsia_wlan_device_service::DeviceMonitorProxy,
    cobalt_1dot1_proxy: fidl_fuchsia_metrics::MetricEventLoggerProxy,
    inspect_node: InspectNode,
    external_inspect_node: InspectNode,
    persistence_req_sender: auto_persist::PersistenceReqSender,
    defect_sender: mpsc::Sender<Defect>,
) -> (TelemetrySender, impl Future<Output = ()>) {
    let (sender, mut receiver) = mpsc::channel::<TelemetryEvent>(TELEMETRY_EVENT_BUFFER_SIZE);
    let sender = TelemetrySender::new(sender);
    let cloned_sender = sender.clone();
    let fut = async move {
        let mut report_interval_stream = fasync::Interval::new(TELEMETRY_QUERY_INTERVAL);
        const ONE_MINUTE: zx::MonotonicDuration = zx::MonotonicDuration::from_minutes(1);
        const_assert_eq!(ONE_MINUTE.into_nanos() % TELEMETRY_QUERY_INTERVAL.into_nanos(), 0);
        const INTERVAL_TICKS_PER_MINUTE: u64 =
            (ONE_MINUTE.into_nanos() / TELEMETRY_QUERY_INTERVAL.into_nanos()) as u64;
        const INTERVAL_TICKS_PER_HR: u64 = INTERVAL_TICKS_PER_MINUTE * 60;
        const INTERVAL_TICKS_PER_DAY: u64 = INTERVAL_TICKS_PER_HR * 24;
        let mut interval_tick = 0u64;
        let mut telemetry = Telemetry::new(
            cloned_sender,
            monitor_svc_proxy,
            cobalt_1dot1_proxy,
            inspect_node,
            external_inspect_node,
            persistence_req_sender,
            defect_sender.clone(),
        );
        loop {
            select! {
                event = receiver.next() => {
                    if let Some(event) = event {
                        telemetry.handle_telemetry_event(event).await;
                    }
                }
                _ = report_interval_stream.next() => {
                    telemetry.handle_periodic_telemetry().await;

                    interval_tick += 1;
                    if interval_tick % INTERVAL_TICKS_PER_DAY == 0 {
                        telemetry.log_daily_cobalt_metrics().await;
                    }

                    if interval_tick % (5 * INTERVAL_TICKS_PER_MINUTE) == 0 {
                        telemetry.persist_client_stats_counters().await;
                    }

                    // This ensures that `signal_hr_passed` is always called after
                    // `handle_periodic_telemetry` at the hour mark. This helps with
                    // ease of testing. Additionally, logging to Cobalt before sliding
                    // the window ensures that Cobalt uses the last 24 hours of data
                    // rather than 23 hours.
                    if interval_tick % INTERVAL_TICKS_PER_HR == 0 {
                        telemetry.signal_hr_passed().await;
                    }
                }
            }
        }
    };
    (sender, fut)
}

// TODO(https://fxbug.dev/324167674): fix.
#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
enum ConnectionState {
    // Like disconnected, but no downtime is tracked.
    Idle(IdleState),
    Connected(ConnectedState),
    Disconnected(DisconnectedState),
}

#[derive(Debug)]
struct IdleState {
    connect_start_time: Option<fasync::MonotonicInstant>,
}

#[derive(Debug)]
struct ConnectedState {
    iface_id: u16,
    /// Time when the user manually initiates connecting to another network via the
    /// Policy ClientController::Connect FIDL call.
    new_connect_start_time: Option<fasync::MonotonicInstant>,
    prev_connection_counters: Option<fidl_fuchsia_wlan_stats::ConnectionCounters>,
    multiple_bss_candidates: bool,
    ap_state: client::types::ApState,
    network_is_likely_hidden: bool,

    last_signal_report: fasync::MonotonicInstant,
    num_consecutive_get_counter_stats_failures: InspectableU64,
    is_driver_unresponsive: InspectableBool,

    telemetry_proxy: Option<fidl_fuchsia_wlan_sme::TelemetryProxy>,
}

#[derive(Debug)]
pub struct DisconnectedState {
    disconnected_since: fasync::MonotonicInstant,
    disconnect_info: DisconnectInfo,
    connect_start_time: Option<fasync::MonotonicInstant>,
    /// The latest time when the device's no saved neighbor duration was accounted.
    /// If this has a value, then conceptually we say that "no saved neighbor" flag
    /// is set.
    latest_no_saved_neighbor_time: Option<fasync::MonotonicInstant>,
    accounted_no_saved_neighbor_duration: zx::MonotonicDuration,
}

fn inspect_create_counters(
    inspect_node: &InspectNode,
    child_name: &str,
    counters: Arc<Mutex<WindowedStats<StatCounters>>>,
) -> LazyNode {
    inspect_node.create_lazy_child(child_name, move || {
        let counters = Arc::clone(&counters);
        async move {
            let inspector = Inspector::default();
            {
                let counters_mutex_guard = counters.lock();
                let counters = counters_mutex_guard.windowed_stat(None);
                inspect_insert!(inspector.root(), {
                    total_duration: counters.total_duration.into_nanos(),
                    connected_duration: counters.connected_duration.into_nanos(),
                    downtime_duration: counters.downtime_duration.into_nanos(),
                    downtime_no_saved_neighbor_duration: counters.downtime_no_saved_neighbor_duration.into_nanos(),
                    connect_attempts_count: counters.connect_attempts_count,
                    connect_successful_count: counters.connect_successful_count,
                    disconnect_count: counters.disconnect_count,
                    roaming_disconnect_count: counters.roaming_disconnect_count,
                    non_roaming_non_user_disconnect_count: counters.non_roaming_disconnect_count,
                    tx_high_packet_drop_duration: counters.tx_high_packet_drop_duration.into_nanos(),
                    rx_high_packet_drop_duration: counters.rx_high_packet_drop_duration.into_nanos(),
                    tx_very_high_packet_drop_duration: counters.tx_very_high_packet_drop_duration.into_nanos(),
                    rx_very_high_packet_drop_duration: counters.rx_very_high_packet_drop_duration.into_nanos(),
                    no_rx_duration: counters.no_rx_duration.into_nanos(),
                });
            }
            Ok(inspector)
        }
        .boxed()
    })
}

fn inspect_record_connection_status(inspect_node: &InspectNode, telemetry_sender: TelemetrySender) {
    inspect_node.record_lazy_child("connection_status", move|| {
        let telemetry_sender = telemetry_sender.clone();
        async move {
            let inspector = Inspector::default();
            let (sender, receiver) = oneshot::channel();
            telemetry_sender.send(TelemetryEvent::QueryStatus { sender });
            let info = match receiver.await {
                Ok(result) => result.connection_state,
                Err(e) => {
                    warn!("Unable to query data for Inspect connection status node: {}", e);
                    return Ok(inspector)
                }
            };

            inspector.root().record_string("status_string", match &info {
                ConnectionStateInfo::Idle => "idle".to_string(),
                ConnectionStateInfo::Disconnected => "disconnected".to_string(),
                ConnectionStateInfo::Connected { .. } => "connected".to_string(),
            });
            if let ConnectionStateInfo::Connected { ap_state, .. } = info {
                inspect_insert!(inspector.root(), connected_network: {
                    rssi_dbm: ap_state.tracked.signal.rssi_dbm,
                    snr_db: ap_state.tracked.signal.snr_db,
                    bssid: ap_state.original().bssid.to_string(),
                    ssid: ap_state.original().ssid.to_string(),
                    protection: format!("{:?}", ap_state.original().protection()),
                    channel: format!("{}", ap_state.original().channel),
                    ht_cap?: ap_state.original().raw_ht_cap().map(|cap| InspectBytes(cap.bytes)),
                    vht_cap?: ap_state.original().raw_vht_cap().map(|cap| InspectBytes(cap.bytes)),
                    wsc?: ap_state.original().probe_resp_wsc().as_ref().map(|wsc| make_inspect_loggable!(
                            device_name: String::from_utf8_lossy(&wsc.device_name[..]).to_string(),
                            manufacturer: String::from_utf8_lossy(&wsc.manufacturer[..]).to_string(),
                            model_name: String::from_utf8_lossy(&wsc.model_name[..]).to_string(),
                            model_number: String::from_utf8_lossy(&wsc.model_number[..]).to_string(),
                        )),
                    is_wmm_assoc: ap_state.original().find_wmm_param().is_some(),
                    wmm_param?: ap_state.original().find_wmm_param().map(InspectBytes),
                });
            }
            Ok(inspector)
        }
        .boxed()
    });
}

fn inspect_record_external_data(
    external_inspect_node: &ExternalInspectNode,
    telemetry_sender: TelemetrySender,
    defect_sender: mpsc::Sender<Defect>,
) {
    external_inspect_node.node.record_lazy_child("connection_status", move || {
        let telemetry_sender = telemetry_sender.clone();
        let mut defect_sender = defect_sender.clone();
        async move {
            let inspector = Inspector::default();
            let (sender, receiver) = oneshot::channel();
            telemetry_sender.send(TelemetryEvent::QueryStatus { sender });
            let info = match receiver.await {
                Ok(result) => result.connection_state,
                Err(e) => {
                    warn!("Unable to query data for Inspect external node: {}", e);
                    return Ok(inspector);
                }
            };

            if let ConnectionStateInfo::Connected { ap_state, telemetry_proxy, iface_id } = info {
                inspect_insert!(inspector.root(), connected_network: {
                    rssi_dbm: ap_state.tracked.signal.rssi_dbm,
                    snr_db: ap_state.tracked.signal.snr_db,
                    wsc?: ap_state.original().probe_resp_wsc().as_ref().map(|wsc| make_inspect_loggable!(
                            device_name: String::from_utf8_lossy(&wsc.device_name[..]).to_string(),
                            manufacturer: String::from_utf8_lossy(&wsc.manufacturer[..]).to_string(),
                            model_name: String::from_utf8_lossy(&wsc.model_name[..]).to_string(),
                            model_number: String::from_utf8_lossy(&wsc.model_number[..]).to_string(),
                        )),
                });

                if let Some(proxy) = telemetry_proxy {
                    match proxy.get_histogram_stats()
                        .on_timeout(GET_IFACE_STATS_TIMEOUT, || {
                            warn!("Timed out waiting for histogram stats");

                            if let Err(e) = defect_sender
                                .try_send(Defect::Iface(IfaceFailure::Timeout {
                                    iface_id,
                                    source: TimeoutSource::GetHistogramStats,
                                })) {
                                    warn!("Failed to report histogram stats defect: {:?}", e)
                                }

                            Ok(Err(zx::Status::TIMED_OUT.into_raw()))
                        })
                        .await {
                            Ok(Ok(stats)) => {
                                let mut histograms = HistogramsNode::new(
                                    inspector.root().create_child("histograms"),
                                );
                                if let Some(snr_histograms) = &stats.snr_histograms {
                                    histograms.log_per_antenna_snr_histograms(&snr_histograms[..]);
                                }
                                if let Some(rx_rate_histograms) = &stats.rx_rate_index_histograms {
                                    histograms.log_per_antenna_rx_rate_histograms(
                                        &rx_rate_histograms[..],
                                    );
                                }
                                if let Some(noise_floor_histograms) = &stats.noise_floor_histograms {
                                    histograms.log_per_antenna_noise_floor_histograms(
                                        &noise_floor_histograms[..],
                                    );
                                }
                                if let Some(rssi_histograms) = &stats.rssi_histograms {
                                    histograms.log_per_antenna_rssi_histograms(
                                        &rssi_histograms[..],
                                    );
                                }

                                inspector.root().record(histograms);
                            }
                            error => {
                                info!("Error reading histogram stats: {:?}", error);
                            },
                        }
                }
            }
            Ok(inspector)
        }
        .boxed()
    });
}

#[derive(Debug)]
struct HistogramsNode {
    node: InspectNode,
    antenna_nodes: HashMap<fidl_fuchsia_wlan_stats::AntennaId, InspectNode>,
}

impl InspectType for HistogramsNode {}

macro_rules! fn_log_per_antenna_histograms {
    ($name:ident, $field:ident, $histogram_ty:ty, $sample:ident => $sample_index_expr:expr) => {
        paste::paste! {
            pub fn [<log_per_antenna_ $name _histograms>](
                &mut self,
                histograms: &[$histogram_ty],
            ) {
                for histogram in histograms {
                    // Only antenna histograms are logged (STATION scope histograms are discarded)
                    let antenna_id = match &histogram.antenna_id {
                        Some(id) => **id,
                        None => continue,
                    };
                    let antenna_node = self.create_or_get_antenna_node(antenna_id);

                    let samples = &histogram.$field;
                    let histogram_prop_name = concat!(stringify!($name), "_histogram");
                    let histogram_prop =
                        antenna_node.create_int_array(histogram_prop_name, samples.len() * 2);
                    for (i, sample) in samples.iter().enumerate() {
                        let $sample = sample;
                        histogram_prop.set(i * 2, $sample_index_expr);
                        histogram_prop.set(i * 2 + 1, $sample.num_samples as i64);
                    }

                    let invalid_samples_name = concat!(stringify!($name), "_invalid_samples");
                    let invalid_samples =
                        antenna_node.create_uint(invalid_samples_name, histogram.invalid_samples);

                    antenna_node.record(histogram_prop);
                    antenna_node.record(invalid_samples);
                }
            }
        }
    };
}

impl HistogramsNode {
    pub fn new(node: InspectNode) -> Self {
        Self { node, antenna_nodes: HashMap::new() }
    }

    // fn log_per_antenna_snr_histograms
    fn_log_per_antenna_histograms!(snr, snr_samples, fidl_fuchsia_wlan_stats::SnrHistogram,
                                   sample => sample.bucket_index as i64);
    // fn log_per_antenna_rx_rate_histograms
    fn_log_per_antenna_histograms!(rx_rate, rx_rate_index_samples,
                                   fidl_fuchsia_wlan_stats::RxRateIndexHistogram,
                                   sample => sample.bucket_index as i64);
    // fn log_per_antenna_noise_floor_histograms
    fn_log_per_antenna_histograms!(noise_floor, noise_floor_samples,
                                   fidl_fuchsia_wlan_stats::NoiseFloorHistogram,
                                   sample => sample.bucket_index as i64 - 255);
    // fn log_per_antenna_rssi_histograms
    fn_log_per_antenna_histograms!(rssi, rssi_samples, fidl_fuchsia_wlan_stats::RssiHistogram,
                                   sample => sample.bucket_index as i64 - 255);

    fn create_or_get_antenna_node(
        &mut self,
        antenna_id: fidl_fuchsia_wlan_stats::AntennaId,
    ) -> &mut InspectNode {
        let histograms_node = &self.node;
        self.antenna_nodes.entry(antenna_id).or_insert_with(|| {
            let freq = match antenna_id.freq {
                fidl_fuchsia_wlan_stats::AntennaFreq::Antenna2G => "2Ghz",
                fidl_fuchsia_wlan_stats::AntennaFreq::Antenna5G => "5Ghz",
            };
            let node =
                histograms_node.create_child(format!("antenna{}_{}", antenna_id.index, freq));
            node.record_uint("antenna_index", antenna_id.index as u64);
            node.record_string("antenna_freq", freq);
            node
        })
    }
}

// Macro wrapper for logging simple events (occurrence, integer, histogram, string)
// and log a warning when the status is not Ok
macro_rules! log_cobalt_1dot1 {
    ($cobalt_proxy:expr, $method_name:ident, $metric_id:expr, $value:expr, $event_codes:expr $(,)?) => {{
        let status = $cobalt_proxy.$method_name($metric_id, $value, $event_codes).await;
        match status {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(format_err!("Failed logging metric: {}, error: {:?}", $metric_id, e)),
            Err(e) => Err(format_err!("Failed logging metric: {}, error: {}", $metric_id, e)),
        }
    }};
}

macro_rules! log_cobalt_1dot1_batch {
    ($cobalt_proxy:expr, $events:expr, $context:expr $(,)?) => {{
        let status = $cobalt_proxy.log_metric_events($events).await;
        match status {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(format_err!(
                "Failed logging batch metrics, context: {}, error: {:?}",
                $context,
                e
            )),
            Err(e) => Err(format_err!(
                "Failed logging batch metrics, context: {}, error: {}",
                $context,
                e
            )),
        }
    }};
}

const INSPECT_SCAN_EVENTS_LIMIT: usize = 7;
const INSPECT_CONNECT_EVENTS_LIMIT: usize = 7;
const INSPECT_DISCONNECT_EVENTS_LIMIT: usize = 7;
const INSPECT_EXTERNAL_DISCONNECT_EVENTS_LIMIT: usize = 2;

/// Inspect node with properties queried by external entities.
/// Do not change or remove existing properties that are still used.
pub struct ExternalInspectNode {
    node: InspectNode,
    disconnect_events: Mutex<BoundedListNode>,
}

impl ExternalInspectNode {
    pub fn new(node: InspectNode) -> Self {
        let disconnect_events = node.create_child("disconnect_events");
        Self {
            node,
            disconnect_events: Mutex::new(BoundedListNode::new(
                disconnect_events,
                INSPECT_EXTERNAL_DISCONNECT_EVENTS_LIMIT,
            )),
        }
    }
}

/// Duration without signal before we determine driver as unresponsive
const UNRESPONSIVE_FLAG_MIN_DURATION: zx::MonotonicDuration =
    zx::MonotonicDuration::from_seconds(60);

pub struct Telemetry {
    monitor_svc_proxy: fidl_fuchsia_wlan_device_service::DeviceMonitorProxy,
    connection_state: ConnectionState,
    last_checked_connection_state: fasync::MonotonicInstant,
    stats_logger: StatsLogger,

    // Inspect properties/nodes that telemetry hangs onto
    inspect_node: InspectNode,
    get_iface_stats_fail_count: UintProperty,
    scan_events_node: Mutex<AutoPersist<BoundedListNode>>,
    connect_events_node: Mutex<AutoPersist<BoundedListNode>>,
    disconnect_events_node: Mutex<AutoPersist<BoundedListNode>>,
    external_inspect_node: ExternalInspectNode,

    // Auto-persistence on various client stats counters
    auto_persist_client_stats_counters: AutoPersist<()>,

    // For keeping track of how long client connections were enabled when turning client
    // connections on and off.
    last_enabled_client_connections: Option<fasync::MonotonicInstant>,

    // For keeping track of how long client connections were disabled when turning client
    // connections off and on again. None if a command to turn off client connections has never
    // been sent or if client connections are on.
    last_disabled_client_connections: Option<fasync::MonotonicInstant>,
    defect_sender: mpsc::Sender<Defect>,
}

impl Telemetry {
    pub fn new(
        telemetry_sender: TelemetrySender,
        monitor_svc_proxy: fidl_fuchsia_wlan_device_service::DeviceMonitorProxy,
        cobalt_1dot1_proxy: fidl_fuchsia_metrics::MetricEventLoggerProxy,
        inspect_node: InspectNode,
        external_inspect_node: InspectNode,
        persistence_req_sender: auto_persist::PersistenceReqSender,
        defect_sender: mpsc::Sender<Defect>,
    ) -> Self {
        let stats_logger = StatsLogger::new(cobalt_1dot1_proxy, &inspect_node);
        inspect_record_connection_status(&inspect_node, telemetry_sender.clone());
        let get_iface_stats_fail_count = inspect_node.create_uint("get_iface_stats_fail_count", 0);
        let scan_events = inspect_node.create_child("scan_events");
        let connect_events = inspect_node.create_child("connect_events");
        let disconnect_events = inspect_node.create_child("disconnect_events");
        let external_inspect_node = ExternalInspectNode::new(external_inspect_node);
        inspect_record_external_data(
            &external_inspect_node,
            telemetry_sender,
            defect_sender.clone(),
        );
        Self {
            monitor_svc_proxy,
            connection_state: ConnectionState::Idle(IdleState { connect_start_time: None }),
            last_checked_connection_state: fasync::MonotonicInstant::now(),
            stats_logger,
            inspect_node,
            get_iface_stats_fail_count,
            scan_events_node: Mutex::new(AutoPersist::new(
                BoundedListNode::new(scan_events, INSPECT_SCAN_EVENTS_LIMIT),
                "wlancfg-scan-events",
                persistence_req_sender.clone(),
            )),
            connect_events_node: Mutex::new(AutoPersist::new(
                BoundedListNode::new(connect_events, INSPECT_CONNECT_EVENTS_LIMIT),
                "wlancfg-connect-events",
                persistence_req_sender.clone(),
            )),
            disconnect_events_node: Mutex::new(AutoPersist::new(
                BoundedListNode::new(disconnect_events, INSPECT_DISCONNECT_EVENTS_LIMIT),
                "wlancfg-disconnect-events",
                persistence_req_sender.clone(),
            )),
            external_inspect_node,
            auto_persist_client_stats_counters: AutoPersist::new(
                (),
                "wlancfg-client-stats-counters",
                persistence_req_sender,
            ),
            last_enabled_client_connections: None,
            last_disabled_client_connections: None,
            defect_sender,
        }
    }

    pub async fn handle_periodic_telemetry(&mut self) {
        let now = fasync::MonotonicInstant::now();
        let duration = now - self.last_checked_connection_state;

        self.stats_logger.log_stat(StatOp::AddTotalDuration(duration)).await;
        self.stats_logger.log_queued_stats().await;

        match &mut self.connection_state {
            ConnectionState::Idle(..) => (),
            ConnectionState::Connected(state) => {
                self.stats_logger.log_stat(StatOp::AddConnectedDuration(duration)).await;
                if let Some(proxy) = &state.telemetry_proxy {
                    match proxy
                        .get_counter_stats()
                        .on_timeout(GET_IFACE_STATS_TIMEOUT, || {
                            warn!("Timed out waiting for counter stats");

                            if let Err(e) =
                                self.defect_sender.try_send(Defect::Iface(IfaceFailure::Timeout {
                                    iface_id: state.iface_id,
                                    source: TimeoutSource::GetCounterStats,
                                }))
                            {
                                warn!("Failed to report counter stats timeout: {:?}", e)
                            }

                            Ok(Err(zx::Status::TIMED_OUT.into_raw()))
                        })
                        .await
                    {
                        Ok(Ok(stats)) => {
                            *state.num_consecutive_get_counter_stats_failures.get_mut() = 0;
                            if let (
                                Some(prev_connection_counters),
                                Some(current_connection_counters),
                            ) = (
                                state.prev_connection_counters.as_ref(),
                                stats.connection_counters.as_ref(),
                            ) {
                                diff_and_log_connection_counters(
                                    &mut self.stats_logger,
                                    prev_connection_counters,
                                    current_connection_counters,
                                    duration,
                                )
                                .await;
                            }
                            state.prev_connection_counters = stats.connection_counters;
                        }
                        error => {
                            info!("Failed to get interface stats: {:?}", error);
                            let _ = self.get_iface_stats_fail_count.add(1);
                            *state.num_consecutive_get_counter_stats_failures.get_mut() += 1;
                            // Safe to unwrap: If we've exceeded 63 bits of consecutive failures,
                            // we have other things to worry about.
                            #[expect(clippy::unwrap_used)]
                            self.stats_logger
                                .log_consecutive_counter_stats_failures(
                                    (*state.num_consecutive_get_counter_stats_failures)
                                        .try_into()
                                        .unwrap(),
                                )
                                .await;
                            let _ = state.prev_connection_counters.take();
                        }
                    }
                }

                let unresponsive_signal_ind =
                    now - state.last_signal_report > UNRESPONSIVE_FLAG_MIN_DURATION;
                let mut is_driver_unresponsive = state.is_driver_unresponsive.get_mut();
                if unresponsive_signal_ind != *is_driver_unresponsive {
                    *is_driver_unresponsive = unresponsive_signal_ind;
                    if unresponsive_signal_ind {
                        warn!("driver unresponsive due to missing signal report");
                    }
                }
            }
            ConnectionState::Disconnected(state) => {
                self.stats_logger.log_stat(StatOp::AddDowntimeDuration(duration)).await;
                if let Some(prev) = state.latest_no_saved_neighbor_time.take() {
                    let duration = now - prev;
                    state.accounted_no_saved_neighbor_duration += duration;
                    self.stats_logger
                        .log_stat(StatOp::AddDowntimeNoSavedNeighborDuration(duration))
                        .await;
                    state.latest_no_saved_neighbor_time = Some(now);
                }
            }
        }
        self.last_checked_connection_state = now;
    }

    pub async fn handle_telemetry_event(&mut self, event: TelemetryEvent) {
        let now = fasync::MonotonicInstant::now();
        match event {
            TelemetryEvent::QueryStatus { sender } => {
                let info = match &self.connection_state {
                    ConnectionState::Idle(..) => ConnectionStateInfo::Idle,
                    ConnectionState::Disconnected(..) => ConnectionStateInfo::Disconnected,
                    ConnectionState::Connected(state) => ConnectionStateInfo::Connected {
                        iface_id: state.iface_id,
                        ap_state: state.ap_state.clone(),
                        telemetry_proxy: state.telemetry_proxy.clone(),
                    },
                };
                let _result = sender.send(QueryStatusResult { connection_state: info });
            }
            TelemetryEvent::StartEstablishConnection { reset_start_time } => match &mut self
                .connection_state
            {
                ConnectionState::Idle(IdleState { connect_start_time })
                | ConnectionState::Disconnected(DisconnectedState { connect_start_time, .. }) => {
                    if reset_start_time || connect_start_time.is_none() {
                        let _prev = connect_start_time.replace(now);
                    }
                }
                ConnectionState::Connected(state) => {
                    // When in connected state, only set the start time if `reset_start_time` is
                    // true because it indicates the user triggers the new connect action.
                    if reset_start_time {
                        let _prev = state.new_connect_start_time.replace(now);
                    }
                }
            },
            TelemetryEvent::ClearEstablishConnectionStartTime => match &mut self.connection_state {
                ConnectionState::Idle(state) => {
                    let _start_time = state.connect_start_time.take();
                }
                ConnectionState::Disconnected(state) => {
                    let _start_time = state.connect_start_time.take();
                }
                ConnectionState::Connected(state) => {
                    let _start_time = state.new_connect_start_time.take();
                }
            },
            TelemetryEvent::ActiveScanRequested { num_ssids_requested } => {
                self.stats_logger
                    .log_active_scan_requested_cobalt_metrics(num_ssids_requested)
                    .await
            }
            TelemetryEvent::ActiveScanRequestedViaApi { num_ssids_requested } => {
                self.stats_logger
                    .log_active_scan_requested_via_api_cobalt_metrics(num_ssids_requested)
                    .await
            }
            TelemetryEvent::NetworkSelectionDecision {
                network_selection_type,
                num_candidates,
                selected_count,
            } => {
                self.stats_logger
                    .log_network_selection_metrics(
                        &mut self.connection_state,
                        network_selection_type,
                        num_candidates,
                        selected_count,
                    )
                    .await;
            }
            TelemetryEvent::ConnectResult {
                iface_id,
                policy_connect_reason,
                result,
                multiple_bss_candidates,
                ap_state,
                network_is_likely_hidden,
            } => {
                let connect_start_time = match &self.connection_state {
                    ConnectionState::Idle(state) => state.connect_start_time,
                    ConnectionState::Disconnected(state) => state.connect_start_time,
                    ConnectionState::Connected(..) => {
                        warn!("Received ConnectResult event while still connected");
                        None
                    }
                };
                self.stats_logger
                    .report_connect_result(
                        policy_connect_reason,
                        result.code,
                        multiple_bss_candidates,
                        &ap_state,
                        connect_start_time,
                    )
                    .await;
                self.stats_logger.log_stat(StatOp::AddConnectAttemptsCount).await;
                if result.code == fidl_ieee80211::StatusCode::Success {
                    self.log_connect_event_inspect(&ap_state, multiple_bss_candidates);
                    self.stats_logger.log_stat(StatOp::AddConnectSuccessfulCount).await;

                    self.stats_logger
                        .log_device_connected_cobalt_metrics(
                            multiple_bss_candidates,
                            &ap_state,
                            network_is_likely_hidden,
                        )
                        .await;
                    if let ConnectionState::Disconnected(state) = &self.connection_state {
                        if state.latest_no_saved_neighbor_time.is_some() {
                            warn!("'No saved neighbor' flag still set even though connected");
                        }
                        self.stats_logger.queue_stat_op(StatOp::AddDowntimeDuration(
                            now - self.last_checked_connection_state,
                        ));
                        let total_downtime = now - state.disconnected_since;
                        if total_downtime < state.accounted_no_saved_neighbor_duration {
                            warn!(
                                "Total downtime is less than no-saved-neighbor duration. \
                                 Total downtime: {:?}, No saved neighbor duration: {:?}",
                                total_downtime, state.accounted_no_saved_neighbor_duration
                            )
                        }
                        let adjusted_downtime = max(
                            total_downtime - state.accounted_no_saved_neighbor_duration,
                            zx::MonotonicDuration::from_seconds(0),
                        );
                        self.stats_logger
                            .log_downtime_cobalt_metrics(adjusted_downtime, &state.disconnect_info)
                            .await;
                        let disconnect_source = state.disconnect_info.disconnect_source;
                        self.stats_logger
                            .log_reconnect_cobalt_metrics(total_downtime, disconnect_source)
                            .await;
                    }

                    // Log successful post-recovery connection attempt if relevant.
                    if let Some(recovery_reason) =
                        self.stats_logger.recovery_record.connect_failure.take()
                    {
                        self.stats_logger
                            .log_post_recovery_result(recovery_reason, RecoveryOutcome::Success)
                            .await
                    }

                    let (proxy, server) = fidl::endpoints::create_proxy();
                    let telemetry_proxy = match self
                        .monitor_svc_proxy
                        .get_sme_telemetry(iface_id, server)
                        .await
                    {
                        Ok(Ok(())) => Some(proxy),
                        Ok(Err(e)) => {
                            error!("Request for SME telemetry for iface {} completed with error {}. No telemetry will be captured.", iface_id, e);
                            None
                        }
                        Err(e) => {
                            error!("Failed to request SME telemetry for iface {} with error {}. No telemetry will be captured.", iface_id, e);
                            None
                        }
                    };
                    self.connection_state = ConnectionState::Connected(ConnectedState {
                        iface_id,
                        new_connect_start_time: None,
                        prev_connection_counters: None,
                        multiple_bss_candidates,
                        ap_state,
                        network_is_likely_hidden,

                        // We have not received a signal report yet, but since this is used as
                        // indicator for whether driver is still responsive, set it to the
                        // connection start time for now.
                        last_signal_report: now,
                        num_consecutive_get_counter_stats_failures: InspectableU64::new(
                            0,
                            &self.inspect_node,
                            "num_consecutive_get_counter_stats_failures",
                        ),
                        is_driver_unresponsive: InspectableBool::new(
                            false,
                            &self.inspect_node,
                            "is_driver_unresponsive",
                        ),

                        telemetry_proxy,
                    });
                    self.last_checked_connection_state = now;
                } else if !result.is_credential_rejected {
                    // In the case where the connection failed for a reason other than a credential
                    // mismatch, log a connection failure occurrence metric.
                    self.stats_logger.log_connection_failure().await;

                    // Log failed post-recovery connection attempt if relevant.
                    if let Some(recovery_reason) =
                        self.stats_logger.recovery_record.connect_failure.take()
                    {
                        self.stats_logger
                            .log_post_recovery_result(recovery_reason, RecoveryOutcome::Failure)
                            .await
                    }
                }

                // Any completed SME operation tells us the SME is operational.
                self.report_sme_timeout_resolved().await;
            }
            TelemetryEvent::RoamResult { iface_id, result, ap_state } => {
                match &self.connection_state {
                    ConnectionState::Connected(state) => {
                        if result.status_code == fidl_ieee80211::StatusCode::Success {
                            self.connection_state = ConnectionState::Connected(ConnectedState {
                                iface_id,
                                new_connect_start_time: None,
                                prev_connection_counters: None,
                                multiple_bss_candidates: state.multiple_bss_candidates,
                                ap_state,
                                network_is_likely_hidden: state.network_is_likely_hidden,

                                // We have not received a signal report yet, but since this is used as
                                // indicator for whether driver is still responsive, set it to the
                                // connection start time for now.
                                last_signal_report: now,
                                num_consecutive_get_counter_stats_failures: InspectableU64::new(
                                    0,
                                    &self.inspect_node,
                                    "num_consecutive_get_counter_stats_failures",
                                ),
                                is_driver_unresponsive: InspectableBool::new(
                                    false,
                                    &self.inspect_node,
                                    "is_driver_unresponsive",
                                ),

                                telemetry_proxy: state.telemetry_proxy.clone(),
                            });
                            self.last_checked_connection_state = now;
                            // TODO(https://fxbug.dev/135975) Log roam success to Cobalt and Inspect.
                        } else {
                            // TODO(https://fxbug.dev/135975) Log roam failure to Cobalt and Inspect.
                        }
                    }
                    _ => {
                        warn!("Received roam event while not connected. Metric may not be logged");
                    }
                }
            }
            TelemetryEvent::Disconnected { track_subsequent_downtime, info } => {
                // Any completed SME operation tells us the SME is operational.
                self.report_sme_timeout_resolved().await;

                self.log_disconnect_event_inspect(&info);
                self.stats_logger
                    .log_stat(StatOp::AddDisconnectCount(info.disconnect_source))
                    .await;
                self.stats_logger
                    .log_pre_disconnect_score_deltas_by_signal(
                        info.connected_duration,
                        info.signals.clone(),
                    )
                    .await;
                self.stats_logger
                    .log_pre_disconnect_rssi_deltas(info.connected_duration, info.signals.clone())
                    .await;
                let duration = now - self.last_checked_connection_state;
                match &self.connection_state {
                    ConnectionState::Connected(state) => {
                        self.stats_logger
                            .log_disconnect_cobalt_metrics(&info, state.multiple_bss_candidates)
                            .await;
                        self.stats_logger.queue_stat_op(StatOp::AddConnectedDuration(duration));
                        // Log device connected to AP metrics right now in case we have not logged it
                        // to Cobalt yet today.
                        self.stats_logger
                            .log_device_connected_cobalt_metrics(
                                state.multiple_bss_candidates,
                                &state.ap_state,
                                state.network_is_likely_hidden,
                            )
                            .await;
                        // Log metrics if connection had a short duration.
                        if info.connected_duration < METRICS_SHORT_CONNECT_DURATION {
                            self.stats_logger
                                .log_short_duration_connection_metrics(
                                    info.signals.clone(),
                                    info.disconnect_source,
                                    info.previous_connect_reason,
                                )
                                .await;
                        }
                    }
                    _ => {
                        warn!("Received disconnect event while not connected. Metric may not be logged");
                    }
                }

                let connect_start_time = if info.is_sme_reconnecting {
                    // If `is_sme_reconnecting` is true, we already know that the process of
                    // establishing connection is already started at the moment of disconnect,
                    // so set the connect_start_time to now.
                    Some(now)
                } else {
                    match &self.connection_state {
                        ConnectionState::Connected(state) => state.new_connect_start_time,
                        _ => None,
                    }
                };
                self.connection_state = if track_subsequent_downtime {
                    ConnectionState::Disconnected(DisconnectedState {
                        disconnected_since: now,
                        disconnect_info: info,
                        connect_start_time,
                        // We assume that there's a saved neighbor in vicinity until proven
                        // otherwise from scan result.
                        latest_no_saved_neighbor_time: None,
                        accounted_no_saved_neighbor_duration: zx::MonotonicDuration::from_seconds(
                            0,
                        ),
                    })
                } else {
                    ConnectionState::Idle(IdleState { connect_start_time })
                };
                self.last_checked_connection_state = now;
            }
            TelemetryEvent::OnSignalReport { ind } => {
                if let ConnectionState::Connected(state) = &mut self.connection_state {
                    state.ap_state.tracked.signal.rssi_dbm = ind.rssi_dbm;
                    state.ap_state.tracked.signal.snr_db = ind.snr_db;
                    state.last_signal_report = now;
                    self.stats_logger.log_signal_report_metrics(ind.rssi_dbm).await;
                }
            }
            TelemetryEvent::OnSignalVelocityUpdate { rssi_velocity } => {
                self.stats_logger.log_signal_velocity_metrics(rssi_velocity).await;
            }
            TelemetryEvent::OnChannelSwitched { info } => {
                if let ConnectionState::Connected(state) = &mut self.connection_state {
                    state.ap_state.tracked.channel.primary = info.new_channel;
                    self.stats_logger
                        .log_device_connected_channel_cobalt_metrics(info.new_channel)
                        .await;
                }
            }
            TelemetryEvent::RoamingScan => {
                self.stats_logger.log_roaming_scan_metrics().await;
            }
            TelemetryEvent::WouldRoamConnect => {
                self.stats_logger.log_would_roam_connect().await;
            }
            TelemetryEvent::SavedNetworkCount {
                saved_network_count,
                config_count_per_saved_network,
            } => {
                self.stats_logger
                    .log_saved_network_counts(saved_network_count, config_count_per_saved_network)
                    .await;
            }
            TelemetryEvent::NetworkSelectionScanInterval { time_since_last_scan } => {
                self.stats_logger.log_network_selection_scan_interval(time_since_last_scan).await;
            }
            TelemetryEvent::ConnectionSelectionScanResults {
                saved_network_count,
                bss_count_per_saved_network,
                saved_network_count_found_by_active_scan,
            } => {
                self.stats_logger
                    .log_connection_selection_scan_results(
                        saved_network_count,
                        bss_count_per_saved_network,
                        saved_network_count_found_by_active_scan,
                    )
                    .await;
            }
            TelemetryEvent::StartClientConnectionsRequest => {
                let now = fasync::MonotonicInstant::now();
                if self.last_enabled_client_connections.is_none() {
                    self.last_enabled_client_connections = Some(now);
                }
                if let Some(disabled_time) = self.last_disabled_client_connections {
                    let disabled_duration = now - disabled_time;
                    self.stats_logger.log_start_client_connections_request(disabled_duration).await
                }
                self.last_disabled_client_connections = None;
            }
            TelemetryEvent::StopClientConnectionsRequest => {
                let now = fasync::MonotonicInstant::now();
                // Do not change the time if the request to turn off connections comes in when
                // client connections are already stopped.
                if self.last_disabled_client_connections.is_none() {
                    self.last_disabled_client_connections = Some(fasync::MonotonicInstant::now());
                }
                if let Some(enabled_time) = self.last_enabled_client_connections {
                    let enabled_duration = now - enabled_time;
                    self.stats_logger.log_stop_client_connections_request(enabled_duration).await
                }
                self.last_enabled_client_connections = None;
            }
            TelemetryEvent::StopAp { enabled_duration } => {
                self.stats_logger.log_stop_ap_cobalt_metrics(enabled_duration).await;

                // Any completed SME operation tells us the SME is operational.
                self.report_sme_timeout_resolved().await;
            }
            TelemetryEvent::IfaceCreationResult(result) => {
                self.stats_logger.log_iface_creation_result(result).await;
            }
            TelemetryEvent::IfaceDestructionResult(result) => {
                self.stats_logger.log_iface_destruction_result(result).await;
            }
            TelemetryEvent::StartApResult(result) => {
                self.stats_logger.log_ap_start_result(result).await;

                // Any completed SME operation tells us the SME is operational.
                self.report_sme_timeout_resolved().await;
            }
            TelemetryEvent::ScanRequestFulfillmentTime { duration, reason } => {
                self.stats_logger.log_scan_request_fulfillment_time(duration, reason).await;
            }
            TelemetryEvent::ScanQueueStatistics { fulfilled_requests, remaining_requests } => {
                self.stats_logger
                    .log_scan_queue_statistics(fulfilled_requests, remaining_requests)
                    .await;
            }
            TelemetryEvent::BssSelectionResult {
                reason,
                scored_candidates,
                selected_candidate,
            } => {
                self.stats_logger
                    .log_bss_selection_metrics(reason, scored_candidates, selected_candidate)
                    .await
            }
            TelemetryEvent::PostConnectionSignals { connect_time, signal_at_connect, signals } => {
                self.stats_logger
                    .log_post_connection_score_deltas_by_signal(
                        connect_time,
                        signal_at_connect,
                        signals.clone(),
                    )
                    .await;
                self.stats_logger
                    .log_post_connection_rssi_deltas(connect_time, signal_at_connect, signals)
                    .await;
            }
            TelemetryEvent::ScanEvent { inspect_data, scan_defects } => {
                self.log_scan_event_inspect(inspect_data);
                self.stats_logger.log_scan_issues(scan_defects).await;

                // Any completed SME operation tells us the SME is operational.
                self.report_sme_timeout_resolved().await;
            }
            TelemetryEvent::LongDurationSignals { signals } => {
                self.stats_logger
                    .log_connection_score_average_by_signal(
                        metrics::ConnectionScoreAverageMetricDimensionDuration::LongDuration as u32,
                        signals.clone(),
                    )
                    .await;
                self.stats_logger
                    .log_connection_rssi_average(
                        metrics::ConnectionRssiAverageMetricDimensionDuration::LongDuration as u32,
                        signals,
                    )
                    .await;
            }
            TelemetryEvent::RecoveryEvent { reason } => {
                self.stats_logger.log_recovery_occurrence(reason).await;
            }
            TelemetryEvent::GetTimeSeries { sender } => {
                let _result = sender.send(Arc::clone(&self.stats_logger.time_series_stats));
            }
            TelemetryEvent::SmeTimeout { source } => {
                self.stats_logger.log_sme_timeout(source).await;

                // If timeouts have been a consistent issue to the point that recovery has been
                // requested and operations are still timing out, record a recovery failure.
                if let Some(recovery_reason) = self.stats_logger.recovery_record.timeout.take() {
                    self.stats_logger
                        .log_post_recovery_result(recovery_reason, RecoveryOutcome::Failure)
                        .await
                }
            }
        }
    }

    pub fn log_scan_event_inspect(&self, scan_event_info: ScanEventInspectData) {
        if !scan_event_info.unknown_protection_ies.is_empty() {
            inspect_log!(self.scan_events_node.lock().get_mut(), {
                unknown_protection_ies: InspectList(&scan_event_info.unknown_protection_ies)
            });
        }
    }

    pub fn log_connect_event_inspect(
        &self,
        ap_state: &client::types::ApState,
        multiple_bss_candidates: bool,
    ) {
        inspect_log!(self.connect_events_node.lock().get_mut(), {
            multiple_bss_candidates: multiple_bss_candidates,
            network: {
                bssid: ap_state.original().bssid.to_string(),
                ssid: ap_state.original().ssid.to_string(),
                rssi_dbm: ap_state.tracked.signal.rssi_dbm,
                snr_db: ap_state.tracked.signal.snr_db,
            },
        });
    }

    pub fn log_disconnect_event_inspect(&self, info: &DisconnectInfo) {
        inspect_log!(self.disconnect_events_node.lock().get_mut(), {
            connected_duration: info.connected_duration.into_nanos(),
            disconnect_source: info.disconnect_source.inspect_string(),
            network: {
                rssi_dbm: info.ap_state.tracked.signal.rssi_dbm,
                snr_db: info.ap_state.tracked.signal.snr_db,
                bssid: info.ap_state.original().bssid.to_string(),
                ssid: info.ap_state.original().ssid.to_string(),
                protection: format!("{:?}", info.ap_state.original().protection()),
                channel: format!("{}", info.ap_state.tracked.channel),
                ht_cap?: info.ap_state.original().raw_ht_cap().map(|cap| InspectBytes(cap.bytes)),
                vht_cap?: info.ap_state.original().raw_vht_cap().map(|cap| InspectBytes(cap.bytes)),
                wsc?: info.ap_state.original().probe_resp_wsc().as_ref().map(|wsc| make_inspect_loggable!(
                        device_name: String::from_utf8_lossy(&wsc.device_name[..]).to_string(),
                        manufacturer: String::from_utf8_lossy(&wsc.manufacturer[..]).to_string(),
                        model_name: String::from_utf8_lossy(&wsc.model_name[..]).to_string(),
                        model_number: String::from_utf8_lossy(&wsc.model_number[..]).to_string(),
                    )),
                is_wmm_assoc: info.ap_state.original().find_wmm_param().is_some(),
                wmm_param?: info.ap_state.original().find_wmm_param().map(InspectBytes),
            }
        });
        inspect_log!(self.external_inspect_node.disconnect_events.lock(), {
            // Flatten the reason code for external consumer as their reason code metric
            // cannot easily be adjusted to accept an additional dimension.
            flattened_reason_code: info.disconnect_source.flattened_reason_code(),
            locally_initiated: info.disconnect_source.locally_initiated(),
            network: {
                channel: {
                    primary: info.ap_state.tracked.channel.primary,
                },
            },
        });
    }

    pub async fn log_daily_cobalt_metrics(&mut self) {
        self.stats_logger.log_daily_cobalt_metrics().await;
        if let ConnectionState::Connected(state) = &self.connection_state {
            self.stats_logger
                .log_device_connected_cobalt_metrics(
                    state.multiple_bss_candidates,
                    &state.ap_state,
                    state.network_is_likely_hidden,
                )
                .await;
        }
    }

    pub async fn signal_hr_passed(&mut self) {
        self.stats_logger.handle_hr_passed().await;
    }

    pub async fn persist_client_stats_counters(&mut self) {
        let _auto_persist_guard = self.auto_persist_client_stats_counters.get_mut();
    }

    // Any return from an SME request is considered a successful outcome of a recovery intervention.
    pub async fn report_sme_timeout_resolved(&mut self) {
        if let Some(recovery_reason) = self.stats_logger.recovery_record.timeout.take() {
            self.stats_logger
                .log_post_recovery_result(recovery_reason, RecoveryOutcome::Success)
                .await
        }
    }
}

// Convert float to an integer in "ten thousandth" unit
// Example: 0.02f64 (i.e. 2%) -> 200 per ten thousand
fn float_to_ten_thousandth(value: f64) -> i64 {
    (value * 10000f64) as i64
}

fn round_to_nearest_second(duration: zx::MonotonicDuration) -> i64 {
    const MILLIS_PER_SEC: i64 = 1000;
    let millis = duration.into_millis();
    let rounded_portion = if millis % MILLIS_PER_SEC >= 500 { 1 } else { 0 };
    millis / MILLIS_PER_SEC + rounded_portion
}

pub async fn connect_to_metrics_logger_factory(
) -> Result<fidl_fuchsia_metrics::MetricEventLoggerFactoryProxy, Error> {
    let cobalt_1dot1_svc = fuchsia_component::client::connect_to_protocol::<
        fidl_fuchsia_metrics::MetricEventLoggerFactoryMarker,
    >()
    .context("failed to connect to metrics service")?;
    Ok(cobalt_1dot1_svc)
}

// Communicates with the MetricEventLoggerFactory service to create a MetricEventLoggerProxy for
// the caller.
pub async fn create_metrics_logger(
    factory_proxy: &fidl_fuchsia_metrics::MetricEventLoggerFactoryProxy,
) -> Result<fidl_fuchsia_metrics::MetricEventLoggerProxy, Error> {
    let (cobalt_1dot1_proxy, cobalt_1dot1_server) =
        fidl::endpoints::create_proxy::<fidl_fuchsia_metrics::MetricEventLoggerMarker>();

    let project_spec = fidl_fuchsia_metrics::ProjectSpec {
        customer_id: None, // defaults to fuchsia
        project_id: Some(metrics::PROJECT_ID),
        ..Default::default()
    };

    let status = factory_proxy
        .create_metric_event_logger(&project_spec, cobalt_1dot1_server)
        .await
        .context("failed to create metrics event logger")?;

    match status {
        Ok(_) => Ok(cobalt_1dot1_proxy),
        Err(err) => Err(format_err!("failed to create metrics event logger: {:?}", err)),
    }
}

const HIGH_PACKET_DROP_RATE_THRESHOLD: f64 = 0.02;
const VERY_HIGH_PACKET_DROP_RATE_THRESHOLD: f64 = 0.05;

const DEVICE_LOW_CONNECTION_SUCCESS_RATE_THRESHOLD: f64 = 0.1;

async fn diff_and_log_connection_counters(
    stats_logger: &mut StatsLogger,
    prev: &fidl_fuchsia_wlan_stats::ConnectionCounters,
    current: &fidl_fuchsia_wlan_stats::ConnectionCounters,
    duration: zx::MonotonicDuration,
) {
    diff_and_log_rx_counters(stats_logger, prev, current, duration).await;
    diff_and_log_tx_counters(stats_logger, prev, current, duration).await;
}

async fn diff_and_log_rx_counters(
    stats_logger: &mut StatsLogger,
    prev: &fidl_fuchsia_wlan_stats::ConnectionCounters,
    current: &fidl_fuchsia_wlan_stats::ConnectionCounters,
    duration: zx::MonotonicDuration,
) {
    let (current_rx_unicast_total, prev_rx_unicast_total) =
        match (current.rx_unicast_total, prev.rx_unicast_total) {
            (Some(current), Some(prev)) => (current, prev),
            _ => return,
        };
    let (current_rx_unicast_drop, prev_rx_unicast_drop) =
        match (current.rx_unicast_drop, prev.rx_unicast_drop) {
            (Some(current), Some(prev)) => (current, prev),
            _ => return,
        };

    let rx_total = current_rx_unicast_total - prev_rx_unicast_total;
    let rx_drop = current_rx_unicast_drop - prev_rx_unicast_drop;
    let rx_drop_rate = if rx_total > 0 { rx_drop as f64 / rx_total as f64 } else { 0f64 };

    stats_logger
        .log_stat(StatOp::AddRxPacketCounters {
            rx_unicast_total: rx_total,
            rx_unicast_drop: rx_drop,
        })
        .await;

    if rx_drop_rate > HIGH_PACKET_DROP_RATE_THRESHOLD {
        stats_logger.log_stat(StatOp::AddRxHighPacketDropDuration(duration)).await;
    }
    if rx_drop_rate > VERY_HIGH_PACKET_DROP_RATE_THRESHOLD {
        stats_logger.log_stat(StatOp::AddRxVeryHighPacketDropDuration(duration)).await;
    }
    if rx_total == 0 {
        stats_logger.log_stat(StatOp::AddNoRxDuration(duration)).await;
    }
}

async fn diff_and_log_tx_counters(
    stats_logger: &mut StatsLogger,
    prev: &fidl_fuchsia_wlan_stats::ConnectionCounters,
    current: &fidl_fuchsia_wlan_stats::ConnectionCounters,
    duration: zx::MonotonicDuration,
) {
    let (current_tx_total, prev_tx_total) = match (current.tx_total, prev.tx_total) {
        (Some(current), Some(prev)) => (current, prev),
        _ => return,
    };
    let (current_tx_drop, prev_tx_drop) = match (current.tx_drop, prev.tx_drop) {
        (Some(current), Some(prev)) => (current, prev),
        _ => return,
    };

    let tx_total = current_tx_total - prev_tx_total;
    let tx_drop = current_tx_drop - prev_tx_drop;
    let tx_drop_rate = if tx_total > 0 { tx_drop as f64 / tx_total as f64 } else { 0f64 };

    stats_logger.log_stat(StatOp::AddTxPacketCounters { tx_total, tx_drop }).await;

    if tx_drop_rate > HIGH_PACKET_DROP_RATE_THRESHOLD {
        stats_logger.log_stat(StatOp::AddTxHighPacketDropDuration(duration)).await;
    }
    if tx_drop_rate > VERY_HIGH_PACKET_DROP_RATE_THRESHOLD {
        stats_logger.log_stat(StatOp::AddTxVeryHighPacketDropDuration(duration)).await;
    }
}

struct StatsLogger {
    cobalt_1dot1_proxy: fidl_fuchsia_metrics::MetricEventLoggerProxy,
    time_series_stats: Arc<Mutex<TimeSeriesStats>>,
    last_1d_stats: Arc<Mutex<WindowedStats<StatCounters>>>,
    last_7d_stats: Arc<Mutex<WindowedStats<StatCounters>>>,
    last_successful_recovery: UintProperty,
    successful_recoveries: UintProperty,
    /// Stats aggregated for each day and then logged into Cobalt.
    /// As these stats are more detailed than `last_1d_stats`, we do not track per-hour
    /// windowed stats in order to reduce space and heap allocation. Instead, these stats
    /// are logged to Cobalt once every 24 hours and then cleared. Additionally, these
    /// are not logged into Inspect.
    last_1d_detailed_stats: DailyDetailedStats,
    stat_ops: Vec<StatOp>,
    hr_tick: u32,
    rssi_velocity_hist: HashMap<u32, fidl_fuchsia_metrics::HistogramBucket>,
    rssi_hist: HashMap<u32, fidl_fuchsia_metrics::HistogramBucket>,
    recovery_record: RecoveryRecord,
    throttled_error_logger: ThrottledErrorLogger,

    // Inspect nodes
    _time_series_inspect_node: LazyNode,
    _1d_counters_inspect_node: LazyNode,
    _7d_counters_inspect_node: LazyNode,
}

impl StatsLogger {
    pub fn new(
        cobalt_1dot1_proxy: fidl_fuchsia_metrics::MetricEventLoggerProxy,
        inspect_node: &InspectNode,
    ) -> Self {
        let time_series_stats = Arc::new(Mutex::new(TimeSeriesStats::new()));
        let last_1d_stats = Arc::new(Mutex::new(WindowedStats::new(24)));
        let last_7d_stats = Arc::new(Mutex::new(WindowedStats::new(7)));
        let last_successful_recovery = inspect_node.create_uint("last_successful_recovery", 0);
        let successful_recoveries = inspect_node.create_uint("successful_recoveries", 0);
        let _time_series_inspect_node = inspect_time_series::inspect_create_stats(
            inspect_node,
            "time_series",
            Arc::clone(&time_series_stats),
        );
        let _1d_counters_inspect_node =
            inspect_create_counters(inspect_node, "1d_counters", Arc::clone(&last_1d_stats));
        let _7d_counters_inspect_node =
            inspect_create_counters(inspect_node, "7d_counters", Arc::clone(&last_7d_stats));

        Self {
            cobalt_1dot1_proxy,
            time_series_stats,
            last_1d_stats,
            last_7d_stats,
            last_successful_recovery,
            successful_recoveries,
            last_1d_detailed_stats: DailyDetailedStats::new(),
            stat_ops: vec![],
            hr_tick: 0,
            rssi_velocity_hist: HashMap::new(),
            rssi_hist: HashMap::new(),
            recovery_record: RecoveryRecord::new(),
            throttled_error_logger: ThrottledErrorLogger::new(
                MINUTES_BETWEEN_COBALT_SYSLOG_WARNINGS,
            ),
            _1d_counters_inspect_node,
            _7d_counters_inspect_node,
            _time_series_inspect_node,
        }
    }

    async fn log_stat(&mut self, stat_op: StatOp) {
        self.log_time_series(&stat_op);
        self.log_stat_counters(stat_op);
    }

    fn log_time_series(&mut self, stat_op: &StatOp) {
        match stat_op {
            StatOp::AddTotalDuration(duration) => {
                self.time_series_stats
                    .lock()
                    .total_duration_sec
                    .log_value(&(round_to_nearest_second(*duration) as i32));
            }
            StatOp::AddConnectedDuration(duration) => {
                self.time_series_stats
                    .lock()
                    .connected_duration_sec
                    .log_value(&(round_to_nearest_second(*duration) as i32));
            }
            StatOp::AddConnectAttemptsCount => {
                self.time_series_stats.lock().connect_attempt_count.log_value(&1u32);
            }
            StatOp::AddConnectSuccessfulCount => {
                self.time_series_stats.lock().connect_successful_count.log_value(&1u32);
            }
            StatOp::AddDisconnectCount(..) => {
                self.time_series_stats.lock().disconnect_count.log_value(&1u32);
            }
            StatOp::AddRxPacketCounters { rx_unicast_total, rx_unicast_drop } => {
                self.time_series_stats
                    .lock()
                    .rx_unicast_total_count
                    .log_value(&(*rx_unicast_total as u32));
                self.time_series_stats
                    .lock()
                    .rx_unicast_drop_count
                    .log_value(&(*rx_unicast_drop as u32));
            }
            StatOp::AddTxPacketCounters { tx_total, tx_drop } => {
                self.time_series_stats.lock().tx_total_count.log_value(&(*tx_total as u32));
                self.time_series_stats.lock().tx_drop_count.log_value(&(*tx_drop as u32));
            }
            StatOp::AddNoRxDuration(duration) => {
                self.time_series_stats
                    .lock()
                    .no_rx_duration_sec
                    .log_value(&(round_to_nearest_second(*duration) as i32));
            }
            StatOp::AddDowntimeDuration(..)
            | StatOp::AddDowntimeNoSavedNeighborDuration(..)
            | StatOp::AddTxHighPacketDropDuration(..)
            | StatOp::AddRxHighPacketDropDuration(..)
            | StatOp::AddTxVeryHighPacketDropDuration(..)
            | StatOp::AddRxVeryHighPacketDropDuration(..) => (),
        }
    }

    fn log_stat_counters(&mut self, stat_op: StatOp) {
        let zero = StatCounters::default();
        let addition = match stat_op {
            StatOp::AddTotalDuration(duration) => StatCounters { total_duration: duration, ..zero },
            StatOp::AddConnectedDuration(duration) => {
                StatCounters { connected_duration: duration, ..zero }
            }
            StatOp::AddDowntimeDuration(duration) => {
                StatCounters { downtime_duration: duration, ..zero }
            }
            StatOp::AddDowntimeNoSavedNeighborDuration(duration) => {
                StatCounters { downtime_no_saved_neighbor_duration: duration, ..zero }
            }
            StatOp::AddConnectAttemptsCount => StatCounters { connect_attempts_count: 1, ..zero },
            StatOp::AddConnectSuccessfulCount => {
                StatCounters { connect_successful_count: 1, ..zero }
            }
            StatOp::AddDisconnectCount(disconnect_source) => match disconnect_source {
                fidl_sme::DisconnectSource::User(reason) => {
                    if is_roam_disconnect(reason) {
                        StatCounters { disconnect_count: 1, roaming_disconnect_count: 1, ..zero }
                    } else {
                        StatCounters { disconnect_count: 1, ..zero }
                    }
                }
                fidl_sme::DisconnectSource::Mlme(_) | fidl_sme::DisconnectSource::Ap(_) => {
                    StatCounters { disconnect_count: 1, non_roaming_disconnect_count: 1, ..zero }
                }
            },
            StatOp::AddTxHighPacketDropDuration(duration) => {
                StatCounters { tx_high_packet_drop_duration: duration, ..zero }
            }
            StatOp::AddRxHighPacketDropDuration(duration) => {
                StatCounters { rx_high_packet_drop_duration: duration, ..zero }
            }
            StatOp::AddTxVeryHighPacketDropDuration(duration) => {
                StatCounters { tx_very_high_packet_drop_duration: duration, ..zero }
            }
            StatOp::AddRxVeryHighPacketDropDuration(duration) => {
                StatCounters { rx_very_high_packet_drop_duration: duration, ..zero }
            }
            StatOp::AddNoRxDuration(duration) => StatCounters { no_rx_duration: duration, ..zero },
            StatOp::AddRxPacketCounters { .. } => StatCounters { ..zero },
            StatOp::AddTxPacketCounters { .. } => StatCounters { ..zero },
        };

        if addition != zero {
            self.last_1d_stats.lock().saturating_add(&addition);
            self.last_7d_stats.lock().saturating_add(&addition);
        }
    }

    // Queue stat operation to be logged later. This allows the caller to control the timing of
    // when stats are logged. This ensures that various counters are not inconsistent with each
    // other because one is logged early and the other one later.
    fn queue_stat_op(&mut self, stat_op: StatOp) {
        self.stat_ops.push(stat_op);
    }

    async fn log_queued_stats(&mut self) {
        while let Some(stat_op) = self.stat_ops.pop() {
            self.log_stat(stat_op).await;
        }
    }

    async fn report_connect_result(
        &mut self,
        policy_connect_reason: Option<client::types::ConnectReason>,
        code: fidl_ieee80211::StatusCode,
        multiple_bss_candidates: bool,
        ap_state: &client::types::ApState,
        connect_start_time: Option<fasync::MonotonicInstant>,
    ) {
        self.log_establish_connection_cobalt_metrics(
            policy_connect_reason,
            code,
            multiple_bss_candidates,
            ap_state,
            connect_start_time,
        )
        .await;

        *self.last_1d_detailed_stats.connect_attempts_status.entry(code).or_insert(0) += 1;

        let is_multi_bss_dim = convert::convert_is_multi_bss(multiple_bss_candidates);
        self.last_1d_detailed_stats
            .connect_per_is_multi_bss
            .entry(is_multi_bss_dim)
            .or_default()
            .increment(code);

        let security_type_dim = convert::convert_security_type(&ap_state.original().protection());
        self.last_1d_detailed_stats
            .connect_per_security_type
            .entry(security_type_dim)
            .or_default()
            .increment(code);

        self.last_1d_detailed_stats
            .connect_per_primary_channel
            .entry(ap_state.tracked.channel.primary)
            .or_default()
            .increment(code);

        let channel_band_dim = convert::convert_channel_band(ap_state.tracked.channel.primary);
        self.last_1d_detailed_stats
            .connect_per_channel_band
            .entry(channel_band_dim)
            .or_default()
            .increment(code);

        let rssi_bucket_dim = convert::convert_rssi_bucket(ap_state.tracked.signal.rssi_dbm);
        self.last_1d_detailed_stats
            .connect_per_rssi_bucket
            .entry(rssi_bucket_dim)
            .or_default()
            .increment(code);

        let snr_bucket_dim = convert::convert_snr_bucket(ap_state.tracked.signal.snr_db);
        self.last_1d_detailed_stats
            .connect_per_snr_bucket
            .entry(snr_bucket_dim)
            .or_default()
            .increment(code);
    }

    async fn log_daily_cobalt_metrics(&mut self) {
        self.log_daily_1d_cobalt_metrics().await;
        self.log_daily_7d_cobalt_metrics().await;
        self.log_daily_detailed_cobalt_metrics().await;
    }

    async fn log_daily_1d_cobalt_metrics(&mut self) {
        let mut metric_events = vec![];

        let c = self.last_1d_stats.lock().windowed_stat(None);
        let uptime_ratio = c.connected_duration.into_seconds() as f64
            / (c.connected_duration + c.adjusted_downtime()).into_seconds() as f64;
        if uptime_ratio.is_finite() {
            metric_events.push(MetricEvent {
                metric_id: metrics::CONNECTED_UPTIME_RATIO_METRIC_ID,
                event_codes: vec![],
                payload: MetricEventPayload::IntegerValue(float_to_ten_thousandth(uptime_ratio)),
            });
        }

        let connected_dur_in_day = c.connected_duration.into_seconds() as f64 / (24 * 3600) as f64;
        let dpdc_ratio = c.disconnect_count as f64 / connected_dur_in_day;
        if dpdc_ratio.is_finite() {
            metric_events.push(MetricEvent {
                metric_id: metrics::DISCONNECT_PER_DAY_CONNECTED_METRIC_ID,
                event_codes: vec![],
                payload: MetricEventPayload::IntegerValue(float_to_ten_thousandth(dpdc_ratio)),
            });
        }

        let roam_dpdc_ratio = c.roaming_disconnect_count as f64 / connected_dur_in_day;
        if roam_dpdc_ratio.is_finite() {
            metric_events.push(MetricEvent {
                metric_id: ROAMING_DISCONNECT_PER_DAY_CONNECTED_METRIC_ID,
                event_codes: vec![],
                payload: MetricEventPayload::IntegerValue(float_to_ten_thousandth(roam_dpdc_ratio)),
            });
        }

        let non_roam_dpdc_ratio = c.non_roaming_disconnect_count as f64 / connected_dur_in_day;
        if non_roam_dpdc_ratio.is_finite() {
            metric_events.push(MetricEvent {
                metric_id: NON_ROAMING_DISCONNECT_PER_DAY_CONNECTED_METRIC_ID,
                event_codes: vec![],
                payload: MetricEventPayload::IntegerValue(float_to_ten_thousandth(
                    non_roam_dpdc_ratio,
                )),
            });
        }

        let high_rx_drop_time_ratio = c.rx_high_packet_drop_duration.into_seconds() as f64
            / c.connected_duration.into_seconds() as f64;
        if high_rx_drop_time_ratio.is_finite() {
            metric_events.push(MetricEvent {
                metric_id: metrics::TIME_RATIO_WITH_HIGH_RX_PACKET_DROP_METRIC_ID,
                event_codes: vec![],
                payload: MetricEventPayload::IntegerValue(float_to_ten_thousandth(
                    high_rx_drop_time_ratio,
                )),
            });
        }

        let high_tx_drop_time_ratio = c.tx_high_packet_drop_duration.into_seconds() as f64
            / c.connected_duration.into_seconds() as f64;
        if high_tx_drop_time_ratio.is_finite() {
            metric_events.push(MetricEvent {
                metric_id: metrics::TIME_RATIO_WITH_HIGH_TX_PACKET_DROP_METRIC_ID,
                event_codes: vec![],
                payload: MetricEventPayload::IntegerValue(float_to_ten_thousandth(
                    high_tx_drop_time_ratio,
                )),
            });
        }

        let very_high_rx_drop_time_ratio = c.rx_very_high_packet_drop_duration.into_seconds()
            as f64
            / c.connected_duration.into_seconds() as f64;
        if very_high_rx_drop_time_ratio.is_finite() {
            metric_events.push(MetricEvent {
                metric_id: metrics::TIME_RATIO_WITH_VERY_HIGH_RX_PACKET_DROP_METRIC_ID,
                event_codes: vec![],
                payload: MetricEventPayload::IntegerValue(float_to_ten_thousandth(
                    very_high_rx_drop_time_ratio,
                )),
            });
        }

        let very_high_tx_drop_time_ratio = c.tx_very_high_packet_drop_duration.into_seconds()
            as f64
            / c.connected_duration.into_seconds() as f64;
        if very_high_tx_drop_time_ratio.is_finite() {
            metric_events.push(MetricEvent {
                metric_id: metrics::TIME_RATIO_WITH_VERY_HIGH_TX_PACKET_DROP_METRIC_ID,
                event_codes: vec![],
                payload: MetricEventPayload::IntegerValue(float_to_ten_thousandth(
                    very_high_tx_drop_time_ratio,
                )),
            });
        }

        let no_rx_time_ratio =
            c.no_rx_duration.into_seconds() as f64 / c.connected_duration.into_seconds() as f64;
        if no_rx_time_ratio.is_finite() {
            metric_events.push(MetricEvent {
                metric_id: metrics::TIME_RATIO_WITH_NO_RX_METRIC_ID,
                event_codes: vec![],
                payload: MetricEventPayload::IntegerValue(float_to_ten_thousandth(
                    no_rx_time_ratio,
                )),
            });
        }

        let connection_success_rate = c.connection_success_rate();
        if connection_success_rate.is_finite() {
            metric_events.push(MetricEvent {
                metric_id: metrics::CONNECTION_SUCCESS_RATE_METRIC_ID,
                event_codes: vec![],
                payload: MetricEventPayload::IntegerValue(float_to_ten_thousandth(
                    connection_success_rate,
                )),
            });
        }

        self.throttled_error_logger.throttle_error(log_cobalt_1dot1_batch!(
            self.cobalt_1dot1_proxy,
            &metric_events,
            "log_daily_1d_cobalt_metrics",
        ));
    }

    async fn log_daily_7d_cobalt_metrics(&mut self) {
        let c = self.last_7d_stats.lock().windowed_stat(None);
        let connected_dur_in_day = c.connected_duration.into_seconds() as f64 / (24 * 3600) as f64;
        let dpdc_ratio = c.disconnect_count as f64 / connected_dur_in_day;
        #[allow(clippy::vec_init_then_push, reason = "mass allow for https://fxbug.dev/381896734")]
        if dpdc_ratio.is_finite() {
            let mut metric_events = vec![];
            metric_events.push(MetricEvent {
                metric_id: metrics::DISCONNECT_PER_DAY_CONNECTED_7D_METRIC_ID,
                event_codes: vec![],
                payload: MetricEventPayload::IntegerValue(float_to_ten_thousandth(dpdc_ratio)),
            });

            self.throttled_error_logger.throttle_error(log_cobalt_1dot1_batch!(
                self.cobalt_1dot1_proxy,
                &metric_events,
                "log_daily_7d_cobalt_metrics",
            ));
        }
    }

    async fn log_daily_detailed_cobalt_metrics(&mut self) {
        let mut metric_events = vec![];

        let c = self.last_1d_stats.lock().windowed_stat(None);
        if c.connection_success_rate().is_finite() {
            let device_low_connection_success =
                c.connection_success_rate() < DEVICE_LOW_CONNECTION_SUCCESS_RATE_THRESHOLD;
            for (status_code, count) in &self.last_1d_detailed_stats.connect_attempts_status {
                metric_events.push(MetricEvent {
                    metric_id: if device_low_connection_success {
                        metrics::CONNECT_ATTEMPT_ON_BAD_DEVICE_BREAKDOWN_BY_STATUS_CODE_METRIC_ID
                    } else {
                        metrics::CONNECT_ATTEMPT_ON_NORMAL_DEVICE_BREAKDOWN_BY_STATUS_CODE_METRIC_ID
                    },
                    event_codes: vec![*status_code as u32],
                    payload: MetricEventPayload::Count(*count),
                });
            }

            for (is_multi_bss_dim, counters) in
                &self.last_1d_detailed_stats.connect_per_is_multi_bss
            {
                let success_rate = counters.success as f64 / counters.total as f64;
                metric_events.push(MetricEvent {
                    metric_id:
                        metrics::DAILY_CONNECT_SUCCESS_RATE_BREAKDOWN_BY_IS_MULTI_BSS_METRIC_ID,
                    event_codes: vec![*is_multi_bss_dim as u32],
                    payload: MetricEventPayload::IntegerValue(float_to_ten_thousandth(
                        success_rate,
                    )),
                });
            }

            for (security_type_dim, counters) in
                &self.last_1d_detailed_stats.connect_per_security_type
            {
                let success_rate = counters.success as f64 / counters.total as f64;
                metric_events.push(MetricEvent {
                    metric_id:
                        metrics::DAILY_CONNECT_SUCCESS_RATE_BREAKDOWN_BY_SECURITY_TYPE_METRIC_ID,
                    event_codes: vec![*security_type_dim as u32],
                    payload: MetricEventPayload::IntegerValue(float_to_ten_thousandth(
                        success_rate,
                    )),
                });
            }

            for (primary_channel, counters) in
                &self.last_1d_detailed_stats.connect_per_primary_channel
            {
                let success_rate = counters.success as f64 / counters.total as f64;
                metric_events.push(MetricEvent {
                    metric_id:
                        metrics::DAILY_CONNECT_SUCCESS_RATE_BREAKDOWN_BY_PRIMARY_CHANNEL_METRIC_ID,
                    event_codes: vec![*primary_channel as u32],
                    payload: MetricEventPayload::IntegerValue(float_to_ten_thousandth(
                        success_rate,
                    )),
                });
            }

            for (channel_band_dim, counters) in
                &self.last_1d_detailed_stats.connect_per_channel_band
            {
                let success_rate = counters.success as f64 / counters.total as f64;
                metric_events.push(MetricEvent {
                    metric_id:
                        metrics::DAILY_CONNECT_SUCCESS_RATE_BREAKDOWN_BY_CHANNEL_BAND_METRIC_ID,
                    event_codes: vec![*channel_band_dim as u32],
                    payload: MetricEventPayload::IntegerValue(float_to_ten_thousandth(
                        success_rate,
                    )),
                });
            }

            for (rssi_bucket_dim, counters) in &self.last_1d_detailed_stats.connect_per_rssi_bucket
            {
                let success_rate = counters.success as f64 / counters.total as f64;
                metric_events.push(MetricEvent {
                    metric_id:
                        metrics::DAILY_CONNECT_SUCCESS_RATE_BREAKDOWN_BY_RSSI_BUCKET_METRIC_ID,
                    event_codes: vec![*rssi_bucket_dim as u32],
                    payload: MetricEventPayload::IntegerValue(float_to_ten_thousandth(
                        success_rate,
                    )),
                });
            }

            for (snr_bucket_dim, counters) in &self.last_1d_detailed_stats.connect_per_snr_bucket {
                let success_rate = counters.success as f64 / counters.total as f64;
                metric_events.push(MetricEvent {
                    metric_id:
                        metrics::DAILY_CONNECT_SUCCESS_RATE_BREAKDOWN_BY_SNR_BUCKET_METRIC_ID,
                    event_codes: vec![*snr_bucket_dim as u32],
                    payload: MetricEventPayload::IntegerValue(float_to_ten_thousandth(
                        success_rate,
                    )),
                });
            }
        }

        self.throttled_error_logger.throttle_error(log_cobalt_1dot1_batch!(
            self.cobalt_1dot1_proxy,
            &metric_events,
            "log_daily_detailed_cobalt_metrics",
        ));
    }

    async fn handle_hr_passed(&mut self) {
        self.log_hourly_fleetwise_quality_cobalt_metrics().await;

        self.hr_tick = (self.hr_tick + 1) % 24;
        self.last_1d_stats.lock().slide_window();
        if self.hr_tick == 0 {
            self.last_7d_stats.lock().slide_window();
            self.last_1d_detailed_stats = DailyDetailedStats::new();
        }

        self.log_hourly_rssi_histogram_metrics().await;
    }

    // Send out the RSSI and RSSI velocity metrics that have been collected over the last hour.
    async fn log_hourly_rssi_histogram_metrics(&mut self) {
        let rssi_buckets: Vec<_> = self.rssi_hist.values().copied().collect();
        self.throttled_error_logger.throttle_error(log_cobalt_1dot1!(
            self.cobalt_1dot1_proxy,
            log_integer_histogram,
            metrics::CONNECTION_RSSI_METRIC_ID,
            &rssi_buckets,
            &[],
        ));
        self.rssi_hist.clear();

        let velocity_buckets: Vec<_> = self.rssi_velocity_hist.values().copied().collect();
        self.throttled_error_logger.throttle_error(log_cobalt_1dot1!(
            self.cobalt_1dot1_proxy,
            log_integer_histogram,
            metrics::RSSI_VELOCITY_METRIC_ID,
            &velocity_buckets,
            &[],
        ));
        self.rssi_velocity_hist.clear();
    }

    async fn log_hourly_fleetwise_quality_cobalt_metrics(&mut self) {
        let mut metric_events = vec![];

        // Get stats from the last hour
        let c = self.last_1d_stats.lock().windowed_stat(Some(1));
        let total_wlan_uptime = c.connected_duration + c.adjusted_downtime();

        // Log the durations calculated in the last hour
        metric_events.push(MetricEvent {
            metric_id: metrics::TOTAL_WLAN_UPTIME_NEAR_SAVED_NETWORK_METRIC_ID,
            event_codes: vec![],
            payload: MetricEventPayload::IntegerValue(total_wlan_uptime.into_micros()),
        });
        metric_events.push(MetricEvent {
            metric_id: metrics::TOTAL_CONNECTED_UPTIME_METRIC_ID,
            event_codes: vec![],
            payload: MetricEventPayload::IntegerValue(c.connected_duration.into_micros()),
        });
        metric_events.push(MetricEvent {
            metric_id: metrics::TOTAL_TIME_WITH_HIGH_RX_PACKET_DROP_METRIC_ID,
            event_codes: vec![],
            payload: MetricEventPayload::IntegerValue(c.rx_high_packet_drop_duration.into_micros()),
        });
        metric_events.push(MetricEvent {
            metric_id: metrics::TOTAL_TIME_WITH_HIGH_TX_PACKET_DROP_METRIC_ID,
            event_codes: vec![],
            payload: MetricEventPayload::IntegerValue(c.tx_high_packet_drop_duration.into_micros()),
        });
        metric_events.push(MetricEvent {
            metric_id: metrics::TOTAL_TIME_WITH_VERY_HIGH_RX_PACKET_DROP_METRIC_ID,
            event_codes: vec![],
            payload: MetricEventPayload::IntegerValue(
                c.rx_very_high_packet_drop_duration.into_micros(),
            ),
        });
        metric_events.push(MetricEvent {
            metric_id: metrics::TOTAL_TIME_WITH_VERY_HIGH_TX_PACKET_DROP_METRIC_ID,
            event_codes: vec![],
            payload: MetricEventPayload::IntegerValue(
                c.tx_very_high_packet_drop_duration.into_micros(),
            ),
        });
        metric_events.push(MetricEvent {
            metric_id: metrics::TOTAL_TIME_WITH_NO_RX_METRIC_ID,
            event_codes: vec![],
            payload: MetricEventPayload::IntegerValue(c.no_rx_duration.into_micros()),
        });

        self.throttled_error_logger.throttle_error(log_cobalt_1dot1_batch!(
            self.cobalt_1dot1_proxy,
            &metric_events,
            "log_hourly_fleetwise_quality_cobalt_metrics",
        ));
    }

    async fn log_disconnect_cobalt_metrics(
        &mut self,
        disconnect_info: &DisconnectInfo,
        multiple_bss_candidates: bool,
    ) {
        let mut metric_events = vec![];
        let policy_disconnect_reason_dim = {
            use metrics::PolicyDisconnectionMigratedMetricDimensionReason::*;
            match &disconnect_info.disconnect_source {
                fidl_sme::DisconnectSource::User(reason) => match reason {
                    fidl_sme::UserDisconnectReason::Unknown => Unknown,
                    fidl_sme::UserDisconnectReason::FailedToConnect => FailedToConnect,
                    fidl_sme::UserDisconnectReason::FidlConnectRequest => FidlConnectRequest,
                    fidl_sme::UserDisconnectReason::FidlStopClientConnectionsRequest => {
                        FidlStopClientConnectionsRequest
                    }
                    fidl_sme::UserDisconnectReason::ProactiveNetworkSwitch => {
                        ProactiveNetworkSwitch
                    }
                    fidl_sme::UserDisconnectReason::DisconnectDetectedFromSme => {
                        DisconnectDetectedFromSme
                    }
                    fidl_sme::UserDisconnectReason::RegulatoryRegionChange => {
                        RegulatoryRegionChange
                    }
                    fidl_sme::UserDisconnectReason::Startup => Startup,
                    fidl_sme::UserDisconnectReason::NetworkUnsaved => NetworkUnsaved,
                    fidl_sme::UserDisconnectReason::NetworkConfigUpdated => NetworkConfigUpdated,
                    fidl_sme::UserDisconnectReason::WlanstackUnitTesting
                    | fidl_sme::UserDisconnectReason::WlanSmeUnitTesting
                    | fidl_sme::UserDisconnectReason::WlanServiceUtilTesting
                    | fidl_sme::UserDisconnectReason::WlanDevTool
                    | fidl_sme::UserDisconnectReason::Recovery => Unknown,
                },
                fidl_sme::DisconnectSource::Ap(..) | fidl_sme::DisconnectSource::Mlme(..) => {
                    DisconnectDetectedFromSme
                }
            }
        };
        metric_events.push(MetricEvent {
            metric_id: metrics::POLICY_DISCONNECTION_MIGRATED_METRIC_ID,
            event_codes: vec![policy_disconnect_reason_dim as u32],
            payload: MetricEventPayload::Count(1),
        });

        metric_events.push(MetricEvent {
            metric_id: metrics::TOTAL_DISCONNECT_COUNT_METRIC_ID,
            event_codes: vec![],
            payload: MetricEventPayload::Count(1),
        });

        let device_uptime_dim = {
            use metrics::DisconnectBreakdownByDeviceUptimeMetricDimensionDeviceUptime::*;
            match fasync::MonotonicInstant::now() - fasync::MonotonicInstant::from_nanos(0) {
                x if x < zx::MonotonicDuration::from_hours(1) => LessThan1Hour,
                x if x < zx::MonotonicDuration::from_hours(3) => LessThan3Hours,
                x if x < zx::MonotonicDuration::from_hours(12) => LessThan12Hours,
                x if x < zx::MonotonicDuration::from_hours(24) => LessThan1Day,
                x if x < zx::MonotonicDuration::from_hours(48) => LessThan2Days,
                _ => AtLeast2Days,
            }
        };
        metric_events.push(MetricEvent {
            metric_id: metrics::DISCONNECT_BREAKDOWN_BY_DEVICE_UPTIME_METRIC_ID,
            event_codes: vec![device_uptime_dim as u32],
            payload: MetricEventPayload::Count(1),
        });

        let connected_duration_dim = {
            use metrics::DisconnectBreakdownByConnectedDurationMetricDimensionConnectedDuration::*;
            match disconnect_info.connected_duration {
                x if x < zx::MonotonicDuration::from_seconds(30) => LessThan30Seconds,
                x if x < zx::MonotonicDuration::from_minutes(5) => LessThan5Minutes,
                x if x < zx::MonotonicDuration::from_hours(1) => LessThan1Hour,
                x if x < zx::MonotonicDuration::from_hours(6) => LessThan6Hours,
                x if x < zx::MonotonicDuration::from_hours(24) => LessThan24Hours,
                _ => AtLeast24Hours,
            }
        };
        metric_events.push(MetricEvent {
            metric_id: metrics::DISCONNECT_BREAKDOWN_BY_CONNECTED_DURATION_METRIC_ID,
            event_codes: vec![connected_duration_dim as u32],
            payload: MetricEventPayload::Count(1),
        });

        let disconnect_source_dim =
            convert::convert_disconnect_source(&disconnect_info.disconnect_source);
        metric_events.push(MetricEvent {
            metric_id: metrics::DISCONNECT_BREAKDOWN_BY_REASON_CODE_METRIC_ID,
            event_codes: vec![
                disconnect_info.disconnect_source.cobalt_reason_code() as u32,
                disconnect_source_dim as u32,
            ],
            payload: MetricEventPayload::Count(1),
        });

        metric_events.push(MetricEvent {
            metric_id: metrics::DISCONNECT_BREAKDOWN_BY_PRIMARY_CHANNEL_METRIC_ID,
            event_codes: vec![disconnect_info.ap_state.tracked.channel.primary as u32],
            payload: MetricEventPayload::Count(1),
        });
        let channel_band_dim =
            convert::convert_channel_band(disconnect_info.ap_state.tracked.channel.primary);
        metric_events.push(MetricEvent {
            metric_id: metrics::DISCONNECT_BREAKDOWN_BY_CHANNEL_BAND_METRIC_ID,
            event_codes: vec![channel_band_dim as u32],
            payload: MetricEventPayload::Count(1),
        });
        let is_multi_bss_dim = convert::convert_is_multi_bss(multiple_bss_candidates);
        metric_events.push(MetricEvent {
            metric_id: metrics::DISCONNECT_BREAKDOWN_BY_IS_MULTI_BSS_METRIC_ID,
            event_codes: vec![is_multi_bss_dim as u32],
            payload: MetricEventPayload::Count(1),
        });
        let security_type_dim =
            convert::convert_security_type(&disconnect_info.ap_state.original().protection());
        metric_events.push(MetricEvent {
            metric_id: metrics::DISCONNECT_BREAKDOWN_BY_SECURITY_TYPE_METRIC_ID,
            event_codes: vec![security_type_dim as u32],
            payload: MetricEventPayload::Count(1),
        });

        // Log connect duration for roaming, non-roaming, not including manual disconnects
        // triggered by a user.
        let duration_minutes = disconnect_info.connected_duration.into_minutes();
        if let fidl_sme::DisconnectSource::User(reason) = disconnect_info.disconnect_source {
            // Log duration for roaming/total disconnects if the disconnect is for roaming, but not
            // if it is another user reason such as an unsaved network.
            if is_roam_disconnect(reason) {
                metric_events.push(MetricEvent {
                    metric_id: CONNECTED_DURATION_BEFORE_ROAMING_DISCONNECT_METRIC_ID,
                    event_codes: vec![],
                    payload: MetricEventPayload::IntegerValue(duration_minutes),
                });
                metric_events.push(MetricEvent {
                    metric_id: NETWORK_ROAMING_DISCONNECT_COUNTS_METRIC_ID,
                    event_codes: vec![],
                    payload: MetricEventPayload::Count(1),
                });
            }
        } else {
            metric_events.push(MetricEvent {
                metric_id: CONNECTED_DURATION_BEFORE_NON_ROAMING_DISCONNECT_METRIC_ID,
                event_codes: vec![],
                payload: MetricEventPayload::IntegerValue(duration_minutes),
            });
            metric_events.push(MetricEvent {
                metric_id: NETWORK_NON_ROAMING_DISCONNECT_COUNTS_METRIC_ID,
                event_codes: vec![],
                payload: MetricEventPayload::Count(1),
            });
        }

        metric_events.push(MetricEvent {
            metric_id: metrics::CONNECTED_DURATION_BEFORE_DISCONNECT_METRIC_ID,
            event_codes: vec![],
            payload: MetricEventPayload::IntegerValue(duration_minutes),
        });

        metric_events.push(MetricEvent {
            metric_id: metrics::NETWORK_DISCONNECT_COUNTS_METRIC_ID,
            event_codes: vec![],
            payload: MetricEventPayload::Count(1),
        });

        if disconnect_info.disconnect_source
            == fidl_sme::DisconnectSource::User(fidl_sme::UserDisconnectReason::FidlConnectRequest)
        {
            metric_events.push(MetricEvent {
                metric_id: metrics::MANUAL_NETWORK_CHANGE_METRIC_ID,
                event_codes: vec![],
                payload: MetricEventPayload::Count(1),
            });
        }

        self.throttled_error_logger.throttle_error(log_cobalt_1dot1_batch!(
            self.cobalt_1dot1_proxy,
            &metric_events,
            "log_disconnect_cobalt_metrics",
        ));
    }

    async fn log_active_scan_requested_cobalt_metrics(&mut self, num_ssids_requested: usize) {
        use metrics::ActiveScanRequestedForNetworkSelectionMigratedMetricDimensionActiveScanSsidsRequested as ActiveScanSsidsRequested;
        let active_scan_ssids_requested_dim = match num_ssids_requested {
            0 => ActiveScanSsidsRequested::Zero,
            1 => ActiveScanSsidsRequested::One,
            2..=4 => ActiveScanSsidsRequested::TwoToFour,
            5..=10 => ActiveScanSsidsRequested::FiveToTen,
            11..=20 => ActiveScanSsidsRequested::ElevenToTwenty,
            21..=50 => ActiveScanSsidsRequested::TwentyOneToFifty,
            51..=100 => ActiveScanSsidsRequested::FiftyOneToOneHundred,
            101.. => ActiveScanSsidsRequested::OneHundredAndOneOrMore,
        };
        self.throttled_error_logger.throttle_error(log_cobalt_1dot1!(
            self.cobalt_1dot1_proxy,
            log_occurrence,
            metrics::ACTIVE_SCAN_REQUESTED_FOR_NETWORK_SELECTION_MIGRATED_METRIC_ID,
            1,
            &[active_scan_ssids_requested_dim as u32],
        ));
    }

    async fn log_active_scan_requested_via_api_cobalt_metrics(
        &mut self,
        num_ssids_requested: usize,
    ) {
        use metrics::ActiveScanRequestedForPolicyApiMetricDimensionActiveScanSsidsRequested as ActiveScanSsidsRequested;
        let active_scan_ssids_requested_dim = match num_ssids_requested {
            0 => ActiveScanSsidsRequested::Zero,
            1 => ActiveScanSsidsRequested::One,
            2..=4 => ActiveScanSsidsRequested::TwoToFour,
            5..=10 => ActiveScanSsidsRequested::FiveToTen,
            11..=20 => ActiveScanSsidsRequested::ElevenToTwenty,
            21..=50 => ActiveScanSsidsRequested::TwentyOneToFifty,
            51..=100 => ActiveScanSsidsRequested::FiftyOneToOneHundred,
            101.. => ActiveScanSsidsRequested::OneHundredAndOneOrMore,
        };
        self.throttled_error_logger.throttle_error(log_cobalt_1dot1!(
            self.cobalt_1dot1_proxy,
            log_occurrence,
            metrics::ACTIVE_SCAN_REQUESTED_FOR_POLICY_API_METRIC_ID,
            1,
            &[active_scan_ssids_requested_dim as u32],
        ));
    }

    async fn log_saved_network_counts(
        &mut self,
        saved_network_count: usize,
        config_count_per_saved_network: Vec<usize>,
    ) {
        let mut metric_events = vec![];

        // Count the total number of saved networks
        use metrics::SavedNetworksMigratedMetricDimensionSavedNetworks as SavedNetworksCount;
        let num_networks = match saved_network_count {
            0 => SavedNetworksCount::Zero,
            1 => SavedNetworksCount::One,
            2..=4 => SavedNetworksCount::TwoToFour,
            5..=40 => SavedNetworksCount::FiveToForty,
            41..=500 => SavedNetworksCount::FortyToFiveHundred,
            501.. => SavedNetworksCount::FiveHundredAndOneOrMore,
        };
        metric_events.push(MetricEvent {
            metric_id: metrics::SAVED_NETWORKS_MIGRATED_METRIC_ID,
            event_codes: vec![num_networks as u32],
            payload: MetricEventPayload::Count(1),
        });

        // Count the number of configs for each saved network
        use metrics::SavedConfigurationsForSavedNetworkMigratedMetricDimensionSavedConfigurations as ConfigCountDimension;
        for config_count in config_count_per_saved_network {
            let num_configs = match config_count {
                0 => ConfigCountDimension::Zero,
                1 => ConfigCountDimension::One,
                2..=4 => ConfigCountDimension::TwoToFour,
                5..=40 => ConfigCountDimension::FiveToForty,
                41..=500 => ConfigCountDimension::FortyToFiveHundred,
                501.. => ConfigCountDimension::FiveHundredAndOneOrMore,
            };
            metric_events.push(MetricEvent {
                metric_id: metrics::SAVED_CONFIGURATIONS_FOR_SAVED_NETWORK_MIGRATED_METRIC_ID,
                event_codes: vec![num_configs as u32],
                payload: MetricEventPayload::Count(1),
            });
        }

        self.throttled_error_logger.throttle_error(log_cobalt_1dot1_batch!(
            self.cobalt_1dot1_proxy,
            &metric_events,
            "log_saved_network_counts",
        ));
    }

    async fn log_network_selection_scan_interval(
        &mut self,
        time_since_last_scan: zx::MonotonicDuration,
    ) {
        self.throttled_error_logger.throttle_error(log_cobalt_1dot1!(
            self.cobalt_1dot1_proxy,
            log_integer,
            metrics::LAST_SCAN_AGE_WHEN_SCAN_REQUESTED_MIGRATED_METRIC_ID,
            time_since_last_scan.into_micros(),
            &[],
        ));
    }

    async fn log_connection_selection_scan_results(
        &mut self,
        saved_network_count: usize,
        bss_count_per_saved_network: Vec<usize>,
        saved_network_count_found_by_active_scan: usize,
    ) {
        let mut metric_events = vec![];

        use metrics::SavedNetworkInScanResultMigratedMetricDimensionBssCount as BssCount;
        for bss_count in bss_count_per_saved_network {
            // Record how many BSSs are visible in the scan results for this saved network.
            let bss_count_metric = match bss_count {
                0 => BssCount::Zero, // The ::Zero enum exists, but we shouldn't get a scan result with no BSS
                1 => BssCount::One,
                2..=4 => BssCount::TwoToFour,
                5..=10 => BssCount::FiveToTen,
                11..=20 => BssCount::ElevenToTwenty,
                21.. => BssCount::TwentyOneOrMore,
            };
            metric_events.push(MetricEvent {
                metric_id: metrics::SAVED_NETWORK_IN_SCAN_RESULT_MIGRATED_METRIC_ID,
                event_codes: vec![bss_count_metric as u32],
                payload: MetricEventPayload::Count(1),
            });
        }

        use metrics::ScanResultsReceivedMigratedMetricDimensionSavedNetworksCount as SavedNetworkCount;
        let saved_network_count_metric = match saved_network_count {
            0 => SavedNetworkCount::Zero,
            1 => SavedNetworkCount::One,
            2..=4 => SavedNetworkCount::TwoToFour,
            5..=20 => SavedNetworkCount::FiveToTwenty,
            21..=40 => SavedNetworkCount::TwentyOneToForty,
            41.. => SavedNetworkCount::FortyOneOrMore,
        };
        metric_events.push(MetricEvent {
            metric_id: metrics::SCAN_RESULTS_RECEIVED_MIGRATED_METRIC_ID,
            event_codes: vec![saved_network_count_metric as u32],
            payload: MetricEventPayload::Count(1),
        });

        use metrics::SavedNetworkInScanResultWithActiveScanMigratedMetricDimensionActiveScanSsidsObserved as ActiveScanSsidsObserved;
        let actively_scanned_networks_metrics = match saved_network_count_found_by_active_scan {
            0 => ActiveScanSsidsObserved::Zero,
            1 => ActiveScanSsidsObserved::One,
            2..=4 => ActiveScanSsidsObserved::TwoToFour,
            5..=10 => ActiveScanSsidsObserved::FiveToTen,
            11..=20 => ActiveScanSsidsObserved::ElevenToTwenty,
            21..=50 => ActiveScanSsidsObserved::TwentyOneToFifty,
            51..=100 => ActiveScanSsidsObserved::FiftyOneToOneHundred,
            101.. => ActiveScanSsidsObserved::OneHundredAndOneOrMore,
        };
        metric_events.push(MetricEvent {
            metric_id: metrics::SAVED_NETWORK_IN_SCAN_RESULT_WITH_ACTIVE_SCAN_MIGRATED_METRIC_ID,
            event_codes: vec![actively_scanned_networks_metrics as u32],
            payload: MetricEventPayload::Count(1),
        });

        self.throttled_error_logger.throttle_error(log_cobalt_1dot1_batch!(
            self.cobalt_1dot1_proxy,
            &metric_events,
            "log_connection_selection_scan_results",
        ));
    }

    async fn log_establish_connection_cobalt_metrics(
        &mut self,
        policy_connect_reason: Option<client::types::ConnectReason>,
        code: fidl_ieee80211::StatusCode,
        multiple_bss_candidates: bool,
        ap_state: &client::types::ApState,
        connect_start_time: Option<fasync::MonotonicInstant>,
    ) {
        let metric_events = self.build_establish_connection_cobalt_metrics(
            policy_connect_reason,
            code,
            multiple_bss_candidates,
            ap_state,
            connect_start_time,
        );
        self.throttled_error_logger.throttle_error(log_cobalt_1dot1_batch!(
            self.cobalt_1dot1_proxy,
            &metric_events,
            "log_establish_connection_cobalt_metrics",
        ));
    }

    fn build_establish_connection_cobalt_metrics(
        &mut self,
        policy_connect_reason: Option<client::types::ConnectReason>,
        code: fidl_ieee80211::StatusCode,
        multiple_bss_candidates: bool,
        ap_state: &client::types::ApState,
        connect_start_time: Option<fasync::MonotonicInstant>,
    ) -> Vec<MetricEvent> {
        let mut metric_events = vec![];
        if let Some(policy_connect_reason) = policy_connect_reason {
            metric_events.push(MetricEvent {
                metric_id: metrics::POLICY_CONNECTION_ATTEMPT_MIGRATED_METRIC_ID,
                event_codes: vec![policy_connect_reason as u32],
                payload: MetricEventPayload::Count(1),
            });

            // Also log non-retry connect attempts without dimension
            match policy_connect_reason {
                metrics::PolicyConnectionAttemptMigratedMetricDimensionReason::FidlConnectRequest
                | metrics::PolicyConnectionAttemptMigratedMetricDimensionReason::ProactiveNetworkSwitch
                | metrics::PolicyConnectionAttemptMigratedMetricDimensionReason::IdleInterfaceAutoconnect
                | metrics::PolicyConnectionAttemptMigratedMetricDimensionReason::NewSavedNetworkAutoconnect => {
                    metric_events.push(MetricEvent {
                        metric_id: metrics::POLICY_CONNECTION_ATTEMPTS_METRIC_ID,
                        event_codes: vec![],
                        payload: MetricEventPayload::Count(1),
                    });
                }
                metrics::PolicyConnectionAttemptMigratedMetricDimensionReason::RetryAfterDisconnectDetected
                | metrics::PolicyConnectionAttemptMigratedMetricDimensionReason::RetryAfterFailedConnectAttempt
                | metrics::PolicyConnectionAttemptMigratedMetricDimensionReason::RegulatoryChangeReconnect => (),
            }
        }

        metric_events.push(MetricEvent {
            metric_id: metrics::CONNECT_ATTEMPT_BREAKDOWN_BY_STATUS_CODE_METRIC_ID,
            event_codes: vec![code as u32],
            payload: MetricEventPayload::Count(1),
        });

        if code != fidl_ieee80211::StatusCode::Success {
            return metric_events;
        }

        match connect_start_time {
            Some(start_time) => {
                let user_wait_time = fasync::MonotonicInstant::now() - start_time;
                let user_wait_time_dim = convert::convert_user_wait_time(user_wait_time);
                metric_events.push(MetricEvent {
                    metric_id: metrics::SUCCESSFUL_CONNECT_BREAKDOWN_BY_USER_WAIT_TIME_METRIC_ID,
                    event_codes: vec![user_wait_time_dim as u32],
                    payload: MetricEventPayload::Count(1),
                });
            }
            None => warn!(
                "Metric for user wait time on connect is not logged because \
                 the start time is not populated"
            ),
        }

        let is_multi_bss_dim = convert::convert_is_multi_bss(multiple_bss_candidates);
        metric_events.push(MetricEvent {
            metric_id: metrics::SUCCESSFUL_CONNECT_BREAKDOWN_BY_IS_MULTI_BSS_METRIC_ID,
            event_codes: vec![is_multi_bss_dim as u32],
            payload: MetricEventPayload::Count(1),
        });

        let security_type_dim = convert::convert_security_type(&ap_state.original().protection());
        metric_events.push(MetricEvent {
            metric_id: metrics::SUCCESSFUL_CONNECT_BREAKDOWN_BY_SECURITY_TYPE_METRIC_ID,
            event_codes: vec![security_type_dim as u32],
            payload: MetricEventPayload::Count(1),
        });

        metric_events.push(MetricEvent {
            metric_id: metrics::SUCCESSFUL_CONNECT_BREAKDOWN_BY_PRIMARY_CHANNEL_METRIC_ID,
            event_codes: vec![ap_state.tracked.channel.primary as u32],
            payload: MetricEventPayload::Count(1),
        });

        let channel_band_dim = convert::convert_channel_band(ap_state.tracked.channel.primary);
        metric_events.push(MetricEvent {
            metric_id: metrics::SUCCESSFUL_CONNECT_BREAKDOWN_BY_CHANNEL_BAND_METRIC_ID,
            event_codes: vec![channel_band_dim as u32],
            payload: MetricEventPayload::Count(1),
        });

        let oui = ap_state.original().bssid.to_oui_uppercase("");
        metric_events.push(MetricEvent {
            metric_id: metrics::SUCCESSFUL_CONNECT_PER_OUI_METRIC_ID,
            event_codes: vec![],
            payload: MetricEventPayload::StringValue(oui),
        });
        metric_events
    }

    async fn log_downtime_cobalt_metrics(
        &mut self,
        downtime: zx::MonotonicDuration,
        disconnect_info: &DisconnectInfo,
    ) {
        let disconnect_source_dim =
            convert::convert_disconnect_source(&disconnect_info.disconnect_source);
        self.throttled_error_logger.throttle_error(log_cobalt_1dot1!(
            self.cobalt_1dot1_proxy,
            log_integer,
            metrics::DOWNTIME_BREAKDOWN_BY_DISCONNECT_REASON_METRIC_ID,
            downtime.into_micros(),
            &[
                disconnect_info.disconnect_source.cobalt_reason_code() as u32,
                disconnect_source_dim as u32
            ],
        ));
    }

    async fn log_reconnect_cobalt_metrics(
        &mut self,
        reconnect_duration: zx::MonotonicDuration,
        disconnect_reason: fidl_sme::DisconnectSource,
    ) {
        let mut metric_events = vec![];
        let reconnect_duration_dim = {
            use metrics::ConnectivityWlanMetricDimensionReconnectDuration::*;
            match reconnect_duration {
                x if x < zx::MonotonicDuration::from_millis(100) => LessThan100Milliseconds,
                x if x < zx::MonotonicDuration::from_seconds(1) => LessThan1Second,
                x if x < zx::MonotonicDuration::from_seconds(5) => LessThan5Seconds,
                x if x < zx::MonotonicDuration::from_seconds(30) => LessThan30Seconds,
                _ => AtLeast30Seconds,
            }
        };
        metric_events.push(MetricEvent {
            metric_id: metrics::RECONNECT_BREAKDOWN_BY_DURATION_METRIC_ID,
            event_codes: vec![reconnect_duration_dim as u32],
            payload: MetricEventPayload::Count(1),
        });

        // Log the reconnect time for roaming or for unexpected disconnects such as lost signal or
        // connection terminated by AP.
        if let fidl_sme::DisconnectSource::User(reason) = disconnect_reason {
            if is_roam_disconnect(reason) {
                metric_events.push(MetricEvent {
                    metric_id: ROAMING_RECONNECT_DURATION_METRIC_ID,
                    event_codes: vec![],
                    payload: MetricEventPayload::IntegerValue(reconnect_duration.into_micros()),
                });
            }
        } else {
            // The other disconnect sources are AP and MLME, which are all considered unexpected.
            metric_events.push(MetricEvent {
                metric_id: NON_ROAMING_RECONNECT_DURATION_METRIC_ID,
                event_codes: vec![],
                payload: MetricEventPayload::IntegerValue(reconnect_duration.into_micros()),
            });
        }

        self.throttled_error_logger.throttle_error(log_cobalt_1dot1_batch!(
            self.cobalt_1dot1_proxy,
            &metric_events,
            "log_reconnect_cobalt_metrics",
        ));
    }

    /// Metrics to log when device first connects to an AP, and periodically afterward
    /// (at least once a day) if the device is still connected to the AP.
    async fn log_device_connected_cobalt_metrics(
        &mut self,
        multiple_bss_candidates: bool,
        ap_state: &client::types::ApState,
        network_is_likely_hidden: bool,
    ) {
        let mut metric_events = vec![];
        metric_events.push(MetricEvent {
            metric_id: metrics::NUMBER_OF_CONNECTED_DEVICES_METRIC_ID,
            event_codes: vec![],
            payload: MetricEventPayload::Count(1),
        });

        let security_type_dim = convert::convert_security_type(&ap_state.original().protection());
        metric_events.push(MetricEvent {
            metric_id: metrics::CONNECTED_NETWORK_SECURITY_TYPE_METRIC_ID,
            event_codes: vec![security_type_dim as u32],
            payload: MetricEventPayload::Count(1),
        });

        if ap_state.original().supports_uapsd() {
            metric_events.push(MetricEvent {
                metric_id: metrics::DEVICE_CONNECTED_TO_AP_THAT_SUPPORTS_APSD_METRIC_ID,
                event_codes: vec![],
                payload: MetricEventPayload::Count(1),
            });
        }

        if let Some(rm_enabled_cap) = ap_state.original().rm_enabled_cap() {
            if rm_enabled_cap.link_measurement_enabled() {
                metric_events.push(MetricEvent {
                    metric_id:
                        metrics::DEVICE_CONNECTED_TO_AP_THAT_SUPPORTS_LINK_MEASUREMENT_METRIC_ID,
                    event_codes: vec![],
                    payload: MetricEventPayload::Count(1),
                });
            }
            if rm_enabled_cap.neighbor_report_enabled() {
                metric_events.push(MetricEvent {
                    metric_id:
                        metrics::DEVICE_CONNECTED_TO_AP_THAT_SUPPORTS_NEIGHBOR_REPORT_METRIC_ID,
                    event_codes: vec![],
                    payload: MetricEventPayload::Count(1),
                });
            }
        }

        if ap_state.original().supports_ft() {
            metric_events.push(MetricEvent {
                metric_id: metrics::DEVICE_CONNECTED_TO_AP_THAT_SUPPORTS_FT_METRIC_ID,
                event_codes: vec![],
                payload: MetricEventPayload::Count(1),
            });
        }

        if let Some(cap) = ap_state.original().ext_cap().and_then(|cap| cap.ext_caps_octet_3) {
            if cap.bss_transition() {
                metric_events.push(MetricEvent {
                    metric_id: metrics::DEVICE_CONNECTED_TO_AP_THAT_SUPPORTS_BSS_TRANSITION_MANAGEMENT_METRIC_ID,
                    event_codes: vec![],
                    payload: MetricEventPayload::Count(1),
                });
            }
        }

        let is_multi_bss_dim = convert::convert_is_multi_bss(multiple_bss_candidates);
        metric_events.push(MetricEvent {
            metric_id: metrics::DEVICE_CONNECTED_TO_AP_BREAKDOWN_BY_IS_MULTI_BSS_METRIC_ID,
            event_codes: vec![is_multi_bss_dim as u32],
            payload: MetricEventPayload::Count(1),
        });

        let oui = ap_state.original().bssid.to_oui_uppercase("");
        metric_events.push(MetricEvent {
            metric_id: metrics::DEVICE_CONNECTED_TO_AP_OUI_2_METRIC_ID,
            event_codes: vec![],
            payload: MetricEventPayload::StringValue(oui.clone()),
        });

        append_device_connected_channel_cobalt_metrics(
            &mut metric_events,
            ap_state.tracked.channel.primary,
        );

        if network_is_likely_hidden {
            metric_events.push(MetricEvent {
                metric_id: metrics::CONNECT_TO_LIKELY_HIDDEN_NETWORK_METRIC_ID,
                event_codes: vec![],
                payload: MetricEventPayload::Count(1),
            });
        }

        self.throttled_error_logger.throttle_error(log_cobalt_1dot1_batch!(
            self.cobalt_1dot1_proxy,
            &metric_events,
            "log_device_connected_cobalt_metrics",
        ));
    }

    async fn log_device_connected_channel_cobalt_metrics(&mut self, primary_channel: u8) {
        let mut metric_events = vec![];

        append_device_connected_channel_cobalt_metrics(&mut metric_events, primary_channel);

        self.throttled_error_logger.throttle_error(log_cobalt_1dot1_batch!(
            self.cobalt_1dot1_proxy,
            &metric_events,
            "log_device_connected_channel_cobalt_metrics",
        ));
    }

    async fn log_roaming_scan_metrics(&mut self) {
        self.throttled_error_logger.throttle_error(log_cobalt_1dot1!(
            self.cobalt_1dot1_proxy,
            log_occurrence,
            POLICY_PROACTIVE_ROAMING_SCAN_COUNTS_METRIC_ID,
            1,
            &[],
        ));
    }

    /// Log metrics that will be used to analyze when roaming would happen before roams are
    /// enabled. This doesn't effect general disconnect metrics, including ones that include roam
    /// event codes.
    async fn log_would_roam_connect(&mut self) {
        self.throttled_error_logger.throttle_error(log_cobalt_1dot1!(
            self.cobalt_1dot1_proxy,
            log_occurrence,
            NETWORK_ROAMING_DISCONNECT_COUNTS_METRIC_ID,
            1,
            &[],
        ));
    }

    async fn log_start_client_connections_request(
        &mut self,
        disabled_duration: zx::MonotonicDuration,
    ) {
        if disabled_duration < USER_RESTART_TIME_THRESHOLD {
            self.throttled_error_logger.throttle_error(log_cobalt_1dot1!(
                self.cobalt_1dot1_proxy,
                log_occurrence,
                metrics::CLIENT_CONNECTIONS_STOP_AND_START_METRIC_ID,
                1,
                &[],
            ));
        }
    }

    async fn log_stop_client_connections_request(
        &mut self,
        enabled_duration: zx::MonotonicDuration,
    ) {
        self.throttled_error_logger.throttle_error(log_cobalt_1dot1!(
            self.cobalt_1dot1_proxy,
            log_integer,
            metrics::CLIENT_CONNECTIONS_ENABLED_DURATION_MIGRATED_METRIC_ID,
            enabled_duration.into_micros(),
            &[],
        ));
    }

    async fn log_stop_ap_cobalt_metrics(&mut self, enabled_duration: zx::MonotonicDuration) {
        self.throttled_error_logger.throttle_error(log_cobalt_1dot1!(
            self.cobalt_1dot1_proxy,
            log_integer,
            metrics::ACCESS_POINT_ENABLED_DURATION_MIGRATED_METRIC_ID,
            enabled_duration.into_micros(),
            &[],
        ));
    }

    async fn log_signal_report_metrics(&mut self, rssi: i8) {
        // The range of the RSSI histogram is -128 to 0 with bucket size 1. The buckets are:
        //     bucket 0: reserved for underflow, although not possible with i8
        //     bucket 1: -128
        //     bucket 2: -127
        //     ...
        //     bucket 129: 0
        //     bucket 130: overflow (1 and above)
        let index = min(130, rssi as i16 + 129) as u32;
        let entry = self
            .rssi_hist
            .entry(index)
            .or_insert(fidl_fuchsia_metrics::HistogramBucket { index, count: 0 });
        entry.count += 1;
    }

    async fn log_signal_velocity_metrics(&mut self, rssi_velocity: f64) {
        // Add the count to the RSSI velocity histogram, which will be periodically logged.
        // The histogram range is -10 to 10, and index 0 is reserved for values below -10. For
        // example, RSSI velocity -10 should map to index 1 and velocity 0 should map to index 11.
        const RSSI_VELOCITY_MIN_IDX: f64 = 0.0;
        const RSSI_VELOCITY_MAX_IDX: f64 = 22.0;
        const RSSI_VELOCITY_HIST_OFFSET: f64 = 11.0;
        let index = (rssi_velocity + RSSI_VELOCITY_HIST_OFFSET)
            .clamp(RSSI_VELOCITY_MIN_IDX, RSSI_VELOCITY_MAX_IDX) as u32;
        let entry = self
            .rssi_velocity_hist
            .entry(index)
            .or_insert(fidl_fuchsia_metrics::HistogramBucket { index, count: 0 });
        entry.count += 1;
    }

    async fn log_iface_creation_result(&mut self, result: Result<(), ()>) {
        if result.is_err() {
            self.throttled_error_logger.throttle_error(log_cobalt_1dot1!(
                self.cobalt_1dot1_proxy,
                log_occurrence,
                metrics::INTERFACE_CREATION_FAILURE_METRIC_ID,
                1,
                &[]
            ))
        }

        if let Some(reason) = self.recovery_record.create_iface_failure.take() {
            match result {
                Ok(()) => self.log_post_recovery_result(reason, RecoveryOutcome::Success).await,
                Err(()) => self.log_post_recovery_result(reason, RecoveryOutcome::Failure).await,
            }
        }
    }

    async fn log_iface_destruction_result(&mut self, result: Result<(), ()>) {
        if result.is_err() {
            self.throttled_error_logger.throttle_error(log_cobalt_1dot1!(
                self.cobalt_1dot1_proxy,
                log_occurrence,
                metrics::INTERFACE_DESTRUCTION_FAILURE_METRIC_ID,
                1,
                &[]
            ))
        }

        if let Some(reason) = self.recovery_record.destroy_iface_failure.take() {
            match result {
                Ok(()) => self.log_post_recovery_result(reason, RecoveryOutcome::Success).await,
                Err(()) => self.log_post_recovery_result(reason, RecoveryOutcome::Failure).await,
            }
        }
    }

    async fn log_scan_issues(&mut self, issues: Vec<ScanIssue>) {
        // If this is a scan result following a recovery intervention, judge whether or not the
        // recovery mechanism was successful.
        if let Some(reason) = self.recovery_record.scan_failure.take() {
            let outcome = match issues.contains(&ScanIssue::ScanFailure) {
                true => RecoveryOutcome::Failure,
                false => RecoveryOutcome::Success,
            };
            self.log_post_recovery_result(reason, outcome).await;
        }
        if let Some(reason) = self.recovery_record.scan_cancellation.take() {
            let outcome = match issues.contains(&ScanIssue::AbortedScan) {
                true => RecoveryOutcome::Failure,
                false => RecoveryOutcome::Success,
            };
            self.log_post_recovery_result(reason, outcome).await;
        }
        if let Some(reason) = self.recovery_record.scan_results_empty.take() {
            let outcome = match issues.contains(&ScanIssue::EmptyScanResults) {
                true => RecoveryOutcome::Failure,
                false => RecoveryOutcome::Success,
            };
            self.log_post_recovery_result(reason, outcome).await;
        }

        // Log general occurrence metrics for any observed defects
        for issue in issues {
            self.throttled_error_logger.throttle_error(log_cobalt_1dot1!(
                self.cobalt_1dot1_proxy,
                log_occurrence,
                issue.as_metric_id(),
                1,
                &[]
            ))
        }
    }

    async fn log_connection_failure(&mut self) {
        self.throttled_error_logger.throttle_error(log_cobalt_1dot1!(
            self.cobalt_1dot1_proxy,
            log_occurrence,
            metrics::CONNECTION_FAILURES_METRIC_ID,
            1,
            &[]
        ))
    }

    async fn log_ap_start_result(&mut self, result: Result<(), ()>) {
        if result.is_err() {
            self.throttled_error_logger.throttle_error(log_cobalt_1dot1!(
                self.cobalt_1dot1_proxy,
                log_occurrence,
                metrics::AP_START_FAILURE_METRIC_ID,
                1,
                &[]
            ))
        }

        if let Some(reason) = self.recovery_record.start_ap_failure.take() {
            match result {
                Ok(()) => self.log_post_recovery_result(reason, RecoveryOutcome::Success).await,
                Err(()) => self.log_post_recovery_result(reason, RecoveryOutcome::Failure).await,
            }
        }
    }

    async fn log_scan_request_fulfillment_time(
        &mut self,
        duration: zx::MonotonicDuration,
        reason: client::scan::ScanReason,
    ) {
        let fulfillment_time_dim = {
            use metrics::ConnectivityWlanMetricDimensionScanFulfillmentTime::*;
            match duration.into_millis() {
                ..=0_000 => Unknown,
                1..=1_000 => LessThanOneSecond,
                1_001..=2_000 => LessThanTwoSeconds,
                2_001..=3_000 => LessThanThreeSeconds,
                3_001..=5_000 => LessThanFiveSeconds,
                5_001..=8_000 => LessThanEightSeconds,
                8_001..=13_000 => LessThanThirteenSeconds,
                13_001..=21_000 => LessThanTwentyOneSeconds,
                21_001..=34_000 => LessThanThirtyFourSeconds,
                34_001..=55_000 => LessThanFiftyFiveSeconds,
                55_001.. => MoreThanFiftyFiveSeconds,
            }
        };
        let reason_dim = {
            use client::scan::ScanReason;
            use metrics::ConnectivityWlanMetricDimensionScanReason::*;
            match reason {
                ScanReason::ClientRequest => ClientRequest,
                ScanReason::NetworkSelection => NetworkSelection,
                ScanReason::BssSelection => BssSelection,
                ScanReason::BssSelectionAugmentation => BssSelectionAugmentation,
                ScanReason::RoamSearch => ProactiveRoaming,
            }
        };
        self.throttled_error_logger.throttle_error(log_cobalt_1dot1!(
            self.cobalt_1dot1_proxy,
            log_occurrence,
            metrics::SUCCESSFUL_SCAN_REQUEST_FULFILLMENT_TIME_METRIC_ID,
            1,
            &[fulfillment_time_dim as u32, reason_dim as u32],
        ))
    }

    async fn log_scan_queue_statistics(
        &mut self,
        fulfilled_requests: usize,
        remaining_requests: usize,
    ) {
        let fulfilled_requests_dim = {
            use metrics::ConnectivityWlanMetricDimensionScanRequestsFulfilled::*;
            match fulfilled_requests {
                0 => Zero,
                1 => One,
                2 => Two,
                3 => Three,
                4 => Four,
                5..=9 => FiveToNine,
                10.. => TenOrMore,
            }
        };
        let remaining_requests_dim = {
            use metrics::ConnectivityWlanMetricDimensionScanRequestsRemaining::*;
            match remaining_requests {
                0 => Zero,
                1 => One,
                2 => Two,
                3 => Three,
                4 => Four,
                5..=9 => FiveToNine,
                10..=14 => TenToFourteen,
                15.. => FifteenOrMore,
            }
        };
        self.throttled_error_logger.throttle_error(log_cobalt_1dot1!(
            self.cobalt_1dot1_proxy,
            log_occurrence,
            metrics::SCAN_QUEUE_STATISTICS_AFTER_COMPLETED_SCAN_METRIC_ID,
            1,
            &[fulfilled_requests_dim as u32, remaining_requests_dim as u32],
        ))
    }

    async fn log_consecutive_counter_stats_failures(&mut self, count: i64) {
        self.throttled_error_logger.throttle_error(log_cobalt_1dot1!(
            self.cobalt_1dot1_proxy,
            log_integer,
            metrics::CONSECUTIVE_COUNTER_STATS_FAILURES_METRIC_ID,
            count,
            &[]
        ))
    }

    // Loops over the list of signal measurements, calculating what the RSSI exponentially-weighted
    // moving average and velocity were at that period in time. Then calculates an average "score"
    // over the entire list based on the EWMA RSSIs and velocities. Logs the average "score" - the
    // "score" of the baseline signal, with the time dimension event_code.
    //
    // This function is used to log 1) the delta between score at connect time and score over a
    // duration of time after, and 2) the delta between score at disconnect time and score over a
    // duration of time before.
    async fn log_average_delta_metric_by_signal(
        &mut self,
        metric_id: u32,
        signals: Vec<client::types::TimestampedSignal>,
        baseline_signal: client::types::Signal,
        time_dimension: u32,
    ) {
        if signals.is_empty() {
            warn!("Signals list for time dimension {:?} is empty.", time_dimension);
            return;
        }
        // Calculate the baseline score from the baseline signal.
        let mut ewma_signal = EwmaSignalData::new(
            baseline_signal.rssi_dbm,
            baseline_signal.snr_db,
            EWMA_SMOOTHING_FACTOR_FOR_METRICS,
        );
        let mut velocity = RssiVelocity::new(baseline_signal.rssi_dbm);
        let baseline_score =
            client::connection_selection::scoring_functions::score_current_connection_signal_data(
                ewma_signal,
                0.0,
            );
        let score_dimension = {
            // This dimension is the same for post-connect and pre-disconnect, representing the
            // first and last recorded score, respectively.
            use metrics::AverageScoreDeltaAfterConnectionByInitialScoreMetricDimensionInitialScore::*;
            match baseline_score {
                u8::MIN..=20 => _0To20,
                21..=40 => _21To40,
                41..=60 => _41To60,
                61..=80 => _61To80,
                81..=u8::MAX => _81To100,
            }
        };
        let mut sum_score = baseline_score as u32;

        // For each entry, update the ewma signal and velocity and calculate the score, using
        // saturating arithmetic to ensure overflow panics are impossible. In practice, integers for
        // this metric should not be remotely near overflowing.
        for timed_signal in &signals {
            ewma_signal.update_with_new_measurement(
                timed_signal.signal.rssi_dbm,
                timed_signal.signal.snr_db,
            );
            velocity.update(ewma_signal.ewma_rssi.get());
            let score = client::connection_selection::scoring_functions::score_current_connection_signal_data(ewma_signal, velocity.get());
            sum_score = sum_score.saturating_add(score as u32);
        }

        // Calculate the average score over the recorded time frame.
        let avg_score = sum_score / (signals.len() + 1) as u32;

        let delta = (avg_score as i64).saturating_sub(baseline_score as i64);
        self.throttled_error_logger.throttle_error(log_cobalt_1dot1!(
            &self.cobalt_1dot1_proxy,
            log_integer,
            metric_id,
            delta,
            &[score_dimension as u32, time_dimension],
        ));
    }

    // Loops over the list of signal measurements, calculating the average RSSI. Logs the average
    // RSSI - the RSSI of the baseline signal, with the time dimension event_code.
    //
    // This function is used to log 1) the delta between RSSI at connect time and RSSI over a
    // duration of time after, and 2) the delta between RSSI at disconnect time and RSSI over a
    // duration of time before.
    async fn log_average_rssi_delta_metric(
        &mut self,
        metric_id: u32,
        signals: Vec<client::types::TimestampedSignal>,
        baseline_signal: client::types::Signal,
        time_dimension: u32,
    ) {
        if signals.is_empty() {
            warn!("Signals list for time dimension {:?} is empty.", time_dimension);
            return;
        }

        let rssi_dimension = {
            use metrics::AverageRssiDeltaAfterConnectionByInitialRssiMetricDimensionRssiBucket::*;
            match baseline_signal.rssi_dbm {
                i8::MIN..=-90 => From128To90,
                -89..=-86 => From89To86,
                -85..=-83 => From85To83,
                -82..=-80 => From82To80,
                -79..=-77 => From79To77,
                -76..=-74 => From76To74,
                -73..=-71 => From73To71,
                -70..=-66 => From70To66,
                -65..=-61 => From65To61,
                -60..=-51 => From60To51,
                -50..=-35 => From50To35,
                -34..=-28 => From34To28,
                -27..=-1 => From27To1,
                0..=i8::MAX => _0,
            }
        };
        // Calculate the average RSSI over the recorded time frame.
        let mut sum_rssi = baseline_signal.rssi_dbm as i64;
        for s in &signals {
            sum_rssi = sum_rssi.saturating_add(s.signal.rssi_dbm as i64);
        }
        let average_rssi = sum_rssi / (signals.len() + 1) as i64;

        let delta = (average_rssi).saturating_sub(baseline_signal.rssi_dbm as i64);
        self.throttled_error_logger.throttle_error(log_cobalt_1dot1!(
            &self.cobalt_1dot1_proxy,
            log_integer,
            metric_id,
            delta,
            &[rssi_dimension as u32, time_dimension],
        ));
    }

    async fn log_post_connection_score_deltas_by_signal(
        &mut self,
        connect_time: fasync::MonotonicInstant,
        signal_at_connect: client::types::Signal,
        signals: HistoricalList<client::types::TimestampedSignal>,
    ) {
        // The following time ranges are 100ms longer than the corresponding duration dimensions.
        // Scores should be logged every 1 second, but the extra time provides a buffer reports are
        // not perfectly periodic.
        use metrics::AverageScoreDeltaAfterConnectionByInitialScoreMetricDimensionTimeSinceConnect as DurationDimension;

        self.log_average_delta_metric_by_signal(
            metrics::AVERAGE_SCORE_DELTA_AFTER_CONNECTION_BY_INITIAL_SCORE_METRIC_ID,
            signals
                .get_between(connect_time, connect_time + zx::MonotonicDuration::from_millis(1100)),
            signal_at_connect,
            DurationDimension::OneSecond as u32,
        )
        .await;

        self.log_average_delta_metric_by_signal(
            metrics::AVERAGE_SCORE_DELTA_AFTER_CONNECTION_BY_INITIAL_SCORE_METRIC_ID,
            signals
                .get_between(connect_time, connect_time + zx::MonotonicDuration::from_millis(5100)),
            signal_at_connect,
            DurationDimension::FiveSeconds as u32,
        )
        .await;

        self.log_average_delta_metric_by_signal(
            metrics::AVERAGE_SCORE_DELTA_AFTER_CONNECTION_BY_INITIAL_SCORE_METRIC_ID,
            signals.get_between(
                connect_time,
                connect_time + zx::MonotonicDuration::from_millis(10100),
            ),
            signal_at_connect,
            DurationDimension::TenSeconds as u32,
        )
        .await;

        self.log_average_delta_metric_by_signal(
            metrics::AVERAGE_SCORE_DELTA_AFTER_CONNECTION_BY_INITIAL_SCORE_METRIC_ID,
            signals.get_between(
                connect_time,
                connect_time + zx::MonotonicDuration::from_millis(30100),
            ),
            signal_at_connect,
            DurationDimension::ThirtySeconds as u32,
        )
        .await;
    }

    async fn log_pre_disconnect_score_deltas_by_signal(
        &mut self,
        connect_duration: zx::MonotonicDuration,
        mut signals: HistoricalList<client::types::TimestampedSignal>,
    ) {
        // The following time ranges are 100ms longer than the corresponding duration dimensions.
        // Scores should be logged every 1 second, but the extra time provides a buffer reports are
        // not perfectly periodic.
        use metrics::AverageScoreDeltaBeforeDisconnectByFinalScoreMetricDimensionTimeUntilDisconnect as DurationDimension;
        if connect_duration >= AVERAGE_SCORE_DELTA_MINIMUM_DURATION {
            // Get the last recorded score before the disconnect occurs.
            if let Some(client::types::TimestampedSignal {
                signal: final_signal,
                time: final_signal_time,
            }) = signals.0.pop_back()
            {
                self.log_average_delta_metric_by_signal(
                    metrics::AVERAGE_SCORE_DELTA_BEFORE_DISCONNECT_BY_FINAL_SCORE_METRIC_ID,
                    signals
                        .get_recent(final_signal_time - zx::MonotonicDuration::from_millis(1100)),
                    final_signal,
                    DurationDimension::OneSecond as u32,
                )
                .await;
                self.log_average_delta_metric_by_signal(
                    metrics::AVERAGE_SCORE_DELTA_BEFORE_DISCONNECT_BY_FINAL_SCORE_METRIC_ID,
                    signals
                        .get_recent(final_signal_time - zx::MonotonicDuration::from_millis(5100)),
                    final_signal,
                    DurationDimension::FiveSeconds as u32,
                )
                .await;
                self.log_average_delta_metric_by_signal(
                    metrics::AVERAGE_SCORE_DELTA_BEFORE_DISCONNECT_BY_FINAL_SCORE_METRIC_ID,
                    signals
                        .get_recent(final_signal_time - zx::MonotonicDuration::from_millis(10100)),
                    final_signal,
                    DurationDimension::TenSeconds as u32,
                )
                .await;
                self.log_average_delta_metric_by_signal(
                    metrics::AVERAGE_SCORE_DELTA_BEFORE_DISCONNECT_BY_FINAL_SCORE_METRIC_ID,
                    signals
                        .get_recent(final_signal_time - zx::MonotonicDuration::from_millis(30100)),
                    final_signal,
                    DurationDimension::ThirtySeconds as u32,
                )
                .await;
            } else {
                warn!("Past signals list is unexpectedly empty");
            }
        }
    }

    async fn log_post_connection_rssi_deltas(
        &mut self,
        connect_time: fasync::MonotonicInstant,
        signal_at_connect: client::types::Signal,
        signals: HistoricalList<client::types::TimestampedSignal>,
    ) {
        // The following time ranges are 100ms longer than the corresponding duration dimensions.
        // RSSI should be logged every 1 second, but the extra time provides a buffer reports are
        // not perfectly periodic.
        use metrics::AverageRssiDeltaAfterConnectionByInitialRssiMetricDimensionTimeSinceConnect as DurationDimension;

        self.log_average_rssi_delta_metric(
            metrics::AVERAGE_RSSI_DELTA_AFTER_CONNECTION_BY_INITIAL_RSSI_METRIC_ID,
            signals
                .get_between(connect_time, connect_time + zx::MonotonicDuration::from_millis(1100)),
            signal_at_connect,
            DurationDimension::OneSecond as u32,
        )
        .await;

        self.log_average_rssi_delta_metric(
            metrics::AVERAGE_RSSI_DELTA_AFTER_CONNECTION_BY_INITIAL_RSSI_METRIC_ID,
            signals
                .get_between(connect_time, connect_time + zx::MonotonicDuration::from_millis(5100)),
            signal_at_connect,
            DurationDimension::FiveSeconds as u32,
        )
        .await;

        self.log_average_rssi_delta_metric(
            metrics::AVERAGE_RSSI_DELTA_AFTER_CONNECTION_BY_INITIAL_RSSI_METRIC_ID,
            signals.get_between(
                connect_time,
                connect_time + zx::MonotonicDuration::from_millis(10100),
            ),
            signal_at_connect,
            DurationDimension::TenSeconds as u32,
        )
        .await;

        self.log_average_rssi_delta_metric(
            metrics::AVERAGE_RSSI_DELTA_AFTER_CONNECTION_BY_INITIAL_RSSI_METRIC_ID,
            signals.get_between(
                connect_time,
                connect_time + zx::MonotonicDuration::from_millis(30100),
            ),
            signal_at_connect,
            DurationDimension::ThirtySeconds as u32,
        )
        .await;
    }

    async fn log_pre_disconnect_rssi_deltas(
        &mut self,
        connect_duration: zx::MonotonicDuration,
        mut signals: HistoricalList<client::types::TimestampedSignal>,
    ) {
        // The following time ranges are 100ms longer than the corresponding duration dimensions.
        // RSSI should be logged every 1 second, but the extra time provides a buffer reports are
        // not perfectly periodic.
        use metrics::AverageRssiDeltaAfterConnectionByInitialRssiMetricDimensionTimeSinceConnect as DurationDimension;

        if connect_duration >= AVERAGE_SCORE_DELTA_MINIMUM_DURATION {
            // Get the last recorded score before the disconnect occurs.
            if let Some(client::types::TimestampedSignal {
                signal: final_signal,
                time: final_signal_time,
            }) = signals.0.pop_back()
            {
                self.log_average_rssi_delta_metric(
                    metrics::AVERAGE_RSSI_DELTA_BEFORE_DISCONNECT_BY_FINAL_RSSI_METRIC_ID,
                    signals.get_between(
                        final_signal_time - zx::MonotonicDuration::from_millis(1100),
                        final_signal_time,
                    ),
                    final_signal,
                    DurationDimension::OneSecond as u32,
                )
                .await;

                self.log_average_rssi_delta_metric(
                    metrics::AVERAGE_RSSI_DELTA_BEFORE_DISCONNECT_BY_FINAL_RSSI_METRIC_ID,
                    signals.get_between(
                        final_signal_time - zx::MonotonicDuration::from_millis(5100),
                        final_signal_time,
                    ),
                    final_signal,
                    DurationDimension::FiveSeconds as u32,
                )
                .await;

                self.log_average_rssi_delta_metric(
                    metrics::AVERAGE_RSSI_DELTA_BEFORE_DISCONNECT_BY_FINAL_RSSI_METRIC_ID,
                    signals.get_between(
                        final_signal_time - zx::MonotonicDuration::from_millis(10100),
                        final_signal_time,
                    ),
                    final_signal,
                    DurationDimension::TenSeconds as u32,
                )
                .await;

                self.log_average_rssi_delta_metric(
                    metrics::AVERAGE_RSSI_DELTA_BEFORE_DISCONNECT_BY_FINAL_RSSI_METRIC_ID,
                    signals.get_between(
                        final_signal_time - zx::MonotonicDuration::from_millis(30100),
                        final_signal_time,
                    ),
                    final_signal,
                    DurationDimension::ThirtySeconds as u32,
                )
                .await;
            }
        }
    }

    async fn log_short_duration_connection_metrics(
        &mut self,
        signals: HistoricalList<client::types::TimestampedSignal>,
        disconnect_source: fidl_sme::DisconnectSource,
        previous_connect_reason: client::types::ConnectReason,
    ) {
        self.log_connection_score_average_by_signal(
            metrics::ConnectionScoreAverageMetricDimensionDuration::ShortDuration as u32,
            signals.get_before(fasync::MonotonicInstant::now()),
        )
        .await;
        self.log_connection_rssi_average(
            metrics::ConnectionRssiAverageMetricDimensionDuration::ShortDuration as u32,
            signals.get_before(fasync::MonotonicInstant::now()),
        )
        .await;
        // Logs user requested connection during short duration connection, which indicates that we
        // did not successfully select the user's preferred connection.
        match disconnect_source {
            fidl_sme::DisconnectSource::User(
                fidl_sme::UserDisconnectReason::FidlConnectRequest,
            )
            | fidl_sme::DisconnectSource::User(fidl_sme::UserDisconnectReason::NetworkUnsaved) => {
                let metric_events = vec![
                    MetricEvent {
                        metric_id: metrics::POLICY_FIDL_CONNECTION_ATTEMPTS_DURING_SHORT_CONNECTION_METRIC_ID,
                        event_codes: vec![],
                        payload: MetricEventPayload::Count(1),
                    },
                    MetricEvent {
                        metric_id: metrics::POLICY_FIDL_CONNECTION_ATTEMPTS_DURING_SHORT_CONNECTION_DETAILED_METRIC_ID,
                        event_codes: vec![previous_connect_reason as u32],
                        payload: MetricEventPayload::Count(1),
                    }
                ];

                self.throttled_error_logger.throttle_error(log_cobalt_1dot1_batch!(
                    self.cobalt_1dot1_proxy,
                    &metric_events,
                    "log_short_duration_connection_metrics",
                ));
            }
            _ => {}
        }
    }

    async fn log_network_selection_metrics(
        &mut self,
        connection_state: &mut ConnectionState,
        network_selection_type: NetworkSelectionType,
        num_candidates: Result<usize, ()>,
        selected_count: usize,
    ) {
        let now = fasync::MonotonicInstant::now();
        let mut metric_events = vec![];
        metric_events.push(MetricEvent {
            metric_id: metrics::NETWORK_SELECTION_COUNT_METRIC_ID,
            event_codes: vec![],
            payload: MetricEventPayload::Count(1),
        });

        match num_candidates {
            Ok(n) if n > 0 => {
                // Saved neighbors are seen, so clear the "no saved neighbor" flag. Account
                // for any untracked time to the `downtime_no_saved_neighbor_duration`
                // counter.
                if let ConnectionState::Disconnected(state) = connection_state {
                    if let Some(prev) = state.latest_no_saved_neighbor_time.take() {
                        let duration = now - prev;
                        state.accounted_no_saved_neighbor_duration += duration;
                        self.queue_stat_op(StatOp::AddDowntimeNoSavedNeighborDuration(duration));
                    }
                }

                if network_selection_type == NetworkSelectionType::Undirected {
                    // Log number of selected networks if a network was not specified.
                    metric_events.push(MetricEvent {
                        metric_id: metrics::NUM_NETWORKS_SELECTED_METRIC_ID,
                        event_codes: vec![],
                        payload: MetricEventPayload::IntegerValue(selected_count as i64),
                    });
                }
            }
            Ok(0) if network_selection_type == NetworkSelectionType::Undirected => {
                // No saved neighbor is seen. If "no saved neighbor" flag isn't set, then
                // set it to the current time. Otherwise, do nothing because the telemetry
                // loop will account for untracked downtime during periodic telemetry run.
                if let ConnectionState::Disconnected(state) = connection_state {
                    if state.latest_no_saved_neighbor_time.is_none() {
                        state.latest_no_saved_neighbor_time = Some(now);
                    }
                }
            }
            _ => (),
        }

        self.throttled_error_logger.throttle_error(log_cobalt_1dot1_batch!(
            self.cobalt_1dot1_proxy,
            &metric_events,
            "log_network_selection_metrics",
        ));
    }

    #[allow(clippy::vec_init_then_push, reason = "mass allow for https://fxbug.dev/381896734")]
    async fn log_bss_selection_metrics(
        &mut self,
        reason: client::types::ConnectReason,
        mut scored_candidates: Vec<(client::types::ScannedCandidate, i16)>,
        selected_candidate: Option<(client::types::ScannedCandidate, i16)>,
    ) {
        let mut metric_events = vec![];

        // Record dimensionless BSS selection count
        metric_events.push(MetricEvent {
            metric_id: metrics::BSS_SELECTION_COUNT_METRIC_ID,
            event_codes: vec![],
            payload: MetricEventPayload::Count(1),
        });

        // Record detailed BSS selection count
        metric_events.push(MetricEvent {
            metric_id: metrics::BSS_SELECTION_COUNT_DETAILED_METRIC_ID,
            event_codes: vec![reason as u32],
            payload: MetricEventPayload::Count(1),
        });

        // Record dimensionless number of BSS candidates
        metric_events.push(MetricEvent {
            metric_id: metrics::NUM_BSS_CONSIDERED_IN_SELECTION_METRIC_ID,
            event_codes: vec![],
            payload: MetricEventPayload::IntegerValue(scored_candidates.len() as i64),
        });
        // Record detailed number of BSS candidates
        metric_events.push(MetricEvent {
            metric_id: metrics::NUM_BSS_CONSIDERED_IN_SELECTION_DETAILED_METRIC_ID,
            event_codes: vec![reason as u32],
            payload: MetricEventPayload::IntegerValue(scored_candidates.len() as i64),
        });

        if !scored_candidates.is_empty() {
            let (mut best_score_2g, mut best_score_5g) = (None, None);
            let mut unique_networks = HashSet::new();

            for (candidate, score) in &scored_candidates {
                // Record candidate's score
                metric_events.push(MetricEvent {
                    metric_id: metrics::BSS_CANDIDATE_SCORE_METRIC_ID,
                    event_codes: vec![],
                    payload: MetricEventPayload::IntegerValue(*score as i64),
                });

                let _ = unique_networks.insert(&candidate.network);

                if candidate.bss.channel.is_2ghz() {
                    best_score_2g = best_score_2g.or(Some(*score)).map(|s| max(s, *score));
                } else {
                    best_score_5g = best_score_5g.or(Some(*score)).map(|s| max(s, *score));
                }
            }

            // Record number of unique networks in bss selection. This differs from number of
            // networks selected, since some actions may bypass network selection (e.g. proactive
            // roaming)
            metric_events.push(MetricEvent {
                metric_id: metrics::NUM_NETWORKS_REPRESENTED_IN_BSS_SELECTION_METRIC_ID,
                event_codes: vec![reason as u32],
                payload: MetricEventPayload::IntegerValue(unique_networks.len() as i64),
            });

            if let Some((_, score)) = selected_candidate {
                // Record selected candidate's score
                metric_events.push(MetricEvent {
                    metric_id: metrics::SELECTED_BSS_SCORE_METRIC_ID,
                    event_codes: vec![],
                    payload: MetricEventPayload::IntegerValue(score as i64),
                });

                // Record runner-up candidate's score, iff:
                // 1. there were multiple candidates and
                // 2. selected candidate is the top scoring candidate (or tied in score)
                scored_candidates.sort_by_key(|(_, score)| Reverse(*score));
                #[expect(clippy::get_first)]
                if let (Some(first_candidate), Some(second_candidate)) =
                    (scored_candidates.get(0), scored_candidates.get(1))
                {
                    if score == first_candidate.1 {
                        let delta = first_candidate.1 - second_candidate.1;
                        metric_events.push(MetricEvent {
                            metric_id: metrics::RUNNER_UP_CANDIDATE_SCORE_DELTA_METRIC_ID,
                            event_codes: vec![],
                            payload: MetricEventPayload::IntegerValue(delta as i64),
                        });
                    }
                }
            }

            let ghz_event_code =
                if let (Some(score_2g), Some(score_5g)) = (best_score_2g, best_score_5g) {
                    // Record delta between best 5GHz and best 2.4GHz candidates
                    metric_events.push(MetricEvent {
                        metric_id: metrics::BEST_CANDIDATES_GHZ_SCORE_DELTA_METRIC_ID,
                        event_codes: vec![],
                        payload: MetricEventPayload::IntegerValue((score_5g - score_2g) as i64),
                    });
                    metrics::ConnectivityWlanMetricDimensionBands::MultiBand
                } else if best_score_2g.is_some() {
                    metrics::ConnectivityWlanMetricDimensionBands::Band2Dot4Ghz
                } else {
                    metrics::ConnectivityWlanMetricDimensionBands::Band5Ghz
                };

            metric_events.push(MetricEvent {
                metric_id: metrics::GHZ_BANDS_AVAILABLE_IN_BSS_SELECTION_METRIC_ID,
                event_codes: vec![ghz_event_code as u32],
                payload: MetricEventPayload::Count(1),
            });
        }

        self.throttled_error_logger.throttle_error(log_cobalt_1dot1_batch!(
            self.cobalt_1dot1_proxy,
            &metric_events,
            "log_bss_selection_cobalt_metrics",
        ));
    }

    async fn log_connection_score_average_by_signal(
        &mut self,
        duration_dim: u32,
        signals: Vec<client::types::TimestampedSignal>,
    ) {
        let Some(first_signal) = signals.first() else {
            warn!("Connection signals list is unexpectedly empty.");
            return;
        };
        let mut sum_scores = 0;
        let mut ewma_signal = EwmaSignalData::new(
            first_signal.signal.rssi_dbm,
            first_signal.signal.snr_db,
            EWMA_SMOOTHING_FACTOR_FOR_METRICS,
        );
        let mut velocity = RssiVelocity::new(first_signal.signal.rssi_dbm);
        for timed_signal in &signals {
            ewma_signal.update_with_new_measurement(
                timed_signal.signal.rssi_dbm,
                timed_signal.signal.snr_db,
            );
            velocity.update(ewma_signal.ewma_rssi.get());
            let score = client::connection_selection::scoring_functions::score_current_connection_signal_data(ewma_signal, velocity.get());
            sum_scores = sum_scores.saturating_add(&(score as u32));
        }
        let avg = sum_scores / (signals.len()) as u32;
        self.throttled_error_logger.throttle_error(log_cobalt_1dot1!(
            self.cobalt_1dot1_proxy,
            log_integer,
            metrics::CONNECTION_SCORE_AVERAGE_METRIC_ID,
            avg as i64,
            &[duration_dim],
        ));
    }

    async fn log_connection_rssi_average(
        &mut self,
        duration_dim: u32,
        signals: Vec<client::types::TimestampedSignal>,
    ) {
        if signals.is_empty() {
            warn!("Connection signals list is unexpectedly empty.");
            return;
        }
        let mut sum_rssi: i64 = 0;
        for s in &signals {
            sum_rssi = sum_rssi.saturating_add(s.signal.rssi_dbm as i64);
        }
        let average_rssi = sum_rssi / (signals.len()) as i64;
        self.throttled_error_logger.throttle_error(log_cobalt_1dot1!(
            self.cobalt_1dot1_proxy,
            log_integer,
            metrics::CONNECTION_RSSI_AVERAGE_METRIC_ID,
            average_rssi,
            &[duration_dim]
        ));
    }

    async fn log_recovery_occurrence(&mut self, reason: RecoveryReason) {
        self.recovery_record.record_recovery_attempt(reason);

        let dimension = match reason {
            RecoveryReason::CreateIfaceFailure(_) => {
                metrics::RecoveryOccurrenceMetricDimensionReason::InterfaceCreationFailure
            }
            RecoveryReason::DestroyIfaceFailure(_) => {
                metrics::RecoveryOccurrenceMetricDimensionReason::InterfaceDestructionFailure
            }
            RecoveryReason::Timeout(_) => metrics::RecoveryOccurrenceMetricDimensionReason::Timeout,
            RecoveryReason::ConnectFailure(_) => {
                metrics::RecoveryOccurrenceMetricDimensionReason::ClientConnectionFailure
            }
            RecoveryReason::StartApFailure(_) => {
                metrics::RecoveryOccurrenceMetricDimensionReason::ApStartFailure
            }
            RecoveryReason::ScanFailure(_) => {
                metrics::RecoveryOccurrenceMetricDimensionReason::ScanFailure
            }
            RecoveryReason::ScanCancellation(_) => {
                metrics::RecoveryOccurrenceMetricDimensionReason::ScanCancellation
            }
            RecoveryReason::ScanResultsEmpty(_) => {
                metrics::RecoveryOccurrenceMetricDimensionReason::ScanResultsEmpty
            }
        };

        self.throttled_error_logger.throttle_error(log_cobalt_1dot1!(
            self.cobalt_1dot1_proxy,
            log_occurrence,
            metrics::RECOVERY_OCCURRENCE_METRIC_ID,
            1,
            &[dimension.as_event_code()],
        ))
    }

    async fn log_post_recovery_result(&mut self, reason: RecoveryReason, outcome: RecoveryOutcome) {
        async fn log_post_recovery_metric(
            throttled_error_logger: &mut ThrottledErrorLogger,
            proxy: &mut fidl_fuchsia_metrics::MetricEventLoggerProxy,
            metric_id: u32,
            event_codes: &[u32],
        ) {
            throttled_error_logger.throttle_error(log_cobalt_1dot1!(
                proxy,
                log_occurrence,
                metric_id,
                1,
                event_codes,
            ))
        }

        if outcome == RecoveryOutcome::Success {
            self.last_successful_recovery.set(fasync::MonotonicInstant::now().into_nanos() as u64);
            let _ = self.successful_recoveries.add(1);
        }

        match reason {
            RecoveryReason::CreateIfaceFailure(_) => {
                log_post_recovery_metric(
                    &mut self.throttled_error_logger,
                    &mut self.cobalt_1dot1_proxy,
                    metrics::INTERFACE_CREATION_RECOVERY_OUTCOME_METRIC_ID,
                    &[outcome.as_event_code()],
                )
                .await;
            }
            RecoveryReason::DestroyIfaceFailure(_) => {
                log_post_recovery_metric(
                    &mut self.throttled_error_logger,
                    &mut self.cobalt_1dot1_proxy,
                    metrics::INTERFACE_DESTRUCTION_RECOVERY_OUTCOME_METRIC_ID,
                    &[outcome.as_event_code()],
                )
                .await;
            }
            RecoveryReason::Timeout(mechanism) => {
                log_post_recovery_metric(
                    &mut self.throttled_error_logger,
                    &mut self.cobalt_1dot1_proxy,
                    metrics::TIMEOUT_RECOVERY_OUTCOME_METRIC_ID,
                    &[outcome.as_event_code(), mechanism.as_event_code()],
                )
                .await;
            }
            RecoveryReason::ConnectFailure(mechanism) => {
                log_post_recovery_metric(
                    &mut self.throttled_error_logger,
                    &mut self.cobalt_1dot1_proxy,
                    metrics::CONNECT_FAILURE_RECOVERY_OUTCOME_METRIC_ID,
                    &[outcome.as_event_code(), mechanism.as_event_code()],
                )
                .await;
            }
            RecoveryReason::StartApFailure(mechanism) => {
                log_post_recovery_metric(
                    &mut self.throttled_error_logger,
                    &mut self.cobalt_1dot1_proxy,
                    metrics::START_ACCESS_POINT_RECOVERY_OUTCOME_METRIC_ID,
                    &[outcome.as_event_code(), mechanism.as_event_code()],
                )
                .await;
            }
            RecoveryReason::ScanFailure(mechanism) => {
                log_post_recovery_metric(
                    &mut self.throttled_error_logger,
                    &mut self.cobalt_1dot1_proxy,
                    metrics::SCAN_FAILURE_RECOVERY_OUTCOME_METRIC_ID,
                    &[outcome.as_event_code(), mechanism.as_event_code()],
                )
                .await;
            }
            RecoveryReason::ScanCancellation(mechanism) => {
                log_post_recovery_metric(
                    &mut self.throttled_error_logger,
                    &mut self.cobalt_1dot1_proxy,
                    metrics::SCAN_CANCELLATION_RECOVERY_OUTCOME_METRIC_ID,
                    &[outcome.as_event_code(), mechanism.as_event_code()],
                )
                .await;
            }
            RecoveryReason::ScanResultsEmpty(mechanism) => {
                log_post_recovery_metric(
                    &mut self.throttled_error_logger,
                    &mut self.cobalt_1dot1_proxy,
                    metrics::EMPTY_SCAN_RESULTS_RECOVERY_OUTCOME_METRIC_ID,
                    &[outcome.as_event_code(), mechanism.as_event_code()],
                )
                .await;
            }
        }
    }

    async fn log_sme_timeout(&mut self, source: TimeoutSource) {
        let dimension = match source {
            TimeoutSource::Scan => {
                metrics::SmeOperationTimeoutMetricDimensionStalledOperation::Scan_
            }
            TimeoutSource::Connect => {
                metrics::SmeOperationTimeoutMetricDimensionStalledOperation::Connect_
            }
            TimeoutSource::Disconnect => {
                metrics::SmeOperationTimeoutMetricDimensionStalledOperation::Disconnect_
            }
            TimeoutSource::ClientStatus => {
                metrics::SmeOperationTimeoutMetricDimensionStalledOperation::ClientStatus_
            }
            TimeoutSource::WmmStatus => {
                metrics::SmeOperationTimeoutMetricDimensionStalledOperation::WmmStatus_
            }
            TimeoutSource::ApStart => {
                metrics::SmeOperationTimeoutMetricDimensionStalledOperation::ApStart_
            }
            TimeoutSource::ApStop => {
                metrics::SmeOperationTimeoutMetricDimensionStalledOperation::ApStop_
            }
            TimeoutSource::ApStatus => {
                metrics::SmeOperationTimeoutMetricDimensionStalledOperation::ApStatus_
            }
            TimeoutSource::GetCounterStats => {
                metrics::SmeOperationTimeoutMetricDimensionStalledOperation::GetCounterStats_
            }
            TimeoutSource::GetHistogramStats => {
                metrics::SmeOperationTimeoutMetricDimensionStalledOperation::GetHistogramStats_
            }
        };

        self.throttled_error_logger.throttle_error(log_cobalt_1dot1!(
            self.cobalt_1dot1_proxy,
            log_occurrence,
            metrics::SME_OPERATION_TIMEOUT_METRIC_ID,
            1,
            &[dimension.as_event_code()],
        ))
    }
}

// If metrics cannot be reported for extended periods of time, logging new metrics will fail and
// the error messages tend to clutter up the logs.  This container limits the rate at which such
// potentially noisy logs are reported.  Duplicate error messages are aggregated periodically
// reported.
struct ThrottledErrorLogger {
    time_of_last_log: fasync::MonotonicInstant,
    suppressed_errors: HashMap<String, usize>,
    minutes_between_reports: i64,
}

impl ThrottledErrorLogger {
    fn new(minutes_between_reports: i64) -> Self {
        Self {
            time_of_last_log: fasync::MonotonicInstant::from_nanos(0),
            suppressed_errors: HashMap::new(),
            minutes_between_reports,
        }
    }

    fn throttle_error(&mut self, result: Result<(), Error>) {
        if let Err(e) = result {
            // If sufficient time has passed since the last time a cobalt error was logged, report
            // the number of cobalt errors that have been encountered.
            let curr_time = fasync::MonotonicInstant::now();
            let time_since_last_log = curr_time - self.time_of_last_log;
            if time_since_last_log.into_minutes() > self.minutes_between_reports {
                warn!("{}", e.to_string());
                if !self.suppressed_errors.is_empty() {
                    for (log, count) in self.suppressed_errors.iter() {
                        warn!("Suppressed {} instances: {}", count, log);
                    }
                    self.suppressed_errors.clear();
                }
                self.time_of_last_log = curr_time;
            } else {
                // If not enough time has passed since the last warning log, just update the record
                // of cobalt errors so that they can be reported later.
                let error_string = e.to_string();
                let count = self.suppressed_errors.entry(error_string).or_default();
                *count += 1;
            }
        }
    }
}

fn append_device_connected_channel_cobalt_metrics(
    metric_events: &mut Vec<MetricEvent>,
    primary_channel: u8,
) {
    metric_events.push(MetricEvent {
        metric_id: metrics::DEVICE_CONNECTED_TO_AP_BREAKDOWN_BY_PRIMARY_CHANNEL_METRIC_ID,
        event_codes: vec![primary_channel as u32],
        payload: MetricEventPayload::Count(1),
    });

    let channel_band_dim = convert::convert_channel_band(primary_channel);
    metric_events.push(MetricEvent {
        metric_id: metrics::DEVICE_CONNECTED_TO_AP_BREAKDOWN_BY_CHANNEL_BAND_METRIC_ID,
        event_codes: vec![channel_band_dim as u32],
        payload: MetricEventPayload::Count(1),
    });
}

// Return whether the user disconnect reason indicates that the disconnect was for a roam.
// This enumerates all enum options in order to cause a compile error if the enum changes.
fn is_roam_disconnect(reason: fidl_sme::UserDisconnectReason) -> bool {
    use fidl_sme::UserDisconnectReason;
    match reason {
        UserDisconnectReason::ProactiveNetworkSwitch => true,
        UserDisconnectReason::Unknown
        | UserDisconnectReason::FailedToConnect
        | UserDisconnectReason::FidlStopClientConnectionsRequest
        | UserDisconnectReason::DisconnectDetectedFromSme
        | UserDisconnectReason::RegulatoryRegionChange
        | UserDisconnectReason::Startup
        | UserDisconnectReason::NetworkUnsaved
        | UserDisconnectReason::NetworkConfigUpdated
        | UserDisconnectReason::FidlConnectRequest
        | UserDisconnectReason::WlanstackUnitTesting
        | UserDisconnectReason::WlanSmeUnitTesting
        | UserDisconnectReason::WlanServiceUtilTesting
        | UserDisconnectReason::WlanDevTool
        | UserDisconnectReason::Recovery => false,
    }
}

#[allow(clippy::enum_variant_names, reason = "mass allow for https://fxbug.dev/381896734")]
enum StatOp {
    AddTotalDuration(zx::MonotonicDuration),
    AddConnectedDuration(zx::MonotonicDuration),
    AddDowntimeDuration(zx::MonotonicDuration),
    // Downtime with no saved network in vicinity
    AddDowntimeNoSavedNeighborDuration(zx::MonotonicDuration),
    AddConnectAttemptsCount,
    AddConnectSuccessfulCount,
    AddDisconnectCount(fidl_sme::DisconnectSource),
    AddTxHighPacketDropDuration(zx::MonotonicDuration),
    AddRxHighPacketDropDuration(zx::MonotonicDuration),
    AddTxVeryHighPacketDropDuration(zx::MonotonicDuration),
    AddRxVeryHighPacketDropDuration(zx::MonotonicDuration),
    AddNoRxDuration(zx::MonotonicDuration),
    AddRxPacketCounters { rx_unicast_total: u64, rx_unicast_drop: u64 },
    AddTxPacketCounters { tx_total: u64, tx_drop: u64 },
}

#[derive(Clone, Default, PartialEq)]
struct StatCounters {
    total_duration: zx::MonotonicDuration,
    connected_duration: zx::MonotonicDuration,
    downtime_duration: zx::MonotonicDuration,
    downtime_no_saved_neighbor_duration: zx::MonotonicDuration,
    connect_attempts_count: u64,
    connect_successful_count: u64,
    disconnect_count: u64,
    roaming_disconnect_count: u64,
    non_roaming_disconnect_count: u64,
    tx_high_packet_drop_duration: zx::MonotonicDuration,
    rx_high_packet_drop_duration: zx::MonotonicDuration,
    tx_very_high_packet_drop_duration: zx::MonotonicDuration,
    rx_very_high_packet_drop_duration: zx::MonotonicDuration,
    no_rx_duration: zx::MonotonicDuration,
}

impl StatCounters {
    fn adjusted_downtime(&self) -> zx::MonotonicDuration {
        max(
            zx::MonotonicDuration::from_seconds(0),
            self.downtime_duration - self.downtime_no_saved_neighbor_duration,
        )
    }

    fn connection_success_rate(&self) -> f64 {
        self.connect_successful_count as f64 / self.connect_attempts_count as f64
    }
}

// `Add` implementation is required to implement `SaturatingAdd` down below.
impl Add for StatCounters {
    type Output = Self;

    fn add(self, other: Self) -> Self {
        Self {
            total_duration: self.total_duration + other.total_duration,
            connected_duration: self.connected_duration + other.connected_duration,
            downtime_duration: self.downtime_duration + other.downtime_duration,
            downtime_no_saved_neighbor_duration: self.downtime_no_saved_neighbor_duration
                + other.downtime_no_saved_neighbor_duration,
            connect_attempts_count: self.connect_attempts_count + other.connect_attempts_count,
            connect_successful_count: self.connect_successful_count
                + other.connect_successful_count,
            disconnect_count: self.disconnect_count + other.disconnect_count,
            roaming_disconnect_count: self.roaming_disconnect_count
                + other.roaming_disconnect_count,
            non_roaming_disconnect_count: self.non_roaming_disconnect_count
                + other.non_roaming_disconnect_count,
            tx_high_packet_drop_duration: self.tx_high_packet_drop_duration
                + other.tx_high_packet_drop_duration,
            rx_high_packet_drop_duration: self.rx_high_packet_drop_duration
                + other.rx_high_packet_drop_duration,
            tx_very_high_packet_drop_duration: self.tx_very_high_packet_drop_duration
                + other.tx_very_high_packet_drop_duration,
            rx_very_high_packet_drop_duration: self.rx_very_high_packet_drop_duration
                + other.rx_very_high_packet_drop_duration,
            no_rx_duration: self.no_rx_duration + other.no_rx_duration,
        }
    }
}

impl SaturatingAdd for StatCounters {
    fn saturating_add(&self, v: &Self) -> Self {
        Self {
            total_duration: zx::MonotonicDuration::from_nanos(
                self.total_duration.into_nanos().saturating_add(v.total_duration.into_nanos()),
            ),
            connected_duration: zx::MonotonicDuration::from_nanos(
                self.connected_duration
                    .into_nanos()
                    .saturating_add(v.connected_duration.into_nanos()),
            ),
            downtime_duration: zx::MonotonicDuration::from_nanos(
                self.downtime_duration
                    .into_nanos()
                    .saturating_add(v.downtime_duration.into_nanos()),
            ),
            downtime_no_saved_neighbor_duration: zx::MonotonicDuration::from_nanos(
                self.downtime_no_saved_neighbor_duration
                    .into_nanos()
                    .saturating_add(v.downtime_no_saved_neighbor_duration.into_nanos()),
            ),
            connect_attempts_count: self
                .connect_attempts_count
                .saturating_add(v.connect_attempts_count),
            connect_successful_count: self
                .connect_successful_count
                .saturating_add(v.connect_successful_count),
            disconnect_count: self.disconnect_count.saturating_add(v.disconnect_count),
            roaming_disconnect_count: self
                .roaming_disconnect_count
                .saturating_add(v.roaming_disconnect_count),
            non_roaming_disconnect_count: self
                .non_roaming_disconnect_count
                .saturating_add(v.non_roaming_disconnect_count),
            tx_high_packet_drop_duration: zx::MonotonicDuration::from_nanos(
                self.tx_high_packet_drop_duration
                    .into_nanos()
                    .saturating_add(v.tx_high_packet_drop_duration.into_nanos()),
            ),
            rx_high_packet_drop_duration: zx::MonotonicDuration::from_nanos(
                self.rx_high_packet_drop_duration
                    .into_nanos()
                    .saturating_add(v.rx_high_packet_drop_duration.into_nanos()),
            ),
            tx_very_high_packet_drop_duration: zx::MonotonicDuration::from_nanos(
                self.tx_very_high_packet_drop_duration
                    .into_nanos()
                    .saturating_add(v.tx_very_high_packet_drop_duration.into_nanos()),
            ),
            rx_very_high_packet_drop_duration: zx::MonotonicDuration::from_nanos(
                self.rx_very_high_packet_drop_duration
                    .into_nanos()
                    .saturating_add(v.rx_very_high_packet_drop_duration.into_nanos()),
            ),
            no_rx_duration: zx::MonotonicDuration::from_nanos(
                self.no_rx_duration.into_nanos().saturating_add(v.no_rx_duration.into_nanos()),
            ),
        }
    }
}

#[derive(Debug)]
struct DailyDetailedStats {
    connect_attempts_status: HashMap<fidl_ieee80211::StatusCode, u64>,
    connect_per_is_multi_bss: HashMap<
        metrics::SuccessfulConnectBreakdownByIsMultiBssMetricDimensionIsMultiBss,
        ConnectAttemptsCounter,
    >,
    connect_per_security_type: HashMap<
        metrics::SuccessfulConnectBreakdownBySecurityTypeMetricDimensionSecurityType,
        ConnectAttemptsCounter,
    >,
    connect_per_primary_channel: HashMap<u8, ConnectAttemptsCounter>,
    connect_per_channel_band: HashMap<
        metrics::SuccessfulConnectBreakdownByChannelBandMetricDimensionChannelBand,
        ConnectAttemptsCounter,
    >,
    connect_per_rssi_bucket:
        HashMap<metrics::ConnectivityWlanMetricDimensionRssiBucket, ConnectAttemptsCounter>,
    connect_per_snr_bucket:
        HashMap<metrics::ConnectivityWlanMetricDimensionSnrBucket, ConnectAttemptsCounter>,
}

impl DailyDetailedStats {
    pub fn new() -> Self {
        Self {
            connect_attempts_status: HashMap::new(),
            connect_per_is_multi_bss: HashMap::new(),
            connect_per_security_type: HashMap::new(),
            connect_per_primary_channel: HashMap::new(),
            connect_per_channel_band: HashMap::new(),
            connect_per_rssi_bucket: HashMap::new(),
            connect_per_snr_bucket: HashMap::new(),
        }
    }
}

#[derive(Debug, Default, Copy, Clone, PartialEq)]
struct ConnectAttemptsCounter {
    success: u64,
    total: u64,
}

impl ConnectAttemptsCounter {
    fn increment(&mut self, code: fidl_ieee80211::StatusCode) {
        self.total += 1;
        if code == fidl_ieee80211::StatusCode::Success {
            self.success += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::testing::{
        create_inspect_persistence_channel, generate_random_bss, generate_random_channel,
        generate_random_scanned_candidate,
    };
    use diagnostics_assertions::{
        AnyBoolProperty, AnyNumericProperty, AnyStringProperty, NonZeroUintProperty,
    };
    use fidl::endpoints::create_proxy_and_stream;
    use fidl_fuchsia_metrics::{MetricEvent, MetricEventLoggerRequest, MetricEventPayload};
    use fuchsia_inspect::reader;
    use futures::stream::FusedStream;
    use futures::task::Poll;
    use futures::TryStreamExt;
    use ieee80211_testutils::{BSSID_REGEX, SSID_REGEX};
    use rand::Rng;
    use regex::Regex;
    use std::collections::VecDeque;
    use std::pin::{pin, Pin};
    use test_case::test_case;
    use test_util::assert_gt;
    use wlan_common::bss::BssDescription;
    use wlan_common::channel::{Cbw, Channel};
    use wlan_common::ie::IeType;
    use wlan_common::test_utils::fake_stas::IesOverrides;
    use wlan_common::{assert_variant, random_bss_description};

    const STEP_INCREMENT: zx::MonotonicDuration = zx::MonotonicDuration::from_seconds(1);
    const IFACE_ID: u16 = 1;

    // Macro rule for testing Inspect data tree. When we query for Inspect data, the LazyNode
    // will make a stats query req that we need to respond to in order to unblock the test.
    macro_rules! assert_data_tree_with_respond_blocking_req {
        ($test_helper:expr, $test_fut:expr, $($rest:tt)+) => {{
            use {
                fuchsia_inspect::reader, diagnostics_assertions::assert_data_tree,
            };

            let inspector = $test_helper.inspector.clone();
            let read_fut = reader::read(&inspector);
            let mut read_fut = pin!(read_fut);
            loop {
                match $test_helper.exec.run_until_stalled(&mut read_fut) {
                    Poll::Pending => {
                        // Run telemetry test future so it can respond to QueryStatus request,
                        // while clearing out any potentially blocking Cobalt events
                        $test_helper.drain_cobalt_events(&mut $test_fut);
                        // Manually respond to iface stats request
                        if let Some(telemetry_svc_stream) = &mut $test_helper.telemetry_svc_stream {
                            if !telemetry_svc_stream.is_terminated() {
                                respond_iface_histogram_stats_req(
                                    &mut $test_helper.exec,
                                    telemetry_svc_stream,
                                );
                            }
                        }

                    }
                    Poll::Ready(result) => {
                        let hierarchy = result.expect("failed to get hierarchy");
                        assert_data_tree!(hierarchy, $($rest)+);
                        break
                    }
                }
            }
        }}
    }

    #[fuchsia::test]
    fn test_detect_driver_unresponsive_signal_ind() {
        let (mut test_helper, mut test_fut) = setup_test();
        test_helper.send_connected_event(random_bss_description!(Wpa2));

        assert_data_tree_with_respond_blocking_req!(test_helper, test_fut, root: contains {
            stats: contains {
                is_driver_unresponsive: false,
            }
        });

        test_helper.advance_by(
            UNRESPONSIVE_FLAG_MIN_DURATION - TELEMETRY_QUERY_INTERVAL,
            test_fut.as_mut(),
        );
        assert_data_tree_with_respond_blocking_req!(test_helper, test_fut, root: contains {
            stats: contains {
                is_driver_unresponsive: false,
            }
        });

        // Send a signal, which resets timing information for determining driver unresponsiveness
        let ind = fidl_internal::SignalReportIndication { rssi_dbm: -40, snr_db: 30 };
        test_helper.telemetry_sender.send(TelemetryEvent::OnSignalReport { ind });

        test_helper.advance_by(UNRESPONSIVE_FLAG_MIN_DURATION, test_fut.as_mut());
        assert_data_tree_with_respond_blocking_req!(test_helper, test_fut, root: contains {
            stats: contains {
                is_driver_unresponsive: false,
            }
        });

        // On the next telemetry interval, driver is recognized as unresponsive
        test_helper.advance_by(TELEMETRY_QUERY_INTERVAL, test_fut.as_mut());
        assert_data_tree_with_respond_blocking_req!(test_helper, test_fut, root: contains {
            stats: contains {
                is_driver_unresponsive: true,
            }
        });
    }

    #[fuchsia::test]
    fn test_histogram_stats_timeout() {
        let mut exec = fasync::TestExecutor::new();

        let inspector = Inspector::default();
        let external_node = inspector.root().create_child("external");
        let external_inspect_node = ExternalInspectNode::new(external_node);

        let (telemetry_sender, mut telemetry_receiver) =
            mpsc::channel::<TelemetryEvent>(TELEMETRY_EVENT_BUFFER_SIZE);
        let (defect_sender, mut defect_receiver) = mpsc::channel(100);

        // Setup the lazy child node.  When the inspect node is read, it will snapshot current
        // interface state.
        inspect_record_external_data(
            &external_inspect_node,
            TelemetrySender::new(telemetry_sender),
            defect_sender,
        );

        // Initiate a read of the inspect node.  This will run the future that was constructed.
        let fut = reader::read(&inspector);
        let mut fut = pin!(fut);
        assert_variant!(exec.run_until_stalled(&mut fut), Poll::Pending);

        // First, inspect will query the current state from the telemetry event loop.  In order to
        // get to the point of querying histograms, we need to reply that we are in the connected
        // state.
        let (telemetry_proxy, _telemetry_server) =
            fidl::endpoints::create_proxy::<fidl_sme::TelemetryMarker>();
        assert_variant!(
            telemetry_receiver.try_next(),
            Ok(Some(TelemetryEvent::QueryStatus {sender})) => {
                sender.send(QueryStatusResult {
                    connection_state: ConnectionStateInfo::Connected {
                        iface_id: 0,
                        ap_state: random_bss_description!(Wpa2).into(),
                        telemetry_proxy: Some(telemetry_proxy)
                    }
                }).expect("failed to send query status result")
            }
        );
        assert_variant!(exec.run_until_stalled(&mut fut), Poll::Pending);

        // The future should block on getting the histogram stats until the timer expires.
        assert!(exec.wake_next_timer().is_some());
        assert_variant!(exec.run_until_stalled(&mut fut), Poll::Ready(_));

        // We should get a timeout defect.
        assert_variant!(
            defect_receiver.try_next(),
            Ok(Some(Defect::Iface(IfaceFailure::Timeout {
                iface_id: 0,
                source: TimeoutSource::GetHistogramStats,
            })))
        );
    }

    #[fuchsia::test]
    fn test_telemetry_timeout() {
        let mut exec = fasync::TestExecutor::new();

        // Boilerplate for creating a Telemetry struct
        let (sender, _receiver) = mpsc::channel::<TelemetryEvent>(TELEMETRY_EVENT_BUFFER_SIZE);
        let (monitor_svc_proxy, _monitor_svc_stream) =
            create_proxy_and_stream::<fidl_fuchsia_wlan_device_service::DeviceMonitorMarker>();
        let (cobalt_1dot1_proxy, _cobalt_1dot1_stream) =
            create_proxy_and_stream::<fidl_fuchsia_metrics::MetricEventLoggerMarker>();
        let inspector = Inspector::default();
        let inspect_node = inspector.root().create_child("stats");
        let external_inspect_node = inspector.root().create_child("external");
        let (persistence_req_sender, _persistence_stream) = create_inspect_persistence_channel();
        let (defect_sender, mut defect_receiver) = mpsc::channel(100);

        let mut telemetry = Telemetry::new(
            TelemetrySender::new(sender),
            monitor_svc_proxy,
            cobalt_1dot1_proxy.clone(),
            inspect_node,
            external_inspect_node,
            persistence_req_sender,
            defect_sender,
        );

        // Setup the Telemetry struct so that it thinks that it is connected.
        let (telemetry_proxy, _telemetry_server) =
            fidl::endpoints::create_proxy::<fidl_sme::TelemetryMarker>();
        telemetry.connection_state = ConnectionState::Connected(ConnectedState {
            iface_id: 0,
            ap_state: random_bss_description!(Wpa2).into(),
            telemetry_proxy: Some(telemetry_proxy),

            // The rest of the fields don't matter for this test case.
            new_connect_start_time: None,
            prev_connection_counters: None,
            multiple_bss_candidates: false,
            network_is_likely_hidden: false,
            last_signal_report: fasync::MonotonicInstant::now(),
            num_consecutive_get_counter_stats_failures: InspectableU64::new(
                0,
                &telemetry.inspect_node,
                "num_consecutive_get_counter_stats_failures",
            ),
            is_driver_unresponsive: InspectableBool::new(
                false,
                &telemetry.inspect_node,
                "is_driver_unresponsive",
            ),
        });

        // Call handle_periodic_telemetry.
        let fut = telemetry.handle_periodic_telemetry();
        let mut fut = pin!(fut);
        assert_variant!(exec.run_until_stalled(&mut fut), Poll::Pending);

        // Have the executor trigger the timeout.
        assert!(exec.wake_next_timer().is_some());
        assert_variant!(exec.run_until_stalled(&mut fut), Poll::Pending);

        // Verify that the timeout has been received.
        assert_variant!(
            defect_receiver.try_next(),
            Ok(Some(Defect::Iface(IfaceFailure::Timeout {
                iface_id: 0,
                source: TimeoutSource::GetCounterStats,
            })))
        );
    }

    #[fuchsia::test]
    fn test_logging_num_consecutive_get_counter_stats_failures() {
        let (mut test_helper, mut test_fut) = setup_test();
        test_helper.set_counter_stats_resp(Box::new(|| Err(zx::sys::ZX_ERR_TIMED_OUT)));
        test_helper.send_connected_event(random_bss_description!(Wpa2));

        assert_data_tree_with_respond_blocking_req!(test_helper, test_fut, root: contains {
            stats: contains {
                num_consecutive_get_counter_stats_failures: 0u64,
            }
        });

        test_helper.advance_by(TELEMETRY_QUERY_INTERVAL * 20i64, test_fut.as_mut());
        assert_data_tree_with_respond_blocking_req!(test_helper, test_fut, root: contains {
            stats: contains {
                num_consecutive_get_counter_stats_failures: 20u64,
            }
        });

        // Expect that Cobalt has been notified.
        test_helper.drain_cobalt_events(&mut test_fut);
        let logged_metrics =
            test_helper.get_logged_metrics(metrics::CONSECUTIVE_COUNTER_STATS_FAILURES_METRIC_ID);
        assert_eq!(logged_metrics.len(), 20);

        assert_eq!(
            logged_metrics[19].payload,
            fidl_fuchsia_metrics::MetricEventPayload::IntegerValue(20)
        );
    }

    #[fuchsia::test]
    fn test_log_connect_event_correct_shape() {
        let (mut test_helper, mut test_fut) = setup_test();
        test_helper.send_connected_event(random_bss_description!(Wpa2));

        assert_eq!(test_helper.advance_test_fut(&mut test_fut), Poll::Pending);

        assert_data_tree_with_respond_blocking_req!(test_helper, test_fut, root: contains {
            stats: contains {
                connect_events: {
                    "0": {
                        "@time": AnyNumericProperty,
                        multiple_bss_candidates: AnyBoolProperty,
                        network: {
                            bssid: &*BSSID_REGEX,
                            ssid: &*SSID_REGEX,
                            rssi_dbm: AnyNumericProperty,
                            snr_db: AnyNumericProperty,
                        }
                    }
                }
            }
        });
    }

    #[fuchsia::test]
    fn test_log_connection_status_correct_shape() {
        let (mut test_helper, mut test_fut) = setup_test();
        test_helper.send_connected_event(random_bss_description!(Wpa2));

        assert_eq!(test_helper.advance_test_fut(&mut test_fut), Poll::Pending);

        assert_data_tree_with_respond_blocking_req!(test_helper, test_fut, root: contains {
            stats: contains {
                connection_status: contains {
                    status_string: AnyStringProperty,
                    connected_network: contains {
                        rssi_dbm: AnyNumericProperty,
                        snr_db: AnyNumericProperty,
                        bssid: &*BSSID_REGEX,
                        ssid: &*SSID_REGEX,
                        protection: AnyStringProperty,
                        channel: AnyStringProperty,
                        is_wmm_assoc: AnyBoolProperty,
                    }
                }
            }
        });
    }

    #[allow(clippy::regex_creation_in_loops, reason = "mass allow for https://fxbug.dev/381896734")]
    #[fuchsia::test]
    fn test_log_disconnect_event_correct_shape() {
        let (mut test_helper, mut test_fut) = setup_test();

        test_helper.telemetry_sender.send(TelemetryEvent::Disconnected {
            track_subsequent_downtime: false,
            info: fake_disconnect_info(),
        });
        assert_eq!(test_helper.advance_test_fut(&mut test_fut), Poll::Pending);

        assert_data_tree_with_respond_blocking_req!(test_helper, test_fut, root: contains {
            external: contains {
                stats: contains {
                    disconnect_events: {
                        "0": {
                            "@time": AnyNumericProperty,
                            flattened_reason_code: AnyNumericProperty,
                            locally_initiated: AnyBoolProperty,
                            network: {
                                channel: {
                                    primary: AnyNumericProperty,
                                }
                            }
                        }
                    }
                }
            },
            stats: contains {
                disconnect_events: {
                    "0": {
                        "@time": AnyNumericProperty,
                        connected_duration: AnyNumericProperty,
                        disconnect_source: Regex::new("^source: [^,]+, reason: [^,]+(?:, mlme_event_name: [^,]+)?$").unwrap(),
                        network: contains {
                            rssi_dbm: AnyNumericProperty,
                            snr_db: AnyNumericProperty,
                            bssid: &*BSSID_REGEX,
                            ssid: &*SSID_REGEX,
                            protection: AnyStringProperty,
                            channel: AnyStringProperty,
                            is_wmm_assoc: AnyBoolProperty,
                        }
                    }
                }
            }
        });
    }

    #[fuchsia::test]
    fn test_stat_cycles() {
        let (mut test_helper, mut test_fut) = setup_test();
        test_helper.send_connected_event(random_bss_description!(Wpa2));
        assert_eq!(test_helper.advance_test_fut(&mut test_fut), Poll::Pending);

        test_helper.advance_by(
            zx::MonotonicDuration::from_hours(24) - TELEMETRY_QUERY_INTERVAL,
            test_fut.as_mut(),
        );
        assert_data_tree_with_respond_blocking_req!(test_helper, test_fut, root: contains {
            stats: contains {
                "1d_counters": contains {
                    total_duration: (zx::MonotonicDuration::from_hours(24) - TELEMETRY_QUERY_INTERVAL).into_nanos(),
                    connected_duration: (zx::MonotonicDuration::from_hours(24) - TELEMETRY_QUERY_INTERVAL).into_nanos(),
                },
                "7d_counters": contains {
                    total_duration: (zx::MonotonicDuration::from_hours(24) - TELEMETRY_QUERY_INTERVAL).into_nanos(),
                    connected_duration: (zx::MonotonicDuration::from_hours(24) - TELEMETRY_QUERY_INTERVAL).into_nanos(),
                },
            }
        });

        test_helper.advance_to_next_telemetry_checkpoint(test_fut.as_mut());
        assert_data_tree_with_respond_blocking_req!(test_helper, test_fut, root: contains {
            stats: contains {
                "1d_counters": contains {
                    // The first hour window is now discarded, so it only shows 23 hours
                    // of total and connected duration.
                    total_duration: zx::MonotonicDuration::from_hours(23).into_nanos(),
                    connected_duration: zx::MonotonicDuration::from_hours(23).into_nanos(),
                },
                "7d_counters": contains {
                    total_duration: zx::MonotonicDuration::from_hours(24).into_nanos(),
                    connected_duration: zx::MonotonicDuration::from_hours(24).into_nanos(),
                },
            }
        });

        test_helper.advance_by(zx::MonotonicDuration::from_hours(2), test_fut.as_mut());
        assert_data_tree_with_respond_blocking_req!(test_helper, test_fut, root: contains {
            stats: contains {
                "1d_counters": contains {
                    total_duration: zx::MonotonicDuration::from_hours(23).into_nanos(),
                    connected_duration: zx::MonotonicDuration::from_hours(23).into_nanos(),
                },
                "7d_counters": contains {
                    total_duration: zx::MonotonicDuration::from_hours(26).into_nanos(),
                    connected_duration: zx::MonotonicDuration::from_hours(26).into_nanos(),
                },
            }
        });

        // Disconnect now
        let info = fake_disconnect_info();
        test_helper
            .telemetry_sender
            .send(TelemetryEvent::Disconnected { track_subsequent_downtime: false, info });
        assert_eq!(test_helper.advance_test_fut(&mut test_fut), Poll::Pending);

        test_helper.advance_by(zx::MonotonicDuration::from_hours(8), test_fut.as_mut());
        assert_data_tree_with_respond_blocking_req!(test_helper, test_fut, root: contains {
            stats: contains {
                "1d_counters": contains {
                    total_duration: zx::MonotonicDuration::from_hours(23).into_nanos(),
                    // Now the 1d connected counter should decrease
                    connected_duration: zx::MonotonicDuration::from_hours(15).into_nanos(),
                },
                "7d_counters": contains {
                    total_duration: zx::MonotonicDuration::from_hours(34).into_nanos(),
                    connected_duration: zx::MonotonicDuration::from_hours(26).into_nanos(),
                },
            }
        });

        // The 7d counters do not decrease before the 7th day
        test_helper.advance_by(zx::MonotonicDuration::from_hours(14), test_fut.as_mut());
        test_helper.advance_by(
            zx::MonotonicDuration::from_hours(5 * 24) - TELEMETRY_QUERY_INTERVAL,
            test_fut.as_mut(),
        );
        assert_data_tree_with_respond_blocking_req!(test_helper, test_fut, root: contains {
            stats: contains {
                "1d_counters": contains {
                    total_duration: (zx::MonotonicDuration::from_hours(24) - TELEMETRY_QUERY_INTERVAL).into_nanos(),
                    connected_duration: 0i64,
                },
                "7d_counters": contains {
                    total_duration: (zx::MonotonicDuration::from_hours(7 * 24) - TELEMETRY_QUERY_INTERVAL).into_nanos(),
                    connected_duration: zx::MonotonicDuration::from_hours(26).into_nanos(),
                },
            }
        });

        // On the 7th day, the first window is removed (24 hours of duration is deducted)
        test_helper.advance_to_next_telemetry_checkpoint(test_fut.as_mut());
        assert_data_tree_with_respond_blocking_req!(test_helper, test_fut, root: contains {
            stats: contains {
                "1d_counters": contains {
                    total_duration: zx::MonotonicDuration::from_hours(23).into_nanos(),
                    connected_duration: 0i64,
                },
                "7d_counters": contains {
                    total_duration: zx::MonotonicDuration::from_hours(6 * 24).into_nanos(),
                    connected_duration: zx::MonotonicDuration::from_hours(2).into_nanos(),
                },
            }
        });
    }

    #[fuchsia::test]
    fn test_daily_detailed_stat_cycles() {
        let (mut test_helper, mut test_fut) = setup_test();
        for _ in 0..10 {
            test_helper.send_connected_event(random_bss_description!(Wpa2));
        }
        test_helper.advance_by(zx::MonotonicDuration::from_hours(24), test_fut.as_mut());

        // On 1st day, 10 successful connects, so verify metric is logged with count of 10.
        let status_codes = test_helper.get_logged_metrics(
            metrics::CONNECT_ATTEMPT_ON_NORMAL_DEVICE_BREAKDOWN_BY_STATUS_CODE_METRIC_ID,
        );
        assert_eq!(status_codes.len(), 1);
        assert_eq!(status_codes[0].event_codes, vec![fidl_ieee80211::StatusCode::Success as u32]);
        assert_eq!(status_codes[0].payload, MetricEventPayload::Count(10));

        test_helper.cobalt_events.clear();

        test_helper.send_connected_event(random_bss_description!(Wpa2));
        test_helper.advance_by(zx::MonotonicDuration::from_hours(24), test_fut.as_mut());

        // On 2nd day, 1 successful connect, so verify metric is logged with count of 1.
        let status_codes = test_helper.get_logged_metrics(
            metrics::CONNECT_ATTEMPT_ON_NORMAL_DEVICE_BREAKDOWN_BY_STATUS_CODE_METRIC_ID,
        );
        assert_eq!(status_codes.len(), 1);
        assert_eq!(status_codes[0].event_codes, vec![fidl_ieee80211::StatusCode::Success as u32]);
        assert_eq!(status_codes[0].payload, MetricEventPayload::Count(1));
    }

    #[fuchsia::test]
    fn test_total_duration_counters() {
        let (mut test_helper, mut test_fut) = setup_test();

        test_helper.advance_by(zx::MonotonicDuration::from_minutes(30), test_fut.as_mut());
        assert_data_tree_with_respond_blocking_req!(test_helper, test_fut, root: contains {
            stats: contains {
                "1d_counters": contains {
                    total_duration: zx::MonotonicDuration::from_minutes(30).into_nanos(),
                },
                "7d_counters": contains {
                    total_duration: zx::MonotonicDuration::from_minutes(30).into_nanos(),
                },
            }
        });

        test_helper.advance_by(zx::MonotonicDuration::from_minutes(30), test_fut.as_mut());
        assert_data_tree_with_respond_blocking_req!(test_helper, test_fut, root: contains {
            stats: contains {
                "1d_counters": contains {
                    total_duration: zx::MonotonicDuration::from_hours(1).into_nanos(),
                },
                "7d_counters": contains {
                    total_duration: zx::MonotonicDuration::from_hours(1).into_nanos(),
                },
            }
        });
    }

    #[fuchsia::test]
    fn test_total_duration_time_series() {
        let (mut test_helper, mut test_fut) = setup_test();

        test_helper.advance_by(zx::MonotonicDuration::from_seconds(25), test_fut.as_mut());
        let time_series = test_helper.get_time_series(&mut test_fut);
        let total_duration_sec: Vec<_> =
            time_series.lock().total_duration_sec.minutely_iter().copied().collect();
        assert_eq!(total_duration_sec, vec![15]);

        test_helper.advance_to_next_telemetry_checkpoint(test_fut.as_mut());
        let total_duration_sec: Vec<_> =
            time_series.lock().total_duration_sec.minutely_iter().copied().collect();
        assert_eq!(total_duration_sec, vec![30]);
    }

    #[fuchsia::test]
    fn test_counters_when_idle() {
        let (mut test_helper, mut test_fut) = setup_test();

        test_helper.advance_by(zx::MonotonicDuration::from_minutes(30), test_fut.as_mut());
        assert_data_tree_with_respond_blocking_req!(test_helper, test_fut, root: contains {
            stats: contains {
                "1d_counters": contains {
                    connected_duration: 0i64,
                    downtime_duration: 0i64,
                    downtime_no_saved_neighbor_duration: 0i64,
                },
                "7d_counters": contains {
                    connected_duration: 0i64,
                    downtime_duration: 0i64,
                    downtime_no_saved_neighbor_duration: 0i64,
                },
            }
        });

        test_helper.advance_by(zx::MonotonicDuration::from_minutes(30), test_fut.as_mut());
        assert_data_tree_with_respond_blocking_req!(test_helper, test_fut, root: contains {
            stats: contains {
                "1d_counters": contains {
                    connected_duration: 0i64,
                    downtime_duration: 0i64,
                    downtime_no_saved_neighbor_duration: 0i64,
                },
                "7d_counters": contains {
                    connected_duration: 0i64,
                    downtime_duration: 0i64,
                    downtime_no_saved_neighbor_duration: 0i64,
                },
            }
        });
    }

    #[fuchsia::test]
    fn test_connected_counters_increase_when_connected() {
        let (mut test_helper, mut test_fut) = setup_test();
        test_helper.send_connected_event(random_bss_description!(Wpa2));
        assert_eq!(test_helper.advance_test_fut(&mut test_fut), Poll::Pending);

        test_helper.advance_by(zx::MonotonicDuration::from_minutes(30), test_fut.as_mut());
        assert_data_tree_with_respond_blocking_req!(test_helper, test_fut, root: contains {
            stats: contains {
                "1d_counters": contains {
                    connected_duration: zx::MonotonicDuration::from_minutes(30).into_nanos(),
                    downtime_duration: 0i64,
                    downtime_no_saved_neighbor_duration: 0i64,
                },
                "7d_counters": contains {
                    connected_duration: zx::MonotonicDuration::from_minutes(30).into_nanos(),
                    downtime_duration: 0i64,
                    downtime_no_saved_neighbor_duration: 0i64,
                },
            }
        });

        test_helper.advance_by(zx::MonotonicDuration::from_minutes(30), test_fut.as_mut());
        assert_data_tree_with_respond_blocking_req!(test_helper, test_fut, root: contains {
            stats: contains {
                "1d_counters": contains {
                    connected_duration: zx::MonotonicDuration::from_hours(1).into_nanos(),
                    downtime_duration: 0i64,
                    downtime_no_saved_neighbor_duration: 0i64,
                },
                "7d_counters": contains {
                    connected_duration: zx::MonotonicDuration::from_hours(1).into_nanos(),
                    downtime_duration: 0i64,
                    downtime_no_saved_neighbor_duration: 0i64,
                },
            }
        });
    }

    #[fuchsia::test]
    fn test_downtime_counter() {
        let (mut test_helper, mut test_fut) = setup_test();

        // Disconnect but not track downtime. Downtime counter should not increase.
        let info = fake_disconnect_info();
        test_helper
            .telemetry_sender
            .send(TelemetryEvent::Disconnected { track_subsequent_downtime: false, info });
        assert_eq!(test_helper.advance_test_fut(&mut test_fut), Poll::Pending);

        test_helper.advance_by(zx::MonotonicDuration::from_minutes(10), test_fut.as_mut());

        assert_data_tree_with_respond_blocking_req!(test_helper, test_fut, root: contains {
            stats: contains {
                "1d_counters": contains {
                    connected_duration: 0i64,
                    downtime_duration: 0i64,
                    downtime_no_saved_neighbor_duration: 0i64,
                },
                "7d_counters": contains {
                    connected_duration: 0i64,
                    downtime_duration: 0i64,
                    downtime_no_saved_neighbor_duration: 0i64,
                },
            }
        });

        // Disconnect and track downtime. Downtime counter should now increase
        let info = fake_disconnect_info();
        test_helper
            .telemetry_sender
            .send(TelemetryEvent::Disconnected { track_subsequent_downtime: true, info });
        assert_eq!(test_helper.advance_test_fut(&mut test_fut), Poll::Pending);

        test_helper.advance_by(zx::MonotonicDuration::from_minutes(15), test_fut.as_mut());

        assert_data_tree_with_respond_blocking_req!(test_helper, test_fut, root: contains {
            stats: contains {
                "1d_counters": contains {
                    connected_duration: 0i64,
                    downtime_duration: zx::MonotonicDuration::from_minutes(15).into_nanos(),
                    downtime_no_saved_neighbor_duration: 0i64,
                },
                "7d_counters": contains {
                    connected_duration: 0i64,
                    downtime_duration: zx::MonotonicDuration::from_minutes(15).into_nanos(),
                    downtime_no_saved_neighbor_duration: 0i64,
                },
            }
        });
    }

    #[fuchsia::test]
    fn test_counters_connect_then_disconnect() {
        let (mut test_helper, mut test_fut) = setup_test();
        test_helper.send_connected_event(random_bss_description!(Wpa2));
        assert_eq!(test_helper.advance_test_fut(&mut test_fut), Poll::Pending);

        test_helper.advance_by(zx::MonotonicDuration::from_seconds(5), test_fut.as_mut());

        // Disconnect but not track downtime. Downtime counter should not increase.
        let info = fake_disconnect_info();
        test_helper
            .telemetry_sender
            .send(TelemetryEvent::Disconnected { track_subsequent_downtime: true, info });
        assert_eq!(test_helper.advance_test_fut(&mut test_fut), Poll::Pending);

        // The 5 seconds connected duration is not accounted for yet.
        assert_data_tree_with_respond_blocking_req!(test_helper, test_fut, root: contains {
            stats: contains {
                "1d_counters": contains {
                    connected_duration: 0i64,
                    downtime_duration: 0i64,
                    downtime_no_saved_neighbor_duration: 0i64,
                },
                "7d_counters": contains {
                    connected_duration: 0i64,
                    downtime_duration: 0i64,
                    downtime_no_saved_neighbor_duration: 0i64,
                },
            }
        });

        // At next telemetry checkpoint, `test_fut` updates the connected and downtime durations.
        let downtime_start = fasync::MonotonicInstant::now();
        test_helper.advance_to_next_telemetry_checkpoint(test_fut.as_mut());
        assert_data_tree_with_respond_blocking_req!(test_helper, test_fut, root: contains {
            stats: contains {
                "1d_counters": contains {
                    connected_duration: zx::MonotonicDuration::from_seconds(5).into_nanos(),
                    downtime_duration: (fasync::MonotonicInstant::now() - downtime_start).into_nanos(),
                    downtime_no_saved_neighbor_duration: 0i64,
                },
                "7d_counters": contains {
                    connected_duration: zx::MonotonicDuration::from_seconds(5).into_nanos(),
                    downtime_duration: (fasync::MonotonicInstant::now() - downtime_start).into_nanos(),
                    downtime_no_saved_neighbor_duration: 0i64,
                },
            }
        });
    }

    #[fuchsia::test]
    fn test_downtime_no_saved_neighbor_duration_counter() {
        let (mut test_helper, mut test_fut) = setup_test();
        test_helper.send_connected_event(random_bss_description!(Wpa2));
        test_helper.drain_cobalt_events(&mut test_fut);

        // Disconnect and track downtime.
        let info = fake_disconnect_info();
        test_helper
            .telemetry_sender
            .send(TelemetryEvent::Disconnected { track_subsequent_downtime: true, info });
        assert_eq!(test_helper.advance_test_fut(&mut test_fut), Poll::Pending);

        test_helper.advance_by(zx::MonotonicDuration::from_seconds(5), test_fut.as_mut());
        // Indicate that there's no saved neighbor in vicinity
        test_helper.telemetry_sender.send(TelemetryEvent::NetworkSelectionDecision {
            network_selection_type: NetworkSelectionType::Undirected,
            num_candidates: Ok(0),
            selected_count: 0,
        });
        assert_eq!(test_helper.advance_test_fut(&mut test_fut), Poll::Pending);

        test_helper.advance_to_next_telemetry_checkpoint(test_fut.as_mut());
        assert_data_tree_with_respond_blocking_req!(test_helper, test_fut, root: contains {
            stats: contains {
                "1d_counters": contains {
                    connected_duration: 0i64,
                    downtime_duration: TELEMETRY_QUERY_INTERVAL.into_nanos(),
                    downtime_no_saved_neighbor_duration: (TELEMETRY_QUERY_INTERVAL - zx::MonotonicDuration::from_seconds(5)).into_nanos(),
                },
                "7d_counters": contains {
                    connected_duration: 0i64,
                    downtime_duration: TELEMETRY_QUERY_INTERVAL.into_nanos(),
                    downtime_no_saved_neighbor_duration: (TELEMETRY_QUERY_INTERVAL - zx::MonotonicDuration::from_seconds(5)).into_nanos(),
                },
            }
        });

        test_helper.advance_to_next_telemetry_checkpoint(test_fut.as_mut());
        assert_data_tree_with_respond_blocking_req!(test_helper, test_fut, root: contains {
            stats: contains {
                "1d_counters": contains {
                    connected_duration: 0i64,
                    downtime_duration: (TELEMETRY_QUERY_INTERVAL * 2).into_nanos(),
                    downtime_no_saved_neighbor_duration: (TELEMETRY_QUERY_INTERVAL*2 - zx::MonotonicDuration::from_seconds(5)).into_nanos(),
                },
                "7d_counters": contains {
                    connected_duration: 0i64,
                    downtime_duration: (TELEMETRY_QUERY_INTERVAL * 2).into_nanos(),
                    downtime_no_saved_neighbor_duration: (TELEMETRY_QUERY_INTERVAL*2 - zx::MonotonicDuration::from_seconds(5)).into_nanos(),
                },
            }
        });

        test_helper.advance_by(zx::MonotonicDuration::from_seconds(5), test_fut.as_mut());
        // Indicate that saved neighbor has been found
        test_helper.telemetry_sender.send(TelemetryEvent::NetworkSelectionDecision {
            network_selection_type: NetworkSelectionType::Undirected,
            num_candidates: Ok(1),
            selected_count: 0,
        });
        assert_eq!(test_helper.advance_test_fut(&mut test_fut), Poll::Pending);

        // `downtime_no_saved_neighbor_duration` counter is not updated right away.
        assert_data_tree_with_respond_blocking_req!(test_helper, test_fut, root: contains {
            stats: contains {
                "1d_counters": contains {
                    connected_duration: 0i64,
                    downtime_duration: (TELEMETRY_QUERY_INTERVAL * 2).into_nanos(),
                    downtime_no_saved_neighbor_duration: (TELEMETRY_QUERY_INTERVAL*2 - zx::MonotonicDuration::from_seconds(5)).into_nanos(),
                },
                "7d_counters": contains {
                    connected_duration: 0i64,
                    downtime_duration: (TELEMETRY_QUERY_INTERVAL * 2).into_nanos(),
                    downtime_no_saved_neighbor_duration: (TELEMETRY_QUERY_INTERVAL*2 - zx::MonotonicDuration::from_seconds(5)).into_nanos(),
                },
            }
        });

        // At the next checkpoint, both downtime counters are updated together.
        test_helper.advance_to_next_telemetry_checkpoint(test_fut.as_mut());
        assert_data_tree_with_respond_blocking_req!(test_helper, test_fut, root: contains {
            stats: contains {
                "1d_counters": contains {
                    connected_duration: 0i64,
                    downtime_duration: (TELEMETRY_QUERY_INTERVAL * 3).into_nanos(),
                    downtime_no_saved_neighbor_duration: (TELEMETRY_QUERY_INTERVAL * 2).into_nanos(),
                },
                "7d_counters": contains {
                    connected_duration: 0i64,
                    downtime_duration: (TELEMETRY_QUERY_INTERVAL * 3).into_nanos(),
                    downtime_no_saved_neighbor_duration: (TELEMETRY_QUERY_INTERVAL * 2).into_nanos(),
                },
            }
        });

        // Disconnect but don't track downtime
        let info = fake_disconnect_info();
        test_helper
            .telemetry_sender
            .send(TelemetryEvent::Disconnected { track_subsequent_downtime: false, info });

        // Indicate that there's no saved neighbor in vicinity
        test_helper.telemetry_sender.send(TelemetryEvent::NetworkSelectionDecision {
            network_selection_type: NetworkSelectionType::Undirected,
            num_candidates: Ok(0),
            selected_count: 0,
        });
        assert_eq!(test_helper.advance_test_fut(&mut test_fut), Poll::Pending);
        test_helper.advance_to_next_telemetry_checkpoint(test_fut.as_mut());

        // However, this time neither of the downtime counters should be incremented
        assert_data_tree_with_respond_blocking_req!(test_helper, test_fut, root: contains {
            stats: contains {
                "1d_counters": contains {
                    connected_duration: 0i64,
                    downtime_duration: (TELEMETRY_QUERY_INTERVAL * 3).into_nanos(),
                    downtime_no_saved_neighbor_duration: (TELEMETRY_QUERY_INTERVAL * 2).into_nanos(),
                },
                "7d_counters": contains {
                    connected_duration: 0i64,
                    downtime_duration: (TELEMETRY_QUERY_INTERVAL * 3).into_nanos(),
                    downtime_no_saved_neighbor_duration: (TELEMETRY_QUERY_INTERVAL * 2).into_nanos(),
                },
            }
        });
    }

    #[fuchsia::test]
    fn test_log_connect_attempt_counters() {
        let (mut test_helper, mut test_fut) = setup_test();

        // Send 10 failed connect results, then 1 successful.
        for i in 0..10 {
            let event = TelemetryEvent::ConnectResult {
                iface_id: IFACE_ID,
                policy_connect_reason: Some(
                    client::types::ConnectReason::RetryAfterFailedConnectAttempt,
                ),
                result: fake_connect_result(fidl_ieee80211::StatusCode::RefusedReasonUnspecified),
                multiple_bss_candidates: true,
                ap_state: random_bss_description!(Wpa1).into(),
                network_is_likely_hidden: false,
            };
            test_helper.telemetry_sender.send(event);

            // Verify that the connection failure has been logged.
            test_helper.drain_cobalt_events(&mut test_fut);
            let logged_metrics =
                test_helper.get_logged_metrics(metrics::CONNECTION_FAILURES_METRIC_ID);
            assert_eq!(logged_metrics.len(), i + 1);
        }
        test_helper.send_connected_event(random_bss_description!(Wpa2));
        test_helper.drain_cobalt_events(&mut test_fut);

        assert_data_tree_with_respond_blocking_req!(test_helper, test_fut, root: contains {
            stats: contains {
                "1d_counters": contains {
                    connect_attempts_count: 11u64,
                    connect_successful_count: 1u64,
                },
                "7d_counters": contains {
                    connect_attempts_count: 11u64,
                    connect_successful_count: 1u64,
                },
            }
        });
    }

    #[fuchsia::test]
    fn test_log_connect_attempt_time_series() {
        let (mut test_helper, mut test_fut) = setup_test();

        // Send 10 failed connect results, then 1 successful.
        for i in 0..10 {
            let event = TelemetryEvent::ConnectResult {
                iface_id: IFACE_ID,
                policy_connect_reason: Some(
                    client::types::ConnectReason::RetryAfterFailedConnectAttempt,
                ),
                result: fake_connect_result(fidl_ieee80211::StatusCode::RefusedReasonUnspecified),
                multiple_bss_candidates: true,
                ap_state: random_bss_description!(Wpa1).into(),
                network_is_likely_hidden: false,
            };
            test_helper.telemetry_sender.send(event);

            // Verify that the connection failure has been logged.
            test_helper.drain_cobalt_events(&mut test_fut);
            let logged_metrics =
                test_helper.get_logged_metrics(metrics::CONNECTION_FAILURES_METRIC_ID);
            assert_eq!(logged_metrics.len(), i + 1);
        }
        test_helper.send_connected_event(random_bss_description!(Wpa2));
        test_helper.drain_cobalt_events(&mut test_fut);
        let time_series = test_helper.get_time_series(&mut test_fut);
        let connect_attempt_count: Vec<_> =
            time_series.lock().connect_attempt_count.minutely_iter().copied().collect();
        let connect_successful_count: Vec<_> =
            time_series.lock().connect_successful_count.minutely_iter().copied().collect();
        assert_eq!(connect_attempt_count, vec![11]);
        assert_eq!(connect_successful_count, vec![1]);
    }

    #[fuchsia::test]
    fn test_disconnect_count_counter() {
        let (mut test_helper, mut test_fut) = setup_test();
        test_helper.send_connected_event(random_bss_description!(Wpa2));
        assert_eq!(test_helper.advance_test_fut(&mut test_fut), Poll::Pending);

        assert_data_tree_with_respond_blocking_req!(test_helper, test_fut, root: contains {
            stats: contains {
                "1d_counters": contains {
                    disconnect_count: 0u64,
                    roaming_disconnect_count: 0u64,
                },
                "7d_counters": contains {
                    disconnect_count: 0u64,
                    roaming_disconnect_count: 0u64,
                },
            }
        });

        // Send a non-roaming disconnect
        let info = DisconnectInfo {
            disconnect_source: fidl_sme::DisconnectSource::Ap(fidl_sme::DisconnectCause {
                reason_code: fidl_ieee80211::ReasonCode::StaLeaving,
                mlme_event_name: fidl_sme::DisconnectMlmeEventName::DisassociateIndication,
            }),
            ..fake_disconnect_info()
        };
        test_helper
            .telemetry_sender
            .send(TelemetryEvent::Disconnected { track_subsequent_downtime: true, info });
        test_helper.drain_cobalt_events(&mut test_fut);

        assert_data_tree_with_respond_blocking_req!(test_helper, test_fut, root: contains {
            stats: contains {
                "1d_counters": contains {
                    disconnect_count: 1u64,
                    roaming_disconnect_count: 0u64,
                    non_roaming_non_user_disconnect_count: 1u64,
                },
                "7d_counters": contains {
                    disconnect_count: 1u64,
                    roaming_disconnect_count: 0u64,
                    non_roaming_non_user_disconnect_count: 1u64,
                },
            }
        });

        let info = DisconnectInfo {
            disconnect_source: fidl_sme::DisconnectSource::User(
                fidl_sme::UserDisconnectReason::Startup,
            ),
            ..fake_disconnect_info()
        };
        test_helper
            .telemetry_sender
            .send(TelemetryEvent::Disconnected { track_subsequent_downtime: false, info });
        test_helper.drain_cobalt_events(&mut test_fut);

        assert_data_tree_with_respond_blocking_req!(test_helper, test_fut, root: contains {
            stats: contains {
                "1d_counters": contains {
                    disconnect_count: 2u64,
                    roaming_disconnect_count: 0u64,
                    non_roaming_non_user_disconnect_count: 1u64,
                },
                "7d_counters": contains {
                    disconnect_count: 2u64,
                    roaming_disconnect_count: 0u64,
                    non_roaming_non_user_disconnect_count: 1u64,
                },
            }
        });

        // Send a roaming disconnect
        let info = DisconnectInfo {
            disconnect_source: fidl_sme::DisconnectSource::User(
                fidl_sme::UserDisconnectReason::ProactiveNetworkSwitch,
            ),
            ..fake_disconnect_info()
        };
        test_helper
            .telemetry_sender
            .send(TelemetryEvent::Disconnected { track_subsequent_downtime: false, info });
        test_helper.drain_cobalt_events(&mut test_fut);

        assert_data_tree_with_respond_blocking_req!(test_helper, test_fut, root: contains {
            stats: contains {
                "1d_counters": contains {
                    disconnect_count: 3u64,
                    roaming_disconnect_count: 1u64,
                    non_roaming_non_user_disconnect_count: 1u64,
                },
                "7d_counters": contains {
                    disconnect_count: 3u64,
                    roaming_disconnect_count: 1u64,
                    non_roaming_non_user_disconnect_count: 1u64,
                },
            }
        });
    }

    #[fuchsia::test]
    fn test_disconnect_count_time_series() {
        let (mut test_helper, mut test_fut) = setup_test();
        test_helper.send_connected_event(random_bss_description!(Wpa2));
        assert_eq!(test_helper.advance_test_fut(&mut test_fut), Poll::Pending);

        let time_series = test_helper.get_time_series(&mut test_fut);
        let disconnect_count: Vec<_> =
            time_series.lock().disconnect_count.minutely_iter().copied().collect();
        assert_eq!(disconnect_count, vec![0]);

        let info = DisconnectInfo {
            disconnect_source: fidl_sme::DisconnectSource::Ap(fidl_sme::DisconnectCause {
                reason_code: fidl_ieee80211::ReasonCode::StaLeaving,
                mlme_event_name: fidl_sme::DisconnectMlmeEventName::DisassociateIndication,
            }),
            ..fake_disconnect_info()
        };
        test_helper
            .telemetry_sender
            .send(TelemetryEvent::Disconnected { track_subsequent_downtime: true, info });
        test_helper.drain_cobalt_events(&mut test_fut);

        let time_series = test_helper.get_time_series(&mut test_fut);
        let disconnect_count: Vec<_> =
            time_series.lock().disconnect_count.minutely_iter().copied().collect();
        assert_eq!(disconnect_count, vec![1]);
    }

    #[fuchsia::test]
    fn test_connected_duration_time_series() {
        let (mut test_helper, mut test_fut) = setup_test();
        test_helper.send_connected_event(random_bss_description!(Wpa2));
        assert_eq!(test_helper.advance_test_fut(&mut test_fut), Poll::Pending);
        test_helper.advance_by(zx::MonotonicDuration::from_seconds(90), test_fut.as_mut());

        let time_series = test_helper.get_time_series(&mut test_fut);
        let connected_duration_sec: Vec<_> =
            time_series.lock().connected_duration_sec.minutely_iter().copied().collect();
        assert_eq!(connected_duration_sec, vec![45, 45]);
    }

    #[fuchsia::test]
    fn test_rx_tx_counters_no_issue() {
        let (mut test_helper, mut test_fut) = setup_test();
        test_helper.send_connected_event(random_bss_description!(Wpa2));
        assert_eq!(test_helper.advance_test_fut(&mut test_fut), Poll::Pending);

        test_helper.advance_by(zx::MonotonicDuration::from_hours(1), test_fut.as_mut());
        assert_data_tree_with_respond_blocking_req!(test_helper, test_fut, root: contains {
            stats: contains {
                get_iface_stats_fail_count: 0u64,
                "1d_counters": contains {
                    tx_high_packet_drop_duration: 0i64,
                    rx_high_packet_drop_duration: 0i64,
                    tx_very_high_packet_drop_duration: 0i64,
                    rx_very_high_packet_drop_duration: 0i64,
                    no_rx_duration: 0i64,
                },
                "7d_counters": contains {
                    tx_high_packet_drop_duration: 0i64,
                    rx_high_packet_drop_duration: 0i64,
                    tx_very_high_packet_drop_duration: 0i64,
                    rx_very_high_packet_drop_duration: 0i64,
                    no_rx_duration: 0i64,
                },
            }
        });
    }

    #[fuchsia::test]
    fn test_tx_high_packet_drop_duration_counters() {
        let (mut test_helper, mut test_fut) = setup_test();
        test_helper.set_counter_stats_resp(Box::new(|| {
            let seed = fasync::MonotonicInstant::now().into_nanos() as u64;
            Ok(fidl_fuchsia_wlan_stats::IfaceCounterStats {
                connection_counters: Some(fidl_fuchsia_wlan_stats::ConnectionCounters {
                    tx_total: Some(10 * seed),
                    tx_drop: Some(3 * seed),
                    ..fake_connection_counters(seed)
                }),
                ..Default::default()
            })
        }));

        test_helper.send_connected_event(random_bss_description!(Wpa2));
        assert_eq!(test_helper.advance_test_fut(&mut test_fut), Poll::Pending);

        test_helper.advance_by(zx::MonotonicDuration::from_hours(1), test_fut.as_mut());
        assert_data_tree_with_respond_blocking_req!(test_helper, test_fut, root: contains {
            stats: contains {
                get_iface_stats_fail_count: 0u64,
                "1d_counters": contains {
                    // Deduct 15 seconds beecause there isn't packet counter to diff against in
                    // the first interval of telemetry
                    tx_high_packet_drop_duration: (zx::MonotonicDuration::from_hours(1) - TELEMETRY_QUERY_INTERVAL).into_nanos(),
                    rx_high_packet_drop_duration: 0i64,
                    tx_very_high_packet_drop_duration: (zx::MonotonicDuration::from_hours(1) - TELEMETRY_QUERY_INTERVAL).into_nanos(),
                    rx_very_high_packet_drop_duration: 0i64,
                    no_rx_duration: 0i64,
                },
                "7d_counters": contains {
                    tx_high_packet_drop_duration: (zx::MonotonicDuration::from_hours(1) - TELEMETRY_QUERY_INTERVAL).into_nanos(),
                    rx_high_packet_drop_duration: 0i64,
                    tx_very_high_packet_drop_duration: (zx::MonotonicDuration::from_hours(1) - TELEMETRY_QUERY_INTERVAL).into_nanos(),
                    rx_very_high_packet_drop_duration: 0i64,
                    no_rx_duration: 0i64,
                },
            }
        });
    }

    #[fuchsia::test]
    fn test_rx_high_packet_drop_duration_counters() {
        let (mut test_helper, mut test_fut) = setup_test();
        test_helper.set_counter_stats_resp(Box::new(|| {
            let seed = fasync::MonotonicInstant::now().into_nanos() as u64;
            Ok(fidl_fuchsia_wlan_stats::IfaceCounterStats {
                connection_counters: Some(fidl_fuchsia_wlan_stats::ConnectionCounters {
                    rx_unicast_total: Some(10 * seed),
                    rx_unicast_drop: Some(3 * seed),
                    ..fake_connection_counters(seed)
                }),
                ..Default::default()
            })
        }));

        test_helper.send_connected_event(random_bss_description!(Wpa2));
        assert_eq!(test_helper.advance_test_fut(&mut test_fut), Poll::Pending);

        test_helper.advance_by(zx::MonotonicDuration::from_hours(1), test_fut.as_mut());
        assert_data_tree_with_respond_blocking_req!(test_helper, test_fut, root: contains {
            stats: contains {
                get_iface_stats_fail_count: 0u64,
                "1d_counters": contains {
                    // Deduct 15 seconds beecause there isn't packet counter to diff against in
                    // the first interval of telemetry
                    rx_high_packet_drop_duration: (zx::MonotonicDuration::from_hours(1) - TELEMETRY_QUERY_INTERVAL).into_nanos(),
                    tx_high_packet_drop_duration: 0i64,
                    rx_very_high_packet_drop_duration: (zx::MonotonicDuration::from_hours(1) - TELEMETRY_QUERY_INTERVAL).into_nanos(),
                    tx_very_high_packet_drop_duration: 0i64,
                    no_rx_duration: 0i64,
                },
                "7d_counters": contains {
                    rx_high_packet_drop_duration: (zx::MonotonicDuration::from_hours(1) - TELEMETRY_QUERY_INTERVAL).into_nanos(),
                    tx_high_packet_drop_duration: 0i64,
                    rx_very_high_packet_drop_duration: (zx::MonotonicDuration::from_hours(1) - TELEMETRY_QUERY_INTERVAL).into_nanos(),
                    tx_very_high_packet_drop_duration: 0i64,
                    no_rx_duration: 0i64,
                },
            }
        });
    }

    #[fuchsia::test]
    fn test_rx_tx_high_but_not_very_high_packet_drop_duration_counters() {
        let (mut test_helper, mut test_fut) = setup_test();
        test_helper.set_counter_stats_resp(Box::new(|| {
            let seed = fasync::MonotonicInstant::now().into_nanos() as u64;
            Ok(fidl_fuchsia_wlan_stats::IfaceCounterStats {
                connection_counters: Some(fidl_fuchsia_wlan_stats::ConnectionCounters {
                    // 3% drop rate would be high, but not very high
                    rx_unicast_total: Some(100 * seed),
                    rx_unicast_drop: Some(3 * seed),
                    tx_total: Some(100 * seed),
                    tx_drop: Some(3 * seed),
                    ..fake_connection_counters(seed)
                }),
                ..Default::default()
            })
        }));

        test_helper.send_connected_event(random_bss_description!(Wpa2));
        assert_eq!(test_helper.advance_test_fut(&mut test_fut), Poll::Pending);

        test_helper.advance_by(zx::MonotonicDuration::from_hours(1), test_fut.as_mut());
        assert_data_tree_with_respond_blocking_req!(test_helper, test_fut, root: contains {
            stats: contains {
                get_iface_stats_fail_count: 0u64,
                "1d_counters": contains {
                    // Deduct 15 seconds beecause there isn't packet counter to diff against in
                    // the first interval of telemetry
                    rx_high_packet_drop_duration: (zx::MonotonicDuration::from_hours(1) - TELEMETRY_QUERY_INTERVAL).into_nanos(),
                    tx_high_packet_drop_duration: (zx::MonotonicDuration::from_hours(1) - TELEMETRY_QUERY_INTERVAL).into_nanos(),
                    // Very high drop rate counters should still be 0
                    rx_very_high_packet_drop_duration: 0i64,
                    tx_very_high_packet_drop_duration: 0i64,
                    no_rx_duration: 0i64,
                },
                "7d_counters": contains {
                    rx_high_packet_drop_duration: (zx::MonotonicDuration::from_hours(1) - TELEMETRY_QUERY_INTERVAL).into_nanos(),
                    tx_high_packet_drop_duration: (zx::MonotonicDuration::from_hours(1) - TELEMETRY_QUERY_INTERVAL).into_nanos(),
                    rx_very_high_packet_drop_duration: 0i64,
                    tx_very_high_packet_drop_duration: 0i64,
                    no_rx_duration: 0i64,
                },
            }
        });
    }

    #[fuchsia::test]
    fn test_rx_tx_packet_time_series() {
        let (mut test_helper, mut test_fut) = setup_test();
        test_helper.set_counter_stats_resp(Box::new(|| {
            let seed = (fasync::MonotonicInstant::now()
                - fasync::MonotonicInstant::from_nanos(0i64))
            .into_seconds() as u64;
            Ok(fidl_fuchsia_wlan_stats::IfaceCounterStats {
                connection_counters: Some(fidl_fuchsia_wlan_stats::ConnectionCounters {
                    rx_unicast_total: Some(100 * seed),
                    rx_unicast_drop: Some(3 * seed),
                    tx_total: Some(10 * seed),
                    tx_drop: Some(2 * seed),
                    ..fake_connection_counters(seed)
                }),
                ..Default::default()
            })
        }));

        test_helper.send_connected_event(random_bss_description!(Wpa2));
        assert_eq!(test_helper.advance_test_fut(&mut test_fut), Poll::Pending);

        test_helper.advance_by(zx::MonotonicDuration::from_minutes(2), test_fut.as_mut());

        let time_series = test_helper.get_time_series(&mut test_fut);
        let rx_unicast_drop_count: Vec<_> =
            time_series.lock().rx_unicast_drop_count.minutely_iter().copied().collect();
        let rx_unicast_total_count: Vec<_> =
            time_series.lock().rx_unicast_total_count.minutely_iter().copied().collect();
        let tx_drop_count: Vec<_> =
            time_series.lock().tx_drop_count.minutely_iter().copied().collect();
        let tx_total_count: Vec<_> =
            time_series.lock().tx_total_count.minutely_iter().copied().collect();

        // Note: Packets from the first 15 seconds are not accounted because we
        //       we did not take packet measurement at 0th second mark.
        //       Additionally, the count for 45th-60th second mark is logged
        //       at the 60th mark, which is considered to be part of the second
        //       window.
        assert_eq!(rx_unicast_drop_count, vec![90, 180, 45]);
        assert_eq!(rx_unicast_total_count, vec![3000, 6000, 1500]);
        assert_eq!(tx_drop_count, vec![60, 120, 30]);
        assert_eq!(tx_total_count, vec![300, 600, 150]);
    }

    #[fuchsia::test]
    fn test_no_rx_duration_counters() {
        let (mut test_helper, mut test_fut) = setup_test();
        test_helper.set_counter_stats_resp(Box::new(|| {
            let seed = fasync::MonotonicInstant::now().into_nanos() as u64;
            Ok(fidl_fuchsia_wlan_stats::IfaceCounterStats {
                connection_counters: Some(fidl_fuchsia_wlan_stats::ConnectionCounters {
                    rx_unicast_total: Some(10),
                    ..fake_connection_counters(seed)
                }),
                ..Default::default()
            })
        }));

        test_helper.send_connected_event(random_bss_description!(Wpa2));
        assert_eq!(test_helper.advance_test_fut(&mut test_fut), Poll::Pending);

        test_helper.advance_by(zx::MonotonicDuration::from_hours(1), test_fut.as_mut());
        assert_data_tree_with_respond_blocking_req!(test_helper, test_fut, root: contains {
            stats: contains {
                get_iface_stats_fail_count: 0u64,
                "1d_counters": contains {
                    // Deduct 15 seconds beecause there isn't packet counter to diff against in
                    // the first interval of telemetry
                    no_rx_duration: (zx::MonotonicDuration::from_hours(1) - TELEMETRY_QUERY_INTERVAL).into_nanos(),
                    rx_high_packet_drop_duration: 0i64,
                    tx_high_packet_drop_duration: 0i64,
                    rx_very_high_packet_drop_duration: 0i64,
                    tx_very_high_packet_drop_duration: 0i64,
                },
                "7d_counters": contains {
                    no_rx_duration: (zx::MonotonicDuration::from_hours(1) - TELEMETRY_QUERY_INTERVAL).into_nanos(),
                    rx_high_packet_drop_duration: 0i64,
                    tx_high_packet_drop_duration: 0i64,
                    rx_very_high_packet_drop_duration: 0i64,
                    tx_very_high_packet_drop_duration: 0i64,
                },
            }
        });
    }

    #[fuchsia::test]
    fn test_no_rx_duration_time_series() {
        let (mut test_helper, mut test_fut) = setup_test();
        test_helper.set_counter_stats_resp(Box::new(|| {
            let seed = fasync::MonotonicInstant::now().into_nanos() as u64;
            Ok(fidl_fuchsia_wlan_stats::IfaceCounterStats {
                connection_counters: Some(fidl_fuchsia_wlan_stats::ConnectionCounters {
                    rx_unicast_total: Some(10),
                    ..fake_connection_counters(seed)
                }),
                ..Default::default()
            })
        }));

        test_helper.send_connected_event(random_bss_description!(Wpa2));
        assert_eq!(test_helper.advance_test_fut(&mut test_fut), Poll::Pending);

        test_helper.advance_by(zx::MonotonicDuration::from_seconds(150), test_fut.as_mut());
        let time_series = test_helper.get_time_series(&mut test_fut);
        let no_rx_duration_sec: Vec<_> =
            time_series.lock().no_rx_duration_sec.minutely_iter().copied().collect();
        assert_eq!(no_rx_duration_sec, vec![30, 60, 45]);
    }

    #[fuchsia::test]
    fn test_get_iface_stats_fail() {
        let (mut test_helper, mut test_fut) = setup_test();
        test_helper.set_counter_stats_resp(Box::new(|| Err(zx::sys::ZX_ERR_NOT_SUPPORTED)));

        test_helper.send_connected_event(random_bss_description!(Wpa2));
        assert_eq!(test_helper.advance_test_fut(&mut test_fut), Poll::Pending);

        test_helper.advance_by(zx::MonotonicDuration::from_hours(1), test_fut.as_mut());
        assert_data_tree_with_respond_blocking_req!(test_helper, test_fut, root: contains {
            stats: contains {
                get_iface_stats_fail_count: NonZeroUintProperty,
                "1d_counters": contains {
                    no_rx_duration: 0i64,
                    rx_high_packet_drop_duration: 0i64,
                    tx_high_packet_drop_duration: 0i64,
                    rx_very_high_packet_drop_duration: 0i64,
                    tx_very_high_packet_drop_duration: 0i64,
                },
                "7d_counters": contains {
                    no_rx_duration: 0i64,
                    rx_high_packet_drop_duration: 0i64,
                    tx_high_packet_drop_duration: 0i64,
                    rx_very_high_packet_drop_duration: 0i64,
                    tx_very_high_packet_drop_duration: 0i64,
                },
            }
        });
    }

    #[fuchsia::test]
    fn test_log_signal_histograms_inspect() {
        let (mut test_helper, mut test_fut) = setup_test();
        test_helper.send_connected_event(random_bss_description!(Wpa2));
        test_helper.drain_cobalt_events(&mut test_fut);

        // Default iface stats responder in `test_helper` already mock these histograms.
        assert_data_tree_with_respond_blocking_req!(test_helper, test_fut, root: contains {
            external: contains {
                stats: contains {
                    connection_status: contains {
                        histograms: {
                            antenna0_2Ghz: {
                                antenna_index: 0u64,
                                antenna_freq: "2Ghz",
                                snr_histogram: vec![30i64, 999],
                                snr_invalid_samples: 11u64,
                                noise_floor_histogram: vec![-55i64, 999],
                                noise_floor_invalid_samples: 44u64,
                                rssi_histogram: vec![-25i64, 999],
                                rssi_invalid_samples: 55u64,
                            },
                            antenna1_5Ghz: {
                                antenna_index: 1u64,
                                antenna_freq: "5Ghz",
                                rx_rate_histogram: vec![100i64, 1500],
                                rx_rate_invalid_samples: 33u64,
                            },
                        }
                    }
                }
            }
        });
    }

    #[fuchsia::test]
    fn test_log_daily_uptime_ratio_cobalt_metric() {
        let (mut test_helper, mut test_fut) = setup_test();
        test_helper.send_connected_event(random_bss_description!(Wpa2));
        assert_eq!(test_helper.advance_test_fut(&mut test_fut), Poll::Pending);

        test_helper.advance_by(zx::MonotonicDuration::from_hours(12), test_fut.as_mut());

        let info = fake_disconnect_info();
        test_helper
            .telemetry_sender
            .send(TelemetryEvent::Disconnected { track_subsequent_downtime: true, info });
        assert_eq!(test_helper.advance_test_fut(&mut test_fut), Poll::Pending);

        test_helper.advance_by(zx::MonotonicDuration::from_hours(6), test_fut.as_mut());

        // Indicate that there's no saved neighbor in vicinity
        test_helper.telemetry_sender.send(TelemetryEvent::NetworkSelectionDecision {
            network_selection_type: NetworkSelectionType::Undirected,
            num_candidates: Ok(0),
            selected_count: 0,
        });

        test_helper.advance_by(zx::MonotonicDuration::from_hours(6), test_fut.as_mut());

        let uptime_ratios =
            test_helper.get_logged_metrics(metrics::CONNECTED_UPTIME_RATIO_METRIC_ID);
        assert_eq!(uptime_ratios.len(), 1);
        // 12 hours of uptime, 6 hours of adjusted downtime => 66.66% uptime
        assert_eq!(uptime_ratios[0].payload, MetricEventPayload::IntegerValue(6666));
    }

    /// Send a random connect event and 4 hours later send a disconnect with the specified
    /// disconnect source.
    fn connect_and_disconnect_with_source(
        test_helper: &mut TestHelper,
        mut test_fut: Pin<&mut impl Future<Output = ()>>,
        disconnect_source: fidl_sme::DisconnectSource,
    ) {
        test_helper.send_connected_event(random_bss_description!(Wpa2));
        assert_eq!(test_helper.advance_test_fut(&mut test_fut), Poll::Pending);

        test_helper.advance_by(zx::MonotonicDuration::from_hours(4), test_fut.as_mut());

        let info = DisconnectInfo { disconnect_source, ..fake_disconnect_info() };
        test_helper
            .telemetry_sender
            .send(TelemetryEvent::Disconnected { track_subsequent_downtime: true, info });
        assert_eq!(test_helper.advance_test_fut(&mut test_fut), Poll::Pending);
        test_helper.drain_cobalt_events(&mut test_fut);
    }

    #[fuchsia::test]
    fn test_log_daily_disconnect_per_day_connected_cobalt_metric() {
        let (mut test_helper, mut test_fut) = setup_test();

        // Send 1 roaming, 1 non-roaming, and 1 user disconnect with the device connected for a
        // total of 12 hours. The user disconnect is counted toward total disconnects but not for
        // roaming or non-roaming disconnects.
        let non_roaming_source = fidl_sme::DisconnectSource::Mlme(fidl_sme::DisconnectCause {
            reason_code: fidl_ieee80211::ReasonCode::LeavingNetworkDeauth,
            mlme_event_name: fidl_sme::DisconnectMlmeEventName::DeauthenticateIndication,
        });
        connect_and_disconnect_with_source(&mut test_helper, test_fut.as_mut(), non_roaming_source);

        let roaming_source = fidl_sme::DisconnectSource::User(
            fidl_sme::UserDisconnectReason::ProactiveNetworkSwitch,
        );
        connect_and_disconnect_with_source(&mut test_helper, test_fut.as_mut(), roaming_source);

        let user_source = fidl_sme::DisconnectSource::User(
            fidl_sme::UserDisconnectReason::FidlStopClientConnectionsRequest,
        );
        connect_and_disconnect_with_source(&mut test_helper, test_fut.as_mut(), user_source);

        test_helper.advance_by(zx::MonotonicDuration::from_hours(12), test_fut.as_mut());

        let dpdc_ratios =
            test_helper.get_logged_metrics(metrics::DISCONNECT_PER_DAY_CONNECTED_METRIC_ID);
        assert_eq!(dpdc_ratios.len(), 1);
        // 3 disconnects, 0.5 day connected => 6 disconnects per day connected
        assert_eq!(dpdc_ratios[0].payload, MetricEventPayload::IntegerValue(60_000));

        // 1 roaming disconnect, 0.5 day connected => 2 roaming disconnects per day connected
        let non_roam_dpdc_ratios =
            test_helper.get_logged_metrics(NON_ROAMING_DISCONNECT_PER_DAY_CONNECTED_METRIC_ID);
        assert_eq!(non_roam_dpdc_ratios.len(), 1);
        assert_eq!(non_roam_dpdc_ratios[0].payload, MetricEventPayload::IntegerValue(20_000));

        let roam_dpdc_ratios =
            test_helper.get_logged_metrics(ROAMING_DISCONNECT_PER_DAY_CONNECTED_METRIC_ID);
        assert_eq!(roam_dpdc_ratios.len(), 1);
        assert_eq!(roam_dpdc_ratios[0].payload, MetricEventPayload::IntegerValue(20_000));

        let dpdc_ratios_7d =
            test_helper.get_logged_metrics(metrics::DISCONNECT_PER_DAY_CONNECTED_7D_METRIC_ID);
        assert_eq!(dpdc_ratios_7d.len(), 1);
        assert_eq!(dpdc_ratios_7d[0].payload, MetricEventPayload::IntegerValue(60_000));

        // Clear record of logged Cobalt events
        test_helper.cobalt_events.clear();

        // Connect for another 1 day to dilute the 7d ratio
        test_helper.send_connected_event(random_bss_description!(Wpa2));
        assert_eq!(test_helper.advance_test_fut(&mut test_fut), Poll::Pending);

        test_helper.advance_by(zx::MonotonicDuration::from_hours(24), test_fut.as_mut());

        // No disconnect in the last day, so the 1d ratio would be 0.
        let dpdc_ratios =
            test_helper.get_logged_metrics(metrics::DISCONNECT_PER_DAY_CONNECTED_METRIC_ID);
        assert_eq!(dpdc_ratios.len(), 1);
        assert_eq!(dpdc_ratios[0].payload, MetricEventPayload::IntegerValue(0));

        let non_roam_dpdc_ratios =
            test_helper.get_logged_metrics(NON_ROAMING_DISCONNECT_PER_DAY_CONNECTED_METRIC_ID);
        assert_eq!(non_roam_dpdc_ratios.len(), 1);
        assert_eq!(non_roam_dpdc_ratios[0].payload, MetricEventPayload::IntegerValue(0));

        let dpdc_ratios_7d =
            test_helper.get_logged_metrics(metrics::DISCONNECT_PER_DAY_CONNECTED_7D_METRIC_ID);
        assert_eq!(dpdc_ratios_7d.len(), 1);
        // 3 disconnects, 1.5 day connected => 2 disconnects per day connected
        // (which equals 20,000 in TenThousandth unit)
        assert_eq!(dpdc_ratios_7d[0].payload, MetricEventPayload::IntegerValue(20_000));

        let roam_dpdc_ratios =
            test_helper.get_logged_metrics(ROAMING_DISCONNECT_PER_DAY_CONNECTED_METRIC_ID);
        assert_eq!(roam_dpdc_ratios.len(), 1);
        assert_eq!(roam_dpdc_ratios[0].payload, MetricEventPayload::IntegerValue(0));
    }

    #[fuchsia::test]
    fn test_log_daily_roaming_disconnect_per_day_connected_cobalt_metric() {
        let (mut test_helper, mut test_fut) = setup_test();
        test_helper.send_connected_event(random_bss_description!(Wpa2));
        assert_eq!(test_helper.advance_test_fut(&mut test_fut), Poll::Pending);

        test_helper.advance_by(zx::MonotonicDuration::from_hours(12), test_fut.as_mut());

        let info = DisconnectInfo {
            disconnect_source: fidl_sme::DisconnectSource::User(
                fidl_sme::UserDisconnectReason::ProactiveNetworkSwitch,
            ),
            ..fake_disconnect_info()
        };
        test_helper
            .telemetry_sender
            .send(TelemetryEvent::Disconnected { track_subsequent_downtime: true, info });
        assert_eq!(test_helper.advance_test_fut(&mut test_fut), Poll::Pending);

        test_helper.advance_by(zx::MonotonicDuration::from_hours(12), test_fut.as_mut());

        let dpdc_ratios =
            test_helper.get_logged_metrics(metrics::DISCONNECT_PER_DAY_CONNECTED_METRIC_ID);
        assert_eq!(dpdc_ratios.len(), 1);
        // 1 disconnect, 0.5 day connected => 2 disconnects per day connected
        // (which equals 20_0000 in TenThousandth unit)
        assert_eq!(dpdc_ratios[0].payload, MetricEventPayload::IntegerValue(20_000));

        let non_roam_dpdc_ratios =
            test_helper.get_logged_metrics(NON_ROAMING_DISCONNECT_PER_DAY_CONNECTED_METRIC_ID);
        assert_eq!(non_roam_dpdc_ratios.len(), 1);
        assert_eq!(non_roam_dpdc_ratios[0].payload, MetricEventPayload::IntegerValue(0));

        let roam_dpdc_ratios =
            test_helper.get_logged_metrics(ROAMING_DISCONNECT_PER_DAY_CONNECTED_METRIC_ID);
        assert_eq!(roam_dpdc_ratios.len(), 1);
        assert_eq!(roam_dpdc_ratios[0].payload, MetricEventPayload::IntegerValue(20_000));
    }

    #[fuchsia::test]
    fn test_log_daily_disconnect_per_day_connected_cobalt_metric_device_high_disconnect() {
        let (mut test_helper, mut test_fut) = setup_test();
        test_helper.send_connected_event(random_bss_description!(Wpa2));
        assert_eq!(test_helper.advance_test_fut(&mut test_fut), Poll::Pending);

        test_helper.advance_by(zx::MonotonicDuration::from_hours(1), test_fut.as_mut());
        let info = fake_disconnect_info();
        test_helper
            .telemetry_sender
            .send(TelemetryEvent::Disconnected { track_subsequent_downtime: true, info });
        assert_eq!(test_helper.advance_test_fut(&mut test_fut), Poll::Pending);

        test_helper.advance_by(zx::MonotonicDuration::from_hours(23), test_fut.as_mut());
    }

    #[fuchsia::test]
    fn test_log_daily_rx_tx_ratio_cobalt_metrics() {
        let (mut test_helper, mut test_fut) = setup_test();
        test_helper.set_counter_stats_resp(Box::new(|| {
            let seed = fasync::MonotonicInstant::now().into_nanos() as u64 / 1_000_000_000;
            Ok(fidl_fuchsia_wlan_stats::IfaceCounterStats {
                connection_counters: Some(fidl_fuchsia_wlan_stats::ConnectionCounters {
                    tx_total: Some(10 * seed),
                    // TX drop rate stops increasing at 1 hour + TELEMETRY_QUERY_INTERVAL mark.
                    // Because the first TELEMETRY_QUERY_INTERVAL doesn't count when
                    // computing counters, this leads to 3 hour of high TX drop rate.
                    tx_drop: Some(
                        3 * min(
                            seed,
                            (zx::MonotonicDuration::from_hours(3) + TELEMETRY_QUERY_INTERVAL)
                                .into_seconds() as u64,
                        ),
                    ),
                    // RX total stops increasing at 23 hour mark
                    rx_unicast_total: Some(
                        10 * min(seed, zx::MonotonicDuration::from_hours(23).into_seconds() as u64),
                    ),
                    // RX drop rate stops increasing at 4 hour + TELEMETRY_QUERY_INTERVAL mark.
                    rx_unicast_drop: Some(
                        3 * min(
                            seed,
                            (zx::MonotonicDuration::from_hours(4) + TELEMETRY_QUERY_INTERVAL)
                                .into_seconds() as u64,
                        ),
                    ),
                    ..fake_connection_counters(seed)
                }),
                ..Default::default()
            })
        }));

        test_helper.send_connected_event(random_bss_description!(Wpa2));
        assert_eq!(test_helper.advance_test_fut(&mut test_fut), Poll::Pending);

        test_helper.advance_by(zx::MonotonicDuration::from_hours(24), test_fut.as_mut());

        let high_rx_drop_time_ratios =
            test_helper.get_logged_metrics(metrics::TIME_RATIO_WITH_HIGH_RX_PACKET_DROP_METRIC_ID);
        // 4 hours of high RX drop rate, 24 hours connected => 16.66% duration
        assert_eq!(high_rx_drop_time_ratios.len(), 1);
        assert_eq!(high_rx_drop_time_ratios[0].payload, MetricEventPayload::IntegerValue(1666));

        let high_tx_drop_time_ratios =
            test_helper.get_logged_metrics(metrics::TIME_RATIO_WITH_HIGH_TX_PACKET_DROP_METRIC_ID);
        // 3 hours of high RX drop rate, 24 hours connected => 12.48% duration
        assert_eq!(high_tx_drop_time_ratios.len(), 1);
        assert_eq!(high_tx_drop_time_ratios[0].payload, MetricEventPayload::IntegerValue(1250));

        let very_high_rx_drop_time_ratios = test_helper
            .get_logged_metrics(metrics::TIME_RATIO_WITH_VERY_HIGH_RX_PACKET_DROP_METRIC_ID);
        assert_eq!(very_high_rx_drop_time_ratios.len(), 1);
        assert_eq!(
            very_high_rx_drop_time_ratios[0].payload,
            MetricEventPayload::IntegerValue(1666)
        );

        let very_high_tx_drop_time_ratios = test_helper
            .get_logged_metrics(metrics::TIME_RATIO_WITH_VERY_HIGH_TX_PACKET_DROP_METRIC_ID);
        assert_eq!(very_high_tx_drop_time_ratios.len(), 1);
        assert_eq!(
            very_high_tx_drop_time_ratios[0].payload,
            MetricEventPayload::IntegerValue(1250)
        );

        // 1 hour of no RX, 24 hours connected => 4.16% duration
        let no_rx_time_ratios =
            test_helper.get_logged_metrics(metrics::TIME_RATIO_WITH_NO_RX_METRIC_ID);
        assert_eq!(no_rx_time_ratios.len(), 1);
        assert_eq!(no_rx_time_ratios[0].payload, MetricEventPayload::IntegerValue(416));
    }

    #[fuchsia::test]
    fn test_log_daily_rx_tx_ratio_cobalt_metrics_zero() {
        // This test is to verify that when the RX/TX ratios are 0 (there's no issue), we still
        // log to Cobalt.
        let (mut test_helper, mut test_fut) = setup_test();

        test_helper.send_connected_event(random_bss_description!(Wpa2));
        assert_eq!(test_helper.advance_test_fut(&mut test_fut), Poll::Pending);

        test_helper.advance_by(zx::MonotonicDuration::from_hours(24), test_fut.as_mut());

        let high_rx_drop_time_ratios =
            test_helper.get_logged_metrics(metrics::TIME_RATIO_WITH_HIGH_RX_PACKET_DROP_METRIC_ID);
        assert_eq!(high_rx_drop_time_ratios.len(), 1);
        assert_eq!(high_rx_drop_time_ratios[0].payload, MetricEventPayload::IntegerValue(0));

        let high_tx_drop_time_ratios =
            test_helper.get_logged_metrics(metrics::TIME_RATIO_WITH_HIGH_TX_PACKET_DROP_METRIC_ID);
        assert_eq!(high_tx_drop_time_ratios.len(), 1);
        assert_eq!(high_tx_drop_time_ratios[0].payload, MetricEventPayload::IntegerValue(0));

        let very_high_rx_drop_time_ratios = test_helper
            .get_logged_metrics(metrics::TIME_RATIO_WITH_VERY_HIGH_RX_PACKET_DROP_METRIC_ID);
        assert_eq!(very_high_rx_drop_time_ratios.len(), 1);
        assert_eq!(very_high_rx_drop_time_ratios[0].payload, MetricEventPayload::IntegerValue(0));

        let very_high_tx_drop_time_ratios = test_helper
            .get_logged_metrics(metrics::TIME_RATIO_WITH_VERY_HIGH_TX_PACKET_DROP_METRIC_ID);
        assert_eq!(very_high_tx_drop_time_ratios.len(), 1);
        assert_eq!(very_high_tx_drop_time_ratios[0].payload, MetricEventPayload::IntegerValue(0));

        let no_rx_time_ratios =
            test_helper.get_logged_metrics(metrics::TIME_RATIO_WITH_NO_RX_METRIC_ID);
        assert_eq!(no_rx_time_ratios.len(), 1);
        assert_eq!(no_rx_time_ratios[0].payload, MetricEventPayload::IntegerValue(0));
    }

    #[fuchsia::test]
    fn test_log_daily_establish_connection_metrics() {
        let (mut test_helper, mut test_fut) = setup_test();

        // Send 10 failed connect results, then 1 successful.
        for _ in 0..10 {
            let event = TelemetryEvent::ConnectResult {
                iface_id: IFACE_ID,
                policy_connect_reason: Some(
                    client::types::ConnectReason::RetryAfterFailedConnectAttempt,
                ),
                result: fake_connect_result(fidl_ieee80211::StatusCode::RefusedReasonUnspecified),
                multiple_bss_candidates: true,
                ap_state: random_bss_description!(Wpa1).into(),
                network_is_likely_hidden: true,
            };
            test_helper.telemetry_sender.send(event);
        }
        test_helper.send_connected_event(random_bss_description!(Wpa2));

        test_helper.advance_by(zx::MonotonicDuration::from_hours(24), test_fut.as_mut());

        let connection_success_rate =
            test_helper.get_logged_metrics(metrics::CONNECTION_SUCCESS_RATE_METRIC_ID);
        assert_eq!(connection_success_rate.len(), 1);
        // 1 successful, 11 total attempts => 9.09% success rate
        assert_eq!(connection_success_rate[0].payload, MetricEventPayload::IntegerValue(909));
    }

    #[fuchsia::test]
    fn test_log_hourly_fleetwide_uptime_cobalt_metrics() {
        let (mut test_helper, mut test_fut) = setup_test();

        test_helper.send_connected_event(random_bss_description!(Wpa2));
        assert_eq!(test_helper.advance_test_fut(&mut test_fut), Poll::Pending);

        test_helper.advance_by(zx::MonotonicDuration::from_hours(1), test_fut.as_mut());

        let total_wlan_uptime_durs =
            test_helper.get_logged_metrics(metrics::TOTAL_WLAN_UPTIME_NEAR_SAVED_NETWORK_METRIC_ID);
        assert_eq!(total_wlan_uptime_durs.len(), 1);
        assert_eq!(
            total_wlan_uptime_durs[0].payload,
            MetricEventPayload::IntegerValue(zx::MonotonicDuration::from_hours(1).into_micros())
        );

        let connected_durs =
            test_helper.get_logged_metrics(metrics::TOTAL_CONNECTED_UPTIME_METRIC_ID);
        assert_eq!(connected_durs.len(), 1);
        assert_eq!(
            connected_durs[0].payload,
            MetricEventPayload::IntegerValue(zx::MonotonicDuration::from_hours(1).into_micros())
        );

        // Clear record of logged Cobalt events
        test_helper.cobalt_events.clear();

        test_helper.advance_by(zx::MonotonicDuration::from_minutes(30), test_fut.as_mut());

        let info = fake_disconnect_info();
        test_helper
            .telemetry_sender
            .send(TelemetryEvent::Disconnected { track_subsequent_downtime: true, info });
        assert_eq!(test_helper.advance_test_fut(&mut test_fut), Poll::Pending);

        test_helper.advance_by(zx::MonotonicDuration::from_minutes(15), test_fut.as_mut());

        // Indicate that there's no saved neighbor in vicinity
        test_helper.telemetry_sender.send(TelemetryEvent::NetworkSelectionDecision {
            network_selection_type: NetworkSelectionType::Undirected,
            num_candidates: Ok(0),
            selected_count: 0,
        });
        assert_eq!(test_helper.advance_test_fut(&mut test_fut), Poll::Pending);

        test_helper.advance_by(zx::MonotonicDuration::from_minutes(15), test_fut.as_mut());

        let total_wlan_uptime_durs =
            test_helper.get_logged_metrics(metrics::TOTAL_WLAN_UPTIME_NEAR_SAVED_NETWORK_METRIC_ID);
        assert_eq!(total_wlan_uptime_durs.len(), 1);
        // 30 minutes connected uptime + 15 minutes downtime near saved network
        assert_eq!(
            total_wlan_uptime_durs[0].payload,
            MetricEventPayload::IntegerValue(zx::MonotonicDuration::from_minutes(45).into_micros())
        );

        let connected_durs =
            test_helper.get_logged_metrics(metrics::TOTAL_CONNECTED_UPTIME_METRIC_ID);
        assert_eq!(connected_durs.len(), 1);
        assert_eq!(
            connected_durs[0].payload,
            MetricEventPayload::IntegerValue(zx::MonotonicDuration::from_minutes(30).into_micros())
        );
    }

    #[fuchsia::test]
    fn test_log_hourly_fleetwide_rx_tx_cobalt_metrics() {
        let (mut test_helper, mut test_fut) = setup_test();
        test_helper.set_counter_stats_resp(Box::new(|| {
            let seed = fasync::MonotonicInstant::now().into_nanos() as u64 / 1_000_000_000;
            Ok(fidl_fuchsia_wlan_stats::IfaceCounterStats {
                connection_counters: Some(fidl_fuchsia_wlan_stats::ConnectionCounters {
                    tx_total: Some(10 * seed),
                    // TX drop rate stops increasing at 10 min + TELEMETRY_QUERY_INTERVAL mark.
                    // Because the first TELEMETRY_QUERY_INTERVAL doesn't count when
                    // computing counters, this leads to 10 min of high TX drop rate.
                    tx_drop: Some(
                        3 * min(
                            seed,
                            (zx::MonotonicDuration::from_minutes(10) + TELEMETRY_QUERY_INTERVAL)
                                .into_seconds() as u64,
                        ),
                    ),
                    // RX total stops increasing at 45 min mark
                    rx_unicast_total: Some(
                        10 * min(
                            seed,
                            zx::MonotonicDuration::from_minutes(45).into_seconds() as u64,
                        ),
                    ),
                    // RX drop rate stops increasing at 20 min + TELEMETRY_QUERY_INTERVAL mark.
                    rx_unicast_drop: Some(
                        3 * min(
                            seed,
                            (zx::MonotonicDuration::from_minutes(20) + TELEMETRY_QUERY_INTERVAL)
                                .into_seconds() as u64,
                        ),
                    ),
                    ..fake_connection_counters(seed)
                }),
                ..Default::default()
            })
        }));

        test_helper.send_connected_event(random_bss_description!(Wpa2));
        assert_eq!(test_helper.advance_test_fut(&mut test_fut), Poll::Pending);

        test_helper.advance_by(zx::MonotonicDuration::from_hours(1), test_fut.as_mut());

        let rx_high_drop_durs =
            test_helper.get_logged_metrics(metrics::TOTAL_TIME_WITH_HIGH_RX_PACKET_DROP_METRIC_ID);
        assert_eq!(rx_high_drop_durs.len(), 1);
        assert_eq!(
            rx_high_drop_durs[0].payload,
            MetricEventPayload::IntegerValue(zx::MonotonicDuration::from_minutes(20).into_micros())
        );

        let tx_high_drop_durs =
            test_helper.get_logged_metrics(metrics::TOTAL_TIME_WITH_HIGH_TX_PACKET_DROP_METRIC_ID);
        assert_eq!(tx_high_drop_durs.len(), 1);
        assert_eq!(
            tx_high_drop_durs[0].payload,
            MetricEventPayload::IntegerValue(zx::MonotonicDuration::from_minutes(10).into_micros())
        );

        let rx_very_high_drop_durs = test_helper
            .get_logged_metrics(metrics::TOTAL_TIME_WITH_VERY_HIGH_RX_PACKET_DROP_METRIC_ID);
        assert_eq!(rx_very_high_drop_durs.len(), 1);
        assert_eq!(
            rx_very_high_drop_durs[0].payload,
            MetricEventPayload::IntegerValue(zx::MonotonicDuration::from_minutes(20).into_micros())
        );

        let tx_very_high_drop_durs = test_helper
            .get_logged_metrics(metrics::TOTAL_TIME_WITH_VERY_HIGH_TX_PACKET_DROP_METRIC_ID);
        assert_eq!(tx_very_high_drop_durs.len(), 1);
        assert_eq!(
            tx_very_high_drop_durs[0].payload,
            MetricEventPayload::IntegerValue(zx::MonotonicDuration::from_minutes(10).into_micros())
        );

        let no_rx_durs = test_helper.get_logged_metrics(metrics::TOTAL_TIME_WITH_NO_RX_METRIC_ID);
        assert_eq!(no_rx_durs.len(), 1);
        assert_eq!(
            no_rx_durs[0].payload,
            MetricEventPayload::IntegerValue(zx::MonotonicDuration::from_minutes(15).into_micros())
        );
    }

    #[fuchsia::test]
    fn test_log_rssi_hourly() {
        let (mut test_helper, mut test_fut) = setup_test();

        // RSSI velocity is only logged if in the connected state.
        test_helper.send_connected_event(random_bss_description!(Wpa2));

        // Send some RSSI velocities
        let ind_1 = fidl_internal::SignalReportIndication { rssi_dbm: -50, snr_db: 30 };
        let ind_2 = fidl_internal::SignalReportIndication { rssi_dbm: -61, snr_db: 40 };
        test_helper.telemetry_sender.send(TelemetryEvent::OnSignalReport { ind: ind_1 });
        test_helper.telemetry_sender.send(TelemetryEvent::OnSignalReport { ind: ind_1 });
        test_helper.telemetry_sender.send(TelemetryEvent::OnSignalReport { ind: ind_2 });

        // After an hour has passed, the RSSI should be logged to cobalt
        test_helper.advance_by(zx::MonotonicDuration::from_hours(1), test_fut.as_mut());
        test_helper.drain_cobalt_events(&mut test_fut);

        let metrics = test_helper.get_logged_metrics(metrics::CONNECTION_RSSI_METRIC_ID);
        assert_eq!(metrics.len(), 1);
        assert_variant!(&metrics[0].payload, MetricEventPayload::Histogram(buckets) => {
            assert_eq!(buckets.len(), 2);
            assert!(buckets.contains(&fidl_fuchsia_metrics::HistogramBucket{index: 79, count: 2}));
            assert!(buckets.contains(&fidl_fuchsia_metrics::HistogramBucket{index: 68, count: 1}));
        });
        test_helper.clear_cobalt_events();

        // Send another different RSSI
        let ind_3 = fidl_internal::SignalReportIndication { rssi_dbm: -75, snr_db: 30 };
        test_helper.telemetry_sender.send(TelemetryEvent::OnSignalReport { ind: ind_3 });
        test_helper.advance_by(zx::MonotonicDuration::from_hours(1), test_fut.as_mut());

        // Check that the previously logged values are not logged again, and the new value is
        // logged.
        test_helper.drain_cobalt_events(&mut test_fut);

        let metrics = test_helper.get_logged_metrics(metrics::CONNECTION_RSSI_METRIC_ID);
        assert_eq!(metrics.len(), 1);
        let buckets =
            assert_variant!(&metrics[0].payload, MetricEventPayload::Histogram(buckets) => buckets);
        assert_eq!(buckets.len(), 1);
        assert!(buckets.contains(&fidl_fuchsia_metrics::HistogramBucket { index: 54, count: 1 }));
    }

    #[fuchsia::test]
    fn test_log_rssi_velocity_hourly() {
        let (mut test_helper, mut test_fut) = setup_test();

        // RSSI velocity is only logged if in the connected state.
        test_helper.send_connected_event(random_bss_description!(Wpa2));

        // Send some RSSI velocities
        let rssi_velocity_1 = -2.0;
        let rssi_velocity_2 = 2.0;
        test_helper
            .telemetry_sender
            .send(TelemetryEvent::OnSignalVelocityUpdate { rssi_velocity: rssi_velocity_1 });
        test_helper
            .telemetry_sender
            .send(TelemetryEvent::OnSignalVelocityUpdate { rssi_velocity: rssi_velocity_2 });
        test_helper
            .telemetry_sender
            .send(TelemetryEvent::OnSignalVelocityUpdate { rssi_velocity: rssi_velocity_2 });

        // After an hour has passed, the RSSI velocity should be logged to cobalt
        test_helper.advance_by(zx::MonotonicDuration::from_hours(1), test_fut.as_mut());
        test_helper.drain_cobalt_events(&mut test_fut);

        let metrics = test_helper.get_logged_metrics(metrics::RSSI_VELOCITY_METRIC_ID);
        assert_eq!(metrics.len(), 1);
        assert_variant!(&metrics[0].payload, MetricEventPayload::Histogram(buckets) => {
            // RSSI velocity in [-2,-1) maps to bucket 9 and velocity in [2,3) maps to bucket 13.
            assert_eq!(buckets.len(), 2);
            assert!(buckets.contains(&fidl_fuchsia_metrics::HistogramBucket{index: 9, count: 1}));
            assert!(buckets.contains(&fidl_fuchsia_metrics::HistogramBucket{index: 13, count: 2}));
        });
        test_helper.clear_cobalt_events();

        // Send another different RSSI velocity
        let rssi_velocity_3 = 3.0;
        test_helper
            .telemetry_sender
            .send(TelemetryEvent::OnSignalVelocityUpdate { rssi_velocity: rssi_velocity_3 });
        test_helper.advance_by(zx::MonotonicDuration::from_hours(1), test_fut.as_mut());

        // Check that the previously logged values are not logged again, and the new value is
        // logged.
        test_helper.drain_cobalt_events(&mut test_fut);

        let metrics = test_helper.get_logged_metrics(metrics::RSSI_VELOCITY_METRIC_ID);
        assert_eq!(metrics.len(), 1);
        assert_eq!(
            metrics[0].payload,
            MetricEventPayload::Histogram(vec![fidl_fuchsia_metrics::HistogramBucket {
                index: 14,
                count: 1
            }])
        );
    }

    #[fuchsia::test]
    fn test_log_rssi_histogram_bounds() {
        let (mut test_helper, mut test_fut) = setup_test();

        // RSSI is only logged if in the connected state.
        test_helper.send_connected_event(random_bss_description!(Wpa2));

        let ind_min = fidl_internal::SignalReportIndication { rssi_dbm: -128, snr_db: 30 };
        // 0 is the highest histogram bucket and 1 and above are in the overflow bucket.
        let ind_max = fidl_internal::SignalReportIndication { rssi_dbm: 0, snr_db: 30 };
        let ind_overflow_1 = fidl_internal::SignalReportIndication { rssi_dbm: 1, snr_db: 30 };
        let ind_overflow_2 = fidl_internal::SignalReportIndication { rssi_dbm: 127, snr_db: 30 };
        // Send the telemetry events. -10 is the min velocity bucket and 10 is the max.
        test_helper.telemetry_sender.send(TelemetryEvent::OnSignalReport { ind: ind_min });
        test_helper.telemetry_sender.send(TelemetryEvent::OnSignalReport { ind: ind_min });
        test_helper.telemetry_sender.send(TelemetryEvent::OnSignalReport { ind: ind_min });
        test_helper.telemetry_sender.send(TelemetryEvent::OnSignalReport { ind: ind_max });
        test_helper.telemetry_sender.send(TelemetryEvent::OnSignalReport { ind: ind_overflow_1 });
        test_helper.telemetry_sender.send(TelemetryEvent::OnSignalReport { ind: ind_overflow_2 });
        test_helper.advance_by(zx::MonotonicDuration::from_hours(1), test_fut.as_mut());

        // Check that the min, max, underflow, and overflow buckets are used correctly.
        test_helper.drain_cobalt_events(&mut test_fut);
        // Check RSSI values
        let metrics = test_helper.get_logged_metrics(metrics::CONNECTION_RSSI_METRIC_ID);
        assert_eq!(metrics.len(), 1);
        let buckets =
            assert_variant!(&metrics[0].payload, MetricEventPayload::Histogram(buckets) => buckets);
        assert!(buckets.contains(&fidl_fuchsia_metrics::HistogramBucket { index: 1, count: 3 }));
        assert!(buckets.contains(&fidl_fuchsia_metrics::HistogramBucket { index: 129, count: 1 }));
        assert!(buckets.contains(&fidl_fuchsia_metrics::HistogramBucket { index: 130, count: 2 }));
    }

    #[fuchsia::test]
    fn test_log_rssi_velocity_histogram_bounds() {
        let (mut test_helper, mut test_fut) = setup_test();

        // RSSI velocity is only logged if in the connected state.
        test_helper.send_connected_event(random_bss_description!(Wpa2));

        // Send the telemetry events. -10 is the min velocity bucket and 10 is the max.
        test_helper
            .telemetry_sender
            .send(TelemetryEvent::OnSignalVelocityUpdate { rssi_velocity: -11.0 });
        test_helper
            .telemetry_sender
            .send(TelemetryEvent::OnSignalVelocityUpdate { rssi_velocity: -15.0 });
        test_helper
            .telemetry_sender
            .send(TelemetryEvent::OnSignalVelocityUpdate { rssi_velocity: 11.0 });
        test_helper
            .telemetry_sender
            .send(TelemetryEvent::OnSignalVelocityUpdate { rssi_velocity: 20.0 });
        test_helper
            .telemetry_sender
            .send(TelemetryEvent::OnSignalVelocityUpdate { rssi_velocity: -10.0 });
        test_helper
            .telemetry_sender
            .send(TelemetryEvent::OnSignalVelocityUpdate { rssi_velocity: 10.0 });
        test_helper.advance_by(zx::MonotonicDuration::from_hours(1), test_fut.as_mut());

        // Check that the min, max, underflow, and overflow buckets are used correctly.
        test_helper.drain_cobalt_events(&mut test_fut);

        // Check RSSI velocity values
        let metrics = test_helper.get_logged_metrics(metrics::RSSI_VELOCITY_METRIC_ID);
        assert_eq!(metrics.len(), 1);
        let buckets =
            assert_variant!(&metrics[0].payload, MetricEventPayload::Histogram(buckets) => buckets);
        // RSSI velocity below -10 maps to underflow bucket, and 11 or above maps to overflow.
        assert!(buckets.contains(&fidl_fuchsia_metrics::HistogramBucket { index: 1, count: 1 }));
        assert!(buckets.contains(&fidl_fuchsia_metrics::HistogramBucket { index: 21, count: 1 }));
        assert!(buckets.contains(&fidl_fuchsia_metrics::HistogramBucket { index: 0, count: 2 }));
        assert!(buckets.contains(&fidl_fuchsia_metrics::HistogramBucket { index: 22, count: 2 }));
    }

    #[fuchsia::test]
    fn test_log_short_duration_connection_metrics() {
        let (mut test_helper, mut test_fut) = setup_test();
        let now = fasync::MonotonicInstant::now();
        test_helper.send_connected_event(random_bss_description!(Wpa2));
        assert_eq!(test_helper.advance_test_fut(&mut test_fut), Poll::Pending);

        let channel = generate_random_channel();
        let ap_state = random_bss_description!(Wpa2, channel: channel).into();
        let mut signals = HistoricalList::new(5);
        signals.add(client::types::TimestampedSignal {
            signal: client::types::Signal { rssi_dbm: -30, snr_db: 60 },
            time: now,
        });
        signals.add(client::types::TimestampedSignal {
            signal: client::types::Signal { rssi_dbm: -30, snr_db: 60 },
            time: now,
        });
        // Log disconnect with reason FidlConnectRequest during short duration
        let info = DisconnectInfo {
            connected_duration: METRICS_SHORT_CONNECT_DURATION
                - zx::MonotonicDuration::from_seconds(1),
            disconnect_source: fidl_sme::DisconnectSource::User(
                fidl_sme::UserDisconnectReason::FidlConnectRequest,
            ),
            ap_state,
            signals,
            ..fake_disconnect_info()
        };
        test_helper.telemetry_sender.send(TelemetryEvent::Disconnected {
            track_subsequent_downtime: true,
            info: info.clone(),
        });

        test_helper.send_connected_event(random_bss_description!(Wpa2));
        assert_eq!(test_helper.advance_test_fut(&mut test_fut), Poll::Pending);

        // Log disconnect with reason NetworkUnsaved during short duration
        let info = DisconnectInfo {
            disconnect_source: fidl_sme::DisconnectSource::User(
                fidl_sme::UserDisconnectReason::NetworkUnsaved,
            ),
            ..info
        };
        test_helper.telemetry_sender.send(TelemetryEvent::Disconnected {
            track_subsequent_downtime: true,
            info: info.clone(),
        });

        test_helper.send_connected_event(random_bss_description!(Wpa2));
        assert_eq!(test_helper.advance_test_fut(&mut test_fut), Poll::Pending);

        // Log disconnect with reason NetworkUnsaved during longer duration connection
        let info = DisconnectInfo {
            connected_duration: METRICS_SHORT_CONNECT_DURATION
                + zx::MonotonicDuration::from_seconds(1),
            ..info
        };
        test_helper.telemetry_sender.send(TelemetryEvent::Disconnected {
            track_subsequent_downtime: true,
            info: info.clone(),
        });

        test_helper.drain_cobalt_events(&mut test_fut);

        let logged_metrics = test_helper.get_logged_metrics(
            metrics::POLICY_FIDL_CONNECTION_ATTEMPTS_DURING_SHORT_CONNECTION_METRIC_ID,
        );
        assert_eq!(logged_metrics.len(), 2);

        let logged_metrics = test_helper.get_logged_metrics(
            metrics::POLICY_FIDL_CONNECTION_ATTEMPTS_DURING_SHORT_CONNECTION_DETAILED_METRIC_ID,
        );
        assert_eq!(logged_metrics.len(), 2);
        assert_eq!(logged_metrics[0].event_codes, vec![info.previous_connect_reason as u32]);

        let logged_metrics =
            test_helper.get_logged_metrics(metrics::CONNECTION_SCORE_AVERAGE_METRIC_ID);
        assert_eq!(logged_metrics.len(), 2);
        assert_eq!(
            logged_metrics[0].event_codes,
            vec![metrics::ConnectionScoreAverageMetricDimensionDuration::ShortDuration as u32]
        );
        assert_eq!(logged_metrics[0].payload, MetricEventPayload::IntegerValue(100));
    }

    #[fuchsia::test]
    fn test_log_disconnect_cobalt_metrics() {
        let (mut test_helper, mut test_fut) = setup_test();
        test_helper.advance_by(zx::MonotonicDuration::from_hours(3), test_fut.as_mut());
        test_helper.send_connected_event(random_bss_description!(Wpa2));
        assert_eq!(test_helper.advance_test_fut(&mut test_fut), Poll::Pending);

        test_helper.advance_by(zx::MonotonicDuration::from_hours(5), test_fut.as_mut());

        let primary_channel = 8;
        let channel = Channel::new(primary_channel, Cbw::Cbw20);
        let ap_state = random_bss_description!(Wpa2, channel: channel).into();
        let info = DisconnectInfo {
            connected_duration: zx::MonotonicDuration::from_hours(5),
            disconnect_source: fidl_sme::DisconnectSource::Mlme(fidl_sme::DisconnectCause {
                reason_code: fidl_ieee80211::ReasonCode::LeavingNetworkDeauth,
                mlme_event_name: fidl_sme::DisconnectMlmeEventName::DeauthenticateIndication,
            }),
            ap_state,
            ..fake_disconnect_info()
        };
        test_helper
            .telemetry_sender
            .send(TelemetryEvent::Disconnected { track_subsequent_downtime: true, info });
        test_helper.drain_cobalt_events(&mut test_fut);

        let policy_disconnection_reasons =
            test_helper.get_logged_metrics(metrics::POLICY_DISCONNECTION_MIGRATED_METRIC_ID);
        assert_eq!(policy_disconnection_reasons.len(), 1);
        assert_eq!(policy_disconnection_reasons[0].payload, MetricEventPayload::Count(1));
        assert_eq!(
            policy_disconnection_reasons[0].event_codes,
            vec![client::types::DisconnectReason::DisconnectDetectedFromSme as u32]
        );

        let disconnect_counts =
            test_helper.get_logged_metrics(metrics::TOTAL_DISCONNECT_COUNT_METRIC_ID);
        assert_eq!(disconnect_counts.len(), 1);
        assert_eq!(disconnect_counts[0].payload, MetricEventPayload::Count(1));

        let breakdowns_by_device_uptime = test_helper
            .get_logged_metrics(metrics::DISCONNECT_BREAKDOWN_BY_DEVICE_UPTIME_METRIC_ID);
        assert_eq!(breakdowns_by_device_uptime.len(), 1);
        assert_eq!(breakdowns_by_device_uptime[0].event_codes, vec![
            metrics::DisconnectBreakdownByDeviceUptimeMetricDimensionDeviceUptime::LessThan12Hours as u32,
        ]);
        assert_eq!(breakdowns_by_device_uptime[0].payload, MetricEventPayload::Count(1));

        let breakdowns_by_connected_duration = test_helper
            .get_logged_metrics(metrics::DISCONNECT_BREAKDOWN_BY_CONNECTED_DURATION_METRIC_ID);
        assert_eq!(breakdowns_by_connected_duration.len(), 1);
        assert_eq!(breakdowns_by_connected_duration[0].event_codes, vec![
            metrics::DisconnectBreakdownByConnectedDurationMetricDimensionConnectedDuration::LessThan6Hours as u32,
        ]);
        assert_eq!(breakdowns_by_connected_duration[0].payload, MetricEventPayload::Count(1));

        let breakdowns_by_reason =
            test_helper.get_logged_metrics(metrics::DISCONNECT_BREAKDOWN_BY_REASON_CODE_METRIC_ID);
        assert_eq!(breakdowns_by_reason.len(), 1);
        assert_eq!(
            breakdowns_by_reason[0].event_codes,
            vec![3u32, metrics::ConnectivityWlanMetricDimensionDisconnectSource::Mlme as u32,]
        );
        assert_eq!(breakdowns_by_reason[0].payload, MetricEventPayload::Count(1));

        let breakdowns_by_channel = test_helper
            .get_logged_metrics(metrics::DISCONNECT_BREAKDOWN_BY_PRIMARY_CHANNEL_METRIC_ID);
        assert_eq!(breakdowns_by_channel.len(), 1);
        assert_eq!(breakdowns_by_channel[0].event_codes, vec![channel.primary as u32]);
        assert_eq!(breakdowns_by_channel[0].payload, MetricEventPayload::Count(1));

        let breakdowns_by_channel_band =
            test_helper.get_logged_metrics(metrics::DISCONNECT_BREAKDOWN_BY_CHANNEL_BAND_METRIC_ID);
        assert_eq!(breakdowns_by_channel_band.len(), 1);
        assert_eq!(
            breakdowns_by_channel_band[0].event_codes,
            vec![
                metrics::DisconnectBreakdownByChannelBandMetricDimensionChannelBand::Band2Dot4Ghz
                    as u32
            ]
        );
        assert_eq!(breakdowns_by_channel_band[0].payload, MetricEventPayload::Count(1));

        let breakdowns_by_is_multi_bss =
            test_helper.get_logged_metrics(metrics::DISCONNECT_BREAKDOWN_BY_IS_MULTI_BSS_METRIC_ID);
        assert_eq!(breakdowns_by_is_multi_bss.len(), 1);
        assert_eq!(
            breakdowns_by_is_multi_bss[0].event_codes,
            vec![metrics::DisconnectBreakdownByIsMultiBssMetricDimensionIsMultiBss::Yes as u32]
        );
        assert_eq!(breakdowns_by_is_multi_bss[0].payload, MetricEventPayload::Count(1));

        let breakdowns_by_security_type = test_helper
            .get_logged_metrics(metrics::DISCONNECT_BREAKDOWN_BY_SECURITY_TYPE_METRIC_ID);
        assert_eq!(breakdowns_by_security_type.len(), 1);
        assert_eq!(
            breakdowns_by_security_type[0].event_codes,
            vec![
                metrics::DisconnectBreakdownBySecurityTypeMetricDimensionSecurityType::Wpa2Personal
                    as u32
            ]
        );
        assert_eq!(breakdowns_by_security_type[0].payload, MetricEventPayload::Count(1));

        // Check that non-roaming and total disconnects are logged but not a roaming disconnect.
        let roam_connected_duration =
            test_helper.get_logged_metrics(CONNECTED_DURATION_BEFORE_ROAMING_DISCONNECT_METRIC_ID);
        assert_eq!(roam_connected_duration.len(), 0);

        let non_roam_connected_duration = test_helper
            .get_logged_metrics(CONNECTED_DURATION_BEFORE_NON_ROAMING_DISCONNECT_METRIC_ID);
        assert_eq!(non_roam_connected_duration.len(), 1);
        assert_eq!(non_roam_connected_duration[0].payload, MetricEventPayload::IntegerValue(300));

        let total_connected_duration =
            test_helper.get_logged_metrics(metrics::CONNECTED_DURATION_BEFORE_DISCONNECT_METRIC_ID);
        assert_eq!(total_connected_duration.len(), 1);
        assert_eq!(total_connected_duration[0].payload, MetricEventPayload::IntegerValue(300));

        let user_network_change_counts =
            test_helper.get_logged_metrics(metrics::MANUAL_NETWORK_CHANGE_METRIC_ID);
        assert!(user_network_change_counts.is_empty());

        let roam_disconnect_counts =
            test_helper.get_logged_metrics(NETWORK_ROAMING_DISCONNECT_COUNTS_METRIC_ID);
        assert!(roam_disconnect_counts.is_empty());

        let non_roam_disconnect_counts =
            test_helper.get_logged_metrics(NETWORK_NON_ROAMING_DISCONNECT_COUNTS_METRIC_ID);
        assert_eq!(non_roam_disconnect_counts.len(), 1);
        assert_eq!(non_roam_disconnect_counts[0].payload, MetricEventPayload::Count(1));

        let total_disconnect_counts =
            test_helper.get_logged_metrics(metrics::NETWORK_DISCONNECT_COUNTS_METRIC_ID);
        assert_eq!(total_disconnect_counts.len(), 1);
        assert_eq!(total_disconnect_counts[0].payload, MetricEventPayload::Count(1));
    }

    #[fuchsia::test]
    fn test_log_roam_disconnect_cobalt_metrics() {
        let (mut test_helper, mut test_fut) = setup_test();
        test_helper.advance_by(zx::MonotonicDuration::from_hours(3), test_fut.as_mut());
        test_helper.send_connected_event(random_bss_description!(Wpa2));
        assert_eq!(test_helper.advance_test_fut(&mut test_fut), Poll::Pending);

        const DUR_MIN: i64 = 125;
        test_helper.advance_by(zx::MonotonicDuration::from_minutes(DUR_MIN), test_fut.as_mut());

        // Send a disconnect event.
        let info = DisconnectInfo {
            connected_duration: zx::MonotonicDuration::from_minutes(DUR_MIN),
            disconnect_source: fidl_sme::DisconnectSource::User(
                fidl_sme::UserDisconnectReason::ProactiveNetworkSwitch,
            ),
            ..fake_disconnect_info()
        };
        test_helper
            .telemetry_sender
            .send(TelemetryEvent::Disconnected { track_subsequent_downtime: true, info });
        test_helper.drain_cobalt_events(&mut test_fut);

        // Check that connected durations for roam and total disconnects are logged, and not for
        // a non-roaming disconnect.
        let roam_connected_duration =
            test_helper.get_logged_metrics(CONNECTED_DURATION_BEFORE_ROAMING_DISCONNECT_METRIC_ID);
        assert_eq!(roam_connected_duration.len(), 1);
        assert_eq!(roam_connected_duration[0].payload, MetricEventPayload::IntegerValue(DUR_MIN));

        let non_roam_connected_duration = test_helper
            .get_logged_metrics(CONNECTED_DURATION_BEFORE_NON_ROAMING_DISCONNECT_METRIC_ID);
        assert_eq!(non_roam_connected_duration.len(), 0);

        let total_connected_duration =
            test_helper.get_logged_metrics(metrics::CONNECTED_DURATION_BEFORE_DISCONNECT_METRIC_ID);
        assert_eq!(total_connected_duration.len(), 1);
        assert_eq!(total_connected_duration[0].payload, MetricEventPayload::IntegerValue(DUR_MIN));

        // Check that a metric count is not logged for a manual network switch.
        let user_network_change_counts =
            test_helper.get_logged_metrics(metrics::MANUAL_NETWORK_CHANGE_METRIC_ID);
        assert!(user_network_change_counts.is_empty());

        // Check that a roam and total disconnect is logged, and not a non-roaming disconnect.
        let roam_disconnect_counts =
            test_helper.get_logged_metrics(NETWORK_ROAMING_DISCONNECT_COUNTS_METRIC_ID);
        assert_eq!(roam_disconnect_counts.len(), 1);
        assert_eq!(roam_disconnect_counts[0].payload, MetricEventPayload::Count(1));

        let non_roam_disconnect_counts =
            test_helper.get_logged_metrics(NETWORK_NON_ROAMING_DISCONNECT_COUNTS_METRIC_ID);
        assert!(non_roam_disconnect_counts.is_empty());

        let total_disconnect_counts =
            test_helper.get_logged_metrics(metrics::NETWORK_DISCONNECT_COUNTS_METRIC_ID);
        assert_eq!(total_disconnect_counts.len(), 1);
        assert_eq!(total_disconnect_counts[0].payload, MetricEventPayload::Count(1));
    }

    #[fuchsia::test]
    fn test_log_user_disconnect_cobalt_metrics() {
        let (mut test_helper, mut test_fut) = setup_test();
        test_helper.advance_by(zx::MonotonicDuration::from_hours(3), test_fut.as_mut());
        test_helper.send_connected_event(random_bss_description!(Wpa2));
        assert_eq!(test_helper.advance_test_fut(&mut test_fut), Poll::Pending);

        const DUR_MIN: i64 = 250;
        test_helper.advance_by(zx::MonotonicDuration::from_minutes(DUR_MIN), test_fut.as_mut());

        // Send a disconnect event.
        let info = DisconnectInfo {
            connected_duration: zx::MonotonicDuration::from_minutes(DUR_MIN),
            disconnect_source: fidl_sme::DisconnectSource::User(
                fidl_sme::UserDisconnectReason::FidlConnectRequest,
            ),
            ..fake_disconnect_info()
        };
        test_helper
            .telemetry_sender
            .send(TelemetryEvent::Disconnected { track_subsequent_downtime: true, info });
        test_helper.drain_cobalt_events(&mut test_fut);

        // Check that a count was logged for a disconnect from a user requested network change.
        let user_network_change_counts =
            test_helper.get_logged_metrics(metrics::MANUAL_NETWORK_CHANGE_METRIC_ID);
        assert_eq!(user_network_change_counts.len(), 1);
        assert_eq!(user_network_change_counts[0].payload, MetricEventPayload::Count(1));

        // Check that nothing was logged for roaming and non-roaming disconnects.
        let roam_connected_duration =
            test_helper.get_logged_metrics(CONNECTED_DURATION_BEFORE_ROAMING_DISCONNECT_METRIC_ID);
        assert_eq!(roam_connected_duration.len(), 0);

        let non_roam_connected_duration = test_helper
            .get_logged_metrics(CONNECTED_DURATION_BEFORE_NON_ROAMING_DISCONNECT_METRIC_ID);
        assert_eq!(non_roam_connected_duration.len(), 0);

        let roam_disconnect_counts =
            test_helper.get_logged_metrics(NETWORK_ROAMING_DISCONNECT_COUNTS_METRIC_ID);
        assert!(roam_disconnect_counts.is_empty());

        let non_roam_disconnect_counts =
            test_helper.get_logged_metrics(NETWORK_NON_ROAMING_DISCONNECT_COUNTS_METRIC_ID);
        assert!(non_roam_disconnect_counts.is_empty());

        // Check that a connected duration and a count were logged for overall disconnects.
        let total_connected_duration =
            test_helper.get_logged_metrics(metrics::CONNECTED_DURATION_BEFORE_DISCONNECT_METRIC_ID);
        assert_eq!(total_connected_duration.len(), 1);
        assert_eq!(total_connected_duration[0].payload, MetricEventPayload::IntegerValue(DUR_MIN));

        let total_disconnect_counts =
            test_helper.get_logged_metrics(metrics::NETWORK_DISCONNECT_COUNTS_METRIC_ID);
        assert_eq!(total_disconnect_counts.len(), 1);
        assert_eq!(total_disconnect_counts[0].payload, MetricEventPayload::Count(1));
    }

    #[fuchsia::test]
    fn test_log_saved_networks_count() {
        let (mut test_helper, mut test_fut) = setup_test();

        let event = TelemetryEvent::SavedNetworkCount {
            saved_network_count: 4,
            config_count_per_saved_network: vec![1, 1],
        };
        test_helper.telemetry_sender.send(event);
        test_helper.drain_cobalt_events(&mut test_fut);

        let saved_networks_count =
            test_helper.get_logged_metrics(metrics::SAVED_NETWORKS_MIGRATED_METRIC_ID);
        assert_eq!(saved_networks_count.len(), 1);
        assert_eq!(
            saved_networks_count[0].event_codes,
            vec![metrics::SavedNetworksMigratedMetricDimensionSavedNetworks::TwoToFour as u32]
        );

        let config_count = test_helper
            .get_logged_metrics(metrics::SAVED_CONFIGURATIONS_FOR_SAVED_NETWORK_MIGRATED_METRIC_ID);
        assert_eq!(config_count.len(), 2);
        assert_eq!(
            config_count[0].event_codes,
            vec![metrics::SavedConfigurationsForSavedNetworkMigratedMetricDimensionSavedConfigurations::One as u32]
        );
        assert_eq!(
            config_count[1].event_codes,
            vec![metrics::SavedConfigurationsForSavedNetworkMigratedMetricDimensionSavedConfigurations::One as u32]
        );
    }

    #[fuchsia::test]
    fn test_log_network_selection_scan_interval() {
        let (mut test_helper, mut test_fut) = setup_test();

        let duration = zx::MonotonicDuration::from_seconds(rand::thread_rng().gen_range(0..100));

        let event = TelemetryEvent::NetworkSelectionScanInterval { time_since_last_scan: duration };
        test_helper.telemetry_sender.send(event);
        test_helper.drain_cobalt_events(&mut test_fut);

        let last_scan_age = test_helper
            .get_logged_metrics(metrics::LAST_SCAN_AGE_WHEN_SCAN_REQUESTED_MIGRATED_METRIC_ID);
        assert_eq!(last_scan_age.len(), 1);
        assert_eq!(
            last_scan_age[0].payload,
            fidl_fuchsia_metrics::MetricEventPayload::IntegerValue(duration.into_micros())
        );
    }

    #[fuchsia::test]
    fn test_log_connection_selection_scan_results() {
        let (mut test_helper, mut test_fut) = setup_test();

        let event = TelemetryEvent::ConnectionSelectionScanResults {
            saved_network_count: 4,
            saved_network_count_found_by_active_scan: 1,
            bss_count_per_saved_network: vec![10, 10],
        };
        test_helper.telemetry_sender.send(event);
        test_helper.drain_cobalt_events(&mut test_fut);

        let saved_networks_count =
            test_helper.get_logged_metrics(metrics::SCAN_RESULTS_RECEIVED_MIGRATED_METRIC_ID);
        assert_eq!(saved_networks_count.len(), 1);
        assert_eq!(
            saved_networks_count[0].event_codes,
            vec![
                metrics::ScanResultsReceivedMigratedMetricDimensionSavedNetworksCount::TwoToFour
                    as u32
            ]
        );

        let active_scanned_network = test_helper.get_logged_metrics(
            metrics::SAVED_NETWORK_IN_SCAN_RESULT_WITH_ACTIVE_SCAN_MIGRATED_METRIC_ID,
        );
        assert_eq!(active_scanned_network.len(), 1);
        assert_eq!(
            active_scanned_network[0].event_codes,
            vec![metrics::SavedNetworkInScanResultWithActiveScanMigratedMetricDimensionActiveScanSsidsObserved::One as u32]
        );

        let bss_count = test_helper
            .get_logged_metrics(metrics::SAVED_NETWORK_IN_SCAN_RESULT_MIGRATED_METRIC_ID);
        assert_eq!(bss_count.len(), 2);
        assert_eq!(
            bss_count[0].event_codes,
            vec![
                metrics::SavedNetworkInScanResultMigratedMetricDimensionBssCount::FiveToTen as u32
            ]
        );
        assert_eq!(
            bss_count[1].event_codes,
            vec![
                metrics::SavedNetworkInScanResultMigratedMetricDimensionBssCount::FiveToTen as u32
            ]
        );
    }

    #[fuchsia::test]
    fn test_log_establish_connection_cobalt_metrics() {
        let (mut test_helper, mut test_fut) = setup_test();

        let primary_channel = 8;
        let channel = Channel::new(primary_channel, Cbw::Cbw20);
        let ap_state = random_bss_description!(Wpa2,
            bssid: [0x00, 0xf6, 0x20, 0x03, 0x04, 0x05],
            channel: channel,
            rssi_dbm: -50,
            snr_db: 25,
        )
        .into();
        let event = TelemetryEvent::ConnectResult {
            iface_id: IFACE_ID,
            policy_connect_reason: Some(client::types::ConnectReason::FidlConnectRequest),
            result: fake_connect_result(fidl_ieee80211::StatusCode::Success),
            multiple_bss_candidates: true,
            ap_state,
            network_is_likely_hidden: true,
        };
        test_helper.telemetry_sender.send(event);
        test_helper.drain_cobalt_events(&mut test_fut);

        let policy_connect_reasons =
            test_helper.get_logged_metrics(metrics::POLICY_CONNECTION_ATTEMPT_MIGRATED_METRIC_ID);
        assert_eq!(policy_connect_reasons.len(), 1);
        assert_eq!(
            policy_connect_reasons[0].event_codes,
            vec![client::types::ConnectReason::FidlConnectRequest as u32]
        );
        assert_eq!(policy_connect_reasons[0].payload, MetricEventPayload::Count(1));

        let breakdowns_by_status_code = test_helper
            .get_logged_metrics(metrics::CONNECT_ATTEMPT_BREAKDOWN_BY_STATUS_CODE_METRIC_ID);
        assert_eq!(breakdowns_by_status_code.len(), 1);
        assert_eq!(
            breakdowns_by_status_code[0].event_codes,
            vec![fidl_ieee80211::StatusCode::Success as u32]
        );
        assert_eq!(breakdowns_by_status_code[0].payload, MetricEventPayload::Count(1));

        let breakdowns_by_user_wait_time = test_helper
            .get_logged_metrics(metrics::SUCCESSFUL_CONNECT_BREAKDOWN_BY_USER_WAIT_TIME_METRIC_ID);
        // TelemetryEvent::StartEstablishConnection is never sent, so connect start time is never
        // tracked, hence this metric is not logged.
        assert_eq!(breakdowns_by_user_wait_time.len(), 0);

        let breakdowns_by_is_multi_bss = test_helper
            .get_logged_metrics(metrics::SUCCESSFUL_CONNECT_BREAKDOWN_BY_IS_MULTI_BSS_METRIC_ID);
        assert_eq!(breakdowns_by_is_multi_bss.len(), 1);
        assert_eq!(
            breakdowns_by_is_multi_bss[0].event_codes,
            vec![
                metrics::SuccessfulConnectBreakdownByIsMultiBssMetricDimensionIsMultiBss::Yes
                    as u32
            ]
        );
        assert_eq!(breakdowns_by_is_multi_bss[0].payload, MetricEventPayload::Count(1));

        let breakdowns_by_security_type = test_helper
            .get_logged_metrics(metrics::SUCCESSFUL_CONNECT_BREAKDOWN_BY_SECURITY_TYPE_METRIC_ID);
        assert_eq!(breakdowns_by_security_type.len(), 1);
        assert_eq!(
            breakdowns_by_security_type[0].event_codes,
            vec![
                metrics::SuccessfulConnectBreakdownBySecurityTypeMetricDimensionSecurityType::Wpa2Personal
                    as u32
            ]
        );
        assert_eq!(breakdowns_by_security_type[0].payload, MetricEventPayload::Count(1));

        let breakdowns_by_channel = test_helper
            .get_logged_metrics(metrics::SUCCESSFUL_CONNECT_BREAKDOWN_BY_PRIMARY_CHANNEL_METRIC_ID);
        assert_eq!(breakdowns_by_channel.len(), 1);
        assert_eq!(breakdowns_by_channel[0].event_codes, vec![primary_channel as u32]);
        assert_eq!(breakdowns_by_channel[0].payload, MetricEventPayload::Count(1));

        let breakdowns_by_channel_band = test_helper
            .get_logged_metrics(metrics::SUCCESSFUL_CONNECT_BREAKDOWN_BY_CHANNEL_BAND_METRIC_ID);
        assert_eq!(breakdowns_by_channel_band.len(), 1);
        assert_eq!(breakdowns_by_channel_band[0].event_codes, vec![
            metrics::SuccessfulConnectBreakdownByChannelBandMetricDimensionChannelBand::Band2Dot4Ghz as u32
        ]);
        assert_eq!(breakdowns_by_channel_band[0].payload, MetricEventPayload::Count(1));

        let per_oui = test_helper.get_logged_metrics(metrics::SUCCESSFUL_CONNECT_PER_OUI_METRIC_ID);
        assert_eq!(per_oui.len(), 1);
        assert_eq!(per_oui[0].payload, MetricEventPayload::StringValue("00F620".to_string()));

        let fidl_connect_count =
            test_helper.get_logged_metrics(metrics::POLICY_CONNECTION_ATTEMPTS_METRIC_ID);
        assert_eq!(fidl_connect_count.len(), 1);
        assert_eq!(fidl_connect_count[0].payload, MetricEventPayload::Count(1));

        let network_is_likely_hidden =
            test_helper.get_logged_metrics(metrics::CONNECT_TO_LIKELY_HIDDEN_NETWORK_METRIC_ID);
        assert_eq!(network_is_likely_hidden.len(), 1);
        assert_eq!(network_is_likely_hidden[0].payload, MetricEventPayload::Count(1));
    }

    #[fuchsia::test]
    fn test_log_connect_attempt_breakdown_by_failed_status_code() {
        let (mut test_helper, mut test_fut) = setup_test();

        let event = TelemetryEvent::ConnectResult {
            iface_id: IFACE_ID,
            policy_connect_reason: None,
            result: fake_connect_result(fidl_ieee80211::StatusCode::RefusedCapabilitiesMismatch),
            multiple_bss_candidates: true,
            ap_state: random_bss_description!(Wpa2).into(),
            network_is_likely_hidden: true,
        };
        test_helper.telemetry_sender.send(event);
        test_helper.drain_cobalt_events(&mut test_fut);

        let breakdowns_by_status_code = test_helper
            .get_logged_metrics(metrics::CONNECT_ATTEMPT_BREAKDOWN_BY_STATUS_CODE_METRIC_ID);
        assert_eq!(breakdowns_by_status_code.len(), 1);
        assert_eq!(
            breakdowns_by_status_code[0].event_codes,
            vec![fidl_ieee80211::StatusCode::RefusedCapabilitiesMismatch as u32]
        );
    }

    #[fuchsia::test]
    fn test_log_establish_connection_status_code_cobalt_metrics_normal_device() {
        let (mut test_helper, mut test_fut) = setup_test();
        for _ in 0..3 {
            let event = TelemetryEvent::ConnectResult {
                iface_id: IFACE_ID,
                policy_connect_reason: Some(
                    client::types::ConnectReason::RetryAfterFailedConnectAttempt,
                ),
                result: fake_connect_result(fidl_ieee80211::StatusCode::RefusedReasonUnspecified),
                multiple_bss_candidates: true,
                ap_state: random_bss_description!(Wpa1).into(),
                network_is_likely_hidden: true,
            };
            test_helper.telemetry_sender.send(event);
        }
        test_helper.send_connected_event(random_bss_description!(Wpa2));
        test_helper.advance_by(zx::MonotonicDuration::from_hours(24), test_fut.as_mut());

        let status_codes = test_helper.get_logged_metrics(
            metrics::CONNECT_ATTEMPT_ON_NORMAL_DEVICE_BREAKDOWN_BY_STATUS_CODE_METRIC_ID,
        );
        assert_eq!(status_codes.len(), 2);
        assert_eq_cobalt_events(
            status_codes,
            vec![
                MetricEvent {
                    metric_id:
                        metrics::CONNECT_ATTEMPT_ON_NORMAL_DEVICE_BREAKDOWN_BY_STATUS_CODE_METRIC_ID,
                    event_codes: vec![fidl_ieee80211::StatusCode::Success as u32],
                    payload: MetricEventPayload::Count(1),
                },
                MetricEvent {
                    metric_id:
                        metrics::CONNECT_ATTEMPT_ON_NORMAL_DEVICE_BREAKDOWN_BY_STATUS_CODE_METRIC_ID,
                    event_codes: vec![fidl_ieee80211::StatusCode::RefusedReasonUnspecified as u32],
                    payload: MetricEventPayload::Count(3),
                },
            ],
        );
    }

    #[fuchsia::test]
    fn test_log_establish_connection_status_code_cobalt_metrics_bad_device() {
        let (mut test_helper, mut test_fut) = setup_test();
        for _ in 0..10 {
            let event = TelemetryEvent::ConnectResult {
                iface_id: IFACE_ID,
                policy_connect_reason: Some(
                    client::types::ConnectReason::RetryAfterFailedConnectAttempt,
                ),
                result: fake_connect_result(fidl_ieee80211::StatusCode::RefusedReasonUnspecified),
                multiple_bss_candidates: true,
                ap_state: random_bss_description!(Wpa1).into(),
                network_is_likely_hidden: true,
            };
            test_helper.telemetry_sender.send(event);
        }
        test_helper.send_connected_event(random_bss_description!(Wpa2));
        test_helper.advance_by(zx::MonotonicDuration::from_hours(24), test_fut.as_mut());

        let status_codes = test_helper.get_logged_metrics(
            metrics::CONNECT_ATTEMPT_ON_BAD_DEVICE_BREAKDOWN_BY_STATUS_CODE_METRIC_ID,
        );
        assert_eq!(status_codes.len(), 2);
        assert_eq_cobalt_events(
            status_codes,
            vec![
                MetricEvent {
                    metric_id:
                        metrics::CONNECT_ATTEMPT_ON_BAD_DEVICE_BREAKDOWN_BY_STATUS_CODE_METRIC_ID,
                    event_codes: vec![fidl_ieee80211::StatusCode::Success as u32],
                    payload: MetricEventPayload::Count(1),
                },
                MetricEvent {
                    metric_id:
                        metrics::CONNECT_ATTEMPT_ON_BAD_DEVICE_BREAKDOWN_BY_STATUS_CODE_METRIC_ID,
                    event_codes: vec![fidl_ieee80211::StatusCode::RefusedReasonUnspecified as u32],
                    payload: MetricEventPayload::Count(10),
                },
            ],
        );
    }

    #[fuchsia::test]
    fn test_log_establish_connection_cobalt_metrics_user_wait_time_tracked_no_reset() {
        let (mut test_helper, mut test_fut) = setup_test();

        test_helper
            .telemetry_sender
            .send(TelemetryEvent::StartEstablishConnection { reset_start_time: false });
        test_helper.advance_by(zx::MonotonicDuration::from_seconds(2), test_fut.as_mut());
        test_helper
            .telemetry_sender
            .send(TelemetryEvent::StartEstablishConnection { reset_start_time: false });
        test_helper.advance_by(zx::MonotonicDuration::from_seconds(4), test_fut.as_mut());
        test_helper.send_connected_event(random_bss_description!(Wpa2));
        test_helper.drain_cobalt_events(&mut test_fut);

        let breakdowns_by_user_wait_time = test_helper
            .get_logged_metrics(metrics::SUCCESSFUL_CONNECT_BREAKDOWN_BY_USER_WAIT_TIME_METRIC_ID);
        assert_eq!(breakdowns_by_user_wait_time.len(), 1);
        assert_eq!(
            breakdowns_by_user_wait_time[0].event_codes,
            // Both the 2 seconds and 4 seconds since the first StartEstablishConnection
            // should be counted.
            vec![metrics::ConnectivityWlanMetricDimensionWaitTime::LessThan8Seconds as u32]
        );
    }

    #[fuchsia::test]
    fn test_log_establish_connection_cobalt_metrics_user_wait_time_tracked_with_reset() {
        let (mut test_helper, mut test_fut) = setup_test();

        test_helper
            .telemetry_sender
            .send(TelemetryEvent::StartEstablishConnection { reset_start_time: false });
        test_helper.advance_by(zx::MonotonicDuration::from_seconds(2), test_fut.as_mut());
        test_helper
            .telemetry_sender
            .send(TelemetryEvent::StartEstablishConnection { reset_start_time: true });
        test_helper.advance_by(zx::MonotonicDuration::from_seconds(4), test_fut.as_mut());
        test_helper.send_connected_event(random_bss_description!(Wpa2));
        test_helper.drain_cobalt_events(&mut test_fut);

        let breakdowns_by_user_wait_time = test_helper
            .get_logged_metrics(metrics::SUCCESSFUL_CONNECT_BREAKDOWN_BY_USER_WAIT_TIME_METRIC_ID);
        assert_eq!(breakdowns_by_user_wait_time.len(), 1);
        assert_eq!(
            breakdowns_by_user_wait_time[0].event_codes,
            // Only the 4 seconds after the last StartEstablishConnection should be counted.
            vec![metrics::ConnectivityWlanMetricDimensionWaitTime::LessThan5Seconds as u32]
        );
    }

    #[fuchsia::test]
    fn test_log_establish_connection_cobalt_metrics_user_wait_time_tracked_with_clear() {
        let (mut test_helper, mut test_fut) = setup_test();

        test_helper
            .telemetry_sender
            .send(TelemetryEvent::StartEstablishConnection { reset_start_time: false });
        test_helper.advance_by(zx::MonotonicDuration::from_seconds(10), test_fut.as_mut());
        test_helper.telemetry_sender.send(TelemetryEvent::ClearEstablishConnectionStartTime);

        test_helper.advance_by(zx::MonotonicDuration::from_seconds(30), test_fut.as_mut());

        test_helper
            .telemetry_sender
            .send(TelemetryEvent::StartEstablishConnection { reset_start_time: false });
        test_helper.advance_by(zx::MonotonicDuration::from_seconds(2), test_fut.as_mut());
        test_helper.send_connected_event(random_bss_description!(Wpa2));
        test_helper.drain_cobalt_events(&mut test_fut);

        let breakdowns_by_user_wait_time = test_helper
            .get_logged_metrics(metrics::SUCCESSFUL_CONNECT_BREAKDOWN_BY_USER_WAIT_TIME_METRIC_ID);
        assert_eq!(breakdowns_by_user_wait_time.len(), 1);
        assert_eq!(
            breakdowns_by_user_wait_time[0].event_codes,
            // Only the 2 seconds after the last StartEstablishConnection should be counted.
            vec![metrics::ConnectivityWlanMetricDimensionWaitTime::LessThan3Seconds as u32]
        );
    }

    #[test_case(
        (true, random_bss_description!(Wpa2)),
        (false, random_bss_description!(Wpa2)),
        metrics::DAILY_CONNECT_SUCCESS_RATE_BREAKDOWN_BY_IS_MULTI_BSS_METRIC_ID,
        metrics::SuccessfulConnectBreakdownByIsMultiBssMetricDimensionIsMultiBss::Yes as u32,
        metrics::SuccessfulConnectBreakdownByIsMultiBssMetricDimensionIsMultiBss::No as u32;
        "breakdown_by_is_multi_bss"
    )]
    #[test_case(
        (false, random_bss_description!(Wpa1)),
        (false, random_bss_description!(Wpa2)),
        metrics::DAILY_CONNECT_SUCCESS_RATE_BREAKDOWN_BY_SECURITY_TYPE_METRIC_ID,
        metrics::SuccessfulConnectBreakdownBySecurityTypeMetricDimensionSecurityType::Wpa1 as u32,
        metrics::SuccessfulConnectBreakdownBySecurityTypeMetricDimensionSecurityType::Wpa2Personal as u32;
        "breakdown_by_security_type"
    )]
    #[test_case(
        (false, random_bss_description!(Wpa2, channel: Channel::new(6, Cbw::Cbw20))),
        (false, random_bss_description!(Wpa2, channel: Channel::new(157, Cbw::Cbw40))),
        metrics::DAILY_CONNECT_SUCCESS_RATE_BREAKDOWN_BY_PRIMARY_CHANNEL_METRIC_ID,
        6,
        157;
        "breakdown_by_primary_channel"
    )]
    #[test_case(
        (false, random_bss_description!(Wpa2, channel: Channel::new(6, Cbw::Cbw20))),
        (false, random_bss_description!(Wpa2, channel: Channel::new(157, Cbw::Cbw40))),
        metrics::DAILY_CONNECT_SUCCESS_RATE_BREAKDOWN_BY_CHANNEL_BAND_METRIC_ID,
        metrics::SuccessfulConnectBreakdownByChannelBandMetricDimensionChannelBand::Band2Dot4Ghz as u32,
        metrics::SuccessfulConnectBreakdownByChannelBandMetricDimensionChannelBand::Band5Ghz as u32;
        "breakdown_by_channel_band"
    )]
    #[test_case(
        (false, random_bss_description!(Wpa2, rssi_dbm: -79)),
        (false, random_bss_description!(Wpa2, rssi_dbm: -40)),
        metrics::DAILY_CONNECT_SUCCESS_RATE_BREAKDOWN_BY_RSSI_BUCKET_METRIC_ID,
        metrics::ConnectivityWlanMetricDimensionRssiBucket::From79To77 as u32,
        metrics::ConnectivityWlanMetricDimensionRssiBucket::From50To35 as u32;
        "breakdown_by_rssi_bucket"
    )]
    #[test_case(
        (false, random_bss_description!(Wpa2, snr_db: 11)),
        (false, random_bss_description!(Wpa2, snr_db: 35)),
        metrics::DAILY_CONNECT_SUCCESS_RATE_BREAKDOWN_BY_SNR_BUCKET_METRIC_ID,
        metrics::ConnectivityWlanMetricDimensionSnrBucket::From11To15 as u32,
        metrics::ConnectivityWlanMetricDimensionSnrBucket::From26To40 as u32;
        "breakdown_by_snr_bucket"
    )]
    #[fuchsia::test(add_test_attr = false)]
    fn test_log_daily_connect_success_rate_breakdown_cobalt_metrics(
        first_connect_result_params: (bool, BssDescription),
        second_connect_result_params: (bool, BssDescription),
        metric_id: u32,
        event_code_1: u32,
        event_code_2: u32,
    ) {
        let (mut test_helper, mut test_fut) = setup_test();

        for i in 0..3 {
            let code = if i == 0 {
                fidl_ieee80211::StatusCode::Success
            } else {
                fidl_ieee80211::StatusCode::RefusedReasonUnspecified
            };
            let event = TelemetryEvent::ConnectResult {
                iface_id: IFACE_ID,
                policy_connect_reason: Some(
                    client::types::ConnectReason::RetryAfterFailedConnectAttempt,
                ),
                result: fake_connect_result(code),
                multiple_bss_candidates: first_connect_result_params.0,
                ap_state: first_connect_result_params.1.clone().into(),
                network_is_likely_hidden: true,
            };
            test_helper.telemetry_sender.send(event);
        }
        for i in 0..2 {
            let code = if i == 0 {
                fidl_ieee80211::StatusCode::Success
            } else {
                fidl_ieee80211::StatusCode::RefusedReasonUnspecified
            };
            let event = TelemetryEvent::ConnectResult {
                iface_id: IFACE_ID,
                policy_connect_reason: Some(
                    client::types::ConnectReason::RetryAfterFailedConnectAttempt,
                ),
                result: fake_connect_result(code),
                multiple_bss_candidates: second_connect_result_params.0,
                ap_state: second_connect_result_params.1.clone().into(),
                network_is_likely_hidden: true,
            };
            test_helper.telemetry_sender.send(event);
        }

        test_helper.advance_by(zx::MonotonicDuration::from_hours(24), test_fut.as_mut());

        let metrics = test_helper.get_logged_metrics(metric_id);
        assert_eq!(metrics.len(), 2);
        assert_eq_cobalt_events(
            metrics,
            vec![
                MetricEvent {
                    metric_id,
                    event_codes: vec![event_code_1],
                    payload: MetricEventPayload::IntegerValue(3333), // 1/3 = 33.33%
                },
                MetricEvent {
                    metric_id,
                    event_codes: vec![event_code_2],
                    payload: MetricEventPayload::IntegerValue(5000), // 1/2 = 50.00%
                },
            ],
        );
    }

    #[fuchsia::test]
    fn test_log_establish_connection_cobalt_metrics_user_wait_time_tracked_while_connected() {
        let (mut test_helper, mut test_fut) = setup_test();
        test_helper.send_connected_event(random_bss_description!(Wpa2));
        test_helper.drain_cobalt_events(&mut test_fut);
        test_helper.cobalt_events.clear();

        test_helper
            .telemetry_sender
            .send(TelemetryEvent::StartEstablishConnection { reset_start_time: true });
        test_helper.advance_by(zx::MonotonicDuration::from_seconds(2), test_fut.as_mut());
        let info = fake_disconnect_info();
        test_helper
            .telemetry_sender
            .send(TelemetryEvent::Disconnected { track_subsequent_downtime: false, info });
        test_helper.advance_by(zx::MonotonicDuration::from_seconds(4), test_fut.as_mut());
        test_helper.send_connected_event(random_bss_description!(Wpa2));
        test_helper.drain_cobalt_events(&mut test_fut);

        let breakdowns_by_user_wait_time = test_helper
            .get_logged_metrics(metrics::SUCCESSFUL_CONNECT_BREAKDOWN_BY_USER_WAIT_TIME_METRIC_ID);
        assert_eq!(breakdowns_by_user_wait_time.len(), 1);
        assert_eq!(
            breakdowns_by_user_wait_time[0].event_codes,
            // Both the 2 seconds and 4 seconds since the first StartEstablishConnection
            // should be counted.
            vec![metrics::ConnectivityWlanMetricDimensionWaitTime::LessThan8Seconds as u32]
        );
    }

    #[fuchsia::test]
    fn test_log_establish_connection_cobalt_metrics_user_wait_time_tracked_with_clear_while_connected(
    ) {
        let (mut test_helper, mut test_fut) = setup_test();
        test_helper.send_connected_event(random_bss_description!(Wpa2));
        test_helper.drain_cobalt_events(&mut test_fut);
        test_helper.cobalt_events.clear();

        test_helper
            .telemetry_sender
            .send(TelemetryEvent::StartEstablishConnection { reset_start_time: true });
        test_helper.telemetry_sender.send(TelemetryEvent::ClearEstablishConnectionStartTime);
        let info = fake_disconnect_info();
        test_helper
            .telemetry_sender
            .send(TelemetryEvent::Disconnected { track_subsequent_downtime: false, info });
        test_helper.advance_by(zx::MonotonicDuration::from_seconds(2), test_fut.as_mut());
        test_helper
            .telemetry_sender
            .send(TelemetryEvent::StartEstablishConnection { reset_start_time: false });
        test_helper.advance_by(zx::MonotonicDuration::from_seconds(4), test_fut.as_mut());
        test_helper.send_connected_event(random_bss_description!(Wpa2));
        test_helper.drain_cobalt_events(&mut test_fut);

        let breakdowns_by_user_wait_time = test_helper
            .get_logged_metrics(metrics::SUCCESSFUL_CONNECT_BREAKDOWN_BY_USER_WAIT_TIME_METRIC_ID);
        assert_eq!(breakdowns_by_user_wait_time.len(), 1);
        assert_eq!(
            breakdowns_by_user_wait_time[0].event_codes,
            // Only the 4 seconds after the last StartEstablishConnection should be counted.
            vec![metrics::ConnectivityWlanMetricDimensionWaitTime::LessThan5Seconds as u32]
        );
    }

    #[fuchsia::test]
    fn test_log_establish_connection_cobalt_metrics_user_wait_time_logged_for_sme_reconnecting() {
        let (mut test_helper, mut test_fut) = setup_test();
        test_helper.send_connected_event(random_bss_description!(Wpa2));
        test_helper.drain_cobalt_events(&mut test_fut);
        test_helper.cobalt_events.clear();

        let info = DisconnectInfo { is_sme_reconnecting: true, ..fake_disconnect_info() };
        test_helper
            .telemetry_sender
            .send(TelemetryEvent::Disconnected { track_subsequent_downtime: false, info });
        test_helper.advance_by(zx::MonotonicDuration::from_seconds(2), test_fut.as_mut());
        test_helper.send_connected_event(random_bss_description!(Wpa2));
        test_helper.drain_cobalt_events(&mut test_fut);

        let breakdowns_by_user_wait_time = test_helper
            .get_logged_metrics(metrics::SUCCESSFUL_CONNECT_BREAKDOWN_BY_USER_WAIT_TIME_METRIC_ID);
        assert_eq!(breakdowns_by_user_wait_time.len(), 1);
        assert_eq!(
            breakdowns_by_user_wait_time[0].event_codes,
            vec![metrics::ConnectivityWlanMetricDimensionWaitTime::LessThan3Seconds as u32]
        );
    }

    #[fuchsia::test]
    fn test_log_downtime_cobalt_metrics() {
        let (mut test_helper, mut test_fut) = setup_test();
        test_helper.send_connected_event(random_bss_description!(Wpa2));
        test_helper.drain_cobalt_events(&mut test_fut);

        let info = DisconnectInfo {
            disconnect_source: fidl_sme::DisconnectSource::Mlme(fidl_sme::DisconnectCause {
                reason_code: fidl_ieee80211::ReasonCode::LeavingNetworkDeauth,
                mlme_event_name: fidl_sme::DisconnectMlmeEventName::DeauthenticateIndication,
            }),
            ..fake_disconnect_info()
        };
        test_helper
            .telemetry_sender
            .send(TelemetryEvent::Disconnected { track_subsequent_downtime: true, info });
        assert_eq!(test_helper.advance_test_fut(&mut test_fut), Poll::Pending);

        test_helper.advance_by(zx::MonotonicDuration::from_minutes(42), test_fut.as_mut());
        // Indicate that there's no saved neighbor in vicinity
        test_helper.telemetry_sender.send(TelemetryEvent::NetworkSelectionDecision {
            network_selection_type: NetworkSelectionType::Undirected,
            num_candidates: Ok(0),
            selected_count: 0,
        });
        assert_eq!(test_helper.advance_test_fut(&mut test_fut), Poll::Pending);

        test_helper.advance_by(zx::MonotonicDuration::from_minutes(5), test_fut.as_mut());
        // Indicate that there's some saved neighbor in vicinity
        test_helper.telemetry_sender.send(TelemetryEvent::NetworkSelectionDecision {
            network_selection_type: NetworkSelectionType::Undirected,
            num_candidates: Ok(5),
            selected_count: 1,
        });
        assert_eq!(test_helper.advance_test_fut(&mut test_fut), Poll::Pending);

        test_helper.advance_by(zx::MonotonicDuration::from_minutes(7), test_fut.as_mut());
        // Reconnect
        test_helper.send_connected_event(random_bss_description!(Wpa2));
        test_helper.drain_cobalt_events(&mut test_fut);

        let breakdowns_by_reason = test_helper
            .get_logged_metrics(metrics::DOWNTIME_BREAKDOWN_BY_DISCONNECT_REASON_METRIC_ID);
        assert_eq!(breakdowns_by_reason.len(), 1);
        assert_eq!(
            breakdowns_by_reason[0].event_codes,
            vec![3u32, metrics::ConnectivityWlanMetricDimensionDisconnectSource::Mlme as u32,]
        );
        assert_eq!(
            breakdowns_by_reason[0].payload,
            MetricEventPayload::IntegerValue(zx::MonotonicDuration::from_minutes(49).into_micros())
        );
    }

    #[fuchsia::test]
    fn test_log_reconnect_cobalt_metrics() {
        let (mut test_helper, mut test_fut) = setup_test();
        test_helper.send_connected_event(random_bss_description!(Wpa2));
        test_helper.drain_cobalt_events(&mut test_fut);

        let info = DisconnectInfo {
            disconnect_source: fidl_sme::DisconnectSource::Mlme(fidl_sme::DisconnectCause {
                reason_code: fidl_ieee80211::ReasonCode::LeavingNetworkDeauth,
                mlme_event_name: fidl_sme::DisconnectMlmeEventName::DeauthenticateIndication,
            }),
            ..fake_disconnect_info()
        };
        test_helper
            .telemetry_sender
            .send(TelemetryEvent::Disconnected { track_subsequent_downtime: true, info });
        assert_eq!(test_helper.advance_test_fut(&mut test_fut), Poll::Pending);

        test_helper.advance_by(zx::MonotonicDuration::from_seconds(3), test_fut.as_mut());
        // Reconnect
        test_helper.send_connected_event(random_bss_description!(Wpa2));
        test_helper.drain_cobalt_events(&mut test_fut);

        let metrics =
            test_helper.get_logged_metrics(metrics::RECONNECT_BREAKDOWN_BY_DURATION_METRIC_ID);
        assert_eq!(metrics.len(), 1);
        assert_eq!(
            metrics[0].event_codes,
            vec![
                metrics::ConnectivityWlanMetricDimensionReconnectDuration::LessThan5Seconds as u32
            ]
        );
        assert_eq!(metrics[0].payload, MetricEventPayload::Count(1));

        // Verify the reconnect duration was logged for an unexpected disconnect and not a roam.
        // 3 seconds would be sent as 3,000,000 microseconds.
        let metrics = test_helper.get_logged_metrics(NON_ROAMING_RECONNECT_DURATION_METRIC_ID);
        assert_eq!(metrics.len(), 1);
        assert_eq!(metrics[0].payload, MetricEventPayload::IntegerValue(3_000_000));
        assert_eq!(test_helper.get_logged_metrics(ROAMING_RECONNECT_DURATION_METRIC_ID).len(), 0);

        // Send a disconnect and reconnect for a proactive network switch.
        test_helper.clear_cobalt_events();
        let info = DisconnectInfo {
            disconnect_source: fidl_sme::DisconnectSource::User(
                fidl_sme::UserDisconnectReason::ProactiveNetworkSwitch,
            ),
            ..fake_disconnect_info()
        };
        test_helper
            .telemetry_sender
            .send(TelemetryEvent::Disconnected { track_subsequent_downtime: true, info });
        assert_eq!(test_helper.advance_test_fut(&mut test_fut), Poll::Pending);
        let downtime = 5_000_000;
        test_helper.advance_by(zx::MonotonicDuration::from_micros(downtime), test_fut.as_mut());

        // Reconnect and verify that a roam reconnect time is logged and a non-roam reconnect time
        // is not logged.
        test_helper.send_connected_event(random_bss_description!(Wpa2));
        test_helper.drain_cobalt_events(&mut test_fut);
        let roam_reconnect = test_helper.get_logged_metrics(ROAMING_RECONNECT_DURATION_METRIC_ID);
        assert_eq!(roam_reconnect.len(), 1);
        assert_eq!(roam_reconnect[0].payload, MetricEventPayload::IntegerValue(downtime));
        let non_roam_reconnect =
            test_helper.get_logged_metrics(NON_ROAMING_RECONNECT_DURATION_METRIC_ID);
        assert_eq!(non_roam_reconnect.len(), 0);
    }

    #[fuchsia::test]
    fn test_log_device_connected_cobalt_metrics() {
        let (mut test_helper, mut test_fut) = setup_test();

        let wmm_info = vec![0x80]; // U-APSD enabled
        #[rustfmt::skip]
        let rm_enabled_capabilities = vec![
            0x03, // link measurement and neighbor report enabled
            0x00, 0x00, 0x00, 0x00,
        ];
        #[rustfmt::skip]
        let ext_capabilities = vec![
            0x04, 0x00,
            0x08, // BSS transition supported
            0x00, 0x00, 0x00, 0x00, 0x40
        ];
        let bss_description = random_bss_description!(Wpa2,
            channel: Channel::new(157, Cbw::Cbw40),
            ies_overrides: IesOverrides::new()
                .remove(IeType::WMM_PARAM)
                .set(IeType::WMM_INFO, wmm_info)
                .set(IeType::RM_ENABLED_CAPABILITIES, rm_enabled_capabilities)
                .set(IeType::MOBILITY_DOMAIN, vec![0x00; 3])
                .set(IeType::EXT_CAPABILITIES, ext_capabilities),
            bssid: [0x00, 0xf6, 0x20, 0x03, 0x04, 0x05],
        );
        test_helper.send_connected_event(bss_description);
        test_helper.drain_cobalt_events(&mut test_fut);

        let num_devices_connected =
            test_helper.get_logged_metrics(metrics::NUMBER_OF_CONNECTED_DEVICES_METRIC_ID);
        assert_eq!(num_devices_connected.len(), 1);
        assert_eq!(num_devices_connected[0].payload, MetricEventPayload::Count(1));

        let connected_security_type =
            test_helper.get_logged_metrics(metrics::CONNECTED_NETWORK_SECURITY_TYPE_METRIC_ID);
        assert_eq!(connected_security_type.len(), 1);
        assert_eq!(
            connected_security_type[0].event_codes,
            vec![
                metrics::ConnectedNetworkSecurityTypeMetricDimensionSecurityType::Wpa2Personal
                    as u32
            ]
        );
        assert_eq!(connected_security_type[0].payload, MetricEventPayload::Count(1));

        let connected_apsd = test_helper
            .get_logged_metrics(metrics::DEVICE_CONNECTED_TO_AP_THAT_SUPPORTS_APSD_METRIC_ID);
        assert_eq!(connected_apsd.len(), 1);
        assert_eq!(connected_apsd[0].payload, MetricEventPayload::Count(1));

        let connected_link_measurement = test_helper.get_logged_metrics(
            metrics::DEVICE_CONNECTED_TO_AP_THAT_SUPPORTS_LINK_MEASUREMENT_METRIC_ID,
        );
        assert_eq!(connected_link_measurement.len(), 1);
        assert_eq!(connected_link_measurement[0].payload, MetricEventPayload::Count(1));

        let connected_neighbor_report = test_helper.get_logged_metrics(
            metrics::DEVICE_CONNECTED_TO_AP_THAT_SUPPORTS_NEIGHBOR_REPORT_METRIC_ID,
        );
        assert_eq!(connected_neighbor_report.len(), 1);
        assert_eq!(connected_neighbor_report[0].payload, MetricEventPayload::Count(1));

        let connected_ft = test_helper
            .get_logged_metrics(metrics::DEVICE_CONNECTED_TO_AP_THAT_SUPPORTS_FT_METRIC_ID);
        assert_eq!(connected_ft.len(), 1);
        assert_eq!(connected_ft[0].payload, MetricEventPayload::Count(1));

        let connected_bss_transition_mgmt = test_helper.get_logged_metrics(
            metrics::DEVICE_CONNECTED_TO_AP_THAT_SUPPORTS_BSS_TRANSITION_MANAGEMENT_METRIC_ID,
        );
        assert_eq!(connected_bss_transition_mgmt.len(), 1);
        assert_eq!(connected_bss_transition_mgmt[0].payload, MetricEventPayload::Count(1));

        let breakdown_by_is_multi_bss = test_helper.get_logged_metrics(
            metrics::DEVICE_CONNECTED_TO_AP_BREAKDOWN_BY_IS_MULTI_BSS_METRIC_ID,
        );
        assert_eq!(breakdown_by_is_multi_bss.len(), 1);
        assert_eq!(
            breakdown_by_is_multi_bss[0].event_codes,
            vec![
                metrics::SuccessfulConnectBreakdownByIsMultiBssMetricDimensionIsMultiBss::Yes
                    as u32
            ]
        );
        assert_eq!(breakdown_by_is_multi_bss[0].payload, MetricEventPayload::Count(1));

        let breakdown_by_primary_channel = test_helper.get_logged_metrics(
            metrics::DEVICE_CONNECTED_TO_AP_BREAKDOWN_BY_PRIMARY_CHANNEL_METRIC_ID,
        );
        assert_eq!(breakdown_by_primary_channel.len(), 1);
        assert_eq!(breakdown_by_primary_channel[0].event_codes, vec![157]);
        assert_eq!(breakdown_by_primary_channel[0].payload, MetricEventPayload::Count(1));

        let breakdown_by_channel_band = test_helper.get_logged_metrics(
            metrics::DEVICE_CONNECTED_TO_AP_BREAKDOWN_BY_CHANNEL_BAND_METRIC_ID,
        );
        assert_eq!(breakdown_by_channel_band.len(), 1);
        assert_eq!(
            breakdown_by_channel_band[0].event_codes,
            vec![
                metrics::SuccessfulConnectBreakdownByChannelBandMetricDimensionChannelBand::Band5Ghz
                    as u32
            ]
        );
        assert_eq!(breakdown_by_channel_band[0].payload, MetricEventPayload::Count(1));

        let ap_oui_connected =
            test_helper.get_logged_metrics(metrics::DEVICE_CONNECTED_TO_AP_OUI_2_METRIC_ID);
        assert_eq!(ap_oui_connected.len(), 1);
        assert_eq!(
            ap_oui_connected[0].payload,
            MetricEventPayload::StringValue("00F620".to_string())
        );

        let network_is_likely_hidden =
            test_helper.get_logged_metrics(metrics::CONNECT_TO_LIKELY_HIDDEN_NETWORK_METRIC_ID);
        assert_eq!(network_is_likely_hidden.len(), 1);
        assert_eq!(network_is_likely_hidden[0].payload, MetricEventPayload::Count(1));
    }

    #[fuchsia::test]
    fn test_log_device_connected_cobalt_metrics_ap_features_not_supported() {
        let (mut test_helper, mut test_fut) = setup_test();

        let bss_description = random_bss_description!(Wpa2,
            ies_overrides: IesOverrides::new()
                .remove(IeType::WMM_PARAM)
                .remove(IeType::WMM_INFO)
                .remove(IeType::RM_ENABLED_CAPABILITIES)
                .remove(IeType::MOBILITY_DOMAIN)
                .remove(IeType::EXT_CAPABILITIES)
        );
        test_helper.send_connected_event(bss_description);
        test_helper.drain_cobalt_events(&mut test_fut);

        let connected_apsd = test_helper
            .get_logged_metrics(metrics::DEVICE_CONNECTED_TO_AP_THAT_SUPPORTS_APSD_METRIC_ID);
        assert_eq!(connected_apsd.len(), 0);

        let connected_link_measurement = test_helper.get_logged_metrics(
            metrics::DEVICE_CONNECTED_TO_AP_THAT_SUPPORTS_LINK_MEASUREMENT_METRIC_ID,
        );
        assert_eq!(connected_link_measurement.len(), 0);

        let connected_neighbor_report = test_helper.get_logged_metrics(
            metrics::DEVICE_CONNECTED_TO_AP_THAT_SUPPORTS_NEIGHBOR_REPORT_METRIC_ID,
        );
        assert_eq!(connected_neighbor_report.len(), 0);

        let connected_ft = test_helper
            .get_logged_metrics(metrics::DEVICE_CONNECTED_TO_AP_THAT_SUPPORTS_FT_METRIC_ID);
        assert_eq!(connected_ft.len(), 0);

        let connected_bss_transition_mgmt = test_helper.get_logged_metrics(
            metrics::DEVICE_CONNECTED_TO_AP_THAT_SUPPORTS_BSS_TRANSITION_MANAGEMENT_METRIC_ID,
        );
        assert_eq!(connected_bss_transition_mgmt.len(), 0);
    }

    #[test_case(metrics::CONNECT_TO_LIKELY_HIDDEN_NETWORK_METRIC_ID, None; "connect_to_likely_hidden_network")]
    #[test_case(metrics::NUMBER_OF_CONNECTED_DEVICES_METRIC_ID, None; "number_of_connected_devices")]
    #[test_case(metrics::CONNECTED_NETWORK_SECURITY_TYPE_METRIC_ID, None; "breakdown_by_security_type")]
    #[test_case(metrics::DEVICE_CONNECTED_TO_AP_BREAKDOWN_BY_IS_MULTI_BSS_METRIC_ID, None; "breakdown_by_is_multi_bss")]
    #[test_case(metrics::DEVICE_CONNECTED_TO_AP_BREAKDOWN_BY_PRIMARY_CHANNEL_METRIC_ID, None; "breakdown_by_primary_channel")]
    #[test_case(metrics::DEVICE_CONNECTED_TO_AP_BREAKDOWN_BY_CHANNEL_BAND_METRIC_ID, None; "breakdown_by_channel_band")]
    #[test_case(metrics::DEVICE_CONNECTED_TO_AP_OUI_2_METRIC_ID,
        Some(vec![
            MetricEvent {
                metric_id: metrics::DEVICE_CONNECTED_TO_AP_OUI_2_METRIC_ID,
                event_codes: vec![],
                payload: MetricEventPayload::StringValue("00F620".to_string()),
            },
        ]); "number_of_devices_connected_to_specific_oui")]
    #[fuchsia::test(add_test_attr = false)]
    fn test_log_device_connected_cobalt_metrics_on_disconnect_and_periodically(
        metric_id: u32,
        payload: Option<Vec<MetricEvent>>,
    ) {
        let (mut test_helper, mut test_fut) = setup_test();

        let bss_description = random_bss_description!(Wpa2,
            bssid: [0x00, 0xf6, 0x20, 0x03, 0x04, 0x05],
        );
        test_helper.send_connected_event(bss_description);
        test_helper.drain_cobalt_events(&mut test_fut);
        test_helper.cobalt_events.clear();

        test_helper.advance_by(zx::MonotonicDuration::from_hours(24), test_fut.as_mut());

        // Verify that after 24 hours has passed, metric is logged at least once because
        // device is still connected
        let metrics = test_helper.get_logged_metrics(metric_id);
        assert!(!metrics.is_empty());

        if let Some(payload) = payload {
            assert_eq_cobalt_events(metrics, payload)
        }

        test_helper.cobalt_events.clear();

        let info = fake_disconnect_info();
        test_helper
            .telemetry_sender
            .send(TelemetryEvent::Disconnected { track_subsequent_downtime: false, info });
        test_helper.drain_cobalt_events(&mut test_fut);

        // Verify that on disconnect, device connected metric is also logged.
        let metrics = test_helper.get_logged_metrics(metric_id);
        assert_eq!(metrics.len(), 1);
    }

    #[fuchsia::test]
    fn test_log_device_connected_cobalt_metrics_on_channel_switched() {
        let (mut test_helper, mut test_fut) = setup_test();
        let bss_description = random_bss_description!(Wpa2,
            channel: Channel::new(4, Cbw::Cbw20),
        );
        test_helper.send_connected_event(bss_description);
        test_helper.drain_cobalt_events(&mut test_fut);

        let breakdown_by_primary_channel = test_helper.get_logged_metrics(
            metrics::DEVICE_CONNECTED_TO_AP_BREAKDOWN_BY_PRIMARY_CHANNEL_METRIC_ID,
        );
        assert_eq!(breakdown_by_primary_channel.len(), 1);
        assert_eq!(breakdown_by_primary_channel[0].event_codes, vec![4]);
        assert_eq!(breakdown_by_primary_channel[0].payload, MetricEventPayload::Count(1));

        let breakdown_by_channel_band = test_helper.get_logged_metrics(
            metrics::DEVICE_CONNECTED_TO_AP_BREAKDOWN_BY_CHANNEL_BAND_METRIC_ID,
        );
        assert_eq!(breakdown_by_channel_band.len(), 1);
        assert_eq!(
            breakdown_by_channel_band[0].event_codes,
            vec![
                metrics::SuccessfulConnectBreakdownByChannelBandMetricDimensionChannelBand::Band2Dot4Ghz
                    as u32
            ]
        );
        assert_eq!(breakdown_by_channel_band[0].payload, MetricEventPayload::Count(1));

        // Clear out existing Cobalt metrics
        test_helper.cobalt_events.clear();

        test_helper.telemetry_sender.send(TelemetryEvent::OnChannelSwitched {
            info: fidl_internal::ChannelSwitchInfo { new_channel: 157 },
        });
        test_helper.drain_cobalt_events(&mut test_fut);

        // On channel switched, device connected metrics for the new channel and channel band
        // are logged.
        let breakdown_by_primary_channel = test_helper.get_logged_metrics(
            metrics::DEVICE_CONNECTED_TO_AP_BREAKDOWN_BY_PRIMARY_CHANNEL_METRIC_ID,
        );
        assert_eq!(breakdown_by_primary_channel.len(), 1);
        assert_eq!(breakdown_by_primary_channel[0].event_codes, vec![157]);
        assert_eq!(breakdown_by_primary_channel[0].payload, MetricEventPayload::Count(1));

        let breakdown_by_channel_band = test_helper.get_logged_metrics(
            metrics::DEVICE_CONNECTED_TO_AP_BREAKDOWN_BY_CHANNEL_BAND_METRIC_ID,
        );
        assert_eq!(breakdown_by_channel_band.len(), 1);
        assert_eq!(
            breakdown_by_channel_band[0].event_codes,
            vec![
                metrics::SuccessfulConnectBreakdownByChannelBandMetricDimensionChannelBand::Band5Ghz
                    as u32
            ]
        );
        assert_eq!(breakdown_by_channel_band[0].payload, MetricEventPayload::Count(1));
    }

    #[fuchsia::test]
    fn test_active_scan_requested_metric() {
        let (mut test_helper, mut test_fut) = setup_test();

        test_helper
            .telemetry_sender
            .send(TelemetryEvent::ActiveScanRequested { num_ssids_requested: 4 });

        test_helper.drain_cobalt_events(&mut test_fut);
        let metrics = test_helper.get_logged_metrics(
            metrics::ACTIVE_SCAN_REQUESTED_FOR_NETWORK_SELECTION_MIGRATED_METRIC_ID,
        );
        assert_eq!(metrics.len(), 1);
        assert_eq!(metrics[0].event_codes, vec![metrics::ActiveScanRequestedForNetworkSelectionMigratedMetricDimensionActiveScanSsidsRequested::TwoToFour as u32]);
        assert_eq!(metrics[0].payload, MetricEventPayload::Count(1));
    }

    #[fuchsia::test]
    fn test_log_device_performed_roaming_scan() {
        let (mut test_helper, mut test_fut) = setup_test();

        // Send a roaming scan event
        test_helper.telemetry_sender.send(TelemetryEvent::RoamingScan);
        test_helper.drain_cobalt_events(&mut test_fut);

        // Check that the event was logged to cobalt.
        let metrics =
            test_helper.get_logged_metrics(POLICY_PROACTIVE_ROAMING_SCAN_COUNTS_METRIC_ID);
        assert_eq!(metrics.len(), 1);
        assert_eq!(metrics[0].payload, MetricEventPayload::Count(1));
    }

    #[fuchsia::test]
    fn test_connection_enabled_duration_metric() {
        let (mut test_helper, mut test_fut) = setup_test();

        test_helper.telemetry_sender.send(TelemetryEvent::StartClientConnectionsRequest);
        assert_eq!(test_helper.advance_test_fut(&mut test_fut), Poll::Pending);
        test_helper.advance_by(zx::MonotonicDuration::from_seconds(10), test_fut.as_mut());
        test_helper.telemetry_sender.send(TelemetryEvent::StopClientConnectionsRequest);

        test_helper.drain_cobalt_events(&mut test_fut);
        let metrics = test_helper
            .get_logged_metrics(metrics::CLIENT_CONNECTIONS_ENABLED_DURATION_MIGRATED_METRIC_ID);
        assert_eq!(metrics.len(), 1);
        assert_eq!(
            metrics[0].payload,
            MetricEventPayload::IntegerValue(zx::MonotonicDuration::from_seconds(10).into_micros())
        );
    }

    #[fuchsia::test]
    fn test_restart_metric_start_client_connections_request_sent_first() {
        let (mut test_helper, mut test_fut) = setup_test();

        // Send a start client connections event and then a stop and start corresponding to a
        // restart. The first start client connections should not count for the metric.
        test_helper.telemetry_sender.send(TelemetryEvent::StartClientConnectionsRequest);
        test_helper.advance_by(zx::MonotonicDuration::from_seconds(2), test_fut.as_mut());
        test_helper.telemetry_sender.send(TelemetryEvent::StopClientConnectionsRequest);
        test_helper.advance_by(zx::MonotonicDuration::from_seconds(1), test_fut.as_mut());
        test_helper.telemetry_sender.send(TelemetryEvent::StartClientConnectionsRequest);

        // Check that exactly 1 restart client connections event was logged to cobalt.
        test_helper.drain_cobalt_events(&mut test_fut);
        let metrics =
            test_helper.get_logged_metrics(metrics::CLIENT_CONNECTIONS_STOP_AND_START_METRIC_ID);
        assert_eq!(metrics.len(), 1);
        assert_eq!(metrics[0].payload, MetricEventPayload::Count(1));
    }

    #[fuchsia::test]
    fn test_restart_metric_stop_client_connections_request_sent_first() {
        let (mut test_helper, mut test_fut) = setup_test();

        // Send stop and start events corresponding to restarting client connections.
        test_helper.telemetry_sender.send(TelemetryEvent::StopClientConnectionsRequest);
        test_helper.advance_by(zx::MonotonicDuration::from_seconds(3), test_fut.as_mut());
        test_helper.telemetry_sender.send(TelemetryEvent::StartClientConnectionsRequest);
        // Check that 1 restart client connection event has been logged to cobalt.
        test_helper.drain_cobalt_events(&mut test_fut);
        let metrics =
            test_helper.get_logged_metrics(metrics::CLIENT_CONNECTIONS_STOP_AND_START_METRIC_ID);
        assert_eq!(metrics.len(), 1);
        assert_eq!(metrics[0].payload, MetricEventPayload::Count(1));

        // Stop and start client connections quickly again.
        test_helper.advance_by(zx::MonotonicDuration::from_seconds(20), test_fut.as_mut());
        test_helper.telemetry_sender.send(TelemetryEvent::StopClientConnectionsRequest);
        test_helper.advance_by(zx::MonotonicDuration::from_seconds(1), test_fut.as_mut());
        test_helper.telemetry_sender.send(TelemetryEvent::StartClientConnectionsRequest);
        // Check that 1 more event has been logged.
        test_helper.drain_cobalt_events(&mut test_fut);
        let metrics =
            test_helper.get_logged_metrics(metrics::CLIENT_CONNECTIONS_STOP_AND_START_METRIC_ID);
        assert_eq!(metrics.len(), 2);
        assert_eq!(metrics[1].payload, MetricEventPayload::Count(1));
    }

    #[fuchsia::test]
    fn test_restart_metric_stop_client_connections_request_long_time_not_counted() {
        let (mut test_helper, mut test_fut) = setup_test();

        // Send a stop and start with some time in between, then a quick stop and start.
        test_helper.telemetry_sender.send(TelemetryEvent::StopClientConnectionsRequest);
        test_helper.advance_by(zx::MonotonicDuration::from_seconds(30), test_fut.as_mut());
        test_helper.telemetry_sender.send(TelemetryEvent::StartClientConnectionsRequest);
        test_helper.advance_by(zx::MonotonicDuration::from_seconds(2), test_fut.as_mut());
        // Check that a restart was not logged since some time passed between requests.
        test_helper.drain_cobalt_events(&mut test_fut);
        let metrics =
            test_helper.get_logged_metrics(metrics::CLIENT_CONNECTIONS_STOP_AND_START_METRIC_ID);
        assert!(metrics.is_empty());

        // Send another stop and start that do correspond to a restart.
        test_helper.telemetry_sender.send(TelemetryEvent::StopClientConnectionsRequest);
        test_helper.advance_by(zx::MonotonicDuration::from_seconds(1), test_fut.as_mut());
        test_helper.telemetry_sender.send(TelemetryEvent::StartClientConnectionsRequest);
        // Check that exactly 1 restart client connections event was logged to cobalt.
        test_helper.drain_cobalt_events(&mut test_fut);
        let metrics =
            test_helper.get_logged_metrics(metrics::CLIENT_CONNECTIONS_STOP_AND_START_METRIC_ID);
        assert_eq!(metrics.len(), 1);
        assert_eq!(metrics[0].payload, MetricEventPayload::Count(1));
    }

    #[fuchsia::test]
    fn test_restart_metric_extra_stop_client_connections_ignored() {
        let (mut test_helper, mut test_fut) = setup_test();

        // Stop client connections well before starting it again.
        test_helper.telemetry_sender.send(TelemetryEvent::StopClientConnectionsRequest);
        test_helper.advance_by(zx::MonotonicDuration::from_seconds(10), test_fut.as_mut());

        // Send another stop client connections shortly before a start request. The second request
        // should should not cause a metric to be logged, since connections were already off.
        test_helper.telemetry_sender.send(TelemetryEvent::StopClientConnectionsRequest);
        test_helper.advance_by(zx::MonotonicDuration::from_seconds(1), test_fut.as_mut());
        test_helper.telemetry_sender.send(TelemetryEvent::StartClientConnectionsRequest);

        test_helper.drain_cobalt_events(&mut test_fut);
        let metrics =
            test_helper.get_logged_metrics(metrics::CLIENT_CONNECTIONS_STOP_AND_START_METRIC_ID);
        assert!(metrics.is_empty());
    }

    #[fuchsia::test]
    fn test_stop_ap_metric() {
        let (mut test_helper, mut test_fut) = setup_test();

        test_helper.telemetry_sender.send(TelemetryEvent::StopAp {
            enabled_duration: zx::MonotonicDuration::from_seconds(50),
        });

        test_helper.drain_cobalt_events(&mut test_fut);
        let metrics = test_helper
            .get_logged_metrics(metrics::ACCESS_POINT_ENABLED_DURATION_MIGRATED_METRIC_ID);
        assert_eq!(metrics.len(), 1);
        assert_eq!(
            metrics[0].payload,
            MetricEventPayload::IntegerValue(zx::MonotonicDuration::from_seconds(50).into_micros())
        );
    }

    #[fuchsia::test]
    fn test_data_persistence_called_every_five_minutes() {
        let (mut test_helper, mut test_fut) = setup_test();
        test_helper.advance_by(zx::MonotonicDuration::from_minutes(5), test_fut.as_mut());

        let tags = test_helper.get_persistence_reqs();
        assert!(tags.contains(&"wlancfg-client-stats-counters".to_string()), "tags: {:?}", tags);

        test_helper.send_connected_event(random_bss_description!(Wpa2));
        test_helper.advance_by(zx::MonotonicDuration::from_minutes(5), test_fut.as_mut());
        let tags = test_helper.get_persistence_reqs();
        assert!(tags.contains(&"wlancfg-client-stats-counters".to_string()), "tags: {:?}", tags);
    }

    #[derive(PartialEq)]
    enum CreateMetricsLoggerFailureMode {
        None,
        FactoryRequest,
        ApiFailure,
    }

    #[test_case(CreateMetricsLoggerFailureMode::None)]
    #[test_case(CreateMetricsLoggerFailureMode::FactoryRequest)]
    #[test_case(CreateMetricsLoggerFailureMode::ApiFailure)]
    #[fuchsia::test]
    fn test_create_metrics_logger(failure_mode: CreateMetricsLoggerFailureMode) {
        let mut exec = fasync::TestExecutor::new();
        let (factory_proxy, mut factory_stream) = fidl::endpoints::create_proxy_and_stream::<
            fidl_fuchsia_metrics::MetricEventLoggerFactoryMarker,
        >();

        let fut = create_metrics_logger(&factory_proxy);
        let mut fut = pin!(fut);

        // First, test the case where the factory service cannot be reached and expect an error.
        if failure_mode == CreateMetricsLoggerFailureMode::FactoryRequest {
            drop(factory_stream);
            assert_variant!(exec.run_until_stalled(&mut fut), Poll::Ready(Err(_)));
            return;
        }

        // If the test case is intended to allow the factory service to be contacted, run the
        // request future until stalled.
        assert_variant!(exec.run_until_stalled(&mut fut), Poll::Pending);

        let request = exec.run_until_stalled(&mut factory_stream.next());
        assert_variant!(
            request,
            Poll::Ready(Some(Ok(fidl_fuchsia_metrics::MetricEventLoggerFactoryRequest::CreateMetricEventLogger {
                project_spec: fidl_fuchsia_metrics::ProjectSpec {
                    customer_id: None,
                    project_id: Some(metrics::PROJECT_ID),
                    ..
                },
                responder,
                ..
            }))) => {
                match failure_mode {
                    CreateMetricsLoggerFailureMode::FactoryRequest => panic!("The factory request failure should have been handled already."),
                    CreateMetricsLoggerFailureMode::None => responder.send(Ok(())).expect("failed to send response"),
                    CreateMetricsLoggerFailureMode::ApiFailure => responder.send(Err(fidl_fuchsia_metrics::Error::InvalidArguments)).expect("failed to send response"),
                }
            }
        );

        // The future should run to completion and the output will vary depending on the specified
        // failure mode.
        assert_variant!(exec.run_until_stalled(&mut fut), Poll::Ready(result) => {
            match failure_mode {
                CreateMetricsLoggerFailureMode::FactoryRequest => panic!("The factory request failure should have been handled already."),
                CreateMetricsLoggerFailureMode::None => assert_variant!(result, Ok(_)),
                CreateMetricsLoggerFailureMode::ApiFailure => assert_variant!(result, Err(_))
            }
        });
    }

    #[fuchsia::test]
    fn test_log_iface_creation_failure() {
        let (mut test_helper, mut test_fut) = setup_test();

        // Send a notification that interface creation has failed.
        test_helper.telemetry_sender.send(TelemetryEvent::IfaceCreationResult(Err(())));

        // Run the telemetry loop until it stalls.
        assert_variant!(test_helper.advance_test_fut(&mut test_fut), Poll::Pending);

        // Expect that Cobalt has been notified of the interface creation failure.
        test_helper.drain_cobalt_events(&mut test_fut);
        let logged_metrics =
            test_helper.get_logged_metrics(metrics::INTERFACE_CREATION_FAILURE_METRIC_ID);
        assert_eq!(logged_metrics.len(), 1);
    }

    #[fuchsia::test]
    fn test_log_iface_destruction_failure() {
        let (mut test_helper, mut test_fut) = setup_test();

        // Send a notification that interface creation has failed.
        test_helper.telemetry_sender.send(TelemetryEvent::IfaceDestructionResult(Err(())));

        // Run the telemetry loop until it stalls.
        assert_variant!(test_helper.advance_test_fut(&mut test_fut), Poll::Pending);

        // Expect that Cobalt has been notified of the interface creation failure.
        test_helper.drain_cobalt_events(&mut test_fut);
        let logged_metrics =
            test_helper.get_logged_metrics(metrics::INTERFACE_DESTRUCTION_FAILURE_METRIC_ID);
        assert_eq!(logged_metrics.len(), 1);
    }

    #[test_case(ScanIssue::ScanFailure, metrics::CLIENT_SCAN_FAILURE_METRIC_ID)]
    #[test_case(ScanIssue::AbortedScan, metrics::ABORTED_SCAN_METRIC_ID)]
    #[test_case(ScanIssue::EmptyScanResults, metrics::EMPTY_SCAN_RESULTS_METRIC_ID)]
    #[fuchsia::test(add_test_attr = false)]
    fn test_scan_defect_metrics(scan_issue: ScanIssue, expected_metric_id: u32) {
        let (mut test_helper, mut test_fut) = setup_test();

        let event = TelemetryEvent::ScanEvent {
            inspect_data: ScanEventInspectData::new(),
            scan_defects: vec![scan_issue],
        };

        // Send a notification that interface creation has failed.
        test_helper.telemetry_sender.send(event);

        // Run the telemetry loop until it stalls.
        assert_variant!(test_helper.advance_test_fut(&mut test_fut), Poll::Pending);

        // Expect that Cobalt has been notified of the metric
        test_helper.drain_cobalt_events(&mut test_fut);
        let logged_metrics = test_helper.get_logged_metrics(expected_metric_id);
        assert_eq!(logged_metrics.len(), 1);
    }

    #[fuchsia::test]
    fn test_log_ap_start_failure() {
        let (mut test_helper, mut test_fut) = setup_test();

        // Send a notification that starting the AP has failed.
        test_helper.telemetry_sender.send(TelemetryEvent::StartApResult(Err(())));

        // Run the telemetry loop until it stalls.
        assert_variant!(test_helper.advance_test_fut(&mut test_fut), Poll::Pending);

        // Expect that Cobalt has been notified of the AP start failure.
        test_helper.drain_cobalt_events(&mut test_fut);
        let logged_metrics = test_helper.get_logged_metrics(metrics::AP_START_FAILURE_METRIC_ID);
        assert_eq!(logged_metrics.len(), 1);
    }

    #[test_case(
        RecoveryReason::CreateIfaceFailure(PhyRecoveryMechanism::PhyReset),
        metrics::RecoveryOccurrenceMetricDimensionReason::InterfaceCreationFailure ;
        "log recovery event for iface creation failure"
    )]
    #[test_case(
        RecoveryReason::DestroyIfaceFailure(PhyRecoveryMechanism::PhyReset),
        metrics::RecoveryOccurrenceMetricDimensionReason::InterfaceDestructionFailure ;
        "log recovery event for iface destruction failure"
    )]
    #[test_case(
        RecoveryReason::ConnectFailure(ClientRecoveryMechanism::Disconnect),
        metrics::RecoveryOccurrenceMetricDimensionReason::ClientConnectionFailure ;
        "log recovery event for connect failure"
    )]
    #[test_case(
        RecoveryReason::StartApFailure(ApRecoveryMechanism::StopAp),
        metrics::RecoveryOccurrenceMetricDimensionReason::ApStartFailure ;
        "log recovery event for start AP failure"
    )]
    #[test_case(
        RecoveryReason::ScanFailure(ClientRecoveryMechanism::Disconnect),
        metrics::RecoveryOccurrenceMetricDimensionReason::ScanFailure ;
        "log recovery event for scan failure"
    )]
    #[test_case(
        RecoveryReason::ScanCancellation(ClientRecoveryMechanism::Disconnect),
         metrics::RecoveryOccurrenceMetricDimensionReason::ScanCancellation ;
        "log recovery event for scan cancellation"
    )]
    #[test_case(
        RecoveryReason::ScanResultsEmpty(ClientRecoveryMechanism::Disconnect),
        metrics::RecoveryOccurrenceMetricDimensionReason::ScanResultsEmpty ;
        "log recovery event for empty scan results"
    )]
    #[fuchsia::test(add_test_attr = false)]
    fn test_log_recovery_occurrence(
        reason: RecoveryReason,
        expected_dimension: metrics::RecoveryOccurrenceMetricDimensionReason,
    ) {
        let (mut test_helper, mut test_fut) = setup_test();

        // Send the recovery event metric.
        test_helper.telemetry_sender.send(TelemetryEvent::RecoveryEvent { reason });

        // Run the telemetry loop until it stalls.
        assert_variant!(test_helper.advance_test_fut(&mut test_fut), Poll::Pending);

        // Expect that Cobalt has been notified of the recovery event
        assert_variant!(
            test_helper.exec.run_until_stalled(&mut test_helper.cobalt_1dot1_stream.next()),
            Poll::Ready(Some(Ok(fidl_fuchsia_metrics::MetricEventLoggerRequest::LogOccurrence {
                metric_id, event_codes, responder, ..
            }))) => {
                assert_eq!(metric_id, metrics::RECOVERY_OCCURRENCE_METRIC_ID);
                assert_eq!(event_codes, vec![expected_dimension.as_event_code()]);

                assert!(responder.send(Ok(())).is_ok());
        });
    }

    #[test_case(
        RecoveryReason::CreateIfaceFailure(PhyRecoveryMechanism::PhyReset),
        RecoveryOutcome::Success,
        metrics::INTERFACE_CREATION_RECOVERY_OUTCOME_METRIC_ID,
        vec![RecoveryOutcome::Success as u32] ;
        "create iface fixed by resetting PHY"
    )]
    #[test_case(
        RecoveryReason::CreateIfaceFailure(PhyRecoveryMechanism::PhyReset),
        RecoveryOutcome::Failure,
        metrics::INTERFACE_CREATION_RECOVERY_OUTCOME_METRIC_ID,
        vec![RecoveryOutcome::Failure as u32] ;
        "create iface not fixed by resetting PHY"
    )]
    #[test_case(
        RecoveryReason::DestroyIfaceFailure(PhyRecoveryMechanism::PhyReset),
        RecoveryOutcome::Success,
        metrics::INTERFACE_DESTRUCTION_RECOVERY_OUTCOME_METRIC_ID,
        vec![RecoveryOutcome::Success as u32] ;
        "destroy iface fixed by resetting PHY"
    )]
    #[test_case(
        RecoveryReason::DestroyIfaceFailure(PhyRecoveryMechanism::PhyReset),
        RecoveryOutcome::Failure,
        metrics::INTERFACE_DESTRUCTION_RECOVERY_OUTCOME_METRIC_ID,
        vec![RecoveryOutcome::Failure as u32] ;
        "destroy iface not fixed by resetting PHY"
    )]
    #[test_case(
        RecoveryReason::ConnectFailure(ClientRecoveryMechanism::Disconnect),
        RecoveryOutcome::Success,
        metrics::CONNECT_FAILURE_RECOVERY_OUTCOME_METRIC_ID,
        vec![RecoveryOutcome::Success as u32, ClientRecoveryMechanism::Disconnect as u32] ;
        "connect works after disconnecting"
    )]
    #[test_case(
        RecoveryReason::ConnectFailure(ClientRecoveryMechanism::DestroyIface),
        RecoveryOutcome::Success,
        metrics::CONNECT_FAILURE_RECOVERY_OUTCOME_METRIC_ID,
        vec![RecoveryOutcome::Success as u32, ClientRecoveryMechanism::DestroyIface as u32] ;
        "connect works after destroying iface"
    )]
    #[test_case(
        RecoveryReason::ConnectFailure(ClientRecoveryMechanism::PhyReset),
        RecoveryOutcome::Success,
        metrics::CONNECT_FAILURE_RECOVERY_OUTCOME_METRIC_ID,
        vec![RecoveryOutcome::Success as u32, ClientRecoveryMechanism::PhyReset as u32] ;
        "connect works after resetting PHY"
    )]
    #[test_case(
        RecoveryReason::ConnectFailure(ClientRecoveryMechanism::Disconnect),
        RecoveryOutcome::Failure,
        metrics::CONNECT_FAILURE_RECOVERY_OUTCOME_METRIC_ID,
        vec![RecoveryOutcome::Failure as u32, ClientRecoveryMechanism::Disconnect as u32] ;
        "connect still fails after disconnecting"
    )]
    #[test_case(
        RecoveryReason::ConnectFailure(ClientRecoveryMechanism::DestroyIface),
        RecoveryOutcome::Failure,
        metrics::CONNECT_FAILURE_RECOVERY_OUTCOME_METRIC_ID,
        vec![RecoveryOutcome::Failure as u32, ClientRecoveryMechanism::DestroyIface as u32] ;
        "connect still fails after destroying iface"
    )]
    #[test_case(
        RecoveryReason::ConnectFailure(ClientRecoveryMechanism::PhyReset),
        RecoveryOutcome::Failure,
        metrics::CONNECT_FAILURE_RECOVERY_OUTCOME_METRIC_ID,
        vec![RecoveryOutcome::Failure as u32, ClientRecoveryMechanism::PhyReset as u32] ;
        "connect still fails after resetting PHY"
    )]
    #[test_case(
        RecoveryReason::StartApFailure(ApRecoveryMechanism::StopAp),
        RecoveryOutcome::Success,
        metrics::START_ACCESS_POINT_RECOVERY_OUTCOME_METRIC_ID,
        vec![RecoveryOutcome::Success as u32, ApRecoveryMechanism::StopAp as u32] ;
        "start AP works after stopping AP"
    )]
    #[test_case(
        RecoveryReason::StartApFailure(ApRecoveryMechanism::DestroyIface),
        RecoveryOutcome::Success,
        metrics::START_ACCESS_POINT_RECOVERY_OUTCOME_METRIC_ID,
        vec![RecoveryOutcome::Success as u32, ApRecoveryMechanism::DestroyIface as u32] ;
        "start AP works after destroying iface"
    )]
    #[test_case(
        RecoveryReason::StartApFailure(ApRecoveryMechanism::ResetPhy),
        RecoveryOutcome::Success,
        metrics::START_ACCESS_POINT_RECOVERY_OUTCOME_METRIC_ID,
        vec![RecoveryOutcome::Success as u32, ApRecoveryMechanism::ResetPhy as u32] ;
        "start AP works after resetting PHY"
    )]
    #[test_case(
        RecoveryReason::StartApFailure(ApRecoveryMechanism::StopAp),
        RecoveryOutcome::Failure,
        metrics::START_ACCESS_POINT_RECOVERY_OUTCOME_METRIC_ID,
        vec![RecoveryOutcome::Failure as u32, ApRecoveryMechanism::StopAp as u32] ;
        "start AP still fails after stopping AP"
    )]
    #[test_case(
        RecoveryReason::StartApFailure(ApRecoveryMechanism::DestroyIface),
        RecoveryOutcome::Failure,
        metrics::START_ACCESS_POINT_RECOVERY_OUTCOME_METRIC_ID,
        vec![RecoveryOutcome::Failure as u32, ApRecoveryMechanism::DestroyIface as u32] ;
        "start AP still fails after destroying iface"
    )]
    #[test_case(
        RecoveryReason::StartApFailure(ApRecoveryMechanism::ResetPhy),
        RecoveryOutcome::Failure,
        metrics::START_ACCESS_POINT_RECOVERY_OUTCOME_METRIC_ID,
        vec![RecoveryOutcome::Failure as u32, ApRecoveryMechanism::ResetPhy as u32] ;
        "start AP still fails after resetting PHY"
    )]
    #[test_case(
        RecoveryReason::ScanFailure(ClientRecoveryMechanism::Disconnect),
        RecoveryOutcome::Success,
        metrics::SCAN_FAILURE_RECOVERY_OUTCOME_METRIC_ID,
        vec![RecoveryOutcome::Success as u32, ClientRecoveryMechanism::Disconnect as u32] ;
        "scan works after disconnecting"
    )]
    #[test_case(
        RecoveryReason::ScanFailure(ClientRecoveryMechanism::DestroyIface),
        RecoveryOutcome::Success,
        metrics::SCAN_FAILURE_RECOVERY_OUTCOME_METRIC_ID,
        vec![RecoveryOutcome::Success as u32, ClientRecoveryMechanism::DestroyIface as u32] ;
        "scan works after destroying iface"
    )]
    #[test_case(
        RecoveryReason::ScanFailure(ClientRecoveryMechanism::PhyReset),
        RecoveryOutcome::Success,
        metrics::SCAN_FAILURE_RECOVERY_OUTCOME_METRIC_ID,
        vec![RecoveryOutcome::Success as u32, ClientRecoveryMechanism::PhyReset as u32] ;
        "scan works after resetting PHY"
    )]
    #[test_case(
        RecoveryReason::ScanFailure(ClientRecoveryMechanism::Disconnect),
        RecoveryOutcome::Failure,
        metrics::SCAN_FAILURE_RECOVERY_OUTCOME_METRIC_ID,
        vec![RecoveryOutcome::Failure as u32, ClientRecoveryMechanism::Disconnect as u32] ;
        "scan still fails after disconnecting"
    )]
    #[test_case(
        RecoveryReason::ScanFailure(ClientRecoveryMechanism::DestroyIface),
        RecoveryOutcome::Failure,
        metrics::SCAN_FAILURE_RECOVERY_OUTCOME_METRIC_ID,
        vec![RecoveryOutcome::Failure as u32, ClientRecoveryMechanism::DestroyIface as u32] ;
        "scan still fails after destroying iface"
    )]
    #[test_case(
        RecoveryReason::ScanFailure(ClientRecoveryMechanism::PhyReset),
        RecoveryOutcome::Failure,
        metrics::SCAN_FAILURE_RECOVERY_OUTCOME_METRIC_ID,
        vec![RecoveryOutcome::Failure as u32, ClientRecoveryMechanism::PhyReset as u32] ;
        "scan still fails after resetting PHY"
    )]
    #[test_case(
        RecoveryReason::ScanCancellation(ClientRecoveryMechanism::Disconnect),
        RecoveryOutcome::Success,
        metrics::SCAN_CANCELLATION_RECOVERY_OUTCOME_METRIC_ID,
        vec![RecoveryOutcome::Success as u32, ClientRecoveryMechanism::Disconnect as u32] ;
        "scan is no longer cancelled after disconnecting"
    )]
    #[test_case(
        RecoveryReason::ScanCancellation(ClientRecoveryMechanism::DestroyIface),
        RecoveryOutcome::Success,
        metrics::SCAN_CANCELLATION_RECOVERY_OUTCOME_METRIC_ID,
        vec![RecoveryOutcome::Success as u32, ClientRecoveryMechanism::DestroyIface as u32] ;
        "scan is no longer cancelled after destroying iface"
    )]
    #[test_case(
        RecoveryReason::ScanCancellation(ClientRecoveryMechanism::PhyReset),
        RecoveryOutcome::Success,
        metrics::SCAN_CANCELLATION_RECOVERY_OUTCOME_METRIC_ID,
        vec![RecoveryOutcome::Success as u32, ClientRecoveryMechanism::PhyReset as u32] ;
        "scan is no longer cancelled after resetting PHY"
    )]
    #[test_case(
        RecoveryReason::ScanCancellation(ClientRecoveryMechanism::Disconnect),
        RecoveryOutcome::Failure,
        metrics::SCAN_CANCELLATION_RECOVERY_OUTCOME_METRIC_ID,
        vec![RecoveryOutcome::Failure as u32, ClientRecoveryMechanism::Disconnect as u32] ;
        "scan is still cancelled after disconnect"
    )]
    #[test_case(
        RecoveryReason::ScanCancellation(ClientRecoveryMechanism::DestroyIface),
        RecoveryOutcome::Failure,
        metrics::SCAN_CANCELLATION_RECOVERY_OUTCOME_METRIC_ID,
        vec![RecoveryOutcome::Failure as u32, ClientRecoveryMechanism::DestroyIface as u32] ;
        "scan is still cancelled after destroying iface"
    )]
    #[test_case(
        RecoveryReason::ScanCancellation(ClientRecoveryMechanism::PhyReset),
        RecoveryOutcome::Failure,
        metrics::SCAN_CANCELLATION_RECOVERY_OUTCOME_METRIC_ID,
        vec![RecoveryOutcome::Failure as u32, ClientRecoveryMechanism::PhyReset as u32] ;
        "scan is still cancelled after resetting PHY"
    )]
    #[test_case(
        RecoveryReason::ScanResultsEmpty(ClientRecoveryMechanism::Disconnect),
        RecoveryOutcome::Success,
        metrics::EMPTY_SCAN_RESULTS_RECOVERY_OUTCOME_METRIC_ID,
        vec![RecoveryOutcome::Success as u32, ClientRecoveryMechanism::Disconnect as u32] ;
        "scan results not empty after disconnect"
    )]
    #[test_case(
        RecoveryReason::ScanResultsEmpty(ClientRecoveryMechanism::DestroyIface),
        RecoveryOutcome::Success,
        metrics::EMPTY_SCAN_RESULTS_RECOVERY_OUTCOME_METRIC_ID,
        vec![RecoveryOutcome::Success as u32, ClientRecoveryMechanism::DestroyIface as u32] ;
        "scan results not empty after destroy iface"
    )]
    #[test_case(
        RecoveryReason::ScanResultsEmpty(ClientRecoveryMechanism::PhyReset),
        RecoveryOutcome::Success,
        metrics::EMPTY_SCAN_RESULTS_RECOVERY_OUTCOME_METRIC_ID,
        vec![RecoveryOutcome::Success as u32, ClientRecoveryMechanism::PhyReset as u32] ;
        "scan results not empty after PHY reset"
    )]
    #[test_case(
        RecoveryReason::ScanResultsEmpty(ClientRecoveryMechanism::Disconnect),
        RecoveryOutcome::Failure,
        metrics::EMPTY_SCAN_RESULTS_RECOVERY_OUTCOME_METRIC_ID,
        vec![RecoveryOutcome::Failure as u32, ClientRecoveryMechanism::Disconnect as u32] ;
        "scan results still empty after disconnect"
    )]
    #[test_case(
        RecoveryReason::ScanResultsEmpty(ClientRecoveryMechanism::DestroyIface),
        RecoveryOutcome::Failure,
        metrics::EMPTY_SCAN_RESULTS_RECOVERY_OUTCOME_METRIC_ID,
        vec![RecoveryOutcome::Failure as u32, ClientRecoveryMechanism::DestroyIface as u32] ;
        "scan results still empty after destroy iface"
    )]
    #[test_case(
        RecoveryReason::ScanResultsEmpty(ClientRecoveryMechanism::PhyReset),
        RecoveryOutcome::Failure,
        metrics::EMPTY_SCAN_RESULTS_RECOVERY_OUTCOME_METRIC_ID,
        vec![RecoveryOutcome::Failure as u32, ClientRecoveryMechanism::PhyReset as u32] ;
        "scan results still empty after PHY reset"
    )]
    #[fuchsia::test(add_test_attr = false)]
    fn test_log_post_recovery_result(
        reason: RecoveryReason,
        outcome: RecoveryOutcome,
        expected_metric_id: u32,
        expected_event_codes: Vec<u32>,
    ) {
        let mut exec = fasync::TestExecutor::new();

        // Construct a StatsLogger
        let (cobalt_1dot1_proxy, mut cobalt_1dot1_stream) =
            create_proxy_and_stream::<fidl_fuchsia_metrics::MetricEventLoggerMarker>();

        let inspector = Inspector::default();
        let inspect_node = inspector.root().create_child("stats");

        let mut stats_logger = StatsLogger::new(cobalt_1dot1_proxy, &inspect_node);

        // Log the test telemetry event.
        let fut = stats_logger.log_post_recovery_result(reason, outcome);
        let mut fut = pin!(fut);
        assert_variant!(exec.run_until_stalled(&mut fut), Poll::Pending);

        // Verify the metric that was emitted.
        assert_variant!(
            exec.run_until_stalled(&mut cobalt_1dot1_stream.next()),
            Poll::Ready(Some(Ok(fidl_fuchsia_metrics::MetricEventLoggerRequest::LogOccurrence {
                metric_id, event_codes, responder, ..
            }))) => {
                assert_eq!(metric_id, expected_metric_id);
                assert_eq!(event_codes, expected_event_codes);

                assert!(responder.send(Ok(())).is_ok());
        });

        // The future should complete.
        assert_variant!(exec.run_until_stalled(&mut fut), Poll::Ready(()));
    }

    #[fuchsia::test]
    fn test_post_recovery_connect_success() {
        let (mut test_helper, mut test_fut) = setup_test();

        // Send the recovery event metric.
        let reason = RecoveryReason::ConnectFailure(ClientRecoveryMechanism::PhyReset);
        let event = TelemetryEvent::RecoveryEvent { reason };
        test_helper.telemetry_sender.send(event);

        // Run the telemetry loop until it stalls.
        assert_variant!(test_helper.advance_test_fut(&mut test_fut), Poll::Pending);

        // Expect that Cobalt has been notified of the recovery event
        test_helper.drain_cobalt_events(&mut test_fut);
        let logged_metrics = test_helper.get_logged_metrics(metrics::RECOVERY_OCCURRENCE_METRIC_ID);
        assert_eq!(logged_metrics.len(), 1);

        // Verify the reason dimension.
        assert_eq!(
            logged_metrics[0].event_codes,
            vec![metrics::RecoveryOccurrenceMetricDimensionReason::ClientConnectionFailure
                .as_event_code()]
        );

        // Send a successful connect result.
        test_helper.telemetry_sender.send(TelemetryEvent::ConnectResult {
            iface_id: IFACE_ID,
            policy_connect_reason: Some(
                client::types::ConnectReason::RetryAfterFailedConnectAttempt,
            ),
            result: fake_connect_result(fidl_ieee80211::StatusCode::Success),
            multiple_bss_candidates: true,
            ap_state: random_bss_description!(Wpa1).into(),
            network_is_likely_hidden: false,
        });

        // Run the telemetry loop until it stalls.
        assert_variant!(test_helper.advance_test_fut(&mut test_fut), Poll::Pending);

        // Verify the connect post-recovery success metric was logged.
        test_helper.drain_cobalt_events(&mut test_fut);
        let logged_metrics =
            test_helper.get_logged_metrics(metrics::CONNECT_FAILURE_RECOVERY_OUTCOME_METRIC_ID);
        assert_eq!(
            logged_metrics[0].event_codes,
            vec![RecoveryOutcome::Success as u32, ClientRecoveryMechanism::PhyReset as u32]
        );

        // Verify a subsequent connect result does not cause another metric to be logged.
        test_helper.telemetry_sender.send(TelemetryEvent::ConnectResult {
            iface_id: IFACE_ID,
            policy_connect_reason: Some(
                client::types::ConnectReason::RetryAfterFailedConnectAttempt,
            ),
            result: fake_connect_result(fidl_ieee80211::StatusCode::Success),
            multiple_bss_candidates: true,
            ap_state: random_bss_description!(Wpa1).into(),
            network_is_likely_hidden: false,
        });

        assert_variant!(test_helper.advance_test_fut(&mut test_fut), Poll::Pending);

        test_helper.cobalt_events = Vec::new();
        test_helper.drain_cobalt_events(&mut test_fut);
        let logged_metrics =
            test_helper.get_logged_metrics(metrics::CONNECT_FAILURE_RECOVERY_OUTCOME_METRIC_ID);
        assert!(logged_metrics.is_empty());
    }

    #[fuchsia::test]
    fn test_post_recovery_connect_failure() {
        let (mut test_helper, mut test_fut) = setup_test();

        // Send the recovery event metric.
        let reason = RecoveryReason::ConnectFailure(ClientRecoveryMechanism::PhyReset);
        let event = TelemetryEvent::RecoveryEvent { reason };
        test_helper.telemetry_sender.send(event);

        // Run the telemetry loop until it stalls.
        assert_variant!(test_helper.advance_test_fut(&mut test_fut), Poll::Pending);

        // Expect that Cobalt has been notified of the recovery event
        test_helper.drain_cobalt_events(&mut test_fut);
        let logged_metrics = test_helper.get_logged_metrics(metrics::RECOVERY_OCCURRENCE_METRIC_ID);
        assert_eq!(logged_metrics.len(), 1);

        // Verify the reason dimension.
        assert_eq!(
            logged_metrics[0].event_codes,
            vec![metrics::RecoveryOccurrenceMetricDimensionReason::ClientConnectionFailure
                .as_event_code()]
        );

        // Send a failed connect result.
        test_helper.telemetry_sender.send(TelemetryEvent::ConnectResult {
            iface_id: IFACE_ID,
            policy_connect_reason: Some(
                client::types::ConnectReason::RetryAfterFailedConnectAttempt,
            ),
            result: fake_connect_result(fidl_ieee80211::StatusCode::RefusedReasonUnspecified),
            multiple_bss_candidates: true,
            ap_state: random_bss_description!(Wpa1).into(),
            network_is_likely_hidden: false,
        });

        // Run the telemetry loop until it stalls.
        assert_variant!(test_helper.advance_test_fut(&mut test_fut), Poll::Pending);

        // Verify the connect post-recovery failure metric was logged.
        test_helper.drain_cobalt_events(&mut test_fut);
        let logged_metrics =
            test_helper.get_logged_metrics(metrics::CONNECT_FAILURE_RECOVERY_OUTCOME_METRIC_ID);
        assert_eq!(
            logged_metrics[0].event_codes,
            vec![RecoveryOutcome::Failure as u32, ClientRecoveryMechanism::PhyReset as u32]
        );

        // Verify a subsequent connect result does not cause another metric to be logged.
        test_helper.telemetry_sender.send(TelemetryEvent::ConnectResult {
            iface_id: IFACE_ID,
            policy_connect_reason: Some(
                client::types::ConnectReason::RetryAfterFailedConnectAttempt,
            ),
            result: fake_connect_result(fidl_ieee80211::StatusCode::RefusedReasonUnspecified),
            multiple_bss_candidates: true,
            ap_state: random_bss_description!(Wpa1).into(),
            network_is_likely_hidden: false,
        });

        assert_variant!(test_helper.advance_test_fut(&mut test_fut), Poll::Pending);

        test_helper.cobalt_events = Vec::new();
        test_helper.drain_cobalt_events(&mut test_fut);
        let logged_metrics =
            test_helper.get_logged_metrics(metrics::CONNECT_FAILURE_RECOVERY_OUTCOME_METRIC_ID);
        assert!(logged_metrics.is_empty());
    }

    fn test_generic_post_recovery_event(
        recovery_event: TelemetryEvent,
        post_recovery_event: TelemetryEvent,
        duplicate_check_event: TelemetryEvent,
        expected_metric_id: u32,
        dimensions: Vec<u32>,
    ) {
        let (mut test_helper, mut test_fut) = setup_test();
        test_helper.exec.set_fake_time(fasync::MonotonicInstant::from_nanos(1));

        // Send the recovery event metric
        test_helper.telemetry_sender.send(recovery_event);
        assert_variant!(test_helper.advance_test_fut(&mut test_fut), Poll::Pending);

        // Send the post-recovery result metric
        test_helper.telemetry_sender.send(post_recovery_event);
        assert_variant!(test_helper.advance_test_fut(&mut test_fut), Poll::Pending);

        // Get the metric that was logged and verify that it was constructed properly.
        test_helper.drain_cobalt_events(&mut test_fut);
        let logged_metrics = test_helper.get_logged_metrics(expected_metric_id);

        assert_eq!(logged_metrics.len(), 1);
        assert_eq!(logged_metrics[0].event_codes, dimensions);

        // Re-send the result metric and verify that nothing new was logged.
        test_helper.cobalt_events = Vec::new();
        test_helper.telemetry_sender.send(duplicate_check_event);
        assert_variant!(test_helper.advance_test_fut(&mut test_fut), Poll::Pending);
        let logged_metrics = test_helper.get_logged_metrics(expected_metric_id);
        assert!(logged_metrics.is_empty());

        // If the recovery was successful, ensure that the last successful recovery time has been
        // updated.  If it was not successful, the last recovery time should not have been changed.
        if dimensions[0] == RecoveryOutcome::Success.as_event_code() {
            assert_data_tree_with_respond_blocking_req!(test_helper, test_fut, root: contains {
                stats: contains {
                    last_successful_recovery: 1_u64,
                    successful_recoveries: 1_u64
                }
            });
        } else {
            assert_data_tree_with_respond_blocking_req!(test_helper, test_fut, root: contains {
                stats: contains {
                    last_successful_recovery: 0_u64,
                    successful_recoveries: 0_u64
                }
            });
        }
    }

    #[test_case(
        TelemetryEvent::RecoveryEvent {
            reason: RecoveryReason::ScanFailure(ClientRecoveryMechanism::Disconnect)
        },
        TelemetryEvent::ScanEvent {
            inspect_data: ScanEventInspectData { unknown_protection_ies: vec![] },
            scan_defects: vec![]
        },
        TelemetryEvent::ScanEvent {
            inspect_data: ScanEventInspectData { unknown_protection_ies: vec![] },
            scan_defects: vec![]
        },
        metrics::SCAN_FAILURE_RECOVERY_OUTCOME_METRIC_ID,
        vec![RecoveryOutcome::Success as u32, ClientRecoveryMechanism::Disconnect as u32] ;
        "Scan succeeds after recovery with no other defects"
    )]
    #[test_case(
        TelemetryEvent::RecoveryEvent {
            reason: RecoveryReason::ScanFailure(ClientRecoveryMechanism::Disconnect)
        },
        TelemetryEvent::ScanEvent {
            inspect_data: ScanEventInspectData { unknown_protection_ies: vec![] },
            scan_defects: vec![ScanIssue::ScanFailure]
        },
        TelemetryEvent::ScanEvent {
            inspect_data: ScanEventInspectData { unknown_protection_ies: vec![] },
            scan_defects: vec![ScanIssue::ScanFailure]
        },
        metrics::SCAN_FAILURE_RECOVERY_OUTCOME_METRIC_ID,
        vec![RecoveryOutcome::Failure as u32, ClientRecoveryMechanism::Disconnect as u32] ;
        "Scan still fails following recovery"
    )]
    #[test_case(
        TelemetryEvent::RecoveryEvent {
            reason: RecoveryReason::ScanFailure(ClientRecoveryMechanism::DestroyIface)
        },
        TelemetryEvent::ScanEvent {
            inspect_data: ScanEventInspectData { unknown_protection_ies: vec![] },
            scan_defects: vec![ScanIssue::AbortedScan]
        },
        TelemetryEvent::ScanEvent {
            inspect_data: ScanEventInspectData { unknown_protection_ies: vec![] },
            scan_defects: vec![ScanIssue::AbortedScan]
        },
        metrics::SCAN_FAILURE_RECOVERY_OUTCOME_METRIC_ID,
        vec![RecoveryOutcome::Success as u32, ClientRecoveryMechanism::DestroyIface as u32] ;
        "Scan succeeds after recovery but the scan was cancelled"
    )]
    #[test_case(
        TelemetryEvent::RecoveryEvent {
            reason: RecoveryReason::ScanFailure(ClientRecoveryMechanism::PhyReset)
        },
        TelemetryEvent::ScanEvent {
            inspect_data: ScanEventInspectData { unknown_protection_ies: vec![] },
            scan_defects: vec![ScanIssue::EmptyScanResults]
        },
        TelemetryEvent::ScanEvent {
            inspect_data: ScanEventInspectData { unknown_protection_ies: vec![] },
            scan_defects: vec![ScanIssue::EmptyScanResults]
        },
        metrics::SCAN_FAILURE_RECOVERY_OUTCOME_METRIC_ID,
        vec![RecoveryOutcome::Success as u32, ClientRecoveryMechanism::PhyReset as u32] ;
        "Scan succeeds after recovery but the results are empty"
    )]
    #[test_case(
        TelemetryEvent::RecoveryEvent {
            reason: RecoveryReason::ScanCancellation(ClientRecoveryMechanism::Disconnect)
        },
        TelemetryEvent::ScanEvent {
            inspect_data: ScanEventInspectData { unknown_protection_ies: vec![] },
            scan_defects: vec![]
        },
        TelemetryEvent::ScanEvent {
            inspect_data: ScanEventInspectData { unknown_protection_ies: vec![] },
            scan_defects: vec![]
        },
        metrics::SCAN_CANCELLATION_RECOVERY_OUTCOME_METRIC_ID,
        vec![RecoveryOutcome::Success as u32, ClientRecoveryMechanism::Disconnect as u32] ;
        "Scan no longer cancelled after recovery"
    )]
    #[test_case(
        TelemetryEvent::RecoveryEvent {
            reason: RecoveryReason::ScanCancellation(ClientRecoveryMechanism::Disconnect)
        },
        TelemetryEvent::ScanEvent {
            inspect_data: ScanEventInspectData { unknown_protection_ies: vec![] },
            scan_defects: vec![ScanIssue::ScanFailure]
        },
        TelemetryEvent::ScanEvent {
            inspect_data: ScanEventInspectData { unknown_protection_ies: vec![] },
            scan_defects: vec![ScanIssue::ScanFailure]
        },
        metrics::SCAN_CANCELLATION_RECOVERY_OUTCOME_METRIC_ID,
        vec![RecoveryOutcome::Success as u32, ClientRecoveryMechanism::Disconnect as u32] ;
        "Scan not cancelled after recovery but fails instead"
    )]
    #[test_case(
        TelemetryEvent::RecoveryEvent {
            reason: RecoveryReason::ScanCancellation(ClientRecoveryMechanism::DestroyIface)
        },
        TelemetryEvent::ScanEvent {
            inspect_data: ScanEventInspectData { unknown_protection_ies: vec![] },
            scan_defects: vec![ScanIssue::AbortedScan]
        },
        TelemetryEvent::ScanEvent {
            inspect_data: ScanEventInspectData { unknown_protection_ies: vec![] },
            scan_defects: vec![ScanIssue::AbortedScan]
        },
        metrics::SCAN_CANCELLATION_RECOVERY_OUTCOME_METRIC_ID,
        vec![RecoveryOutcome::Failure as u32, ClientRecoveryMechanism::DestroyIface as u32] ;
        "Scan still cancelled after recovery"
    )]
    #[test_case(
        TelemetryEvent::RecoveryEvent {
            reason: RecoveryReason::ScanCancellation(ClientRecoveryMechanism::PhyReset)
        },
        TelemetryEvent::ScanEvent {
            inspect_data: ScanEventInspectData { unknown_protection_ies: vec![] },
            scan_defects: vec![ScanIssue::EmptyScanResults]
        },
        TelemetryEvent::ScanEvent {
            inspect_data: ScanEventInspectData { unknown_protection_ies: vec![] },
            scan_defects: vec![ScanIssue::EmptyScanResults]
        },
        metrics::SCAN_CANCELLATION_RECOVERY_OUTCOME_METRIC_ID,
        vec![RecoveryOutcome::Success as u32, ClientRecoveryMechanism::PhyReset as u32] ;
        "Scan not cancelled after recovery but results are empty"
    )]
    #[test_case(
        TelemetryEvent::RecoveryEvent {
            reason: RecoveryReason::ScanResultsEmpty(ClientRecoveryMechanism::Disconnect)
        },
        TelemetryEvent::ScanEvent {
            inspect_data: ScanEventInspectData { unknown_protection_ies: vec![] },
            scan_defects: vec![]
        },
        TelemetryEvent::ScanEvent {
            inspect_data: ScanEventInspectData { unknown_protection_ies: vec![] },
            scan_defects: vec![]
        },
        metrics::EMPTY_SCAN_RESULTS_RECOVERY_OUTCOME_METRIC_ID,
        vec![RecoveryOutcome::Success as u32, ClientRecoveryMechanism::Disconnect as u32] ;
        "Scan results not empty after recovery and no other errors"
    )]
    #[test_case(
        TelemetryEvent::RecoveryEvent {
            reason: RecoveryReason::ScanResultsEmpty(ClientRecoveryMechanism::Disconnect)
        },
        TelemetryEvent::ScanEvent {
            inspect_data: ScanEventInspectData { unknown_protection_ies: vec![] },
            scan_defects: vec![ScanIssue::ScanFailure]
        },
        TelemetryEvent::ScanEvent {
            inspect_data: ScanEventInspectData { unknown_protection_ies: vec![] },
            scan_defects: vec![ScanIssue::ScanFailure]
        },
        metrics::EMPTY_SCAN_RESULTS_RECOVERY_OUTCOME_METRIC_ID,
        vec![RecoveryOutcome::Success as u32, ClientRecoveryMechanism::Disconnect as u32] ;
        "Scan results no longer empty after recovery, but scan fails"
    )]
    #[test_case(
        TelemetryEvent::RecoveryEvent {
            reason: RecoveryReason::ScanResultsEmpty(ClientRecoveryMechanism::DestroyIface)
        },
        TelemetryEvent::ScanEvent {
            inspect_data: ScanEventInspectData { unknown_protection_ies: vec![] },
            scan_defects: vec![ScanIssue::AbortedScan]
        },
        TelemetryEvent::ScanEvent {
            inspect_data: ScanEventInspectData { unknown_protection_ies: vec![] },
            scan_defects: vec![ScanIssue::AbortedScan]
        },
        metrics::EMPTY_SCAN_RESULTS_RECOVERY_OUTCOME_METRIC_ID,
        vec![RecoveryOutcome::Success as u32, ClientRecoveryMechanism::DestroyIface as u32] ;
        "Scan results not empty after recovery but scan is cancelled"
    )]
    #[test_case(
        TelemetryEvent::RecoveryEvent {
            reason: RecoveryReason::ScanResultsEmpty(ClientRecoveryMechanism::PhyReset)
        },
        TelemetryEvent::ScanEvent {
            inspect_data: ScanEventInspectData { unknown_protection_ies: vec![] },
            scan_defects: vec![ScanIssue::EmptyScanResults]
        },
        TelemetryEvent::ScanEvent {
            inspect_data: ScanEventInspectData { unknown_protection_ies: vec![] },
            scan_defects: vec![ScanIssue::EmptyScanResults]
        },
        metrics::EMPTY_SCAN_RESULTS_RECOVERY_OUTCOME_METRIC_ID,
        vec![RecoveryOutcome::Failure as u32, ClientRecoveryMechanism::PhyReset as u32] ;
        "Scan results still empty after recovery"
    )]
    #[fuchsia::test(add_test_attr = false)]
    fn test_post_recovery_scan_metrics(
        recovery_event: TelemetryEvent,
        post_recovery_event: TelemetryEvent,
        duplicate_check_event: TelemetryEvent,
        expected_metric_id: u32,
        dimensions: Vec<u32>,
    ) {
        test_generic_post_recovery_event(
            recovery_event,
            post_recovery_event,
            duplicate_check_event,
            expected_metric_id,
            dimensions,
        );
    }

    #[test_case(
        TelemetryEvent::RecoveryEvent {
            reason: RecoveryReason::StartApFailure(ApRecoveryMechanism::ResetPhy)
        },
        TelemetryEvent::StartApResult(Err(())),
        TelemetryEvent::StartApResult(Err(())),
        metrics::START_ACCESS_POINT_RECOVERY_OUTCOME_METRIC_ID,
        vec![RecoveryOutcome::Failure as u32, ApRecoveryMechanism::ResetPhy as u32] ;
        "start AP still does not work after recovery"
    )]
    #[test_case(
        TelemetryEvent::RecoveryEvent {
            reason: RecoveryReason::StartApFailure(ApRecoveryMechanism::ResetPhy)
        },
        TelemetryEvent::StartApResult(Ok(())),
        TelemetryEvent::StartApResult(Ok(())),
        metrics::START_ACCESS_POINT_RECOVERY_OUTCOME_METRIC_ID,
        vec![RecoveryOutcome::Success as u32, ApRecoveryMechanism::ResetPhy as u32] ;
        "start AP works after recovery"
    )]
    #[fuchsia::test(add_test_attr = false)]
    fn test_post_recovery_start_ap(
        recovery_event: TelemetryEvent,
        post_recovery_event: TelemetryEvent,
        duplicate_check_event: TelemetryEvent,
        expected_metric_id: u32,
        dimensions: Vec<u32>,
    ) {
        test_generic_post_recovery_event(
            recovery_event,
            post_recovery_event,
            duplicate_check_event,
            expected_metric_id,
            dimensions,
        );
    }

    #[test_case(
        TelemetryEvent::RecoveryEvent {
            reason: RecoveryReason::CreateIfaceFailure(PhyRecoveryMechanism::PhyReset)
        },
        TelemetryEvent::IfaceCreationResult(Err(())),
        TelemetryEvent::IfaceCreationResult(Err(())),
        metrics::INTERFACE_CREATION_RECOVERY_OUTCOME_METRIC_ID,
        vec![RecoveryOutcome::Failure as u32] ;
        "create iface still does not work after recovery"
    )]
    #[test_case(
        TelemetryEvent::RecoveryEvent {
            reason: RecoveryReason::CreateIfaceFailure(PhyRecoveryMechanism::PhyReset)
        },
        TelemetryEvent::IfaceCreationResult(Ok(())),
        TelemetryEvent::IfaceCreationResult(Ok(())),
        metrics::INTERFACE_CREATION_RECOVERY_OUTCOME_METRIC_ID,
        vec![RecoveryOutcome::Success as u32] ;
        "create iface works after recovery"
    )]
    #[fuchsia::test(add_test_attr = false)]
    fn test_post_recovery_create_iface(
        recovery_event: TelemetryEvent,
        post_recovery_event: TelemetryEvent,
        duplicate_check_event: TelemetryEvent,
        expected_metric_id: u32,
        dimensions: Vec<u32>,
    ) {
        test_generic_post_recovery_event(
            recovery_event,
            post_recovery_event,
            duplicate_check_event,
            expected_metric_id,
            dimensions,
        );
    }

    #[test_case(
        TelemetryEvent::RecoveryEvent {
            reason: RecoveryReason::DestroyIfaceFailure(PhyRecoveryMechanism::PhyReset)
        },
        TelemetryEvent::IfaceDestructionResult(Err(())),
        TelemetryEvent::IfaceDestructionResult(Err(())),
        metrics::INTERFACE_DESTRUCTION_RECOVERY_OUTCOME_METRIC_ID,
        vec![RecoveryOutcome::Failure as u32] ;
        "destroy iface does not work after recovery"
    )]
    #[test_case(
        TelemetryEvent::RecoveryEvent {
            reason: RecoveryReason::DestroyIfaceFailure(PhyRecoveryMechanism::PhyReset)
        },
        TelemetryEvent::IfaceDestructionResult(Ok(())),
        TelemetryEvent::IfaceDestructionResult(Ok(())),
        metrics::INTERFACE_DESTRUCTION_RECOVERY_OUTCOME_METRIC_ID,
        vec![RecoveryOutcome::Success as u32] ;
        "destroy iface works after recovery"
    )]
    #[fuchsia::test(add_test_attr = false)]
    fn test_post_recovery_destroy_iface(
        recovery_event: TelemetryEvent,
        post_recovery_event: TelemetryEvent,
        duplicate_check_event: TelemetryEvent,
        expected_metric_id: u32,
        dimensions: Vec<u32>,
    ) {
        test_generic_post_recovery_event(
            recovery_event,
            post_recovery_event,
            duplicate_check_event,
            expected_metric_id,
            dimensions,
        );
    }

    #[test_case(
        TelemetryEvent::RecoveryEvent {
            reason: RecoveryReason::Timeout(TimeoutRecoveryMechanism::PhyReset)
        },
        TelemetryEvent::ConnectResult {
            iface_id: IFACE_ID,
            policy_connect_reason: Some(
                client::types::ConnectReason::RetryAfterFailedConnectAttempt,
            ),
            result: fake_connect_result(fidl_ieee80211::StatusCode::Success),
            multiple_bss_candidates: true,
            ap_state: random_bss_description!(Wpa2).into(),
            network_is_likely_hidden: true,
        },
        TelemetryEvent::ConnectResult {
            iface_id: IFACE_ID,
            policy_connect_reason: Some(
                client::types::ConnectReason::RetryAfterFailedConnectAttempt,
            ),
            result: fake_connect_result(fidl_ieee80211::StatusCode::Success),
            multiple_bss_candidates: true,
            ap_state: random_bss_description!(Wpa2).into(),
            network_is_likely_hidden: true,
        },
        metrics::TIMEOUT_RECOVERY_OUTCOME_METRIC_ID,
        vec![RecoveryOutcome::Success as u32, TimeoutRecoveryMechanism::PhyReset as u32] ;
        "Connect works after recovery"
    )]
    #[test_case(
        TelemetryEvent::RecoveryEvent {
            reason: RecoveryReason::Timeout(TimeoutRecoveryMechanism::PhyReset)
        },
        TelemetryEvent::Disconnected {
            track_subsequent_downtime: false,
            info: fake_disconnect_info(),
        },
        TelemetryEvent::Disconnected {
            track_subsequent_downtime: false,
            info: fake_disconnect_info(),
        },
        metrics::TIMEOUT_RECOVERY_OUTCOME_METRIC_ID,
        vec![RecoveryOutcome::Success as u32, TimeoutRecoveryMechanism::PhyReset as u32] ;
        "Disconnect works after recovery"
    )]
    #[test_case(
        TelemetryEvent::RecoveryEvent {
            reason: RecoveryReason::Timeout(TimeoutRecoveryMechanism::PhyReset)
        },
        TelemetryEvent::StopAp { enabled_duration: zx::MonotonicDuration::from_seconds(0) },
        TelemetryEvent::StopAp { enabled_duration: zx::MonotonicDuration::from_seconds(0) },
        metrics::TIMEOUT_RECOVERY_OUTCOME_METRIC_ID,
        vec![RecoveryOutcome::Success as u32, TimeoutRecoveryMechanism::PhyReset as u32] ;
        "Stop AP works after recovery"
    )]
    #[test_case(
        TelemetryEvent::RecoveryEvent {
            reason: RecoveryReason::Timeout(TimeoutRecoveryMechanism::PhyReset)
        },
        TelemetryEvent::StartApResult(Ok(())),
        TelemetryEvent::StartApResult(Ok(())),
        metrics::TIMEOUT_RECOVERY_OUTCOME_METRIC_ID,
        vec![RecoveryOutcome::Success as u32, TimeoutRecoveryMechanism::PhyReset as u32] ;
        "Start AP works after recovery"
    )]
    #[test_case(
        TelemetryEvent::RecoveryEvent {
            reason: RecoveryReason::Timeout(TimeoutRecoveryMechanism::DestroyIface)
        },
        TelemetryEvent::ScanEvent {
            inspect_data: ScanEventInspectData::default(),
            scan_defects: vec![]
        },
        TelemetryEvent::ScanEvent {
            inspect_data: ScanEventInspectData::default(),
            scan_defects: vec![]
        },
        metrics::TIMEOUT_RECOVERY_OUTCOME_METRIC_ID,
        vec![RecoveryOutcome::Success as u32, TimeoutRecoveryMechanism::DestroyIface as u32] ;
        "Scan works after timeout recovery"
    )]
    #[test_case(
        TelemetryEvent::RecoveryEvent {
            reason: RecoveryReason::Timeout(TimeoutRecoveryMechanism::PhyReset)
        },
        TelemetryEvent::SmeTimeout { source: TimeoutSource::Scan },
        TelemetryEvent::SmeTimeout { source: TimeoutSource::Scan },
        metrics::TIMEOUT_RECOVERY_OUTCOME_METRIC_ID,
        vec![RecoveryOutcome::Failure as u32, TimeoutRecoveryMechanism::PhyReset as u32] ;
        "SME timeout after recovery"
    )]
    #[fuchsia::test(add_test_attr = false)]
    fn test_post_recovery_timeout(
        recovery_event: TelemetryEvent,
        post_recovery_event: TelemetryEvent,
        duplicate_check_event: TelemetryEvent,
        expected_metric_id: u32,
        dimensions: Vec<u32>,
    ) {
        test_generic_post_recovery_event(
            recovery_event,
            post_recovery_event,
            duplicate_check_event,
            expected_metric_id,
            dimensions,
        );
    }

    #[fuchsia::test]
    fn test_log_scan_request_fulfillment_time() {
        let (mut test_helper, mut test_fut) = setup_test();

        // Send a scan fulfillment duration
        let duration = zx::MonotonicDuration::from_seconds(15);
        test_helper.telemetry_sender.send(TelemetryEvent::ScanRequestFulfillmentTime {
            duration,
            reason: client::scan::ScanReason::ClientRequest,
        });

        // Run the telemetry loop until it stalls.
        assert_variant!(test_helper.advance_test_fut(&mut test_fut), Poll::Pending);

        // Expect that Cobalt has been notified of the scan fulfillment metric
        test_helper.drain_cobalt_events(&mut test_fut);
        let logged_metrics = test_helper
            .get_logged_metrics(metrics::SUCCESSFUL_SCAN_REQUEST_FULFILLMENT_TIME_METRIC_ID);
        assert_eq!(logged_metrics.len(), 1);
        assert_eq!(
            logged_metrics[0].event_codes,
            vec![
                metrics::ConnectivityWlanMetricDimensionScanFulfillmentTime::LessThanTwentyOneSeconds as u32,
                metrics::ConnectivityWlanMetricDimensionScanReason::ClientRequest as u32
            ]
        );
    }

    #[fuchsia::test]
    fn test_log_scan_queue_statistics() {
        let (mut test_helper, mut test_fut) = setup_test();

        // Send a scan queue report
        test_helper.telemetry_sender.send(TelemetryEvent::ScanQueueStatistics {
            fulfilled_requests: 4,
            remaining_requests: 12,
        });

        // Run the telemetry loop until it stalls.
        assert_variant!(test_helper.advance_test_fut(&mut test_fut), Poll::Pending);

        // Expect that Cobalt has been notified of the scan queue metrics
        test_helper.drain_cobalt_events(&mut test_fut);
        let logged_metrics = test_helper
            .get_logged_metrics(metrics::SCAN_QUEUE_STATISTICS_AFTER_COMPLETED_SCAN_METRIC_ID);
        assert_eq!(logged_metrics.len(), 1);
        assert_eq!(
            logged_metrics[0].event_codes,
            vec![
                metrics::ConnectivityWlanMetricDimensionScanRequestsFulfilled::Four as u32,
                metrics::ConnectivityWlanMetricDimensionScanRequestsRemaining::TenToFourteen as u32
            ]
        );
    }

    #[fuchsia::test]
    fn test_log_post_connection_score_deltas_by_signal_and_post_connection_rssi_deltas() {
        let (mut test_helper, mut test_fut) = setup_test();
        let connect_time = fasync::MonotonicInstant::from_nanos(31_000_000_000);

        let signals_deque: VecDeque<client::types::TimestampedSignal> = VecDeque::from_iter([
            client::types::TimestampedSignal {
                signal: client::types::Signal { rssi_dbm: -70, snr_db: 10 },
                time: connect_time + zx::MonotonicDuration::from_millis(500),
            },
            client::types::TimestampedSignal {
                signal: client::types::Signal { rssi_dbm: -50, snr_db: 30 },
                time: connect_time + zx::MonotonicDuration::from_seconds(4),
            },
            client::types::TimestampedSignal {
                signal: client::types::Signal { rssi_dbm: -30, snr_db: 60 },
                time: connect_time + zx::MonotonicDuration::from_seconds(9),
            },
            client::types::TimestampedSignal {
                signal: client::types::Signal { rssi_dbm: -10, snr_db: 80 },
                time: connect_time + zx::MonotonicDuration::from_seconds(20),
            },
        ]);
        let signals = HistoricalList(signals_deque);
        let signal_at_connect = client::types::Signal { rssi_dbm: -90, snr_db: 0 };

        test_helper.telemetry_sender.send(TelemetryEvent::PostConnectionSignals {
            connect_time,
            signal_at_connect,
            signals,
        });

        // Catch logged score delta metrics
        test_helper.drain_cobalt_events(&mut test_fut);
        let logged_metrics = test_helper.get_logged_metrics(
            metrics::AVERAGE_SCORE_DELTA_AFTER_CONNECTION_BY_INITIAL_SCORE_METRIC_ID,
        );

        use metrics::AverageScoreDeltaAfterConnectionByInitialScoreMetricDimensionTimeSinceConnect as DurationDimension;

        // Logged metrics for one, five, ten, and thirty seconds.
        assert_eq!(logged_metrics.len(), 4);

        let mut prev_score = 0;
        // Verify one second average delta
        assert_eq!(logged_metrics[0].event_codes[1], DurationDimension::OneSecond as u32);
        assert_variant!(&logged_metrics[0].payload, MetricEventPayload::IntegerValue(delta) => {
            assert_gt!(*delta, prev_score);
            prev_score = *delta;
        });

        // Verify five second average delta
        assert_eq!(logged_metrics[1].event_codes[1], DurationDimension::FiveSeconds as u32);
        assert_variant!(&logged_metrics[1].payload, MetricEventPayload::IntegerValue(delta) => {
            assert_gt!(*delta, prev_score);
            prev_score = *delta;
        });
        // Verify ten second average delta
        assert_eq!(logged_metrics[2].event_codes[1], DurationDimension::TenSeconds as u32);
        assert_variant!(&logged_metrics[2].payload, MetricEventPayload::IntegerValue(delta) => {
            assert_gt!(*delta, prev_score);
            prev_score = *delta;
        });
        // Verify thirty second average delta
        assert_eq!(logged_metrics[3].event_codes[1], DurationDimension::ThirtySeconds as u32);
        assert_variant!(&logged_metrics[3].payload, MetricEventPayload::IntegerValue(delta) => {
            assert_gt!(*delta, prev_score);
        });

        // Catch logged RSSI delta metrics
        test_helper.drain_cobalt_events(&mut test_fut);
        let logged_metrics = test_helper.get_logged_metrics(
            metrics::AVERAGE_RSSI_DELTA_AFTER_CONNECTION_BY_INITIAL_RSSI_METRIC_ID,
        );
        // Logged metrics for one, five, ten, and thirty seconds.
        assert_eq!(logged_metrics.len(), 4);

        // Verify one second average RSSI delta
        assert_eq!(logged_metrics[0].event_codes[1], DurationDimension::OneSecond as u32);
        assert_variant!(&logged_metrics[0].payload, MetricEventPayload::IntegerValue(delta) => {
            assert_eq!(*delta, 10);
        });

        // Verify five second average RSSI delta
        assert_eq!(logged_metrics[1].event_codes[1], DurationDimension::FiveSeconds as u32);
        assert_variant!(&logged_metrics[1].payload, MetricEventPayload::IntegerValue(delta) => {
            assert_eq!(*delta, 20);
        });
        // Verify ten second average RSSI delta
        assert_eq!(logged_metrics[2].event_codes[1], DurationDimension::TenSeconds as u32);
        assert_variant!(&logged_metrics[2].payload, MetricEventPayload::IntegerValue(delta) => {
            assert_eq!(*delta, 30);
        });
        // Verify thirty second average RSSI delta
        assert_eq!(logged_metrics[3].event_codes[1], DurationDimension::ThirtySeconds as u32);
        assert_variant!(&logged_metrics[3].payload, MetricEventPayload::IntegerValue(delta) => {
            assert_eq!(*delta, 40);
        });
    }

    #[fuchsia::test]
    fn test_log_pre_disconnect_score_deltas_by_signal_and_pre_disconnect_rssi_deltas() {
        let (mut test_helper, mut test_fut) = setup_test();
        // 31 seconds
        let final_score_time = fasync::MonotonicInstant::from_nanos(31_000_000_000);

        let signals_deque: VecDeque<client::types::TimestampedSignal> = VecDeque::from_iter([
            client::types::TimestampedSignal {
                signal: client::types::Signal { rssi_dbm: -10, snr_db: 80 },
                time: final_score_time - zx::MonotonicDuration::from_seconds(20),
            },
            client::types::TimestampedSignal {
                signal: client::types::Signal { rssi_dbm: -30, snr_db: 60 },
                time: final_score_time - zx::MonotonicDuration::from_seconds(9),
            },
            client::types::TimestampedSignal {
                signal: client::types::Signal { rssi_dbm: -50, snr_db: 30 },
                time: final_score_time - zx::MonotonicDuration::from_seconds(4),
            },
            client::types::TimestampedSignal {
                signal: client::types::Signal { rssi_dbm: -70, snr_db: 10 },
                time: final_score_time - zx::MonotonicDuration::from_millis(500),
            },
            client::types::TimestampedSignal {
                signal: client::types::Signal { rssi_dbm: -90, snr_db: 0 },
                time: final_score_time,
            },
        ]);
        let signals = HistoricalList(signals_deque);

        let disconnect_info = DisconnectInfo {
            connected_duration: AVERAGE_SCORE_DELTA_MINIMUM_DURATION,
            signals,
            ..fake_disconnect_info()
        };
        test_helper.telemetry_sender.send(TelemetryEvent::Disconnected {
            track_subsequent_downtime: false,
            info: disconnect_info,
        });

        // Catch logged score delta metrics
        test_helper.drain_cobalt_events(&mut test_fut);
        let logged_metrics = test_helper.get_logged_metrics(
            metrics::AVERAGE_SCORE_DELTA_BEFORE_DISCONNECT_BY_FINAL_SCORE_METRIC_ID,
        );

        use metrics::AverageScoreDeltaBeforeDisconnectByFinalScoreMetricDimensionTimeUntilDisconnect as DurationDimension;

        // Logged metrics for one, five, ten, and thirty seconds.
        assert_eq!(logged_metrics.len(), 4);

        let mut prev_score = 0;
        // Verify one second average delta
        assert_eq!(logged_metrics[0].event_codes[1], DurationDimension::OneSecond as u32);
        assert_variant!(&logged_metrics[0].payload, MetricEventPayload::IntegerValue(delta) => {
            assert_gt!(*delta, prev_score);
            prev_score = *delta;
        });

        // Verify five second average delta
        assert_eq!(logged_metrics[1].event_codes[1], DurationDimension::FiveSeconds as u32);
        assert_variant!(&logged_metrics[1].payload, MetricEventPayload::IntegerValue(delta) => {
            assert_gt!(*delta, prev_score);
            prev_score = *delta;
        });
        // Verify ten second average delta
        assert_eq!(logged_metrics[2].event_codes[1], DurationDimension::TenSeconds as u32);
        assert_variant!(&logged_metrics[2].payload, MetricEventPayload::IntegerValue(delta) => {
            assert_gt!(*delta, prev_score);
            prev_score = *delta;
        });
        // Verify thirty second average delta
        assert_eq!(logged_metrics[3].event_codes[1], DurationDimension::ThirtySeconds as u32);
        assert_variant!(&logged_metrics[3].payload, MetricEventPayload::IntegerValue(delta) => {
            assert_gt!(*delta, prev_score);
        });

        // Catch logged RSSI delta metrics
        test_helper.drain_cobalt_events(&mut test_fut);
        let logged_metrics = test_helper.get_logged_metrics(
            metrics::AVERAGE_RSSI_DELTA_BEFORE_DISCONNECT_BY_FINAL_RSSI_METRIC_ID,
        );
        // Logged metrics for one, five, ten, and thirty seconds.
        assert_eq!(logged_metrics.len(), 4);

        // Verify one second average RSSI delta
        assert_eq!(logged_metrics[0].event_codes[1], DurationDimension::OneSecond as u32);
        assert_variant!(&logged_metrics[0].payload, MetricEventPayload::IntegerValue(delta) => {
            assert_eq!(*delta, 10);
        });

        // Verify five second average RSSI delta
        assert_eq!(logged_metrics[1].event_codes[1], DurationDimension::FiveSeconds as u32);
        assert_variant!(&logged_metrics[1].payload, MetricEventPayload::IntegerValue(delta) => {
            assert_eq!(*delta, 20);
        });
        // Verify ten second average RSSI delta
        assert_eq!(logged_metrics[2].event_codes[1], DurationDimension::TenSeconds as u32);
        assert_variant!(&logged_metrics[2].payload, MetricEventPayload::IntegerValue(delta) => {
            assert_eq!(*delta, 30);
        });
        // Verify thirty second average RSSI delta
        assert_eq!(logged_metrics[3].event_codes[1], DurationDimension::ThirtySeconds as u32);
        assert_variant!(&logged_metrics[3].payload, MetricEventPayload::IntegerValue(delta) => {
            assert_eq!(*delta, 40);
        });

        // Record a disconnect shorter than the minimum required duration
        let disconnect_info = DisconnectInfo {
            connected_duration: AVERAGE_SCORE_DELTA_MINIMUM_DURATION
                - zx::MonotonicDuration::from_seconds(1),
            ..fake_disconnect_info()
        };
        test_helper.telemetry_sender.send(TelemetryEvent::Disconnected {
            track_subsequent_downtime: false,
            info: disconnect_info,
        });
        test_helper.drain_cobalt_events(&mut test_fut);

        // No additional metrics should be logged.
        let logged_metrics = test_helper.get_logged_metrics(
            metrics::AVERAGE_SCORE_DELTA_BEFORE_DISCONNECT_BY_FINAL_SCORE_METRIC_ID,
        );
        assert_eq!(logged_metrics.len(), 4);
        let logged_metrics = test_helper.get_logged_metrics(
            metrics::AVERAGE_RSSI_DELTA_BEFORE_DISCONNECT_BY_FINAL_RSSI_METRIC_ID,
        );
        assert_eq!(logged_metrics.len(), 4);
    }

    #[fuchsia::test]
    fn test_log_network_selection_metrics() {
        let (mut test_helper, mut test_fut) = setup_test();

        // Send network selection event
        test_helper.telemetry_sender.send(TelemetryEvent::NetworkSelectionDecision {
            network_selection_type: NetworkSelectionType::Undirected,
            num_candidates: Ok(3),
            selected_count: 2,
        });

        // Run the telemetry loop until it stalls.
        assert_variant!(test_helper.advance_test_fut(&mut test_fut), Poll::Pending);
        test_helper.drain_cobalt_events(&mut test_fut);

        // Verify the network selection is counted
        let logged_metrics =
            test_helper.get_logged_metrics(metrics::NETWORK_SELECTION_COUNT_METRIC_ID);
        assert_eq!(logged_metrics.len(), 1);
        assert_eq!(logged_metrics[0].payload, MetricEventPayload::Count(1));

        // Verify the number of selected candidates is recorded
        let logged_metrics =
            test_helper.get_logged_metrics(metrics::NUM_NETWORKS_SELECTED_METRIC_ID);
        assert_eq!(logged_metrics.len(), 1);
        assert_eq!(logged_metrics[0].payload, MetricEventPayload::IntegerValue(2));

        // Send a network selection metric where there were 0 candidates.
        test_helper.telemetry_sender.send(TelemetryEvent::NetworkSelectionDecision {
            network_selection_type: NetworkSelectionType::Undirected,
            num_candidates: Ok(0),
            selected_count: 0,
        });

        // Run the telemetry loop until it stalls.
        assert_variant!(test_helper.advance_test_fut(&mut test_fut), Poll::Pending);
        test_helper.drain_cobalt_events(&mut test_fut);

        // Verify the network selection is counted
        let logged_metrics =
            test_helper.get_logged_metrics(metrics::NETWORK_SELECTION_COUNT_METRIC_ID);
        assert_eq!(logged_metrics.len(), 2);

        // The number of selected networks should not be recorded, since there were no candidates
        // to select from
        let logged_metrics =
            test_helper.get_logged_metrics(metrics::NUM_NETWORKS_SELECTED_METRIC_ID);
        assert_eq!(logged_metrics.len(), 1);
    }

    #[fuchsia::test]
    fn test_log_bss_selection_metrics() {
        let (mut test_helper, mut test_fut) = setup_test();

        // Send BSS selection result event with 3 candidate, multi-bss, one selected
        let selected_candidate_2g = client::types::ScannedCandidate {
            bss: client::types::Bss {
                channel: client::types::WlanChan::new(1, wlan_common::channel::Cbw::Cbw20),
                ..generate_random_bss()
            },
            ..generate_random_scanned_candidate()
        };
        let candidate_2g = client::types::ScannedCandidate {
            bss: client::types::Bss {
                channel: client::types::WlanChan::new(1, wlan_common::channel::Cbw::Cbw20),
                ..generate_random_bss()
            },
            ..generate_random_scanned_candidate()
        };
        let candidate_5g = client::types::ScannedCandidate {
            bss: client::types::Bss {
                channel: client::types::WlanChan::new(36, wlan_common::channel::Cbw::Cbw40),
                ..generate_random_bss()
            },
            ..generate_random_scanned_candidate()
        };
        let scored_candidates =
            vec![(selected_candidate_2g.clone(), 70), (candidate_2g, 60), (candidate_5g, 50)];

        test_helper.telemetry_sender.send(TelemetryEvent::BssSelectionResult {
            reason: client::types::ConnectReason::FidlConnectRequest,
            scored_candidates: scored_candidates.clone(),
            selected_candidate: Some((selected_candidate_2g, 70)),
        });

        test_helper.drain_cobalt_events(&mut test_fut);

        let fidl_connect_event_code = vec![
            metrics::PolicyConnectionAttemptMigratedMetricDimensionReason::FidlConnectRequest
                as u32,
        ];
        // Check that the BSS selection occurrence metrics are logged
        let logged_metrics = test_helper.get_logged_metrics(metrics::BSS_SELECTION_COUNT_METRIC_ID);
        assert_eq!(logged_metrics.len(), 1);
        assert_eq!(logged_metrics[0].event_codes, Vec::<u32>::new());
        assert_eq!(logged_metrics[0].payload, MetricEventPayload::Count(1));

        let logged_metrics =
            test_helper.get_logged_metrics(metrics::BSS_SELECTION_COUNT_DETAILED_METRIC_ID);
        assert_eq!(logged_metrics.len(), 1);
        assert_eq!(logged_metrics[0].event_codes, fidl_connect_event_code);
        assert_eq!(logged_metrics[0].payload, MetricEventPayload::Count(1));

        // Check that the candidate count metrics are logged
        let logged_metrics =
            test_helper.get_logged_metrics(metrics::NUM_BSS_CONSIDERED_IN_SELECTION_METRIC_ID);
        assert_eq!(logged_metrics.len(), 1);
        assert_eq!(logged_metrics[0].event_codes, Vec::<u32>::new());
        assert_eq!(logged_metrics[0].payload, MetricEventPayload::IntegerValue(3));

        let logged_metrics = test_helper
            .get_logged_metrics(metrics::NUM_BSS_CONSIDERED_IN_SELECTION_DETAILED_METRIC_ID);
        assert_eq!(logged_metrics.len(), 1);
        assert_eq!(logged_metrics[0].event_codes, fidl_connect_event_code);
        assert_eq!(logged_metrics[0].payload, MetricEventPayload::IntegerValue(3));

        // Check that all candidate scores are logged
        let logged_metrics = test_helper.get_logged_metrics(metrics::BSS_CANDIDATE_SCORE_METRIC_ID);
        assert_eq!(logged_metrics.len(), 3);
        for i in 0..3 {
            assert_eq!(
                logged_metrics[i].payload,
                MetricEventPayload::IntegerValue(scored_candidates[i].1 as i64)
            )
        }

        // Check that unique network count is logged
        let logged_metrics = test_helper
            .get_logged_metrics(metrics::NUM_NETWORKS_REPRESENTED_IN_BSS_SELECTION_METRIC_ID);
        assert_eq!(logged_metrics.len(), 1);
        assert_eq!(logged_metrics[0].event_codes, fidl_connect_event_code);
        assert_eq!(logged_metrics[0].payload, MetricEventPayload::IntegerValue(3));

        // Check that selected candidate score is logged
        let logged_metrics = test_helper.get_logged_metrics(metrics::SELECTED_BSS_SCORE_METRIC_ID);
        assert_eq!(logged_metrics.len(), 1);
        assert_eq!(logged_metrics[0].payload, MetricEventPayload::IntegerValue(70));

        // Check that runner-up score delta is logged
        let logged_metrics =
            test_helper.get_logged_metrics(metrics::RUNNER_UP_CANDIDATE_SCORE_DELTA_METRIC_ID);
        assert_eq!(logged_metrics.len(), 1);
        assert_eq!(logged_metrics[0].payload, MetricEventPayload::IntegerValue(10));

        // Check that GHz score delta is logged
        let logged_metrics =
            test_helper.get_logged_metrics(metrics::BEST_CANDIDATES_GHZ_SCORE_DELTA_METRIC_ID);
        assert_eq!(logged_metrics.len(), 1);
        assert_eq!(logged_metrics[0].payload, MetricEventPayload::IntegerValue(-20));

        // Check that GHz bands present in selection is logged
        let logged_metrics =
            test_helper.get_logged_metrics(metrics::GHZ_BANDS_AVAILABLE_IN_BSS_SELECTION_METRIC_ID);
        assert_eq!(logged_metrics.len(), 1);
        assert_eq!(
            logged_metrics[0].event_codes,
            vec![metrics::GhzBandsAvailableInBssSelectionMetricDimensionBands::MultiBand as u32]
        );
        assert_eq!(logged_metrics[0].payload, MetricEventPayload::Count(1));
    }

    #[fuchsia::test]
    fn test_log_bss_selection_metrics_none_selected() {
        let (mut test_helper, mut test_fut) = setup_test();

        test_helper.telemetry_sender.send(TelemetryEvent::BssSelectionResult {
            reason: client::types::ConnectReason::FidlConnectRequest,
            scored_candidates: vec![],
            selected_candidate: None,
        });

        test_helper.drain_cobalt_events(&mut test_fut);

        // Check that only the BSS selection occurrence and candidate count metrics are recorded
        assert!(!test_helper.get_logged_metrics(metrics::BSS_SELECTION_COUNT_METRIC_ID).is_empty());
        assert!(!test_helper
            .get_logged_metrics(metrics::BSS_SELECTION_COUNT_DETAILED_METRIC_ID)
            .is_empty());
        assert!(!test_helper
            .get_logged_metrics(metrics::NUM_BSS_CONSIDERED_IN_SELECTION_METRIC_ID)
            .is_empty());
        assert!(!test_helper
            .get_logged_metrics(metrics::NUM_BSS_CONSIDERED_IN_SELECTION_DETAILED_METRIC_ID)
            .is_empty());
        assert!(test_helper.get_logged_metrics(metrics::BSS_CANDIDATE_SCORE_METRIC_ID).is_empty());
        assert!(test_helper
            .get_logged_metrics(metrics::NUM_NETWORKS_REPRESENTED_IN_BSS_SELECTION_METRIC_ID)
            .is_empty());
        assert!(test_helper
            .get_logged_metrics(metrics::RUNNER_UP_CANDIDATE_SCORE_DELTA_METRIC_ID)
            .is_empty());
        assert!(test_helper
            .get_logged_metrics(metrics::NUM_NETWORKS_REPRESENTED_IN_BSS_SELECTION_METRIC_ID)
            .is_empty());
        assert!(test_helper
            .get_logged_metrics(metrics::BEST_CANDIDATES_GHZ_SCORE_DELTA_METRIC_ID)
            .is_empty());
        assert!(test_helper
            .get_logged_metrics(metrics::GHZ_BANDS_AVAILABLE_IN_BSS_SELECTION_METRIC_ID)
            .is_empty());
    }

    #[fuchsia::test]
    fn test_log_bss_selection_metrics_runner_up_delta_not_recorded() {
        let (mut test_helper, mut test_fut) = setup_test();

        let scored_candidates = vec![
            (generate_random_scanned_candidate(), 90),
            (generate_random_scanned_candidate(), 60),
            (generate_random_scanned_candidate(), 50),
        ];

        test_helper.telemetry_sender.send(TelemetryEvent::BssSelectionResult {
            reason: client::types::ConnectReason::FidlConnectRequest,
            scored_candidates,
            // Report that the selected candidate was not the highest scoring candidate.
            selected_candidate: Some((generate_random_scanned_candidate(), 60)),
        });

        test_helper.drain_cobalt_events(&mut test_fut);

        // No delta metric should be recorded
        assert!(test_helper
            .get_logged_metrics(metrics::RUNNER_UP_CANDIDATE_SCORE_DELTA_METRIC_ID)
            .is_empty());
    }

    #[fuchsia::test]
    fn test_log_connection_score_average_long_duration() {
        let (mut test_helper, mut test_fut) = setup_test();
        let now = fasync::MonotonicInstant::now();
        let signals = vec![
            client::types::TimestampedSignal {
                signal: client::types::Signal { rssi_dbm: -60, snr_db: 30 },
                time: now,
            },
            client::types::TimestampedSignal {
                signal: client::types::Signal { rssi_dbm: -60, snr_db: 30 },
                time: now,
            },
            client::types::TimestampedSignal {
                signal: client::types::Signal { rssi_dbm: -80, snr_db: 10 },
                time: now,
            },
            client::types::TimestampedSignal {
                signal: client::types::Signal { rssi_dbm: -80, snr_db: 10 },
                time: now,
            },
        ];

        test_helper.telemetry_sender.send(TelemetryEvent::LongDurationSignals { signals });
        test_helper.drain_cobalt_events(&mut test_fut);

        let logged_metrics =
            test_helper.get_logged_metrics(metrics::CONNECTION_SCORE_AVERAGE_METRIC_ID);
        assert_eq!(logged_metrics.len(), 1);
        assert_eq!(
            logged_metrics[0].event_codes,
            vec![metrics::ConnectionScoreAverageMetricDimensionDuration::LongDuration as u32]
        );
        assert_eq!(logged_metrics[0].payload, MetricEventPayload::IntegerValue(55));

        // Ensure an empty score list would not cause an arithmetic error.
        test_helper.telemetry_sender.send(TelemetryEvent::LongDurationSignals { signals: vec![] });
        test_helper.drain_cobalt_events(&mut test_fut);
        assert_eq!(
            test_helper.get_logged_metrics(metrics::CONNECTION_SCORE_AVERAGE_METRIC_ID).len(),
            1
        );
    }

    #[fuchsia::test]
    fn test_log_connection_rssi_average_long_duration() {
        let (mut test_helper, mut test_fut) = setup_test();
        let now = fasync::MonotonicInstant::now();
        let signals = vec![
            client::types::TimestampedSignal {
                signal: client::types::Signal { rssi_dbm: -60, snr_db: 30 },
                time: now,
            },
            client::types::TimestampedSignal {
                signal: client::types::Signal { rssi_dbm: -60, snr_db: 30 },
                time: now,
            },
            client::types::TimestampedSignal {
                signal: client::types::Signal { rssi_dbm: -80, snr_db: 10 },
                time: now,
            },
            client::types::TimestampedSignal {
                signal: client::types::Signal { rssi_dbm: -80, snr_db: 10 },
                time: now,
            },
        ];

        test_helper.telemetry_sender.send(TelemetryEvent::LongDurationSignals { signals });
        test_helper.drain_cobalt_events(&mut test_fut);

        let logged_metrics =
            test_helper.get_logged_metrics(metrics::CONNECTION_RSSI_AVERAGE_METRIC_ID);
        assert_eq!(logged_metrics.len(), 1);
        assert_eq!(
            logged_metrics[0].event_codes,
            vec![metrics::ConnectionScoreAverageMetricDimensionDuration::LongDuration as u32]
        );
        assert_eq!(logged_metrics[0].payload, MetricEventPayload::IntegerValue(-70));

        // Ensure an empty score list would not cause an arithmetic error.
        test_helper.telemetry_sender.send(TelemetryEvent::LongDurationSignals { signals: vec![] });
        test_helper.drain_cobalt_events(&mut test_fut);
        assert_eq!(
            test_helper.get_logged_metrics(metrics::CONNECTION_RSSI_AVERAGE_METRIC_ID).len(),
            1
        );
    }

    #[test_case(
        TimeoutSource::Scan,
        metrics::SmeOperationTimeoutMetricDimensionStalledOperation::Scan_ ;
        "log scan timeout"
    )]
    #[test_case(
        TimeoutSource::Connect,
        metrics::SmeOperationTimeoutMetricDimensionStalledOperation::Connect_ ;
        "log connect"
    )]
    #[test_case(
        TimeoutSource::Disconnect,
        metrics::SmeOperationTimeoutMetricDimensionStalledOperation::Disconnect_ ;
        "log disconnect timeout"
    )]
    #[test_case(
        TimeoutSource::ClientStatus,
        metrics::SmeOperationTimeoutMetricDimensionStalledOperation::ClientStatus_ ;
        "log client status timeout"
    )]
    #[test_case(
        TimeoutSource::WmmStatus,
        metrics::SmeOperationTimeoutMetricDimensionStalledOperation::WmmStatus_ ;
        "log WMM status timeout"
    )]
    #[test_case(
        TimeoutSource::ApStart,
        metrics::SmeOperationTimeoutMetricDimensionStalledOperation::ApStart_ ;
        "log AP start timeout"
    )]
    #[test_case(
        TimeoutSource::ApStop,
        metrics::SmeOperationTimeoutMetricDimensionStalledOperation::ApStop_ ;
        "log Ap stop timeout"
    )]
    #[test_case(
        TimeoutSource::ApStatus,
        metrics::SmeOperationTimeoutMetricDimensionStalledOperation::ApStatus_ ;
        "log AP status timeout"
    )]
    #[test_case(
        TimeoutSource::GetCounterStats,
        metrics::SmeOperationTimeoutMetricDimensionStalledOperation::GetCounterStats_ ;
        "log counter stats timeout"
    )]
    #[test_case(
        TimeoutSource::GetHistogramStats,
        metrics::SmeOperationTimeoutMetricDimensionStalledOperation::GetHistogramStats_ ;
        "log histogram stats timeout"
    )]
    #[fuchsia::test(add_test_attr = false)]
    fn test_log_sme_timeout(
        source: TimeoutSource,
        expected_dimension: metrics::SmeOperationTimeoutMetricDimensionStalledOperation,
    ) {
        let (mut test_helper, mut test_fut) = setup_test();

        // Send the timeout event
        test_helper.telemetry_sender.send(TelemetryEvent::SmeTimeout { source });

        // Run the telemetry loop until it stalls.
        assert_variant!(test_helper.advance_test_fut(&mut test_fut), Poll::Pending);

        // Expect that Cobalt has been notified of the timeout
        assert_variant!(
            test_helper.exec.run_until_stalled(&mut test_helper.cobalt_1dot1_stream.next()),
            Poll::Ready(Some(Ok(fidl_fuchsia_metrics::MetricEventLoggerRequest::LogOccurrence {
                metric_id, event_codes, responder, ..
            }))) => {
                assert_eq!(metric_id, metrics::SME_OPERATION_TIMEOUT_METRIC_ID);
                assert_eq!(event_codes, vec![expected_dimension.as_event_code()]);

                assert!(responder.send(Ok(())).is_ok());
        });
    }

    struct TestHelper {
        telemetry_sender: TelemetrySender,
        inspector: Inspector,
        monitor_svc_stream: fidl_fuchsia_wlan_device_service::DeviceMonitorRequestStream,
        telemetry_svc_stream: Option<fidl_fuchsia_wlan_sme::TelemetryRequestStream>,
        cobalt_1dot1_stream: fidl_fuchsia_metrics::MetricEventLoggerRequestStream,
        persistence_stream: mpsc::Receiver<String>,
        counter_stats_resp:
            Option<Box<dyn Fn() -> fidl_fuchsia_wlan_sme::TelemetryGetCounterStatsResult>>,
        /// As requests to Cobalt are responded to via `self.drain_cobalt_events()`,
        /// their payloads are drained to this HashMap
        cobalt_events: Vec<MetricEvent>,
        _defect_receiver: mpsc::Receiver<Defect>,

        // Note: keep the executor field last in the struct so it gets dropped last.
        exec: fasync::TestExecutor,
    }

    impl TestHelper {
        /// Advance executor until stalled.
        /// This function will also reply to any ongoing requests to establish an iface
        /// telemetry channel.
        fn advance_test_fut<T>(
            &mut self,
            test_fut: &mut (impl Future<Output = T> + Unpin),
        ) -> Poll<T> {
            let result = self.exec.run_until_stalled(test_fut);
            if let Poll::Ready(Some(Ok(req))) =
                self.exec.run_until_stalled(&mut self.monitor_svc_stream.next())
            {
                match req {
                    fidl_fuchsia_wlan_device_service::DeviceMonitorRequest::GetSmeTelemetry {
                        iface_id,
                        telemetry_server,
                        responder,
                    } => {
                        assert_eq!(iface_id, IFACE_ID);
                        let telemetry_stream = telemetry_server.into_stream();
                        responder.send(Ok(())).expect("Failed to respond to telemetry request");
                        self.telemetry_svc_stream = Some(telemetry_stream);
                        self.exec.run_until_stalled(test_fut)
                    }
                    _ => panic!("Unexpected device monitor request: {:?}", req),
                }
            } else {
                result
            }
        }

        /// Advance executor by `duration`.
        /// This function repeatedly advances the executor by 1 second, triggering
        /// any expired timers and running the test_fut, until `duration` is reached.
        fn advance_by(
            &mut self,
            duration: zx::MonotonicDuration,
            mut test_fut: Pin<&mut impl Future<Output = ()>>,
        ) {
            assert_eq!(
                duration.into_nanos() % STEP_INCREMENT.into_nanos(),
                0,
                "duration {:?} is not divisible by STEP_INCREMENT",
                duration,
            );
            const_assert_eq!(
                TELEMETRY_QUERY_INTERVAL.into_nanos() % STEP_INCREMENT.into_nanos(),
                0
            );

            for _i in 0..(duration.into_nanos() / STEP_INCREMENT.into_nanos()) {
                self.exec.set_fake_time(fasync::MonotonicInstant::after(STEP_INCREMENT));
                let _ = self.exec.wake_expired_timers();
                assert_eq!(self.advance_test_fut(&mut test_fut), Poll::Pending);

                if let Some(telemetry_svc_stream) = &mut self.telemetry_svc_stream {
                    if !telemetry_svc_stream.is_terminated() {
                        respond_iface_counter_stats_req(
                            &mut self.exec,
                            telemetry_svc_stream,
                            &self.counter_stats_resp,
                        );
                    }
                }

                // Respond to any potential Cobalt request, draining their payloads to
                // `self.cobalt_events`.
                self.drain_cobalt_events(&mut test_fut);

                assert_eq!(self.advance_test_fut(&mut test_fut), Poll::Pending);
            }
        }

        fn set_counter_stats_resp(
            &mut self,
            counter_stats_resp: Box<
                dyn Fn() -> fidl_fuchsia_wlan_sme::TelemetryGetCounterStatsResult,
            >,
        ) {
            let _ = self.counter_stats_resp.replace(counter_stats_resp);
        }

        /// Advance executor by some duration until the next time `test_fut` handles periodic
        /// telemetry. This uses `self.advance_by` underneath.
        ///
        /// This function assumes that executor starts test_fut at time 0 (which should be true
        /// if TestHelper is created from `setup_test()`)
        fn advance_to_next_telemetry_checkpoint(
            &mut self,
            test_fut: Pin<&mut impl Future<Output = ()>>,
        ) {
            let now = fasync::MonotonicInstant::now();
            let remaining_interval = TELEMETRY_QUERY_INTERVAL.into_nanos()
                - (now.into_nanos() % TELEMETRY_QUERY_INTERVAL.into_nanos());
            self.advance_by(zx::MonotonicDuration::from_nanos(remaining_interval), test_fut)
        }

        /// Continually execute the future and respond to any incoming Cobalt request with Ok.
        /// Append each metric request payload into `self.cobalt_events`.
        fn drain_cobalt_events(&mut self, test_fut: &mut (impl Future + Unpin)) {
            let mut made_progress = true;
            while made_progress {
                let _result = self.advance_test_fut(test_fut);
                made_progress = false;
                while let Poll::Ready(Some(Ok(req))) =
                    self.exec.run_until_stalled(&mut self.cobalt_1dot1_stream.next())
                {
                    self.cobalt_events.append(&mut req.respond_to_metric_req(Ok(())));
                    made_progress = true;
                }
            }
        }

        fn get_logged_metrics(&self, metric_id: u32) -> Vec<MetricEvent> {
            self.cobalt_events.iter().filter(|ev| ev.metric_id == metric_id).cloned().collect()
        }

        fn send_connected_event(&mut self, ap_state: impl Into<client::types::ApState>) {
            let event = TelemetryEvent::ConnectResult {
                iface_id: IFACE_ID,
                policy_connect_reason: Some(
                    client::types::ConnectReason::RetryAfterFailedConnectAttempt,
                ),
                result: fake_connect_result(fidl_ieee80211::StatusCode::Success),
                multiple_bss_candidates: true,
                ap_state: ap_state.into(),
                network_is_likely_hidden: true,
            };
            self.telemetry_sender.send(event);
        }

        // Empty the cobalt metrics can be stored so that future checks on cobalt metrics can
        // ignore previous values.
        fn clear_cobalt_events(&mut self) {
            self.cobalt_events = Vec::new();
        }

        fn get_persistence_reqs(&mut self) -> Vec<String> {
            let mut persistence_reqs = vec![];
            loop {
                match self.persistence_stream.try_next() {
                    Ok(Some(tag)) => persistence_reqs.push(tag),
                    _ => return persistence_reqs,
                }
            }
        }

        fn get_time_series(
            &mut self,
            test_fut: &mut (impl Future<Output = ()> + Unpin),
        ) -> Arc<Mutex<TimeSeriesStats>> {
            let (sender, mut receiver) = oneshot::channel();
            self.telemetry_sender.send(TelemetryEvent::GetTimeSeries { sender });
            assert_variant!(self.advance_test_fut(test_fut), Poll::Pending);
            self.drain_cobalt_events(test_fut);
            assert_variant!(receiver.try_recv(), Ok(Some(stats)) => stats)
        }
    }

    fn respond_iface_counter_stats_req(
        executor: &mut fasync::TestExecutor,
        telemetry_svc_stream: &mut fidl_fuchsia_wlan_sme::TelemetryRequestStream,
        counter_stats_resp: &Option<
            Box<dyn Fn() -> fidl_fuchsia_wlan_sme::TelemetryGetCounterStatsResult>,
        >,
    ) {
        let telemetry_svc_req_fut = telemetry_svc_stream.try_next();
        let mut telemetry_svc_req_fut = pin!(telemetry_svc_req_fut);
        if let Poll::Ready(Ok(Some(request))) =
            executor.run_until_stalled(&mut telemetry_svc_req_fut)
        {
            match request {
                fidl_fuchsia_wlan_sme::TelemetryRequest::GetCounterStats { responder } => {
                    let resp = match &counter_stats_resp {
                        Some(get_resp) => get_resp(),
                        None => {
                            let seed = fasync::MonotonicInstant::now().into_nanos() as u64;
                            Ok(fidl_fuchsia_wlan_stats::IfaceCounterStats {
                                connection_counters: Some(fake_connection_counters(seed)),
                                ..Default::default()
                            })
                        }
                    };
                    responder
                        .send(resp.as_ref().map_err(|e| *e))
                        .expect("expect sending GetCounterStats response to succeed");
                }
                _ => {
                    panic!("unexpected request: {:?}", request);
                }
            }
        }
    }

    fn respond_iface_histogram_stats_req(
        executor: &mut fasync::TestExecutor,
        telemetry_svc_stream: &mut fidl_fuchsia_wlan_sme::TelemetryRequestStream,
    ) {
        let telemetry_svc_req_fut = telemetry_svc_stream.try_next();
        let mut telemetry_svc_req_fut = pin!(telemetry_svc_req_fut);
        if let Poll::Ready(Ok(Some(request))) =
            executor.run_until_stalled(&mut telemetry_svc_req_fut)
        {
            match request {
                fidl_fuchsia_wlan_sme::TelemetryRequest::GetHistogramStats { responder } => {
                    responder
                        .send(Ok(&fake_iface_histogram_stats()))
                        .expect("expect sending GetHistogramStats response to succeed");
                }
                _ => {
                    panic!("unexpected request: {:?}", request);
                }
            }
        }
    }

    /// Assert two set of Cobalt MetricEvent equal, disregarding the order
    #[track_caller]
    fn assert_eq_cobalt_events(
        mut left: Vec<fidl_fuchsia_metrics::MetricEvent>,
        mut right: Vec<fidl_fuchsia_metrics::MetricEvent>,
    ) {
        left.sort_by(metric_event_cmp);
        right.sort_by(metric_event_cmp);
        assert_eq!(left, right);
    }

    fn metric_event_cmp(
        left: &fidl_fuchsia_metrics::MetricEvent,
        right: &fidl_fuchsia_metrics::MetricEvent,
    ) -> std::cmp::Ordering {
        match left.metric_id.cmp(&right.metric_id) {
            std::cmp::Ordering::Equal => match left.event_codes.len().cmp(&right.event_codes.len())
            {
                std::cmp::Ordering::Equal => (),
                ordering => return ordering,
            },
            ordering => return ordering,
        }

        for i in 0..left.event_codes.len() {
            match left.event_codes[i].cmp(&right.event_codes[i]) {
                std::cmp::Ordering::Equal => (),
                ordering => return ordering,
            }
        }

        match (&left.payload, &right.payload) {
            (MetricEventPayload::Count(v1), MetricEventPayload::Count(v2)) => v1.cmp(v2),
            (MetricEventPayload::IntegerValue(v1), MetricEventPayload::IntegerValue(v2)) => {
                v1.cmp(v2)
            }
            (MetricEventPayload::StringValue(v1), MetricEventPayload::StringValue(v2)) => {
                v1.cmp(v2)
            }
            (MetricEventPayload::Histogram(_), MetricEventPayload::Histogram(_)) => {
                unimplemented!()
            }
            _ => unimplemented!(),
        }
    }

    trait CobaltExt {
        // Respond to MetricEventLoggerRequest and extract its MetricEvent
        fn respond_to_metric_req(
            self,
            result: Result<(), fidl_fuchsia_metrics::Error>,
        ) -> Vec<fidl_fuchsia_metrics::MetricEvent>;
    }

    impl CobaltExt for MetricEventLoggerRequest {
        fn respond_to_metric_req(
            self,
            result: Result<(), fidl_fuchsia_metrics::Error>,
        ) -> Vec<fidl_fuchsia_metrics::MetricEvent> {
            match self {
                Self::LogOccurrence { metric_id, count, event_codes, responder } => {
                    assert!(responder.send(result).is_ok());
                    vec![MetricEvent {
                        metric_id,
                        event_codes,
                        payload: MetricEventPayload::Count(count),
                    }]
                }
                Self::LogInteger { metric_id, value, event_codes, responder } => {
                    assert!(responder.send(result).is_ok());
                    vec![MetricEvent {
                        metric_id,
                        event_codes,
                        payload: MetricEventPayload::IntegerValue(value),
                    }]
                }
                Self::LogIntegerHistogram { metric_id, histogram, event_codes, responder } => {
                    assert!(responder.send(result).is_ok());
                    vec![MetricEvent {
                        metric_id,
                        event_codes,
                        payload: MetricEventPayload::Histogram(histogram),
                    }]
                }
                Self::LogString { metric_id, string_value, event_codes, responder } => {
                    assert!(responder.send(result).is_ok());
                    vec![MetricEvent {
                        metric_id,
                        event_codes,
                        payload: MetricEventPayload::StringValue(string_value),
                    }]
                }
                Self::LogMetricEvents { events, responder } => {
                    assert!(responder.send(result).is_ok());
                    events
                }
            }
        }
    }

    fn setup_test() -> (TestHelper, Pin<Box<impl Future<Output = ()>>>) {
        let mut exec = fasync::TestExecutor::new_with_fake_time();
        exec.set_fake_time(fasync::MonotonicInstant::from_nanos(0));

        let (monitor_svc_proxy, monitor_svc_stream) =
            create_proxy_and_stream::<fidl_fuchsia_wlan_device_service::DeviceMonitorMarker>();

        let (cobalt_1dot1_proxy, cobalt_1dot1_stream) =
            create_proxy_and_stream::<fidl_fuchsia_metrics::MetricEventLoggerMarker>();

        let inspector = Inspector::default();
        let inspect_node = inspector.root().create_child("stats");
        let external_inspect_node = inspector.root().create_child("external");
        let (persistence_req_sender, persistence_stream) = create_inspect_persistence_channel();
        let (defect_sender, _defect_receiver) = mpsc::channel(100);
        let (telemetry_sender, test_fut) = serve_telemetry(
            monitor_svc_proxy,
            cobalt_1dot1_proxy.clone(),
            inspect_node,
            external_inspect_node.create_child("stats"),
            persistence_req_sender,
            defect_sender,
        );
        inspector.root().record(external_inspect_node);
        let mut test_fut = Box::pin(test_fut);

        assert_eq!(exec.run_until_stalled(&mut test_fut), Poll::Pending);

        let test_helper = TestHelper {
            telemetry_sender,
            inspector,
            monitor_svc_stream,
            telemetry_svc_stream: None,
            cobalt_1dot1_stream,
            persistence_stream,
            counter_stats_resp: None,
            cobalt_events: vec![],
            _defect_receiver,
            exec,
        };
        (test_helper, test_fut)
    }

    fn fake_connection_counters(nth_req: u64) -> fidl_fuchsia_wlan_stats::ConnectionCounters {
        fidl_fuchsia_wlan_stats::ConnectionCounters {
            connection_id: Some(1),
            rx_unicast_total: Some(nth_req),
            rx_unicast_drop: Some(0),
            rx_multicast: Some(2 * nth_req),
            tx_total: Some(nth_req),
            tx_drop: Some(0),
            ..Default::default()
        }
    }

    fn fake_iface_histogram_stats() -> fidl_fuchsia_wlan_stats::IfaceHistogramStats {
        fidl_fuchsia_wlan_stats::IfaceHistogramStats {
            noise_floor_histograms: Some(fake_noise_floor_histograms()),
            rssi_histograms: Some(fake_rssi_histograms()),
            rx_rate_index_histograms: Some(fake_rx_rate_index_histograms()),
            snr_histograms: Some(fake_snr_histograms()),
            ..Default::default()
        }
    }

    fn fake_noise_floor_histograms() -> Vec<fidl_fuchsia_wlan_stats::NoiseFloorHistogram> {
        vec![fidl_fuchsia_wlan_stats::NoiseFloorHistogram {
            hist_scope: fidl_fuchsia_wlan_stats::HistScope::PerAntenna,
            antenna_id: Some(Box::new(fidl_fuchsia_wlan_stats::AntennaId {
                freq: fidl_fuchsia_wlan_stats::AntennaFreq::Antenna2G,
                index: 0,
            })),
            noise_floor_samples: vec![fidl_fuchsia_wlan_stats::HistBucket {
                bucket_index: 200,
                num_samples: 999,
            }],
            invalid_samples: 44,
        }]
    }

    fn fake_rssi_histograms() -> Vec<fidl_fuchsia_wlan_stats::RssiHistogram> {
        vec![fidl_fuchsia_wlan_stats::RssiHistogram {
            hist_scope: fidl_fuchsia_wlan_stats::HistScope::PerAntenna,
            antenna_id: Some(Box::new(fidl_fuchsia_wlan_stats::AntennaId {
                freq: fidl_fuchsia_wlan_stats::AntennaFreq::Antenna2G,
                index: 0,
            })),
            rssi_samples: vec![fidl_fuchsia_wlan_stats::HistBucket {
                bucket_index: 230,
                num_samples: 999,
            }],
            invalid_samples: 55,
        }]
    }

    fn fake_rx_rate_index_histograms() -> Vec<fidl_fuchsia_wlan_stats::RxRateIndexHistogram> {
        vec![
            fidl_fuchsia_wlan_stats::RxRateIndexHistogram {
                hist_scope: fidl_fuchsia_wlan_stats::HistScope::Station,
                antenna_id: None,
                rx_rate_index_samples: vec![fidl_fuchsia_wlan_stats::HistBucket {
                    bucket_index: 99,
                    num_samples: 1400,
                }],
                invalid_samples: 22,
            },
            fidl_fuchsia_wlan_stats::RxRateIndexHistogram {
                hist_scope: fidl_fuchsia_wlan_stats::HistScope::PerAntenna,
                antenna_id: Some(Box::new(fidl_fuchsia_wlan_stats::AntennaId {
                    freq: fidl_fuchsia_wlan_stats::AntennaFreq::Antenna5G,
                    index: 1,
                })),
                rx_rate_index_samples: vec![fidl_fuchsia_wlan_stats::HistBucket {
                    bucket_index: 100,
                    num_samples: 1500,
                }],
                invalid_samples: 33,
            },
        ]
    }

    fn fake_snr_histograms() -> Vec<fidl_fuchsia_wlan_stats::SnrHistogram> {
        vec![fidl_fuchsia_wlan_stats::SnrHistogram {
            hist_scope: fidl_fuchsia_wlan_stats::HistScope::PerAntenna,
            antenna_id: Some(Box::new(fidl_fuchsia_wlan_stats::AntennaId {
                freq: fidl_fuchsia_wlan_stats::AntennaFreq::Antenna2G,
                index: 0,
            })),
            snr_samples: vec![fidl_fuchsia_wlan_stats::HistBucket {
                bucket_index: 30,
                num_samples: 999,
            }],
            invalid_samples: 11,
        }]
    }

    fn fake_disconnect_info() -> DisconnectInfo {
        use crate::util::testing::generate_disconnect_info;
        let is_sme_reconnecting = false;
        let fidl_disconnect_info = generate_disconnect_info(is_sme_reconnecting);
        DisconnectInfo {
            connected_duration: zx::MonotonicDuration::from_hours(6),
            is_sme_reconnecting: fidl_disconnect_info.is_sme_reconnecting,
            disconnect_source: fidl_disconnect_info.disconnect_source,
            previous_connect_reason: client::types::ConnectReason::IdleInterfaceAutoconnect,
            ap_state: random_bss_description!(Wpa2).into(),
            signals: HistoricalList::new(8),
        }
    }

    fn fake_connect_result(code: fidl_ieee80211::StatusCode) -> fidl_sme::ConnectResult {
        fidl_sme::ConnectResult { code, is_credential_rejected: false, is_reconnect: false }
    }

    #[fuchsia::test]
    fn test_error_throttling() {
        let exec = fasync::TestExecutor::new_with_fake_time();
        exec.set_fake_time(fasync::MonotonicInstant::from_nanos(0));
        let mut error_logger = ThrottledErrorLogger::new(MINUTES_BETWEEN_COBALT_SYSLOG_WARNINGS);

        // Set the fake time to 61 minutes past 0 time to ensure that messages will be logged.
        exec.set_fake_time(fasync::MonotonicInstant::after(
            fasync::MonotonicDuration::from_minutes(MINUTES_BETWEEN_COBALT_SYSLOG_WARNINGS + 1),
        ));

        // Log an error and verify that no record of it was retained (ie: the error was emitted
        // immediately).
        error_logger.throttle_error(Err(format_err!("")));
        assert!(!error_logger.suppressed_errors.contains_key(&String::from("")));

        // Log another error and verify that the error counter has been incremented.
        error_logger.throttle_error(Err(format_err!("")));
        assert_eq!(error_logger.suppressed_errors[&String::from("")], 1);

        // Advance time again and log another error to verify that the counter resets (ie: log was
        // emitted).
        exec.set_fake_time(fasync::MonotonicInstant::after(
            fasync::MonotonicDuration::from_minutes(MINUTES_BETWEEN_COBALT_SYSLOG_WARNINGS + 1),
        ));
        error_logger.throttle_error(Err(format_err!("")));
        assert!(!error_logger.suppressed_errors.contains_key(&String::from("")));

        // Log another error to verify that the counter begins incrementing again.
        error_logger.throttle_error(Err(format_err!("")));
        assert_eq!(error_logger.suppressed_errors[&String::from("")], 1);
    }
}
