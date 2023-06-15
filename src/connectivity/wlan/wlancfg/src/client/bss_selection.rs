// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{
        client::types as client_types, config_management::network_config::PastConnectionList,
        util::pseudo_energy::*,
    },
    tracing::error,
};

/// Number of previous RSSI measurements to exponentially weigh into average.
/// TODO(fxbug.dev/84870): Tune smoothing factor.
pub const EWMA_SMOOTHING_FACTOR: usize = 10;
/// Number of previous RSSI velocities to exponentially weigh into the average. Keeping the number
/// small lets the number react quickly and have a magnitude similar to if it weren't smoothed as
/// an EWMA, but makes the EWMA less resistant to momentary outliers.
pub const EWMA_VELOCITY_SMOOTHING_FACTOR: usize = 3;

pub const RSSI_AND_VELOCITY_SCORE_WEIGHT: f32 = 0.6;
pub const SNR_SCORE_WEIGHT: f32 = 0.4;

/// Threshold for BSS signal scores (bound from 0-100), under which a BSS's signal is considered
/// suboptimal. Used to determine if roaming should be considered.
pub const SUBOPTIMAL_SIGNAL_THRESHOLD: u8 = 45;

/// Connection quality data related to signal
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SignalData {
    pub ewma_rssi: EwmaPseudoDecibel,
    pub ewma_snr: EwmaPseudoDecibel,
    pub ewma_rssi_velocity: EwmaPseudoDecibel,
}

impl SignalData {
    pub fn new(
        initial_rssi: PseudoDecibel,
        initial_snr: PseudoDecibel,
        ewma_weight: usize,
        ewma_velocity_weight: usize,
    ) -> Self {
        Self {
            ewma_rssi: EwmaPseudoDecibel::new(ewma_weight, initial_rssi),
            ewma_snr: EwmaPseudoDecibel::new(ewma_weight, initial_snr),
            ewma_rssi_velocity: EwmaPseudoDecibel::new(ewma_velocity_weight, 0),
        }
    }
    pub fn update_with_new_measurement(&mut self, rssi: PseudoDecibel, snr: PseudoDecibel) {
        let prev_rssi = self.ewma_rssi.get();
        self.ewma_rssi.update_average(rssi);
        self.ewma_snr.update_average(snr);

        match calculate_pseudodecibel_velocity(vec![prev_rssi, self.ewma_rssi.get()]) {
            Ok(velocity) => {
                self.ewma_rssi_velocity.update_average(velocity);
            }
            Err(e) => {
                error!("Failed to calculate SignalData velocity: {:?}", e);
            }
        }
    }
}

/// Aggregated information about the current BSS's connection quality, used for evaluation.
#[derive(Clone, Debug)]
pub struct BssQualityData {
    pub signal_data: SignalData,
    pub channel: client_types::WlanChan,
    // TX and RX rate, respectively.
    pub phy_rates: (u32, u32),
    // Connection data  of past successful connections to this BSS.
    pub past_connections_list: PastConnectionList,
}

impl BssQualityData {
    pub fn new(
        signal_data: SignalData,
        channel: client_types::WlanChan,
        past_connections_list: PastConnectionList,
    ) -> Self {
        BssQualityData { signal_data, channel, phy_rates: (0, 0), past_connections_list }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RoamReason {
    SuboptimalSignal,
}

/// Returns scoring information for a particular BSS
pub fn evaluate_current_bss(bss: &BssQualityData) -> (u8, Vec<RoamReason>) {
    let signal_score = score_signal_data(bss.signal_data);
    let mut roam_reasons: Vec<RoamReason> = vec![];

    // Add RoamReasons based on the raw signal score
    // TODO(haydennix): Add more RoamReasons for different score ranges
    match signal_score {
        u8::MIN..=SUBOPTIMAL_SIGNAL_THRESHOLD => {
            roam_reasons.push(RoamReason::SuboptimalSignal);
        }
        _ => {}
    }

    // TODO(haydennix): Adjust score based on system requirements,  user intent, etc.
    return (signal_score, roam_reasons);
}

/// Scoring table from go/tq-bss-eval-design.
fn score_signal_data(data: SignalData) -> u8 {
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

#[cfg(test)]
mod test {
    use {
        super::*,
        test_util::{assert_gt, assert_lt},
        wlan_common::channel,
    };

    #[fuchsia::test]
    fn test_update_with_new_measurements() {
        let mut signal_data =
            SignalData::new(-40, 30, EWMA_SMOOTHING_FACTOR, EWMA_VELOCITY_SMOOTHING_FACTOR);
        signal_data.update_with_new_measurement(-60, 15);
        assert_lt!(signal_data.ewma_rssi.get(), -40.0);
        assert_gt!(signal_data.ewma_rssi.get(), -60.0);
        assert_lt!(signal_data.ewma_snr.get(), 30.0);
        assert_gt!(signal_data.ewma_snr.get(), 15.0);
        assert_lt!(signal_data.ewma_rssi_velocity.get(), 0.0);
    }

    #[fuchsia::test]
    fn test_weights_sum_to_one() {
        assert_eq!(RSSI_AND_VELOCITY_SCORE_WEIGHT + SNR_SCORE_WEIGHT, 1.0);
    }

    // Trivial scoring algorithm test cases. Should pass (or be removed with acknowledgment) for
    // any scoring algorithm implementation.
    #[fuchsia::test]
    fn high_rssi_scores_higher_than_low_rssi() {
        let strong_clear_signal = SignalData::new(-50, 35, 10, EWMA_VELOCITY_SMOOTHING_FACTOR);
        let weak_clear_signal = SignalData::new(-85, 35, 10, EWMA_VELOCITY_SMOOTHING_FACTOR);
        assert_gt!(score_signal_data(strong_clear_signal), score_signal_data(weak_clear_signal));

        let strong_noisy_signal = SignalData::new(-50, 5, 10, EWMA_VELOCITY_SMOOTHING_FACTOR);
        let weak_noisy_signal = SignalData::new(-85, 55, 10, EWMA_VELOCITY_SMOOTHING_FACTOR);
        assert_gt!(score_signal_data(strong_noisy_signal), score_signal_data(weak_noisy_signal));
    }

    #[fuchsia::test]
    fn high_snr_scores_higher_than_low_snr() {
        let strong_clear_signal = SignalData::new(-50, 35, 10, EWMA_VELOCITY_SMOOTHING_FACTOR);
        let strong_noisy_signal = SignalData::new(-50, 5, 10, EWMA_VELOCITY_SMOOTHING_FACTOR);
        assert_gt!(score_signal_data(strong_clear_signal), score_signal_data(strong_noisy_signal));

        let weak_clear_signal = SignalData::new(-85, 35, 10, EWMA_VELOCITY_SMOOTHING_FACTOR);
        let weak_noisy_signal = SignalData::new(-85, 5, 10, EWMA_VELOCITY_SMOOTHING_FACTOR);
        assert_gt!(score_signal_data(weak_clear_signal), score_signal_data(weak_noisy_signal));
    }

    #[fuchsia::test]
    fn positive_velocity_scores_higher_than_negative_velocity() {
        let mut improving_signal = SignalData::new(-50, 35, 10, EWMA_VELOCITY_SMOOTHING_FACTOR);
        improving_signal.ewma_rssi_velocity =
            EwmaPseudoDecibel::new(EWMA_VELOCITY_SMOOTHING_FACTOR, 3);
        let mut degrading_signal = SignalData::new(-50, 35, 10, EWMA_VELOCITY_SMOOTHING_FACTOR);
        degrading_signal.ewma_rssi_velocity =
            EwmaPseudoDecibel::new(EWMA_VELOCITY_SMOOTHING_FACTOR, -3);
        assert_gt!(score_signal_data(improving_signal), score_signal_data(degrading_signal));
    }

    #[fuchsia::test]
    fn stable_high_rssi_scores_higher_than_volatile_high_rssi() {
        let strong_stable_signal = SignalData::new(-50, 35, 10, EWMA_VELOCITY_SMOOTHING_FACTOR);
        let mut strong_improving_signal = strong_stable_signal;
        strong_improving_signal.ewma_rssi_velocity =
            EwmaPseudoDecibel::new(EWMA_VELOCITY_SMOOTHING_FACTOR, 3);

        assert_gt!(
            score_signal_data(strong_stable_signal),
            score_signal_data(strong_improving_signal)
        );

        let mut strong_degrading_signal = strong_stable_signal;
        strong_degrading_signal.ewma_rssi_velocity =
            EwmaPseudoDecibel::new(EWMA_VELOCITY_SMOOTHING_FACTOR, -3);

        assert_gt!(
            score_signal_data(strong_stable_signal),
            score_signal_data(strong_degrading_signal)
        );
    }

    #[fuchsia::test]
    fn improving_weak_rssi_scores_higher_than_stable_weak_rssi() {
        let mut weak_improving_signal =
            SignalData::new(-85, 10, 10, EWMA_VELOCITY_SMOOTHING_FACTOR);
        weak_improving_signal.ewma_rssi_velocity =
            EwmaPseudoDecibel::new(EWMA_VELOCITY_SMOOTHING_FACTOR, 3);
        let weak_stable_signal = SignalData::new(-85, 10, 10, EWMA_VELOCITY_SMOOTHING_FACTOR);

        assert_gt!(score_signal_data(weak_improving_signal), score_signal_data(weak_stable_signal));
    }

    #[fuchsia::test]
    fn test_evaluate_trivial_roam_reasons() {
        // Low RSSI and SNR
        let weak_signal_bss = BssQualityData::new(
            SignalData::new(-90, 5, 10, EWMA_VELOCITY_SMOOTHING_FACTOR),
            channel::Channel::new(11, channel::Cbw::Cbw20),
            PastConnectionList::default(),
        );
        let (_, roam_reasons) = evaluate_current_bss(&weak_signal_bss);
        assert!(roam_reasons.iter().any(|&r| r == RoamReason::SuboptimalSignal));

        // Moderate RSSI, low SNR
        let low_snr_bss = BssQualityData::new(
            SignalData::new(-65, 5, 10, EWMA_VELOCITY_SMOOTHING_FACTOR),
            channel::Channel::new(11, channel::Cbw::Cbw20),
            PastConnectionList::default(),
        );
        let (_, roam_reasons) = evaluate_current_bss(&low_snr_bss);
        assert!(roam_reasons.iter().any(|&r| r == RoamReason::SuboptimalSignal));
    }
}
