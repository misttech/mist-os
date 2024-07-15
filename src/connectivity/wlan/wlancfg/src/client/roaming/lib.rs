// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::client::types;
use crate::util::pseudo_energy::{EwmaSignalData, RssiVelocity};
use tracing::error;
use {fidl_fuchsia_wlan_internal as fidl_internal, fuchsia_async as fasync};

pub const ROAMING_CHANNEL_BUFFER_SIZE: usize = 100;

// LINT.IfChange
#[derive(Clone, Copy)]
pub enum RoamingPolicy {
    Disabled,
    Enabled { profile: RoamingProfile, mode: RoamingMode },
}

#[derive(Clone, Copy)]
pub enum RoamingProfile {
    Stationary,
}
#[derive(Clone, Copy, Default)]
pub enum RoamingMode {
    MetricsOnly,
    #[default]
    CanRoam,
}

impl From<String> for RoamingPolicy {
    fn from(string: String) -> Self {
        match string.as_str() {
            "enabled_stationary_can_roam" => RoamingPolicy::Enabled {
                profile: RoamingProfile::Stationary,
                mode: RoamingMode::CanRoam,
            },
            "enabled_stationary_metrics_only" => RoamingPolicy::Enabled {
                profile: RoamingProfile::Stationary,
                mode: RoamingMode::MetricsOnly,
            },
            "disabled" => RoamingPolicy::Disabled,
            _ => {
                error!(
                    "Unknown roaming profile string ({}). Continuing with roaming disabled.",
                    string
                );
                RoamingPolicy::Disabled
            }
        }
    }
}
// LINT.ThenChange(//src/lib/assembly/config_schema/src/platform_config/connectivity_config.rs)

/// Data tracked about a connection used to make roaming decisions.
#[derive(Clone)]
#[cfg_attr(test, derive(Debug, PartialEq))]
pub struct RoamingConnectionData {
    // Information about the current connection, from the time of initial connection.
    pub currently_fulfilled_connection: types::ConnectSelection,
    // Tracked and updated throughout the connection.
    pub signal_data: EwmaSignalData,
    pub rssi_velocity: RssiVelocity,
    pub previous_roam_scan_data: PreviousRoamScanData,
}
impl RoamingConnectionData {
    pub fn new(
        currently_fulfilled_connection: types::ConnectSelection,
        signal_data: EwmaSignalData,
    ) -> Self {
        Self {
            currently_fulfilled_connection: currently_fulfilled_connection.clone(),
            signal_data,
            rssi_velocity: RssiVelocity::new(signal_data.ewma_rssi.get()),
            previous_roam_scan_data: PreviousRoamScanData::new(
                currently_fulfilled_connection.target.bss.signal.rssi_dbm,
            ),
        }
    }
}
// Metadata related to the previous roam scan event.
#[derive(Clone)]
#[cfg_attr(test, derive(Debug, PartialEq))]
pub struct PreviousRoamScanData {
    pub(crate) time_prev_roam_scan: fasync::Time,
    pub roam_reasons_prev_scan: Vec<RoamReason>,
    /// This is the EWMA value, hence why it is an f64
    pub rssi_prev_roam_scan: f64,
}
impl PreviousRoamScanData {
    pub fn new(rssi: impl Into<f64>) -> Self {
        Self {
            time_prev_roam_scan: fasync::Time::now(),
            roam_reasons_prev_scan: vec![],
            rssi_prev_roam_scan: rssi.into(),
        }
    }
}

// Data that could trigger roaming actions to occur. Roam monitor implementations
// MUST read all trigger data types from the channel, even if they ignore/drop it.
#[derive(Clone, Debug)]
pub enum RoamTriggerData {
    SignalReportInd(fidl_internal::SignalReportIndication),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RoamReason {
    RssiBelowThreshold,
    SnrBelowThreshold,
}
