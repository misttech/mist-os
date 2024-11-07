// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl::endpoints::ProtocolMarker as _;
use fidl_fuchsia_netemul as fnetemul;
use futures::FutureExt as _;
use net_types::ip::Ipv4;
use netstack_testing_common::realms::{
    constants, KnownServiceProvider, Netstack, ProdNetstack2, ProdNetstack3, TestSandboxExt as _,
};
use netstack_testing_common::{get_component_moniker, wait_for_component_stopped};
use std::io::Write as _;

const IPERF_URL: &str = "#meta/iperf.cm";
const NAME_PROVIDER_URL: &str = "#meta/device-name-provider.cm";
const NAME_PROVIDER_MONIKER: &str = "device-name-provider";
const PRIMARY_INTERFACE_CONFIGURATION: &str = "fuchsia.network.PrimaryInterface";

fn iperf_component<'a>(
    name: &str,
    program_args: impl IntoIterator<Item = &'a str>,
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
        uses: Some(fnetemul::ChildUses::Capabilities(vec![
            fnetemul::Capability::LogSink(fnetemul::Empty),
            fidl_fuchsia_netemul::Capability::ChildDep(fidl_fuchsia_netemul::ChildDep {
                capability: Some(fidl_fuchsia_netemul::ExposedCapability::Configuration(
                    PRIMARY_INTERFACE_CONFIGURATION.to_string(),
                )),
                ..Default::default()
            }),
        ])),
        program_args: Some(vec!["--nodename".to_string(), "fuchsia-test-device".to_string()]),
        ..Default::default()
    }
}

async fn watch_for_exit(
    realm: &netemul::TestRealm<'_>,
    component_moniker: &str,
) -> component_events::events::ExitStatus {
    let event = wait_for_component_stopped(&realm, component_moniker, None)
        .await
        .expect("observe stopped event");
    let component_events::events::StoppedPayload { status, .. } =
        event.result().expect("extract event payload");
    *status
}

#[derive(PartialEq)]
enum Protocol {
    Tcp,
    Udp,
}

impl std::str::FromStr for Protocol {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "tcp" => Ok(Self::Tcp),
            "udp" => Ok(Self::Udp),
            s => Err(anyhow::anyhow!("unknown protocol {s}")),
        }
    }
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

#[derive(argh::FromArgs)]
/// Iperf3 loopback benchmarks.
struct Args {
    /// benchmark against NS3
    #[argh(switch)]
    netstack3: bool,

    /// transport layer protocol (UDP or TCP)
    #[argh(option)]
    protocol: Protocol,

    /// message size
    #[argh(option)]
    message_size: u16,

    /// number of flows
    #[argh(option)]
    flows: u8,
}

#[fuchsia::main]
async fn main() {
    let Args { netstack3, protocol, message_size, flows } = argh::from_env();
    // TODO(https://fxbug.dev/359670074): Consider adding IPv6 versions of
    // these benchmarks.
    if netstack3 {
        bench::<ProdNetstack3, Ipv4>("bench", protocol, message_size, flows, true /* bench */)
            .await;
    } else {
        bench::<ProdNetstack2, Ipv4>("bench", protocol, message_size, flows, true /* bench */)
            .await;
    }
}

async fn bench<N: Netstack, I: TestIpExt>(
    name: &str,
    protocol: Protocol,
    message_size: u16,
    flows: u8,
    bench: bool,
) {
    let sandbox = netemul::TestSandbox::new().expect("create sandbox");

    let client_moniker = |i| format!("iperf-client-{i}");
    let server_moniker = |i| format!("iperf-server-{i}");
    let realm = sandbox
        .create_netstack_realm_with::<N, _, _>(
            name,
            std::iter::once(device_name_provider_component())
                .chain(bench.then_some(KnownServiceProvider::SecureStash.into()))
                .chain(
                    (0..flows)
                        .map(|i| {
                            // NB: The port numbers are arbitrarily chosen.
                            let port = 9001 + u16::from(i);
                            [
                                iperf_component(
                                    &server_moniker(i),
                                    ["-s", I::SERVER_FLAG, "--port", &port.to_string(), "--json"],
                                    /* eager */ true,
                                ),
                                iperf_component(
                                    &client_moniker(i),
                                    [
                                        "-c",
                                        I::SERVER_ADDR,
                                        "--port",
                                        &port.to_string(),
                                        "--length",
                                        &message_size.to_string(),
                                        "--json",
                                        "--bitrate",
                                        "0",
                                        "--get-server-output",
                                        if bench { "-t5" } else { "-n1" },
                                    ]
                                    .into_iter()
                                    .chain((protocol == Protocol::Udp).then_some("-u")),
                                    /* eager */ false,
                                ),
                            ]
                            .into_iter()
                        })
                        .flatten(),
                ),
        )
        .expect("create realm");

    // Start the iPerf client until a successful run is observed.
    let realm_ref = &realm;
    let mut servers = futures::future::select_all(
        (0..flows)
            .map(|i| Box::pin(async move { watch_for_exit(realm_ref, &server_moniker(i)).await })),
    )
    .fuse();
    let mut clients = futures::future::join_all((0..flows).map(|i| async move {
        loop {
            realm_ref.start_child_component(&client_moniker(i)).await.expect("start client");
            if watch_for_exit(realm_ref, &client_moniker(i)).await
                == component_events::events::ExitStatus::Clean
            {
                return;
            }
        }
    }))
    .fuse();

    futures::select! {
        _ = clients => {},
        (status, index, _remaining_futs) = servers => {
            panic!(
                "servers should not stop but server with index {} exited with status {:?}",
                index, status,
            );
        },
    }

    if bench {
        async fn get_output(
            realm: &netemul::TestRealm<'_>,
            client: bool,
            moniker: &str,
            path: String,
        ) {
            let moniker = get_component_moniker(&realm, moniker).await.expect("get moniker");
            let output = diagnostics_reader::ArchiveReader::new()
                .select_all_for_moniker(&moniker)
                .snapshot()
                .await
                .expect("take snapshot");

            // We want only the last JSON object (starts with a line with
            // just an opening brace) in the client output as there may be
            // output from failed attempts to start the client.
            let start_index = if client {
                output
                    .iter()
                    .rposition(|line| if let Some(line) = line.msg() { line == "{" } else { false })
                    .expect("client JSON output should have a line with only {")
            } else {
                0
            };

            let mut f = std::io::BufWriter::new(
                std::fs::File::create(path).expect("create JSON output file"),
            );
            output.iter().skip(start_index).for_each(|line| {
                if let Some(line) = line.msg() {
                    f.write_all(line.as_bytes()).expect("write output");
                    f.write_all("\n".as_bytes()).expect("write newline");
                }
            });
            f.flush().expect("flush writes");
        }
        for i in 0..flows {
            get_output(
                &realm,
                true, /* client */
                &client_moniker(i),
                format!("/custom_artifacts/iperf_client_{i}.json"),
            )
            .await;
            get_output(
                &realm,
                false, /* client */
                &server_moniker(i),
                format!("/custom_artifacts/iperf_server_{i}.json"),
            )
            .await;
        }
    }
}

#[cfg(test)]
mod test {
    use futures::StreamExt as _;
    use netstack_testing_common::realms::{Netstack, TestSandboxExt as _};
    use netstack_testing_macros::netstack_test;
    use test_case::test_case;

    use super::*;

    async fn wait_for_log(
        stream: diagnostics_reader::Subscription<
            diagnostics_reader::Data<diagnostics_reader::Logs>,
        >,
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

    #[netstack_test]
    #[variant(N, Netstack)]
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

        let ((), status) = futures::future::join(
            wait_for_log(stream, "iperf 3.7-FUCHSIA"),
            watch_for_exit(&realm, IPERF_MONIKER),
        )
        .await;
        assert_eq!(status, component_events::events::ExitStatus::Clean,);
    }

    #[netstack_test]
    #[variant(N, Netstack)]
    #[variant(I, Ip)]
    #[test_case(Protocol::Tcp; "tcp")]
    #[test_case(Protocol::Udp; "udp")]
    async fn loopback<N: Netstack, I: TestIpExt>(name: &str, protocol: Protocol) {
        bench::<N, I>(
            name, protocol, 1400,  /* message_size */
            1,     /* flows */
            false, /* bench */
        )
        .await;
    }
}
