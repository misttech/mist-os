// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    drivers_only_common::DriversOnlyTestRealm,
    fidl_fuchsia_wlan_fullmac as fidl_fullmac, fidl_fuchsia_wlan_sme as fidl_sme,
    fidl_test_wlan_testcontroller as fidl_testcontroller, fuchsia_zircon as zx,
    fullmac_helpers::{
        config::FullmacDriverConfig, recorded_request_stream::RecordedRequestStream,
    },
};

mod ap;
mod client;
mod query;
mod telemetry;

/// Fixture that holds all the relevant data and proxies for the fullmac driver.
/// This can be shared among the different types of tests (client, telemetry, AP).
///
/// Note that wlanif considers it an error for |generic_sme_proxy| to be closed before wlanif is
/// torn down. To avoid that, |FullmacDriverFixture| contains |generic_sme_proxy| to ensure that it
/// outlives wlanif.
struct FullmacDriverFixture {
    id: fidl_testcontroller::FullmacId,
    config: FullmacDriverConfig,
    ifc_proxy: fidl_fullmac::WlanFullmacImplIfcBridgeProxy,
    request_stream: RecordedRequestStream,
    generic_sme_proxy: fidl_sme::GenericSmeProxy,
    realm: DriversOnlyTestRealm,
}

impl FullmacDriverFixture {
    async fn create(config: FullmacDriverConfig) -> Self {
        let realm = DriversOnlyTestRealm::new().await;
        let (id, fullmac_req_stream, fullmac_ifc_proxy, generic_sme_proxy) =
            fullmac_helpers::create_fullmac_driver(realm.testcontroller_proxy(), &config).await;
        tracing::info!("Created fullmac driver {:?}", id);
        Self {
            id,
            config,
            ifc_proxy: fullmac_ifc_proxy,
            generic_sme_proxy,
            request_stream: RecordedRequestStream::new(fullmac_req_stream),
            realm,
        }
    }

    fn sta_addr(&self) -> [u8; 6] {
        self.config.query_info.sta_addr
    }
}

impl Drop for FullmacDriverFixture {
    fn drop(&mut self) {
        tracing::info!("FullmacDriverFixture deleting fullmac driver {:?}...", self.id);
        let testcontroller_sync_proxy = self.realm.take_sync_testcontroller_proxy();
        testcontroller_sync_proxy
            .delete_fullmac(self.id, zx::Time::INFINITE)
            .expect("FIDL error when calling DeleteFullmac")
            .expect("TestController error when calling DeleteFullmac");
    }
}
