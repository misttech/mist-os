// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{
        client::types as client_types, config_management::network_config::PastConnectionList,
        util::pseudo_energy::*,
    },
    log::error,
};

// Number of previous RSSI measurements to exponentially weigh into average.
// TODO(fxbug.dev/84870): Tune smoothing factor.
pub const EWMA_SMOOTHING_FACTOR: usize = 10;
// Number of previous RSSI velocities to exponentially weigh into the average. Keeping the number
// small lets the number react quickly and have a magnitude similar to if it weren't smoothed as
// an EWMA, but makes the EWMA less resistant to momentary outliers.
pub const EWMA_VELOCITY_SMOOTHING_FACTOR: usize = 3;

pub const RSSI_AND_VELOCITY_SCORE_WEIGHT: f32 = 0.6;
pub const SNR_SCORE_WEIGHT: f32 = 0.4;

// Threshold for BSS signal scores (bound from 0-100), under which a BSS's signal is considered
// suboptimal. Used to determine if roaming should be considered.
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
        BssQualityData {
            signal_data: signal_data,
            channel: channel,
            phy_rates: (0, 0),
            past_connections_list: past_connections_list,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RoamReason {
    SuboptimalSignal,
}

/// Returns scoring information for a particular BSS
pub fn evaluate_current_bss(bss: BssQualityData) -> (u8, Vec<RoamReason>) {
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
        r if (f64::MIN..=-81.0).contains(&r) => match ewma_rssi_velocity {
            v if (f64::MIN..-2.7).contains(&v) => 0,
            v if (-2.7..-1.8).contains(&v) => 0,
            v if (-1.8..-0.9).contains(&v) => 0,
            v if (-0.9..=0.9).contains(&v) => 0,
            v if (0.9..=1.8).contains(&v) => 20,
            v if (1.8..=2.7).contains(&v) => 18,
            v if (2.7..=f64::MAX).contains(&v) => 10,
            _ => {
                // This shouldn't happen because all possible values should be matched above. If
                // there is an error, return 100 so that roaming is not triggered from this error.
                error!("Unexpected error occurred scoring RSSI velocity {}", ewma_rssi_velocity);
                100
            }
        },
        r if (-81.0..=-76.0).contains(&r) => match ewma_rssi_velocity {
            v if (f64::MIN..-2.7).contains(&v) => 0,
            v if (-2.7..-1.8).contains(&v) => 0,
            v if (-1.8..-0.9).contains(&v) => 0,
            v if (-0.9..=0.9).contains(&v) => 15,
            v if (0.9..=1.8).contains(&v) => 28,
            v if (1.8..=2.7).contains(&v) => 25,
            v if (2.7..=f64::MAX).contains(&v) => 15,
            _ => {
                error!("Unexpected error occurred scoring RSSI velocity {}", ewma_rssi_velocity);
                100
            }
        },
        r if (-75.0..=-71.0).contains(&r) => match ewma_rssi_velocity {
            v if (f64::MIN..-2.7).contains(&v) => 0,
            v if (-2.7..-1.8).contains(&v) => 5,
            v if (-1.8..-0.9).contains(&v) => 15,
            v if (-0.9..=0.9).contains(&v) => 30,
            v if (0.9..=1.8).contains(&v) => 45,
            v if (1.8..=2.7).contains(&v) => 38,
            v if (2.7..=f64::MAX).contains(&v) => 4,
            _ => {
                error!("Unexpected error occurred scoring RSSI velocity {}", ewma_rssi_velocity);
                100
            }
        },
        r if (-70.0..=-66.0).contains(&r) => match ewma_rssi_velocity {
            v if (f64::MIN..-2.7).contains(&v) => 10,
            v if (-2.7..-1.8).contains(&v) => 18,
            v if (-1.8..-0.9).contains(&v) => 30,
            v if (-0.9..=0.9).contains(&v) => 48,
            v if (0.9..=1.8).contains(&v) => 60,
            v if (1.8..=2.7).contains(&v) => 50,
            v if (2.7..=f64::MAX).contains(&v) => 38,
            _ => {
                error!("Unexpected error occurred scoring RSSI velocity {}", ewma_rssi_velocity);
                100
            }
        },
        r if (-65.0..=-61.0).contains(&r) => match ewma_rssi_velocity {
            v if (f64::MIN..-2.7).contains(&v) => 20,
            v if (-2.7..-1.8).contains(&v) => 30,
            v if (-1.8..-0.9).contains(&v) => 45,
            v if (-0.9..=0.9).contains(&v) => 70,
            v if (0.9..=1.8).contains(&v) => 75,
            v if (1.8..=2.7).contains(&v) => 60,
            v if (2.7..=f64::MAX).contains(&v) => 55,
            _ => {
                error!("Unexpected error occurred scoring RSSI velocity {}", ewma_rssi_velocity);
                100
            }
        },
        r if (-60.0..=-56.0).contains(&r) => match ewma_rssi_velocity {
            v if (f64::MIN..-2.7).contains(&v) => 40,
            v if (-2.7..-1.8).contains(&v) => 50,
            v if (-1.8..-0.9).contains(&v) => 63,
            v if (-0.9..=0.9).contains(&v) => 85,
            v if (0.9..=1.8).contains(&v) => 85,
            v if (1.8..=2.7).contains(&v) => 70,
            v if (2.7..=f64::MAX).contains(&v) => 65,
            _ => {
                error!("Unexpected error occurred scoring RSSI velocity {}", ewma_rssi_velocity);
                100
            }
        },
        r if (-55.0..=-51.0).contains(&r) => match ewma_rssi_velocity {
            v if (f64::MIN..-2.7).contains(&v) => 55,
            v if (-2.7..-1.8).contains(&v) => 65,
            v if (-1.8..-0.9).contains(&v) => 75,
            v if (-0.9..=0.9).contains(&v) => 95,
            v if (0.9..=1.8).contains(&v) => 90,
            v if (1.8..=2.7).contains(&v) => 80,
            v if (2.7..=f64::MAX).contains(&v) => 75,
            _ => {
                error!("Unexpected error occurred scoring RSSI velocity {}", ewma_rssi_velocity);
                100
            }
        },
        r if (-50.0..=f64::MAX).contains(&r) => match ewma_rssi_velocity {
            v if (f64::MIN..-2.7).contains(&v) => 60,
            v if (-2.7..-1.8).contains(&v) => 70,
            v if (-1.8..-0.9).contains(&v) => 80,
            v if (-0.9..=0.9).contains(&v) => 100,
            v if (0.9..=1.8).contains(&v) => 95,
            v if (1.8..=2.7).contains(&v) => 90,
            v if (2.7..=f64::MAX).contains(&v) => 80,
            _ => {
                error!("Unexpected error occurred scoring RSSI velocity {}", ewma_rssi_velocity);
                100
            }
        },
        unexpected_rssi => {
            error!("Unexpected error occurreed scoring EWMA RSSI {}", unexpected_rssi);
            100
        }
    };

    let snr_score = match data.ewma_snr.get() {
        s if (f64::MIN..=10.0).contains(&s) => 0,
        s if (10.0..=15.0).contains(&s) => 15,
        s if (15.0..=20.0).contains(&s) => 37,
        s if (20.0..=25.0).contains(&s) => 53,
        s if (25.0..=3.00).contains(&s) => 68,
        s if (30.0..=35.0).contains(&s) => 80,
        s if (35.0..=40.0).contains(&s) => 95,
        s if (40.0..=f64::MAX).contains(&s) => 100,
        unexpected_snr => {
            error!("Unxexpected error occurred scoring EWMA SNR {}", unexpected_snr);
            100
        }
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

    #[fuchsia::test]
    fn test_trivial_signal_data_scores() {
        let strong_clear_stable_signal =
            SignalData::new(-55, 35, 10, EWMA_VELOCITY_SMOOTHING_FACTOR);
        let weak_clear_stable_signal = SignalData::new(-80, 35, 10, EWMA_VELOCITY_SMOOTHING_FACTOR);

        let mut strong_clear_degrading_signal =
            SignalData::new(-55, 35, 10, EWMA_VELOCITY_SMOOTHING_FACTOR);
        strong_clear_degrading_signal.ewma_rssi_velocity =
            EwmaPseudoDecibel::new(EWMA_VELOCITY_SMOOTHING_FACTOR, -3);
        let mut weak_clear_improving_signal =
            SignalData::new(-80, 35, 10, EWMA_VELOCITY_SMOOTHING_FACTOR);
        weak_clear_improving_signal.ewma_rssi_velocity =
            EwmaPseudoDecibel::new(EWMA_VELOCITY_SMOOTHING_FACTOR, 2);

        let strong_noisy_stable_signal =
            SignalData::new(-55, 15, 10, EWMA_VELOCITY_SMOOTHING_FACTOR);

        assert_gt!(
            score_signal_data(strong_clear_stable_signal),
            score_signal_data(weak_clear_stable_signal)
        );

        assert_gt!(
            score_signal_data(strong_clear_stable_signal),
            score_signal_data(strong_clear_degrading_signal)
        );

        assert_gt!(
            score_signal_data(weak_clear_improving_signal),
            score_signal_data(weak_clear_stable_signal)
        );

        assert_gt!(
            score_signal_data(strong_clear_stable_signal),
            score_signal_data(strong_noisy_stable_signal)
        )
    }

    #[fuchsia::test]
    fn test_evaluate_trivial_roam_reasons() {
        // Low RSSI and SNR
        let weak_signal_bss = BssQualityData::new(
            SignalData::new(-90, 5, 10, EWMA_VELOCITY_SMOOTHING_FACTOR),
            channel::Channel::new(11, channel::Cbw::Cbw20),
            PastConnectionList::new(),
        );
        let (_, roam_reasons) = evaluate_current_bss(weak_signal_bss);
        assert!(roam_reasons.iter().any(|&r| r == RoamReason::SuboptimalSignal));

        // Moderate RSSI, low SNR
        let low_snr_bss = BssQualityData::new(
            SignalData::new(-65, 5, 10, EWMA_VELOCITY_SMOOTHING_FACTOR),
            channel::Channel::new(11, channel::Cbw::Cbw20),
            PastConnectionList::new(),
        );
        let (_, roam_reasons) = evaluate_current_bss(low_snr_bss);
        assert!(roam_reasons.iter().any(|&r| r == RoamReason::SuboptimalSignal));
    }
}
