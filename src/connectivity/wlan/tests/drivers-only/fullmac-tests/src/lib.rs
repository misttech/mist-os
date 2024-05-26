// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    drivers_only_common::DriversOnlyTestRealm,
    fidl_fuchsia_wlan_fullmac as fidl_fullmac, fidl_fuchsia_wlan_sme as fidl_sme,
    fullmac_helpers::{
        config::FullmacDriverConfig, recorded_request_stream::RecordedRequestStream,
    },
};

mod client;
mod query;

/// Fixture that holds all the relevant data and proxies for the fullmac driver.
/// This can be shared among the different types of tests (client, telemetry, AP).
struct FullmacDriverFixture {
    config: FullmacDriverConfig,
    ifc_proxy: fidl_fullmac::WlanFullmacImplIfcBridgeProxy,
    request_stream: RecordedRequestStream,
    _realm: DriversOnlyTestRealm,
}

impl FullmacDriverFixture {
    async fn create_and_get_generic_sme(
        config: FullmacDriverConfig,
    ) -> (Self, fidl_sme::GenericSmeProxy) {
        let realm = DriversOnlyTestRealm::new().await;
        let (fullmac_req_stream, fullmac_ifc_proxy, generic_sme_proxy) =
            fullmac_helpers::create_fullmac_driver(&realm.testcontroller_proxy, &config).await;

        let fixture = Self {
            config,
            ifc_proxy: fullmac_ifc_proxy,
            request_stream: RecordedRequestStream::new(fullmac_req_stream),
            _realm: realm,
        };

        (fixture, generic_sme_proxy)
    }

    fn sta_addr(&self) -> [u8; 6] {
        self.config.query_info.sta_addr
    }
}
