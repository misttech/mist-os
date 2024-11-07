// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::client::roaming::lib::*;
use crate::client::roaming::roam_monitor::{RoamMonitorApi, RoamTriggerDataOutcome};
use crate::client::types;
use anyhow::format_err;

pub struct DefaultRoamMonitor {}

impl Default for DefaultRoamMonitor {
    fn default() -> Self {
        Self::new()
    }
}

impl DefaultRoamMonitor {
    pub fn new() -> Self {
        Self {}
    }
}

use async_trait::async_trait;
#[async_trait(?Send)]
impl RoamMonitorApi for DefaultRoamMonitor {
    async fn handle_roam_trigger_data(
        &mut self,
        _data: RoamTriggerData,
    ) -> Result<RoamTriggerDataOutcome, anyhow::Error> {
        // Default response to noop. Metrics for default devices can be added here.
        Ok(RoamTriggerDataOutcome::Noop)
    }
    fn should_send_roam_request(
        &self,
        candidate: types::ScannedCandidate,
    ) -> Result<bool, anyhow::Error> {
        Err(format_err!(
            "Default roam monitor unexpectedly received a roam candidate: {}",
            candidate.to_string_without_pii()
        ))
    }
}
#[cfg(test)]
mod test {
    use super::*;
    use crate::util::testing::generate_random_scanned_candidate;
    use fidl_fuchsia_wlan_internal as fidl_internal;
    use wlan_common::assert_variant;

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_handle_roam_trigger_data_always_returns_noop() {
        let mut monitor = DefaultRoamMonitor::new();

        // Send each type of trigger data and verify the default monitor always returns noop.
        assert_variant!(
            monitor
                .handle_roam_trigger_data(RoamTriggerData::SignalReportInd(
                    fidl_internal::SignalReportIndication { rssi_dbm: -100, snr_db: 0 },
                ))
                .await,
            Ok(RoamTriggerDataOutcome::Noop)
        );
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_should_send_roam_request_returns_error() {
        let monitor = DefaultRoamMonitor::new();

        // Send a candidate and verify an error is returned
        let candidate = generate_random_scanned_candidate();
        assert_variant!(monitor.should_send_roam_request(candidate), Err(_));
    }
}
