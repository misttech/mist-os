// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![cfg(test)]

use fidl::endpoints::{DiscoverableProtocolMarker, RequestStream};
use fidl::AsHandleRef as _;
use fuchsia_component::server::{ServiceFs, ServiceFsDir, ServiceObj};
use futures::StreamExt as _;
use netemul::{TestRealm, TestSandbox};
use netstack_testing_common::realms::{constants, Netstack, NetstackVersion, TestSandboxExt as _};
use netstack_testing_macros::netstack_test;
use std::borrow::Cow;
use {fidl_fuchsia_netemul as fnetemul, fidl_fuchsia_posix_socket as fposix_socket};

const MOCK_SERVICES_NAME: &str = "mock";
const CONFIG_PATH: &str = "/pkg/data/netstack.persist";
const NETSTACK_SERVICE_NAME: &str = "netstack";

fn create_netstack_with_mock_endpoint<'s, RS: RequestStream + 'static, N: Netstack>(
    sandbox: &'s TestSandbox,
    service_name: String,
    name: &'s str,
) -> (TestRealm<'s>, ServiceFs<ServiceObj<'s, RS>>)
where
    RS::Protocol: DiscoverableProtocolMarker,
{
    let mut netstack: fnetemul::ChildDef =
        netstack_testing_common::realms::KnownServiceProvider::Netstack(
            match N::VERSION {
                // The prod ns2 has a route for
                // fuchsia.scheduler.deprecated.ProfileProvider which is needed for tests
                // in this suite.
                NetstackVersion::Netstack2 { tracing: false, fast_udp: false } => NetstackVersion::ProdNetstack2,
                v @ NetstackVersion::Netstack3 => v,
                v @ (NetstackVersion::Netstack2 { tracing: _, fast_udp: _ }
                | NetstackVersion::ProdNetstack2
                | NetstackVersion::ProdNetstack3
                ) => {
                    panic!("netstack_test should only be parameterized with Netstack2 or Netstack3: got {:?}", v);
                }
            }
        )
            .into();
    {
        let fnetemul::ChildUses::Capabilities(capabilities) =
            netstack.uses.as_mut().expect("empty uses");
        capabilities.push(fnetemul::Capability::ChildDep(fnetemul::ChildDep {
            name: Some(MOCK_SERVICES_NAME.to_string()),
            capability: Some(fnetemul::ExposedCapability::Protocol(service_name.clone())),
            ..Default::default()
        }));
    }

    let (mock_dir, server_end) = fidl::endpoints::create_endpoints();
    let mut fs = ServiceFs::new();
    let _: &mut ServiceFsDir<'_, _> = fs.dir("svc").add_fidl_service_at(service_name, |rs: RS| rs);
    let _: &mut ServiceFs<_> = fs.serve_connection(server_end).expect("serve connection");

    let realm = sandbox
        .create_realm(
            name,
            [
                netstack,
                (&netstack_testing_common::realms::KnownServiceProvider::SecureStash).into(),
                fnetemul::ChildDef {
                    source: Some(fnetemul::ChildSource::Mock(mock_dir)),
                    name: Some(MOCK_SERVICES_NAME.to_string()),
                    ..Default::default()
                },
            ],
        )
        .expect("failed to create realm");

    // Connect to any service to get netstack launched.
    let _: fidl_fuchsia_net_interfaces::StateProxy = realm
        .connect_to_protocol::<fidl_fuchsia_net_interfaces::StateMarker>()
        .expect("connect to protocol");

    (realm, fs)
}

#[netstack_test]
#[variant(N, Netstack)]
async fn ns_sets_thread_profiles<N: Netstack>(name: &str) {
    let sandbox = netemul::TestSandbox::new().expect("failed to create sandbox");
    let (_realm, mut fs) = create_netstack_with_mock_endpoint::<
        fidl_fuchsia_scheduler_deprecated::ProfileProviderRequestStream,
        N,
    >(
        &sandbox,
        fidl_fuchsia_scheduler_deprecated::ProfileProviderMarker::PROTOCOL_NAME.to_string(),
        name,
    );

    let profile_provider_request_stream = fs.next().await.expect("fs terminated unexpectedly");

    #[derive(Default, Debug)]
    struct ExpectProfiles {
        sysmon: bool,
        worker: bool,
    }

    impl ExpectProfiles {
        fn all_done(&self) -> bool {
            let Self { sysmon, worker } = self;
            *sysmon && *worker
        }

        fn update(&mut self, profile: &str) {
            let Self { sysmon, worker } = self;
            match profile {
                "fuchsia.netstack.go-worker" => {
                    *worker = true;
                }
                "fuchsia.netstack.go-sysmon" => {
                    assert!(!*sysmon, "sysmon observed more than once");
                    *sysmon = true;
                }
                other => panic!("unexpected profile {other}"),
            }
        }
    }

    let result = async_utils::fold::fold_while(
        profile_provider_request_stream,
        ExpectProfiles::default(),
        |mut expect, r| {
            let (thread, profile, responder) =
                r.expect("request failure").into_set_profile_by_role().expect("unexpected request");
            expect.update(profile.as_str());
            assert_eq!(
                thread.basic_info().expect("failed to get basic info").rights,
                zx::Rights::TRANSFER | zx::Rights::MANAGE_THREAD
            );
            responder.send(zx::Status::OK.into_raw()).expect("failed to respond");

            futures::future::ready(if expect.all_done() {
                async_utils::fold::FoldWhile::Done(())
            } else {
                async_utils::fold::FoldWhile::Continue(expect)
            })
        },
    )
    .await;
    result.short_circuited().expect("didn't observe all profiles installed");
}

#[netstack_test]
#[variant(N, Netstack)]
async fn ns_requests_inspect_persistence<N: Netstack>(name: &str) {
    let persist_path = format!(
        "{}-netstack",
        fidl_fuchsia_diagnostics_persist::DataPersistenceMarker::PROTOCOL_NAME
    );

    let sandbox = netemul::TestSandbox::new().expect("failed to create sandbox");
    let (_realm, fs) = create_netstack_with_mock_endpoint::<
        fidl_fuchsia_diagnostics_persist::DataPersistenceRequestStream,
        N,
    >(&sandbox, persist_path, name);

    let config = persistence_config::load_configuration_files_from(CONFIG_PATH)
        .expect("load configuration files failed");

    assert_persist_called_for_tags::<N>(fs, &config).await
}

// N.B.: If the test realm has been created with a mock service endpoint for
// fuchsia.diagnostics.persist.DataPersistence, then this function must be
// called to unblock netstack's calls to that mock service.
async fn assert_persist_called_for_tags<N: Netstack>(
    fs: ServiceFs<ServiceObj<'_, fidl_fuchsia_diagnostics_persist::DataPersistenceRequestStream>>,
    config: &persistence_config::Config,
) {
    let tags = config.get(NETSTACK_SERVICE_NAME).expect("config missing netstack service");
    let _: Vec<()> = fs
        .flatten()
        .map(|request| {
            let (tag, responder) =
                request.expect("fs failure").into_persist().expect("unexpected request");
            assert!(tags.contains_key(&persistence_config::Tag::new(tag).expect("invalid tag")));
            responder
                .send(fidl_fuchsia_diagnostics_persist::PersistResult::Queued)
                .expect("failed to respond");
        })
        .take(tags.len())
        .collect()
        .await;
}

#[netstack_test]
#[variant(N, Netstack)]
async fn ns_persist_tags_under_size_limits<N: Netstack>(name: &str) {
    test_persistence::<N, _>(name, |inspect_payload, tag, tag_config| {
        // Convert inspect payload to a JSON string.
        let data = serde_json::to_string(&inspect_payload).expect("serialization failed");

        // Assert data to be persisted obeys size constraints specified in
        // configuration.
        assert!(data.len() > 0);
        assert!(
            data.len() <= tag_config.max_bytes,
            "{}: data = {}, max = {}",
            tag,
            data.len(),
            tag_config.max_bytes
        );
    })
    .await
}

// This test validates that for any given selector in netstack.persist, the root
// inspect node specified in that selector has been persisted in an archivist
// payload.
//
// TODO(https://fxbug.dev/42076420): Note that this test does NOT validate that
// child nodes specified using wildcards (e.g. `Foo/*/*:*`) are present in the
// archivist payload, nor that all child nodes of any given selector are
// persisted. We're still relying on fireteam primaries and netstack developers
// to keep the persist file in sync with our inspect logic.
#[netstack_test]
#[variant(N, Netstack)]
async fn ns_persist_root_inspect_nodes_for_selectors<N: Netstack>(name: &str) {
    test_persistence::<N, _>(name, |inspect_payload, _tag, tag_config| {
        for selector in tag_config.selectors.iter() {
            // Retrieve the root inspect node name from the diagnostics selector.
            let root_node_name = extract_node_subtree_selector(selector)
                .split("/")
                .next()
                .expect("empty node subtree selector");

            // Assert payload has the node name specified in the selector.
            assert_eq!(
                root_node_name,
                &selectors::sanitize_string_for_selectors(&inspect_payload.name)
            );
        }
    })
    .await
}

fn extract_node_subtree_selector(selector: &str) -> &str {
    // Raw selector strings have the schema
    // <type>:<component>:<node_subtree>:<property>. Split the selector into its
    // constituent parts and skip to the node subtree selector.
    selector.split(":").skip(2).next().expect("raw selector does not follow schema")
}

fn persistence_tag_to_ns2_diagnostics_dir(tag: &persistence_config::Tag) -> Cow<'static, str> {
    match tag.as_str() {
        "fidl" => Cow::from("fidlStats"),
        "nics" => Cow::from("interfaces"),
        "runtime" => Cow::from("configuration"),
        other => Cow::from(format!("{other}")),
    }
}

async fn test_persistence<N, F>(name: &str, validate_payload: F)
where
    N: Netstack,
    F: Fn(
        diagnostics_reader::DiagnosticsHierarchy,
        &persistence_config::Tag,
        &persistence_config::TagConfig,
    ) -> (),
{
    let persist_path = format!(
        "{}-netstack",
        fidl_fuchsia_diagnostics_persist::DataPersistenceMarker::PROTOCOL_NAME
    );

    let sandbox = netemul::TestSandbox::new().expect("failed to create sandbox");
    let (realm, fs) = create_netstack_with_mock_endpoint::<
        fidl_fuchsia_diagnostics_persist::DataPersistenceRequestStream,
        N,
    >(&sandbox, persist_path, name);

    let config = persistence_config::load_configuration_files_from(CONFIG_PATH)
        .expect("load configuration files failed");

    assert_persist_called_for_tags::<N>(fs, &config).await;

    // Create a socket to ensure socket Inspect data is available.
    let _socket = realm
        .datagram_socket(fposix_socket::Domain::Ipv4, fposix_socket::DatagramSocketProtocol::Udp)
        .await
        .expect("datagram socket creation failed");

    // The realm moniker is needed to construct the component part of an Inspect
    // selector.
    let moniker = realm.get_moniker().await.expect("get moniker failed");
    let realm_moniker = selectors::sanitize_moniker_for_selectors(&moniker);
    const SANDBOX_MONIKER: &str = "sandbox";
    const NETSTACK_MONIKER: &str = "netstack";

    let tags = config.get(NETSTACK_SERVICE_NAME).expect("service not present");
    for (tag, tag_config) in tags {
        // Modify selectors to use test realm moniker.
        let selectors = tag_config
            .selectors
            .iter()
            // Raw selector strings have the schema
            // <type>:<component>:<subtree>:<property>. Extract the subtree portion
            // of the selector, and combine it with a test realm specific component
            // selector.
            .map(|v| {
                selectors::parse_selector::<selectors::VerboseError>(&format!(
                    "{SANDBOX_MONIKER}/{realm_moniker}/{NETSTACK_MONIKER}:{}",
                    extract_node_subtree_selector(v),
                ))
                .unwrap()
            });

        let inspect_payload = match N::VERSION {
            NetstackVersion::ProdNetstack2 | NetstackVersion::Netstack2 { .. } => {
                let diagnostics_dir = realm.open_diagnostics_directory(NETSTACK_MONIKER).unwrap();
                let subdir = persistence_tag_to_ns2_diagnostics_dir(tag);
                netstack_testing_common::get_deprecated_netstack2_inspect_data(
                    &diagnostics_dir,
                    &subdir,
                    selectors,
                )
                .await
            }
            NetstackVersion::ProdNetstack3 | NetstackVersion::Netstack3 => {
                // Retrieve the inspect payload from the archivist.
                let mut archive_reader = diagnostics_reader::ArchiveReader::new();
                let archive_reader = archive_reader.add_selectors(selectors);
                archive_reader
                    .snapshot::<diagnostics_reader::Inspect>()
                    .await
                    .expect("snapshot failed")
                    .into_iter()
                    .filter_map(|v| v.payload)
                    .next()
                    .expect("no payload in snapshot")
            }
        };

        // Assert on payload.
        validate_payload(inspect_payload, tag, tag_config);
    }
}

#[netstack_test]
#[variant(N, Netstack)]
async fn serves_update_verify<N: Netstack>(name: &str) {
    let sandbox = netemul::TestSandbox::new().expect("create sandbox");
    let realm = sandbox.create_netstack_realm::<N, _>(name).expect("create netstack realm");
    let verifier = realm
        .connect_to_protocol::<fidl_fuchsia_update_verify::NetstackVerifierMarker>()
        .expect("connect to protocol");

    let response = verifier
        .verify(&fidl_fuchsia_update_verify::VerifyOptions::default())
        .await
        .expect("call succeeded");
    assert_eq!(response, Ok(()));
}

#[netstack_test]
#[variant(N, Netstack)]
async fn serves_ota_health_check<N: Netstack>(name: &str) {
    let sandbox = netemul::TestSandbox::new().expect("create sandbox");
    let realm = sandbox.create_netstack_realm::<N, _>(name).expect("create netstack realm");
    let health_check = realm
        .connect_to_protocol::<fidl_fuchsia_update_verify::ComponentOtaHealthCheckMarker>()
        .expect("connect to protocol");

    let response = health_check.get_health_status().await.expect("call succeeded");
    assert_eq!(response, fidl_fuchsia_update_verify::HealthStatus::Healthy);
}

#[netstack_test]
#[variant(N, Netstack)]
async fn emits_logs<N: Netstack>(name: &str) {
    let sandbox = netemul::TestSandbox::new().expect("create sandbox");
    let realm = sandbox.create_netstack_realm::<N, _>(name).expect("create netstack realm");
    // Start the netstack.
    let _ = realm
        .connect_to_protocol::<fidl_fuchsia_net_interfaces::StateMarker>()
        .expect("connect to protocol");

    let netstack_moniker =
        netstack_testing_common::get_component_moniker(&realm, constants::netstack::COMPONENT_NAME)
            .await
            .expect("get netstack moniker");
    let mut stream = diagnostics_reader::ArchiveReader::new()
        .select_all_for_moniker(&netstack_moniker)
        .snapshot_then_subscribe()
        .expect("subscribe to netstack logs");
    let payload = stream
        .next()
        .await
        .expect("netstack should emit logs on startup")
        .expect("extract syslogs from archivist payload");
    assert!(payload.msg().is_some(), "syslog should contain message");
}
