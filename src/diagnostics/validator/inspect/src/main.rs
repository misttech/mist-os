// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

mod data; // Inspect data maintainer/updater/scanner-from-vmo; compare engine
mod metrics; // Evaluates memory performance of Inspect library
mod puppet; // Interface to target Inspect library wrapper programs (puppets)
mod results; // Stores and formats reports-to-user
mod runner; // Coordinates testing operations
mod trials; // Defines the trials to run

use {
    fidl_diagnostics_validate as validate, fuchsia_component::server::ServiceFs, futures::StreamExt,
};

/// meta/validator.shard.cml must use this name in `children: name:`.
const PUPPET_MONIKER: &str = "puppet";

enum IncomingRequest {
    Validator(validate::ValidatorRequestStream),
}

#[fuchsia::main]
async fn main() {
    let mut fs = ServiceFs::new();
    fs.dir("svc").add_fidl_service(IncomingRequest::Validator);
    fs.take_and_serve_directory_handle().expect("serve dir");

    fs.for_each_concurrent(None, |IncomingRequest::Validator(stream)| {
        runner::serve_connection_requests(stream)
    })
    .await;
}

// The only way to test this file is to actually start a component, and that's
// not suitable for unit tests. Failures will be caught in integration
// tests.
