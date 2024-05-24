// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::FullmacDriverFixture,
    fidl_fuchsia_wlan_common as fidl_common, fidl_fuchsia_wlan_fullmac as fidl_fullmac,
    fullmac_helpers::config::{default_fullmac_query_info, FullmacDriverConfig},
    rand::seq::SliceRandom,
    wlan_common::assert_variant,
};

#[fuchsia::test]
async fn test_generic_sme_query() {
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

    let (mut fullmac_driver, generic_sme_proxy) =
        FullmacDriverFixture::create_and_get_generic_sme(config).await;

    // Returns the query response
    let sme_fut = async { generic_sme_proxy.query().await.expect("Failed to request SME query") };

    let driver_fut = async {
        assert_variant!(fullmac_driver.request_stream.next().await,
        fidl_fullmac::WlanFullmacImplBridgeRequest::Query { responder } => {
            responder.send(Ok(&fullmac_driver.config.query_info))
                .expect("Failed to respondt to Query");
        });
    };

    let (query_resp, _) = futures::join!(sme_fut, driver_fut);
    assert_eq!(query_resp.role, fullmac_driver.config.query_info.role);
    assert_eq!(query_resp.sta_addr, fullmac_driver.sta_addr());
}
