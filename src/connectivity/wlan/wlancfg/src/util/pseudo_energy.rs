// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::{format_err, Error},
    std::convert::TryFrom,
    tracing::error,
};

/// Unified type for dB, dBm, and dB/s for Policy consumption.
pub type PseudoDecibel = i8;

/// Update a weighted average with a new measurement
fn calculate_ewma_update(current: f64, next: f64, weighting_factor: f64) -> f64 {
    let weight = 2.0 / (1.0 + weighting_factor);
    return weight * next + (1.0 - weight) * current;
}

/// Struct for maintaining a dB or dBm exponentially weighted moving average. Differs from
/// SignalStrengthAverage, which is not exponentially weighted.
///
/// dB and dBm are represented throughout the WLAN stack as an i8. However, due to integer rounding,
/// small updates to the average may never move an i8 value (e.g. avg(-50, -51) rounds to -50, so
/// updates of -51 will never result in an average of -51). This struct maintains the average signal
/// strength as an f64, so even small changes will affect the average.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EwmaPseudoDecibel {
    current: f64,
    weighting_factor: f64,
}

impl EwmaPseudoDecibel {
    pub fn new(n: usize, initial_signal: PseudoDecibel) -> Self {
        Self { current: initial_signal as f64, weighting_factor: n as f64 }
    }

    /// Returns the current EWMA value
    pub fn get(&self) -> f64 {
        self.current
    }

    pub fn update_average(&mut self, next: impl Into<f64>) {
        self.current = calculate_ewma_update(self.current, next.into(), self.weighting_factor);
    }
}

/// Calculates the rate of change across a vector of dB measurements by determining
/// the slope of the line of best fit using least squares regression. Return is technically
/// dB(i8)/t where t is the unit of time used in the vector. Returns error if integer overflows.
///
/// Note: This is the linear velocity (not the logarithmic velocity), but it is a useful
/// abstraction for monitoring real-world signal changes.
///
/// Intended to be used for RSSI Values, ranging from -128 to -1.
pub fn calculate_pseudodecibel_velocity(historical_pdb: Vec<f64>) -> Result<f64, Error> {
    let n = i32::try_from(historical_pdb.len())?;
    if n < 2 {
        return Err(format_err!("At least two data points required to calculate velocity"));
    }
    // Using i32 for the calculations, to allow more room for preventing overflows
    let mut sum_x: i32 = 0;
    let mut sum_y: i32 = 0;
    let mut sum_xy: i32 = 0;
    let mut sum_x2: i32 = 0;

    // Least squares regression summations, returning an error if there are any overflows
    for (i, y) in historical_pdb.iter().enumerate() {
        if *y >= 0.0 {
            return Err(format_err!(
                "Function is intended for RSSI values, which should be -128 < n < 0"
            ));
        }
        let x = i32::try_from(i).map_err(|_| format_err!("failed to convert index to i32"))?;
        sum_x = sum_x.checked_add(x).ok_or_else(|| format_err!("overflow of X summation"))?;
        sum_y =
            sum_y.checked_add(*y as i32).ok_or_else(|| format_err!("overflow of Y summation"))?;
        sum_xy = sum_xy
            .checked_add(x.checked_mul(*y as i32).ok_or_else(|| format_err!("overflow of X * Y"))?)
            .ok_or_else(|| format_err!("overflow of XY summation"))?;
        sum_x2 = sum_x2
            .checked_add(x.checked_mul(x).ok_or_else(|| format_err!("overflow of X**2"))?)
            .ok_or_else(|| format_err!("overflow of X2 summation"))?;
    }

    // Calculate velocity from summations, returning an error if there are any overflows. Note that
    // in practice, the try_from should never fail, since the input values are bound from 0 to -128.
    let velocity = (n.checked_mul(sum_xy).ok_or_else(|| format_err!("overflow in n * sum_xy"))?
        - sum_x.checked_mul(sum_y).ok_or_else(|| format_err!("overflow in sum_x * sum_y"))?)
        / (n.checked_mul(sum_x2).ok_or_else(|| format_err!("overflow in n * sum_x2"))?
            - sum_x.checked_mul(sum_x).ok_or_else(|| format_err!("overflow in sum_x**2"))?);
    Ok(velocity.into())
}

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

#[cfg(test)]
mod tests {
    use {
        super::*,
        test_util::{assert_gt, assert_lt},
    };

    #[fuchsia::test]
    fn test_simple_averaging_calculations() {
        let mut ewma_signal = EwmaPseudoDecibel::new(10, -50);
        assert_eq!(ewma_signal.get(), -50.0);

        // Validate average moves using exponential weighting
        ewma_signal.update_average(-60);
        assert_lt!(ewma_signal.get(), -50.0);
        assert_gt!(ewma_signal.get(), -60.0);

        // Validate average will eventually stabilize.
        for _ in 0..20 {
            ewma_signal.update_average(-60)
        }
        assert_eq!(ewma_signal.get().round(), -60.0);
    }

    #[fuchsia::test]
    fn test_small_variation_averaging() {
        let mut ewma_signal = EwmaPseudoDecibel::new(5, -90);
        assert_eq!(ewma_signal.get(), -90.0);

        // Validate that a small change that does not change the i8 dBm average still changes the
        // internal f64 average.
        ewma_signal.update_average(-91);
        assert_eq!(ewma_signal.get().round(), -90.0);
        assert_lt!(ewma_signal.current, -90.0);

        // Validate that eventually the small changes are enough to change the i8 dbm average.
        for _ in 0..5 {
            ewma_signal.update_average(-91);
        }
        assert_lt!(ewma_signal.get(), -90.9);
    }

    /// Vector argument must have length >=2, and be negative.
    #[fuchsia::test]
    fn test_insufficient_args() {
        assert!(calculate_pseudodecibel_velocity(vec![]).is_err());
        assert!(calculate_pseudodecibel_velocity(vec![-60.0]).is_err());
        assert!(calculate_pseudodecibel_velocity(vec![-60.0, 20.0]).is_err());
    }

    #[test]
    fn test_calculate_negative_velocity() {
        assert_eq!(
            calculate_pseudodecibel_velocity(vec![-60.0, -75.0]).expect("failed to calculate"),
            -15.0
        );
        assert_eq!(
            calculate_pseudodecibel_velocity(vec![-40.0, -50.0, -58.0, -64.0])
                .expect("failed to calculate"),
            -8.0
        );
    }

    #[test]
    fn test_calculate_positive_velocity() {
        assert_eq!(
            calculate_pseudodecibel_velocity(vec![-48.0, -45.0]).expect("failed to calculate"),
            3.0
        );
        assert_eq!(
            calculate_pseudodecibel_velocity(vec![-70.0, -55.0, -45.0, -30.0])
                .expect("failed to calculate"),
            13.0
        );
    }

    #[test]
    fn test_calculate_constant_zero_velocity() {
        assert_eq!(
            calculate_pseudodecibel_velocity(vec![-25.0, -25.0, -25.0, -25.0, -25.0, -25.0])
                .expect("failed to calculate"),
            0.0
        );
    }

    #[test]
    fn test_calculate_oscillating_zero_velocity() {
        assert_eq!(
            calculate_pseudodecibel_velocity(
                vec![-35.0, -45.0, -35.0, -25.0, -35.0, -45.0, -35.0,]
            )
            .expect("failed to calculate"),
            0.0
        );
    }

    #[test]
    fn test_calculate_min_max_velocity() {
        assert_eq!(
            calculate_pseudodecibel_velocity(vec![-1.0, -128.0]).expect("failed to calculate"),
            -127.0
        );
        assert_eq!(
            calculate_pseudodecibel_velocity(vec![-128.0, -1.0]).expect("failed to calculate"),
            127.0
        );
    }

    #[test]
    fn test_update_with_new_measurements() {
        let mut signal_data = SignalData::new(-40, 30, 10, 3);
        signal_data.update_with_new_measurement(-60, 15);
        assert_lt!(signal_data.ewma_rssi.get(), -40.0);
        assert_gt!(signal_data.ewma_rssi.get(), -60.0);
        assert_lt!(signal_data.ewma_snr.get(), 30.0);
        assert_gt!(signal_data.ewma_snr.get(), 15.0);
        assert_lt!(signal_data.ewma_rssi_velocity.get(), 0.0);
    }
}
