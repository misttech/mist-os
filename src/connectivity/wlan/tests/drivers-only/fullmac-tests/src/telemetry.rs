// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use crate::FullmacDriverFixture;
use drivers_only_common::sme_helpers;
use fullmac_helpers::config::FullmacDriverConfig;
use rand::Rng;
use wlan_common::assert_variant;
use {fidl_fuchsia_wlan_fullmac as fidl_fullmac, fidl_fuchsia_wlan_stats as fidl_stats};

#[fuchsia::test]
async fn test_get_iface_counter_stats() {
    let mut fullmac_driver =
        FullmacDriverFixture::create(FullmacDriverConfig { ..Default::default() }).await;
    let telemetry_proxy = sme_helpers::get_telemetry(&fullmac_driver.generic_sme_proxy).await;
    let telemetry_fut = telemetry_proxy.get_counter_stats();

    let driver_counter_stats = fidl_stats::IfaceCounterStats {
        rx_unicast_total: Some(rand::thread_rng().gen()),
        rx_unicast_drop: Some(rand::thread_rng().gen()),
        rx_multicast: Some(rand::thread_rng().gen()),
        tx_total: Some(rand::thread_rng().gen()),
        tx_drop: Some(rand::thread_rng().gen()),
        ..Default::default()
    };

    let driver_fut = async {
        assert_variant!(fullmac_driver.request_stream.next().await,
            fidl_fullmac::WlanFullmacImpl_Request::GetIfaceCounterStats { responder } => {
                responder.send(Ok(&driver_counter_stats))
                    .expect("Could not respond to GetIfaceHistogramStats");
        });
    };

    let (sme_counter_stats_result, _) = futures::join!(telemetry_fut, driver_fut);
    let sme_counter_stats =
        sme_counter_stats_result.expect("SME Error getting counter stats").unwrap();

    assert_eq!(
        sme_counter_stats,
        fidl_stats::IfaceCounterStats {
            rx_unicast_total: driver_counter_stats.rx_unicast_total,
            rx_unicast_drop: driver_counter_stats.rx_unicast_drop,
            rx_multicast: driver_counter_stats.rx_multicast,
            tx_total: driver_counter_stats.tx_total,
            tx_drop: driver_counter_stats.tx_drop,
            ..Default::default()
        }
    );
}

#[fuchsia::test]
async fn test_get_iface_histogram_stats() {
    let mut fullmac_driver =
        FullmacDriverFixture::create(FullmacDriverConfig { ..Default::default() }).await;
    let telemetry_proxy = sme_helpers::get_telemetry(&fullmac_driver.generic_sme_proxy).await;
    let telemetry_fut = telemetry_proxy.get_histogram_stats();

    // This contains every combination of hist scope and antenna frequency.
    // E.g., (per antenna, 2g), (station, 2g), (per antenna, 5g), (station, 5g)
    let driver_iface_histogram_stats = fidl_stats::IfaceHistogramStats {
        noise_floor_histograms: Some(vec![fidl_stats::NoiseFloorHistogram {
            hist_scope: fidl_stats::HistScope::PerAntenna,
            antenna_id: Some(Box::new(fidl_stats::AntennaId {
                freq: fidl_stats::AntennaFreq::Antenna2G,
                index: rand::thread_rng().gen(),
            })),
            noise_floor_samples: vec![fidl_stats::HistBucket {
                bucket_index: rand::thread_rng().gen(),
                num_samples: rand::thread_rng().gen(),
            }],
            invalid_samples: rand::thread_rng().gen(),
        }]),
        rssi_histograms: Some(vec![fidl_stats::RssiHistogram {
            hist_scope: fidl_stats::HistScope::Station,
            antenna_id: None,
            rssi_samples: vec![fidl_stats::HistBucket {
                bucket_index: rand::thread_rng().gen(),
                num_samples: rand::thread_rng().gen(),
            }],
            invalid_samples: rand::thread_rng().gen(),
        }]),
        rx_rate_index_histograms: Some(vec![fidl_stats::RxRateIndexHistogram {
            hist_scope: fidl_stats::HistScope::PerAntenna,
            antenna_id: Some(Box::new(fidl_stats::AntennaId {
                freq: fidl_stats::AntennaFreq::Antenna5G,
                index: rand::thread_rng().gen(),
            })),
            rx_rate_index_samples: vec![fidl_stats::HistBucket {
                bucket_index: rand::thread_rng().gen(),
                num_samples: rand::thread_rng().gen(),
            }],
            invalid_samples: rand::thread_rng().gen(),
        }]),
        snr_histograms: Some(vec![fidl_stats::SnrHistogram {
            hist_scope: fidl_stats::HistScope::Station,
            antenna_id: None,
            snr_samples: vec![fidl_stats::HistBucket {
                bucket_index: rand::thread_rng().gen(),
                num_samples: rand::thread_rng().gen(),
            }],
            invalid_samples: rand::thread_rng().gen(),
        }]),
        ..Default::default()
    };

    let driver_fut = async {
        assert_variant!(fullmac_driver.request_stream.next().await,
            fidl_fullmac::WlanFullmacImpl_Request::GetIfaceHistogramStats { responder } => {
                responder.send(Ok(&driver_iface_histogram_stats))
                    .expect("Could not respond to GetIfaceHistogramStats");
        });
    };

    let (sme_histogram_stats_result, _) = futures::join!(telemetry_fut, driver_fut);
    let sme_histogram_stats =
        sme_histogram_stats_result.expect("SME error when getting histogram stats").unwrap();

    assert_eq!(driver_iface_histogram_stats, sme_histogram_stats);
}
