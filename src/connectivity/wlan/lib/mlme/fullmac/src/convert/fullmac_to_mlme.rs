// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{bail, Context, Error, Result};
use tracing::warn;
use {
    fidl_fuchsia_wlan_common as fidl_common, fidl_fuchsia_wlan_fullmac as fidl_fullmac,
    fidl_fuchsia_wlan_ieee80211 as fidl_ieee80211, fidl_fuchsia_wlan_internal as fidl_internal,
    fidl_fuchsia_wlan_mlme as fidl_mlme, fidl_fuchsia_wlan_stats as fidl_stats,
};

pub fn convert_iface_counter_stats(
    stats: fidl_fullmac::WlanFullmacIfaceCounterStats,
) -> fidl_stats::IfaceCounterStats {
    fidl_stats::IfaceCounterStats {
        rx_unicast_total: stats.rx_unicast_total,
        rx_unicast_drop: stats.rx_unicast_drop,
        rx_multicast: stats.rx_multicast,
        tx_total: stats.tx_total,
        tx_drop: stats.tx_drop,
    }
}

pub fn convert_iface_histogram_stats(
    stats: fidl_fullmac::WlanFullmacIfaceHistogramStats,
) -> fidl_stats::IfaceHistogramStats {
    // TODO(https://fxbug.dev/356115270): Understand why these asserts are needed, or remove them if not.
    assert!(stats.noise_floor_histograms.is_some());
    assert_eq!(stats.noise_floor_histograms.as_ref().unwrap().len(), 1);

    assert!(stats.rssi_histograms.is_some());
    assert_eq!(stats.rssi_histograms.as_ref().unwrap().len(), 1);

    assert!(stats.rx_rate_index_histograms.is_some());
    assert_eq!(stats.rx_rate_index_histograms.as_ref().unwrap().len(), 1);

    assert!(stats.snr_histograms.is_some());
    assert_eq!(stats.snr_histograms.as_ref().unwrap().len(), 1);

    fidl_stats::IfaceHistogramStats {
        noise_floor_histograms: stats.noise_floor_histograms.map_or_else(
            || vec![],
            |histograms| {
                histograms
                    .into_iter()
                    .map(|hist| {
                        let (hist_scope, antenna_id) =
                            convert_hist_scope_and_antenna_id(hist.hist_scope, hist.antenna_id);
                        fidl_stats::NoiseFloorHistogram {
                            hist_scope,
                            antenna_id: antenna_id.map(Box::new),
                            noise_floor_samples: hist
                                .noise_floor_samples
                                .iter()
                                .filter(|s| s.num_samples > 0)
                                .map(|s| fidl_stats::HistBucket {
                                    bucket_index: s.bucket_index,
                                    num_samples: s.num_samples,
                                })
                                .collect(),
                            invalid_samples: hist.invalid_samples,
                        }
                    })
                    .collect()
            },
        ),
        rssi_histograms: stats.rssi_histograms.map_or_else(
            || vec![],
            |histograms| {
                histograms
                    .into_iter()
                    .map(|hist| {
                        let (hist_scope, antenna_id) =
                            convert_hist_scope_and_antenna_id(hist.hist_scope, hist.antenna_id);
                        fidl_stats::RssiHistogram {
                            hist_scope,
                            antenna_id: antenna_id.map(Box::new),
                            rssi_samples: hist
                                .rssi_samples
                                .iter()
                                .filter(|s| s.num_samples > 0)
                                .map(|s| fidl_stats::HistBucket {
                                    bucket_index: s.bucket_index,
                                    num_samples: s.num_samples,
                                })
                                .collect(),
                            invalid_samples: hist.invalid_samples,
                        }
                    })
                    .collect()
            },
        ),
        rx_rate_index_histograms: stats.rx_rate_index_histograms.map_or_else(
            || vec![],
            |histograms| {
                histograms
                    .into_iter()
                    .map(|hist| {
                        let (hist_scope, antenna_id) =
                            convert_hist_scope_and_antenna_id(hist.hist_scope, hist.antenna_id);
                        fidl_stats::RxRateIndexHistogram {
                            hist_scope,
                            antenna_id: antenna_id.map(Box::new),
                            rx_rate_index_samples: hist
                                .rx_rate_index_samples
                                .iter()
                                .filter(|s| s.num_samples > 0)
                                .map(|s| fidl_stats::HistBucket {
                                    bucket_index: s.bucket_index,
                                    num_samples: s.num_samples,
                                })
                                .collect(),
                            invalid_samples: hist.invalid_samples,
                        }
                    })
                    .collect()
            },
        ),
        snr_histograms: stats.snr_histograms.map_or_else(
            || vec![],
            |histograms| {
                histograms
                    .into_iter()
                    .map(|hist| {
                        let (hist_scope, antenna_id) =
                            convert_hist_scope_and_antenna_id(hist.hist_scope, hist.antenna_id);
                        fidl_stats::SnrHistogram {
                            hist_scope,
                            antenna_id: antenna_id.map(Box::new),
                            snr_samples: hist
                                .snr_samples
                                .iter()
                                .filter(|s| s.num_samples > 0)
                                .map(|s| fidl_stats::HistBucket {
                                    bucket_index: s.bucket_index,
                                    num_samples: s.num_samples,
                                })
                                .collect(),
                            invalid_samples: hist.invalid_samples,
                        }
                    })
                    .collect()
            },
        ),
    }
}

pub fn convert_device_info(
    info: fidl_fullmac::WlanFullmacImplQueryResponse,
) -> Result<fidl_mlme::DeviceInfo> {
    let bands: Vec<fidl_mlme::BandCapability> = info
        .band_caps
        .context("missing band_caps")?
        .into_iter()
        .map(|band_cap| convert_band_cap(band_cap))
        .collect();
    Ok(fidl_mlme::DeviceInfo {
        sta_addr: info.sta_addr.context("missing sta_addr")?,
        role: info.role.context("missing role")?,
        bands,
        // TODO(https://fxbug.dev/42169534): This field will be replaced in the new driver features
        // framework.
        softmac_hardware_capability: 0,
        // TODO(https://fxbug.dev/42120297): This field is stubbed out for future use.
        qos_capable: false,
    })
}

pub fn convert_set_keys_resp(
    resp: fidl_fullmac::WlanFullmacSetKeysResp,
    original_set_keys_req: &fidl_mlme::SetKeysRequest,
) -> Result<fidl_mlme::SetKeysConfirm> {
    if resp.statuslist.len() != original_set_keys_req.keylist.len() {
        bail!(
            "SetKeysReq and SetKeysResp num_keys count differ: {} != {}",
            original_set_keys_req.keylist.len(),
            resp.statuslist.len()
        );
    }
    let mut results = vec![];
    for i in 0..resp.statuslist.len() {
        results.push(fidl_mlme::SetKeyResult {
            key_id: original_set_keys_req.keylist[i].key_id,
            status: resp.statuslist[i],
        });
    }
    Ok(fidl_mlme::SetKeysConfirm { results })
}

pub fn convert_scan_result(
    result: fidl_fullmac::WlanFullmacImplIfcOnScanResultRequest,
) -> Result<fidl_mlme::ScanResult> {
    Ok(fidl_mlme::ScanResult {
        txn_id: result.txn_id.context("missing txn_id")?,
        timestamp_nanos: result.timestamp_nanos.context("missing timestamp_nanos")?,
        bss: result.bss.context("missing bss")?,
    })
}

pub fn convert_scan_end(
    end: fidl_fullmac::WlanFullmacImplIfcOnScanEndRequest,
) -> Result<fidl_mlme::ScanEnd> {
    use fidl_fullmac::WlanScanResult;
    Ok(fidl_mlme::ScanEnd {
        txn_id: end.txn_id.context("missing txn_id")?,
        code: match end.code.context("missing code")? {
            WlanScanResult::Success => fidl_mlme::ScanResultCode::Success,
            WlanScanResult::NotSupported => fidl_mlme::ScanResultCode::NotSupported,
            WlanScanResult::InvalidArgs => fidl_mlme::ScanResultCode::InvalidArgs,
            WlanScanResult::InternalError => fidl_mlme::ScanResultCode::InternalError,
            WlanScanResult::ShouldWait => fidl_mlme::ScanResultCode::ShouldWait,
            WlanScanResult::CanceledByDriverOrFirmware => {
                fidl_mlme::ScanResultCode::CanceledByDriverOrFirmware
            }
            _ => {
                warn!(
                    "Invalid scan result code {}, defaulting to ScanResultCode::NotSupported",
                    end.code.unwrap().into_primitive()
                );
                fidl_mlme::ScanResultCode::NotSupported
            }
        },
    })
}

pub fn convert_connect_confirm(
    conf: fidl_fullmac::WlanFullmacImplIfcConnectConfRequest,
) -> Result<fidl_mlme::ConnectConfirm> {
    Ok(fidl_mlme::ConnectConfirm {
        peer_sta_address: conf.peer_sta_address.context("missing peer_sta_address")?,
        result_code: conf.result_code.context("missing result_code")?,
        association_id: if conf.result_code == Some(fidl_ieee80211::StatusCode::Success) {
            conf.association_id.context("missing association_id")?
        } else {
            0
        },
        association_ies: if conf.result_code == Some(fidl_ieee80211::StatusCode::Success) {
            conf.association_ies.context("missing association_ies")?
        } else {
            vec![]
        },
    })
}

pub fn convert_roam_confirm(
    conf: fidl_fullmac::WlanFullmacImplIfcRoamConfRequest,
) -> Result<fidl_mlme::RoamConfirm> {
    match conf.status_code {
        Some(status_code) => match status_code {
            fidl_ieee80211::StatusCode::Success => Ok(fidl_mlme::RoamConfirm {
                selected_bssid: conf.selected_bssid.context("missing selected BSSID")?,
                status_code,
                original_association_maintained: conf
                    .original_association_maintained
                    .context("missing original_association_maintained")?,
                target_bss_authenticated: conf
                    .target_bss_authenticated
                    .context("missing target_bss_authenticated")?,
                association_id: conf.association_id.context("missing association_id")?,
                association_ies: conf.association_ies.context("missing association_ies")?,
            }),
            _ => Ok(fidl_mlme::RoamConfirm {
                selected_bssid: conf.selected_bssid.context("missing selected BSSID")?,
                status_code,
                original_association_maintained: conf
                    .original_association_maintained
                    .context("missing original_association_maintained")?,
                target_bss_authenticated: conf
                    .target_bss_authenticated
                    .context("missing target_bss_authenticated")?,
                association_id: 0,
                association_ies: Vec::new(),
            }),
        },
        None => Err(Error::msg("Fullmac RoamConf is missing status_code")),
    }
}

pub fn convert_roam_start_indication(
    ind: fidl_fullmac::WlanFullmacImplIfcRoamStartIndRequest,
) -> Result<fidl_mlme::RoamStartIndication> {
    Ok(fidl_mlme::RoamStartIndication {
        selected_bss: ind.selected_bss.context("missing selected_bss")?,
        selected_bssid: ind.selected_bssid.context("missing selected bssid")?,
        original_association_maintained: ind
            .original_association_maintained
            .context("missing original_association_maintained")?,
    })
}

pub fn convert_roam_result_indication(
    ind: fidl_fullmac::WlanFullmacImplIfcRoamResultIndRequest,
) -> Result<fidl_mlme::RoamResultIndication> {
    Ok(fidl_mlme::RoamResultIndication {
        selected_bssid: ind.selected_bssid.context("missing selected_bss_id")?,
        status_code: ind.status_code.context("missing status code")?,
        original_association_maintained: ind
            .original_association_maintained
            .context("missing original_association_maintained")?,
        target_bss_authenticated: ind
            .target_bss_authenticated
            .context("missing target_bss_authenticated")?,
        association_id: ind.association_id.context("missing association_id")?,
        association_ies: ind.association_ies.context("missing association_ies")?,
    })
}

pub fn convert_authenticate_indication(
    ind: fidl_fullmac::WlanFullmacImplIfcAuthIndRequest,
) -> Result<fidl_mlme::AuthenticateIndication> {
    use fidl_fullmac::WlanAuthType;
    Ok(fidl_mlme::AuthenticateIndication {
        peer_sta_address: ind.peer_sta_address.context("missing peer_sta_address")?,
        auth_type: match ind.auth_type {
            Some(WlanAuthType::OpenSystem) => fidl_mlme::AuthenticationTypes::OpenSystem,
            Some(WlanAuthType::SharedKey) => fidl_mlme::AuthenticationTypes::SharedKey,
            Some(WlanAuthType::FastBssTransition) => {
                fidl_mlme::AuthenticationTypes::FastBssTransition
            }
            Some(WlanAuthType::Sae) => fidl_mlme::AuthenticationTypes::Sae,
            _ => {
                warn!(
                    "Invalid auth type {}, defaulting to AuthenticationTypes::OpenSystem",
                    ind.auth_type.expect("missing auth type").into_primitive()
                );
                fidl_mlme::AuthenticationTypes::OpenSystem
            }
        },
    })
}

pub fn convert_deauthenticate_confirm(
    conf: fidl_fullmac::WlanFullmacImplIfcDeauthConfRequest,
) -> fidl_mlme::DeauthenticateConfirm {
    let peer_sta_address = conf
        .peer_sta_address
        .or_else(|| {
            warn!(
                "Got None for peer_sta_address when converting DeauthConf. Substituting all zeros."
            );
            Some([0 as u8; fidl_ieee80211::MAC_ADDR_LEN as usize])
        })
        .unwrap();
    fidl_mlme::DeauthenticateConfirm { peer_sta_address }
}

pub fn convert_deauthenticate_indication(
    ind: fidl_fullmac::WlanFullmacImplIfcDeauthIndRequest,
) -> Result<fidl_mlme::DeauthenticateIndication> {
    Ok(fidl_mlme::DeauthenticateIndication {
        peer_sta_address: ind.peer_sta_address.context("missing peer sta address")?,
        reason_code: ind.reason_code.context("missing reason code")?,
        locally_initiated: ind.locally_initiated.context("missing locally initiated")?,
    })
}
pub fn convert_associate_indication(
    ind: fidl_fullmac::WlanFullmacImplIfcAssocIndRequest,
) -> Result<fidl_mlme::AssociateIndication> {
    Ok(fidl_mlme::AssociateIndication {
        peer_sta_address: ind.peer_sta_address.context("missing peer sta address")?,
        // TODO(https://fxbug.dev/42068281): Fix the discrepancy between WlanFullmacAssocInd and
        // fidl_mlme::AssociateIndication
        capability_info: 0,
        listen_interval: ind.listen_interval.context("missing listen interval")?,
        ssid: if ind.ssid.clone().expect("missing ssid").len() > 0 {
            Some(ind.ssid.expect("missing ssid"))
        } else {
            None
        },
        rates: vec![],
        rsne: if ind.rsne.clone().expect("missing rsne").len() > 0 {
            Some(ind.rsne.expect("missing rsne"))
        } else {
            None
        },
    })
}

pub fn convert_disassociate_confirm(
    conf: fidl_fullmac::WlanFullmacImplIfcDisassocConfRequest,
) -> fidl_mlme::DisassociateConfirm {
    let status = conf
        .status
        .or_else(|| {
            warn!("Got None for status when converting DisassocConf. Using error INTERNAL.");
            Some(zx::Status::INTERNAL.into_raw())
        })
        .unwrap();
    fidl_mlme::DisassociateConfirm { status }
}

pub fn convert_disassociate_indication(
    ind: fidl_fullmac::WlanFullmacImplIfcDisassocIndRequest,
) -> Result<fidl_mlme::DisassociateIndication> {
    Ok(fidl_mlme::DisassociateIndication {
        peer_sta_address: ind.peer_sta_address.context("missing peer_sta_address")?,
        reason_code: ind.reason_code.context("missing reason_code")?,
        locally_initiated: ind.locally_initiated.context("missing locally_initiated")?,
    })
}

pub fn convert_start_confirm(
    conf: fidl_fullmac::WlanFullmacImplIfcStartConfRequest,
) -> Result<fidl_mlme::StartConfirm> {
    use fidl_fullmac::StartResult;
    let result_code = conf.result_code.context("missing result_code")?;
    Ok(fidl_mlme::StartConfirm {
        result_code: match result_code {
            StartResult::Success => fidl_mlme::StartResultCode::Success,
            StartResult::BssAlreadyStartedOrJoined => {
                fidl_mlme::StartResultCode::BssAlreadyStartedOrJoined
            }
            StartResult::ResetRequiredBeforeStart => {
                fidl_mlme::StartResultCode::ResetRequiredBeforeStart
            }
            StartResult::NotSupported => fidl_mlme::StartResultCode::NotSupported,
            _ => {
                warn!(
                    "Invalid start result {}, defaulting to StartResultCode::InternalError",
                    result_code.into_primitive()
                );
                fidl_mlme::StartResultCode::InternalError
            }
        },
    })
}
pub fn convert_stop_confirm(
    conf: fidl_fullmac::WlanFullmacImplIfcStopConfRequest,
) -> Result<fidl_mlme::StopConfirm> {
    use fidl_fullmac::StopResult;
    let result_code = conf.result_code.context("missing result_code")?;
    Ok(fidl_mlme::StopConfirm {
        result_code: match result_code {
            StopResult::Success => fidl_mlme::StopResultCode::Success,
            StopResult::BssAlreadyStopped => fidl_mlme::StopResultCode::BssAlreadyStopped,
            StopResult::InternalError => fidl_mlme::StopResultCode::InternalError,
            _ => {
                warn!(
                    "Invalid stop result {}, defaulting to StopResultCode::InternalError",
                    result_code.into_primitive()
                );
                fidl_mlme::StopResultCode::InternalError
            }
        },
    })
}
pub fn convert_eapol_confirm(
    conf: fidl_fullmac::WlanFullmacImplIfcEapolConfRequest,
) -> Result<fidl_mlme::EapolConfirm> {
    use fidl_fullmac::EapolTxResult;
    let result_code = conf.result_code.context("missing result_code")?;
    Ok(fidl_mlme::EapolConfirm {
        result_code: match result_code {
            EapolTxResult::Success => fidl_mlme::EapolResultCode::Success,
            EapolTxResult::TransmissionFailure => fidl_mlme::EapolResultCode::TransmissionFailure,
            _ => {
                warn!(
                    "Invalid eapol result code {}, defaulting to EapolResultCode::TransmissionFailure",
                    result_code.into_primitive()
                );
                fidl_mlme::EapolResultCode::TransmissionFailure
            }
        },
        dst_addr: conf.dst_addr.context("missing dst_addr")?,
    })
}
pub fn convert_channel_switch_info(
    info: fidl_fullmac::WlanFullmacChannelSwitchInfo,
) -> fidl_internal::ChannelSwitchInfo {
    fidl_internal::ChannelSwitchInfo { new_channel: info.new_channel }
}
pub fn convert_signal_report_indication(
    ind: fidl_fullmac::WlanFullmacSignalReportIndication,
) -> fidl_internal::SignalReportIndication {
    fidl_internal::SignalReportIndication { rssi_dbm: ind.rssi_dbm, snr_db: ind.snr_db }
}
pub fn convert_eapol_indication(
    ind: fidl_fullmac::WlanFullmacEapolIndication,
) -> fidl_mlme::EapolIndication {
    fidl_mlme::EapolIndication { src_addr: ind.src_addr, dst_addr: ind.dst_addr, data: ind.data }
}
pub fn convert_pmk_info(
    info: fidl_fullmac::WlanFullmacImplIfcOnPmkAvailableRequest,
) -> Result<fidl_mlme::PmkInfo> {
    Ok(fidl_mlme::PmkInfo {
        pmk: info.pmk.context("missing pmk")?,
        pmkid: info.pmkid.context("missing pmkid")?,
    })
}
pub fn convert_sae_handshake_indication(
    ind: fidl_fullmac::WlanFullmacSaeHandshakeInd,
) -> fidl_mlme::SaeHandshakeIndication {
    fidl_mlme::SaeHandshakeIndication { peer_sta_address: ind.peer_sta_address }
}
pub fn convert_sae_frame(frame: fidl_fullmac::SaeFrame) -> Result<fidl_mlme::SaeFrame> {
    Ok(fidl_mlme::SaeFrame {
        peer_sta_address: frame.peer_sta_address.context("missing peer_sta_address")?,
        status_code: frame.status_code.context("missing status code")?,
        seq_num: frame.seq_num.context("missing seq_num")?,
        sae_fields: frame.sae_fields.context("missing sae_fields")?,
    })
}
pub fn convert_wmm_params(
    wmm_params: fidl_common::WlanWmmParameters,
) -> fidl_internal::WmmStatusResponse {
    fidl_internal::WmmStatusResponse {
        apsd: wmm_params.apsd,
        ac_be_params: convert_wmm_ac_params(wmm_params.ac_be_params),
        ac_bk_params: convert_wmm_ac_params(wmm_params.ac_bk_params),
        ac_vi_params: convert_wmm_ac_params(wmm_params.ac_vi_params),
        ac_vo_params: convert_wmm_ac_params(wmm_params.ac_vo_params),
    }
}
fn convert_wmm_ac_params(
    params: fidl_common::WlanWmmAccessCategoryParameters,
) -> fidl_internal::WmmAcParams {
    fidl_internal::WmmAcParams {
        ecw_min: params.ecw_min,
        ecw_max: params.ecw_max,
        aifsn: params.aifsn,
        txop_limit: params.txop_limit,
        acm: params.acm,
    }
}
fn convert_band_cap(cap: fidl_fullmac::WlanFullmacBandCapability) -> fidl_mlme::BandCapability {
    fidl_mlme::BandCapability {
        band: cap.band,
        basic_rates: cap.basic_rates,
        ht_cap: if cap.ht_supported {
            Some(Box::new(fidl_ieee80211::HtCapabilities { bytes: cap.ht_caps.bytes }))
        } else {
            None
        },
        vht_cap: if cap.vht_supported {
            Some(Box::new(fidl_ieee80211::VhtCapabilities { bytes: cap.vht_caps.bytes }))
        } else {
            None
        },
        operating_channels: cap.operating_channel_list[..cap.operating_channel_count as usize]
            .to_vec(),
    }
}
fn convert_hist_scope_and_antenna_id(
    scope: fidl_fullmac::WlanFullmacHistScope,
    antenna_id: fidl_fullmac::WlanFullmacAntennaId,
) -> (fidl_stats::HistScope, Option<fidl_stats::AntennaId>) {
    match scope {
        fidl_fullmac::WlanFullmacHistScope::Station => (fidl_stats::HistScope::Station, None),
        fidl_fullmac::WlanFullmacHistScope::PerAntenna => {
            let antenna_id = fidl_stats::AntennaId {
                freq: match fidl_stats::AntennaFreq::from_primitive(
                    antenna_id.freq.into_primitive(),
                ) {
                    Some(freq) => freq,
                    None => {
                        warn!(
                            "Invalid antenna freq {}, defaulting to AntennaFreq::Antenna2G",
                            antenna_id.freq.into_primitive()
                        );
                        fidl_stats::AntennaFreq::Antenna2G
                    }
                },
                index: antenna_id.index,
            };
            (fidl_stats::HistScope::PerAntenna, Some(antenna_id))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn test_convert_iface_histogram_stats() {
        let stats = fidl_fullmac::WlanFullmacIfaceHistogramStats {
            noise_floor_histograms: Some(vec![fidl_fullmac::WlanFullmacNoiseFloorHistogram {
                hist_scope: fidl_fullmac::WlanFullmacHistScope::Station,
                antenna_id: fidl_fullmac::WlanFullmacAntennaId {
                    freq: fidl_fullmac::WlanFullmacAntennaFreq::Antenna2G,
                    index: 0,
                },
                noise_floor_samples: vec![fidl_fullmac::WlanFullmacHistBucket {
                    bucket_index: 1,
                    num_samples: 1,
                }],
                invalid_samples: 1,
            }]),
            rssi_histograms: Some(vec![fidl_fullmac::WlanFullmacRssiHistogram {
                hist_scope: fidl_fullmac::WlanFullmacHistScope::Station,
                antenna_id: fidl_fullmac::WlanFullmacAntennaId {
                    freq: fidl_fullmac::WlanFullmacAntennaFreq::Antenna2G,
                    index: 0,
                },
                rssi_samples: vec![fidl_fullmac::WlanFullmacHistBucket {
                    bucket_index: 2,
                    num_samples: 2,
                }],
                invalid_samples: 1,
            }]),
            rx_rate_index_histograms: Some(vec![fidl_fullmac::WlanFullmacRxRateIndexHistogram {
                hist_scope: fidl_fullmac::WlanFullmacHistScope::Station,
                antenna_id: fidl_fullmac::WlanFullmacAntennaId {
                    freq: fidl_fullmac::WlanFullmacAntennaFreq::Antenna2G,
                    index: 0,
                },
                rx_rate_index_samples: vec![fidl_fullmac::WlanFullmacHistBucket {
                    bucket_index: 3,
                    num_samples: 3,
                }],
                invalid_samples: 1,
            }]),
            snr_histograms: Some(vec![fidl_fullmac::WlanFullmacSnrHistogram {
                hist_scope: fidl_fullmac::WlanFullmacHistScope::Station,
                antenna_id: fidl_fullmac::WlanFullmacAntennaId {
                    freq: fidl_fullmac::WlanFullmacAntennaFreq::Antenna2G,
                    index: 0,
                },
                snr_samples: vec![fidl_fullmac::WlanFullmacHistBucket {
                    bucket_index: 4,
                    num_samples: 4,
                }],
                invalid_samples: 1,
            }]),
            ..Default::default()
        };

        assert_eq!(
            convert_iface_histogram_stats(stats),
            fidl_stats::IfaceHistogramStats {
                noise_floor_histograms: vec![fidl_stats::NoiseFloorHistogram {
                    hist_scope: fidl_stats::HistScope::Station,
                    antenna_id: None,
                    noise_floor_samples: vec![fidl_stats::HistBucket {
                        bucket_index: 1,
                        num_samples: 1,
                    }],
                    invalid_samples: 1,
                }],
                rssi_histograms: vec![fidl_stats::RssiHistogram {
                    hist_scope: fidl_stats::HistScope::Station,
                    antenna_id: None,
                    rssi_samples: vec![fidl_stats::HistBucket { bucket_index: 2, num_samples: 2 }],
                    invalid_samples: 1,
                }],
                rx_rate_index_histograms: vec![fidl_stats::RxRateIndexHistogram {
                    hist_scope: fidl_stats::HistScope::Station,
                    antenna_id: None,
                    rx_rate_index_samples: vec![fidl_stats::HistBucket {
                        bucket_index: 3,
                        num_samples: 3,
                    }],
                    invalid_samples: 1,
                }],
                snr_histograms: vec![fidl_stats::SnrHistogram {
                    hist_scope: fidl_stats::HistScope::Station,
                    antenna_id: None,
                    snr_samples: vec![fidl_stats::HistBucket { bucket_index: 4, num_samples: 4 }],
                    invalid_samples: 1,
                }],
            }
        );
    }

    #[test]
    fn test_convert_iface_histogram_stats_filters_out_samples_with_len_0() {
        let stats = fidl_fullmac::WlanFullmacIfaceHistogramStats {
            noise_floor_histograms: Some(vec![fidl_fullmac::WlanFullmacNoiseFloorHistogram {
                hist_scope: fidl_fullmac::WlanFullmacHistScope::Station,
                antenna_id: fidl_fullmac::WlanFullmacAntennaId {
                    freq: fidl_fullmac::WlanFullmacAntennaFreq::Antenna2G,
                    index: 0,
                },
                noise_floor_samples: vec![fidl_fullmac::WlanFullmacHistBucket {
                    bucket_index: 0,
                    num_samples: 0,
                }],
                invalid_samples: 1,
            }]),
            rssi_histograms: Some(vec![fidl_fullmac::WlanFullmacRssiHistogram {
                hist_scope: fidl_fullmac::WlanFullmacHistScope::Station,
                antenna_id: fidl_fullmac::WlanFullmacAntennaId {
                    freq: fidl_fullmac::WlanFullmacAntennaFreq::Antenna2G,
                    index: 0,
                },
                rssi_samples: vec![fidl_fullmac::WlanFullmacHistBucket {
                    bucket_index: 0,
                    num_samples: 0,
                }],
                invalid_samples: 1,
            }]),
            rx_rate_index_histograms: Some(vec![fidl_fullmac::WlanFullmacRxRateIndexHistogram {
                hist_scope: fidl_fullmac::WlanFullmacHistScope::Station,
                antenna_id: fidl_fullmac::WlanFullmacAntennaId {
                    freq: fidl_fullmac::WlanFullmacAntennaFreq::Antenna2G,
                    index: 0,
                },
                rx_rate_index_samples: vec![fidl_fullmac::WlanFullmacHistBucket {
                    bucket_index: 0,
                    num_samples: 0,
                }],
                invalid_samples: 1,
            }]),
            snr_histograms: Some(vec![fidl_fullmac::WlanFullmacSnrHistogram {
                hist_scope: fidl_fullmac::WlanFullmacHistScope::Station,
                antenna_id: fidl_fullmac::WlanFullmacAntennaId {
                    freq: fidl_fullmac::WlanFullmacAntennaFreq::Antenna2G,
                    index: 0,
                },
                snr_samples: vec![fidl_fullmac::WlanFullmacHistBucket {
                    bucket_index: 0,
                    num_samples: 0,
                }],
                invalid_samples: 1,
            }]),
            ..Default::default()
        };

        assert_eq!(
            convert_iface_histogram_stats(stats),
            fidl_stats::IfaceHistogramStats {
                noise_floor_histograms: vec![fidl_stats::NoiseFloorHistogram {
                    hist_scope: fidl_stats::HistScope::Station,
                    antenna_id: None,
                    noise_floor_samples: vec![],
                    invalid_samples: 1,
                }],
                rssi_histograms: vec![fidl_stats::RssiHistogram {
                    hist_scope: fidl_stats::HistScope::Station,
                    antenna_id: None,
                    rssi_samples: vec![],
                    invalid_samples: 1,
                }],
                rx_rate_index_histograms: vec![fidl_stats::RxRateIndexHistogram {
                    hist_scope: fidl_stats::HistScope::Station,
                    antenna_id: None,
                    rx_rate_index_samples: vec![],
                    invalid_samples: 1,
                }],
                snr_histograms: vec![fidl_stats::SnrHistogram {
                    hist_scope: fidl_stats::HistScope::Station,
                    antenna_id: None,
                    snr_samples: vec![],
                    invalid_samples: 1,
                }],
            }
        );
    }

    #[test]
    fn test_convert_set_keys_resp() {
        let fullmac_resp =
            fidl_fullmac::WlanFullmacSetKeysResp { statuslist: vec![zx::sys::ZX_ERR_INTERNAL; 1] };
        let original_req =
            fidl_mlme::SetKeysRequest { keylist: vec![fake_set_key_descriptor(); 1] };

        assert_eq!(
            convert_set_keys_resp(fullmac_resp, &original_req).unwrap(),
            fidl_mlme::SetKeysConfirm {
                results: vec![fidl_mlme::SetKeyResult {
                    key_id: original_req.keylist[0].key_id,
                    status: zx::sys::ZX_ERR_INTERNAL,
                }],
            }
        );
    }

    #[test]
    fn test_convert_set_keys_resp_mismatching_original_req_is_error() {
        let fullmac_resp =
            fidl_fullmac::WlanFullmacSetKeysResp { statuslist: vec![zx::sys::ZX_ERR_INTERNAL; 2] };
        let original_req =
            fidl_mlme::SetKeysRequest { keylist: vec![fake_set_key_descriptor(); 1] };
        assert!(convert_set_keys_resp(fullmac_resp, &original_req).is_err());
    }

    #[test]
    fn test_convert_authenticate_indication_with_unknown_auth_type_defaults_to_open_system() {
        let fullmac = fidl_fullmac::WlanFullmacImplIfcAuthIndRequest {
            peer_sta_address: Some([8; 6]),
            auth_type: Some(fidl_fullmac::WlanAuthType::from_primitive_allow_unknown(100)),
            ..Default::default()
        };
        assert_eq!(
            convert_authenticate_indication(fullmac).unwrap(),
            fidl_mlme::AuthenticateIndication {
                peer_sta_address: [8; 6],
                auth_type: fidl_mlme::AuthenticationTypes::OpenSystem,
            }
        );
    }

    #[test]
    fn test_convert_deauthenticate_confirm_missing_address_defaults_to_zeros() {
        let fullmac = fidl_fullmac::WlanFullmacImplIfcDeauthConfRequest {
            peer_sta_address: None,
            ..Default::default()
        };
        assert_eq!(
            convert_deauthenticate_confirm(fullmac),
            fidl_mlme::DeauthenticateConfirm { peer_sta_address: [0; 6] }
        );
    }

    #[test]
    fn test_convert_associate_indication_empty_vec_and_ssid_are_none() {
        let fullmac = fidl_fullmac::WlanFullmacImplIfcAssocIndRequest {
            peer_sta_address: Some([3; 6]),
            listen_interval: Some(123),
            ssid: vec![].into(),
            rsne: vec![].into(),
            vendor_ie: vec![].into(),
            ..Default::default()
        };

        let mlme = convert_associate_indication(fullmac).unwrap();
        assert!(mlme.ssid.is_none());
        assert!(mlme.rsne.is_none());
    }

    #[test]
    fn test_convert_start_confirm_unknown_result_code_defaults_to_internal_error() {
        let fullmac = fidl_fullmac::WlanFullmacImplIfcStartConfRequest {
            result_code: Some(fidl_fullmac::StartResult::from_primitive_allow_unknown(123)),
            ..Default::default()
        };
        assert_eq!(
            convert_start_confirm(fullmac).unwrap(),
            fidl_mlme::StartConfirm { result_code: fidl_mlme::StartResultCode::InternalError }
        );
    }

    #[test]
    fn test_convert_stop_confirm_unknown_result_code_defaults_to_internal_error() {
        let fullmac = fidl_fullmac::WlanFullmacImplIfcStopConfRequest {
            result_code: Some(fidl_fullmac::StopResult::from_primitive_allow_unknown(123)),
            ..Default::default()
        };
        assert_eq!(
            convert_stop_confirm(fullmac).unwrap(),
            fidl_mlme::StopConfirm { result_code: fidl_mlme::StopResultCode::InternalError }
        );
    }

    #[test]
    fn test_convert_eapol_confirm_unknown_result_code_defaults_to_transmission_failure() {
        let fullmac = fidl_fullmac::WlanFullmacImplIfcEapolConfRequest {
            dst_addr: Some([1; 6]),
            result_code: Some(fidl_fullmac::EapolTxResult::from_primitive_allow_unknown(123)),
            ..Default::default()
        };
        assert_eq!(
            convert_eapol_confirm(fullmac).unwrap(),
            fidl_mlme::EapolConfirm {
                dst_addr: [1; 6],
                result_code: fidl_mlme::EapolResultCode::TransmissionFailure,
            }
        );
    }

    //
    // Tests for helper functions
    //
    #[test]
    fn test_convert_band_cap() {
        let fullmac = fidl_fullmac::WlanFullmacBandCapability {
            band: fidl_ieee80211::WlanBand::FiveGhz,
            basic_rates: vec![123; 3],
            ht_supported: true,
            ht_caps: fidl_ieee80211::HtCapabilities { bytes: [8; 26] },
            vht_supported: true,
            vht_caps: fidl_ieee80211::VhtCapabilities { bytes: [9; 12] },
            operating_channel_count: 45,
            operating_channel_list: [21; 256],
        };

        assert_eq!(
            convert_band_cap(fullmac),
            fidl_mlme::BandCapability {
                band: fidl_ieee80211::WlanBand::FiveGhz,
                basic_rates: vec![123; 3],
                ht_cap: Some(Box::new(fidl_ieee80211::HtCapabilities { bytes: [8; 26] })),
                vht_cap: Some(Box::new(fidl_ieee80211::VhtCapabilities { bytes: [9; 12] })),
                operating_channels: vec![21; 45],
            }
        );
    }

    #[test]
    fn test_convert_band_cap_no_ht_vht_become_none() {
        let fullmac = fidl_fullmac::WlanFullmacBandCapability {
            band: fidl_ieee80211::WlanBand::FiveGhz,
            basic_rates: vec![123; 3],
            ht_supported: false,
            ht_caps: fidl_ieee80211::HtCapabilities { bytes: [8; 26] },
            vht_supported: false,
            vht_caps: fidl_ieee80211::VhtCapabilities { bytes: [9; 12] },
            operating_channel_count: 45,
            operating_channel_list: [21; 256],
        };

        let mlme = convert_band_cap(fullmac);
        assert!(mlme.ht_cap.is_none());
        assert!(mlme.vht_cap.is_none());
    }

    #[test]
    fn test_convert_hist_scope_and_antenna_id_station_ignores_antenna_id() {
        assert_eq!(
            convert_hist_scope_and_antenna_id(
                fidl_fullmac::WlanFullmacHistScope::Station,
                fidl_fullmac::WlanFullmacAntennaId {
                    freq: fidl_fullmac::WlanFullmacAntennaFreq::Antenna2G,
                    index: 3,
                }
            ),
            (fidl_stats::HistScope::Station, None)
        );
        assert_eq!(
            convert_hist_scope_and_antenna_id(
                fidl_fullmac::WlanFullmacHistScope::Station,
                fidl_fullmac::WlanFullmacAntennaId {
                    freq: fidl_fullmac::WlanFullmacAntennaFreq::Antenna5G,
                    index: 3,
                }
            ),
            (fidl_stats::HistScope::Station, None)
        );
    }

    #[test]
    fn test_convert_hist_scope_and_antenna_id_per_antenna_valid_freq() {
        assert_eq!(
            convert_hist_scope_and_antenna_id(
                fidl_fullmac::WlanFullmacHistScope::PerAntenna,
                fidl_fullmac::WlanFullmacAntennaId {
                    freq: fidl_fullmac::WlanFullmacAntennaFreq::Antenna2G,
                    index: 3,
                }
            ),
            (
                fidl_stats::HistScope::PerAntenna,
                Some(fidl_stats::AntennaId { freq: fidl_stats::AntennaFreq::Antenna2G, index: 3 })
            )
        );
        assert_eq!(
            convert_hist_scope_and_antenna_id(
                fidl_fullmac::WlanFullmacHistScope::PerAntenna,
                fidl_fullmac::WlanFullmacAntennaId {
                    freq: fidl_fullmac::WlanFullmacAntennaFreq::Antenna5G,
                    index: 1,
                }
            ),
            (
                fidl_stats::HistScope::PerAntenna,
                Some(fidl_stats::AntennaId { freq: fidl_stats::AntennaFreq::Antenna5G, index: 1 })
            )
        );
    }
}
