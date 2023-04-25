// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    fidl_fuchsia_wlan_common as fidl_common, fidl_fuchsia_wlan_internal as fidl_internal,
    fidl_fuchsia_wlan_sme as fidl_sme,
    serde::{Deserialize, Serialize},
};

/// Enums and structs for wlan client status.
/// These definitions come from fuchsia.wlan.policy/client_provider.fidl
///
#[derive(Serialize, Deserialize, Debug)]
pub enum WlanClientState {
    ConnectionsDisabled = 1,
    ConnectionsEnabled = 2,
}

#[derive(Serialize, Deserialize, Debug)]
pub enum ConnectionState {
    Failed = 1,
    Disconnected = 2,
    Connecting = 3,
    Connected = 4,
}

#[derive(Serialize, Deserialize, Debug)]
pub enum SecurityType {
    None = 1,
    Wep = 2,
    Wpa = 3,
    Wpa2 = 4,
    Wpa3 = 5,
}

#[derive(Serialize, Deserialize, Debug)]
pub enum DisconnectStatus {
    TimedOut = 1,
    CredentialsFailed = 2,
    ConnectionStopped = 3,
    ConnectionFailed = 4,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct NetworkIdentifier {
    /// Network name, often used by users to choose between networks in the UI.
    pub ssid: Vec<u8>,
    /// Protection type (or not) for the network.
    pub type_: SecurityType,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct NetworkState {
    /// Network id for the current connection (or attempt).
    pub id: Option<NetworkIdentifier>,
    /// Current state for the connection.
    pub state: Option<ConnectionState>,
    /// Extra information for debugging or Settings display
    pub status: Option<DisconnectStatus>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct ClientStateSummary {
    /// State indicating whether wlan will attempt to connect to networks or not.
    pub state: Option<WlanClientState>,

    /// Active connections, connection attempts or failed connections.
    pub networks: Option<Vec<NetworkState>>,
}

#[derive(Serialize, Deserialize)]
#[serde(remote = "fidl_sme::Protection")]
pub(crate) enum ProtectionDef {
    Unknown = 0,
    Open = 1,
    Wep = 2,
    Wpa1 = 3,
    Wpa1Wpa2PersonalTkipOnly = 4,
    Wpa2PersonalTkipOnly = 5,
    Wpa1Wpa2Personal = 6,
    Wpa2Personal = 7,
    Wpa2Wpa3Personal = 8,
    Wpa3Personal = 9,
    Wpa2Enterprise = 10,
    Wpa3Enterprise = 11,
}

// The following definitions derive Serialize and Deserialize for remote types, i.e. types
// defined in other crates. See https://serde.rs/remote-derive.html for more info.
#[derive(Serialize, Deserialize)]
#[serde(remote = "fidl_common::ChannelBandwidth")]
#[repr(u32)]
pub(crate) enum ChannelBandwidthDef {
    Cbw20 = 0,
    Cbw40 = 1,
    Cbw40Below = 2,
    Cbw80 = 3,
    Cbw160 = 4,
    Cbw80P80 = 5,
}

#[derive(Serialize, Deserialize)]
#[serde(remote = "fidl_common::WlanChannel")]
pub(crate) struct WlanChannelDef {
    pub primary: u8,
    #[serde(with = "ChannelBandwidthDef")]
    pub cbw: fidl_common::ChannelBandwidth,
    pub secondary80: u8,
}

// Since fidl_internal::BssType is a flexible enum, the generated Rust enum is declared
// non-exhaustive, requiring a wildcard arm on all match expressions.
//
// However, deriving Serialize/Deserialize on a non-exhaustive enum generates code that have match
// expressions that do not have wildcard arms. As a workaround, define the types used by serde
// separately from the FIDL type and implement conversion functions between the two types.
#[derive(Serialize, Deserialize)]
pub(crate) enum BssTypeDef {
    Infrastructure = 1,
    Personal = 2,
    Independent = 3,
    Mesh = 4,
    Unknown = 255,
}

impl From<BssTypeDef> for fidl_internal::BssType {
    fn from(serde_type: BssTypeDef) -> Self {
        match serde_type {
            BssTypeDef::Infrastructure => Self::Infrastructure,
            BssTypeDef::Personal => Self::Personal,
            BssTypeDef::Independent => Self::Independent,
            BssTypeDef::Mesh => Self::Mesh,
            BssTypeDef::Unknown => Self::unknown(),
        }
    }
}

impl From<fidl_internal::BssType> for BssTypeDef {
    fn from(fidl_type: fidl_internal::BssType) -> Self {
        match fidl_type {
            fidl_internal::BssType::Infrastructure => Self::Infrastructure,
            fidl_internal::BssType::Personal => Self::Personal,
            fidl_internal::BssType::Independent => Self::Independent,
            fidl_internal::BssType::Mesh => Self::Mesh,
            _ => Self::Unknown,
        }
    }
}

#[derive(Serialize, Deserialize)]
pub(crate) struct BssDescriptionDef {
    pub bssid: [u8; 6],
    pub bss_type: BssTypeDef,
    pub beacon_period: u16,
    pub capability_info: u16,
    pub ies: Vec<u8>,
    #[serde(with = "WlanChannelDef")]
    pub channel: fidl_common::WlanChannel,
    pub rssi_dbm: i8,
    pub snr_db: i8,
}

impl From<BssDescriptionDef> for fidl_internal::BssDescription {
    fn from(serde_type: BssDescriptionDef) -> Self {
        Self {
            bssid: serde_type.bssid,
            bss_type: serde_type.bss_type.into(),
            beacon_period: serde_type.beacon_period,
            capability_info: serde_type.capability_info,
            ies: serde_type.ies,
            channel: serde_type.channel,
            rssi_dbm: serde_type.rssi_dbm,
            snr_db: serde_type.snr_db,
        }
    }
}

impl From<fidl_internal::BssDescription> for BssDescriptionDef {
    fn from(fidl_type: fidl_internal::BssDescription) -> Self {
        Self {
            bssid: fidl_type.bssid,
            bss_type: fidl_type.bss_type.into(),
            beacon_period: fidl_type.beacon_period,
            capability_info: fidl_type.capability_info,
            ies: fidl_type.ies,
            channel: fidl_type.channel,
            rssi_dbm: fidl_type.rssi_dbm,
            snr_db: fidl_type.snr_db,
        }
    }
}

#[derive(Serialize)]
#[serde(remote = "fidl_sme::ServingApInfo")]
pub(crate) struct ServingApInfoDef {
    pub bssid: [u8; 6],
    pub ssid: Vec<u8>,
    pub rssi_dbm: i8,
    pub snr_db: i8,
    #[serde(with = "WlanChannelDef")]
    pub channel: fidl_common::WlanChannel,
    #[serde(with = "ProtectionDef")]
    pub protection: fidl_sme::Protection,
}

#[derive(Serialize)]
#[serde(remote = "fidl_sme::Empty")]
pub(crate) struct SmeEmptyDef;

#[derive(Serialize)]
#[serde(remote = "fidl_sme::ClientStatusResponse")]
pub(crate) enum ClientStatusResponseDef {
    Connected(#[serde(with = "ServingApInfoDef")] fidl_sme::ServingApInfo),
    Connecting(Vec<u8>),
    #[serde(with = "SmeEmptyDef")]
    Idle(fidl_sme::Empty),
}

#[derive(Serialize)]
pub(crate) struct ClientStatusResponseWrapper(
    #[serde(with = "ClientStatusResponseDef")] pub fidl_sme::ClientStatusResponse,
);

#[derive(Serialize)]
pub(crate) enum WlanMacRoleDef {
    Client = 1,
    Ap = 2,
    Mesh = 3,
    Unknown = 255,
}

impl From<fidl_fuchsia_wlan_common::WlanMacRole> for WlanMacRoleDef {
    fn from(fidl_type: fidl_fuchsia_wlan_common::WlanMacRole) -> Self {
        match fidl_type {
            fidl_fuchsia_wlan_common::WlanMacRole::Client => Self::Client,
            fidl_fuchsia_wlan_common::WlanMacRole::Ap => Self::Ap,
            fidl_fuchsia_wlan_common::WlanMacRole::Mesh => Self::Mesh,
            fidl_fuchsia_wlan_common::WlanMacRoleUnknown!() => Self::Unknown,
        }
    }
}

#[derive(Serialize)]
pub(crate) struct QueryIfaceResponseDef {
    pub role: WlanMacRoleDef,
    pub id: u16,
    pub phy_id: u16,
    pub phy_assigned_id: u16,
    pub sta_addr: [u8; 6],
}

#[derive(Serialize)]
pub(crate) struct QueryIfaceResponseWrapper(pub QueryIfaceResponseDef);

impl From<fidl_fuchsia_wlan_device_service::QueryIfaceResponse> for QueryIfaceResponseDef {
    fn from(resp: fidl_fuchsia_wlan_device_service::QueryIfaceResponse) -> QueryIfaceResponseDef {
        QueryIfaceResponseDef {
            role: resp.role.into(),
            id: resp.id,
            phy_id: resp.phy_id,
            phy_assigned_id: resp.phy_assigned_id,
            sta_addr: resp.sta_addr,
        }
    }
}
