// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl_fidl_examples_routing_echo::EchoMarker;
use fuchsia_component::client;

#[fuchsia::main]
async fn main() {
    tracing::info!("Started");
    let echo = client::connect_to_protocol::<EchoMarker>().unwrap();
    let res = echo.echo_string(Some(&format!("hello"))).await;
    tracing::info!("Received: '{}'", res.unwrap().unwrap());
}
