// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::client::roaming::lib::{RoamingPolicy, ROAMING_CHANNEL_BUFFER_SIZE};
use crate::client::roaming::local_roam_manager::{serve_local_roam_manager_requests, RoamManager};
use crate::client::{connection_selection, scan, serve_provider_requests, types};
use crate::config_management::{SavedNetworksManager, SavedNetworksManagerApi};
use crate::legacy;
use crate::mode_management::iface_manager_api::IfaceManagerApi;
use crate::mode_management::phy_manager::{PhyManager, PhyManagerApi};
use crate::mode_management::{create_iface_manager, device_monitor, recovery, DEFECT_CHANNEL_SIZE};
use crate::telemetry::{TelemetryEvent, TelemetrySender};
use crate::util::listener;
use crate::util::testing::{create_inspect_persistence_channel, run_while};
use anyhow::{format_err, Error};
use fidl::endpoints::{create_proxy, create_request_stream};
use fidl_fuchsia_wlan_device_service::DeviceWatcherEvent;
use fuchsia_async::{self as fasync, TestExecutor};
use fuchsia_inspect::{self as inspect};
use futures::channel::mpsc;
use futures::future::{join_all, JoinAll};
use futures::lock::Mutex;
use futures::prelude::*;
use futures::stream::StreamExt;
use futures::task::Poll;
use lazy_static::lazy_static;
use std::convert::Infallible;
use std::pin::{pin, Pin};
use std::rc::Rc;
use std::sync::Arc;
use test_case::test_case;
use tracing::{debug, info};
use wlan_common::scan::write_vmo;
use wlan_common::test_utils::ExpectWithin;
use wlan_common::{assert_variant, random_fidl_bss_description};
use {
    fidl_fuchsia_wlan_common as fidl_common,
    fidl_fuchsia_wlan_common_security as fidl_common_security,
    fidl_fuchsia_wlan_ieee80211 as fidl_ieee80211, fidl_fuchsia_wlan_policy as fidl_policy,
    fidl_fuchsia_wlan_sme as fidl_sme, hex,
};

pub const TEST_CLIENT_IFACE_ID: u16 = 42;
pub const TEST_PHY_ID: u16 = 41;
const RECOVERY_PROFILE_EMPTY_STRING: &str = "";
const RECOVERY_PROFILE_THRESHOLDED_RECOVERY: &str = "thresholded_recovery";
lazy_static! {
    pub static ref TEST_SSID: types::Ssid = types::Ssid::try_from("test_ssid").unwrap();
}

#[derive(Clone)]
pub struct TestCredentials {
    policy: fidl_policy::Credential,
    sme: Option<Box<fidl_common_security::Credentials>>,
}
pub struct TestCredentialVariants {
    pub none: TestCredentials,
    pub wep_64_hex: TestCredentials,
    pub wep_64_ascii: TestCredentials,
    pub wep_128_hex: TestCredentials,
    pub wep_128_ascii: TestCredentials,
    pub wpa_pass_min: TestCredentials,
    pub wpa_pass_max: TestCredentials,
    pub wpa_psk: TestCredentials,
}

lazy_static! {
    pub static ref TEST_CREDS: TestCredentialVariants = TestCredentialVariants {
        none: TestCredentials {
            policy: fidl_policy::Credential::None(fidl_policy::Empty),
            sme: None
        },
        wep_64_hex: TestCredentials {
            policy: fidl_policy::Credential::Password(b"7465737431".to_vec()),
            sme: Some(Box::new(fidl_common_security::Credentials::Wep(
                fidl_common_security::WepCredentials { key: b"test1".to_vec() }
            )))
        },
        wep_64_ascii: TestCredentials {
            policy: fidl_policy::Credential::Password(b"test1".to_vec()),
            sme: Some(Box::new(fidl_common_security::Credentials::Wep(
                fidl_common_security::WepCredentials { key: b"test1".to_vec() }
            )))
        },
        wep_128_hex: TestCredentials {
            policy: fidl_policy::Credential::Password(b"74657374317465737432333435".to_vec()),
            sme: Some(Box::new(fidl_common_security::Credentials::Wep(
                fidl_common_security::WepCredentials { key: b"test1test2345".to_vec() }
            )))
        },
        wep_128_ascii: TestCredentials {
            policy: fidl_policy::Credential::Password(b"test1test2345".to_vec()),
            sme: Some(Box::new(fidl_common_security::Credentials::Wep(
                fidl_common_security::WepCredentials { key: b"test1test2345".to_vec() }
            )))
        },
        wpa_pass_min: TestCredentials {
            policy: fidl_policy::Credential::Password(b"eight111".to_vec()),
            sme: Some(Box::new(fidl_common_security::Credentials::Wpa(
                fidl_common_security::WpaCredentials::Passphrase(b"eight111".to_vec())
            )))
        },
        wpa_pass_max: TestCredentials {
            policy: fidl_policy::Credential::Password(
                b"thisIs63CharactersLong!!!#$#%thisIs63CharactersLong!!!#$#%00009".to_vec()
            ),
            sme: Some(Box::new(fidl_common_security::Credentials::Wpa(
                fidl_common_security::WpaCredentials::Passphrase(
                    b"thisIs63CharactersLong!!!#$#%thisIs63CharactersLong!!!#$#%00009".to_vec()
                )
            )))
        },
        wpa_psk: TestCredentials {
            policy: fidl_policy::Credential::Psk(
                hex::decode(b"f10aedbb0ea29c928b06997ed305a697706ddad220ff5a98f252558a470a748f")
                    .unwrap()
            ),
            sme: Some(Box::new(fidl_common_security::Credentials::Wpa(
                fidl_common_security::WpaCredentials::Psk(
                    hex::decode(
                        b"f10aedbb0ea29c928b06997ed305a697706ddad220ff5a98f252558a470a748f"
                    )
                    .unwrap()
                    .try_into()
                    .unwrap()
                )
            )))
        },
    };
}

struct TestValues {
    internal_objects: InternalObjects,
    external_interfaces: ExternalInterfaces,
}

// Internal policy objects, used for manipulating state within tests
#[allow(clippy::type_complexity)]
struct InternalObjects {
    internal_futures: JoinAll<Pin<Box<dyn Future<Output = Result<Infallible, Error>>>>>,
    _saved_networks: Arc<dyn SavedNetworksManagerApi>,
    phy_manager: Arc<Mutex<dyn PhyManagerApi>>,
    iface_manager: Arc<Mutex<dyn IfaceManagerApi>>,
}

struct ExternalInterfaces {
    monitor_service_proxy: fidl_fuchsia_wlan_device_service::DeviceMonitorProxy,
    monitor_service_stream: fidl_fuchsia_wlan_device_service::DeviceMonitorRequestStream,
    client_controller: fidl_policy::ClientControllerProxy,
    listener_updates_stream: fidl_policy::ClientStateUpdatesRequestStream,
    _telemetry_receiver: mpsc::Receiver<TelemetryEvent>,
}

struct ExistingConnectionSmeObjects {
    iface_sme_stream: fidl_sme::ClientSmeRequestStream,
    state_machine_sme_stream: fidl_sme::ClientSmeRequestStream,
    connect_txn_handle: fidl_sme::ConnectTransactionControlHandle,
}

// setup channels and proxies needed for the tests
fn test_setup(
    exec: &mut TestExecutor,
    recovery_profile: &str,
    recovery_enabled: bool,
) -> TestValues {
    let (monitor_service_proxy, monitor_service_requests) =
        create_proxy::<fidl_fuchsia_wlan_device_service::DeviceMonitorMarker>()
            .expect("failed to create SeviceService proxy");
    let monitor_service_stream = monitor_service_requests.into_stream();

    let saved_networks = exec.run_singlethreaded(SavedNetworksManager::new_for_test());
    let saved_networks = Arc::new(saved_networks);
    let (persistence_req_sender, _persistence_stream) = create_inspect_persistence_channel();
    let (telemetry_sender, telemetry_receiver) = mpsc::channel::<TelemetryEvent>(100);
    let telemetry_sender = TelemetrySender::new(telemetry_sender);
    let (scan_request_sender, scan_request_receiver) =
        mpsc::channel(scan::SCAN_REQUEST_BUFFER_SIZE);
    let scan_requester = Arc::new(scan::ScanRequester { sender: scan_request_sender });
    let (recovery_sender, recovery_receiver) =
        mpsc::channel::<recovery::RecoverySummary>(recovery::RECOVERY_SUMMARY_CHANNEL_CAPACITY);
    let connection_selector = Rc::new(connection_selection::ConnectionSelector::new(
        saved_networks.clone(),
        scan_requester.clone(),
        inspect::Inspector::default().root().create_child("connection_selector"),
        persistence_req_sender.clone(),
        telemetry_sender.clone(),
    ));
    let (connection_selection_request_sender, connection_selection_request_receiver) =
        mpsc::channel(5);
    let connection_selection_fut = Box::pin(
        connection_selection::serve_connection_selection_request_loop(
            connection_selector,
            connection_selection_request_receiver,
        ) // Map the output type of this future to match the other ones we want to combine with it
        .map(|_| {
            let result: Result<Infallible, Error> =
                Err(format_err!("connection_selection future exited unexpectedly"));
            result
        }),
    );
    let connection_selection_requester = connection_selection::ConnectionSelectionRequester::new(
        connection_selection_request_sender,
    );
    let (roam_service_request_sender, roam_service_request_receiver) =
        mpsc::channel(ROAMING_CHANNEL_BUFFER_SIZE);
    let roam_manager_service_fut = Box::pin(
        serve_local_roam_manager_requests(
            RoamingPolicy::Disabled,
            roam_service_request_receiver,
            connection_selection_requester.clone(),
            telemetry_sender.clone(),
            saved_networks.clone(),
        )
        .map(|_| {
            let result: Result<Infallible, Error> =
                Err(format_err!("roam manager service future exited unexpectedly"));
            result
        }),
    );
    let roam_manager = RoamManager::new(roam_service_request_sender);

    let (client_provider_proxy, client_provider_requests) =
        create_proxy::<fidl_policy::ClientProviderMarker>()
            .expect("failed to create ClientProvider proxy");
    let client_provider_requests = client_provider_requests.into_stream();

    let (client_update_sender, client_update_receiver) = mpsc::unbounded();
    let (ap_update_sender, _ap_update_receiver) = mpsc::unbounded();

    let phy_manager = Arc::new(Mutex::new(PhyManager::new(
        monitor_service_proxy.clone(),
        recovery::lookup_recovery_profile(recovery_profile),
        recovery_enabled,
        inspect::Inspector::default().root().create_child("phy_manager"),
        telemetry_sender.clone(),
        recovery_sender,
    )));
    let (defect_sender, defect_receiver) = mpsc::channel(DEFECT_CHANNEL_SIZE);
    let (iface_manager, iface_manager_service) = create_iface_manager(
        phy_manager.clone(),
        client_update_sender.clone(),
        ap_update_sender.clone(),
        monitor_service_proxy.clone(),
        saved_networks.clone(),
        connection_selection_requester.clone(),
        roam_manager.clone(),
        telemetry_sender.clone(),
        defect_sender,
        defect_receiver,
        recovery_receiver,
        inspect::Inspector::default().root().create_child("iface_manager"),
    );
    let iface_manager_service = Box::pin(iface_manager_service);
    let scan_manager_service = Box::pin(
        scan::serve_scanning_loop(
            iface_manager.clone(),
            saved_networks.clone(),
            telemetry_sender.clone(),
            scan::LocationSensorUpdater {},
            scan_request_receiver,
        )
        // Map the output type of this future to match the other ones we want to combine with it
        .map(|_| {
            let result: Result<Infallible, Error> =
                Err(format_err!("scan_manager_service future exited unexpectedly"));
            result
        }),
    );

    let client_provider_lock = Arc::new(Mutex::new(()));

    let serve_fut: Pin<Box<dyn Future<Output = Result<Infallible, Error>>>> = Box::pin(
        serve_provider_requests(
            iface_manager.clone(),
            client_update_sender,
            saved_networks.clone(),
            scan_requester,
            client_provider_lock,
            client_provider_requests,
            telemetry_sender,
        )
        // Map the output type of this future to match the other ones we want to combine with it
        .map(|_| {
            let result: Result<Infallible, Error> =
                Err(format_err!("serve_provider_requests future exited unexpectedly"));
            result
        }),
    );

    let serve_client_policy_listeners = Box::pin(
        listener::serve::<
            fidl_policy::ClientStateUpdatesProxy,
            fidl_policy::ClientStateSummary,
            listener::ClientStateUpdate,
        >(client_update_receiver)
        // Map the output type of this future to match the other ones we want to combine with it
        .map(|_| {
            let result: Result<Infallible, Error> =
                Err(format_err!("serve_client_policy_listeners future exited unexpectedly"));
            result
        })
        .fuse(),
    );

    let (client_controller, listener_updates_stream) = request_controller(&client_provider_proxy);

    // Combine all our "internal" futures into one, since we don't care about their individual progress
    let internal_futures = join_all(vec![
        serve_fut,
        iface_manager_service,
        scan_manager_service,
        serve_client_policy_listeners,
        roam_manager_service_fut,
        connection_selection_fut,
    ]);

    let internal_objects = InternalObjects {
        internal_futures,
        _saved_networks: saved_networks,
        phy_manager,
        iface_manager,
    };

    let external_interfaces = ExternalInterfaces {
        monitor_service_proxy,
        monitor_service_stream,
        client_controller,
        listener_updates_stream,
        _telemetry_receiver: telemetry_receiver,
    };

    TestValues { internal_objects, external_interfaces }
}

fn add_phy(exec: &mut TestExecutor, test_values: &mut TestValues) {
    // Use the "legacy" module to mimic the wlancfg main module. When the main module
    // is refactored to remove the "legacy" module, we can also refactor this section.
    let legacy_client = legacy::IfaceRef::new();
    let listener = device_monitor::Listener::new(
        test_values.external_interfaces.monitor_service_proxy.clone(),
        legacy_client.clone(),
        test_values.internal_objects.phy_manager.clone(),
        test_values.internal_objects.iface_manager.clone(),
    );
    let add_phy_event = DeviceWatcherEvent::OnPhyAdded { phy_id: TEST_PHY_ID };
    let add_phy_fut = device_monitor::handle_event(&listener, add_phy_event);
    let mut add_phy_fut = pin!(add_phy_fut);

    let device_monitor_req = run_while(
        exec,
        &mut add_phy_fut,
        test_values.external_interfaces.monitor_service_stream.next(),
    );
    assert_variant!(
        device_monitor_req,
        Some(Ok(fidl_fuchsia_wlan_device_service::DeviceMonitorRequest::GetSupportedMacRoles {
            phy_id: TEST_PHY_ID, responder
        })) => {
            // Send back a positive acknowledgement.
            assert!(responder.send(Ok(&[fidl_common::WlanMacRole::Client])).is_ok());
        }
    );

    exec.run_singlethreaded(pin!(
        add_phy_fut.expect_within(zx::MonotonicDuration::from_seconds(5), "future didn't complete")
    ));
}

fn security_support_with_wpa3() -> fidl_common::SecuritySupport {
    fidl_common::SecuritySupport {
        mfp: fidl_common::MfpFeature { supported: true },
        sae: fidl_common::SaeFeature {
            driver_handler_supported: true,
            sme_handler_supported: true,
        },
    }
}

/// Adds a phy and prepares client interfaces by turning on client connections
fn prepare_client_interface(
    exec: &mut TestExecutor,
    test_values: &mut TestValues,
) -> fidl_sme::ClientSmeRequestStream {
    // Add the phy
    add_phy(exec, test_values);

    // Use the Policy API to start client connections
    let start_connections_fut =
        test_values.external_interfaces.client_controller.start_client_connections();
    let mut start_connections_fut = pin!(start_connections_fut);
    assert_variant!(exec.run_until_stalled(&mut start_connections_fut), Poll::Pending);

    // Expect an interface creation request
    let iface_creation_req = run_while(
        exec,
        &mut test_values.internal_objects.internal_futures,
        test_values.external_interfaces.monitor_service_stream.next(),
    );
    assert_variant!(
        iface_creation_req,
        Some(Ok(fidl_fuchsia_wlan_device_service::DeviceMonitorRequest::CreateIface {
            req: fidl_fuchsia_wlan_device_service::CreateIfaceRequest {
                phy_id: TEST_PHY_ID, role: fidl_common::WlanMacRole::Client, sta_addr: [0, 0, 0, 0, 0, 0]
            },
            responder
        })) => {
            assert!(responder.send(
                zx::sys::ZX_OK,
                Some(&fidl_fuchsia_wlan_device_service::CreateIfaceResponse {iface_id: TEST_CLIENT_IFACE_ID})
            ).is_ok());
        }
    );

    // Expect a feature support query as part of the interface creation
    let feature_support_req = run_while(
        exec,
        &mut test_values.internal_objects.internal_futures,
        test_values.external_interfaces.monitor_service_stream.next(),
    );
    assert_variant!(
        feature_support_req,
        Some(Ok(fidl_fuchsia_wlan_device_service::DeviceMonitorRequest::GetFeatureSupport {
            iface_id: TEST_CLIENT_IFACE_ID, feature_support_server, responder
        })) => {
            assert!(responder.send(Ok(())).is_ok());
            let (mut stream, _handle) = feature_support_server.into_stream_and_control_handle().unwrap();

            // Send back feature support information
            let security_support_req = run_while(
                exec,
                &mut test_values.internal_objects.internal_futures,
                stream.next(),
            );
            assert_variant!(
                security_support_req,
                Some(Ok(fidl_sme::FeatureSupportRequest::QuerySecuritySupport {
                    responder
                })) => {
                    assert!(responder.send(Ok(&security_support_with_wpa3())).is_ok());
                }
            );
        }
    );

    // Expect an interface query and notify that this is a client interface.
    let iface_query = run_while(
        exec,
        &mut test_values.internal_objects.internal_futures,
        test_values.external_interfaces.monitor_service_stream.next(),
    );
    assert_variant!(
        iface_query,
        Some(Ok(fidl_fuchsia_wlan_device_service::DeviceMonitorRequest::QueryIface {
            iface_id: TEST_CLIENT_IFACE_ID, responder
        })) => {
            let response = fidl_fuchsia_wlan_device_service::QueryIfaceResponse {
                role: fidl_fuchsia_wlan_common::WlanMacRole::Client,
                id: TEST_CLIENT_IFACE_ID,
                phy_id: 0,
                phy_assigned_id: 0,
                sta_addr: [0; 6],
            };
            responder
                .send(Ok(&response))
                .expect("Sending iface response");
        }
    );

    // Expect that we have requested a client SME proxy as part of interface creation
    let sme_req = run_while(
        exec,
        &mut test_values.internal_objects.internal_futures,
        test_values.external_interfaces.monitor_service_stream.next(),
    );
    let sme_server = assert_variant!(
        sme_req,
        Some(Ok(fidl_fuchsia_wlan_device_service::DeviceMonitorRequest::GetClientSme {
            iface_id: TEST_CLIENT_IFACE_ID, sme_server, responder
        })) => {
            // Send back a positive acknowledgement.
            assert!(responder.send(Ok(())).is_ok());
            sme_server
        }
    );

    let iface_sme_stream = sme_server.into_stream();

    // There will be another security support query as part of adding the interface to iface_manager
    let feature_support_req = run_while(
        exec,
        &mut test_values.internal_objects.internal_futures,
        test_values.external_interfaces.monitor_service_stream.next(),
    );
    assert_variant!(
        feature_support_req,
        Some(Ok(fidl_fuchsia_wlan_device_service::DeviceMonitorRequest::GetFeatureSupport {
            iface_id: TEST_CLIENT_IFACE_ID, feature_support_server, responder
        })) => {
            assert!(responder.send(Ok(())).is_ok());
            let (mut stream, _handle) = feature_support_server.into_stream_and_control_handle().unwrap();

            // Send back feature support information
            let security_support_req = run_while(
                exec,
                &mut test_values.internal_objects.internal_futures,
                stream.next(),
            );
            assert_variant!(
                security_support_req,
                Some(Ok(fidl_sme::FeatureSupportRequest::QuerySecuritySupport {
                    responder
                })) => {
                    assert!(responder.send(Ok(&security_support_with_wpa3())).is_ok());
                }
            );
        }
    );

    // Expect to get an SME request for the state machine creation
    let sme_req = run_while(
        exec,
        &mut test_values.internal_objects.internal_futures,
        test_values.external_interfaces.monitor_service_stream.next(),
    );
    let sme_server = assert_variant!(
        sme_req,
        Some(Ok(fidl_fuchsia_wlan_device_service::DeviceMonitorRequest::GetClientSme {
            iface_id: TEST_CLIENT_IFACE_ID, sme_server, responder
        })) => {
            // Send back a positive acknowledgement.
            assert!(responder.send(Ok(())).is_ok());
            sme_server
        }
    );
    let mut sme_stream = sme_server.into_stream();

    // State machine does an initial disconnect
    let sme_req =
        run_while(exec, &mut test_values.internal_objects.internal_futures, sme_stream.next());
    assert_variant!(
        sme_req,
        Some(Ok(fidl_sme::ClientSmeRequest::Disconnect {
            reason, responder
        })) => {
            assert_eq!(fidl_sme::UserDisconnectReason::Startup, reason);
            assert!(responder.send().is_ok());
        }
    );

    // Ensure the disconnect is fully processed by our state machine
    assert_variant!(
        exec.run_until_stalled(&mut test_values.internal_objects.internal_futures),
        Poll::Pending
    );

    // Check for a response to the Policy API start client connections request
    let start_connections_resp = run_while(
        exec,
        &mut test_values.internal_objects.internal_futures,
        &mut start_connections_fut,
    );
    assert_variant!(start_connections_resp, Ok(fidl_policy::RequestStatus::Acknowledged));

    iface_sme_stream
}

fn request_controller(
    provider: &fidl_policy::ClientProviderProxy,
) -> (fidl_policy::ClientControllerProxy, fidl_policy::ClientStateUpdatesRequestStream) {
    let (controller, requests) = create_proxy::<fidl_policy::ClientControllerMarker>()
        .expect("failed to create ClientController proxy");
    let (update_sink, update_stream) =
        create_request_stream::<fidl_policy::ClientStateUpdatesMarker>()
            .expect("failed to create ClientStateUpdates proxy");
    provider.get_controller(requests, update_sink).expect("error getting controller");
    (controller, update_stream)
}

#[track_caller]
fn get_client_state_update<BackgroundFut>(
    exec: &mut TestExecutor,
    background_tasks: &mut BackgroundFut,
    client_listener_update_requests: &mut fidl_policy::ClientStateUpdatesRequestStream,
) -> fidl_policy::ClientStateSummary
where
    BackgroundFut: Future + Unpin,
    <BackgroundFut as futures::Future>::Output: std::fmt::Debug,
{
    // Get the next update
    let next_update_req = run_while(exec, background_tasks, client_listener_update_requests.next());
    let update_request = assert_variant!(
        next_update_req,
        Some(Ok(update_request)) => {
            update_request
        }
    );
    let (update, responder) = update_request.into_on_client_state_update().unwrap();

    // Send ack and ensure it is processed
    let _ = responder.send();
    assert_variant!(exec.run_until_stalled(background_tasks), Poll::Pending);

    update
}

/// Gets a set of security protocols that describe the protection of a BSS.
///
/// This function does **not** consider hardware and driver support. Returns an empty `Vec` if
/// there are no corresponding security protocols for the given BSS protection.
fn security_protocols_from_protection(
    protection: fidl_sme::Protection,
) -> Vec<fidl_common_security::Protocol> {
    use fidl_sme::Protection::*;

    match protection {
        Open => vec![fidl_common_security::Protocol::Open],
        Wep => vec![fidl_common_security::Protocol::Wep],
        Wpa1 => vec![fidl_common_security::Protocol::Wpa1],
        Wpa1Wpa2PersonalTkipOnly | Wpa1Wpa2Personal => {
            vec![fidl_common_security::Protocol::Wpa2Personal, fidl_common_security::Protocol::Wpa1]
        }
        Wpa2PersonalTkipOnly | Wpa2Personal => vec![fidl_common_security::Protocol::Wpa2Personal],
        Wpa2Wpa3Personal => vec![
            fidl_common_security::Protocol::Wpa3Personal,
            fidl_common_security::Protocol::Wpa2Personal,
        ],
        Wpa3Personal => vec![fidl_common_security::Protocol::Wpa3Personal],
        Wpa2Enterprise | Wpa3Enterprise | Unknown => vec![],
    }
}

// Setup a client iface, save a network, and connect to that network via the new client iface.
// Returns the SME streams and transaction handle for the connected iface and state machine. If
// these are dropped, the connected state machine will exit when progressed.
fn save_and_connect(
    ssid: types::Ssid,
    saved_security: fidl_policy::SecurityType,
    scanned_security: fidl_sme::Protection,
    test_credentials: TestCredentials,
    exec: &mut fasync::TestExecutor,
    test_values: &mut TestValues,
) -> ExistingConnectionSmeObjects {
    // No request has been sent yet. Future should be idle.
    assert_variant!(
        exec.run_until_stalled(&mut test_values.internal_objects.internal_futures),
        Poll::Pending
    );

    // Initial update should reflect client connections are disabled
    let fidl_policy::ClientStateSummary { state, networks, .. } = get_client_state_update(
        exec,
        &mut test_values.internal_objects.internal_futures,
        &mut test_values.external_interfaces.listener_updates_stream,
    );
    assert_eq!(state.unwrap(), fidl_policy::WlanClientState::ConnectionsDisabled);
    assert_eq!(networks.unwrap().len(), 0);

    // Get ready for client connections
    let mut iface_sme_stream = prepare_client_interface(exec, test_values);

    // Check for a listener update saying client connections are enabled
    let fidl_policy::ClientStateSummary { state, networks, .. } = get_client_state_update(
        exec,
        &mut test_values.internal_objects.internal_futures,
        &mut test_values.external_interfaces.listener_updates_stream,
    );
    assert_eq!(state.unwrap(), fidl_policy::WlanClientState::ConnectionsEnabled);
    assert_eq!(networks.unwrap().len(), 0);

    // Generate network ID
    let network_id = fidl_policy::NetworkIdentifier { ssid: ssid.to_vec(), type_: saved_security };
    let network_config = fidl_policy::NetworkConfig {
        id: Some(network_id.clone()),
        credential: Some(test_credentials.policy.clone()),
        ..Default::default()
    };

    // Save the network
    let save_fut = test_values.external_interfaces.client_controller.save_network(&network_config);
    let save_fut = pin!(save_fut);

    // Continue processing the save request. Connect process starts, and save request returns once the scan has been queued.
    let save_resp = run_while(exec, &mut test_values.internal_objects.internal_futures, save_fut);
    assert_variant!(save_resp, Ok(Ok(())));

    // Check for a listener update saying we're connecting
    let fidl_policy::ClientStateSummary { state, networks, .. } = get_client_state_update(
        exec,
        &mut test_values.internal_objects.internal_futures,
        &mut test_values.external_interfaces.listener_updates_stream,
    );
    assert_eq!(state.unwrap(), fidl_policy::WlanClientState::ConnectionsEnabled);
    let mut networks = networks.unwrap();
    assert_eq!(networks.len(), 1);
    let network = networks.pop().unwrap();
    assert_eq!(network.id.unwrap(), network_id.clone());
    assert_eq!(network.state.unwrap(), types::ConnectionState::Connecting);

    // BSS selection scans occur for requested network. Return scan results.
    let expected_scan_request = fidl_sme::ScanRequest::Active(fidl_sme::ActiveScanRequest {
        ssids: vec![TEST_SSID.clone().into()],
        channels: vec![],
    });
    let mutual_security_protocols = security_protocols_from_protection(scanned_security);
    assert!(!mutual_security_protocols.is_empty(), "no mutual security protocols");
    let mock_scan_results = vec![fidl_sme::ScanResult {
        compatibility: Some(Box::new(fidl_sme::Compatibility { mutual_security_protocols })),
        timestamp_nanos: zx::MonotonicInstant::get().into_nanos(),
        bss_description: random_fidl_bss_description!(
            protection =>  wlan_common::test_utils::fake_stas::FakeProtectionCfg::from(scanned_security),
            bssid: [0, 0, 0, 0, 0, 0],
            ssid: TEST_SSID.clone(),
            rssi_dbm: 10,
            snr_db: 10,
            channel: types::WlanChan::new(1, types::Cbw::Cbw20),
        ),
    }];
    let next_sme_stream_req = run_while(
        exec,
        &mut test_values.internal_objects.internal_futures,
        iface_sme_stream.next(),
    );
    assert_variant!(
        next_sme_stream_req,
        Some(Ok(fidl_sme::ClientSmeRequest::Scan {
            req, responder
        })) => {
            assert_eq!(req, expected_scan_request);
            let vmo = write_vmo(mock_scan_results).expect("failed to write VMO");
            responder.send(Ok(vmo)).expect("failed to send scan data");
        }
    );

    // Expect to get an SME request for state machine creation.
    let next_device_monitor_req = run_while(
        exec,
        &mut test_values.internal_objects.internal_futures,
        test_values.external_interfaces.monitor_service_stream.next(),
    );
    let sme_server = assert_variant!(
        next_device_monitor_req,
        Some(Ok(fidl_fuchsia_wlan_device_service::DeviceMonitorRequest::GetClientSme {
            iface_id: TEST_CLIENT_IFACE_ID, sme_server, responder
        })) => {
            // Send back a positive acknowledgement.
            assert!(responder.send(Ok(())).is_ok());
            sme_server
        }
    );
    let mut state_machine_sme_stream = sme_server.into_stream();

    // State machine does an initial disconnect. Ack.
    let next_sme_req = run_while(
        exec,
        &mut test_values.internal_objects.internal_futures,
        state_machine_sme_stream.next(),
    );
    assert_variant!(
        next_sme_req,
        Some(Ok(fidl_sme::ClientSmeRequest::Disconnect {
            reason, responder
        })) => {
            assert_eq!(fidl_sme::UserDisconnectReason::Startup, reason);
            assert!(responder.send().is_ok());
        }
    );

    // State machine connects
    let next_sme_req = run_while(
        exec,
        &mut test_values.internal_objects.internal_futures,
        state_machine_sme_stream.next(),
    );
    let connect_txn_handle = assert_variant!(
        next_sme_req,
        Some(Ok(fidl_sme::ClientSmeRequest::Connect {
            req, txn, control_handle: _
        })) => {
            assert_eq!(req.ssid, TEST_SSID.clone());
            assert_eq!(test_credentials.sme.clone(), req.authentication.credentials);
            let (_stream, ctrl) = txn.expect("connect txn unused")
                .into_stream_and_control_handle().expect("error accessing control handle");
            ctrl
        }
    );
    connect_txn_handle
        .send_on_connect_result(&fidl_sme::ConnectResult {
            code: fidl_fuchsia_wlan_ieee80211::StatusCode::Success,
            is_credential_rejected: false,
            is_reconnect: false,
        })
        .expect("failed to send connection completion");

    // Check for a listener update saying we're connected
    let fidl_policy::ClientStateSummary { state, networks, .. } = get_client_state_update(
        exec,
        &mut test_values.internal_objects.internal_futures,
        &mut test_values.external_interfaces.listener_updates_stream,
    );
    assert_eq!(state.unwrap(), fidl_policy::WlanClientState::ConnectionsEnabled);
    let mut networks = networks.unwrap();
    assert_eq!(networks.len(), 1);
    let network = networks.pop().unwrap();
    assert_eq!(network.id.unwrap(), network_id.clone());
    assert_eq!(network.state.unwrap(), types::ConnectionState::Connected);

    ExistingConnectionSmeObjects { iface_sme_stream, state_machine_sme_stream, connect_txn_handle }
}

use fidl_policy::SecurityType as Saved;
use fidl_sme::Protection as Scanned;
#[test_case(Saved::None, Scanned::Open, TEST_CREDS.none.clone())]
// Saved credential: WEP 40/64 bit
#[test_case(Saved::Wep, Scanned::Wep, TEST_CREDS.wep_64_ascii.clone())]
#[test_case(Saved::Wep, Scanned::Wep, TEST_CREDS.wep_64_hex.clone())]
// Saved credential: WEP 104/128 bit
#[test_case(Saved::Wep, Scanned::Wep, TEST_CREDS.wep_128_ascii.clone())]
#[test_case(Saved::Wep, Scanned::Wep, TEST_CREDS.wep_128_hex.clone())]
// Saved credential: WPA1
#[test_case(Saved::Wpa, Scanned::Wpa1, TEST_CREDS.wpa_pass_min.clone())]
#[test_case(Saved::Wpa, Scanned::Wpa1, TEST_CREDS.wpa_pass_max.clone())]
#[test_case(Saved::Wpa, Scanned::Wpa1, TEST_CREDS.wpa_psk.clone())]
#[test_case(Saved::Wpa, Scanned::Wpa1Wpa2Personal, TEST_CREDS.wpa_pass_min.clone())]
#[test_case(Saved::Wpa, Scanned::Wpa1Wpa2Personal, TEST_CREDS.wpa_pass_max.clone())]
#[test_case(Saved::Wpa, Scanned::Wpa1Wpa2Personal, TEST_CREDS.wpa_psk.clone())]
#[test_case(Saved::Wpa, Scanned::Wpa1Wpa2PersonalTkipOnly, TEST_CREDS.wpa_pass_min.clone())]
#[test_case(Saved::Wpa, Scanned::Wpa1Wpa2PersonalTkipOnly, TEST_CREDS.wpa_pass_max.clone())]
#[test_case(Saved::Wpa, Scanned::Wpa1Wpa2PersonalTkipOnly, TEST_CREDS.wpa_psk.clone())]
#[test_case(Saved::Wpa, Scanned::Wpa2Personal, TEST_CREDS.wpa_pass_min.clone())]
#[test_case(Saved::Wpa, Scanned::Wpa2Personal, TEST_CREDS.wpa_pass_max.clone())]
#[test_case(Saved::Wpa, Scanned::Wpa2Personal, TEST_CREDS.wpa_psk.clone())]
#[test_case(Saved::Wpa, Scanned::Wpa2PersonalTkipOnly, TEST_CREDS.wpa_pass_min.clone())]
#[test_case(Saved::Wpa, Scanned::Wpa2PersonalTkipOnly, TEST_CREDS.wpa_pass_max.clone())]
#[test_case(Saved::Wpa, Scanned::Wpa2PersonalTkipOnly, TEST_CREDS.wpa_psk.clone())]
// TODO(https://fxbug.dev/42166758): reenable credential upgrading
// #[test_case(Saved::Wpa, Scanned::Wpa2Wpa3Personal, TEST_CREDS.wpa_pass_min.clone())]
// #[test_case(Saved::Wpa, Scanned::Wpa2Wpa3Personal, TEST_CREDS.wpa_pass_max.clone())]
// #[test_case(Saved::Wpa, Scanned::Wpa2Wpa3Personal, TEST_CREDS.wpa_psk.clone())]
// #[test_case(Saved::Wpa, Scanned::Wpa3Personal, TEST_CREDS.wpa_pass_min.clone())]
// #[test_case(Saved::Wpa, Scanned::Wpa3Personal, TEST_CREDS.wpa_pass_max.clone())]
// Saved credential: WPA2
#[test_case(Saved::Wpa2, Scanned::Wpa2Personal, TEST_CREDS.wpa_pass_min.clone())]
#[test_case(Saved::Wpa2, Scanned::Wpa2Personal, TEST_CREDS.wpa_pass_max.clone())]
#[test_case(Saved::Wpa2, Scanned::Wpa2Personal, TEST_CREDS.wpa_psk.clone())]
#[test_case(Saved::Wpa2, Scanned::Wpa2PersonalTkipOnly, TEST_CREDS.wpa_pass_min.clone())]
#[test_case(Saved::Wpa2, Scanned::Wpa2PersonalTkipOnly, TEST_CREDS.wpa_pass_max.clone())]
#[test_case(Saved::Wpa2, Scanned::Wpa2PersonalTkipOnly, TEST_CREDS.wpa_psk.clone())]
#[test_case(Saved::Wpa2, Scanned::Wpa2Wpa3Personal, TEST_CREDS.wpa_pass_min.clone())]
#[test_case(Saved::Wpa2, Scanned::Wpa2Wpa3Personal, TEST_CREDS.wpa_pass_max.clone())]
#[test_case(Saved::Wpa2, Scanned::Wpa2Wpa3Personal, TEST_CREDS.wpa_psk.clone())]
#[test_case(Saved::Wpa2, Scanned::Wpa3Personal, TEST_CREDS.wpa_pass_min.clone())]
#[test_case(Saved::Wpa2, Scanned::Wpa3Personal, TEST_CREDS.wpa_pass_max.clone())]
// Saved credential: WPA3
#[test_case(Saved::Wpa3, Scanned::Wpa2Wpa3Personal, TEST_CREDS.wpa_pass_min.clone())]
#[test_case(Saved::Wpa3, Scanned::Wpa2Wpa3Personal, TEST_CREDS.wpa_pass_max.clone())]
#[test_case(Saved::Wpa3, Scanned::Wpa3Personal, TEST_CREDS.wpa_pass_min.clone())]
#[test_case(Saved::Wpa3, Scanned::Wpa3Personal, TEST_CREDS.wpa_pass_max.clone())]
#[fuchsia::test(add_test_attr = false)]
/// Tests saving and connecting across various security types
fn test_save_and_connect(
    saved_security: fidl_policy::SecurityType,
    scanned_security: fidl_sme::Protection,
    test_credentials: TestCredentials,
) {
    let mut exec = fasync::TestExecutor::new();
    let mut test_values = test_setup(&mut exec, RECOVERY_PROFILE_EMPTY_STRING, false);

    let _ = save_and_connect(
        TEST_SSID.clone(),
        saved_security,
        scanned_security,
        test_credentials,
        &mut exec,
        &mut test_values,
    );
}

// TODO(https://fxbug.dev/42166758): reenable credential upgrading, which will make these cases connect
#[test_case(Saved::Wpa, Scanned::Wpa2Wpa3Personal, TEST_CREDS.wpa_pass_min.clone())]
#[test_case(Saved::Wpa, Scanned::Wpa2Wpa3Personal, TEST_CREDS.wpa_pass_max.clone())]
#[test_case(Saved::Wpa, Scanned::Wpa2Wpa3Personal, TEST_CREDS.wpa_psk.clone())]
#[test_case(Saved::Wpa, Scanned::Wpa3Personal, TEST_CREDS.wpa_pass_min.clone())]
#[test_case(Saved::Wpa, Scanned::Wpa3Personal, TEST_CREDS.wpa_pass_max.clone())]
// WPA credentials should never be used for WEP or Open networks
#[test_case(Saved::Wpa, Scanned::Open, TEST_CREDS.wpa_pass_min.clone())]
#[test_case(Saved::Wpa, Scanned::Open, TEST_CREDS.wpa_psk.clone())]
#[test_case(Saved::Wpa, Scanned::Wep, TEST_CREDS.wep_64_hex.clone())] // Use credentials which are valid len for WEP and WPA
#[test_case(Saved::Wpa, Scanned::Wep, TEST_CREDS.wpa_psk.clone())]
#[test_case(Saved::Wpa2, Scanned::Open, TEST_CREDS.wpa_pass_min.clone())]
#[test_case(Saved::Wpa2, Scanned::Open, TEST_CREDS.wpa_psk.clone())]
#[test_case(Saved::Wpa2, Scanned::Wep, TEST_CREDS.wep_64_hex.clone())] // Use credentials which are valid len for WEP and WPA
#[test_case(Saved::Wpa2, Scanned::Wep, TEST_CREDS.wpa_psk.clone())]
#[test_case(Saved::Wpa3, Scanned::Open, TEST_CREDS.wpa_pass_min.clone())]
#[test_case(Saved::Wpa3, Scanned::Wep, TEST_CREDS.wep_64_hex.clone())] // Use credentials which are valid len for WEP and WPA
// PSKs should never be used for WPA3
#[test_case(Saved::Wpa, Scanned::Wpa3Personal, TEST_CREDS.wpa_psk.clone())]
#[test_case(Saved::Wpa2, Scanned::Wpa3Personal, TEST_CREDS.wpa_psk.clone())]
// Saved credential: WPA2: downgrades are disallowed
#[test_case(Saved::Wpa2, Scanned::Wpa1, TEST_CREDS.wpa_pass_min.clone())]
#[test_case(Saved::Wpa2, Scanned::Wpa1, TEST_CREDS.wpa_pass_max.clone())]
#[test_case(Saved::Wpa2, Scanned::Wpa1, TEST_CREDS.wpa_psk.clone())]
// Saved credential: WPA3: downgrades are disallowed
#[test_case(Saved::Wpa3, Scanned::Wpa1, TEST_CREDS.wpa_pass_min.clone())]
#[test_case(Saved::Wpa3, Scanned::Wpa1, TEST_CREDS.wpa_pass_max.clone())]
#[test_case(Saved::Wpa3, Scanned::Wpa1Wpa2Personal, TEST_CREDS.wpa_pass_min.clone())]
#[test_case(Saved::Wpa3, Scanned::Wpa1Wpa2Personal, TEST_CREDS.wpa_pass_max.clone())]
#[test_case(Saved::Wpa3, Scanned::Wpa1Wpa2PersonalTkipOnly, TEST_CREDS.wpa_pass_min.clone())]
#[test_case(Saved::Wpa3, Scanned::Wpa1Wpa2PersonalTkipOnly, TEST_CREDS.wpa_pass_max.clone())]
#[test_case(Saved::Wpa3, Scanned::Wpa2Personal, TEST_CREDS.wpa_pass_min.clone())]
#[test_case(Saved::Wpa3, Scanned::Wpa2Personal, TEST_CREDS.wpa_pass_max.clone())]
#[test_case(Saved::Wpa3, Scanned::Wpa2PersonalTkipOnly, TEST_CREDS.wpa_pass_min.clone())]
#[test_case(Saved::Wpa3, Scanned::Wpa2PersonalTkipOnly, TEST_CREDS.wpa_pass_max.clone())]
#[fuchsia::test(add_test_attr = false)]
/// Tests saving and connecting across various security types, where the connection is expected to fail
fn test_save_and_fail_to_connect(
    saved_security: fidl_policy::SecurityType,
    scanned_security: fidl_sme::Protection,
    test_credentials: TestCredentials,
) {
    let saved_credential = test_credentials.policy.clone();

    let mut exec = fasync::TestExecutor::new();
    let mut test_values = test_setup(&mut exec, RECOVERY_PROFILE_EMPTY_STRING, false);

    // No request has been sent yet. Future should be idle.
    assert_variant!(
        exec.run_until_stalled(&mut test_values.internal_objects.internal_futures),
        Poll::Pending
    );

    // Initial update should reflect client connections are disabled
    let fidl_policy::ClientStateSummary { state, networks, .. } = get_client_state_update(
        &mut exec,
        &mut test_values.internal_objects.internal_futures,
        &mut test_values.external_interfaces.listener_updates_stream,
    );
    assert_eq!(state.unwrap(), fidl_policy::WlanClientState::ConnectionsDisabled);
    assert_eq!(networks.unwrap().len(), 0);

    // Get ready for client connections
    let mut iface_sme_stream = prepare_client_interface(&mut exec, &mut test_values);

    // Check for a listener update saying client connections are enabled
    let fidl_policy::ClientStateSummary { state, networks, .. } = get_client_state_update(
        &mut exec,
        &mut test_values.internal_objects.internal_futures,
        &mut test_values.external_interfaces.listener_updates_stream,
    );
    assert_eq!(state.unwrap(), fidl_policy::WlanClientState::ConnectionsEnabled);
    assert_eq!(networks.unwrap().len(), 0);

    // Generate network ID
    let network_id =
        fidl_policy::NetworkIdentifier { ssid: TEST_SSID.clone().into(), type_: saved_security };
    let network_config = fidl_policy::NetworkConfig {
        id: Some(network_id.clone()),
        credential: Some(saved_credential),
        ..Default::default()
    };

    // Save the network
    let save_fut = test_values.external_interfaces.client_controller.save_network(&network_config);
    let mut save_fut = pin!(save_fut);

    // Continue processing the save request. Auto-connection process starts, and we get an update
    // saying we are connecting.
    let fidl_policy::ClientStateSummary { state, networks, .. } = get_client_state_update(
        &mut exec,
        &mut test_values.internal_objects.internal_futures,
        &mut test_values.external_interfaces.listener_updates_stream,
    );
    assert_eq!(state.unwrap(), fidl_policy::WlanClientState::ConnectionsEnabled);
    let mut networks = networks.unwrap();
    assert_eq!(networks.len(), 1);
    let network = networks.pop().unwrap();
    assert_eq!(network.state.unwrap(), types::ConnectionState::Connecting);
    assert_eq!(network.id.unwrap(), network_id.clone());

    // Save request returns once the scan has been queued.
    let save_resp =
        run_while(&mut exec, &mut test_values.internal_objects.internal_futures, &mut save_fut);
    assert_variant!(save_resp, Ok(Ok(())));

    let mut bss_selection_scan_count = 0;
    while bss_selection_scan_count < 3 {
        // BSS selection scans occur for requested network. Return scan results.
        let expected_scan_request =
            fidl_sme::ActiveScanRequest { ssids: vec![TEST_SSID.clone().into()], channels: vec![] };
        let mutual_security_protocols = security_protocols_from_protection(scanned_security);
        assert!(!mutual_security_protocols.is_empty(), "no mutual security protocols");
        let mock_scan_results = vec![fidl_sme::ScanResult {
            compatibility: Some(Box::new(fidl_sme::Compatibility { mutual_security_protocols })),
            timestamp_nanos: zx::MonotonicInstant::get().into_nanos(),
            bss_description: random_fidl_bss_description!(
                protection =>  wlan_common::test_utils::fake_stas::FakeProtectionCfg::from(scanned_security),
                bssid: [0, 0, 0, 0, 0, 0],
                ssid: TEST_SSID.clone(),
                rssi_dbm: 10,
                snr_db: 10,
                channel: types::WlanChan::new(1, types::Cbw::Cbw20),
            ),
        }];
        let sme_request = run_while(
            &mut exec,
            &mut test_values.internal_objects.internal_futures,
            iface_sme_stream.next(),
        );
        assert_variant!(
            sme_request,
            Some(Ok(fidl_sme::ClientSmeRequest::Scan {
                req, responder
            })) => {
                match req {
                    fidl_sme::ScanRequest::Active(req) => {
                        assert_eq!(req, expected_scan_request);
                        bss_selection_scan_count += 1;
                    },
                    fidl_sme::ScanRequest::Passive(_req) => {
                        // Sometimes, a passive scan sneaks in for the idle interface. Ignore it.
                        // Context: https://fxbug.dev/42066447
                    },
                }
                let vmo = write_vmo(mock_scan_results).expect("failed to write VMO");
                responder.send(Ok(vmo)).expect("failed to send scan data");
            }
        );
    }

    // Check for a listener update saying we failed to connect
    let fidl_policy::ClientStateSummary { state, networks, .. } = get_client_state_update(
        &mut exec,
        &mut test_values.internal_objects.internal_futures,
        &mut test_values.external_interfaces.listener_updates_stream,
    );
    assert_eq!(state.unwrap(), fidl_policy::WlanClientState::ConnectionsEnabled);
    let mut networks = networks.unwrap();
    assert_eq!(networks.len(), 1);
    let network = networks.pop().unwrap();
    assert_eq!(network.state.unwrap(), types::ConnectionState::Failed);
    assert_eq!(network.id.unwrap(), network_id.clone());
}

// Saving this network should fail because a PSK cannot be used to connect to a WPA3 network.
#[test_case(fidl_policy::NetworkConfigChangeError::InvalidSecurityCredentialError, TEST_SSID.clone().into(), Saved::Wpa3, TEST_CREDS.wpa_psk.policy.clone())]
#[test_case(fidl_policy::NetworkConfigChangeError::SsidEmptyError, vec![], Saved::Wpa3, TEST_CREDS.wpa_psk.policy.clone())]
// Saving this network should fail because the PSK is too short.
#[test_case(fidl_policy::NetworkConfigChangeError::CredentialLenError, TEST_SSID.clone().into(), Saved::Wpa2, fidl_policy::Credential::Psk(hex::decode(b"12345678").unwrap()))]
// Saving this network should fail because the password is too short.
#[test_case(fidl_policy::NetworkConfigChangeError::CredentialLenError, TEST_SSID.clone().into(), Saved::Wpa2, fidl_policy::Credential::Password(b"12".to_vec()))]
fn test_fail_to_save(
    save_error: fidl_policy::NetworkConfigChangeError,
    ssid: Vec<u8>,
    saved_security: fidl_policy::SecurityType,
    saved_credential: fidl_policy::Credential,
) {
    let mut exec = fasync::TestExecutor::new();
    let mut test_values = test_setup(&mut exec, RECOVERY_PROFILE_EMPTY_STRING, false);

    // Generate network ID
    let network_id = fidl_policy::NetworkIdentifier { ssid, type_: saved_security };
    let network_config = fidl_policy::NetworkConfig {
        id: Some(network_id.clone()),
        credential: Some(saved_credential),
        ..Default::default()
    };

    // Save the network
    let save_fut = test_values.external_interfaces.client_controller.save_network(&network_config);
    let mut save_fut = pin!(save_fut);

    // Progress the WLAN policy side of the future
    assert_variant!(
        exec.run_until_stalled(&mut test_values.internal_objects.internal_futures),
        Poll::Pending
    );

    // Saving the network should return an error
    assert_variant!(exec.run_until_stalled(&mut save_fut), Poll::Ready(Ok(Err(error))) => {
        assert_eq!(error, save_error);
    });
}

// Tests the connect request path to a new network while already connected.
#[fuchsia::test]
fn test_connect_to_new_network() {
    let mut exec = fasync::TestExecutor::new();
    let mut test_values = test_setup(&mut exec, RECOVERY_PROFILE_EMPTY_STRING, false);

    // Connect to a network initially.
    let mut existing_connection = save_and_connect(
        TEST_SSID.clone(),
        Saved::Wpa2,
        Scanned::Wpa2Personal,
        TEST_CREDS.wpa_pass_min.clone(),
        &mut exec,
        &mut test_values,
    );

    // Generate a second network ID.
    let second_ssid = types::Ssid::try_from("test_ssid_2").unwrap();
    let network_id =
        fidl_policy::NetworkIdentifier { ssid: second_ssid.to_vec(), type_: Saved::Wpa2 };
    let network_config = fidl_policy::NetworkConfig {
        id: Some(network_id.clone()),
        credential: Some(TEST_CREDS.wpa_pass_min.policy.clone()),
        ..Default::default()
    };

    // Save the second network.
    let save_fut = test_values.external_interfaces.client_controller.save_network(&network_config);
    let save_fut = pin!(save_fut);

    // Save request returns.
    let save_fut_resp =
        run_while(&mut exec, &mut test_values.internal_objects.internal_futures, save_fut);
    assert_variant!(save_fut_resp, Ok(Ok(())));

    // Send a request to connect to the second network.
    let connect_fut = test_values.external_interfaces.client_controller.connect(&network_id);
    let connect_fut = pin!(connect_fut);

    // Check that connect request was acknowledged.
    let connect_fut_resp =
        run_while(&mut exec, &mut test_values.internal_objects.internal_futures, connect_fut);
    assert_variant!(connect_fut_resp, Ok(fidl_policy::RequestStatus::Acknowledged));

    // Check for a listener update saying we're both connected and connecting.
    let fidl_policy::ClientStateSummary { state, networks, .. } = get_client_state_update(
        &mut exec,
        &mut test_values.internal_objects.internal_futures,
        &mut test_values.external_interfaces.listener_updates_stream,
    );
    assert_eq!(state.unwrap(), fidl_policy::WlanClientState::ConnectionsEnabled);
    let networks = networks.unwrap();
    assert_eq!(networks.len(), 2);
    for network in networks {
        if network.id.unwrap() == network_id.clone() {
            assert_eq!(network.state.unwrap(), types::ConnectionState::Connecting)
        } else {
            assert_eq!(network.state.unwrap(), types::ConnectionState::Connected)
        }
    }

    // Expect a directed active scan, return scan results.
    let expected_scan_request = fidl_sme::ScanRequest::Active(fidl_sme::ActiveScanRequest {
        ssids: vec![second_ssid.to_vec()],
        channels: vec![],
    });
    let mutual_security_protocols = security_protocols_from_protection(Scanned::Wpa2Personal);
    let mock_scan_results = vec![
        fidl_sme::ScanResult {
            compatibility: Some(Box::new(fidl_sme::Compatibility {
                mutual_security_protocols: mutual_security_protocols.clone(),
            })),
            timestamp_nanos: zx::MonotonicInstant::get().into_nanos(),
            bss_description: random_fidl_bss_description!(
                protection =>  wlan_common::test_utils::fake_stas::FakeProtectionCfg::from(Scanned::Wpa2Personal),
                bssid: [0, 0, 0, 0, 0, 0],
                ssid: second_ssid.clone(),
                rssi_dbm: -70,
                snr_db: 20,
                channel: types::WlanChan::new(1, types::Cbw::Cbw20),
            ),
        },
        fidl_sme::ScanResult {
            compatibility: Some(Box::new(fidl_sme::Compatibility { mutual_security_protocols })),
            timestamp_nanos: zx::MonotonicInstant::get().into_nanos(),
            bss_description: random_fidl_bss_description!(
                protection =>  wlan_common::test_utils::fake_stas::FakeProtectionCfg::from(Scanned::Wpa2Personal),
                bssid: [0, 0, 0, 0, 0, 1],
                ssid: second_ssid.clone(),
                rssi_dbm: -40,
                snr_db: 30,
                channel: types::WlanChan::new(36, types::Cbw::Cbw40),
            ),
        },
    ];
    let client_sme_request = run_while(
        &mut exec,
        &mut test_values.internal_objects.internal_futures,
        existing_connection.iface_sme_stream.next(),
    );
    assert_variant!(
        client_sme_request,
        Some(Ok(fidl_sme::ClientSmeRequest::Scan {
            req, responder
        })) => {
            assert_eq!(req, expected_scan_request);
            let vmo = write_vmo(mock_scan_results).expect("failed to write VMO");
            responder.send(Ok(vmo)).expect("failed to send scan data");
        }
    );

    // State machine disconnects due to new connect request.
    let client_sme_request = run_while(
        &mut exec,
        &mut test_values.internal_objects.internal_futures,
        existing_connection.state_machine_sme_stream.next(),
    );
    assert_variant!(
        client_sme_request,
        Some(Ok(fidl_sme::ClientSmeRequest::Disconnect {
            reason, responder
        })) => {
            assert_eq!(fidl_sme::UserDisconnectReason::FidlConnectRequest, reason);
            assert!(responder.send().is_ok());
        }
    );

    // Listener update should now show first network as disconnected, second as still connecting.
    let fidl_policy::ClientStateSummary { state, networks, .. } = get_client_state_update(
        &mut exec,
        &mut test_values.internal_objects.internal_futures,
        &mut test_values.external_interfaces.listener_updates_stream,
    );
    assert_eq!(state.unwrap(), fidl_policy::WlanClientState::ConnectionsEnabled);
    let networks = networks.unwrap();
    assert_eq!(networks.len(), 2);
    for network in networks {
        if network.id.unwrap() == network_id.clone() {
            assert_eq!(network.state.unwrap(), types::ConnectionState::Connecting)
        } else {
            assert_eq!(network.state.unwrap(), types::ConnectionState::Disconnected)
        }
    }

    // State machine connects, create new connect txt handle. Verify the SME connect request is for
    // the much better BSS.
    let client_sme_request = run_while(
        &mut exec,
        &mut test_values.internal_objects.internal_futures,
        existing_connection.state_machine_sme_stream.next(),
    );
    let connect_txn_handle = assert_variant!(
        client_sme_request,
        Some(Ok(fidl_sme::ClientSmeRequest::Connect {
            req, txn, control_handle: _
        })) => {
            assert_eq!(req.ssid, second_ssid.clone());
            assert_eq!(TEST_CREDS.wpa_pass_min.sme.clone(), req.authentication.credentials);
            assert_eq!(req.bss_description.bssid, [0, 0, 0, 0, 0, 1]);
            let (_stream, ctrl) = txn.expect("connect txn unused")
                .into_stream_and_control_handle().expect("error accessing control handle");
            ctrl
        }
    );
    connect_txn_handle
        .send_on_connect_result(&fidl_sme::ConnectResult {
            code: fidl_fuchsia_wlan_ieee80211::StatusCode::Success,
            is_credential_rejected: false,
            is_reconnect: false,
        })
        .expect("failed to send connection completion");

    // Update sme connect transaction handle. The previous handle was not used, but had to be held
    // to prevent the channel from closing.
    existing_connection.connect_txn_handle = connect_txn_handle;

    // Listener update should now show only the second network with connected state.
    let fidl_policy::ClientStateSummary { state, networks, .. } = get_client_state_update(
        &mut exec,
        &mut test_values.internal_objects.internal_futures,
        &mut test_values.external_interfaces.listener_updates_stream,
    );
    assert_eq!(state.unwrap(), fidl_policy::WlanClientState::ConnectionsEnabled);
    let mut networks = networks.unwrap();
    assert_eq!(networks.len(), 1);
    let network = networks.pop().unwrap();
    assert_eq!(network.id.unwrap().ssid, second_ssid.to_vec());
    assert_eq!(network.state.unwrap(), types::ConnectionState::Connected);
}

/// Tests that an idle interface is autoconnect to a saved network if available.
#[fuchsia::test]
fn test_autoconnect_to_saved_network() {
    let mut exec = fasync::TestExecutor::new();
    let mut test_values = test_setup(&mut exec, RECOVERY_PROFILE_EMPTY_STRING, false);

    // No request has been sent yet. Future should be idle.
    assert_variant!(
        exec.run_until_stalled(&mut test_values.internal_objects.internal_futures),
        Poll::Pending
    );

    // Initial update should reflect that client connections are disabled.
    let fidl_policy::ClientStateSummary { state, networks, .. } = get_client_state_update(
        &mut exec,
        &mut test_values.internal_objects.internal_futures,
        &mut test_values.external_interfaces.listener_updates_stream,
    );
    assert_eq!(state.unwrap(), fidl_policy::WlanClientState::ConnectionsDisabled);
    assert_eq!(networks.unwrap().len(), 0);

    // Generate network ID.
    let network_id =
        fidl_policy::NetworkIdentifier { ssid: TEST_SSID.clone().into(), type_: Saved::Wpa2 };
    let network_config = fidl_policy::NetworkConfig {
        id: Some(network_id.clone()),
        credential: Some(TEST_CREDS.wpa_pass_min.policy.clone()),
        ..Default::default()
    };

    // Save the network.
    let save_fut = test_values.external_interfaces.client_controller.save_network(&network_config);
    let save_fut = pin!(save_fut);

    // Save request returns.
    let save_fut_resp =
        run_while(&mut exec, &mut test_values.internal_objects.internal_futures, save_fut);
    assert_variant!(save_fut_resp, Ok(Ok(())));

    // Enable client connections.
    let mut iface_sme_stream = prepare_client_interface(&mut exec, &mut test_values);

    // Passive scanning should start due to the idle interface and saved network.
    let expected_scan_request = fidl_sme::ScanRequest::Passive(fidl_sme::PassiveScanRequest);
    let mutual_security_protocols = security_protocols_from_protection(Scanned::Wpa2Personal);
    assert!(!mutual_security_protocols.is_empty(), "no mutual security protocols");
    let mock_scan_results = vec![fidl_sme::ScanResult {
        compatibility: Some(Box::new(fidl_sme::Compatibility { mutual_security_protocols })),
        timestamp_nanos: zx::MonotonicInstant::get().into_nanos(),
        bss_description: random_fidl_bss_description!(
            protection =>  wlan_common::test_utils::fake_stas::FakeProtectionCfg::from(Scanned::Wpa2Personal),
            bssid: [0, 0, 0, 0, 0, 0],
            ssid: TEST_SSID.clone(),
            rssi_dbm: 10,
            snr_db: 10,
            channel: types::WlanChan::new(1, types::Cbw::Cbw20),
        ),
    }];
    let next_sme_stream_req = run_while(
        &mut exec,
        &mut test_values.internal_objects.internal_futures,
        iface_sme_stream.next(),
    );
    assert_variant!(
        next_sme_stream_req,
        Some(Ok(fidl_sme::ClientSmeRequest::Scan {
            req, responder
        })) => {
            assert_eq!(req, expected_scan_request);
            let vmo = write_vmo(mock_scan_results.clone()).expect("failed to write VMO");
            responder.send(Ok(vmo)).expect("failed to send scan data");
        }
    );

    // At least one active scan will follow.
    let mutual_security_protocols = security_protocols_from_protection(Scanned::Wpa2Personal);
    assert!(!mutual_security_protocols.is_empty(), "no mutual security protocols");

    let next_sme_stream_req = run_while(
        &mut exec,
        &mut test_values.internal_objects.internal_futures,
        iface_sme_stream.next(),
    );
    let active_scan_channels = assert_variant!(
        next_sme_stream_req,
        Some(Ok(fidl_sme::ClientSmeRequest::Scan {
            req, responder
        })) => {
            let channels = assert_variant!(req, fidl_sme::ScanRequest::Active(fidl_sme::ActiveScanRequest {
                mut ssids, channels
            }) => {
                assert_eq!(ssids.len(), 1);
                assert_eq!(ssids.pop().unwrap(), TEST_SSID.to_vec());
                channels
            });
            let vmo = write_vmo(mock_scan_results.clone()).expect("failed to write VMO");
            responder.send(Ok(vmo)).expect("failed to send scan data");
            channels
        }
    );

    // Determine if active scan was the hidden network probabilistic scan or the connection scan.
    match active_scan_channels[..] {
        [] => {
            debug!(
                "Probabilistic active scan was selected. A second active scan, with the channel
            populated, will follow in order to complete the connection."
            );
            let expected_scan_request =
                fidl_sme::ScanRequest::Active(fidl_sme::ActiveScanRequest {
                    ssids: vec![TEST_SSID.to_vec()],
                    channels: vec![1],
                });
            let next_sme_stream_req = run_while(
                &mut exec,
                &mut test_values.internal_objects.internal_futures,
                iface_sme_stream.next(),
            );
            assert_variant!(
                next_sme_stream_req,
                Some(Ok(fidl_sme::ClientSmeRequest::Scan {
                    req, responder
                })) => {
                    assert_eq!(req, expected_scan_request);
                    let vmo = write_vmo(mock_scan_results).expect("failed to write VMO");
                    responder.send(Ok(vmo)).expect("failed to send scan data");
                }
            );
        }
        [1] => {
            debug!("Probabilistic active scan not selected. Proceeding with connection.");
        }
        _ => panic!("Unexpected channel set for active scan: {:?}", active_scan_channels),
    }

    // Expect to get an SME request for state machine creation.
    let next_device_monitor_req = run_while(
        &mut exec,
        &mut test_values.internal_objects.internal_futures,
        test_values.external_interfaces.monitor_service_stream.next(),
    );
    let sme_server = assert_variant!(
        next_device_monitor_req,
        Some(Ok(fidl_fuchsia_wlan_device_service::DeviceMonitorRequest::GetClientSme {
            iface_id: TEST_CLIENT_IFACE_ID, sme_server, responder
        })) => {
            // Send back a positive acknowledgement.
            assert!(responder.send(Ok(())).is_ok());
            sme_server
        }
    );
    let mut state_machine_sme_stream = sme_server.into_stream();

    // State machine does an initial disconnect. Ack.
    let next_sme_req = run_while(
        &mut exec,
        &mut test_values.internal_objects.internal_futures,
        state_machine_sme_stream.next(),
    );
    assert_variant!(
        next_sme_req,
        Some(Ok(fidl_sme::ClientSmeRequest::Disconnect {
            reason, responder
        })) => {
            assert_eq!(fidl_sme::UserDisconnectReason::Startup, reason);
            assert!(responder.send().is_ok());
        }
    );

    // Check for a listener update saying client connections are enabled.
    let fidl_policy::ClientStateSummary { state, networks, .. } = get_client_state_update(
        &mut exec,
        &mut test_values.internal_objects.internal_futures,
        &mut test_values.external_interfaces.listener_updates_stream,
    );
    assert_eq!(state.unwrap(), fidl_policy::WlanClientState::ConnectionsEnabled);
    assert_eq!(networks.unwrap().len(), 0);

    // Check for listener update saying we're connecting.
    let fidl_policy::ClientStateSummary { state, networks, .. } = get_client_state_update(
        &mut exec,
        &mut test_values.internal_objects.internal_futures,
        &mut test_values.external_interfaces.listener_updates_stream,
    );
    assert_eq!(state.unwrap(), fidl_policy::WlanClientState::ConnectionsEnabled);
    let mut networks = networks.unwrap();
    assert_eq!(networks.len(), 1);
    let network = networks.pop().unwrap();
    assert_eq!(network.state.unwrap(), types::ConnectionState::Connecting);
    assert_eq!(network.id.unwrap(), network_id.clone());

    // State machine connects
    let next_sme_req = run_while(
        &mut exec,
        &mut test_values.internal_objects.internal_futures,
        state_machine_sme_stream.next(),
    );
    let connect_txn_handle = assert_variant!(
        next_sme_req,
        Some(Ok(fidl_sme::ClientSmeRequest::Connect {
            req, txn, control_handle: _
        })) => {
            assert_eq!(req.ssid, TEST_SSID.clone());
            assert_eq!(TEST_CREDS.wpa_pass_min.sme.clone(), req.authentication.credentials);
            let (_stream, ctrl) = txn.expect("connect txn unused")
                .into_stream_and_control_handle().expect("error accessing control handle");
            ctrl
        }
    );
    connect_txn_handle
        .send_on_connect_result(&fidl_sme::ConnectResult {
            code: fidl_fuchsia_wlan_ieee80211::StatusCode::Success,
            is_credential_rejected: false,
            is_reconnect: false,
        })
        .expect("failed to send connection completion");

    // Check for a listener update saying we're Connected.
    let fidl_policy::ClientStateSummary { state, networks, .. } = get_client_state_update(
        &mut exec,
        &mut test_values.internal_objects.internal_futures,
        &mut test_values.external_interfaces.listener_updates_stream,
    );
    assert_eq!(state.unwrap(), fidl_policy::WlanClientState::ConnectionsEnabled);
    let mut networks = networks.unwrap();
    assert_eq!(networks.len(), 1);
    let network = networks.pop().unwrap();
    assert_eq!(network.state.unwrap(), types::ConnectionState::Connected);
    assert_eq!(network.id.unwrap(), network_id.clone());
}

/// Tests that, after multiple disconnects, we'll continue to scan and connect to a hidden network.
/// Use "stop_client_connections" to initiate the disconnect, so that this test isn't affected
/// by changes to reconnect backoffs and/or number of reconnects before scan.
#[fuchsia::test]
fn test_autoconnect_to_hidden_saved_network_and_reconnect() {
    let mut exec = fasync::TestExecutor::new();
    let mut test_values = test_setup(&mut exec, RECOVERY_PROFILE_EMPTY_STRING, false);

    // No request has been sent yet. Future should be idle.
    assert_variant!(
        exec.run_until_stalled(&mut test_values.internal_objects.internal_futures),
        Poll::Pending
    );

    // Initial update should reflect that client connections are disabled.
    let fidl_policy::ClientStateSummary { state, networks, .. } = get_client_state_update(
        &mut exec,
        &mut test_values.internal_objects.internal_futures,
        &mut test_values.external_interfaces.listener_updates_stream,
    );
    assert_eq!(state.unwrap(), fidl_policy::WlanClientState::ConnectionsDisabled);
    assert_eq!(networks.unwrap().len(), 0);

    // Generate network ID.
    let network_id =
        fidl_policy::NetworkIdentifier { ssid: TEST_SSID.clone().into(), type_: Saved::Wpa2 };
    let network_config = fidl_policy::NetworkConfig {
        id: Some(network_id.clone()),
        credential: Some(TEST_CREDS.wpa_pass_min.policy.clone()),
        ..Default::default()
    };

    // Save the network.
    let save_fut = test_values.external_interfaces.client_controller.save_network(&network_config);
    let save_fut = pin!(save_fut);

    // Save request returns.
    let save_fut_resp =
        run_while(&mut exec, &mut test_values.internal_objects.internal_futures, save_fut);
    assert_variant!(save_fut_resp, Ok(Ok(())));

    // Enter the loop of:
    //  - start client connections
    //  - scan
    //  - connect
    //  - stop client connections
    for connect_disconnect_loop_counter in 1..=10 {
        info!("Starting test loop #{}", connect_disconnect_loop_counter);

        // Enable client connections.
        let mut iface_sme_stream = prepare_client_interface(&mut exec, &mut test_values);

        // Generate mock scan results
        let mutual_security_protocols = security_protocols_from_protection(Scanned::Wpa2Personal);
        assert!(!mutual_security_protocols.is_empty(), "no mutual security protocols");
        let mock_scan_results = vec![fidl_sme::ScanResult {
            compatibility: Some(Box::new(fidl_sme::Compatibility { mutual_security_protocols })),
            timestamp_nanos: zx::MonotonicInstant::get().into_nanos(),
            bss_description: random_fidl_bss_description!(
                protection =>  wlan_common::test_utils::fake_stas::FakeProtectionCfg::from(Scanned::Wpa2Personal),
                bssid: [0, 0, 0, 0, 0, 0],
                ssid: TEST_SSID.clone(),
                rssi_dbm: 10,
                snr_db: 10,
                channel: types::WlanChan::new(1, types::Cbw::Cbw20),
            ),
        }];

        // Because scanning for hidden networks is probabilistic, there will be an unknown number of
        // passive scans before the active scan. At the time of writing, hidden probability will
        // start at 90%, meaning this has a 0.1^6 chance of flaking (one in a million).
        for _i in 1..=7 {
            // First, pop any pending timers to make sure the idle interface scanning mechanism
            // activates now. Do it enough times to also catch any built up timers for sending
            // location sensor results.
            let mut scan_req = None;
            for _j in 1..=10 {
                assert_variant!(
                    exec.run_until_stalled(&mut test_values.internal_objects.internal_futures),
                    Poll::Pending
                );

                // Check for scan requests before waking timers, since scanning has a timeout.
                if let Poll::Ready(req) = exec.run_until_stalled(&mut iface_sme_stream.next()) {
                    scan_req = req;
                    break;
                }

                let _woken_timer = exec.wake_next_timer();
            }

            // If a scan request came from the above loop, use it. If not, run the wlancfg future
            // until a scan request comes in.
            let next_sme_stream_req = scan_req.or_else(|| {
                run_while(
                    &mut exec,
                    &mut test_values.internal_objects.internal_futures,
                    iface_sme_stream.next(),
                )
            });
            debug!("This is scan number {}", _i);
            assert_variant!(
                next_sme_stream_req,
                Some(Ok(fidl_sme::ClientSmeRequest::Scan {
                    req, responder
                })) => {
                    match req {
                        fidl_sme::ScanRequest::Passive(_) => {
                            // This is not the active scan we're looking for, continue
                            debug!("Got a passive scan, continuing");
                            let vmo = write_vmo(vec![]).expect("failed to write VMO");
                            responder
                                .send(Ok(vmo))
                                .expect("failed to send scan data");
                            continue;
                        }
                        fidl_sme::ScanRequest::Active(active_req) => {
                            assert_eq!(active_req.ssids.len(), 1);
                            assert_eq!(active_req.ssids[0], TEST_SSID.to_vec());
                            assert_eq!(active_req.channels.len(), 0);
                            let vmo = write_vmo(mock_scan_results).expect("failed to write VMO");
                            responder
                                .send(Ok(vmo))
                                .expect("failed to send scan data");
                            break;
                        }
                    };
                }
            );
        }

        // Expect to get an SME request for state machine creation.
        let next_device_monitor_req = run_while(
            &mut exec,
            &mut test_values.internal_objects.internal_futures,
            test_values.external_interfaces.monitor_service_stream.next(),
        );
        let sme_server = assert_variant!(
            next_device_monitor_req,
            Some(Ok(fidl_fuchsia_wlan_device_service::DeviceMonitorRequest::GetClientSme {
                iface_id: TEST_CLIENT_IFACE_ID, sme_server, responder
            })) => {
                // Send back a positive acknowledgement.
                assert!(responder.send(Ok(())).is_ok());
                sme_server
            }
        );
        let mut state_machine_sme_stream = sme_server.into_stream();

        // State machine does an initial disconnect. Ack.
        let next_sme_req = run_while(
            &mut exec,
            &mut test_values.internal_objects.internal_futures,
            state_machine_sme_stream.next(),
        );
        assert_variant!(
            next_sme_req,
            Some(Ok(fidl_sme::ClientSmeRequest::Disconnect {
                reason, responder
            })) => {
                assert_eq!(fidl_sme::UserDisconnectReason::Startup, reason);
                assert!(responder.send().is_ok());
            }
        );

        // Check for a listener update saying client connections are enabled.
        let fidl_policy::ClientStateSummary { state, networks, .. } = get_client_state_update(
            &mut exec,
            &mut test_values.internal_objects.internal_futures,
            &mut test_values.external_interfaces.listener_updates_stream,
        );
        assert_eq!(state.unwrap(), fidl_policy::WlanClientState::ConnectionsEnabled);
        assert_eq!(networks.unwrap().len(), 0);

        // Check for listener update saying we're connecting.
        let fidl_policy::ClientStateSummary { state, networks, .. } = get_client_state_update(
            &mut exec,
            &mut test_values.internal_objects.internal_futures,
            &mut test_values.external_interfaces.listener_updates_stream,
        );
        assert_eq!(state.unwrap(), fidl_policy::WlanClientState::ConnectionsEnabled);
        let mut networks = networks.unwrap();
        assert_eq!(networks.len(), 1);
        let network = networks.pop().unwrap();
        assert_eq!(network.state.unwrap(), types::ConnectionState::Connecting);
        assert_eq!(network.id.unwrap(), network_id.clone());

        // State machine connects
        let next_sme_req = run_while(
            &mut exec,
            &mut test_values.internal_objects.internal_futures,
            state_machine_sme_stream.next(),
        );
        let connect_txn_handle = assert_variant!(
            next_sme_req,
            Some(Ok(fidl_sme::ClientSmeRequest::Connect {
                req, txn, control_handle: _
            })) => {
                assert_eq!(req.ssid, TEST_SSID.clone());
                assert_eq!(TEST_CREDS.wpa_pass_min.sme.clone(), req.authentication.credentials);
                let (_stream, ctrl) = txn.expect("connect txn unused")
                    .into_stream_and_control_handle().expect("error accessing control handle");
                ctrl
            }
        );
        connect_txn_handle
            .send_on_connect_result(&fidl_sme::ConnectResult {
                code: fidl_fuchsia_wlan_ieee80211::StatusCode::Success,
                is_credential_rejected: false,
                is_reconnect: false,
            })
            .expect("failed to send connection completion");

        // Check for a listener update saying we're Connected.
        let fidl_policy::ClientStateSummary { state, networks, .. } = get_client_state_update(
            &mut exec,
            &mut test_values.internal_objects.internal_futures,
            &mut test_values.external_interfaces.listener_updates_stream,
        );
        assert_eq!(state.unwrap(), fidl_policy::WlanClientState::ConnectionsEnabled);
        let mut networks = networks.unwrap();
        assert_eq!(networks.len(), 1);
        let network = networks.pop().unwrap();
        assert_eq!(network.state.unwrap(), types::ConnectionState::Connected);
        assert_eq!(network.id.unwrap(), network_id.clone());

        // Turn off client connections via Policy API
        let stop_connections_fut =
            test_values.external_interfaces.client_controller.stop_client_connections();
        let mut stop_connections_fut = pin!(stop_connections_fut);
        assert_variant!(exec.run_until_stalled(&mut stop_connections_fut), Poll::Pending);

        // State machine disconnects
        let next_sme_req = run_while(
            &mut exec,
            &mut test_values.internal_objects.internal_futures,
            state_machine_sme_stream.next(),
        );
        assert_variant!(
            next_sme_req,
            Some(Ok(fidl_sme::ClientSmeRequest::Disconnect {
                reason, responder
            })) => {
                assert_eq!(fidl_sme::UserDisconnectReason::FidlStopClientConnectionsRequest, reason);
                assert!(responder.send().is_ok());
            }
        );

        // Check for a listener update about the disconnect.
        let fidl_policy::ClientStateSummary { state, networks, .. } = get_client_state_update(
            &mut exec,
            &mut test_values.internal_objects.internal_futures,
            &mut test_values.external_interfaces.listener_updates_stream,
        );
        assert_eq!(state.unwrap(), fidl_policy::WlanClientState::ConnectionsEnabled);
        let mut networks = networks.unwrap();
        assert_eq!(networks.len(), 1);
        let network = networks.pop().unwrap();
        assert_eq!(network.state.unwrap(), types::ConnectionState::Disconnected);
        assert_eq!(network.id.unwrap(), network_id.clone());

        // Device monitor gets an iface destruction request
        let iface_destruction_req = run_while(
            &mut exec,
            &mut test_values.internal_objects.internal_futures,
            test_values.external_interfaces.monitor_service_stream.next(),
        );
        assert_variant!(
            iface_destruction_req,
            Some(Ok(fidl_fuchsia_wlan_device_service::DeviceMonitorRequest::DestroyIface {
                req: fidl_fuchsia_wlan_device_service::DestroyIfaceRequest {
                    iface_id: TEST_CLIENT_IFACE_ID
                },
                responder
            })) => {
                assert!(responder.send(
                    zx::sys::ZX_OK
                ).is_ok());
            }
        );

        // Check for a response to the Policy API stop client connections request
        let stop_connections_resp = run_while(
            &mut exec,
            &mut test_values.internal_objects.internal_futures,
            &mut stop_connections_fut,
        );
        assert_variant!(stop_connections_resp, Ok(fidl_policy::RequestStatus::Acknowledged));

        // Check for a listener update saying client connections are disabled.
        let fidl_policy::ClientStateSummary { state, networks, .. } = get_client_state_update(
            &mut exec,
            &mut test_values.internal_objects.internal_futures,
            &mut test_values.external_interfaces.listener_updates_stream,
        );
        assert_eq!(state.unwrap(), fidl_policy::WlanClientState::ConnectionsDisabled);
        assert_eq!(networks.unwrap().len(), 0);
    }
}

fn request_scan_and_reply(
    exec: &mut TestExecutor,
    test_values: &mut TestValues,
    sme_stream: &mut fidl_sme::ClientSmeRequestStream,
    reply: Result<Vec<fidl_sme::ScanResult>, fidl_sme::ScanErrorCode>,
) {
    // Make a scan request.
    let (_iter, server) = create_proxy().expect("failed to create iterator");
    assert_variant!(
        test_values.external_interfaces.client_controller.scan_for_networks(server),
        Ok(())
    );

    // Run the internal futures to route the scan request to SME.
    assert_variant!(
        exec.run_until_stalled(&mut test_values.internal_objects.internal_futures),
        Poll::Pending
    );
    let next_sme_stream_req =
        run_while(exec, &mut test_values.internal_objects.internal_futures, sme_stream.next());

    // Respond with a failure.
    assert_variant!(
        next_sme_stream_req,
        Some(Ok(fidl_sme::ClientSmeRequest::Scan {
            responder, ..
        })) => {
            responder
                .send(
                    reply
                        .map(|results| write_vmo(results)
                        .expect("failed to create scan result VMO"))
                )
                .expect("failed to send scan error");
        }
    );
}

fn expect_destroy_iface_request_and_reply(
    exec: &mut TestExecutor,
    test_values: &mut TestValues,
    expected_iface_id: u16,
    reply: i32,
) {
    let destroy_iface_req = run_while(
        exec,
        &mut test_values.internal_objects.internal_futures,
        test_values.external_interfaces.monitor_service_stream.next(),
    );
    assert_variant!(
        destroy_iface_req,
        Some(Ok(fidl_fuchsia_wlan_device_service::DeviceMonitorRequest::DestroyIface {
            req: fidl_fuchsia_wlan_device_service::DestroyIfaceRequest {iface_id},
            responder
        })) => {
            assert_eq!(iface_id, expected_iface_id);
            responder
                .send(reply)
                .expect("failed to ack destroy iface request")
        }
    );
}

fn inform_watcher_of_client_iface_removal_and_expect_iface_recovery(
    exec: &mut TestExecutor,
    test_values: &mut TestValues,
    expected_phy_id: u16,
    expected_iface_id: u16,
) -> fidl_sme::ClientSmeRequestStream {
    let legacy_client = legacy::IfaceRef::new();
    let listener = device_monitor::Listener::new(
        test_values.external_interfaces.monitor_service_proxy.clone(),
        legacy_client.clone(),
        test_values.internal_objects.phy_manager.clone(),
        test_values.internal_objects.iface_manager.clone(),
    );
    let remove_iface_event = DeviceWatcherEvent::OnIfaceRemoved { iface_id: expected_iface_id };
    let remove_iface_fut = device_monitor::handle_event(&listener, remove_iface_event);
    let mut remove_iface_fut = pin!(remove_iface_fut);
    assert_variant!(exec.run_until_stalled(&mut remove_iface_fut), Poll::Pending);

    let iface_creation_req = run_while(
        exec,
        &mut test_values.internal_objects.internal_futures,
        test_values.external_interfaces.monitor_service_stream.next(),
    );
    assert_variant!(
        iface_creation_req,
        Some(Ok(fidl_fuchsia_wlan_device_service::DeviceMonitorRequest::CreateIface {
            req: fidl_fuchsia_wlan_device_service::CreateIfaceRequest {
                phy_id, role: fidl_common::WlanMacRole::Client, sta_addr: [0, 0, 0, 0, 0, 0]
            },
            responder
        })) => {
            assert_eq!(phy_id, expected_phy_id);
            assert!(responder.send(
                zx::sys::ZX_OK,
                Some(&fidl_fuchsia_wlan_device_service::CreateIfaceResponse {iface_id: expected_iface_id})
            ).is_ok());
        }
    );

    // Expect a feature support query as part of the interface creation
    let feature_support_req = run_while(
        exec,
        &mut test_values.internal_objects.internal_futures,
        test_values.external_interfaces.monitor_service_stream.next(),
    );
    assert_variant!(
        feature_support_req,
        Some(Ok(fidl_fuchsia_wlan_device_service::DeviceMonitorRequest::GetFeatureSupport {
            iface_id, feature_support_server, responder
        })) => {
            assert_eq!(iface_id, expected_iface_id);
            assert!(responder.send(Ok(())).is_ok());
            let (mut stream, _handle) = feature_support_server.into_stream_and_control_handle().unwrap();

            // Send back feature support information
            let security_support_req = run_while(
                exec,
                &mut test_values.internal_objects.internal_futures,
                stream.next(),
            );
            assert_variant!(
                security_support_req,
                Some(Ok(fidl_sme::FeatureSupportRequest::QuerySecuritySupport {
                    responder
                })) => {
                    assert!(responder.send(Ok(&security_support_with_wpa3())).is_ok());
                }
            );
        }
    );

    // Expect that we have requested a client SME proxy as part of interface creation
    let sme_req = run_while(
        exec,
        &mut test_values.internal_objects.internal_futures,
        test_values.external_interfaces.monitor_service_stream.next(),
    );
    let sme_server = assert_variant!(
        sme_req,
        Some(Ok(fidl_fuchsia_wlan_device_service::DeviceMonitorRequest::GetClientSme {
            iface_id, sme_server, responder
        })) => {
            assert_eq!(iface_id, expected_iface_id);
            // Send back a positive acknowledgement.
            assert!(responder.send(Ok(())).is_ok());
            sme_server
        }
    );
    let sme_stream = sme_server.into_stream();

    // There will be another security support query as part of adding the interface to iface_manager
    let feature_support_req = run_while(
        exec,
        &mut test_values.internal_objects.internal_futures,
        test_values.external_interfaces.monitor_service_stream.next(),
    );
    assert_variant!(
        feature_support_req,
        Some(Ok(fidl_fuchsia_wlan_device_service::DeviceMonitorRequest::GetFeatureSupport {
            iface_id, feature_support_server, responder
        })) => {
            assert_eq!(iface_id, expected_iface_id);
            assert!(responder.send(Ok(())).is_ok());
            let (mut stream, _handle) = feature_support_server.into_stream_and_control_handle().unwrap();

            // Send back feature support information
            let security_support_req = run_while(
                exec,
                &mut test_values.internal_objects.internal_futures,
                stream.next(),
            );
            assert_variant!(
                security_support_req,
                Some(Ok(fidl_sme::FeatureSupportRequest::QuerySecuritySupport {
                    responder
                })) => {
                    assert!(responder.send(Ok(&security_support_with_wpa3())).is_ok());
                }
            );
        }
    );

    // Run the iface removal notification to completion.  This is subtle, but the device watcher
    // holds a lock on the IfaceManager until this future completes.
    assert_variant!(
        exec.run_until_stalled(&mut test_values.internal_objects.internal_futures),
        Poll::Pending
    );
    assert_variant!(exec.run_until_stalled(&mut remove_iface_fut), Poll::Ready(()));

    sme_stream
}

#[test_case(Err(fidl_sme::ScanErrorCode::ShouldWait), 9; "should wait")]
#[test_case(Err(fidl_sme::ScanErrorCode::CanceledByDriverOrFirmware), 9; "cancelled")]
#[test_case(Err(fidl_sme::ScanErrorCode::InternalError), 5; "internal error")]
#[test_case(Err(fidl_sme::ScanErrorCode::InternalMlmeError), 5; "mlme error")]
#[test_case(Err(fidl_sme::ScanErrorCode::NotSupported), 5; "not supported")]
#[test_case(Ok(vec![]), 10; "empty results")]
#[fuchsia::test(add_test_attr = false)]
fn test_scan_recovery(
    scan_result: Result<Vec<fidl_sme::ScanResult>, fidl_sme::ScanErrorCode>,
    defect_threshold: usize,
) {
    let mut exec = fasync::TestExecutor::new();
    let mut test_values = test_setup(&mut exec, RECOVERY_PROFILE_THRESHOLDED_RECOVERY, true);

    // No request has been sent yet. Future should be idle.
    assert_variant!(
        exec.run_until_stalled(&mut test_values.internal_objects.internal_futures),
        Poll::Pending
    );

    // Enable client connections.
    let mut iface_sme_stream = prepare_client_interface(&mut exec, &mut test_values);

    // Return the specified scan result until the recovery threshold is triggered.
    for _ in 0..defect_threshold {
        request_scan_and_reply(
            &mut exec,
            &mut test_values,
            &mut iface_sme_stream,
            scan_result.clone(),
        );
    }

    // Recovery should now recommend that the interface be destroyed.
    expect_destroy_iface_request_and_reply(
        &mut exec,
        &mut test_values,
        TEST_CLIENT_IFACE_ID,
        zx::sys::ZX_OK,
    );

    // Inform the internals that the interface was removed.  The internals will then recreate the
    // interface.
    let _ = inform_watcher_of_client_iface_removal_and_expect_iface_recovery(
        &mut exec,
        &mut test_values,
        TEST_PHY_ID,
        TEST_CLIENT_IFACE_ID,
    );
}

// This function manages interactions from IfaceManager and a client state machine as wlancfg
// attempts to get a client interface connected to a network.  In the process, it injects fake scan
// results and positively acknowledges disconnect requests, but all connection requests are
// rejected.
fn reject_connect_requests(
    exec: &mut TestExecutor,
    test_values: &mut TestValues,
    iface_manager_sme_stream: &mut fidl_sme::ClientSmeRequestStream,
    state_machine_sme_stream: &mut Option<fidl_sme::ClientSmeRequestStream>,
    scan_results: Vec<fidl_sme::ScanResult>,
    connection_failures_to_inject: usize,
) {
    // The network selection process will scan and the client state machine will make connect and
    // disconnect requests.  The order here doesn't matter, but the scan and disconnects must be
    // allowed to succeed so that the process can continue while the connect requests should all
    // result in failure to instigate the recovery process.
    //
    // Note that the scans will come from the IfaceManager's SME while disconnect and connect
    // requests will come from the state machine's SME.  This is slightly complicated since the
    // state machine SME proxy is dropped and recreated every time the state machine exits.
    let mut connect_failures = 0;
    loop {
        // Once the connect failure threshold has been hit, break out to validate the recovery
        // process.
        if connect_failures == connection_failures_to_inject {
            break;
        }

        // Progress the internal futures to run the iface manager, client state machine, scan
        // manager, and network selection logic.
        assert_variant!(
            exec.run_until_stalled(&mut test_values.internal_objects.internal_futures),
            Poll::Pending
        );

        // Check if there are any new SME proxy requests.  If there are, this indicates that a new
        // state machine is being created and we should no longer care about the old state machine
        // client interface.
        if let Poll::Ready(req) = exec
            .run_until_stalled(&mut test_values.external_interfaces.monitor_service_stream.next())
        {
            match req {
                Some(Ok(
                    fidl_fuchsia_wlan_device_service::DeviceMonitorRequest::GetClientSme {
                        sme_server,
                        responder,
                        ..
                    },
                )) => {
                    assert!(responder.send(Ok(())).is_ok());
                    *state_machine_sme_stream = Some(sme_server.into_stream());
                }
                other => panic!("Unexpected DeviceMonitor operation: {:?}", other),
            }
            continue;
        }

        // Check if there are any scan requests from the IfaceManager SME.  This indicates that
        // network selection is in progress.
        if let Poll::Ready(req) = exec.run_until_stalled(&mut iface_manager_sme_stream.next()) {
            match req {
                Some(Ok(fidl_sme::ClientSmeRequest::Scan { responder, .. })) => {
                    let vmo = write_vmo(scan_results.clone()).expect("failed to write VMO");
                    responder.send(Ok(vmo)).expect("failed to send scan data");
                }
                other => panic!("Only scans were expected from the IfaceManager SME: {:?}", other),
            }

            continue;
        }

        // Check for any disconnect or connect requests from the state machine.
        if let Some(mut sme_stream) = state_machine_sme_stream.take() {
            if let Poll::Ready(req) = exec.run_until_stalled(&mut sme_stream.next()) {
                match req {
                    Some(Ok(fidl_sme::ClientSmeRequest::Connect { txn, .. })) => {
                        // If there is a connect request, send back a failure.
                        let (_, connect_txn_handle) = txn
                            .expect("connect txn unused")
                            .into_stream_and_control_handle()
                            .expect("error accessing control handle");

                        connect_txn_handle
                            .send_on_connect_result(&fidl_sme::ConnectResult {
                                code: fidl_ieee80211::StatusCode::RefusedReasonUnspecified,
                                is_credential_rejected: false,
                                is_reconnect: false,
                            })
                            .expect("failed to send connect response");

                        // Increment the number of connect failures.
                        connect_failures += 1;

                        // Wait for the connect backoff timer to expire.
                        let _ = exec.wake_next_timer();
                    }
                    Some(Ok(fidl_sme::ClientSmeRequest::Disconnect { responder, .. })) => {
                        responder.send().expect("failed to send disconnect response");
                    }
                    other => panic!("Unexpected SME operation: {:?}", other),
                }
                *state_machine_sme_stream = Some(sme_stream);
                continue;
            }
        }
    }
}

#[fuchsia::test]
fn test_connect_failure_recovery() {
    let mut exec = fasync::TestExecutor::new();
    let mut test_values = test_setup(&mut exec, RECOVERY_PROFILE_THRESHOLDED_RECOVERY, true);

    // For the purpose of this test, use a mock open network.
    let network_id = fidl_policy::NetworkIdentifier {
        ssid: TEST_SSID.to_vec(),
        type_: fidl_policy::SecurityType::None,
    };
    let network_config = fidl_policy::NetworkConfig {
        id: Some(network_id.clone()),
        credential: Some(TEST_CREDS.none.policy.clone()),
        ..Default::default()
    };
    let scan_results = vec![fidl_sme::ScanResult {
        compatibility: Some(Box::new(fidl_sme::Compatibility {
            mutual_security_protocols: vec![fidl_common_security::Protocol::Open],
        })),
        timestamp_nanos: zx::MonotonicInstant::get().into_nanos(),
        bss_description: random_fidl_bss_description!(
            protection =>  wlan_common::test_utils::fake_stas::FakeProtectionCfg::from(fidl_sme::Protection::Open),
            bssid: [0, 0, 0, 0, 0, 0],
            ssid: TEST_SSID.clone(),
            rssi_dbm: 10,
            snr_db: 10,
            channel: types::WlanChan::new(1, types::Cbw::Cbw20),
        ),
    }];

    // No request has been sent yet. Future should be idle.
    assert_variant!(
        exec.run_until_stalled(&mut test_values.internal_objects.internal_futures),
        Poll::Pending
    );

    // Enable client connections.
    let mut iface_manager_sme_stream = prepare_client_interface(&mut exec, &mut test_values);

    // Save the network
    let save_fut = test_values.external_interfaces.client_controller.save_network(&network_config);
    let save_fut = pin!(save_fut);

    // Continue processing the save request. Connect process starts, and save request returns once
    // the scan has been queued.
    let save_resp =
        run_while(&mut exec, &mut test_values.internal_objects.internal_futures, save_fut);
    assert_variant!(save_resp, Ok(Ok(())));

    // Reject connect requests until the recovery threshold has been hit.
    let mut state_machine_sme_stream = None;
    reject_connect_requests(
        &mut exec,
        &mut test_values,
        &mut iface_manager_sme_stream,
        &mut state_machine_sme_stream,
        scan_results.clone(),
        recovery::CONNECT_FAILURE_RECOVERY_THRESHOLD,
    );

    // Recovery should now recommend that the interface be destroyed.
    expect_destroy_iface_request_and_reply(
        &mut exec,
        &mut test_values,
        TEST_CLIENT_IFACE_ID,
        zx::sys::ZX_OK,
    );

    // Inform the internals that the interface was removed.  The internals will then recreate the
    // interface.
    let _ = inform_watcher_of_client_iface_removal_and_expect_iface_recovery(
        &mut exec,
        &mut test_values,
        TEST_PHY_ID,
        TEST_CLIENT_IFACE_ID,
    );
}
