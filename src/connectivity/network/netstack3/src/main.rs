// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! A networking stack.
#![warn(clippy::unused_async)]
#![warn(missing_docs, unreachable_patterns, unused)]
#![recursion_limit = "256"]

mod bindings;

use std::num::NonZeroU8;

use fuchsia_component::server::{ServiceFs, ServiceFsDir};
use log::info;

use bindings::{GlobalConfig, InspectPublisher, NetstackSeed, Service};

/// Runs Netstack3.
pub fn main() {
    let config = ns3_config::Config::take_from_startup_handle();
    let ns3_config::Config { num_threads, debug_logs, suspend_enabled } = &config;
    let num_threads = NonZeroU8::new(*num_threads).expect("invalid 0 thread count value");
    let mut executor = fuchsia_async::SendExecutor::new(num_threads.get().into());

    let mut log_options = diagnostics_log::PublishOptions::default();

    // NB: netstack3 is usually launched with a 'netstack' moniker already -
    // which implies an automatic 'netstack' tag. However, the automatic tag has
    // shown problems when extra tags are present in specific log lines (e.g.
    // https://fxbug.dev/390252317, https://fxbug.dev/390252218). Given that,
    // we always initialize with the netstack tag here.
    log_options = log_options.tags(&["netstack"]);

    if *debug_logs {
        // When forcing debug logs, disable all the dynamic features from the
        // logging framework, we want logs pegged at Severity::Debug.
        log_options = log_options
            .minimum_severity(diagnostics_log::Severity::Debug)
            .wait_for_initial_interest(false)
            .listen_for_interest_updates(false);
    }
    diagnostics_log::initialize(log_options).expect("failed to initialize log");

    fuchsia_trace_provider::trace_provider_create_with_fdio();

    info!("starting netstack3 with {config:?}");

    let mut fs = ServiceFs::new();
    let _: &mut ServiceFsDir<'_, _> = fs
        .dir("svc")
        // TODO(https://fxbug.dev/42076541): This is transitional. Once the
        // out-of-stack DHCP client is being used by both netstacks, it
        // should be moved out of the netstack realm and into the network
        // realm. The trip through Netstack3 allows for availability of DHCP
        // client to be dependent on Netstack version when using
        // netstack-proxy.
        .add_proxy_service::<fidl_fuchsia_net_dhcp::ClientProviderMarker, _>()
        .add_service_connector(Service::DebugDiagnostics)
        .add_fidl_service(Service::DebugInterfaces)
        .add_fidl_service(Service::DnsServerWatcher)
        .add_fidl_service(Service::Stack)
        .add_fidl_service(Service::Socket)
        .add_fidl_service(Service::PacketSocket)
        .add_fidl_service(Service::RawSocket)
        .add_fidl_service(Service::RootInterfaces)
        .add_fidl_service(Service::RootFilter)
        .add_fidl_service(Service::RootRoutesV4)
        .add_fidl_service(Service::RootRoutesV6)
        .add_fidl_service(Service::RoutesState)
        .add_fidl_service(Service::RoutesStateV4)
        .add_fidl_service(Service::RoutesStateV6)
        .add_fidl_service(Service::RoutesAdminV4)
        .add_fidl_service(Service::RoutesAdminV6)
        .add_fidl_service(Service::RouteTableProviderV4)
        .add_fidl_service(Service::RouteTableProviderV6)
        .add_fidl_service(Service::RuleTableV4)
        .add_fidl_service(Service::RuleTableV6)
        .add_fidl_service(Service::Interfaces)
        .add_fidl_service(Service::InterfacesAdmin)
        .add_fidl_service(Service::MulticastAdminV4)
        .add_fidl_service(Service::MulticastAdminV6)
        .add_fidl_service(Service::FilterState)
        .add_fidl_service(Service::FilterControl)
        .add_fidl_service(Service::Neighbor)
        .add_fidl_service(Service::NeighborController)
        .add_fidl_service(Service::Verifier)
        .add_fidl_service(Service::HealthCheck);

    let seed = NetstackSeed::new(GlobalConfig { suspend_enabled: *suspend_enabled });

    let inspect_publisher = InspectPublisher::new();
    inspect_publisher
        .inspector()
        .root()
        .record_child("Config", |config_node| config.record_inspect(config_node));

    let _: &mut ServiceFs<_> = fs.take_and_serve_directory_handle().expect("directory handle");

    executor.run(seed.serve(fs, inspect_publisher))
}
