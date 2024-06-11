// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{
        client::types,
        util::pseudo_energy::{EwmaSignalData, RssiVelocity},
    },
    fuchsia_async as fasync,
    futures::channel::mpsc,
    tracing::error,
};

#[derive(Clone, Copy)]
pub enum RoamingProfile {
    RoamingOff,
    StationaryRoaming,
}

impl From<String> for RoamingProfile {
    fn from(string: String) -> Self {
        match string.as_str() {
            "roaming_off" => Self::RoamingOff,
            "stationary_roaming" => Self::StationaryRoaming,
            _ => {
                error!("Invalid roam profile: {}. Defaulting to RoamingOff.", string);
                Self::RoamingOff
            }
        }
    }
}

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

// Requests to execute BSS selection, searching for a roam candidate.
#[cfg_attr(test, derive(Debug))]
pub struct RoamSearchRequest {
    pub connection_data: RoamingConnectionData,
    _roam_req_sender: mpsc::UnboundedSender<types::ScannedCandidate>,
}
impl RoamSearchRequest {
    pub fn new(
        connection_data: RoamingConnectionData,
        // Sender to tell state machine to roam. State machine should drop the receiver end if the
        // connection has changed.
        _roam_req_sender: mpsc::UnboundedSender<types::ScannedCandidate>,
    ) -> Self {
        RoamSearchRequest { connection_data, _roam_req_sender }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RoamReason {
    RssiBelowThreshold,
    SnrBelowThreshold,
}
