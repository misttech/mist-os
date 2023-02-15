// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {fidl_fidl_test_components as ftest, fuchsia_component::client, tracing::*};

#[fuchsia::main]
async fn main() {
    info!("start");

    let trigger = client::connect_to_protocol::<ftest::TriggerMarker>().unwrap();
    trigger.run().await.unwrap();
}
