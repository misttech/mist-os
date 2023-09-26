// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fuchsia_async::Timer;
use fuchsia_inspect::{component, Inspector};
use fuchsia_zircon::Duration;
use futures::FutureExt;

#[fuchsia::main]
async fn main() {
    let insp = component::inspector();
    insp.root().record_string("child", "value");

    insp.root().record_lazy_values("lazy-node-always-hangs", || {
        async move {
            Timer::new(Duration::from_minutes(60)).await;
            Ok(Inspector::default())
        }
        .boxed()
    });

    insp.root().record_int("int", 3);

    if let Some(inspect_server) =
        inspect_runtime::publish(insp, inspect_runtime::PublishOptions::default())
    {
        inspect_server.await
    }
}
