// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    drivers_only_common::DriversOnlyTestRealm,
    fidl_fuchsia_wlan_common as fidl_common, fidl_fuchsia_wlan_fullmac as fidl_fullmac,
    fullmac_helpers::config::{default_fullmac_query_info, FullmacDriverConfig},
    futures::StreamExt,
    rand::seq::SliceRandom,
    wlan_common::assert_variant,
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
