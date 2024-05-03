// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{
        client::{connection_selection::bss_selection, types},
        util::pseudo_energy::SignalData,
    },
    fuchsia_async as fasync,
    futures::channel::mpsc,
};

/// Data tracked about a connection to make decisions about whether to roam.
#[derive(Clone)]
#[cfg_attr(test, derive(Debug, PartialEq))]
pub struct ConnectionData {
    // Information about the current connection, from the time of initial connection.
    pub currently_fulfilled_connection: types::ConnectSelection,
    // Rolling signal data
    pub signal_data: SignalData,
    // Roaming related metadata
    pub roam_decision_data: RoamDecisionData,
}

impl ConnectionData {
    pub fn new(
        currently_fulfilled_connection: types::ConnectSelection,
        signal_data: SignalData,
        connection_start_time: fasync::Time,
    ) -> Self {
        Self {
            currently_fulfilled_connection,
            roam_decision_data: RoamDecisionData::new(
                signal_data.ewma_rssi.get(),
                connection_start_time,
            ),
            signal_data,
        }
    }
}

#[derive(Clone)]
#[cfg_attr(test, derive(Debug, PartialEq))]
pub struct RoamDecisionData {
    pub(crate) time_prev_roam_scan: fasync::Time,
    pub roam_reasons_prev_scan: Vec<bss_selection::RoamReason>,
    /// This is the EWMA value, hence why it is an f64
    pub rssi_prev_roam_scan: f64,
}

impl RoamDecisionData {
    pub fn new(rssi: f64, connection_start_time: fasync::Time) -> Self {
        Self {
            time_prev_roam_scan: connection_start_time,
            roam_reasons_prev_scan: vec![],
            rssi_prev_roam_scan: rssi,
        }
    }
}

#[cfg_attr(test, derive(Debug))]
pub struct RoamSearchRequest {
    pub connection_data: ConnectionData,
    /// This is used to tell the state machine to roam. The state machine should drop its end of
    /// the channel to ignore roam requests if the connection has already changed.
    _roam_req_sender: mpsc::UnboundedSender<types::ScannedCandidate>,
}

impl RoamSearchRequest {
    pub fn new(
        connection_data: ConnectionData,
        _roam_req_sender: mpsc::UnboundedSender<types::ScannedCandidate>,
    ) -> Self {
        RoamSearchRequest { connection_data, _roam_req_sender }
    }
}
