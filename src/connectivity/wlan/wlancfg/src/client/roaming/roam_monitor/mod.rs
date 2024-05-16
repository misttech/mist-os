// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{
        client::{connection_selection::scoring_functions, roaming::lib::*, types},
        telemetry::{TelemetryEvent, TelemetrySender},
        util::pseudo_energy::EwmaSignalData,
    },
    fidl_fuchsia_wlan_internal as fidl_internal, fuchsia_async as fasync, fuchsia_zircon as zx,
    futures::channel::mpsc,
};

/// If there isn't a change in reasons to roam or significant change in RSSI, wait a while between
/// scans. If there isn't a change, it is unlikely that there would be a reason to roam now.
const TIME_BETWEEN_ROAM_SCANS_IF_NO_CHANGE: zx::Duration = zx::Duration::from_minutes(15);
const MIN_TIME_BETWEEN_ROAM_SCANS: zx::Duration = zx::Duration::from_minutes(1);
const MIN_RSSI_CHANGE_TO_ROAM_SCAN: f64 = 5.0;

const LOCAL_ROAM_THRESHOLD_RSSI_2G: f64 = -72.0;
const LOCAL_ROAM_THRESHOLD_RSSI_5G: f64 = -75.0;
const LOCAL_ROAM_THRESHOLD_SNR_2G: f64 = 20.0;
const LOCAL_ROAM_THRESHOLD_SNR_5G: f64 = 17.0;

/// Trait so that RoamMonitor can be mocked in tests.
pub trait RoamMonitorApi: Send + Sync {
    fn handle_connection_stats(
        &mut self,
        stats: fidl_internal::SignalReportIndication,
    ) -> Result<u8, anyhow::Error>;

    fn get_signal_data(&self) -> EwmaSignalData;
}

/// Keeps record of connection data and a valid roam sender, and can trigger roam search requests,
/// which may lead to roaming.
pub struct RoamMonitor {
    /// Channel to send requests for roam searches so LocalRoamManagerService can serve
    /// connection selection scans.
    roam_search_sender: mpsc::UnboundedSender<RoamSearchRequest>,
    /// Channel to send roam requests to a state machine.
    roam_sender: mpsc::UnboundedSender<types::ScannedCandidate>,
    connection_data: ConnectionData,
    telemetry_sender: TelemetrySender,
}

impl RoamMonitor {
    pub fn new(
        roam_search_sender: mpsc::UnboundedSender<RoamSearchRequest>,
        roam_sender: mpsc::UnboundedSender<types::ScannedCandidate>,
        connection_data: ConnectionData,
        telemetry_sender: TelemetrySender,
    ) -> Self {
        Self { roam_search_sender, roam_sender, connection_data, telemetry_sender }
    }
}

impl RoamMonitorApi for RoamMonitor {
    fn handle_connection_stats(
        &mut self,
        stats: fidl_internal::SignalReportIndication,
    ) -> Result<u8, anyhow::Error> {
        self.connection_data.signal_data.update_with_new_measurement(stats.rssi_dbm, stats.snr_db);

        // Send RSSI and RSSI velocity metrics
        self.telemetry_sender.send(TelemetryEvent::OnSignalReport {
            ind: stats,
            rssi_velocity: self.connection_data.signal_data.ewma_rssi_velocity.get(),
        });

        // Evaluate current BSS, and determine if roaming future should be triggered.
        let (roam_reasons, bss_score) = check_signal_thresholds(
            self.connection_data.signal_data,
            self.connection_data.currently_fulfilled_connection.target.bss.channel,
        );
        if !roam_reasons.is_empty() {
            let now = fasync::Time::now();
            if now
                < self.connection_data.roam_decision_data.time_prev_roam_scan
                    + MIN_TIME_BETWEEN_ROAM_SCANS
            {
                return Ok(bss_score);
            }
            // If there isn't a new reason to roam and the previous scan
            // happened recently, do not scan again.
            let is_scan_old = now
                > self.connection_data.roam_decision_data.time_prev_roam_scan
                    + TIME_BETWEEN_ROAM_SCANS_IF_NO_CHANGE;
            let has_new_reason = roam_reasons.iter().any(|r| {
                !self.connection_data.roam_decision_data.roam_reasons_prev_scan.contains(r)
            });
            let rssi = self.connection_data.signal_data.ewma_rssi.get();
            let is_rssi_different =
                (self.connection_data.roam_decision_data.rssi_prev_roam_scan - rssi).abs()
                    > MIN_RSSI_CHANGE_TO_ROAM_SCAN;
            if is_scan_old || has_new_reason || is_rssi_different {
                // Initiate roam scan.
                let req =
                    RoamSearchRequest::new(self.connection_data.clone(), self.roam_sender.clone());
                let _ = self.roam_search_sender.unbounded_send(req);

                // Updated fields for tracking roam scan decisions
                self.connection_data.roam_decision_data.time_prev_roam_scan = fasync::Time::now();
                self.connection_data.roam_decision_data.roam_reasons_prev_scan = roam_reasons;
                self.connection_data.roam_decision_data.rssi_prev_roam_scan = rssi;
            }
        }
        // return score for metrics purposes
        return Ok(bss_score);
    }

    // Return the signal data for the tracked connection.
    fn get_signal_data(&self) -> EwmaSignalData {
        self.connection_data.signal_data
    }
}

// Return roam reasons if the signal measurements fall below given thresholds.
fn check_signal_thresholds(
    signal_data: EwmaSignalData,
    channel: types::WlanChan,
) -> (Vec<RoamReason>, u8) {
    let mut roam_reasons = vec![];
    let (rssi_threshold, snr_threshold) = if channel.is_5ghz() {
        (LOCAL_ROAM_THRESHOLD_RSSI_5G, LOCAL_ROAM_THRESHOLD_SNR_5G)
    } else {
        (LOCAL_ROAM_THRESHOLD_RSSI_2G, LOCAL_ROAM_THRESHOLD_SNR_2G)
    };
    if signal_data.ewma_rssi.get() <= rssi_threshold {
        roam_reasons.push(RoamReason::RssiBelowThreshold)
    }
    if signal_data.ewma_snr.get() <= snr_threshold {
        roam_reasons.push(RoamReason::SnrBelowThreshold)
    }

    let signal_score = scoring_functions::score_current_connection_signal_data(signal_data);
    return (roam_reasons, signal_score);
}

#[cfg(test)]
mod test {
    use {
        super::*,
        crate::{
            client::connection_selection::{EWMA_SMOOTHING_FACTOR, EWMA_VELOCITY_SMOOTHING_FACTOR},
            util::testing::generate_connect_selection,
        },
        fidl_fuchsia_wlan_internal as fidl_internal,
        fuchsia_async::TestExecutor,
        test_util::{assert_gt, assert_lt},
        wlan_common::{assert_variant, channel},
    };

    #[fuchsia::test]
    fn test_check_signal_thresholds_2g() {
        let (roam_reasons, _) = check_signal_thresholds(
            EwmaSignalData::new(
                LOCAL_ROAM_THRESHOLD_RSSI_2G - 1.0,
                LOCAL_ROAM_THRESHOLD_SNR_2G - 1.0,
                EWMA_SMOOTHING_FACTOR,
                EWMA_VELOCITY_SMOOTHING_FACTOR,
            ),
            channel::Channel::new(11, channel::Cbw::Cbw20),
        );
        assert!(roam_reasons.iter().any(|&r| r == RoamReason::SnrBelowThreshold));
        assert!(roam_reasons.iter().any(|&r| r == RoamReason::RssiBelowThreshold));

        let (roam_reasons, _) = check_signal_thresholds(
            EwmaSignalData::new(
                LOCAL_ROAM_THRESHOLD_RSSI_2G + 1.0,
                LOCAL_ROAM_THRESHOLD_SNR_2G + 1.0,
                EWMA_SMOOTHING_FACTOR,
                EWMA_VELOCITY_SMOOTHING_FACTOR,
            ),
            channel::Channel::new(11, channel::Cbw::Cbw20),
        );
        assert!(roam_reasons.is_empty());
    }

    #[fuchsia::test]
    fn test_check_signal_thresholds_5g() {
        let (roam_reasons, _) = check_signal_thresholds(
            EwmaSignalData::new(
                LOCAL_ROAM_THRESHOLD_RSSI_5G - 1.0,
                LOCAL_ROAM_THRESHOLD_SNR_5G - 1.0,
                EWMA_SMOOTHING_FACTOR,
                EWMA_VELOCITY_SMOOTHING_FACTOR,
            ),
            channel::Channel::new(36, channel::Cbw::Cbw80),
        );
        assert!(roam_reasons.iter().any(|&r| r == RoamReason::SnrBelowThreshold));
        assert!(roam_reasons.iter().any(|&r| r == RoamReason::RssiBelowThreshold));

        let (roam_reasons, _) = check_signal_thresholds(
            EwmaSignalData::new(
                LOCAL_ROAM_THRESHOLD_RSSI_5G + 1.0,
                LOCAL_ROAM_THRESHOLD_SNR_5G + 1.0,
                EWMA_SMOOTHING_FACTOR,
                EWMA_VELOCITY_SMOOTHING_FACTOR,
            ),
            channel::Channel::new(36, channel::Cbw::Cbw80),
        );
        assert!(roam_reasons.is_empty());
    }

    struct RoamMonitorTestValues {
        roam_search_sender: mpsc::UnboundedSender<RoamSearchRequest>,
        roam_search_receiver: mpsc::UnboundedReceiver<RoamSearchRequest>,
        roam_req_sender: mpsc::UnboundedSender<types::ScannedCandidate>,
        telemetry_sender: TelemetrySender,
        telemetry_receiver: mpsc::Receiver<TelemetryEvent>,
        currently_fulfilled_connection: types::ConnectSelection,
    }

    fn roam_monitor_test_setup() -> RoamMonitorTestValues {
        let (roam_search_sender, roam_search_receiver) = mpsc::unbounded();
        let (telemetry_sender, telemetry_receiver) = mpsc::channel::<TelemetryEvent>(100);
        let telemetry_sender = TelemetrySender::new(telemetry_sender);
        let (roam_req_sender, _roam_req_receiver) = mpsc::unbounded();
        let currently_fulfilled_connection = generate_connect_selection();

        RoamMonitorTestValues {
            roam_search_sender,
            roam_search_receiver,
            roam_req_sender,
            telemetry_sender,
            telemetry_receiver,
            currently_fulfilled_connection,
        }
    }

    #[fuchsia::test]
    fn test_roam_monitor_should_queue_scan() {
        // Test that if connection quality data comes in indicating that a roam should be
        // considered, the roam monitor will send out a scan request.
        let exec = fasync::TestExecutor::new_with_fake_time();
        exec.set_fake_time(fasync::Time::now());
        let mut test_values = roam_monitor_test_setup();

        let init_rssi = -75;
        let init_snr = 15;
        let roam_data = RoamDecisionData::new(init_rssi as f64, fasync::Time::now());
        let mut signal_data = EwmaSignalData::new(
            init_rssi,
            init_snr,
            EWMA_SMOOTHING_FACTOR,
            EWMA_VELOCITY_SMOOTHING_FACTOR,
        );
        let connection_data = ConnectionData {
            currently_fulfilled_connection: test_values.currently_fulfilled_connection.clone(),
            signal_data,
            roam_decision_data: roam_data,
        };

        let mut roam_monitor = RoamMonitor::new(
            test_values.roam_search_sender.clone(),
            test_values.roam_req_sender.clone(),
            connection_data.clone(),
            test_values.telemetry_sender.clone(),
        );

        // Advance the time so that we allow roam scanning
        exec.set_fake_time(fasync::Time::after(fasync::Duration::from_hours(1)));

        let rssi_dbm = -85;
        let snr_db = 5;
        let signal_report = fidl_internal::SignalReportIndication { rssi_dbm, snr_db };
        // Send some periodic stats to the RoamMonitor for a connection with poor signal.
        let _score = roam_monitor
            .handle_connection_stats(signal_report)
            .expect("Failed to get connection stats");
        signal_data.update_with_new_measurement(rssi_dbm, snr_db);

        // Check that a scan request is sent to the Roam Manager Service.
        let received_roam_req = test_values.roam_search_receiver.try_next();
        assert_variant!(received_roam_req, Ok(Some(req)) => {
            assert_eq!(req.connection_data.currently_fulfilled_connection, test_values.currently_fulfilled_connection);
            assert_eq!(req.connection_data.signal_data, signal_data);
        });

        // Verify that a telemerty event is sent for the RSSI and RSSI velocity
        assert_variant!(test_values.telemetry_receiver.try_next(), Ok(Some(TelemetryEvent::OnSignalReport {ind, rssi_velocity})) => {
            assert_eq!(ind, signal_report);
            // verify that RSSI velocity is negative since the signal report RSSI is lower.
            assert_lt!(rssi_velocity, 0.0);
        });
    }

    #[fuchsia::test]
    fn test_roam_monitor_tracks_signal_velocity_and_sends_telemetry_events() {
        // RoamMonitor should keep track of the RSSI velocity which will be used to evaluate
        // connection quality and be sent to telemetry.
        let mut _exec = TestExecutor::new();
        let mut test_values = roam_monitor_test_setup();

        let init_rssi = -70;
        let init_snr = 20;
        let roam_data = RoamDecisionData::new(init_rssi as f64, fasync::Time::now());
        let signal_data = EwmaSignalData::new(
            init_rssi,
            init_snr,
            EWMA_SMOOTHING_FACTOR,
            EWMA_VELOCITY_SMOOTHING_FACTOR,
        );

        let connection_data = ConnectionData {
            currently_fulfilled_connection: test_values.currently_fulfilled_connection.clone(),
            signal_data,
            roam_decision_data: roam_data,
        };
        let mut roam_monitor = RoamMonitor::new(
            test_values.roam_search_sender.clone(),
            test_values.roam_req_sender.clone(),
            connection_data.clone(),
            test_values.telemetry_sender.clone(),
        );

        // Send some stats with RSSI and SNR getting worse to the RoamMonitor.
        let rssi_dbm_1 = -80;
        let snr_db_1 = 10;
        let signal_report_1 =
            fidl_internal::SignalReportIndication { rssi_dbm: rssi_dbm_1, snr_db: snr_db_1 };
        let _score = roam_monitor
            .handle_connection_stats(signal_report_1)
            .expect("Failed to get connection stats");

        // Verify that a telemerty event is sent for the RSSI and RSSI velocity
        assert_variant!(test_values.telemetry_receiver.try_next(), Ok(Some(TelemetryEvent::OnSignalReport {ind, rssi_velocity})) => {
            assert_eq!(ind, signal_report_1);
            // verify that RSSI velocity is negative since the signal report RSSI is lower.
            assert_lt!(rssi_velocity, 0.0);
        });

        // Send some stats with RSSI and SNR getting getting better and check that RSSI velocity
        // is positive.
        let rssi_dbm_2 = -60;
        let snr_db_2 = 30;
        let signal_report_2 =
            fidl_internal::SignalReportIndication { rssi_dbm: rssi_dbm_2, snr_db: snr_db_2 };
        let _score = roam_monitor
            .handle_connection_stats(signal_report_2)
            .expect("Failed to get connection stats");

        assert_variant!(test_values.telemetry_receiver.try_next(), Ok(Some(TelemetryEvent::OnSignalReport {ind, rssi_velocity})) => {
            assert_eq!(ind, signal_report_2);
            // verify that RSSI velocity is negative since the signal report RSSI is lower.
            assert_gt!(rssi_velocity, 0.0);
        });
    }

    #[fuchsia::test]
    fn test_roam_monitor_should_not_roam_scan_frequently() {
        let exec = fasync::TestExecutor::new_with_fake_time();
        exec.set_fake_time(fasync::Time::now());
        // Test that if the connection continues to be bad, the RoamMonitor does not scan too
        // often.
        let mut test_values = roam_monitor_test_setup();

        let init_rssi = -80;
        let init_snr = 10;
        let roam_data = RoamDecisionData::new(init_rssi as f64, fasync::Time::now());
        let signal_data = EwmaSignalData::new(
            init_rssi,
            init_snr,
            EWMA_SMOOTHING_FACTOR,
            EWMA_VELOCITY_SMOOTHING_FACTOR,
        );
        let connection_data = ConnectionData {
            currently_fulfilled_connection: test_values.currently_fulfilled_connection.clone(),
            signal_data,
            roam_decision_data: roam_data,
        };
        let mut roam_monitor = RoamMonitor::new(
            test_values.roam_search_sender.clone(),
            test_values.roam_req_sender.clone(),
            connection_data.clone(),
            test_values.telemetry_sender.clone(),
        );

        exec.set_fake_time(fasync::Time::after(fasync::Duration::from_hours(1)));
        let signal_report =
            fidl_internal::SignalReportIndication { rssi_dbm: init_rssi, snr_db: init_snr };
        // Send some periodic stats to the RoamMonitor for a connection with poor signal.
        let _score = roam_monitor
            .handle_connection_stats(signal_report)
            .expect("Failed to get connection stats");

        // Check that a scan request is sent to the Roam Manager Service.
        let received_roam_req = test_values.roam_search_receiver.try_next();
        assert_variant!(received_roam_req, Ok(Some(req)) => {
            assert_eq!(req.connection_data.currently_fulfilled_connection, test_values.currently_fulfilled_connection);
            assert_eq!(req.connection_data.signal_data, signal_data.clone());
        });

        // Send stats with a worse RSSI and check that a roam scan is not initiated
        let init_rssi = -85;
        let init_snr = 5;
        let signal_report =
            fidl_internal::SignalReportIndication { rssi_dbm: init_rssi, snr_db: init_snr };

        let _score = roam_monitor
            .handle_connection_stats(signal_report)
            .expect("Failed to get connection stats");
    }
}
