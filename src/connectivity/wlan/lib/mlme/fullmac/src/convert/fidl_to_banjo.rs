// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::{self, bail},
    banjo_fuchsia_hardware_wlan_fullmac as banjo_wlan_fullmac,
    banjo_fuchsia_wlan_common as banjo_wlan_common,
    banjo_fuchsia_wlan_ieee80211 as banjo_wlan_ieee80211,
    banjo_fuchsia_wlan_internal as banjo_wlan_internal, fidl_fuchsia_wlan_common as fidl_common,
    fidl_fuchsia_wlan_internal as fidl_internal, fidl_fuchsia_wlan_mlme as fidl_mlme,
    static_assertions::assert_eq_size_val,
    std::cmp::min,
    tracing::warn,
};

/// Generic wrapper for a banjo return type, with a lifetime parameter. The
/// intention is to allow a way to specify lifetime on the return type of the conversion
/// functions below. Because the the banjo return type may contain a raw pointer to the
/// original FIDL type, having the lifetime parameter allow us to tie the lifetime of the
/// two types together to reduce the chance of the banjo type being used when its pointer
/// to a FIDL type's field is no longer valid.
pub struct BanjoReturnType<'a, T>(T, std::marker::PhantomData<&'a T>);

impl<'a, T> BanjoReturnType<'a, T> {
    fn new(obj: T) -> Self {
        Self(obj, std::marker::PhantomData)
    }
}

impl<'a, T> std::ops::Deref for BanjoReturnType<'a, T> {
    type Target = T;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<'a, T> std::ops::DerefMut for BanjoReturnType<'a, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

fn convert_bss_description(
    bss: &fidl_internal::BssDescription,
) -> BanjoReturnType<'_, banjo_wlan_internal::BssDescription> {
    BanjoReturnType::new(banjo_wlan_internal::BssDescription {
        bssid: bss.bssid,
        bss_type: banjo_wlan_common::BssType(bss.bss_type.into_primitive()),
        beacon_period: bss.beacon_period,
        capability_info: bss.capability_info,
        ies_list: bss.ies.as_ptr(),
        ies_count: bss.ies.len(),
        channel: convert_channel(&bss.channel),
        rssi_dbm: bss.rssi_dbm,
        snr_db: bss.snr_db,
    })
}

fn convert_channel(channel: &fidl_common::WlanChannel) -> banjo_wlan_common::WlanChannel {
    banjo_wlan_common::WlanChannel {
        primary: channel.primary,
        cbw: banjo_wlan_common::ChannelBandwidth(channel.cbw.into_primitive()),
        secondary80: channel.secondary80,
    }
}

fn convert_key_type(key_type: &fidl_mlme::KeyType) -> banjo_wlan_common::WlanKeyType {
    match key_type {
        fidl_mlme::KeyType::Group => banjo_wlan_common::WlanKeyType::GROUP,
        fidl_mlme::KeyType::Pairwise => banjo_wlan_common::WlanKeyType::PAIRWISE,
        fidl_mlme::KeyType::PeerKey => banjo_wlan_common::WlanKeyType::PEER,
        fidl_mlme::KeyType::Igtk => banjo_wlan_common::WlanKeyType::IGTK,
    }
}

fn convert_set_key_descriptor(
    descriptor: &fidl_mlme::SetKeyDescriptor,
) -> BanjoReturnType<'_, banjo_wlan_fullmac::SetKeyDescriptor> {
    BanjoReturnType::new(banjo_wlan_fullmac::SetKeyDescriptor {
        key_list: descriptor.key.as_ptr(),
        key_count: descriptor.key.len(),
        key_id: descriptor.key_id,
        key_type: convert_key_type(&descriptor.key_type),
        address: descriptor.address,
        rsc: descriptor.rsc,
        cipher_suite_oui: descriptor.cipher_suite_oui,
        cipher_suite_type: descriptor.cipher_suite_type,
    })
}

fn dummy_set_key_descriptor() -> banjo_wlan_fullmac::SetKeyDescriptor {
    banjo_wlan_fullmac::SetKeyDescriptor {
        key_list: std::ptr::null(),
        key_count: 0,
        key_id: 0,
        key_type: banjo_wlan_common::WlanKeyType(0),
        address: [0; 6],
        rsc: 0,
        cipher_suite_oui: [0; 3],
        cipher_suite_type: 0,
    }
}

fn convert_delete_key_descriptor(
    descriptor: &fidl_mlme::DeleteKeyDescriptor,
) -> banjo_wlan_fullmac::DeleteKeyDescriptor {
    banjo_wlan_fullmac::DeleteKeyDescriptor {
        key_id: descriptor.key_id,
        key_type: convert_key_type(&descriptor.key_type),
        address: descriptor.address,
    }
}

fn dummy_delete_key_descriptor() -> banjo_wlan_fullmac::DeleteKeyDescriptor {
    banjo_wlan_fullmac::DeleteKeyDescriptor {
        key_id: 0,
        key_type: banjo_wlan_common::WlanKeyType(0),
        address: [0; 6],
    }
}

fn convert_rsne(
    rsne: &Option<Vec<u8>>,
) -> ([u8; banjo_wlan_ieee80211::WLAN_IE_BODY_MAX_LEN as usize], u64) {
    let mut rsne_len = 0;
    let mut rsne_copy = [0; banjo_wlan_ieee80211::WLAN_IE_BODY_MAX_LEN as usize];
    if let Some(rsne) = rsne {
        if rsne.len() > banjo_wlan_ieee80211::WLAN_IE_BODY_MAX_LEN as usize {
            warn!(
                "Truncating rsne len from {} to {}",
                rsne.len(),
                banjo_wlan_ieee80211::WLAN_IE_BODY_MAX_LEN
            );
        }
        rsne_len = min(rsne.len(), banjo_wlan_ieee80211::WLAN_IE_BODY_MAX_LEN as usize);
        rsne_copy[..rsne_len].copy_from_slice(&rsne[..rsne_len]);
    }
    (rsne_copy, rsne_len as u64)
}

pub fn convert_ssid(ssid: &[u8]) -> banjo_wlan_ieee80211::CSsid {
    assert_eq_size_val!(banjo_wlan_ieee80211::MAX_SSID_BYTE_LEN, 0u8);
    if ssid.len() > banjo_wlan_ieee80211::MAX_SSID_BYTE_LEN as usize {
        // This shouldn't happen because we always validate SSID length elsewhere beforehand,
        // but to be on the safe side, truncate the SSID if it exceeds max length.
        warn!(
            "Truncating SSID len from {} to {}",
            ssid.len(),
            banjo_wlan_ieee80211::MAX_SSID_BYTE_LEN as usize
        );
    }
    // Safe to cast `ssid.len()` to u8 since it's not larger than
    // banjo_wlan_ieee80211::MAX_SSID_BYTE_LEN, which is a u8 type
    let len = min(ssid.len() as u8, banjo_wlan_ieee80211::MAX_SSID_BYTE_LEN);
    let mut data = [0; banjo_wlan_ieee80211::MAX_SSID_BYTE_LEN as usize];
    data[..len as usize].copy_from_slice(&ssid[..len as usize]);
    banjo_wlan_ieee80211::CSsid { len, data }
}

pub fn convert_scan_request<'a>(
    req: &'a fidl_mlme::ScanRequest,
    ssid_list_copy: &'a [banjo_wlan_ieee80211::CSsid],
) -> BanjoReturnType<'a, banjo_wlan_fullmac::WlanFullmacScanReq> {
    BanjoReturnType::new(banjo_wlan_fullmac::WlanFullmacScanReq {
        txn_id: req.txn_id,
        scan_type: match req.scan_type {
            fidl_mlme::ScanTypes::Active => banjo_wlan_fullmac::WlanScanType::ACTIVE,
            fidl_mlme::ScanTypes::Passive => banjo_wlan_fullmac::WlanScanType::PASSIVE,
        },
        channels_list: req.channel_list.as_ptr(),
        channels_count: req.channel_list.len(),
        ssids_list: ssid_list_copy.as_ptr(),
        ssids_count: ssid_list_copy.len(),
        min_channel_time: req.min_channel_time,
        max_channel_time: req.max_channel_time,
    })
}

pub fn convert_connect_request(
    req: &fidl_mlme::ConnectRequest,
) -> BanjoReturnType<'_, banjo_wlan_fullmac::WlanFullmacConnectReq> {
    use banjo_wlan_fullmac::WlanAuthType;
    BanjoReturnType::new(banjo_wlan_fullmac::WlanFullmacConnectReq {
        selected_bss: convert_bss_description(&req.selected_bss).0,
        connect_failure_timeout: req.connect_failure_timeout,
        auth_type: match req.auth_type {
            fidl_mlme::AuthenticationTypes::OpenSystem => WlanAuthType::OPEN_SYSTEM,
            fidl_mlme::AuthenticationTypes::SharedKey => WlanAuthType::SHARED_KEY,
            fidl_mlme::AuthenticationTypes::FastBssTransition => WlanAuthType::FAST_BSS_TRANSITION,
            fidl_mlme::AuthenticationTypes::Sae => WlanAuthType::SAE,
        },
        sae_password_list: req.sae_password.as_ptr(),
        sae_password_count: req.sae_password.len(),
        wep_key: match &req.wep_key {
            Some(wep_key) => convert_set_key_descriptor(&*wep_key).0,
            None => dummy_set_key_descriptor(),
        },
        security_ie_list: req.security_ie.as_ptr(),
        security_ie_count: req.security_ie.len(),
    })
}

pub fn convert_reconnect_request(
    req: &fidl_mlme::ReconnectRequest,
) -> banjo_wlan_fullmac::WlanFullmacReconnectReq {
    banjo_wlan_fullmac::WlanFullmacReconnectReq { peer_sta_address: req.peer_sta_address }
}

pub fn convert_authenticate_response(
    resp: &fidl_mlme::AuthenticateResponse,
) -> banjo_wlan_fullmac::WlanFullmacAuthResp {
    use banjo_wlan_fullmac::WlanAuthResult;
    banjo_wlan_fullmac::WlanFullmacAuthResp {
        peer_sta_address: resp.peer_sta_address,
        result_code: match resp.result_code {
            fidl_mlme::AuthenticateResultCode::Success => WlanAuthResult::SUCCESS,
            fidl_mlme::AuthenticateResultCode::Refused => WlanAuthResult::REFUSED,
            fidl_mlme::AuthenticateResultCode::AntiCloggingTokenRequired => {
                WlanAuthResult::ANTI_CLOGGING_TOKEN_REQUIRED
            }
            fidl_mlme::AuthenticateResultCode::FiniteCyclicGroupNotSupported => {
                WlanAuthResult::FINITE_CYCLIC_GROUP_NOT_SUPPORTED
            }
            fidl_mlme::AuthenticateResultCode::AuthenticationRejected => WlanAuthResult::REJECTED,
            fidl_mlme::AuthenticateResultCode::AuthFailureTimeout => {
                WlanAuthResult::FAILURE_TIMEOUT
            }
        },
    }
}

pub fn convert_deauthenticate_request(
    req: &fidl_mlme::DeauthenticateRequest,
) -> banjo_wlan_fullmac::WlanFullmacDeauthReq {
    banjo_wlan_fullmac::WlanFullmacDeauthReq {
        peer_sta_address: req.peer_sta_address,
        reason_code: banjo_wlan_ieee80211::ReasonCode(req.reason_code.into_primitive()),
    }
}

pub fn convert_associate_response(
    resp: &fidl_mlme::AssociateResponse,
) -> banjo_wlan_fullmac::WlanFullmacAssocResp {
    use banjo_wlan_fullmac::WlanAssocResult;
    banjo_wlan_fullmac::WlanFullmacAssocResp {
        peer_sta_address: resp.peer_sta_address,
        result_code: match resp.result_code {
            fidl_mlme::AssociateResultCode::Success => WlanAssocResult::SUCCESS,
            fidl_mlme::AssociateResultCode::RefusedReasonUnspecified => {
                WlanAssocResult::REFUSED_REASON_UNSPECIFIED
            }
            fidl_mlme::AssociateResultCode::RefusedNotAuthenticated => {
                WlanAssocResult::REFUSED_NOT_AUTHENTICATED
            }
            fidl_mlme::AssociateResultCode::RefusedCapabilitiesMismatch => {
                WlanAssocResult::REFUSED_CAPABILITIES_MISMATCH
            }
            fidl_mlme::AssociateResultCode::RefusedExternalReason => {
                WlanAssocResult::REFUSED_EXTERNAL_REASON
            }
            fidl_mlme::AssociateResultCode::RefusedApOutOfMemory => {
                WlanAssocResult::REFUSED_AP_OUT_OF_MEMORY
            }
            fidl_mlme::AssociateResultCode::RefusedBasicRatesMismatch => {
                WlanAssocResult::REFUSED_BASIC_RATES_MISMATCH
            }
            fidl_mlme::AssociateResultCode::RejectedEmergencyServicesNotSupported => {
                WlanAssocResult::REJECTED_EMERGENCY_SERVICES_NOT_SUPPORTED
            }
            fidl_mlme::AssociateResultCode::RefusedTemporarily => {
                WlanAssocResult::REFUSED_TEMPORARILY
            }
        },
        association_id: resp.association_id,
    }
}

pub fn convert_disassociate_request(
    req: &fidl_mlme::DisassociateRequest,
) -> banjo_wlan_fullmac::WlanFullmacDisassocReq {
    banjo_wlan_fullmac::WlanFullmacDisassocReq {
        peer_sta_address: req.peer_sta_address,
        reason_code: banjo_wlan_ieee80211::ReasonCode(req.reason_code.into_primitive()),
    }
}

pub fn convert_reset_request(
    req: &fidl_mlme::ResetRequest,
) -> banjo_wlan_fullmac::WlanFullmacResetReq {
    banjo_wlan_fullmac::WlanFullmacResetReq {
        sta_address: req.sta_address,
        set_default_mib: req.set_default_mib,
    }
}

pub fn convert_start_request(
    req: &fidl_mlme::StartRequest,
) -> banjo_wlan_fullmac::WlanFullmacStartReq {
    let (rsne, rsne_len) = convert_rsne(&req.rsne);
    banjo_wlan_fullmac::WlanFullmacStartReq {
        ssid: convert_ssid(&req.ssid[..]),
        bss_type: banjo_wlan_common::BssType(req.bss_type.into_primitive()),
        beacon_period: req.beacon_period as u32,
        dtim_period: req.dtim_period as u32,
        channel: req.channel,
        rsne_len,
        rsne,
        vendor_ie_len: 0,
        vendor_ie: [0; banjo_wlan_fullmac::WLAN_VIE_MAX_LEN as usize],
    }
}

pub fn convert_stop_request(
    req: &fidl_mlme::StopRequest,
) -> banjo_wlan_fullmac::WlanFullmacStopReq {
    banjo_wlan_fullmac::WlanFullmacStopReq { ssid: convert_ssid(&req.ssid[..]) }
}

pub fn convert_set_keys_request(
    req: &fidl_mlme::SetKeysRequest,
) -> Result<BanjoReturnType<'_, banjo_wlan_fullmac::WlanFullmacSetKeysReq>, anyhow::Error> {
    const MAX_NUM_KEYS: u64 = banjo_wlan_fullmac::WLAN_MAX_KEYLIST_SIZE as u64;
    if req.keylist.len() > MAX_NUM_KEYS as usize {
        bail!("keylist len {} higher than max {}", req.keylist.len(), MAX_NUM_KEYS);
    }
    let num_keys = min(req.keylist.len(), MAX_NUM_KEYS as usize);
    let mut keylist = [dummy_set_key_descriptor(); MAX_NUM_KEYS as usize];
    for i in 0..num_keys {
        keylist[i] = convert_set_key_descriptor(&req.keylist[i]).0;
    }

    Ok(BanjoReturnType::new(banjo_wlan_fullmac::WlanFullmacSetKeysReq {
        num_keys: num_keys as u64,
        keylist,
    }))
}

pub fn convert_delete_keys_request(
    req: &fidl_mlme::DeleteKeysRequest,
) -> banjo_wlan_fullmac::WlanFullmacDelKeysReq {
    const MAX_NUM_KEYS: u64 = 4;
    if req.keylist.len() > MAX_NUM_KEYS as usize {
        warn!("Truncating keylist len from {} to {}", req.keylist.len(), MAX_NUM_KEYS);
    }
    let num_keys = min(req.keylist.len(), MAX_NUM_KEYS as usize);
    let mut keylist = [dummy_delete_key_descriptor(); MAX_NUM_KEYS as usize];
    for i in 0..num_keys {
        keylist[i] = convert_delete_key_descriptor(&req.keylist[i]);
    }

    banjo_wlan_fullmac::WlanFullmacDelKeysReq { num_keys: num_keys as u64, keylist }
}

pub fn convert_eapol_request(
    req: &fidl_mlme::EapolRequest,
) -> BanjoReturnType<'_, banjo_wlan_fullmac::WlanFullmacEapolReq> {
    BanjoReturnType::new(banjo_wlan_fullmac::WlanFullmacEapolReq {
        src_addr: req.src_addr,
        dst_addr: req.dst_addr,
        data_list: req.data.as_ptr(),
        data_count: req.data.len(),
    })
}

pub fn convert_sae_handshake_response(
    resp: &fidl_mlme::SaeHandshakeResponse,
) -> banjo_wlan_fullmac::WlanFullmacSaeHandshakeResp {
    banjo_wlan_fullmac::WlanFullmacSaeHandshakeResp {
        peer_sta_address: resp.peer_sta_address,
        status_code: banjo_wlan_ieee80211::StatusCode(resp.status_code.into_primitive()),
    }
}

pub fn convert_sae_frame(
    frame: &fidl_mlme::SaeFrame,
) -> BanjoReturnType<'_, banjo_wlan_fullmac::WlanFullmacSaeFrame> {
    BanjoReturnType::new(banjo_wlan_fullmac::WlanFullmacSaeFrame {
        peer_sta_address: frame.peer_sta_address,
        status_code: banjo_wlan_ieee80211::StatusCode(frame.status_code.into_primitive()),
        seq_num: frame.seq_num,
        sae_fields_list: frame.sae_fields.as_ptr(),
        sae_fields_count: frame.sae_fields.len(),
    })
}

// No unit tests in this file because the conversions are already verified in the unit
// tests for the whole library.
