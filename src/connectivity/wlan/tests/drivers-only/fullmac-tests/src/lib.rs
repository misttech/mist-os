// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    drivers_only_common::DriversOnlyTestRealm,
    fidl_fuchsia_wlan_common as fidl_common, fidl_fuchsia_wlan_fullmac as fidl_fullmac,
    fidl_fuchsia_wlan_sme as fidl_sme, fuchsia_zircon as zx,
    fullmac_helpers::config::{default_fullmac_query_info, FullmacDriverConfig},
    futures::StreamExt,
    rand::{seq::SliceRandom, Rng},
    wlan_common::{assert_variant, random_fidl_bss_description},
};

#[fuchsia::test]
async fn test_sme_query() {
    let realm = DriversOnlyTestRealm::new().await;

    // The role and sta_addr are randomly generated for each run of the test case to ensure that
    // the platform driver doesn't hardcode either of these values.
    let roles = [fidl_common::WlanMacRole::Client, fidl_common::WlanMacRole::Ap];
    let config = FullmacDriverConfig {
        query_info: fidl_fullmac::WlanFullmacQueryInfo {
            sta_addr: rand::random(),
            role: *roles.choose(&mut rand::thread_rng()).unwrap(),
            ..default_fullmac_query_info()
        },
        ..Default::default()
    };
    let (mut fullmac_req_stream, _fullmac_ifc_proxy, generic_sme_proxy) =
        fullmac_helpers::create_fullmac_driver(&realm.testcontroller_proxy, &config).await;

    // Returns the query response
    let sme_fut = async { generic_sme_proxy.query().await.expect("Failed to request SME query") };

    let driver_fut = async {
        assert_variant!(fullmac_req_stream.next().await,
        Some(Ok(fidl_fullmac::WlanFullmacImplBridgeRequest::Query { responder })) => {
            responder
                .send(Ok(&config.query_info))
                .expect("Failed to respond to Query");
        });
    };

    let (query_resp, _) = futures::join!(sme_fut, driver_fut);
    assert_eq!(query_resp.role, config.query_info.role);
    assert_eq!(query_resp.sta_addr, config.query_info.sta_addr);
}

#[fuchsia::test]
async fn test_scan_request_success() {
    let realm = DriversOnlyTestRealm::new().await;

    let config = FullmacDriverConfig { ..Default::default() };

    let (mut fullmac_req_stream, fullmac_ifc_proxy, generic_sme_proxy) =
        fullmac_helpers::create_fullmac_driver(&realm.testcontroller_proxy, &config).await;

    let (client_sme_proxy, client_sme_server) =
        fidl::endpoints::create_proxy().expect("Failed to create client SME proxy");
    async {
        generic_sme_proxy
            .get_client_sme(client_sme_server)
            .await
            .expect("FIDL error")
            .expect("GetClientSme Error")
    }
    .await;

    let client_fut = async {
        client_sme_proxy
            .scan(&fidl_sme::ScanRequest::Passive(fidl_sme::PassiveScanRequest {}))
            .await
            .expect("FIDL error")
            .expect("ScanRequest error")
    };

    let driver_fut = async {
        let txn_id = assert_variant!(fullmac_req_stream.next().await,
        Some(Ok(fidl_fullmac::WlanFullmacImplBridgeRequest::StartScan { payload, responder })) => {
            assert_eq!(payload.scan_type.unwrap(), fidl_fullmac::WlanScanType::Passive);
            responder
                .send()
                .expect("Failed to respond to StartScan");
            payload.txn_id.expect("No txn_id found")
        });

        let scan_result_list = vec![
            fidl_fullmac::WlanFullmacScanResult {
                txn_id,
                timestamp_nanos: zx::Time::get_monotonic().into_nanos(),
                bss: random_fidl_bss_description!(),
            },
            fidl_fullmac::WlanFullmacScanResult {
                txn_id,
                timestamp_nanos: zx::Time::get_monotonic().into_nanos() + 1,
                bss: random_fidl_bss_description!(),
            },
        ];

        for scan_result in &scan_result_list {
            fullmac_ifc_proxy
                .on_scan_result(&scan_result)
                .await
                .expect("Failed to send on_scan_result");
        }

        fullmac_ifc_proxy
            .on_scan_end(&fidl_fullmac::WlanFullmacScanEnd {
                txn_id,
                code: fidl_fullmac::WlanScanResult::Success,
            })
            .await
            .expect("Failed to send on_scan_end");

        scan_result_list
    };

    let (scan_result_vmo, expected_scan_result_list) = futures::join!(client_fut, driver_fut);
    let scan_result_list =
        wlan_common::scan::read_vmo(scan_result_vmo).expect("Could not read scan result VMO");

    assert_eq!(scan_result_list.len(), expected_scan_result_list.len());
    let expected_bss_descriptions: Vec<_> =
        expected_scan_result_list.iter().map(|scan_result| scan_result.bss.clone()).collect();

    for actual in scan_result_list {
        // TODO(https://g-issues.fuchsia.dev/issues/42164608):  SME ignores timestamps so they
        // aren't checked here.
        // NOTE: order of returned scans is not guaranteed.
        assert!(expected_bss_descriptions.contains(&actual.bss_description));
    }
}

#[fuchsia::test]
async fn test_scan_request_error() {
    let realm = DriversOnlyTestRealm::new().await;

    let config = FullmacDriverConfig { ..Default::default() };

    let (mut fullmac_req_stream, fullmac_ifc_proxy, generic_sme_proxy) =
        fullmac_helpers::create_fullmac_driver(&realm.testcontroller_proxy, &config).await;

    let (client_sme_proxy, client_sme_server) =
        fidl::endpoints::create_proxy().expect("Failed to create client SME proxy");
    async {
        generic_sme_proxy
            .get_client_sme(client_sme_server)
            .await
            .expect("FIDL error")
            .expect("GetClientSme Error")
    }
    .await;

    let client_fut = async {
        client_sme_proxy
            .scan(&fidl_sme::ScanRequest::Passive(fidl_sme::PassiveScanRequest {}))
            .await
            .expect("FIDL error")
    };

    let driver_fut = async {
        let txn_id = assert_variant!(fullmac_req_stream.next().await,
        Some(Ok(fidl_fullmac::WlanFullmacImplBridgeRequest::StartScan { payload, responder })) => {
            assert_eq!(payload.scan_type.unwrap(), fidl_fullmac::WlanScanType::Passive);
            responder
                .send()
                .expect("Failed to respond to StartScan");
            payload.txn_id.expect("No txn_id found")
        });

        fullmac_ifc_proxy
            .on_scan_end(&fidl_fullmac::WlanFullmacScanEnd {
                txn_id,
                code: fidl_fullmac::WlanScanResult::NotSupported,
            })
            .await
            .expect("Failed to send on_scan_end");
    };

    let (scan_result, _) = futures::join!(client_fut, driver_fut);
    assert_eq!(scan_result.unwrap_err(), fidl_sme::ScanErrorCode::NotSupported);
}
