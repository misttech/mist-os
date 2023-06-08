// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use example_config::Config;
use fuchsia_component::server::ServiceFs;
use futures::StreamExt;
use tracing::info;

#[fuchsia::main]
async fn main() {
    let config = Config::take_from_startup_handle();
    info!("Hello, {}! (from Rust)", config.greeting);

    let inspector = fuchsia_inspect::component::inspector();
    inspector.root().record_child("config", |config_node| config.record_inspect(config_node));

    let mut fs = ServiceFs::new_local();
    inspect_runtime::serve(inspector, &mut fs).unwrap();
    fs.take_and_serve_directory_handle().unwrap();
    while let Some(()) = fs.next().await {}
}
