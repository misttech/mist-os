// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    fidl::endpoints::{create_endpoints, create_proxy},
    fidl_fuchsia_wlan_fullmac as fidl_fullmac, fidl_fuchsia_wlan_sme as fidl_sme,
    fidl_test_wlan_testcontroller as fidl_testcontroller,
    futures::StreamExt,
    wlan_common::assert_variant,
};

pub mod config;

/// Creates and starts fullmac driver using |testcontroller_proxy|.
/// This handles the request to start SME through the UsmeBootstrap protocol,
/// and the sequence of query requests that SME makes to the fullmac driver on startup.
///
/// After this function is called, the fullmac driver is ready to be used in the test suite.
pub async fn create_fullmac_driver(
    testcontroller_proxy: &fidl_testcontroller::TestControllerProxy,
    config: &config::FullmacDriverConfig,
) -> (
    fidl_fullmac::WlanFullmacImplBridgeRequestStream,
    fidl_fullmac::WlanFullmacImplIfcBridgeProxy,
    fidl_sme::GenericSmeProxy,
) {
    let (fullmac_bridge_client, fullmac_bridge_server) = create_endpoints();

    testcontroller_proxy
        .create_fullmac(fullmac_bridge_client)
        .await
        .expect("FIDL error on create_fullmac")
        .expect("TestController returned an error on create fullmac");

    let mut fullmac_bridge_stream =
        fullmac_bridge_server.into_stream().expect("Could not create stream");

    // Fullmac MLME queries driver before starting
    assert_variant!(fullmac_bridge_stream.next().await,
        Some(Ok(fidl_fullmac::WlanFullmacImplBridgeRequest::QueryMacSublayerSupport { responder })) => {
            responder
                .send(Ok(&config.mac_sublayer_support))
                .expect("Failed to respond to QueryMacSublayerSupport");
        }
    );

    let (usme_bootstrap_proxy, usme_bootstrap_server) =
        create_proxy::<fidl_sme::UsmeBootstrapMarker>()
            .expect("Could not craete usme_bootstrap proxy");

    let fullmac_ifc_proxy = assert_variant!(fullmac_bridge_stream.next().await,
        Some(Ok(fidl_fullmac::WlanFullmacImplBridgeRequest::Start { ifc, responder })) => {
            responder
                .send(Ok(usme_bootstrap_server.into_channel()))
                .expect("Failed to respond to Start");
            ifc.into_proxy().expect("Could not turn fullmac_ifc_channel into proxy")
        }
    );

    let (generic_sme_proxy, generic_sme_server) =
        create_proxy::<fidl_sme::GenericSmeMarker>().expect("Failed to create generic_sme_proxy");

    let _bootstrap_result = usme_bootstrap_proxy
        .start(generic_sme_server, &config.sme_legacy_privacy_support)
        .await
        .expect("Failed to call usme_bootstrap.start");

    assert_variant!(fullmac_bridge_stream.next().await,
        Some(Ok(fidl_fullmac::WlanFullmacImplBridgeRequest::Query { responder })) => {
            responder
                .send(Ok(&config.query_info))
                .expect("Failed to respond to Query");
        }
    );

    assert_variant!(fullmac_bridge_stream.next().await,
        Some(Ok(fidl_fullmac::WlanFullmacImplBridgeRequest::QueryMacSublayerSupport {
            responder,
        })) => {
            responder
                .send(Ok(&config.mac_sublayer_support))
                .expect("Failed to respond to QueryMacSublayerSupport");
        }
    );

    assert_variant!(fullmac_bridge_stream.next().await,
        Some(Ok(fidl_fullmac::WlanFullmacImplBridgeRequest::QuerySecuritySupport {
            responder,
        })) => {
            responder
                .send(Ok(&config.security_support))
                .expect("Failed to respond to QuerySecuritySupport");
        }
    );

    assert_variant!(fullmac_bridge_stream.next().await,
        Some(Ok(fidl_fullmac::WlanFullmacImplBridgeRequest::QuerySpectrumManagementSupport {
                responder,
        })) => {
            responder
                .send(Ok(&config.spectrum_management_support))
                .expect("Failed to respond to QuerySpectrumManagementSupport");
        }
    );

    assert_variant!(fullmac_bridge_stream.next().await,
        Some(Ok(fidl_fullmac::WlanFullmacImplBridgeRequest::Query { responder })) => {
            responder
                .send(Ok(&config.query_info))
                .expect("Failed to respond to Query");
        }
    );

    (fullmac_bridge_stream, fullmac_ifc_proxy, generic_sme_proxy)
}
