// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Error;
use diagnostics_log::{OnInterestChanged, PublishOptions, Publisher};
use fidl_fuchsia_diagnostics::Severity;
use fuchsia_component::server::ServiceFs;
use futures::StreamExt;
use tracing::{debug, error, info, warn};

#[fuchsia::main(logging = false)]
async fn main() -> Result<(), Error> {
    diagnostics_log::initialize(PublishOptions::default().wait_for_initial_interest(false))
        .expect("initialized tracing");
    let mut fs = ServiceFs::new();
    tracing::dispatcher::get_default(|dispatcher| {
        let publisher: &Publisher = dispatcher.downcast_ref().unwrap();
        publisher.set_interest_listener(Listener::new());
    });
    fs.take_and_serve_directory_handle()?;
    fs.collect::<()>().await;
    Ok(())
}

struct Listener;

impl Listener {
    fn new() -> Self {
        Self {}
    }
}

impl OnInterestChanged for Listener {
    fn on_changed(&self, severity: &Severity) {
        if *severity <= Severity::Debug {
            debug!("debug msg");
        }
        if *severity <= Severity::Info {
            info!("info msg");
        }
        if *severity <= Severity::Warn {
            warn!("warn msg");
        }
        if *severity <= Severity::Error {
            error!("error msg");
        }
    }
}
