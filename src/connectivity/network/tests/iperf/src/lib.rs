// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![cfg(test)]

use fidl::endpoints::ProtocolMarker as _;
use fidl_fuchsia_netemul as fnetemul;
use futures::{FutureExt as _, StreamExt as _};
use netstack_testing_common::{
    get_component_moniker,
    realms::{constants, Netstack, TestSandboxExt as _},
    wait_for_component_stopped,
};
use netstack_testing_macros::netstack_test;
use std::pin::pin;
use test_case::test_case;

const IPERF_URL: &str = "#meta/iperf.cm";
const NAME_PROVIDER_URL: &str = "#meta/device-name-provider.cm";
const NAME_PROVIDER_MONIKER: &str = "device-name-provider";

fn iperf_component(
    name: &str,
    program_args: impl IntoIterator<Item = &'static str>,
    eager_startup: bool,
) -> fnetemul::ChildDef {
    fnetemul::ChildDef {
        source: Some(fnetemul::ChildSource::Component(IPERF_URL.to_string())),
        name: Some(name.to_string()),
        program_args: Some(program_args.into_iter().map(Into::into).collect::<Vec<_>>()),
        eager: Some(eager_startup),
        uses: Some(fnetemul::ChildUses::Capabilities(vec![
            fnetemul::Capability::LogSink(fnetemul::Empty),
            fnetemul::Capability::ChildDep(fnetemul::ChildDep {
                name: Some(constants::netstack::COMPONENT_NAME.to_string()),
                capability: Some(fnetemul::ExposedCapability::Protocol(
                    fidl_fuchsia_posix_socket::ProviderMarker::DEBUG_NAME.to_string(),
                )),
                ..Default::default()
            }),
            fnetemul::Capability::ChildDep(fnetemul::ChildDep {
                name: Some(NAME_PROVIDER_MONIKER.to_string()),
                capability: Some(fnetemul::ExposedCapability::Protocol(
                    fidl_fuchsia_device::NameProviderMarker::DEBUG_NAME.to_string(),
                )),
                ..Default::default()
            }),
            fnetemul::Capability::StorageDep(fnetemul::StorageDep {
                variant: Some(fnetemul::StorageVariant::Tmp),
                path: Some("/tmp".to_string()),
                ..Default::default()
            }),
        ])),
        ..Default::default()
    }
}

fn device_name_provider_component() -> fnetemul::ChildDef {
    fnetemul::ChildDef {
        source: Some(fnetemul::ChildSource::Component(NAME_PROVIDER_URL.to_string())),
        name: Some(NAME_PROVIDER_MONIKER.to_string()),
        exposes: Some(vec![fidl_fuchsia_device::NameProviderMarker::DEBUG_NAME.to_string()]),
        uses: Some(fnetemul::ChildUses::Capabilities(vec![fnetemul::Capability::LogSink(
            fnetemul::Empty,
        )])),
        program_args: Some(vec!["--nodename".to_string(), "fuchsia-test-device".to_string()]),
        ..Default::default()
    }
}

async fn wait_for_log(
    stream: diagnostics_reader::Subscription<diagnostics_reader::Data<diagnostics_reader::Logs>>,
    log: &str,
) {
    stream
        .filter_map(|data| {
            futures::future::ready(
                data.expect("stream error")
                    .msg()
                    .map(|msg| msg.contains(log))
                    .unwrap_or(false)
                    .then_some(()),
            )
        })
        .next()
        .await
        .expect("observe expected log");
}

async fn watch_for_crash(realm: &netemul::TestRealm<'_>, component_moniker: &str) {
    let event = wait_for_component_stopped(&realm, component_moniker, None)
        .await
        .expect("observe stopped event");
    let component_events::events::StoppedPayload { status } =
        event.result().expect("extract event payload");
    assert_eq!(status, &component_events::events::ExitStatus::Clean);
}

#[netstack_test]
async fn version<N: Netstack>(name: &str) {
    let sandbox = netemul::TestSandbox::new().expect("create sandbox");

    const IPERF_MONIKER: &str = "iperf";
    let realm = sandbox
        .create_netstack_realm_with::<N, _, _>(
            name,
            [
                device_name_provider_component(),
                iperf_component(IPERF_MONIKER, ["-v"], /* eager */ true),
            ],
        )
        .expect("create realm");

    let iperf_moniker =
        get_component_moniker(&realm, IPERF_MONIKER).await.expect("get iperf moniker");
    let stream = diagnostics_reader::ArchiveReader::new()
        .select_all_for_moniker(&iperf_moniker)
        .snapshot_then_subscribe()
        .expect("subscribe to logs");

    let crash_monitor = watch_for_crash(&realm, IPERF_MONIKER).fuse();
    let mut crash_monitor = pin!(crash_monitor);
    futures::select! {
        () = wait_for_log(stream, "iperf 3.7-FUCHSIA").fuse() => {},
        () = crash_monitor => {},
    }
}

#[derive(PartialEq)]
enum Protocol {
    Tcp,
    Udp,
}

trait TestIpExt {
    const SERVER_FLAG: &'static str;
    const SERVER_ADDR: &'static str;
}

impl TestIpExt for net_types::ip::Ipv4 {
    const SERVER_FLAG: &'static str = "-4";
    const SERVER_ADDR: &'static str = "127.0.0.1";
}

impl TestIpExt for net_types::ip::Ipv6 {
    const SERVER_FLAG: &'static str = "-6";
    const SERVER_ADDR: &'static str = "::1";
}

#[netstack_test]
#[test_case(Protocol::Tcp; "tcp")]
#[test_case(Protocol::Udp; "udp")]
async fn loopback<N: Netstack, I: net_types::ip::Ip + TestIpExt>(name: &str, protocol: Protocol) {
    let sandbox = netemul::TestSandbox::new().expect("create sandbox");

    const SERVER_MONIKER: &str = "iperf-server";
    const CLIENT_MONIKER: &str = "iperf-client";
    let realm = sandbox
        .create_netstack_realm_with::<N, _, _>(
            name,
            [
                device_name_provider_component(),
                iperf_component(SERVER_MONIKER, ["-s", I::SERVER_FLAG], /* eager */ true),
                iperf_component(
                    CLIENT_MONIKER,
                    ["-c", I::SERVER_ADDR, "-t", "1"]
                        .into_iter()
                        .chain((protocol == Protocol::Udp).then_some("-u")),
                    /* eager */ false,
                ),
            ],
        )
        .expect("create realm");

    let mut client_crash_monitor = pin!(watch_for_crash(&realm, CLIENT_MONIKER).fuse());
    let mut server_crash_monitor = pin!(watch_for_crash(&realm, SERVER_MONIKER).fuse());

    // Wait for the server to start up.
    let server_moniker =
        get_component_moniker(&realm, SERVER_MONIKER).await.expect("get server moniker");
    let server_stream = diagnostics_reader::ArchiveReader::new()
        .select_all_for_moniker(&server_moniker)
        .snapshot_then_subscribe()
        .expect("subscribe to server logs");
    futures::select! {
        () = wait_for_log(server_stream, "-------------").fuse() => {},
        () = client_crash_monitor => {},
        () = server_crash_monitor => {},
    }

    // Start the iPerf client and wait for the test run to complete.
    realm.start_child_component(CLIENT_MONIKER).await.expect("start client");
    let client_moniker =
        get_component_moniker(&realm, CLIENT_MONIKER).await.expect("get client moniker");
    let client_stream = diagnostics_reader::ArchiveReader::new()
        .select_all_for_moniker(&client_moniker)
        .snapshot_then_subscribe()
        .expect("subscribe to client logs");
    futures::select! {
        () = wait_for_log(client_stream, "iperf Done.").fuse() => {},
        () = client_crash_monitor => {},
        () = server_crash_monitor => {},
    }
}
