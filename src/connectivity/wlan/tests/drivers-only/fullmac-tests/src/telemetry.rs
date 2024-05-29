// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use {
    crate::FullmacDriverFixture, drivers_only_common::sme_helpers,
    fidl_fuchsia_wlan_fullmac as fidl_fullmac, fidl_fuchsia_wlan_stats as fidl_stats,
    fullmac_helpers::config::FullmacDriverConfig, rand::Rng, wlan_common::assert_variant,
};

#[fuchsia::test]
async fn test_get_iface_histogram_stats() {
    let (mut fullmac_driver, generic_sme_proxy) =
        FullmacDriverFixture::create_and_get_generic_sme(FullmacDriverConfig {
            ..Default::default()
        })
        .await;
    let telemetry_proxy = sme_helpers::get_telemetry(&generic_sme_proxy).await;

    let telemetry_fut = telemetry_proxy.get_histogram_stats();

    // This contains every combination of hist scope and antenna frequency.
    // E.g., (per antenna, 2g), (station, 2g), (per antenna, 5g), (station, 5g)
    let driver_iface_histogram_stats = fidl_fullmac::WlanFullmacIfaceHistogramStats {
        noise_floor_histograms: Some(vec![fidl_fullmac::WlanFullmacNoiseFloorHistogram {
            hist_scope: fidl_fullmac::WlanFullmacHistScope::PerAntenna,
            antenna_id: fidl_fullmac::WlanFullmacAntennaId {
                freq: fidl_fullmac::WlanFullmacAntennaFreq::Antenna2G,
                index: rand::thread_rng().gen(),
            },
            noise_floor_samples: vec![fidl_fullmac::WlanFullmacHistBucket {
                bucket_index: rand::thread_rng().gen(),
                num_samples: rand::thread_rng().gen(),
            }],
            invalid_samples: rand::thread_rng().gen(),
        }]),
        rssi_histograms: Some(vec![fidl_fullmac::WlanFullmacRssiHistogram {
            hist_scope: fidl_fullmac::WlanFullmacHistScope::Station,
            antenna_id: fidl_fullmac::WlanFullmacAntennaId {
                freq: fidl_fullmac::WlanFullmacAntennaFreq::Antenna2G,
                index: rand::thread_rng().gen(),
            },
            rssi_samples: vec![fidl_fullmac::WlanFullmacHistBucket {
                bucket_index: rand::thread_rng().gen(),
                num_samples: rand::thread_rng().gen(),
            }],
            invalid_samples: rand::thread_rng().gen(),
        }]),
        rx_rate_index_histograms: Some(vec![fidl_fullmac::WlanFullmacRxRateIndexHistogram {
            hist_scope: fidl_fullmac::WlanFullmacHistScope::PerAntenna,
            antenna_id: fidl_fullmac::WlanFullmacAntennaId {
                freq: fidl_fullmac::WlanFullmacAntennaFreq::Antenna5G,
                index: rand::thread_rng().gen(),
            },
            rx_rate_index_samples: vec![fidl_fullmac::WlanFullmacHistBucket {
                bucket_index: rand::thread_rng().gen(),
                num_samples: rand::thread_rng().gen(),
            }],
            invalid_samples: rand::thread_rng().gen(),
        }]),
        snr_histograms: Some(vec![fidl_fullmac::WlanFullmacSnrHistogram {
            hist_scope: fidl_fullmac::WlanFullmacHistScope::Station,
            antenna_id: fidl_fullmac::WlanFullmacAntennaId {
                freq: fidl_fullmac::WlanFullmacAntennaFreq::Antenna5G,
                index: rand::thread_rng().gen(),
            },
            snr_samples: vec![fidl_fullmac::WlanFullmacHistBucket {
                bucket_index: rand::thread_rng().gen(),
                num_samples: rand::thread_rng().gen(),
            }],
            invalid_samples: rand::thread_rng().gen(),
        }]),
        ..Default::default()
    };

    let driver_fut = async {
        assert_variant!(fullmac_driver.request_stream.next().await,
            fidl_fullmac::WlanFullmacImplBridgeRequest::GetIfaceHistogramStats { responder } => {
                responder.send(Ok(&driver_iface_histogram_stats))
                    .expect("Could not respond to GetIfaceHistogramStats");
        });
    };

    let (sme_histogram_stats_result, _) = futures::join!(telemetry_fut, driver_fut);
    let sme_histogram_stats =
        sme_histogram_stats_result.expect("SME error when getting histogram stats").unwrap();

    // noise floor histograms
    let driver_noise_floor_histogram_vec =
        driver_iface_histogram_stats.noise_floor_histograms.unwrap();

    // Note: wlanif asserts that histogram vector only has one element
    assert_eq!(sme_histogram_stats.noise_floor_histograms.len(), 1);
    assert_eq!(
        sme_histogram_stats.noise_floor_histograms.len(),
        driver_noise_floor_histogram_vec.len()
    );

    let driver_noise_floor_histogram = &driver_noise_floor_histogram_vec[0];
    let sme_noise_floor_histogram = &sme_histogram_stats.noise_floor_histograms[0];
    assert!(check_hist_scope_and_antenna_id(
        sme_noise_floor_histogram.hist_scope,
        &sme_noise_floor_histogram.antenna_id,
        driver_noise_floor_histogram.hist_scope,
        &driver_noise_floor_histogram.antenna_id
    ));
    assert_eq!(
        sme_noise_floor_histogram.noise_floor_samples.len(),
        driver_noise_floor_histogram.noise_floor_samples.len()
    );
    assert!(hist_bucket_eq(
        &sme_noise_floor_histogram.noise_floor_samples[0],
        &driver_noise_floor_histogram.noise_floor_samples[0]
    ));
    assert_eq!(
        sme_noise_floor_histogram.invalid_samples,
        driver_noise_floor_histogram.invalid_samples
    );

    // rssi histograms
    let driver_rssi_histogram_vec = driver_iface_histogram_stats.rssi_histograms.unwrap();

    // Note: wlanif asserts that histogram vector only has one element
    assert_eq!(sme_histogram_stats.rssi_histograms.len(), 1);
    assert_eq!(sme_histogram_stats.rssi_histograms.len(), driver_rssi_histogram_vec.len());

    let driver_rssi_histogram = &driver_rssi_histogram_vec[0];
    let sme_rssi_histogram = &sme_histogram_stats.rssi_histograms[0];
    assert!(check_hist_scope_and_antenna_id(
        sme_rssi_histogram.hist_scope,
        &sme_rssi_histogram.antenna_id,
        driver_rssi_histogram.hist_scope,
        &driver_rssi_histogram.antenna_id
    ));
    assert_eq!(sme_rssi_histogram.rssi_samples.len(), driver_rssi_histogram.rssi_samples.len());
    assert!(hist_bucket_eq(
        &sme_rssi_histogram.rssi_samples[0],
        &driver_rssi_histogram.rssi_samples[0]
    ));
    assert_eq!(sme_rssi_histogram.invalid_samples, driver_rssi_histogram.invalid_samples);

    // rx_rate_index histograms
    let driver_rx_rate_index_histogram_vec =
        driver_iface_histogram_stats.rx_rate_index_histograms.unwrap();

    // Note: wlanif asserts that histogram vector only has one element
    assert_eq!(sme_histogram_stats.rx_rate_index_histograms.len(), 1);
    assert_eq!(
        sme_histogram_stats.rx_rate_index_histograms.len(),
        driver_rx_rate_index_histogram_vec.len()
    );

    let driver_rx_rate_index_histogram = &driver_rx_rate_index_histogram_vec[0];
    let sme_rx_rate_index_histogram = &sme_histogram_stats.rx_rate_index_histograms[0];
    assert!(check_hist_scope_and_antenna_id(
        sme_rx_rate_index_histogram.hist_scope,
        &sme_rx_rate_index_histogram.antenna_id,
        driver_rx_rate_index_histogram.hist_scope,
        &driver_rx_rate_index_histogram.antenna_id
    ));
    assert_eq!(
        sme_rx_rate_index_histogram.rx_rate_index_samples.len(),
        driver_rx_rate_index_histogram.rx_rate_index_samples.len()
    );
    assert!(hist_bucket_eq(
        &sme_rx_rate_index_histogram.rx_rate_index_samples[0],
        &driver_rx_rate_index_histogram.rx_rate_index_samples[0]
    ));
    assert_eq!(
        sme_rx_rate_index_histogram.invalid_samples,
        driver_rx_rate_index_histogram.invalid_samples
    );

    // snr histograms
    let driver_snr_histogram_vec = driver_iface_histogram_stats.snr_histograms.unwrap();

    // Note: wlanif asserts that histogram vector only has one element
    assert_eq!(sme_histogram_stats.snr_histograms.len(), 1);
    assert_eq!(sme_histogram_stats.snr_histograms.len(), driver_snr_histogram_vec.len());

    let driver_snr_histogram = &driver_snr_histogram_vec[0];
    let sme_snr_histogram = &sme_histogram_stats.snr_histograms[0];
    assert!(check_hist_scope_and_antenna_id(
        sme_snr_histogram.hist_scope,
        &sme_snr_histogram.antenna_id,
        driver_snr_histogram.hist_scope,
        &driver_snr_histogram.antenna_id
    ));
    assert_eq!(sme_snr_histogram.snr_samples.len(), driver_snr_histogram.snr_samples.len());
    assert!(hist_bucket_eq(
        &sme_snr_histogram.snr_samples[0],
        &driver_snr_histogram.snr_samples[0]
    ));
    assert_eq!(sme_snr_histogram.invalid_samples, driver_snr_histogram.invalid_samples);
}

fn check_hist_scope_and_antenna_id(
    sme_hist_scope: fidl_stats::HistScope,
    sme_antenna_id: &Option<Box<fidl_stats::AntennaId>>,
    fullmac_hist_scope: fidl_fullmac::WlanFullmacHistScope,
    fullmac_antenna_id: &fidl_fullmac::WlanFullmacAntennaId,
) -> bool {
    if !hist_scope_eq(sme_hist_scope, fullmac_hist_scope) {
        return false;
    }

    if sme_hist_scope == fidl_stats::HistScope::Station {
        // For station histograms, the antenna id should not be present in SME.
        return sme_antenna_id.is_none();
    }

    antenna_id_eq(sme_antenna_id.as_ref().unwrap(), fullmac_antenna_id)
}

fn hist_bucket_eq(
    sme_hist_bucket: &fidl_stats::HistBucket,
    fullmac_hist_bucket: &fidl_fullmac::WlanFullmacHistBucket,
) -> bool {
    sme_hist_bucket.bucket_index == fullmac_hist_bucket.bucket_index
        && sme_hist_bucket.num_samples == fullmac_hist_bucket.num_samples
}

fn antenna_id_eq(
    sme_antenna_id: &fidl_stats::AntennaId,
    fullmac_antenna_id: &fidl_fullmac::WlanFullmacAntennaId,
) -> bool {
    let sme_freq_as_fullmac = match sme_antenna_id.freq {
        fidl_stats::AntennaFreq::Antenna2G => fidl_fullmac::WlanFullmacAntennaFreq::Antenna2G,
        fidl_stats::AntennaFreq::Antenna5G => fidl_fullmac::WlanFullmacAntennaFreq::Antenna5G,
    };

    fullmac_antenna_id.freq == sme_freq_as_fullmac
        && sme_antenna_id.index == fullmac_antenna_id.index
}

fn hist_scope_eq(
    sme_hist_scope: fidl_stats::HistScope,
    fullmac_hist_scope: fidl_fullmac::WlanFullmacHistScope,
) -> bool {
    let sme_hist_scope_as_fullmac = match sme_hist_scope {
        fidl_stats::HistScope::Station => fidl_fullmac::WlanFullmacHistScope::Station,
        fidl_stats::HistScope::PerAntenna => fidl_fullmac::WlanFullmacHistScope::PerAntenna,
    };
    fullmac_hist_scope == sme_hist_scope_as_fullmac
}
