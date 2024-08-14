// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{bail, Result};
use tracing::warn;
use {
    fidl_fuchsia_wlan_fullmac as fidl_fullmac, fidl_fuchsia_wlan_ieee80211 as fidl_ieee80211,
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

pub fn convert_device_info(info: fidl_fullmac::WlanFullmacQueryInfo) -> fidl_mlme::DeviceInfo {
    let bands: Vec<fidl_mlme::BandCapability> = info.band_cap_list[0..info.band_cap_count as usize]
        .into_iter()
        .map(|band_cap| convert_band_cap(band_cap.clone()))
        .collect();
    fidl_mlme::DeviceInfo {
        sta_addr: info.sta_addr,
        role: info.role,
        bands,
        // TODO(https://fxbug.dev/42169534): This field will be replaced in the new driver features
        // framework.
        softmac_hardware_capability: 0,
        // TODO(https://fxbug.dev/42120297): This field is stubbed out for future use.
        qos_capable: false,
    }
}

pub fn convert_set_keys_resp(
    resp: fidl_fullmac::WlanFullmacSetKeysResp,
    original_set_keys_req: &fidl_mlme::SetKeysRequest,
) -> Result<fidl_mlme::SetKeysConfirm> {
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

fn convert_band_cap(cap: fidl_fullmac::WlanFullmacBandCapability) -> fidl_mlme::BandCapability {
    fidl_mlme::BandCapability {
        band: cap.band,
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
