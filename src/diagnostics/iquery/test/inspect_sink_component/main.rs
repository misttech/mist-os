// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fuchsia_inspect::{component, health::Reporter};

#[fuchsia::main(logging_tags = [ "iquery_inspect_sink_component" ])]
async fn main() {
    component::inspector().root().record_string("iquery", "rules");
    component::health().set_ok();

    if let Some(server) =
        inspect_runtime::publish(component::inspector(), inspect_runtime::PublishOptions::default())
    {
        server.await
    }
}
