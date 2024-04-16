// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fuchsia_inspect::{component, health::Reporter};
use tracing::info;

#[fuchsia::main]
async fn main() {
    component::health().set_ok();
    info!("This is a syslog message");
    info!("This is another syslog message");
    if let Some(inspect_server) =
        inspect_runtime::publish(component::inspector(), inspect_runtime::PublishOptions::default())
    {
        inspect_server.await
    }
}
