// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![cfg(test)]

use fidl::{
    endpoints::{DiscoverableProtocolMarker, RequestStream},
    AsHandleRef as _,
};
use fidl_fuchsia_netemul as fnetemul;
use fuchsia_component::server::{ServiceFs, ServiceFsDir, ServiceObj};
use fuchsia_zircon as zx;

use futures::{StreamExt as _, TryStreamExt as _};
use netemul::{TestRealm, TestSandbox};
use netstack_testing_common::realms::{constants, Netstack, NetstackVersion, TestSandboxExt as _};
use netstack_testing_macros::netstack_test;

const MOCK_SERVICES_NAME: &str = "mock";

fn create_netstack_with_mock_endpoint<'s, RS: RequestStream + 'static, N: Netstack>(
    sandbox: &'s TestSandbox,
    service_name: String,
    name: &'s str,
) -> (TestRealm<'s>, ServiceFs<ServiceObj<'s, RS>>)
where
    RS::Protocol: DiscoverableProtocolMarker,
{
    let mut netstack: fnetemul::ChildDef =
        (&netstack_testing_common::realms::KnownServiceProvider::Netstack(
            match N::VERSION {
                // The prod ns2 has a route for
                // fuchsia.scheduler.ProfileProvider which is needed for tests
                // in this suite.
                NetstackVersion::Netstack2 => NetstackVersion::ProdNetstack2,
                v @ NetstackVersion::Netstack3 => v,
                v @ (NetstackVersion::Netstack2WithFastUdp
                | NetstackVersion::ProdNetstack2
                | NetstackVersion::ProdNetstack3) => {
                    panic!("netstack_test should only be parameterized with Netstack2 or Netstack3: got {:?}", v);
                }
            }
        ))
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
async fn ns_sets_thread_profiles<N: Netstack>(name: &str) {
    let sandbox = netemul::TestSandbox::new().expect("failed to create sandbox");
    let (_realm, mut fs) = create_netstack_with_mock_endpoint::<
        fidl_fuchsia_scheduler::ProfileProviderRequestStream,
        N,
    >(
        &sandbox,
        fidl_fuchsia_scheduler::ProfileProviderMarker::PROTOCOL_NAME.to_string(),
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
    assert_persist_called::<N>(fs).await
}

async fn assert_persist_called<N: Netstack>(
    fs: ServiceFs<ServiceObj<'_, fidl_fuchsia_diagnostics_persist::DataPersistenceRequestStream>>,
) {
    // And expect that we'll see a connection to profile provider.
    let (tag, responder) = fs
        .flatten()
        .try_next()
        .await
        .expect("fs failure")
        .expect("fs terminated unexpectedly")
        .into_persist()
        .expect("unexpected request");

    assert_eq!(tag, "netstack-counters");
    responder
        .send(fidl_fuchsia_diagnostics_persist::PersistResult::Queued)
        .expect("failed to respond");
}

#[netstack_test]
async fn ns_persist_counters_under_size_limit<N: Netstack>(name: &str) {
    let persist_path = format!(
        "{}-netstack",
        fidl_fuchsia_diagnostics_persist::DataPersistenceMarker::PROTOCOL_NAME
    );

    let sandbox = netemul::TestSandbox::new().expect("failed to create sandbox");
    let (realm, fs) = create_netstack_with_mock_endpoint::<
        fidl_fuchsia_diagnostics_persist::DataPersistenceRequestStream,
        N,
    >(&sandbox, persist_path, name);

    assert_persist_called::<N>(fs).await;

    // Load netstack's persistence configuration file.
    const CONFIG_PATH: &str = "/pkg/data/netstack.persist";
    let config = persistence_config::load_configuration_files_from(CONFIG_PATH)
        .expect("load configuration files failed");

    // Retrieve netstack's persisted Inspect selectors.
    const NETSTACK_SERVICE_NAME: &str = "netstack";
    const NETSTACK_COUNTERS_TAG: &str = "netstack-counters";
    let tags = config.get(NETSTACK_SERVICE_NAME).expect("service not present");
    let tag_config = tags.get(NETSTACK_COUNTERS_TAG).expect("tag not present");

    // The realm moniker is needed to construct the component part of an Inspect
    // selector.
    let moniker = realm.get_moniker().await.expect("get moniker failed");
    let realm_moniker = selectors::sanitize_moniker_for_selectors(&moniker);
    const SANDBOX_MONIKER: &str = "sandbox";
    const NETSTACK_MONIKER: &str = "netstack";

    // Modify selectors to use test realm moniker.
    let selectors = tag_config
        .selectors
        .iter()
        // Raw selector strings have the schema
        // <type>:<component>:<subtree>:<property>. Extract the subtree portion
        // of the selector, and combine it with a test realm specific component
        // selector.
        .map(|v| {
            diagnostics_reader::ComponentSelector::new(vec![
                SANDBOX_MONIKER.to_string(),
                realm_moniker.clone(),
                NETSTACK_MONIKER.to_string(),
            ])
            .with_tree_selector(
                v.split(":")
                    .skip(2)
                    .next()
                    .expect("raw selector string does not follow schema")
                    .to_string(),
            )
        });

    // Retrieve the data associated with selectors from the archivist.
    let mut archive_reader = diagnostics_reader::ArchiveReader::new();
    let archive_reader = archive_reader.add_selectors(selectors).retry_if_empty(true);
    let data = archive_reader
        .snapshot_raw::<diagnostics_reader::Inspect, serde_json::Value>()
        .await
        .expect("snapshot raw failed");

    // Assert data to be persisted obeys size constraints specified in
    // configuration.
    let data = data.to_string();
    assert!(data.len() > 0);
    assert!(data.len() <= tag_config.max_bytes);
}

#[netstack_test]
async fn serves_update_verify<N: Netstack>(name: &str) {
    let sandbox = netemul::TestSandbox::new().expect("create sandbox");
    let sandbox = &sandbox;
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
