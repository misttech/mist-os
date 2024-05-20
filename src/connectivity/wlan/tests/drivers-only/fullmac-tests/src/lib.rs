// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    drivers_only_common::{sme_helpers, DriversOnlyTestRealm},
    fidl_fuchsia_wlan_common as fidl_common,
    fidl_fuchsia_wlan_common_security as fidl_wlan_security,
    fidl_fuchsia_wlan_fullmac as fidl_fullmac, fidl_fuchsia_wlan_ieee80211 as fidl_ieee80211,
    fidl_fuchsia_wlan_sme as fidl_sme, fuchsia_zircon as zx,
    fullmac_helpers::{
        config::{default_fullmac_query_info, FullmacDriverConfig},
        COMPATIBLE_OPEN_BSS, COMPATIBLE_WPA2_BSS,
    },
    futures::StreamExt,
    ieee80211::{MacAddr, MacAddrBytes},
    rand::{seq::SliceRandom, Rng},
    wlan_common::{assert_variant, random_fidl_bss_description},
    wlan_rsn::{
        key::{exchange::Key, Tk},
        rsna::{SecAssocStatus, SecAssocUpdate, UpdateSink},
    },
};

/// Fixture that holds all the relevant data and proxies for the fullmac driver.
/// This can be shared among the different types of tests (client, telemetry, AP).
struct FullmacDriverFixture {
    config: FullmacDriverConfig,
    ifc_proxy: fidl_fullmac::WlanFullmacImplIfcBridgeProxy,
    request_stream: fidl_fullmac::WlanFullmacImplBridgeRequestStream,
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
            request_stream: fullmac_req_stream,
            _realm: realm,
        };

        (fixture, generic_sme_proxy)
    }

    fn sta_addr(&self) -> [u8; 6] {
        self.config.query_info.sta_addr
    }
}

#[fuchsia::test]
async fn test_sme_query() {
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
            Some(Ok(fidl_fullmac::WlanFullmacImplBridgeRequest::Query { responder })) => {
            responder.send(Ok(&fullmac_driver.config.query_info))
                .expect("Failed to respondt to Query");
        });
    };

    let (query_resp, _) = futures::join!(sme_fut, driver_fut);
    assert_eq!(query_resp.role, fullmac_driver.config.query_info.role);
    assert_eq!(query_resp.sta_addr, fullmac_driver.sta_addr());
}

#[fuchsia::test]
async fn test_scan_request_success() {
    let (mut fullmac_driver, generic_sme_proxy) =
        FullmacDriverFixture::create_and_get_generic_sme(FullmacDriverConfig {
            ..Default::default()
        })
        .await;
    let client_sme_proxy = sme_helpers::get_client_sme(&generic_sme_proxy).await;

    let client_fut = async {
        client_sme_proxy
            .scan(&fidl_sme::ScanRequest::Passive(fidl_sme::PassiveScanRequest {}))
            .await
            .expect("FIDL error")
            .expect("ScanRequest error")
    };

    let driver_fut = async {
        let txn_id = assert_variant!(fullmac_driver.request_stream.next().await,
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
            fullmac_driver
                .ifc_proxy
                .on_scan_result(&scan_result)
                .await
                .expect("Failed to send on_scan_result");
        }

        fullmac_driver
            .ifc_proxy
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
    let (mut fullmac_driver, generic_sme_proxy) =
        FullmacDriverFixture::create_and_get_generic_sme(FullmacDriverConfig {
            ..Default::default()
        })
        .await;
    let client_sme_proxy = sme_helpers::get_client_sme(&generic_sme_proxy).await;

    let client_fut = async {
        client_sme_proxy
            .scan(&fidl_sme::ScanRequest::Passive(fidl_sme::PassiveScanRequest {}))
            .await
            .expect("FIDL error")
    };

    let driver_fut = async {
        let txn_id = assert_variant!(fullmac_driver.request_stream.next().await,
        Some(Ok(fidl_fullmac::WlanFullmacImplBridgeRequest::StartScan { payload, responder })) => {
            assert_eq!(payload.scan_type.unwrap(), fidl_fullmac::WlanScanType::Passive);
            responder
                .send()
                .expect("Failed to respond to StartScan");
            payload.txn_id.expect("No txn_id found")
        });

        fullmac_driver
            .ifc_proxy
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

#[fuchsia::test]
async fn test_open_connect_request_success() {
    let (mut fullmac_driver, generic_sme_proxy) =
        FullmacDriverFixture::create_and_get_generic_sme(FullmacDriverConfig {
            ..Default::default()
        })
        .await;
    let client_sme_proxy = sme_helpers::get_client_sme(&generic_sme_proxy).await;

    let client_fut = async {
        let (connect_txn, connect_txn_server) =
            fidl::endpoints::create_proxy::<fidl_sme::ConnectTransactionMarker>().unwrap();
        let mut connect_txn_event_stream = connect_txn.take_event_stream();

        let connect_req = fidl_sme::ConnectRequest {
            ssid: COMPATIBLE_OPEN_BSS.ssid.clone().into(),
            bss_description: COMPATIBLE_OPEN_BSS.clone().into(),
            multiple_bss_candidates: false,
            authentication: fidl_wlan_security::Authentication {
                protocol: fidl_wlan_security::Protocol::Open,
                credentials: None,
            },

            // Note: this field has no effect for fullmac drivers.
            deprecated_scan_type: fidl_common::ScanType::Passive,
        };

        client_sme_proxy
            .connect(&connect_req, Some(connect_txn_server))
            .expect("Connect FIDL error.");

        let connect_txn_event = connect_txn_event_stream
            .next()
            .await
            .expect("Connect event stream FIDL error")
            .expect("Connect txn returned error");

        // Returns the Connect result code.
        assert_variant!(connect_txn_event,
            fidl_sme::ConnectTransactionEvent::OnConnectResult { result } => {
                result
            }
        )
    };

    let driver_fut = async {
        let connect_req = assert_variant!(fullmac_driver.request_stream.next().await,
        Some(Ok(fidl_fullmac::WlanFullmacImplBridgeRequest::Connect { payload, responder })) => {
            responder
                .send()
                .expect("Failed to respond to Connect");
             payload
        });

        fullmac_driver
            .ifc_proxy
            .connect_conf(&fidl_fullmac::WlanFullmacConnectConfirm {
                peer_sta_address: COMPATIBLE_OPEN_BSS.bssid.to_array(),
                result_code: fidl_ieee80211::StatusCode::Success,
                association_id: 0,
                association_ies: vec![],
            })
            .await
            .expect("Failed to send ConnectConf");

        let online = assert_variant!(fullmac_driver.request_stream.next().await,
        Some(Ok(fidl_fullmac::WlanFullmacImplBridgeRequest::OnLinkStateChanged { online, responder })) => {
            responder
                .send()
                .expect("Failed to respond to OnLinkStateChanged");
            online
        });

        (connect_req, online)
    };

    let (connect_result, (driver_connect_req, driver_online)) =
        futures::join!(client_fut, driver_fut);
    assert_eq!(connect_result.code, fidl_ieee80211::StatusCode::Success);
    assert!(driver_online);

    assert_eq!(driver_connect_req.selected_bss.unwrap(), COMPATIBLE_OPEN_BSS.clone().into());
    assert_eq!(driver_connect_req.connect_failure_timeout.unwrap(), 60);
    assert_eq!(driver_connect_req.auth_type.unwrap(), fidl_fullmac::WlanAuthType::OpenSystem);

    // TODO(https://fxbug.dev/337074689): Check that these are None instead of empty vectors.
    assert_eq!(driver_connect_req.sae_password.unwrap(), vec![]);
    assert_eq!(driver_connect_req.security_ie.unwrap(), vec![]);
}

#[fuchsia::test]
async fn test_open_connect_request_error() {
    let (mut fullmac_driver, generic_sme_proxy) =
        FullmacDriverFixture::create_and_get_generic_sme(FullmacDriverConfig {
            ..Default::default()
        })
        .await;
    let client_sme_proxy = sme_helpers::get_client_sme(&generic_sme_proxy).await;

    let client_fut = async {
        let (connect_txn, connect_txn_server) =
            fidl::endpoints::create_proxy::<fidl_sme::ConnectTransactionMarker>().unwrap();
        let mut connect_txn_event_stream = connect_txn.take_event_stream();

        let connect_req = fidl_sme::ConnectRequest {
            ssid: COMPATIBLE_OPEN_BSS.ssid.clone().into(),
            bss_description: COMPATIBLE_OPEN_BSS.clone().into(),
            multiple_bss_candidates: false,
            authentication: fidl_wlan_security::Authentication {
                protocol: fidl_wlan_security::Protocol::Open,
                credentials: None,
            },

            // Note: this field has no effect for fullmac drivers.
            deprecated_scan_type: fidl_common::ScanType::Passive,
        };

        client_sme_proxy
            .connect(&connect_req, Some(connect_txn_server))
            .expect("Connect FIDL error.");

        let connect_txn_event = connect_txn_event_stream
            .next()
            .await
            .expect("Connect event stream FIDL error")
            .expect("Connect txn returned error");

        // Returns the Connect result code.
        assert_variant!(connect_txn_event,
            fidl_sme::ConnectTransactionEvent::OnConnectResult { result } => {
                result
            }
        )
    };

    let driver_fut = async {
        // The driver responds to the initial Connect request after it sends a failed ConnectConf.
        let connect_req_responder = assert_variant!(fullmac_driver.request_stream.next().await,
        Some(Ok(fidl_fullmac::WlanFullmacImplBridgeRequest::Connect { payload: _, responder })) => {
            responder
        });

        fullmac_driver
            .ifc_proxy
            .connect_conf(&fidl_fullmac::WlanFullmacConnectConfirm {
                peer_sta_address: COMPATIBLE_OPEN_BSS.bssid.to_array(),
                result_code: fidl_ieee80211::StatusCode::RefusedReasonUnspecified,
                association_id: 0,
                association_ies: vec![],
            })
            .await
            .expect("Failed to send ConnectConf");

        connect_req_responder.send().expect("Failed to respond to connect req");

        let deauth_req = assert_variant!(fullmac_driver.request_stream.next().await,
        Some(Ok(fidl_fullmac::WlanFullmacImplBridgeRequest::Deauth { payload, responder })) => {
            responder
                .send()
                .expect("Failed to respond to Deauth");
            payload
        });

        deauth_req
    };

    let (connect_result, driver_deauth_req) = futures::join!(client_fut, driver_fut);
    assert_eq!(connect_result.code, fidl_ieee80211::StatusCode::RefusedReasonUnspecified);
    assert_eq!(driver_deauth_req.reason_code.unwrap(), fidl_ieee80211::ReasonCode::StaLeaving);
    assert_eq!(driver_deauth_req.peer_sta_address.unwrap(), COMPATIBLE_OPEN_BSS.bssid.to_array());
}

#[fuchsia::test]
async fn test_wpa2_connect_request_success() {
    let (mut fullmac_driver, generic_sme_proxy) =
        FullmacDriverFixture::create_and_get_generic_sme(FullmacDriverConfig {
            ..Default::default()
        })
        .await;
    let client_sme_proxy = sme_helpers::get_client_sme(&generic_sme_proxy).await;

    let credentials = fidl_wlan_security::WpaCredentials::Passphrase(vec![8, 7, 6, 5, 4, 3, 2, 1]);

    let client_fut = async {
        let (connect_txn, connect_txn_server) =
            fidl::endpoints::create_proxy::<fidl_sme::ConnectTransactionMarker>().unwrap();
        let mut connect_txn_event_stream = connect_txn.take_event_stream();

        let connect_req = fidl_sme::ConnectRequest {
            ssid: COMPATIBLE_WPA2_BSS.ssid.clone().into(),
            bss_description: COMPATIBLE_WPA2_BSS.clone().into(),
            multiple_bss_candidates: false,
            authentication: fidl_wlan_security::Authentication {
                protocol: fidl_wlan_security::Protocol::Wpa2Personal,
                credentials: Some(Box::new(fidl_wlan_security::Credentials::Wpa(
                    credentials.clone(),
                ))),
            },
            // Note: this field has no effect for fullmac drivers.
            deprecated_scan_type: fidl_common::ScanType::Passive,
        };

        client_sme_proxy
            .connect(&connect_req, Some(connect_txn_server))
            .expect("Connect FIDL error.");

        let connect_txn_event = connect_txn_event_stream
            .next()
            .await
            .expect("Connect event stream FIDL error")
            .expect("Connect txn returned error");

        // Returns the Connect result code.
        assert_variant!(connect_txn_event,
            fidl_sme::ConnectTransactionEvent::OnConnectResult { result } => {
                result
            }
        )
    };

    let driver_fut = async {
        let connect_req = assert_variant!(fullmac_driver.request_stream.next().await,
        Some(Ok(fidl_fullmac::WlanFullmacImplBridgeRequest::Connect { payload, responder })) => {
            responder
                .send()
                .expect("Failed to respond to Connect");
             payload
        });

        fullmac_driver
            .ifc_proxy
            .connect_conf(&fidl_fullmac::WlanFullmacConnectConfirm {
                peer_sta_address: COMPATIBLE_WPA2_BSS.bssid.to_array(),
                result_code: fidl_ieee80211::StatusCode::Success,
                association_id: 0,
                association_ies: vec![],
            })
            .await
            .expect("Failed to send ConnectConf");

        let mut authenticator = fullmac_helpers::fake_ap::create_wpa2_authenticator(
            MacAddr::from(fullmac_driver.sta_addr()),
            &COMPATIBLE_WPA2_BSS,
            credentials.clone(),
        );

        let initial_eapol_frame = {
            let mut update_sink = UpdateSink::new();
            authenticator.initiate(&mut update_sink).expect("Could not initiate authenticator");
            assert_variant!(
                update_sink.remove(0),
                SecAssocUpdate::Status(SecAssocStatus::PmkSaEstablished)
            );
            assert_variant!(update_sink.remove(0), SecAssocUpdate::TxEapolKeyFrame { frame, .. } => frame)
        };

        let update_sink = fullmac_helpers::fake_ap::handle_fourway_eapol_handshake(
            &mut authenticator,
            initial_eapol_frame,
            COMPATIBLE_WPA2_BSS.bssid.to_array(),
            fullmac_driver.sta_addr(),
            &mut fullmac_driver.request_stream,
            &fullmac_driver.ifc_proxy,
        )
        .await;

        let driver_ptk_req = assert_variant!(fullmac_driver.request_stream.next().await,
        Some(Ok(fidl_fullmac::WlanFullmacImplBridgeRequest::SetKeysReq { req, responder })) => {
            responder.send(&fidl_fullmac::WlanFullmacSetKeysResp {
                num_keys: req.num_keys,
                statuslist: [zx::sys::ZX_OK; fidl_fullmac::WLAN_MAX_KEYLIST_SIZE as usize],
            }).expect("Failed to respond to SetKeys");
            req
        });

        let driver_gtk_req = assert_variant!(fullmac_driver.request_stream.next().await,
        Some(Ok(fidl_fullmac::WlanFullmacImplBridgeRequest::SetKeysReq { req, responder })) => {
            responder.send(&fidl_fullmac::WlanFullmacSetKeysResp {
                num_keys: req.num_keys,
                statuslist: [zx::sys::ZX_OK; fidl_fullmac::WLAN_MAX_KEYLIST_SIZE as usize],
            }).expect("Failed to respond to SetKeys");
            req
        });

        let online = assert_variant!(fullmac_driver.request_stream.next().await,
        Some(Ok(fidl_fullmac::WlanFullmacImplBridgeRequest::OnLinkStateChanged { online, responder })) => {
            responder
                .send()
                .expect("Failed to respond to OnLinkStateChanged");
            online
        });

        (connect_req, update_sink, driver_ptk_req, driver_gtk_req, online)
    };

    let (
        connect_result,
        (driver_connect_req, auth_update_sink, driver_ptk_req, driver_gtk_req, driver_online),
    ) = futures::join!(client_fut, driver_fut);
    assert_eq!(connect_result.code, fidl_ieee80211::StatusCode::Success);
    assert!(driver_online);

    assert_eq!(driver_connect_req.security_ie.unwrap(), COMPATIBLE_WPA2_BSS.rsne().unwrap());
    assert_eq!(driver_connect_req.selected_bss.unwrap(), COMPATIBLE_WPA2_BSS.clone().into());
    assert_eq!(driver_connect_req.connect_failure_timeout.unwrap(), 60);
    assert_eq!(driver_connect_req.auth_type.unwrap(), fidl_fullmac::WlanAuthType::OpenSystem);

    // TODO(https://fxbug.dev/337074689): Check that these are None instead of empty vectors.
    assert_eq!(driver_connect_req.sae_password.unwrap(), vec![]);

    // Check that PTK received by driver matches the authenticator's PTK
    assert_eq!(driver_ptk_req.num_keys, 1);
    let driver_ptk = &driver_ptk_req.keylist[0];
    assert_eq!(driver_ptk.key_type.unwrap(), fidl_common::WlanKeyType::Pairwise);
    assert_eq!(driver_ptk.key_idx.unwrap(), 0);
    assert_eq!(driver_ptk.peer_addr.unwrap(), COMPATIBLE_WPA2_BSS.bssid.to_array());
    assert_eq!(driver_ptk.cipher_type.unwrap(), fidl_ieee80211::CipherSuiteType::Ccmp128);

    let auth_ptk =
        assert_variant!(&auth_update_sink[0], SecAssocUpdate::Key(Key::Ptk(ptk)) => ptk.clone());
    assert_eq!(driver_ptk.cipher_oui.unwrap(), *auth_ptk.cipher.oui);
    assert_eq!(*driver_ptk.key.as_ref().unwrap(), auth_ptk.tk());

    // Check that GTK received by driver matches the authenticator's GTK
    assert_eq!(driver_gtk_req.num_keys, 1);
    let driver_gtk = &driver_gtk_req.keylist[0];
    assert_eq!(driver_gtk.protection.unwrap(), fidl_common::WlanProtection::RxTx);
    assert_eq!(driver_gtk.key_type.unwrap(), fidl_common::WlanKeyType::Group);
    assert_eq!(driver_gtk.peer_addr.unwrap(), ieee80211::BROADCAST_ADDR.to_array());
    assert_eq!(driver_gtk.cipher_type.unwrap(), fidl_ieee80211::CipherSuiteType::Ccmp128);

    let auth_gtk =
        assert_variant!(&auth_update_sink[1], SecAssocUpdate::Key(Key::Gtk(gtk)) => gtk.clone());
    assert_eq!(driver_gtk.key_idx.unwrap(), auth_gtk.key_id());
    assert_eq!(driver_gtk.cipher_oui.unwrap(), *auth_gtk.cipher().oui);
    assert_eq!(*driver_gtk.key.as_ref().unwrap(), auth_gtk.tk());
}
