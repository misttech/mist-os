// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    fidl_fuchsia_wlan_common_security as fidl_wlan_security,
    fidl_fuchsia_wlan_fullmac as fidl_fullmac,
    fidl_fuchsia_wlan_mlme::EapolResultCode,
    futures::StreamExt,
    ieee80211::MacAddr,
    std::sync::{Arc, Mutex},
    wlan_common::{
        assert_variant, bss,
        ie::rsn::{cipher::CIPHER_CCMP_128, rsne},
    },
    wlan_rsn::{
        rsna::{SecAssocUpdate, UpdateSink},
        Authenticator,
    },
    zerocopy::IntoBytes,
};

/// Creates a WPA2 authenticator based on the given parameters.
pub fn create_wpa2_authenticator(
    client_mac_addr: MacAddr,
    bss_description: &bss::BssDescription,
    credentials: fidl_wlan_security::WpaCredentials,
) -> Authenticator {
    assert_eq!(bss_description.protection(), bss::Protection::Wpa2Personal);

    // It's assumed that advertised and supplicant protections are the same.
    let advertised_protection_info = get_protection_info(bss_description);
    let supplicant_protection_info = get_protection_info(bss_description);

    let nonce_rdr = wlan_rsn::nonce::NonceReader::new(&bss_description.bssid.clone().into())
        .expect("creating nonce reader");
    let gtk_provider =
        wlan_rsn::GtkProvider::new(CIPHER_CCMP_128, 1, 0).expect("creating gtk provider");

    let psk = match credentials {
        fidl_wlan_security::WpaCredentials::Passphrase(passphrase) => {
            wlan_rsn::psk::compute(passphrase.as_bytes(), &bss_description.ssid)
                .expect("Could not compute psk")
        }
        fidl_wlan_security::WpaCredentials::Psk(psk) => Box::new(psk),
        _ => panic!("Unsupported credential type"),
    };

    Authenticator::new_wpa2psk_ccmp128(
        nonce_rdr,
        Arc::new(Mutex::new(gtk_provider)),
        psk,
        client_mac_addr,
        supplicant_protection_info,
        bss_description.bssid.clone().into(),
        advertised_protection_info,
    )
    .expect("Failed to create authenticator")
}

/// Uses |fullmac_req_stream| and |fullmac_ifc_proxy| to perform an EAPOL handshake as an
/// authenticator.
/// This assumes that |authenticator| is already initiated.
/// |frame_to_client| is the first EAPOL frame that the authenticator sends.
///
/// Returns the UpdateSink of |authenticator|. By the end of a successful EAPOL handshake, the
/// UpdateSink should include all the keys for a successful EAPOL handshake.
/// Panics if the handshake fails for any reason.
pub async fn handle_fourway_eapol_handshake(
    authenticator: &mut Authenticator,
    frame_to_client: eapol::KeyFrameBuf,
    bssid: [u8; 6],
    client_sta_addr: [u8; 6],
    fullmac_req_stream: &mut fidl_fullmac::WlanFullmacImplBridgeRequestStream,
    fullmac_ifc_proxy: &fidl_fullmac::WlanFullmacImplIfcBridgeProxy,
) -> UpdateSink {
    let mut update_sink = UpdateSink::new();
    let mic_size = authenticator.get_negotiated_protection().mic_size;

    send_eapol_frame_to_test_realm(
        authenticator,
        frame_to_client,
        bssid.clone(),
        client_sta_addr.clone(),
        fullmac_ifc_proxy,
    )
    .await;
    let frame_to_auth_data = get_eapol_frame_from_test_realm(
        bssid.clone(),
        client_sta_addr.clone(),
        fullmac_req_stream,
        fullmac_ifc_proxy,
    )
    .await;
    let frame_to_auth = eapol::KeyFrameRx::parse(mic_size as usize, &frame_to_auth_data[..])
        .expect("Could not parse EAPOL key frame");
    authenticator
        .on_eapol_frame(&mut update_sink, eapol::Frame::Key(frame_to_auth))
        .expect("Could not send EAPOL frame to authenticator");

    let frame_to_client = assert_variant!(update_sink.remove(0), SecAssocUpdate::TxEapolKeyFrame { frame, .. } => frame);
    send_eapol_frame_to_test_realm(
        authenticator,
        frame_to_client,
        bssid.clone(),
        client_sta_addr.clone(),
        fullmac_ifc_proxy,
    )
    .await;
    let frame_to_auth_data = get_eapol_frame_from_test_realm(
        bssid.clone(),
        client_sta_addr.clone(),
        fullmac_req_stream,
        fullmac_ifc_proxy,
    )
    .await;
    let frame_to_auth = eapol::KeyFrameRx::parse(mic_size as usize, &frame_to_auth_data[..])
        .expect("Could not parse EAPOL key frame");
    authenticator
        .on_eapol_frame(&mut update_sink, eapol::Frame::Key(frame_to_auth))
        .expect("Could not send EAPOL frame to authenticator");

    update_sink
}

/// Sends an EAPOL |frame| to the test realm via |fullmac_ifc_proxy|.
async fn send_eapol_frame_to_test_realm(
    authenticator: &mut Authenticator,
    frame: eapol::KeyFrameBuf,
    authenticator_addr: [u8; 6],
    client_addr: [u8; 6],
    fullmac_ifc_proxy: &fidl_fullmac::WlanFullmacImplIfcBridgeProxy,
) {
    fullmac_ifc_proxy
        .eapol_ind(&fidl_fullmac::WlanFullmacEapolIndication {
            src_addr: authenticator_addr,
            dst_addr: client_addr,
            data: frame.into(),
        })
        .await
        .expect("Could not send EAPOL ind");
    let mut update_sink = UpdateSink::new();
    authenticator
        .on_eapol_conf(&mut update_sink, EapolResultCode::Success)
        .expect("Could not send EAPOL conf to authenticator");
    assert_eq!(update_sink.len(), 0);
}

/// Returns the buffer containing EAPOL frame data from the test realm through |fullmac_req_stream|.
async fn get_eapol_frame_from_test_realm(
    authenticator_addr: [u8; 6],
    client_addr: [u8; 6],
    fullmac_req_stream: &mut fidl_fullmac::WlanFullmacImplBridgeRequestStream,
    fullmac_ifc_proxy: &fidl_fullmac::WlanFullmacImplIfcBridgeProxy,
) -> Vec<u8> {
    let frame_data = assert_variant!(fullmac_req_stream.next().await,
    Some(Ok(fidl_fullmac::WlanFullmacImplBridgeRequest::EapolTx { payload, responder })) => {
        responder
            .send()
            .expect("Failed to respond to EapolTx");

        assert!(payload.src_addr.unwrap() == client_addr && payload.dst_addr.unwrap() == authenticator_addr);
        payload.data.unwrap()
    });

    fullmac_ifc_proxy
        .eapol_conf(&fidl_fullmac::WlanFullmacEapolConfirm {
            result_code: fidl_fullmac::WlanEapolResult::Success,
            dst_addr: authenticator_addr,
        })
        .await
        .expect("Could not send EAPOL conf");

    frame_data
}

fn get_protection_info(bss_description: &bss::BssDescription) -> wlan_rsn::ProtectionInfo {
    let (_, rsne) = rsne::from_bytes(bss_description.rsne().unwrap()).expect("Could not get RSNE");
    wlan_rsn::ProtectionInfo::Rsne(rsne)
}
