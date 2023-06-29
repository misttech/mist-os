// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::Error,
    fuchsia_component::server::ServiceFs,
    fuchsia_inspect::{component, health::Reporter},
    futures::StreamExt,
};

#[fuchsia::main(logging_tags = [ "iquery_basic_component" ])]
async fn main() -> Result<(), Error> {
    let inspector = component::inspector();
    inspector.root().record_string("iquery", "rocks");
    component::health().set_ok();

    let mut fs = ServiceFs::new();
    inspect_runtime::serve(&inspector, &mut fs)?;
    fs.take_and_serve_directory_handle()?;
    Ok(fs.collect().await)
}
