// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::bail, banjo_fuchsia_hardware_wlan_associnfo as banjo_wlan_associnfo,
    banjo_fuchsia_hardware_wlan_fullmac as banjo_wlan_fullmac,
    banjo_fuchsia_wlan_common as banjo_wlan_common,
    banjo_fuchsia_wlan_ieee80211 as banjo_wlan_ieee80211,
    banjo_fuchsia_wlan_internal as banjo_wlan_internal, fidl_fuchsia_wlan_common as fidl_common,
    fidl_fuchsia_wlan_ieee80211 as fidl_ieee80211, fidl_fuchsia_wlan_internal as fidl_internal,
    fidl_fuchsia_wlan_mlme as fidl_mlme, fidl_fuchsia_wlan_stats as fidl_stats, std::slice,
    tracing::warn,
};

fn unsafe_slice_to_vec<T: Clone>(data: *const T, len: usize) -> Vec<T> {
    if data.is_null() || len == 0 {
        vec![]
    } else {
        unsafe { slice::from_raw_parts(data, len) }.to_vec()
    }
}

pub fn convert_mac_sublayer_support(
    support: banjo_wlan_common::MacSublayerSupport,
) -> Result<fidl_common::MacSublayerSupport, anyhow::Error> {
    let rate_selection_offload = fidl_common::RateSelectionOffloadExtension {
        supported: support.rate_selection_offload.supported,
    };

    let mac_implementation_type = match fidl_common::MacImplementationType::from_primitive(
        support.device.mac_implementation_type.0,
    ) {
        Some(mac_implementation_type) => mac_implementation_type,
        None => {
            bail!(
                "Unexpected banjo_comon::MacImplementationType value {}",
                support.device.mac_implementation_type.0
            )
        }
    };

    let device = fidl_common::DeviceExtension {
        is_synthetic: support.device.is_synthetic,
        tx_status_report_supported: support.device.tx_status_report_supported,
        mac_implementation_type: mac_implementation_type,
    };

    let data_plane_type =
        match fidl_common::DataPlaneType::from_primitive(support.data_plane.data_plane_type.0) {
            Some(data_plane_type) => data_plane_type,
            None => {
                bail!(
                    "Unexpected banjo_comon::DataPlaneType value {}",
                    support.data_plane.data_plane_type.0
                )
            }
        };
    let data_plane = fidl_common::DataPlaneExtension { data_plane_type: data_plane_type };
    Ok(fidl_common::MacSublayerSupport { rate_selection_offload, data_plane, device })
}

pub fn convert_security_support(
    support: banjo_wlan_common::SecuritySupport,
) -> fidl_common::SecuritySupport {
    let mfp = fidl_common::MfpFeature { supported: support.mfp.supported };
    let sae = fidl_common::SaeFeature {
        driver_handler_supported: support.sae.driver_handler_supported,
        sme_handler_supported: support.sae.sme_handler_supported,
    };
    fidl_common::SecuritySupport { sae, mfp }
}

pub fn convert_spectrum_management_support(
    support: banjo_wlan_common::SpectrumManagementSupport,
) -> fidl_common::SpectrumManagementSupport {
    fidl_common::SpectrumManagementSupport {
        dfs: fidl_common::DfsFeature { supported: support.dfs.supported },
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

pub fn convert_roam_confirm(
    conf: banjo_wlan_fullmac::WlanFullmacRoamConfirm,
) -> fidl_mlme::RoamConfirm {
    fidl_mlme::RoamConfirm {
        target_bssid: conf.target_bssid,
        result_code: convert_status_code(conf.result_code),
        selected_bss: convert_bss_description(conf.selected_bss),
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
    conf: banjo_wlan_fullmac::WlanFullmacDeauthConfirm,
) -> fidl_mlme::DeauthenticateConfirm {
    fidl_mlme::DeauthenticateConfirm { peer_sta_address: conf.peer_sta_address }
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
        // TODO(fxbug.dev/117108): Fix the discrepancy between WlanFullmacAssocInd and
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
    params: banjo_wlan_associnfo::WlanWmmAccessCategoryParameters,
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
    wmm_params: banjo_wlan_associnfo::WlanWmmParameters,
) -> fidl_internal::WmmStatusResponse {
    fidl_internal::WmmStatusResponse {
        apsd: wmm_params.apsd,
        ac_be_params: convert_wmm_ac_params(wmm_params.ac_be_params),
        ac_bk_params: convert_wmm_ac_params(wmm_params.ac_bk_params),
        ac_vi_params: convert_wmm_ac_params(wmm_params.ac_vi_params),
        ac_vo_params: convert_wmm_ac_params(wmm_params.ac_vo_params),
    }
}

fn convert_band_cap(
    cap: banjo_wlan_fullmac::WlanFullmacBandCapability,
) -> Result<fidl_mlme::BandCapability, anyhow::Error> {
    use banjo_wlan_common::WlanBand;
    Ok(fidl_mlme::BandCapability {
        band: match cap.band {
            WlanBand::TWO_GHZ => fidl_common::WlanBand::TwoGhz,
            WlanBand::FIVE_GHZ => fidl_common::WlanBand::FiveGhz,
            _ => bail!("Invalid WLAN band {}", cap.band.0),
        },
        basic_rates: cap.basic_rate_list[..cap.basic_rate_count as usize].to_vec(),
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
    })
}

pub fn convert_device_info(
    info: banjo_wlan_fullmac::WlanFullmacQueryInfo,
) -> Result<fidl_mlme::DeviceInfo, anyhow::Error> {
    use banjo_wlan_common::WlanMacRole;
    let mut bands = vec![];
    for i in 0..info.band_cap_count as usize {
        match convert_band_cap(info.band_cap_list[i]) {
            Ok(cap) => bands.push(cap),
            Err(e) => bail!("cannot convert band cap at index {}: {}", i, e),
        }
    }

    Ok(fidl_mlme::DeviceInfo {
        sta_addr: info.sta_addr,
        role: match info.role {
            WlanMacRole::CLIENT => fidl_common::WlanMacRole::Client,
            WlanMacRole::AP => fidl_common::WlanMacRole::Ap,
            WlanMacRole::MESH => fidl_common::WlanMacRole::Mesh,
            _ => bail!("Invalid WLAN MAC role {}", info.role.0),
        },
        bands,
        // TODO(fxbug.dev/88315): This field will be replaced in the new driver features
        // framework.
        softmac_hardware_capability: 0,
        // TODO(fxbug.dev/43938): This field is stubbed out for future use.
        qos_capable: false,
    })
}

pub fn convert_set_keys_resp(
    resp: banjo_wlan_fullmac::WlanFullmacSetKeysResp,
    original_set_keys_req: &fidl_mlme::SetKeysRequest,
) -> Result<fidl_mlme::SetKeysConfirm, anyhow::Error> {
    if resp.num_keys as usize != original_set_keys_req.keylist.len() {
        bail!(
            "SetKeysReq and SetKeysResp num_keys count differ: {} != {}",
            original_set_keys_req.keylist.len(),
            resp.num_keys
        );
    }

    let mut results = vec![];
    for i in 0..resp.num_keys as usize {
        results.push(fidl_mlme::SetKeyResult {
            key_id: original_set_keys_req.keylist[i].key_id,
            status: resp.statuslist[i],
        });
    }
    Ok(fidl_mlme::SetKeysConfirm { results })
}

pub fn convert_iface_counter_stats(
    stats: banjo_wlan_fullmac::WlanFullmacIfaceCounterStats,
) -> fidl_stats::IfaceCounterStats {
    fidl_stats::IfaceCounterStats {
        rx_unicast_total: stats.rx_unicast_total,
        rx_unicast_drop: stats.rx_unicast_drop,
        rx_multicast: stats.rx_multicast,
        tx_total: stats.tx_total,
        tx_drop: stats.tx_drop,
    }
}

fn convert_hist_scope_and_antenna_id(
    scope: banjo_wlan_fullmac::WlanFullmacHistScope,
    antenna_id: banjo_wlan_fullmac::WlanFullmacAntennaId,
) -> (fidl_stats::HistScope, Option<fidl_stats::AntennaId>) {
    match scope {
        banjo_wlan_fullmac::WlanFullmacHistScope::STATION => (fidl_stats::HistScope::Station, None),
        banjo_wlan_fullmac::WlanFullmacHistScope::PER_ANTENNA => {
            let antenna_id = fidl_stats::AntennaId {
                freq: match fidl_stats::AntennaFreq::from_primitive(antenna_id.freq.0) {
                    Some(freq) => freq,
                    None => {
                        warn!(
                            "Invalid antenna freq {}, defaulting to AntennaFreq::Antenna2G",
                            antenna_id.freq.0
                        );
                        fidl_stats::AntennaFreq::Antenna2G
                    }
                },
                index: antenna_id.index,
            };
            (fidl_stats::HistScope::PerAntenna, Some(antenna_id))
        }
        _ => {
            warn!("Invalid hist scope {}, defaulting to HistScope::Station", scope.0);
            (fidl_stats::HistScope::Station, None)
        }
    }
}

pub fn convert_iface_histogram_stats(
    stats: banjo_wlan_fullmac::WlanFullmacIfaceHistogramStats,
) -> fidl_stats::IfaceHistogramStats {
    // TODO(fxbug.dev/117109): DRY these with macros
    let original_noise_floor_histograms = unsafe {
        std::slice::from_raw_parts(
            stats.noise_floor_histograms_list,
            stats.noise_floor_histograms_count,
        )
    };
    let noise_floor_histograms = original_noise_floor_histograms
        .iter()
        .map(|h| {
            let original_samples = unsafe {
                std::slice::from_raw_parts(h.noise_floor_samples_list, h.noise_floor_samples_count)
            };
            let (hist_scope, antenna_id) =
                convert_hist_scope_and_antenna_id(h.hist_scope, h.antenna_id);
            fidl_stats::NoiseFloorHistogram {
                hist_scope,
                antenna_id: antenna_id.map(Box::new),
                // Only keep the non-zero samples to make the histogram compact
                noise_floor_samples: original_samples
                    .iter()
                    .filter(|s| s.num_samples > 0)
                    .map(|s| fidl_stats::HistBucket {
                        bucket_index: s.bucket_index,
                        num_samples: s.num_samples,
                    })
                    .collect(),
                invalid_samples: h.invalid_samples,
            }
        })
        .collect();

    let original_rssi_histograms = unsafe {
        std::slice::from_raw_parts(stats.rssi_histograms_list, stats.rssi_histograms_count)
    };
    let rssi_histograms = original_rssi_histograms
        .iter()
        .map(|h| {
            let original_samples =
                unsafe { std::slice::from_raw_parts(h.rssi_samples_list, h.rssi_samples_count) };
            let (hist_scope, antenna_id) =
                convert_hist_scope_and_antenna_id(h.hist_scope, h.antenna_id);
            fidl_stats::RssiHistogram {
                hist_scope,
                antenna_id: antenna_id.map(Box::new),
                // Only keep the non-zero samples to make the histogram compact
                rssi_samples: original_samples
                    .iter()
                    .filter(|s| s.num_samples > 0)
                    .map(|s| fidl_stats::HistBucket {
                        bucket_index: s.bucket_index,
                        num_samples: s.num_samples,
                    })
                    .collect(),
                invalid_samples: h.invalid_samples,
            }
        })
        .collect();

    let original_rx_rate_index_histograms = unsafe {
        std::slice::from_raw_parts(
            stats.rx_rate_index_histograms_list,
            stats.rx_rate_index_histograms_count,
        )
    };
    let rx_rate_index_histograms = original_rx_rate_index_histograms
        .iter()
        .map(|h| {
            let original_samples = unsafe {
                std::slice::from_raw_parts(
                    h.rx_rate_index_samples_list,
                    h.rx_rate_index_samples_count,
                )
            };
            let (hist_scope, antenna_id) =
                convert_hist_scope_and_antenna_id(h.hist_scope, h.antenna_id);
            fidl_stats::RxRateIndexHistogram {
                hist_scope,
                antenna_id: antenna_id.map(Box::new),
                // Only keep the non-zero samples to make the histogram compact
                rx_rate_index_samples: original_samples
                    .iter()
                    .filter(|s| s.num_samples > 0)
                    .map(|s| fidl_stats::HistBucket {
                        bucket_index: s.bucket_index,
                        num_samples: s.num_samples,
                    })
                    .collect(),
                invalid_samples: h.invalid_samples,
            }
        })
        .collect();

    let original_snr_histograms = unsafe {
        std::slice::from_raw_parts(stats.snr_histograms_list, stats.snr_histograms_count)
    };
    let snr_histograms = original_snr_histograms
        .iter()
        .map(|h| {
            let original_samples =
                unsafe { std::slice::from_raw_parts(h.snr_samples_list, h.snr_samples_count) };
            let (hist_scope, antenna_id) =
                convert_hist_scope_and_antenna_id(h.hist_scope, h.antenna_id);
            fidl_stats::SnrHistogram {
                hist_scope,
                antenna_id: antenna_id.map(Box::new),
                // Only keep the non-zero samples to make the histogram compact
                snr_samples: original_samples
                    .iter()
                    .filter(|s| s.num_samples > 0)
                    .map(|s| fidl_stats::HistBucket {
                        bucket_index: s.bucket_index,
                        num_samples: s.num_samples,
                    })
                    .collect(),
                invalid_samples: h.invalid_samples,
            }
        })
        .collect();

    fidl_stats::IfaceHistogramStats {
        noise_floor_histograms,
        rssi_histograms,
        rx_rate_index_histograms,
        snr_histograms,
    }
}

// Most of the conversions are verified in the unit tests for the whole library,
// so only a few remaining ones are tested here.
#[cfg(test)]
mod tests {
    use {super::*, wlan_common::assert_variant};

    #[test]
    fn test_convert_mac_sublayer_support() {
        let banjo_support = banjo_wlan_common::MacSublayerSupport {
            rate_selection_offload: banjo_wlan_common::RateSelectionOffloadExtension {
                supported: true,
            },
            data_plane: banjo_wlan_common::DataPlaneExtension {
                data_plane_type: banjo_wlan_common::DataPlaneType::GENERIC_NETWORK_DEVICE,
            },
            device: banjo_wlan_common::DeviceExtension {
                is_synthetic: true,
                mac_implementation_type: banjo_wlan_common::MacImplementationType::FULLMAC,
                tx_status_report_supported: true,
            },
        };

        let fidl_support = assert_variant!(convert_mac_sublayer_support(banjo_support), Ok(s) => s);
        assert_eq!(
            fidl_support,
            fidl_common::MacSublayerSupport {
                rate_selection_offload: fidl_common::RateSelectionOffloadExtension {
                    supported: true,
                },
                data_plane: fidl_common::DataPlaneExtension {
                    data_plane_type: fidl_common::DataPlaneType::GenericNetworkDevice,
                },
                device: fidl_common::DeviceExtension {
                    is_synthetic: true,
                    mac_implementation_type: fidl_common::MacImplementationType::Fullmac,
                    tx_status_report_supported: true,
                },
            }
        );
    }

    #[test]
    fn test_convert_security_support() {
        let banjo_support = banjo_wlan_common::SecuritySupport {
            sae: banjo_wlan_common::SaeFeature {
                driver_handler_supported: false,
                sme_handler_supported: true,
            },
            mfp: banjo_wlan_common::MfpFeature { supported: true },
        };

        let fidl_support = convert_security_support(banjo_support);
        assert_eq!(
            fidl_support,
            fidl_common::SecuritySupport {
                sae: fidl_common::SaeFeature {
                    driver_handler_supported: false,
                    sme_handler_supported: true,
                },
                mfp: fidl_common::MfpFeature { supported: true },
            }
        );
    }

    #[test]
    fn test_convert_spectrum_management_support() {
        let banjo_support = banjo_wlan_common::SpectrumManagementSupport {
            dfs: banjo_wlan_common::DfsFeature { supported: true },
        };

        let fidl_support = convert_spectrum_management_support(banjo_support);
        assert_eq!(
            fidl_support,
            fidl_common::SpectrumManagementSupport {
                dfs: fidl_common::DfsFeature { supported: true },
            }
        );
    }

    #[test]
    fn test_convert_device_info() {
        let dummy_band_capability = banjo_wlan_fullmac::WlanFullmacBandCapability {
            band: banjo_wlan_common::WlanBand::TWO_GHZ,
            basic_rate_list: [3u8; 12],
            basic_rate_count: 4,
            ht_supported: true,
            ht_caps: banjo_wlan_ieee80211::HtCapabilities { bytes: [5u8; 26] },
            vht_supported: true,
            vht_caps: banjo_wlan_ieee80211::VhtCapabilities { bytes: [6u8; 12] },
            operating_channel_list: [7u8; 256],
            operating_channel_count: 8,
        };
        let banjo_info = banjo_wlan_fullmac::WlanFullmacQueryInfo {
            sta_addr: [1u8; 6],
            role: banjo_wlan_common::WlanMacRole::CLIENT,
            features: 2,
            band_cap_list: [dummy_band_capability.clone(); 16],
            band_cap_count: 1,
        };

        let fidl_info = assert_variant!(convert_device_info(banjo_info), Ok(info) => info);
        assert_eq!(
            fidl_info,
            fidl_mlme::DeviceInfo {
                sta_addr: [1u8; 6],
                role: fidl_common::WlanMacRole::Client,
                bands: vec![fidl_mlme::BandCapability {
                    band: fidl_common::WlanBand::TwoGhz,
                    basic_rates: vec![3u8; 4],
                    ht_cap: Some(Box::new(fidl_ieee80211::HtCapabilities { bytes: [5u8; 26] })),
                    vht_cap: Some(Box::new(fidl_ieee80211::VhtCapabilities { bytes: [6u8; 12] })),
                    operating_channels: vec![7u8; 8],
                }],
                softmac_hardware_capability: 0,
                qos_capable: false,
            }
        );
    }
}
