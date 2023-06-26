// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    client::{state_machine::PeriodicConnectionStats, types},
    config_management::FailureReason::CredentialRejected,
    util::pseudo_energy::*,
};

const RSSI_AND_VELOCITY_SCORE_WEIGHT: f32 = 0.6;
const SNR_SCORE_WEIGHT: f32 = 0.4;

/// Above or at this RSSI, we'll give 5G networks a preference
const RSSI_CUTOFF_5G_PREFERENCE: i16 = -64;
/// The score boost for 5G networks that we are giving preference to.
const RSSI_5G_PREFERENCE_BOOST: i16 = 20;
/// The amount to decrease the score by for each failed connection attempt.
const SCORE_PENALTY_FOR_RECENT_FAILURE: i16 = 5;
/// This penalty is much higher than for a general failure because we are not likely to succeed
/// on a retry.
const SCORE_PENALTY_FOR_RECENT_CREDENTIAL_REJECTED: i16 = 30;
/// The amount to decrease the score for each time we are connected for only a short amount
/// of time before disconncting. This amount is the same as the penalty for 4 failed connect
/// attempts to a BSS.
const SCORE_PENALTY_FOR_SHORT_CONNECTION: i16 = 20;

pub fn score_bss_scanned_candidate(bss_candidate: types::ScannedCandidate) -> i16 {
    let mut score = bss_candidate.bss.rssi as i16;
    let channel = bss_candidate.bss.channel;

    // If the network is 5G and has a strong enough RSSI, give it a bonus
    if channel.is_5ghz() && score >= RSSI_CUTOFF_5G_PREFERENCE {
        score = score.saturating_add(RSSI_5G_PREFERENCE_BOOST);
    }

    // Penalize APs with recent failures to connect
    let matching_failures = bss_candidate
        .saved_network_info
        .recent_failures
        .iter()
        .filter(|failure| failure.bssid == bss_candidate.bss.bssid);
    for failure in matching_failures {
        // Count failures for rejected credentials higher since we probably won't succeed
        // another try with the same credentials.
        if failure.reason == CredentialRejected {
            score = score.saturating_sub(SCORE_PENALTY_FOR_RECENT_CREDENTIAL_REJECTED);
        } else {
            score = score.saturating_sub(SCORE_PENALTY_FOR_RECENT_FAILURE);
        }
    }
    // Penalize APs with recent short connections before disconnecting.
    let short_connection_score: i16 = bss_candidate
        .recent_short_connections()
        .try_into()
        .unwrap_or_else(|_| i16::MAX)
        .saturating_mul(SCORE_PENALTY_FOR_SHORT_CONNECTION);

    return score.saturating_sub(short_connection_score);
}

pub fn score_current_connection_signal_data(data: SignalData) -> u8 {
    let ewma_rssi_velocity = data.ewma_rssi_velocity.get();
    let rssi_velocity_score = match data.ewma_rssi.get() {
        r if r <= -81.0 => match ewma_rssi_velocity {
            v if v < -2.7 => 0,
            v if v < -1.8 => 0,
            v if v < -0.9 => 0,
            v if v <= 0.9 => 0,
            v if v <= 1.8 => 20,
            v if v <= 2.7 => 18,
            _ => 10,
        },
        r if r <= -76.0 => match ewma_rssi_velocity {
            v if v < -2.7 => 0,
            v if v < -1.8 => 0,
            v if v < -0.9 => 0,
            v if v <= 0.9 => 15,
            v if v <= 1.8 => 28,
            v if v <= 2.7 => 25,
            _ => 15,
        },
        r if r <= -71.0 => match ewma_rssi_velocity {
            v if v < -2.7 => 0,
            v if v < -1.8 => 5,
            v if v < -0.9 => 15,
            v if v <= 0.9 => 30,
            v if v <= 1.8 => 45,
            v if v <= 2.7 => 38,
            _ => 4,
        },
        r if r <= -66.0 => match ewma_rssi_velocity {
            v if v < -2.7 => 10,
            v if v < -1.8 => 18,
            v if v < -0.9 => 30,
            v if v <= 0.9 => 48,
            v if v <= 1.8 => 60,
            v if v <= 2.7 => 50,
            _ => 38,
        },
        r if r <= -61.0 => match ewma_rssi_velocity {
            v if v < -2.7 => 20,
            v if v < -1.8 => 30,
            v if v < -0.9 => 45,
            v if v <= 0.9 => 70,
            v if v <= 1.8 => 75,
            v if v <= 2.7 => 60,
            _ => 55,
        },
        r if r <= -56.0 => match ewma_rssi_velocity {
            v if v < -2.7 => 40,
            v if v < -1.8 => 50,
            v if v < -0.9 => 63,
            v if v <= 0.9 => 85,
            v if v <= 1.8 => 85,
            v if v <= 2.7 => 70,
            _ => 65,
        },
        r if r <= -51.0 => match ewma_rssi_velocity {
            v if v < -2.7 => 55,
            v if v < -1.8 => 65,
            v if v < -0.9 => 75,
            v if v <= 0.9 => 95,
            v if v <= 1.8 => 90,
            v if v <= 2.7 => 80,
            _ => 75,
        },
        _ => match ewma_rssi_velocity {
            v if v < -2.7 => 60,
            v if v < -1.8 => 70,
            v if v < -0.9 => 80,
            v if v <= 0.9 => 100,
            v if v <= 1.8 => 95,
            v if v <= 2.7 => 90,
            _ => 80,
        },
    };

    let snr_score = match data.ewma_snr.get() {
        s if s <= 10.0 => 0,
        s if s <= 15.0 => 15,
        s if s <= 20.0 => 37,
        s if s <= 25.0 => 53,
        s if s <= 30.0 => 68,
        s if s <= 35.0 => 80,
        s if s <= 40.0 => 95,
        _ => 100,
    };

    return ((rssi_velocity_score as f32 * RSSI_AND_VELOCITY_SCORE_WEIGHT)
        + (snr_score as f32 * SNR_SCORE_WEIGHT)) as u8;
}

/// Give a numerical score to the connection quality in order to decide whether to look for a new
/// network and to ultimately decide whether to switch to a new network or stay on the same one.
/// score should be between 0 and 1, where 0 is an unusable connection and 1 is a great connection.
pub fn score_connection_quality(_connection_stats: &PeriodicConnectionStats) -> f32 {
    // TODO(fxbug.dev/84551) Actually implement the connection quality scoring and the threshold
    // for a bad connection
    return 1.0;
}

#[cfg(test)]
mod test {
    use {
        super::*,
        crate::{
            client::connection_selection::EWMA_VELOCITY_SMOOTHING_FACTOR,
            config_management::{ConnectFailure, FailureReason, PastConnectionData},
            util::{
                pseudo_energy::SignalData,
                testing::{
                    generate_channel, generate_random_bss, generate_random_saved_network_data,
                    generate_random_scanned_candidate, random_connection_data,
                },
            },
        },
        fuchsia_async as fasync, fuchsia_zircon as zx,
        test_util::assert_gt,
    };

    fn connect_failure_with_bssid(bssid: types::Bssid) -> ConnectFailure {
        ConnectFailure {
            reason: FailureReason::GeneralFailure,
            time: fasync::Time::INFINITE,
            bssid,
        }
    }

    fn past_connection_with_bssid_uptime(
        bssid: types::Bssid,
        uptime: zx::Duration,
    ) -> PastConnectionData {
        PastConnectionData {
            bssid,
            connection_uptime: uptime,
            disconnect_time: fasync::Time::INFINITE, // disconnect will always be considered recent
            ..random_connection_data()
        }
    }

    #[fuchsia::test]
    fn test_weights_sum_to_one() {
        assert_eq!(RSSI_AND_VELOCITY_SCORE_WEIGHT + SNR_SCORE_WEIGHT, 1.0);
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_score_bss_prefers_less_short_connections() {
        let bss_worse =
            types::Bss { rssi: -60, channel: generate_channel(3), ..generate_random_bss() };
        let bss_better =
            types::Bss { rssi: -60, channel: generate_channel(3), ..generate_random_bss() };
        let mut internal_data = generate_random_saved_network_data();
        let short_uptime = zx::Duration::from_seconds(30);
        let okay_uptime = zx::Duration::from_minutes(100);
        // Record a short uptime for the worse network and a long enough uptime for the better one.
        let short_uptime_data = past_connection_with_bssid_uptime(bss_worse.bssid, short_uptime);
        let okay_uptime_data = past_connection_with_bssid_uptime(bss_better.bssid, okay_uptime);
        internal_data.past_connections.add(bss_worse.bssid, short_uptime_data);
        internal_data.past_connections.add(bss_better.bssid, okay_uptime_data);
        let shared_candidate_data = types::ScannedCandidate {
            saved_network_info: internal_data,
            ..generate_random_scanned_candidate()
        };
        let bss_worse = types::ScannedCandidate { bss: bss_worse, ..shared_candidate_data.clone() };
        let bss_better =
            types::ScannedCandidate { bss: bss_better, ..shared_candidate_data.clone() };

        // Check that the better BSS has a higher score than the worse BSS.
        assert!(score_bss_scanned_candidate(bss_better) > score_bss_scanned_candidate(bss_worse));
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_score_bss_prefers_less_failures() {
        let bss_worse =
            types::Bss { rssi: -60, channel: generate_channel(3), ..generate_random_bss() };
        let bss_better =
            types::Bss { rssi: -60, channel: generate_channel(3), ..generate_random_bss() };
        let mut internal_data = generate_random_saved_network_data();
        // Add many test failures for the worse BSS and one for the better BSS
        let mut failures = vec![connect_failure_with_bssid(bss_worse.bssid); 12];
        failures.push(connect_failure_with_bssid(bss_better.bssid));
        internal_data.recent_failures = failures;
        let shared_candidate_data = types::ScannedCandidate {
            saved_network_info: internal_data,
            ..generate_random_scanned_candidate()
        };
        let bss_worse = types::ScannedCandidate { bss: bss_worse, ..shared_candidate_data.clone() };
        let bss_better =
            types::ScannedCandidate { bss: bss_better, ..shared_candidate_data.clone() };
        // Check that the better BSS has a higher score than the worse BSS.
        assert!(score_bss_scanned_candidate(bss_better) > score_bss_scanned_candidate(bss_worse));
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_score_bss_prefers_strong_5ghz_with_failures() {
        // Test test that if one network has a few network failures but is 5 Ghz instead of 2.4,
        // the 5 GHz network has a higher score.
        let bss_worse =
            types::Bss { rssi: -35, channel: generate_channel(3), ..generate_random_bss() };
        let bss_better =
            types::Bss { rssi: -35, channel: generate_channel(36), ..generate_random_bss() };
        let mut internal_data = generate_random_saved_network_data();
        // Set the failure list to have 0 failures for the worse BSS and 4 failures for the
        // stronger BSS.
        internal_data.recent_failures = vec![connect_failure_with_bssid(bss_better.bssid); 2];
        let shared_candidate_data = types::ScannedCandidate {
            saved_network_info: internal_data,
            ..generate_random_scanned_candidate()
        };
        let bss_worse = types::ScannedCandidate { bss: bss_worse, ..shared_candidate_data.clone() };
        let bss_better =
            types::ScannedCandidate { bss: bss_better, ..shared_candidate_data.clone() };
        assert!(score_bss_scanned_candidate(bss_better) > score_bss_scanned_candidate(bss_worse));
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_score_credentials_rejected_worse() {
        // If two BSS are identical other than one failed to connect with wrong credentials and
        // the other failed with a few connect failurs, the one with wrong credentials has a lower
        // score.
        let bss_worse =
            types::Bss { rssi: -30, channel: generate_channel(44), ..generate_random_bss() };
        let bss_better =
            types::Bss { rssi: -30, channel: generate_channel(44), ..generate_random_bss() };
        let mut internal_data = generate_random_saved_network_data();
        // Add many test failures for the worse BSS and one for the better BSS
        let mut failures = vec![connect_failure_with_bssid(bss_better.bssid); 4];
        failures.push(ConnectFailure {
            bssid: bss_worse.bssid,
            time: fasync::Time::now(),
            reason: FailureReason::CredentialRejected,
        });
        internal_data.recent_failures = failures;
        let shared_candidate_data = types::ScannedCandidate {
            saved_network_info: internal_data,
            ..generate_random_scanned_candidate()
        };
        let bss_worse = types::ScannedCandidate { bss: bss_worse, ..shared_candidate_data.clone() };
        let bss_better =
            types::ScannedCandidate { bss: bss_better, ..shared_candidate_data.clone() };

        assert!(score_bss_scanned_candidate(bss_better) > score_bss_scanned_candidate(bss_worse));
    }

    #[fasync::run_singlethreaded(test)]
    async fn score_many_penalties_do_not_cause_panic() {
        let bss = types::Bss { rssi: -80, channel: generate_channel(1), ..generate_random_bss() };
        let mut internal_data = generate_random_saved_network_data();
        // Add 10 general failures and 10 rejected credentials failures
        internal_data.recent_failures = vec![connect_failure_with_bssid(bss.bssid); 10];
        for _ in 0..1200 {
            internal_data.recent_failures.push(ConnectFailure {
                bssid: bss.bssid,
                time: fasync::Time::now(),
                reason: FailureReason::CredentialRejected,
            });
        }
        let short_uptime = zx::Duration::from_seconds(30);
        let data = past_connection_with_bssid_uptime(bss.bssid, short_uptime);
        for _ in 0..10 {
            internal_data.past_connections.add(bss.bssid, data);
        }
        let scanned_candidate = types::ScannedCandidate {
            bss,
            saved_network_info: internal_data,
            ..generate_random_scanned_candidate()
        };

        assert_eq!(score_bss_scanned_candidate(scanned_candidate), i16::MIN);
    }

    // Trivial scoring algorithm test cases. Should pass (or be removed with acknowledgment) for
    // any scoring algorithm implementation.
    #[fuchsia::test]
    fn high_rssi_scores_higher_than_low_rssi() {
        let strong_clear_signal = SignalData::new(-50, 35, 10, EWMA_VELOCITY_SMOOTHING_FACTOR);
        let weak_clear_signal = SignalData::new(-85, 35, 10, EWMA_VELOCITY_SMOOTHING_FACTOR);
        assert_gt!(
            score_current_connection_signal_data(strong_clear_signal),
            score_current_connection_signal_data(weak_clear_signal)
        );

        let strong_noisy_signal = SignalData::new(-50, 5, 10, EWMA_VELOCITY_SMOOTHING_FACTOR);
        let weak_noisy_signal = SignalData::new(-85, 55, 10, EWMA_VELOCITY_SMOOTHING_FACTOR);
        assert_gt!(
            score_current_connection_signal_data(strong_noisy_signal),
            score_current_connection_signal_data(weak_noisy_signal)
        );
    }

    #[fuchsia::test]
    fn high_snr_scores_higher_than_low_snr() {
        let strong_clear_signal = SignalData::new(-50, 35, 10, EWMA_VELOCITY_SMOOTHING_FACTOR);
        let strong_noisy_signal = SignalData::new(-50, 5, 10, EWMA_VELOCITY_SMOOTHING_FACTOR);
        assert_gt!(
            score_current_connection_signal_data(strong_clear_signal),
            score_current_connection_signal_data(strong_noisy_signal)
        );

        let weak_clear_signal = SignalData::new(-85, 35, 10, EWMA_VELOCITY_SMOOTHING_FACTOR);
        let weak_noisy_signal = SignalData::new(-85, 5, 10, EWMA_VELOCITY_SMOOTHING_FACTOR);
        assert_gt!(
            score_current_connection_signal_data(weak_clear_signal),
            score_current_connection_signal_data(weak_noisy_signal)
        );
    }

    #[fuchsia::test]
    fn positive_velocity_scores_higher_than_negative_velocity() {
        let mut improving_signal = SignalData::new(-50, 35, 10, EWMA_VELOCITY_SMOOTHING_FACTOR);
        improving_signal.ewma_rssi_velocity =
            EwmaPseudoDecibel::new(EWMA_VELOCITY_SMOOTHING_FACTOR, 3);
        let mut degrading_signal = SignalData::new(-50, 35, 10, EWMA_VELOCITY_SMOOTHING_FACTOR);
        degrading_signal.ewma_rssi_velocity =
            EwmaPseudoDecibel::new(EWMA_VELOCITY_SMOOTHING_FACTOR, -3);
        assert_gt!(
            score_current_connection_signal_data(improving_signal),
            score_current_connection_signal_data(degrading_signal)
        );
    }

    #[fuchsia::test]
    fn stable_high_rssi_scores_higher_than_volatile_high_rssi() {
        let strong_stable_signal = SignalData::new(-50, 35, 10, EWMA_VELOCITY_SMOOTHING_FACTOR);
        let mut strong_improving_signal = strong_stable_signal;
        strong_improving_signal.ewma_rssi_velocity =
            EwmaPseudoDecibel::new(EWMA_VELOCITY_SMOOTHING_FACTOR, 3);

        assert_gt!(
            score_current_connection_signal_data(strong_stable_signal),
            score_current_connection_signal_data(strong_improving_signal)
        );

        let mut strong_degrading_signal = strong_stable_signal;
        strong_degrading_signal.ewma_rssi_velocity =
            EwmaPseudoDecibel::new(EWMA_VELOCITY_SMOOTHING_FACTOR, -3);

        assert_gt!(
            score_current_connection_signal_data(strong_stable_signal),
            score_current_connection_signal_data(strong_degrading_signal)
        );
    }

    #[fuchsia::test]
    fn improving_weak_rssi_scores_higher_than_stable_weak_rssi() {
        let mut weak_improving_signal =
            SignalData::new(-85, 10, 10, EWMA_VELOCITY_SMOOTHING_FACTOR);
        weak_improving_signal.ewma_rssi_velocity =
            EwmaPseudoDecibel::new(EWMA_VELOCITY_SMOOTHING_FACTOR, 3);
        let weak_stable_signal = SignalData::new(-85, 10, 10, EWMA_VELOCITY_SMOOTHING_FACTOR);

        assert_gt!(
            score_current_connection_signal_data(weak_improving_signal),
            score_current_connection_signal_data(weak_stable_signal)
        );
    }
}
