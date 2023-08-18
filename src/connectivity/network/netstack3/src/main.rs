// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! A networking stack.
#![deny(clippy::unused_async)]
#![deny(missing_docs, unreachable_patterns, unused)]
#![recursion_limit = "256"]

#[cfg(feature = "instrumented")]
extern crate netstack3_core_instrumented as netstack3_core;

use fuchsia_component::server::{ServiceFs, ServiceFsDir};

mod bindings;

use bindings::{NetstackSeed, Service};

/// Runs Netstack3.
#[fuchsia::main(logging_minimum_severity = "debug")]
pub fn main() {
    // TOOD(https://fxbug.dev/125388): Support running with multiple threads.
    // This is currently blocked on fixing race conditions when concurrent
    // operations are allowed.
    let mut executor = fuchsia_async::SendExecutor::new(1 /* num_threads */);

    fuchsia_trace_provider::trace_provider_create_with_fdio();

    let mut fs = ServiceFs::new();
    let _: &mut ServiceFsDir<'_, _> = fs
        .dir("svc")
        .add_service_connector(Service::DebugDiagnostics)
        .add_fidl_service(Service::DebugInterfaces)
        .add_fidl_service(Service::Stack)
        .add_fidl_service(Service::Socket)
        .add_fidl_service(Service::PacketSocket)
        .add_fidl_service(Service::RawSocket)
        .add_fidl_service(Service::RootInterfaces)
        .add_fidl_service(Service::RoutesState)
        .add_fidl_service(Service::RoutesStateV4)
        .add_fidl_service(Service::RoutesStateV6)
        .add_fidl_service(Service::Interfaces)
        .add_fidl_service(Service::InterfacesAdmin)
        .add_fidl_service(Service::Filter)
        .add_fidl_service(Service::Neighbor)
        .add_fidl_service(Service::Verifier);

    let seed = NetstackSeed::default();

    let inspector = fuchsia_inspect::component::inspector();
    inspect_runtime::serve(inspector, &mut fs).expect("failed to serve inspect");
    let _: &mut ServiceFs<_> = fs.take_and_serve_directory_handle().expect("directory handle");

    executor.run(seed.serve(fs, inspector))
}
