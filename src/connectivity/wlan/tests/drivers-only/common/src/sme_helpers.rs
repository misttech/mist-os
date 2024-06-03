// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
//
use fidl_fuchsia_wlan_sme as fidl_sme;

pub async fn get_client_sme(
    generic_sme_proxy: &fidl_sme::GenericSmeProxy,
) -> fidl_sme::ClientSmeProxy {
    let (client_sme_proxy, client_sme_server) =
        fidl::endpoints::create_proxy().expect("Failed to create client SME proxy");
    generic_sme_proxy
        .get_client_sme(client_sme_server)
        .await
        .expect("FIDL error")
        .expect("GetClientSme Error");
    client_sme_proxy
}

pub async fn get_telemetry(
    generic_sme_proxy: &fidl_sme::GenericSmeProxy,
) -> fidl_sme::TelemetryProxy {
    let (telemetry_proxy, telemetry_server) =
        fidl::endpoints::create_proxy().expect("Failed to create telemetry SME proxy");
    generic_sme_proxy
        .get_sme_telemetry(telemetry_server)
        .await
        .expect("FIDL error")
        .expect("GetTelemetry error");
    telemetry_proxy
}
