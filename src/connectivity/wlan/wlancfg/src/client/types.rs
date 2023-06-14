// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::config_management::{self},
    fidl_fuchsia_wlan_internal as fidl_internal, fidl_fuchsia_wlan_policy as fidl_policy,
    fidl_fuchsia_wlan_sme as fidl_sme, fuchsia_zircon as zx,
    wlan_common::{
        self, bss::BssDescription, channel::Channel, hasher::WlanHasher,
        security::SecurityAuthenticator, sequestered::Sequestered,
    },
    wlan_metrics_registry::{
        PolicyConnectionAttemptMigratedMetricDimensionReason,
        PolicyDisconnectionMigratedMetricDimensionReason,
    },
};

#[cfg(test)]
pub(crate) use crate::regulatory_manager::REGION_CODE_LEN;

pub type NetworkIdentifier = config_management::network_config::NetworkIdentifier;
pub type SecurityTypeDetailed = fidl_sme::Protection;
pub type SecurityType = config_management::network_config::SecurityType;
pub type ConnectionState = fidl_policy::ConnectionState;
pub type ClientState = fidl_policy::WlanClientState;
pub type DisconnectStatus = fidl_policy::DisconnectStatus;
pub type Compatibility = fidl_policy::Compatibility;
pub type WlanChan = wlan_common::channel::Channel;
pub type Cbw = wlan_common::channel::Cbw;
pub use ieee80211::Bssid;
pub use ieee80211::Ssid;
pub type DisconnectReason = PolicyDisconnectionMigratedMetricDimensionReason;
pub type ConnectReason = PolicyConnectionAttemptMigratedMetricDimensionReason;
pub type ScanError = fidl_policy::ScanErrorCode;

pub fn convert_to_sme_disconnect_reason(
    disconnect_reason: PolicyDisconnectionMigratedMetricDimensionReason,
) -> fidl_sme::UserDisconnectReason {
    match disconnect_reason {
        PolicyDisconnectionMigratedMetricDimensionReason::Unknown => {
            fidl_sme::UserDisconnectReason::Unknown
        }
        PolicyDisconnectionMigratedMetricDimensionReason::FailedToConnect => {
            fidl_sme::UserDisconnectReason::FailedToConnect
        }
        PolicyDisconnectionMigratedMetricDimensionReason::FidlConnectRequest => {
            fidl_sme::UserDisconnectReason::FidlConnectRequest
        }
        PolicyDisconnectionMigratedMetricDimensionReason::FidlStopClientConnectionsRequest => {
            fidl_sme::UserDisconnectReason::FidlStopClientConnectionsRequest
        }
        PolicyDisconnectionMigratedMetricDimensionReason::ProactiveNetworkSwitch => {
            fidl_sme::UserDisconnectReason::ProactiveNetworkSwitch
        }
        PolicyDisconnectionMigratedMetricDimensionReason::DisconnectDetectedFromSme => {
            fidl_sme::UserDisconnectReason::DisconnectDetectedFromSme
        }
        PolicyDisconnectionMigratedMetricDimensionReason::RegulatoryRegionChange => {
            fidl_sme::UserDisconnectReason::RegulatoryRegionChange
        }
        PolicyDisconnectionMigratedMetricDimensionReason::Startup => {
            fidl_sme::UserDisconnectReason::Startup
        }
        PolicyDisconnectionMigratedMetricDimensionReason::NetworkUnsaved => {
            fidl_sme::UserDisconnectReason::NetworkUnsaved
        }
        PolicyDisconnectionMigratedMetricDimensionReason::NetworkConfigUpdated => {
            fidl_sme::UserDisconnectReason::NetworkConfigUpdated
        }
    }
}

// An internal version of fidl_policy::ScanResult that can be cloned
// To avoid printing PII, only allow Debug in tests, runtime logging should use Display
#[cfg_attr(test, derive(Debug))]
#[derive(Clone, PartialEq)]
pub struct ScanResult {
    /// Network properties used to distinguish between networks and to group
    /// individual APs.
    pub ssid: Ssid,
    pub security_type_detailed: SecurityTypeDetailed,
    /// Individual access points offering the specified network.
    pub entries: Vec<Bss>,
    /// Indication if the detected network is supported by the implementation.
    pub compatibility: Compatibility,
}

// Only derive(Debug) in tests, we should never directly print this in non-test code
#[cfg_attr(test, derive(Debug, PartialEq, Eq, PartialOrd, Ord))]
pub struct NetworkIdentifierDetailed {
    pub ssid: Ssid,
    pub security_type: SecurityTypeDetailed,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ScanObservation {
    Passive,
    Active,
    Unknown,
}

// An internal version of fidl_policy::Bss with extended information
// To avoid printing PII, only allow Debug in tests, runtime logging should use Display
#[cfg_attr(test, derive(Debug))]
#[derive(Clone, PartialEq)]
pub struct Bss {
    /// MAC address for the AP interface.
    pub bssid: Bssid,
    /// Calculated received signal strength for the beacon/probe response.
    pub rssi: i8,
    /// Signal to noise ratio  for the beacon/probe response.
    pub snr_db: i8,
    /// Channel for this network.
    pub channel: WlanChan,
    /// Realtime timestamp for this scan result entry.
    pub timestamp: zx::Time,
    /// The scanning mode used to observe the BSS.
    pub observation: ScanObservation,
    /// Compatibility with this device's network stack.
    pub compatibility: Option<wlan_common::scan::Compatibility>,
    /// The BSS description with information that SME needs for connecting.
    pub bss_description: Sequestered<fidl_internal::BssDescription>,
}

impl Bss {
    pub fn is_compatible(&self) -> bool {
        self.compatibility.is_some()
    }
}

// TODO(fxbug.dev/113996): Move this into `wlan_common::bss` and use it in place of signal fields
//                         in `BssDescription`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Signal {
    pub rssi_dbm: i8,
    pub snr_db: i8,
}

/// BSS information tracked by the client state machine.
///
/// While connected to an AP, some important BSS configuration may change, such as the channel and
/// signal quality statistics. `TrackedBss` provides fields for this configuration that are managed
/// by the client state machine.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TrackedBss {
    pub signal: Signal,
    pub channel: Channel,
}

impl TrackedBss {
    /// Snapshots a BSS description.
    ///
    /// A snapshot copies configuration from the given BSS description into a `TrackedBss`.
    pub fn snapshot(original: &BssDescription) -> Self {
        TrackedBss {
            signal: Signal { rssi_dbm: original.rssi_dbm, snr_db: original.snr_db },
            channel: original.channel,
        }
    }
}

impl PartialEq<BssDescription> for TrackedBss {
    fn eq(&self, bss: &BssDescription) -> bool {
        // This implementation is robust in the face of changes to `BssDescription` and
        // `TrackedBss`, but must copy fields. This could be deceptively expensive for an
        // equivalence query if `TrackedBss` has many congruent fields with respect to
        // `BssDescription`.
        *self == TrackedBss::snapshot(bss)
    }
}

impl PartialEq<TrackedBss> for BssDescription {
    fn eq(&self, tracked: &TrackedBss) -> bool {
        tracked == self
    }
}

/// Candidate BSS observed in a scan.
#[derive(Debug, Clone)]
#[cfg_attr(test, derive(PartialEq))]
pub struct InternalSavedNetworkData {
    pub has_ever_connected: bool,
    pub recent_failures: Vec<config_management::ConnectFailure>,
    pub past_connections:
        config_management::HistoricalListsByBssid<config_management::PastConnectionData>,
}
#[derive(Clone)]
// To avoid printing PII, only allow Debug in tests, runtime logging should use Display
#[cfg_attr(test, derive(Debug))]
#[cfg_attr(test, derive(PartialEq))]
pub struct ScannedCandidate {
    pub network: NetworkIdentifier,
    pub security_type_detailed: SecurityTypeDetailed,
    pub credential: config_management::Credential,
    pub bss: Bss,
    pub network_has_multiple_bss: bool,
    pub authenticator: SecurityAuthenticator,
    pub saved_network_info: InternalSavedNetworkData,
    pub hasher: WlanHasher,
}

/// Selected network candidate for a connection.
///
/// This type is a promotion of a scanned candidate and provides the necessary data required to
/// establish a connection.
#[derive(Clone)]
// To avoid printing PII, only allow Debug in tests, runtime logging should use Display
#[cfg_attr(test, derive(Debug))]
#[cfg_attr(test, derive(PartialEq))]
pub struct ConnectSelection {
    pub target: ScannedCandidate,
    pub reason: ConnectReason,
}

/// The state of a remote AP.
///
/// `ApState` describes the configuration of a BSS to which a client is connected. The state is
/// comprised of an initial BSS description as well as tracked configuration. The tracked
/// configuration may change while a client is connected and is managed by the client state
/// machine. The initial BSS description is immutable.
///
/// See `TrackedBss`.
#[derive(Clone, Debug, PartialEq)]
pub struct ApState {
    /// The initial configuration of the BSS (e.g., as seen from a scan).
    original: BssDescription,
    /// Tracked BSS configuration.
    ///
    /// This subset of the initial BSS description is managed by the client state machine and may
    /// change while connected to an AP.
    pub tracked: TrackedBss,
}

impl ApState {
    /// Gets the initial BSS description for the AP to which a client is connected.
    pub fn original(&self) -> &BssDescription {
        &self.original
    }

    /// Returns `true` if the tracked BSS configuration differs from the initial BSS description.
    pub fn has_changes(&self) -> bool {
        self.original != self.tracked
    }
}

impl From<BssDescription> for ApState {
    fn from(original: BssDescription) -> Self {
        let tracked = TrackedBss::snapshot(&original);
        ApState { original, tracked }
    }
}
