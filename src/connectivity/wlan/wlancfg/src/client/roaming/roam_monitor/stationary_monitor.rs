// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::client::roaming::lib::*;
use crate::client::roaming::roam_monitor::RoamMonitorApi;
use crate::client::types;
use crate::telemetry::{TelemetryEvent, TelemetrySender};
use crate::util::pseudo_energy::EwmaSignalData;
use tracing::info;
use {fidl_fuchsia_wlan_internal as fidl_internal, fuchsia_async as fasync, fuchsia_zircon as zx};

/// If there isn't a change in reasons to roam or significant change in RSSI, wait a while between
/// scans, as it is unlikely that there would be a reason to roam.
const TIME_BETWEEN_ROAM_SCANS_IF_NO_CHANGE: zx::Duration = zx::Duration::from_minutes(15);
const MIN_TIME_BETWEEN_ROAM_SCANS: zx::Duration = zx::Duration::from_minutes(1);
const MIN_RSSI_CHANGE_TO_ROAM_SCAN: f64 = 5.0;

const LOCAL_ROAM_THRESHOLD_RSSI_2G: f64 = -72.0;
const LOCAL_ROAM_THRESHOLD_RSSI_5G: f64 = -75.0;
const LOCAL_ROAM_THRESHOLD_SNR_2G: f64 = 20.0;
const LOCAL_ROAM_THRESHOLD_SNR_5G: f64 = 17.0;

const MIN_RSSI_IMPROVEMENT_TO_ROAM: f64 = 3.0;
const MIN_SNR_IMPROVEMENT_TO_ROAM: f64 = 3.0;

/// Number of previous RSSI measurements to exponentially weigh into average.
/// TODO(https://fxbug.dev/42165706): Tune smoothing factor.
pub const STATIONARY_ROAMING_EWMA_SMOOTHING_FACTOR: usize = 10;

pub struct StationaryMonitor {
    pub connection_data: RoamingConnectionData,
    pub telemetry_sender: TelemetrySender,
}

impl StationaryMonitor {
    pub fn new(
        currently_fulfilled_connection: types::ConnectSelection,
        signal: types::Signal,
        telemetry_sender: TelemetrySender,
    ) -> Self {
        // Calculate a connection quality score
        let connection_data = RoamingConnectionData::new(
            currently_fulfilled_connection,
            EwmaSignalData::new(
                signal.rssi_dbm,
                signal.snr_db,
                STATIONARY_ROAMING_EWMA_SMOOTHING_FACTOR,
            ),
        );
        Self { connection_data, telemetry_sender }
    }

    // Handle signal report indiciations. Update internal connection data, if necessary. Returns
    // true if a roam search should be initiated.
    fn handle_signal_report(
        &mut self,
        stats: fidl_internal::SignalReportIndication,
    ) -> Result<bool, anyhow::Error> {
        self.connection_data.signal_data.update_with_new_measurement(stats.rssi_dbm, stats.snr_db);

        // Update velocity with EWMA signal, to smooth out noise.
        self.connection_data.rssi_velocity.update(self.connection_data.signal_data.ewma_rssi.get());

        self.telemetry_sender.send(TelemetryEvent::OnSignalVelocityUpdate {
            rssi_velocity: self.connection_data.rssi_velocity.get(),
        });

        // Determine any roam reasons based on the signal thresholds.
        let mut roam_reasons: Vec<RoamReason> = vec![];
        roam_reasons.append(&mut check_signal_thresholds(
            &self.connection_data.signal_data,
            self.connection_data.currently_fulfilled_connection.target.bss.channel,
        ));

        let now = fasync::Time::now();
        if roam_reasons.is_empty()
            || now
                < self.connection_data.previous_roam_scan_data.time_prev_roam_scan
                    + MIN_TIME_BETWEEN_ROAM_SCANS
        {
            return Ok(false);
        }

        let is_scan_old = now
            > self.connection_data.previous_roam_scan_data.time_prev_roam_scan
                + TIME_BETWEEN_ROAM_SCANS_IF_NO_CHANGE;
        let has_new_reason = roam_reasons.iter().any(|r| {
            !self.connection_data.previous_roam_scan_data.roam_reasons_prev_scan.contains(r)
        });
        let rssi = self.connection_data.signal_data.ewma_rssi.get();
        let is_rssi_different =
            (self.connection_data.previous_roam_scan_data.rssi_prev_roam_scan - rssi).abs()
                > MIN_RSSI_CHANGE_TO_ROAM_SCAN;
        // Only initiate roam search if there are new roam reasons, a changed RSSI, or a significant
        // amount of time has passed.
        if is_scan_old || has_new_reason || is_rssi_different {
            // Updated fields for tracking roam scan decisions and initiated roam search.
            self.connection_data.previous_roam_scan_data.time_prev_roam_scan = fasync::Time::now();
            self.connection_data.previous_roam_scan_data.roam_reasons_prev_scan = roam_reasons;
            self.connection_data.previous_roam_scan_data.rssi_prev_roam_scan = rssi;
            return Ok(true);
        }
        Ok(false)
    }
}

impl RoamMonitorApi for StationaryMonitor {
    fn should_roam_search(&mut self, data: RoamTriggerData) -> Result<bool, anyhow::Error> {
        match data {
            RoamTriggerData::SignalReportInd(stats) => self.handle_signal_report(stats),
        }
    }
    fn should_send_roam_request(
        &self,
        candidate: types::ScannedCandidate,
    ) -> Result<bool, anyhow::Error> {
        if candidate.is_same_bss_security_and_credential(
            &self.connection_data.currently_fulfilled_connection.target,
        ) {
            info!("Selected roam candidate is the currently connected candidate, ignoring");
            return Ok(false);
        }

        // Only send roam scan if the selected candidate shows a significant signal improvement,
        // compared to the most up-to-date roaming connection data
        let latest_rssi = self.connection_data.signal_data.ewma_rssi.get();
        let latest_snr = self.connection_data.signal_data.ewma_snr.get();
        if (candidate.bss.signal.rssi_dbm as f64) < latest_rssi + MIN_RSSI_IMPROVEMENT_TO_ROAM
            && (candidate.bss.signal.snr_db as f64) < latest_snr + MIN_SNR_IMPROVEMENT_TO_ROAM
        {
            info!(
                "Selected roam candidate ({:?}) is not enough of an improvement. Ignoring.",
                candidate.to_string_without_pii()
            );
            return Ok(false);
        }
        Ok(true)
    }

    fn get_roam_data(&self) -> Result<RoamingConnectionData, anyhow::Error> {
        Ok(self.connection_data.clone())
    }
}

// Return roam reasons if the signal measurements fall below given thresholds.
fn check_signal_thresholds(
    signal_data: &EwmaSignalData,
    channel: types::WlanChan,
) -> Vec<RoamReason> {
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
    roam_reasons
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::util::testing::{
        generate_random_bss, generate_random_roaming_connection_data,
        generate_random_scanned_candidate,
    };
    use fidl_fuchsia_wlan_internal as fidl_internal;
    use futures::channel::mpsc;
    use test_case::test_case;
    use wlan_common::{assert_variant, channel};

    struct TestValues {
        monitor: StationaryMonitor,
        telemetry_receiver: mpsc::Receiver<TelemetryEvent>,
    }

    fn setup_test() -> TestValues {
        let connection_data = generate_random_roaming_connection_data();
        let (telemetry_sender, telemetry_receiver) = mpsc::channel::<TelemetryEvent>(100);
        let telemetry_sender = TelemetrySender::new(telemetry_sender);
        let monitor = StationaryMonitor { connection_data, telemetry_sender };
        TestValues { monitor, telemetry_receiver }
    }

    fn setup_test_with_data(connection_data: RoamingConnectionData) -> TestValues {
        let (telemetry_sender, telemetry_receiver) = mpsc::channel::<TelemetryEvent>(100);
        let telemetry_sender = TelemetrySender::new(telemetry_sender);
        let monitor = StationaryMonitor { connection_data, telemetry_sender };
        TestValues { monitor, telemetry_receiver }
    }

    #[fuchsia::test]
    fn test_check_signal_thresholds_2g() {
        let roam_reasons = check_signal_thresholds(
            &EwmaSignalData::new(
                LOCAL_ROAM_THRESHOLD_RSSI_2G - 1.0,
                LOCAL_ROAM_THRESHOLD_SNR_2G - 1.0,
                STATIONARY_ROAMING_EWMA_SMOOTHING_FACTOR,
            ),
            channel::Channel::new(11, channel::Cbw::Cbw20),
        );
        assert!(roam_reasons.iter().any(|&r| r == RoamReason::SnrBelowThreshold));
        assert!(roam_reasons.iter().any(|&r| r == RoamReason::RssiBelowThreshold));

        let roam_reasons = check_signal_thresholds(
            &EwmaSignalData::new(
                LOCAL_ROAM_THRESHOLD_RSSI_2G + 1.0,
                LOCAL_ROAM_THRESHOLD_SNR_2G + 1.0,
                STATIONARY_ROAMING_EWMA_SMOOTHING_FACTOR,
            ),
            channel::Channel::new(11, channel::Cbw::Cbw20),
        );
        assert!(roam_reasons.is_empty());
    }

    #[fuchsia::test]
    fn test_check_signal_thresholds_5g() {
        let roam_reasons = check_signal_thresholds(
            &EwmaSignalData::new(
                LOCAL_ROAM_THRESHOLD_RSSI_5G - 1.0,
                LOCAL_ROAM_THRESHOLD_SNR_5G - 1.0,
                STATIONARY_ROAMING_EWMA_SMOOTHING_FACTOR,
            ),
            channel::Channel::new(36, channel::Cbw::Cbw80),
        );
        assert!(roam_reasons.iter().any(|&r| r == RoamReason::SnrBelowThreshold));
        assert!(roam_reasons.iter().any(|&r| r == RoamReason::RssiBelowThreshold));

        let roam_reasons = check_signal_thresholds(
            &EwmaSignalData::new(
                LOCAL_ROAM_THRESHOLD_RSSI_5G + 1.0,
                LOCAL_ROAM_THRESHOLD_SNR_5G + 1.0,
                STATIONARY_ROAMING_EWMA_SMOOTHING_FACTOR,
            ),
            channel::Channel::new(36, channel::Cbw::Cbw80),
        );
        assert!(roam_reasons.is_empty());
    }

    enum HandleSignalReportTriggerDataTestCase {
        BadRssi,
        BadSnr,
        BadRssiAndSnr,
        GoodRssiAndSnr,
    }
    #[test_case(HandleSignalReportTriggerDataTestCase::BadRssi; "bad rssi")]
    #[test_case(HandleSignalReportTriggerDataTestCase::BadSnr; "bad snr")]
    #[test_case(HandleSignalReportTriggerDataTestCase::BadRssiAndSnr; "bad rssi and snr")]
    #[test_case(HandleSignalReportTriggerDataTestCase::GoodRssiAndSnr; "good rssi and snr")]
    #[fuchsia::test(add_test_attr = false)]
    fn test_handle_signal_report_trigger_data(test_case: HandleSignalReportTriggerDataTestCase) {
        let exec = fasync::TestExecutor::new_with_fake_time();
        exec.set_fake_time(fasync::Time::now());

        // Generate initial connection data based on test case.
        let (rssi, snr) = match test_case {
            HandleSignalReportTriggerDataTestCase::BadRssi => (-80, 50),
            HandleSignalReportTriggerDataTestCase::BadSnr => (-40, 10),
            HandleSignalReportTriggerDataTestCase::BadRssiAndSnr => (-80, 10),
            HandleSignalReportTriggerDataTestCase::GoodRssiAndSnr => (-40, 50),
        };
        let connection_data = RoamingConnectionData {
            signal_data: EwmaSignalData::new(rssi, snr, 10),
            ..generate_random_roaming_connection_data()
        };
        let mut test_values = setup_test_with_data(connection_data);

        // Advance the time so that we allow roam scanning,
        exec.set_fake_time(fasync::Time::after(fasync::Duration::from_hours(1)));

        // Generate trigger data that won't change the above values, and send to should_roam_search
        // method.
        let trigger_data =
            RoamTriggerData::SignalReportInd(fidl_internal::SignalReportIndication {
                rssi_dbm: rssi,
                snr_db: snr,
            });
        let should_roam_search = test_values
            .monitor
            .should_roam_search(trigger_data)
            .expect("error handling roam trigger data");

        match test_case {
            HandleSignalReportTriggerDataTestCase::GoodRssiAndSnr => assert!(!should_roam_search),
            _ => assert!(should_roam_search),
        }
    }

    #[fuchsia::test]
    fn test_minimum_time_between_roam_scans() {
        let exec = fasync::TestExecutor::new_with_fake_time();
        exec.set_fake_time(fasync::Time::now());

        // Setup monitor with connection data that would trigger a roam scan due to SNR below
        // threshold. Set the EWMA weights to 1 so the values can be easily changed later in tests.
        let rssi = LOCAL_ROAM_THRESHOLD_RSSI_2G + 1.0;
        let snr = LOCAL_ROAM_THRESHOLD_SNR_5G - 1.0;
        let connection_data = RoamingConnectionData {
            signal_data: EwmaSignalData::new(rssi, snr, 1),
            ..generate_random_roaming_connection_data()
        };
        let mut test_values = setup_test_with_data(connection_data);

        // Generate trigger data with same signal values as initial, which would trigger a roam
        // search due to the below threshold SNR.
        let trigger_data =
            RoamTriggerData::SignalReportInd(fidl_internal::SignalReportIndication {
                rssi_dbm: rssi as i8,
                snr_db: snr as i8,
            });

        // Advance the time so that we allow roam scanning
        exec.set_fake_time(fasync::Time::after(fasync::Duration::from_hours(1)));

        // Send trigger data, and verify that we are told to roam search.
        assert!(test_values
            .monitor
            .should_roam_search(trigger_data.clone())
            .expect("error handling roam trigger data"));

        // Advance the time less than the minimum between roam scans.
        exec.set_fake_time(fasync::Time::after(
            MIN_TIME_BETWEEN_ROAM_SCANS - fasync::Duration::from_seconds(1),
        ));

        // Generate trigger data that would add a new roam reason for the RSSI. This ensures we
        // would roam search due to a new roam reason, if it weren't for the min time.
        let trigger_data =
            RoamTriggerData::SignalReportInd(fidl_internal::SignalReportIndication {
                rssi_dbm: (LOCAL_ROAM_THRESHOLD_RSSI_5G - 1.0) as i8,
                snr_db: snr as i8,
            });

        // Send trigger data, and verify that we aren't told to roam search because it is too soon.
        assert!(!test_values
            .monitor
            .should_roam_search(trigger_data.clone())
            .expect("error handling roam trigger data"));

        // Advance the time past the minimum time.
        exec.set_fake_time(fasync::Time::after(fasync::Duration::from_seconds(2)));

        // Verify that we now are told to roam scan search.
        assert!(test_values
            .monitor
            .should_roam_search(trigger_data)
            .expect("error handling roam trigger data"));
    }

    #[fuchsia::test]
    fn test_check_scan_age_rssi_change_and_new_reasons() {
        let exec = fasync::TestExecutor::new_with_fake_time();
        exec.set_fake_time(fasync::Time::now());

        // Setup monitor with connection data that would trigger a roam scan due to SNR and RSSI
        // below thresholds.
        let rssi = LOCAL_ROAM_THRESHOLD_RSSI_5G - 1.0;
        let snr = LOCAL_ROAM_THRESHOLD_SNR_5G - 1.0;
        let connection_data = RoamingConnectionData {
            signal_data: EwmaSignalData::new(rssi, snr, 10),
            ..generate_random_roaming_connection_data()
        };
        let mut test_values = setup_test_with_data(connection_data);

        // Generate trigger data with same signal values as initial, which would trigger a roam scan
        // due to low SNR/RSSI.
        let trigger_data =
            RoamTriggerData::SignalReportInd(fidl_internal::SignalReportIndication {
                rssi_dbm: rssi as i8,
                snr_db: snr as i8,
            });

        // Advance the time so that we allow roam scanning.
        let initial_time = fasync::Time::after(fasync::Duration::from_hours(1));
        exec.set_fake_time(initial_time);

        // Send trigger data, and verify that we would be told to roam scan.
        assert!(test_values
            .monitor
            .should_roam_search(trigger_data.clone())
            .expect("error handling roam trigger data"));

        // Advance the time so its past the minimum between roam scans, but not the time between
        // scans if there are no other changes.
        exec.set_fake_time(
            initial_time + MIN_TIME_BETWEEN_ROAM_SCANS + fasync::Duration::from_seconds(1),
        );

        // Send identical trigger data, and verify that we don't scan, because the RSSI is unchanged,
        // the roam reasons are unchanged, and the last scan is too recent.
        assert!(!test_values
            .monitor
            .should_roam_search(trigger_data.clone())
            .expect("error handling roam trigger data"));
    }

    #[fuchsia::test]
    fn test_is_scan_old() {
        let exec = fasync::TestExecutor::new_with_fake_time();
        exec.set_fake_time(fasync::Time::now());

        // Setup monitor with connection data that would trigger a roam scan due to SNR and SNR below
        // threshold.
        let rssi = LOCAL_ROAM_THRESHOLD_RSSI_5G - 1.0;
        let snr = LOCAL_ROAM_THRESHOLD_SNR_5G - 1.0;
        let connection_data = RoamingConnectionData {
            signal_data: EwmaSignalData::new(rssi, snr, 10),
            ..generate_random_roaming_connection_data()
        };
        let mut test_values = setup_test_with_data(connection_data);

        // Generate trigger data with same signal values as initial, which would trigger a roam scan
        // due to low SNR/RSSI.
        let trigger_data =
            RoamTriggerData::SignalReportInd(fidl_internal::SignalReportIndication {
                rssi_dbm: rssi as i8,
                snr_db: snr as i8,
            });

        // Advance the time so that we allow roam scanning.
        let initial_time = fasync::Time::after(fasync::Duration::from_hours(1));
        exec.set_fake_time(initial_time);

        // Send trigger data, and verify that we would be told to roam scan.
        assert!(test_values
            .monitor
            .should_roam_search(trigger_data.clone())
            .expect("error handling roam trigger data"));

        // Advance the time so its past the time between roam scans, even if no change.
        exec.set_fake_time(
            initial_time + TIME_BETWEEN_ROAM_SCANS_IF_NO_CHANGE + fasync::Duration::from_seconds(1),
        );

        // Send trigger data, and verify that we will now scan, despite no RSSI or roam reason
        // change, because the last scan is considered old.
        assert!(test_values
            .monitor
            .should_roam_search(trigger_data.clone())
            .expect("error handling roam trigger data"));
    }

    #[fuchsia::test]
    fn test_rssi_has_changed() {
        let exec = fasync::TestExecutor::new_with_fake_time();
        exec.set_fake_time(fasync::Time::now());

        // Setup monitor with connection data that would trigger a roam scan due to low RSSI. Set
        // ewma weight to 1, so its easy to change.
        let rssi = LOCAL_ROAM_THRESHOLD_RSSI_5G - 1.0;
        let snr = LOCAL_ROAM_THRESHOLD_SNR_2G + 1.0;
        let connection_data = RoamingConnectionData {
            signal_data: EwmaSignalData::new(rssi, snr, 1),
            ..generate_random_roaming_connection_data()
        };
        let mut test_values = setup_test_with_data(connection_data);

        // Generate trigger data with same signal values as initial, which would trigger a roam scan
        // due to low RSSI.
        let trigger_data =
            RoamTriggerData::SignalReportInd(fidl_internal::SignalReportIndication {
                rssi_dbm: rssi as i8,
                snr_db: snr as i8,
            });

        // Advance the time so that we allow roam scanning
        let initial_time = fasync::Time::after(fasync::Duration::from_hours(1));
        exec.set_fake_time(initial_time);

        // Send trigger data, and verify that we would be told to roam scan.
        assert!(test_values
            .monitor
            .should_roam_search(trigger_data.clone())
            .expect("error handling roam trigger data"));

        // Advance the time so its past the minimum between roam scans.
        exec.set_fake_time(
            initial_time + MIN_TIME_BETWEEN_ROAM_SCANS + fasync::Duration::from_seconds(1),
        );

        // Change the RSSI beyond the minimum range. The roam reasons will not change.
        let trigger_data =
            RoamTriggerData::SignalReportInd(fidl_internal::SignalReportIndication {
                rssi_dbm: (rssi - MIN_RSSI_CHANGE_TO_ROAM_SCAN - 1.0) as i8,
                snr_db: snr as i8,
            });

        // Send trigger data, and verify we will now scan, despite a recent scan and no new roam
        // reasons, because the RSSI is different.
        assert!(test_values
            .monitor
            .should_roam_search(trigger_data.clone())
            .expect("error handling roam trigger data"));
    }

    #[fuchsia::test]
    fn test_roam_reasons_have_changed() {
        let exec = fasync::TestExecutor::new_with_fake_time();
        exec.set_fake_time(fasync::Time::now());

        // Setup monitor with connection data that would trigger a roam scan due to RSSI. Set an
        // ewma weight of 1, so its easy to change.
        let rssi = LOCAL_ROAM_THRESHOLD_RSSI_5G - 1.0;
        let snr = LOCAL_ROAM_THRESHOLD_SNR_2G + 1.0;
        let connection_data = RoamingConnectionData {
            signal_data: EwmaSignalData::new(rssi, snr, 1),
            ..generate_random_roaming_connection_data()
        };
        let mut test_values = setup_test_with_data(connection_data);

        // Generate trigger data with same signal values as initial, which would trigger a roam scan.
        let trigger_data =
            RoamTriggerData::SignalReportInd(fidl_internal::SignalReportIndication {
                rssi_dbm: rssi as i8,
                snr_db: snr as i8,
            });

        // Advance the time so that we allow roam scanning.
        let initial_time = fasync::Time::after(fasync::Duration::from_hours(1));
        exec.set_fake_time(initial_time);

        // Send trigger data, and verify that we would be told to roam scan.
        assert!(test_values
            .monitor
            .should_roam_search(trigger_data.clone())
            .expect("error handling roam trigger data"));

        // Advance the time so its past the minimum between roam scans.
        exec.set_fake_time(
            initial_time + MIN_TIME_BETWEEN_ROAM_SCANS + fasync::Duration::from_seconds(1),
        );

        // Change the SNR so that we will now get an SNR threshold roam reason.
        let trigger_data =
            RoamTriggerData::SignalReportInd(fidl_internal::SignalReportIndication {
                rssi_dbm: rssi as i8,
                snr_db: (snr - 10.0) as i8,
            });

        // Send trigger data, and verify we will now scan, despite a recent scan and no RSSI change,
        // because the there is a new roam reason.
        assert!(test_values
            .monitor
            .should_roam_search(trigger_data.clone())
            .expect("error handling roam trigger data"));
    }

    #[fuchsia::test]
    fn test_should_send_roam_request() {
        let _exec = fasync::TestExecutor::new();
        let test_values = setup_test();

        // Get the randomized RSSI and SNR values.
        let current_rssi = test_values.monitor.connection_data.signal_data.ewma_rssi.get();
        let current_snr = test_values.monitor.connection_data.signal_data.ewma_snr.get();

        // Verify that roam recommendations are blocked if RSSI and SNR are insufficient
        // improvements.
        let candidate = types::ScannedCandidate {
            bss: types::Bss {
                signal: types::Signal {
                    rssi_dbm: (current_rssi + MIN_RSSI_IMPROVEMENT_TO_ROAM - 1.0) as i8,
                    snr_db: (current_snr + MIN_SNR_IMPROVEMENT_TO_ROAM - 1.0) as i8,
                },
                ..generate_random_bss()
            },
            ..generate_random_scanned_candidate()
        };
        assert!(!test_values
            .monitor
            .should_send_roam_request(candidate)
            .expect("failed to check roam request"));

        // Verify that a roam recommendation is made if RSSI improvement exceeds threshold.
        let candidate = types::ScannedCandidate {
            bss: types::Bss {
                signal: types::Signal {
                    rssi_dbm: (current_rssi + MIN_RSSI_IMPROVEMENT_TO_ROAM) as i8,
                    snr_db: (current_snr + MIN_SNR_IMPROVEMENT_TO_ROAM - 1.0) as i8,
                },
                ..generate_random_bss()
            },
            ..generate_random_scanned_candidate()
        };
        assert!(test_values
            .monitor
            .should_send_roam_request(candidate)
            .expect("failed to check roam request"));

        // Verify that a roam recommendation is made if SNR improvement exceeds threshold.
        let candidate = types::ScannedCandidate {
            bss: types::Bss {
                signal: types::Signal {
                    rssi_dbm: (current_rssi + MIN_RSSI_IMPROVEMENT_TO_ROAM - 1.0) as i8,
                    snr_db: (current_snr + MIN_SNR_IMPROVEMENT_TO_ROAM) as i8,
                },
                ..generate_random_bss()
            },
            ..generate_random_scanned_candidate()
        };
        assert!(test_values
            .monitor
            .should_send_roam_request(candidate)
            .expect("failed to check roam request"));

        // Verify that roam recommendations are blocked if the selected candidate is the currently
        // connected BSS. Set signal values high enough to isolate the dedupe function.
        let candidate = types::ScannedCandidate {
            bss: types::Bss {
                signal: types::Signal {
                    rssi_dbm: (current_rssi + MIN_RSSI_IMPROVEMENT_TO_ROAM + 1.0) as i8,
                    snr_db: (current_snr + MIN_SNR_IMPROVEMENT_TO_ROAM + 1.0) as i8,
                },
                ..test_values
                    .monitor
                    .connection_data
                    .currently_fulfilled_connection
                    .target
                    .bss
                    .clone()
            },
            ..test_values.monitor.connection_data.currently_fulfilled_connection.target.clone()
        };
        assert!(!test_values
            .monitor
            .should_send_roam_request(candidate)
            .expect("failed to check roam reqeust"));
    }

    #[fuchsia::test]
    fn test_send_signal_velocity_metric_event() {
        let exec = fasync::TestExecutor::new_with_fake_time();
        exec.set_fake_time(fasync::Time::now());

        let connection_data = RoamingConnectionData {
            signal_data: EwmaSignalData::new(-40, 50, 1),
            ..generate_random_roaming_connection_data()
        };
        let mut test_values = setup_test_with_data(connection_data);

        let trigger_data =
            RoamTriggerData::SignalReportInd(fidl_internal::SignalReportIndication {
                rssi_dbm: -80,
                snr_db: 10,
            });
        let _ = test_values
            .monitor
            .should_roam_search(trigger_data)
            .expect("error handling roam trigger data");

        assert_variant!(
            test_values.telemetry_receiver.try_next(),
            Ok(Some(TelemetryEvent::OnSignalVelocityUpdate { .. }))
        );
    }
}
