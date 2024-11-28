// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
//
use lazy_static::lazy_static;
use rand::distributions::Alphanumeric;
use rand::Rng;
use {
    fidl_fuchsia_wlan_common as fidl_common, fidl_fuchsia_wlan_ieee80211 as fidl_ieee80211,
    fidl_fuchsia_wlan_sme as fidl_sme,
};

lazy_static! {
    pub static ref DEFAULT_OPEN_AP_CONFIG: fidl_sme::ApConfig = fidl_sme::ApConfig {
        ssid: random_ssid(),
        password: vec![],
        radio_cfg: fidl_sme::RadioConfig {
            phy: fidl_common::WlanPhyType::Ofdm,
            channel: fidl_common::WlanChannel {
                primary: 1,
                cbw: fidl_common::ChannelBandwidth::Cbw20,
                secondary80: 0,
            },
        },
    };
}

pub async fn get_client_sme(
    generic_sme_proxy: &fidl_sme::GenericSmeProxy,
) -> fidl_sme::ClientSmeProxy {
    let (client_sme_proxy, client_sme_server) = fidl::endpoints::create_proxy();
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
    let (telemetry_proxy, telemetry_server) = fidl::endpoints::create_proxy();
    generic_sme_proxy
        .get_sme_telemetry(telemetry_server)
        .await
        .expect("FIDL error")
        .expect("GetTelemetry error");
    telemetry_proxy
}

pub async fn get_ap_sme(generic_sme_proxy: &fidl_sme::GenericSmeProxy) -> fidl_sme::ApSmeProxy {
    let (ap_sme_proxy, ap_sme_server) = fidl::endpoints::create_proxy();
    generic_sme_proxy.get_ap_sme(ap_sme_server).await.expect("FIDL error").expect("GetApSme Error");
    ap_sme_proxy
}

pub fn random_string_as_bytes(len: usize) -> Vec<u8> {
    rand::thread_rng().sample_iter(&Alphanumeric).take(len).collect()
}

pub fn random_ssid() -> Vec<u8> {
    let ssid_len = rand::thread_rng().gen_range(1..=fidl_ieee80211::MAX_SSID_BYTE_LEN as usize);
    random_string_as_bytes(ssid_len)
}

pub fn random_password() -> Vec<u8> {
    let pw_len = rand::thread_rng().gen_range(8..=63);
    random_string_as_bytes(pw_len)
}
