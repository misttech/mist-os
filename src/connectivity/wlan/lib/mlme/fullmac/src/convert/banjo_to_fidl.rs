// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::slice;
use tracing::{error, warn};
use {
    banjo_fuchsia_wlan_common as banjo_wlan_common,
    banjo_fuchsia_wlan_fullmac as banjo_wlan_fullmac,
    banjo_fuchsia_wlan_ieee80211 as banjo_wlan_ieee80211,
    banjo_fuchsia_wlan_internal as banjo_wlan_internal, fidl_fuchsia_wlan_common as fidl_common,
    fidl_fuchsia_wlan_ieee80211 as fidl_ieee80211, fidl_fuchsia_wlan_internal as fidl_internal,
    fidl_fuchsia_wlan_mlme as fidl_mlme,
};

pub fn unsafe_slice_to_vec<T: Clone>(data: *const T, len: usize) -> Vec<T> {
    if data.is_null() || len == 0 {
        vec![]
    } else {
        unsafe { slice::from_raw_parts(data, len) }.to_vec()
    }
}

pub fn convert_bss_description(
    bss: banjo_wlan_internal::BssDescription,
) -> fidl_internal::BssDescription {
    fidl_internal::BssDescription {
        bssid: bss.bssid,
        bss_type: convert_bss_type(bss.bss_type),
        beacon_period: bss.beacon_period,
        capability_info: bss.capability_info,
        ies: unsafe_slice_to_vec(bss.ies_list, bss.ies_count),
        channel: convert_channel(bss.channel),
        rssi_dbm: bss.rssi_dbm,
        snr_db: bss.snr_db,
    }
}

pub fn convert_bss_type(bss_type: banjo_wlan_common::BssType) -> fidl_common::BssType {
    match fidl_common::BssType::from_primitive(bss_type.0) {
        Some(bss_type) => bss_type,
        None => {
            warn!("Invalid BSS type {}, defaulting to BssType::Unknown", bss_type.0);
            fidl_common::BssType::Unknown
        }
    }
}

pub fn convert_channel(channel: banjo_wlan_common::WlanChannel) -> fidl_common::WlanChannel {
    fidl_common::WlanChannel {
        primary: channel.primary,
        cbw: match fidl_common::ChannelBandwidth::from_primitive(channel.cbw.0) {
            Some(cbw) => cbw,
            None => {
                warn!(
                    "Invalid channel bandwidth {}, defaulting to ChannelBandwidth::Cbw20",
                    channel.cbw.0
                );
                fidl_common::ChannelBandwidth::Cbw20
            }
        },
        secondary80: channel.secondary80,
    }
}

fn convert_status_code(code: banjo_wlan_ieee80211::StatusCode) -> fidl_ieee80211::StatusCode {
    match fidl_ieee80211::StatusCode::from_primitive(code.0) {
        Some(code) => code,
        None => {
            warn!(
                "Invalid status code {}, defaulting to StatusCode::RefusedReasonUnspecified",
                code.0
            );
            fidl_ieee80211::StatusCode::RefusedReasonUnspecified
        }
    }
}

fn convert_reason_code(code: banjo_wlan_ieee80211::ReasonCode) -> fidl_ieee80211::ReasonCode {
    match fidl_ieee80211::ReasonCode::from_primitive(code.0) {
        Some(code) => code,
        None => {
            warn!("Invalid reason code {}, defaulting to ReasonCode::UnspecifiedReason", code.0);
            fidl_ieee80211::ReasonCode::UnspecifiedReason
        }
    }
}

pub fn convert_scan_result(
    result: banjo_wlan_fullmac::WlanFullmacScanResult,
) -> fidl_mlme::ScanResult {
    fidl_mlme::ScanResult {
        txn_id: result.txn_id,
        timestamp_nanos: result.timestamp_nanos,
        bss: convert_bss_description(result.bss),
    }
}

pub fn convert_scan_end(end: banjo_wlan_fullmac::WlanFullmacScanEnd) -> fidl_mlme::ScanEnd {
    use banjo_wlan_fullmac::WlanScanResult;
    fidl_mlme::ScanEnd {
        txn_id: end.txn_id,
        code: match end.code {
            WlanScanResult::SUCCESS => fidl_mlme::ScanResultCode::Success,
            WlanScanResult::NOT_SUPPORTED => fidl_mlme::ScanResultCode::NotSupported,
            WlanScanResult::INVALID_ARGS => fidl_mlme::ScanResultCode::InvalidArgs,
            WlanScanResult::INTERNAL_ERROR => fidl_mlme::ScanResultCode::InternalError,
            WlanScanResult::SHOULD_WAIT => fidl_mlme::ScanResultCode::ShouldWait,
            WlanScanResult::CANCELED_BY_DRIVER_OR_FIRMWARE => {
                fidl_mlme::ScanResultCode::CanceledByDriverOrFirmware
            }
            _ => {
                warn!(
                    "Invalid scan result code {}, defaulting to ScanResultCode::NotSupported",
                    end.code.0
                );
                fidl_mlme::ScanResultCode::NotSupported
            }
        },
    }
}

pub fn convert_connect_confirm(
    conf: banjo_wlan_fullmac::WlanFullmacConnectConfirm,
) -> fidl_mlme::ConnectConfirm {
    fidl_mlme::ConnectConfirm {
        peer_sta_address: conf.peer_sta_address,
        result_code: convert_status_code(conf.result_code),
        association_id: conf.association_id,
        association_ies: unsafe_slice_to_vec(conf.association_ies_list, conf.association_ies_count),
    }
}

pub fn convert_roam_start_indication(
    selected_bssid: *const u8,
    selected_bss: banjo_wlan_internal::BssDescription,
    original_association_maintained: bool,
) -> fidl_mlme::RoamStartIndication {
    let bssid_as_slice = unsafe {
        std::slice::from_raw_parts(selected_bssid, banjo_wlan_ieee80211::MAC_ADDR_LEN as usize)
    };
    let fidl_selected_bssid = match bssid_as_slice.try_into() {
        Ok(bssid) => bssid,
        Err(e) => {
            // Fullmac sets selected_bssid, and currently Fullmac will crash if the field cannot be
            // populated; so this conversion is not expected to fail. But handle that possibility,
            // just in case.
            warn!("selected_bssid conversion failed in RoamStartIndication: {}. Substituting all zeros.", e);
            [0 as u8; banjo_wlan_ieee80211::MAC_ADDR_LEN as usize]
        }
    };
    fidl_mlme::RoamStartIndication {
        selected_bssid: fidl_selected_bssid,
        selected_bss: convert_bss_description(selected_bss),
        original_association_maintained,
    }
}

pub fn convert_roam_result_indication(
    selected_bssid: *const u8,
    status_code: banjo_wlan_ieee80211::StatusCode,
    original_association_maintained: bool,
    target_bss_authenticated: bool,
    association_id: u16,
    association_ies_list: *const u8,
    association_ies_count: usize,
) -> fidl_mlme::RoamResultIndication {
    let bssid_as_slice = unsafe {
        std::slice::from_raw_parts(selected_bssid, banjo_wlan_ieee80211::MAC_ADDR_LEN as usize)
    };
    let association_ies = match status_code {
        banjo_wlan_ieee80211::StatusCode::SUCCESS => {
            unsafe_slice_to_vec(association_ies_list, association_ies_count)
        }
        _ => Vec::new(),
    };

    let mut fidl_status_code = convert_status_code(status_code);
    let fidl_selected_bssid = match bssid_as_slice.try_into() {
        Ok(bssid) => bssid,
        Err(e) => {
            // As above in convert_roam_start_indication, this conversion is not expected to fail.
            warn!("selected_bssid conversion failed in RoamResultIndication: {}. Substituting all zeros.", e);
            // Though it's very unlikely, if the roam start and the roam result have both failed
            // selected_bssid conversion, then MLME could propagate a roam success upward with an
            // incorrect (all zero) BSSID. To guard against that, override a success status to
            // canceled here so SME will fail the roam attempt.
            if fidl_status_code == fidl_ieee80211::StatusCode::Success {
                error!("RoamResultIndication reports success, but is malformed. Overriding status to canceled.");
                fidl_status_code = fidl_ieee80211::StatusCode::Canceled;
            }
            [0 as u8; banjo_wlan_ieee80211::MAC_ADDR_LEN as usize]
        }
    };
    fidl_mlme::RoamResultIndication {
        selected_bssid: fidl_selected_bssid,
        original_association_maintained,
        target_bss_authenticated,
        status_code: fidl_status_code,
        association_id,
        association_ies,
    }
}

pub fn convert_authenticate_indication(
    ind: banjo_wlan_fullmac::WlanFullmacAuthInd,
) -> fidl_mlme::AuthenticateIndication {
    use banjo_wlan_fullmac::WlanAuthType;
    fidl_mlme::AuthenticateIndication {
        peer_sta_address: ind.peer_sta_address,
        auth_type: match ind.auth_type {
            WlanAuthType::OPEN_SYSTEM => fidl_mlme::AuthenticationTypes::OpenSystem,
            WlanAuthType::SHARED_KEY => fidl_mlme::AuthenticationTypes::SharedKey,
            WlanAuthType::FAST_BSS_TRANSITION => fidl_mlme::AuthenticationTypes::FastBssTransition,
            WlanAuthType::SAE => fidl_mlme::AuthenticationTypes::Sae,
            _ => {
                warn!(
                    "Invalid auth type {}, defaulting to AuthenticationTypes::OpenSystem",
                    ind.auth_type.0
                );
                fidl_mlme::AuthenticationTypes::OpenSystem
            }
        },
    }
}

pub fn convert_deauthenticate_confirm(
    peer_sta_address_ptr: *const u8,
) -> fidl_mlme::DeauthenticateConfirm {
    let peer_sta_address = if peer_sta_address_ptr.is_null() {
        warn!("Got null pointer for peer_sta_address when converting DeauthConf. Substituting all zeros.");
        [0 as u8; banjo_wlan_ieee80211::MAC_ADDR_LEN as usize]
    } else {
        unsafe {
            std::slice::from_raw_parts(
                peer_sta_address_ptr,
                banjo_wlan_ieee80211::MAC_ADDR_LEN as usize,
            )
            .try_into()
            .expect("Could not convert")
        }
    };
    fidl_mlme::DeauthenticateConfirm { peer_sta_address }
}

pub fn convert_deauthenticate_indication(
    ind: banjo_wlan_fullmac::WlanFullmacDeauthIndication,
) -> fidl_mlme::DeauthenticateIndication {
    fidl_mlme::DeauthenticateIndication {
        peer_sta_address: ind.peer_sta_address,
        reason_code: convert_reason_code(ind.reason_code),
        locally_initiated: ind.locally_initiated,
    }
}

pub fn convert_associate_indication(
    ind: banjo_wlan_fullmac::WlanFullmacAssocInd,
) -> fidl_mlme::AssociateIndication {
    fidl_mlme::AssociateIndication {
        peer_sta_address: ind.peer_sta_address,
        // TODO(https://fxbug.dev/42068281): Fix the discrepancy between WlanFullmacAssocInd and
        // fidl_mlme::AssociateIndication
        capability_info: 0,
        listen_interval: ind.listen_interval,
        ssid: if ind.ssid.len > 0 {
            Some(ind.ssid.data[..ind.ssid.len as usize].to_vec())
        } else {
            None
        },
        rates: vec![],
        rsne: if ind.rsne_len > 0 {
            Some(ind.rsne[..ind.rsne_len as usize].to_vec())
        } else {
            None
        },
    }
}

pub fn convert_disassociate_confirm(
    conf: banjo_wlan_fullmac::WlanFullmacDisassocConfirm,
) -> fidl_mlme::DisassociateConfirm {
    fidl_mlme::DisassociateConfirm { status: conf.status }
}

pub fn convert_disassociate_indication(
    ind: banjo_wlan_fullmac::WlanFullmacDisassocIndication,
) -> fidl_mlme::DisassociateIndication {
    fidl_mlme::DisassociateIndication {
        peer_sta_address: ind.peer_sta_address,
        reason_code: convert_reason_code(ind.reason_code),
        locally_initiated: ind.locally_initiated,
    }
}

pub fn convert_start_confirm(
    conf: banjo_wlan_fullmac::WlanFullmacStartConfirm,
) -> fidl_mlme::StartConfirm {
    use banjo_wlan_fullmac::WlanStartResult;
    fidl_mlme::StartConfirm {
        result_code: match conf.result_code {
            WlanStartResult::SUCCESS => fidl_mlme::StartResultCode::Success,
            WlanStartResult::BSS_ALREADY_STARTED_OR_JOINED => {
                fidl_mlme::StartResultCode::BssAlreadyStartedOrJoined
            }
            WlanStartResult::RESET_REQUIRED_BEFORE_START => {
                fidl_mlme::StartResultCode::ResetRequiredBeforeStart
            }
            WlanStartResult::NOT_SUPPORTED => fidl_mlme::StartResultCode::NotSupported,
            _ => {
                warn!(
                    "Invalid start result {}, defaulting to StartResultCode::InternalError",
                    conf.result_code.0
                );
                fidl_mlme::StartResultCode::InternalError
            }
        },
    }
}

pub fn convert_stop_confirm(
    conf: banjo_wlan_fullmac::WlanFullmacStopConfirm,
) -> fidl_mlme::StopConfirm {
    use banjo_wlan_fullmac::WlanStopResult;
    fidl_mlme::StopConfirm {
        result_code: match conf.result_code {
            WlanStopResult::SUCCESS => fidl_mlme::StopResultCode::Success,
            WlanStopResult::BSS_ALREADY_STOPPED => fidl_mlme::StopResultCode::BssAlreadyStopped,
            WlanStopResult::INTERNAL_ERROR => fidl_mlme::StopResultCode::InternalError,
            _ => {
                warn!(
                    "Invalid stop result {}, defaulting to StopResultCode::InternalError",
                    conf.result_code.0
                );
                fidl_mlme::StopResultCode::InternalError
            }
        },
    }
}

pub fn convert_eapol_confirm(
    conf: banjo_wlan_fullmac::WlanFullmacEapolConfirm,
) -> fidl_mlme::EapolConfirm {
    use banjo_wlan_fullmac::WlanEapolResult;
    fidl_mlme::EapolConfirm {
        result_code: match conf.result_code {
            WlanEapolResult::SUCCESS => fidl_mlme::EapolResultCode::Success,
            WlanEapolResult::TRANSMISSION_FAILURE => {
                fidl_mlme::EapolResultCode::TransmissionFailure
            }
            _ => {
                warn!(
                    "Invalid eapol result code {}, defaulting to EapolResultCode::TransmissionFailure",
                    conf.result_code.0
                );
                fidl_mlme::EapolResultCode::TransmissionFailure
            }
        },
        dst_addr: conf.dst_addr,
    }
}

pub fn convert_channel_switch_info(
    info: banjo_wlan_fullmac::WlanFullmacChannelSwitchInfo,
) -> fidl_internal::ChannelSwitchInfo {
    fidl_internal::ChannelSwitchInfo { new_channel: info.new_channel }
}

pub fn convert_signal_report_indication(
    ind: banjo_wlan_fullmac::WlanFullmacSignalReportIndication,
) -> fidl_internal::SignalReportIndication {
    fidl_internal::SignalReportIndication { rssi_dbm: ind.rssi_dbm, snr_db: ind.snr_db }
}

pub fn convert_eapol_indication(
    ind: banjo_wlan_fullmac::WlanFullmacEapolIndication,
) -> fidl_mlme::EapolIndication {
    fidl_mlme::EapolIndication {
        src_addr: ind.src_addr,
        dst_addr: ind.dst_addr,
        data: unsafe_slice_to_vec(ind.data_list, ind.data_count),
    }
}

pub fn convert_pmk_info(info: banjo_wlan_fullmac::WlanFullmacPmkInfo) -> fidl_mlme::PmkInfo {
    fidl_mlme::PmkInfo {
        pmk: unsafe_slice_to_vec(info.pmk_list, info.pmk_count),
        pmkid: unsafe_slice_to_vec(info.pmkid_list, info.pmkid_count),
    }
}

pub fn convert_sae_handshake_indication(
    ind: banjo_wlan_fullmac::WlanFullmacSaeHandshakeInd,
) -> fidl_mlme::SaeHandshakeIndication {
    fidl_mlme::SaeHandshakeIndication { peer_sta_address: ind.peer_sta_address }
}

pub fn convert_sae_frame(frame: banjo_wlan_fullmac::WlanFullmacSaeFrame) -> fidl_mlme::SaeFrame {
    fidl_mlme::SaeFrame {
        peer_sta_address: frame.peer_sta_address,
        status_code: convert_status_code(frame.status_code),
        seq_num: frame.seq_num,
        sae_fields: unsafe_slice_to_vec(frame.sae_fields_list, frame.sae_fields_count),
    }
}

fn convert_wmm_ac_params(
    params: banjo_wlan_common::WlanWmmAccessCategoryParameters,
) -> fidl_internal::WmmAcParams {
    fidl_internal::WmmAcParams {
        ecw_min: params.ecw_min,
        ecw_max: params.ecw_max,
        aifsn: params.aifsn,
        txop_limit: params.txop_limit,
        acm: params.acm,
    }
}

pub fn convert_wmm_params(
    wmm_params: banjo_wlan_common::WlanWmmParameters,
) -> fidl_internal::WmmStatusResponse {
    fidl_internal::WmmStatusResponse {
        apsd: wmm_params.apsd,
        ac_be_params: convert_wmm_ac_params(wmm_params.ac_be_params),
        ac_bk_params: convert_wmm_ac_params(wmm_params.ac_bk_params),
        ac_vi_params: convert_wmm_ac_params(wmm_params.ac_vi_params),
        ac_vo_params: convert_wmm_ac_params(wmm_params.ac_vo_params),
    }
}
