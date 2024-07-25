// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Implementation of the network socket proxy.
//!
//! Runs proxied versions of fuchsia.posix.socket.Provider and fuchsia.posix.socket.raw.Provider.
//! Exposes fuchsia.netpol.socketproxy.StarnixNetworks and
//! fuchsia.netpol.socketproxy.DnsServerWatcher.

use fuchsia_inspect::health::Reporter;

/// Main entry point for the network socket proxy.
#[fuchsia::main(logging_tags = ["network_socket_proxy"])]
pub async fn main() -> Result<(), anyhow::Error> {
    fuchsia_inspect::component::health().set_starting_up();

    let inspector = fuchsia_inspect::component::inspector();
    let _inspect_server_task =
        inspect_runtime::publish(inspector, inspect_runtime::PublishOptions::default());

    let _root_node = inspector.root();

    fuchsia_inspect::component::health().set_ok();

    Ok(())
}
