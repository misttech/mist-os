// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::client::roaming::lib::*;
use crate::client::roaming::roam_monitor::RoamMonitorApi;
use crate::client::types;
use anyhow::format_err;

pub struct DefaultRoamMonitor {}

impl DefaultRoamMonitor {
    pub fn new() -> Self {
        Self {}
    }
}

impl RoamMonitorApi for DefaultRoamMonitor {
    fn should_roam_search(&mut self, _data: RoamTriggerData) -> Result<bool, anyhow::Error> {
        // Default response to never roam scan. Metrics for default devices can be added here.
        Ok(false)
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
    fn get_roam_data(&self) -> Result<RoamingConnectionData, anyhow::Error> {
        // This is currently unused for devices that do not roam scan, but not strictly forbidden
        // from use in the future.
        Err(format_err!("Default roam monitor unexpectedly received request for roam data."))
    }
}
#[cfg(test)]
mod test {
    use super::*;
    use crate::util::testing::generate_random_scanned_candidate;
    use fidl_fuchsia_wlan_internal as fidl_internal;
    use wlan_common::assert_variant;

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_should_roam_search_returns_false() {
        let mut monitor = DefaultRoamMonitor::new();

        // Send each type of trigger data and verify the default monitor always returns false.
        assert_variant!(
            monitor.should_roam_search(RoamTriggerData::SignalReportInd(
                fidl_internal::SignalReportIndication { rssi_dbm: -100, snr_db: 0 },
            )),
            Ok(false)
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
