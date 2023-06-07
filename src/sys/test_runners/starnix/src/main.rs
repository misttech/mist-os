// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

mod binder_latency;
mod gbenchmark;
mod gtest;
mod helpers;
mod ltp;
mod results_parser;
mod runner;
mod test_container;
mod test_suite;

use anyhow::Error;
use fidl_fuchsia_component_runner as frunner;
use fuchsia_component::server::ServiceFs;
use futures::StreamExt;
use tracing::debug;

enum Services {
    ComponentRunner(frunner::ComponentRunnerRequestStream),
}

#[fuchsia::main(logging_tags=["starnix_test_runner"])]
async fn main() -> Result<(), Error> {
    debug!("starnix test runner started");
    let mut fs = ServiceFs::new_local();
    fs.dir("svc").add_fidl_service(Services::ComponentRunner);
    fs.take_and_serve_directory_handle()?;
    fs.for_each_concurrent(None, |request| async {
        match request {
            Services::ComponentRunner(stream) => runner::handle_runner_requests(stream)
                .await
                .expect("Error serving runner requests."),
        }
    })
    .await;
    Ok(())
}
