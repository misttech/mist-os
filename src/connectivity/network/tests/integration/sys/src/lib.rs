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
use netstack_testing_common::realms::{Netstack, NetstackVersion, TestSandboxExt as _};
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
                v @ NetstackVersion::Netstack2WithFastUdp | v @ NetstackVersion::ProdNetstack2 => {
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
            ..fnetemul::ChildDep::EMPTY
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
                    ..fnetemul::ChildDef::EMPTY
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
async fn serves_update_verify<N: Netstack>(name: &str) {
    let sandbox = netemul::TestSandbox::new().expect("create sandbox");
    let sandbox = &sandbox;
    let realm = sandbox.create_netstack_realm::<N, _>(name).expect("create netstack realm");
    let verifier = realm
        .connect_to_protocol::<fidl_fuchsia_update_verify::NetstackVerifierMarker>()
        .expect("connect to protocol");

    let response = verifier
        .verify(fidl_fuchsia_update_verify::VerifyOptions::EMPTY)
        .await
        .expect("call succeeded");
    assert_eq!(response, Ok(()));
}
