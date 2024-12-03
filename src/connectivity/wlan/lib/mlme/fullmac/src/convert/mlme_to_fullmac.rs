// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// TODO(https://fxbug.dev/313442116): These are currently only used in tests, but will be
// used in production when the Banjo FFI is removed.
#![allow(dead_code)]
use anyhow::{bail, Result};
use {
    fidl_fuchsia_wlan_common as fidl_common, fidl_fuchsia_wlan_fullmac as fidl_fullmac,
    fidl_fuchsia_wlan_ieee80211 as fidl_ieee80211, fidl_fuchsia_wlan_mlme as fidl_mlme,
};

pub fn convert_scan_request(
    req: fidl_mlme::ScanRequest,
) -> Result<fidl_fullmac::WlanFullmacImplStartScanRequest> {
    let ssids = req
        .ssid_list
        .into_iter()
        .map(|ssid| convert_ssid(&ssid))
        .collect::<Result<Vec<fidl_ieee80211::CSsid>>>()?;

    Ok(fidl_fullmac::WlanFullmacImplStartScanRequest {
        txn_id: Some(req.txn_id),
        scan_type: Some(match req.scan_type {
            fidl_mlme::ScanTypes::Active => fidl_fullmac::WlanScanType::Active,
            fidl_mlme::ScanTypes::Passive => fidl_fullmac::WlanScanType::Passive,
        }),

        // TODO(https://fxbug.dev/301104836): Consider using None instead of Some(vec![]) for empty
        // vectors.
        channels: Some(req.channel_list),
        ssids: Some(ssids),
        min_channel_time: Some(req.min_channel_time),
        max_channel_time: Some(req.max_channel_time),
        ..Default::default()
    })
}

pub fn convert_connect_request(
    req: fidl_mlme::ConnectRequest,
) -> fidl_fullmac::WlanFullmacImplConnectRequest {
    fidl_fullmac::WlanFullmacImplConnectRequest {
        selected_bss: Some(req.selected_bss),
        connect_failure_timeout: Some(req.connect_failure_timeout),
        auth_type: Some(convert_auth_type(req.auth_type)),
        sae_password: Some(req.sae_password),

        wep_key: req.wep_key.map(|key| convert_set_key_descriptor(&key)),
        security_ie: Some(req.security_ie),
        ..Default::default()
    }
}

pub fn convert_reconnect_request(
    req: fidl_mlme::ReconnectRequest,
) -> fidl_fullmac::WlanFullmacImplReconnectRequest {
    fidl_fullmac::WlanFullmacImplReconnectRequest {
        peer_sta_address: Some(req.peer_sta_address),
        ..Default::default()
    }
}

pub fn convert_roam_request(
    req: fidl_mlme::RoamRequest,
) -> fidl_fullmac::WlanFullmacImplRoamRequest {
    fidl_fullmac::WlanFullmacImplRoamRequest {
        selected_bss: Some(req.selected_bss),
        ..Default::default()
    }
}
pub fn convert_authenticate_response(
    resp: fidl_mlme::AuthenticateResponse,
) -> fidl_fullmac::WlanFullmacImplAuthRespRequest {
    fidl_fullmac::WlanFullmacImplAuthRespRequest {
        peer_sta_address: Some(resp.peer_sta_address),
        result_code: Some(match resp.result_code {
            fidl_mlme::AuthenticateResultCode::Success => fidl_fullmac::WlanAuthResult::Success,
            fidl_mlme::AuthenticateResultCode::Refused => fidl_fullmac::WlanAuthResult::Refused,
            fidl_mlme::AuthenticateResultCode::AntiCloggingTokenRequired => {
                fidl_fullmac::WlanAuthResult::AntiCloggingTokenRequired
            }
            fidl_mlme::AuthenticateResultCode::FiniteCyclicGroupNotSupported => {
                fidl_fullmac::WlanAuthResult::FiniteCyclicGroupNotSupported
            }
            fidl_mlme::AuthenticateResultCode::AuthenticationRejected => {
                fidl_fullmac::WlanAuthResult::Rejected
            }
            fidl_mlme::AuthenticateResultCode::AuthFailureTimeout => {
                fidl_fullmac::WlanAuthResult::FailureTimeout
            }
        }),
        ..Default::default()
    }
}

pub fn convert_deauthenticate_request(
    req: fidl_mlme::DeauthenticateRequest,
) -> fidl_fullmac::WlanFullmacImplDeauthRequest {
    fidl_fullmac::WlanFullmacImplDeauthRequest {
        peer_sta_address: Some(req.peer_sta_address),
        reason_code: Some(req.reason_code),
        ..Default::default()
    }
}

pub fn convert_associate_response(
    resp: fidl_mlme::AssociateResponse,
) -> fidl_fullmac::WlanFullmacImplAssocRespRequest {
    use fidl_fullmac::WlanAssocResult;
    fidl_fullmac::WlanFullmacImplAssocRespRequest {
        peer_sta_address: Some(resp.peer_sta_address),
        result_code: Some(match resp.result_code {
            fidl_mlme::AssociateResultCode::Success => WlanAssocResult::Success,
            fidl_mlme::AssociateResultCode::RefusedReasonUnspecified => {
                WlanAssocResult::RefusedReasonUnspecified
            }
            fidl_mlme::AssociateResultCode::RefusedNotAuthenticated => {
                WlanAssocResult::RefusedNotAuthenticated
            }
            fidl_mlme::AssociateResultCode::RefusedCapabilitiesMismatch => {
                WlanAssocResult::RefusedCapabilitiesMismatch
            }
            fidl_mlme::AssociateResultCode::RefusedExternalReason => {
                WlanAssocResult::RefusedExternalReason
            }
            fidl_mlme::AssociateResultCode::RefusedApOutOfMemory => {
                WlanAssocResult::RefusedApOutOfMemory
            }
            fidl_mlme::AssociateResultCode::RefusedBasicRatesMismatch => {
                WlanAssocResult::RefusedBasicRatesMismatch
            }
            fidl_mlme::AssociateResultCode::RejectedEmergencyServicesNotSupported => {
                WlanAssocResult::RejectedEmergencyServicesNotSupported
            }
            fidl_mlme::AssociateResultCode::RefusedTemporarily => {
                WlanAssocResult::RefusedTemporarily
            }
        }),
        association_id: Some(resp.association_id),
        ..Default::default()
    }
}
pub fn convert_disassociate_request(
    req: fidl_mlme::DisassociateRequest,
) -> fidl_fullmac::WlanFullmacImplDisassocRequest {
    fidl_fullmac::WlanFullmacImplDisassocRequest {
        peer_sta_address: Some(req.peer_sta_address),
        reason_code: Some(req.reason_code),
        ..Default::default()
    }
}

pub fn convert_start_bss_request(
    req: fidl_mlme::StartRequest,
) -> Result<fidl_fullmac::WlanFullmacImplStartBssRequest> {
    if let Some(rsne) = &req.rsne {
        if rsne.len() > fidl_ieee80211::WLAN_IE_MAX_LEN as usize {
            bail!(
                "MLME RSNE length ({}) exceeds allowed maximum ({})",
                rsne.len(),
                fidl_ieee80211::WLAN_IE_BODY_MAX_LEN
            );
        }
    }
    Ok(fidl_fullmac::WlanFullmacImplStartBssRequest {
        ssid: Some(convert_ssid(&req.ssid[..])?),
        bss_type: Some(req.bss_type),
        beacon_period: Some(req.beacon_period as u32),
        dtim_period: Some(req.dtim_period as u32),
        channel: Some(req.channel),
        rsne: req.rsne,

        // TODO(https://fxbug.dev/301104836): Consider removing this field or using None instead of Some(vec![]).
        vendor_ie: Some(vec![]),
        ..Default::default()
    })
}

pub fn convert_stop_bss_request(
    req: fidl_mlme::StopRequest,
) -> Result<fidl_fullmac::WlanFullmacImplStopBssRequest> {
    Ok(fidl_fullmac::WlanFullmacImplStopBssRequest {
        ssid: Some(convert_ssid(&req.ssid[..])?),
        ..Default::default()
    })
}

// Note: this takes a reference since |req| will be used later to convert the response.
pub fn convert_set_keys_request(
    req: &fidl_mlme::SetKeysRequest,
) -> Result<fidl_fullmac::WlanFullmacImplSetKeysRequest> {
    const MAX_NUM_KEYS: usize = fidl_fullmac::WLAN_MAX_KEYLIST_SIZE as usize;
    if req.keylist.len() > MAX_NUM_KEYS {
        bail!(
            "SetKeysRequest keylist len {} exceeds allowed maximum {}",
            req.keylist.len(),
            MAX_NUM_KEYS
        );
    }
    let keylist: Vec<_> = req.keylist.iter().map(convert_set_key_descriptor).collect();

    Ok(fidl_fullmac::WlanFullmacImplSetKeysRequest { keylist: Some(keylist), ..Default::default() })
}

pub fn convert_eapol_request(
    req: fidl_mlme::EapolRequest,
) -> fidl_fullmac::WlanFullmacImplEapolTxRequest {
    fidl_fullmac::WlanFullmacImplEapolTxRequest {
        src_addr: Some(req.src_addr),
        dst_addr: Some(req.dst_addr),
        data: Some(req.data),
        ..Default::default()
    }
}

pub fn convert_sae_handshake_response(
    resp: fidl_mlme::SaeHandshakeResponse,
) -> fidl_fullmac::WlanFullmacImplSaeHandshakeRespRequest {
    fidl_fullmac::WlanFullmacImplSaeHandshakeRespRequest {
        peer_sta_address: Some(resp.peer_sta_address),
        status_code: Some(resp.status_code),
        ..Default::default()
    }
}

pub fn convert_sae_frame(frame: fidl_mlme::SaeFrame) -> fidl_fullmac::SaeFrame {
    fidl_fullmac::SaeFrame {
        peer_sta_address: Some(frame.peer_sta_address),
        status_code: Some(frame.status_code),
        seq_num: Some(frame.seq_num),
        sae_fields: Some(frame.sae_fields),
        ..Default::default()
    }
}

//
// Internal helper functions
//

fn convert_auth_type(mlme_auth: fidl_mlme::AuthenticationTypes) -> fidl_fullmac::WlanAuthType {
    match mlme_auth {
        fidl_mlme::AuthenticationTypes::OpenSystem => fidl_fullmac::WlanAuthType::OpenSystem,
        fidl_mlme::AuthenticationTypes::SharedKey => fidl_fullmac::WlanAuthType::SharedKey,
        fidl_mlme::AuthenticationTypes::FastBssTransition => {
            fidl_fullmac::WlanAuthType::FastBssTransition
        }
        fidl_mlme::AuthenticationTypes::Sae => fidl_fullmac::WlanAuthType::Sae,
    }
}
fn convert_set_key_descriptor(
    mlme_key: &fidl_mlme::SetKeyDescriptor,
) -> fidl_common::WlanKeyConfig {
    fidl_common::WlanKeyConfig {
        // TODO(https://fxbug.dev/301104836): This is always set to RxTx. Consider removing if it's
        // always the same value.
        protection: Some(fidl_common::WlanProtection::RxTx),
        cipher_oui: Some(mlme_key.cipher_suite_oui.clone()),
        cipher_type: Some(mlme_key.cipher_suite_type),
        key_type: Some(convert_key_type(mlme_key.key_type)),
        peer_addr: Some(mlme_key.address.clone()),
        key_idx: Some(mlme_key.key_id as u8),
        key: Some(mlme_key.key.clone()),
        rsc: Some(mlme_key.rsc),
        ..Default::default()
    }
}
fn convert_key_type(mlme_key_type: fidl_mlme::KeyType) -> fidl_common::WlanKeyType {
    match mlme_key_type {
        fidl_mlme::KeyType::Group => fidl_common::WlanKeyType::Group,
        fidl_mlme::KeyType::Pairwise => fidl_common::WlanKeyType::Pairwise,
        fidl_mlme::KeyType::PeerKey => fidl_common::WlanKeyType::Peer,
        fidl_mlme::KeyType::Igtk => fidl_common::WlanKeyType::Igtk,
    }
}

fn convert_delete_key_descriptor(
    descriptor: fidl_mlme::DeleteKeyDescriptor,
) -> fidl_fullmac::DeleteKeyDescriptor {
    fidl_fullmac::DeleteKeyDescriptor {
        key_id: Some(descriptor.key_id),
        key_type: Some(convert_key_type(descriptor.key_type)),
        address: Some(descriptor.address),
        ..Default::default()
    }
}

/// TODO(https://fxbug.dev/353733695): Remove this once CSsid is no longer needed.
fn convert_ssid(ssid: &[u8]) -> Result<fidl_ieee80211::CSsid> {
    if ssid.len() > fidl_ieee80211::MAX_SSID_BYTE_LEN as usize {
        bail!(
            "SSID length ({}) exceeds maximum size ({})",
            ssid.len(),
            fidl_ieee80211::MAX_SSID_BYTE_LEN
        );
    }
    let mut data = [0; fidl_ieee80211::MAX_SSID_BYTE_LEN as usize];
    data[..ssid.len() as usize].copy_from_slice(&ssid[..]);
    Ok(fidl_ieee80211::CSsid { len: ssid.len() as u8, data })
}

// TODO(https://fxbug.dev/301104836): Cleanup Fullmac FIDL API such that this is no longer needed.
fn dummy_delete_key_descriptor() -> fidl_fullmac::DeleteKeyDescriptor {
    fidl_fullmac::DeleteKeyDescriptor {
        key_id: Some(0),
        key_type: Some(fidl_common::WlanKeyType::Pairwise),
        address: Some([0; 6]),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_bss_description() -> fidl_common::BssDescription {
        fidl_common::BssDescription {
            bssid: [6, 5, 4, 3, 2, 1],
            bss_type: fidl_common::BssType::Infrastructure,
            beacon_period: 123u16,
            capability_info: 456u16,
            ies: vec![1, 2, 3, 4],
            channel: fidl_common::WlanChannel {
                primary: 112,
                cbw: fidl_common::ChannelBandwidth::Cbw20,
                secondary80: 45,
            },
            rssi_dbm: -41i8,
            snr_db: -90i8,
        }
    }

    fn fake_set_key_descriptor() -> fidl_mlme::SetKeyDescriptor {
        fidl_mlme::SetKeyDescriptor {
            key: vec![99, 100, 101, 102, 103, 14],
            key_id: 23,
            key_type: fidl_mlme::KeyType::Group,
            address: [4u8; 6],
            rsc: 123456,
            cipher_suite_oui: [77, 88, 99],
            cipher_suite_type: fidl_ieee80211::CipherSuiteType::Ccmp128,
        }
    }

    fn fake_delete_key_descriptor() -> fidl_mlme::DeleteKeyDescriptor {
        fidl_mlme::DeleteKeyDescriptor {
            key_id: 23,
            key_type: fidl_mlme::KeyType::Group,
            address: [3u8; 6],
        }
    }

    #[test]
    fn test_convert_scan_request_empty_vectors() {
        let mlme = fidl_mlme::ScanRequest {
            txn_id: 123,
            scan_type: fidl_mlme::ScanTypes::Passive,
            channel_list: vec![],
            ssid_list: vec![],
            probe_delay: 42,
            min_channel_time: 10,
            max_channel_time: 100,
        };

        assert_eq!(
            convert_scan_request(mlme.clone()).unwrap(),
            fidl_fullmac::WlanFullmacImplStartScanRequest {
                txn_id: Some(123),
                scan_type: Some(fidl_fullmac::WlanScanType::Passive),
                channels: Some(vec![]),
                ssids: Some(vec![]),
                min_channel_time: Some(10),
                max_channel_time: Some(100),
                ..Default::default()
            }
        );
    }

    #[test]
    fn test_convert_scan_request_ssid_too_long() {
        let mlme = fidl_mlme::ScanRequest {
            txn_id: 123,
            scan_type: fidl_mlme::ScanTypes::Passive,
            channel_list: vec![],
            ssid_list: vec![vec![123; 4], vec![42; fidl_ieee80211::MAX_SSID_BYTE_LEN as usize + 1]],
            probe_delay: 42,
            min_channel_time: 10,
            max_channel_time: 100,
        };

        assert!(convert_scan_request(mlme).is_err());
    }

    #[test]
    fn test_convert_connect_request_no_wep_key() {
        let mlme = fidl_mlme::ConnectRequest {
            selected_bss: fake_bss_description(),
            connect_failure_timeout: 60,
            auth_type: fidl_mlme::AuthenticationTypes::OpenSystem,
            sae_password: vec![10, 11, 12, 13, 14],
            wep_key: None,
            security_ie: vec![44, 55, 66],
        };

        assert_eq!(
            convert_connect_request(mlme.clone()),
            fidl_fullmac::WlanFullmacImplConnectRequest {
                selected_bss: Some(mlme.selected_bss.clone()),
                connect_failure_timeout: Some(60),
                auth_type: Some(fidl_fullmac::WlanAuthType::OpenSystem),
                sae_password: Some(vec![10, 11, 12, 13, 14]),
                security_ie: Some(vec![44, 55, 66]),
                ..Default::default()
            }
        );
    }

    #[test]
    fn test_convert_connect_request_empty_vectors() {
        let mlme = fidl_mlme::ConnectRequest {
            selected_bss: fake_bss_description(),
            connect_failure_timeout: 60,
            auth_type: fidl_mlme::AuthenticationTypes::OpenSystem,
            sae_password: vec![],
            wep_key: None,
            security_ie: vec![],
        };

        assert_eq!(
            convert_connect_request(mlme.clone()),
            fidl_fullmac::WlanFullmacImplConnectRequest {
                selected_bss: Some(mlme.selected_bss.clone()),
                connect_failure_timeout: Some(60),
                auth_type: Some(fidl_fullmac::WlanAuthType::OpenSystem),
                sae_password: Some(vec![]),
                security_ie: Some(vec![]),
                ..Default::default()
            }
        );
    }

    #[test]
    fn test_convert_start_bss_request_rsne_too_long() {
        let mlme = fidl_mlme::StartRequest {
            ssid: vec![1, 2, 3, 4],
            bss_type: fidl_common::BssType::Independent,
            beacon_period: 10000,
            dtim_period: 123,
            channel: 12,
            capability_info: 4321,
            rates: vec![10, 20, 30, 40],
            country: fidl_mlme::Country { alpha2: [1, 2], suffix: 45 },
            mesh_id: vec![6, 5, 6, 5],
            rsne: Some(vec![123; fidl_ieee80211::WLAN_IE_MAX_LEN as usize + 1]),
            phy: fidl_common::WlanPhyType::Ofdm,
            channel_bandwidth: fidl_common::ChannelBandwidth::Cbw20,
        };

        assert!(convert_start_bss_request(mlme).is_err());
    }

    #[test]
    fn test_convert_stop_bss_request_ssid_too_long() {
        let mlme = fidl_mlme::StopRequest {
            ssid: vec![42; fidl_ieee80211::MAX_SSID_BYTE_LEN as usize + 1],
        };
        assert!(convert_stop_bss_request(mlme).is_err());
    }

    #[test]
    fn test_convert_set_keys_request() {
        let mlme = fidl_mlme::SetKeysRequest { keylist: vec![fake_set_key_descriptor(); 2] };

        let fullmac = convert_set_keys_request(&mlme).unwrap();

        assert_eq!(fullmac.keylist.as_ref().unwrap().len(), 2);
        let keylist = fullmac.keylist.unwrap();
        for key in &keylist[0..2] {
            assert_eq!(key, &convert_set_key_descriptor(&fake_set_key_descriptor()));
        }
    }

    #[test]
    fn test_convert_set_keys_request_keylist_too_long() {
        let mlme = fidl_mlme::SetKeysRequest {
            keylist: vec![
                fake_set_key_descriptor();
                fidl_fullmac::WLAN_MAX_KEYLIST_SIZE as usize + 1
            ],
        };
        assert!(convert_set_keys_request(&mlme).is_err());
    }

    //
    // Helper function unit tests
    //

    #[test]
    fn test_convert_ssid() {
        let ssid = vec![1, 2, 3, 4, 5];
        let cssid = convert_ssid(&ssid).unwrap();

        assert_eq!(cssid.len as usize, ssid.len());
        for (i, byte) in ssid.into_iter().enumerate() {
            assert_eq!(byte, cssid.data[i]);
        }

        // The rest of cssid.data should be zerod out.
        for byte in &cssid.data[cssid.len as usize..] {
            assert_eq!(byte, &0u8);
        }
    }

    #[test]
    fn test_convert_ssid_length_too_long() {
        let ssid = vec![42; fidl_ieee80211::MAX_SSID_BYTE_LEN as usize + 1];
        assert!(convert_ssid(&ssid).is_err());
    }

    #[test]
    fn test_convert_set_key_descriptor() {
        let mlme = fidl_mlme::SetKeyDescriptor {
            key: vec![1, 2, 3],
            key_id: 123,
            key_type: fidl_mlme::KeyType::Group,
            address: [3; 6],
            rsc: 1234567,
            cipher_suite_oui: [4, 3, 2],
            cipher_suite_type: fidl_ieee80211::CipherSuiteType::Ccmp128,
        };

        assert_eq!(
            convert_set_key_descriptor(&mlme),
            fidl_common::WlanKeyConfig {
                protection: Some(fidl_common::WlanProtection::RxTx),
                cipher_oui: Some([4, 3, 2]),
                cipher_type: Some(fidl_ieee80211::CipherSuiteType::Ccmp128),
                key_type: Some(fidl_common::WlanKeyType::Group),
                peer_addr: Some([3; 6]),
                key_idx: Some(123),
                key: Some(vec![1, 2, 3]),
                rsc: Some(1234567),
                ..Default::default()
            }
        );
    }

    // In MLME FIDL, key_id is a u16 but in Fullmac it's a u8.
    // This tests the behavior when MLME uses a value that doesn't fit into a u8.
    #[test]
    fn test_convert_set_key_descriptor_truncates_key_id_if_too_large() {
        let mlme = fidl_mlme::SetKeyDescriptor {
            key: vec![1, 2, 3],
            key_id: 0xAABB,
            key_type: fidl_mlme::KeyType::Group,
            address: [3; 6],
            rsc: 1234567,
            cipher_suite_oui: [4, 3, 2],
            cipher_suite_type: fidl_ieee80211::CipherSuiteType::Ccmp128,
        };

        let fullmac = convert_set_key_descriptor(&mlme);

        // key_idx becomes the bottom 8 bits of mlme.key_id
        assert_eq!(fullmac.key_idx.unwrap(), 0xBBu8);
    }
}
