// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! A component that serves a FIDL API `fuchsia.time.alarms/Wake`.
//!
//! Used in hermetic integration tests that exercise timekeeper functionality
//! unrelated to UTC clock maintenance.  This is done to remove the need to
//! set up UTC clock maintenance, which brings in complexity that these tests
//! do not need.
//!
//! This fake service uses the real alarm handling logic from the prod `alarms`
//! crate, so the fake's behavior is faithful to the production code.

use anyhow::{Context, Result};
use fidl_fuchsia_time_alarms as ffta;
use fuchsia_component::server::ServiceFs;
use futures::StreamExt;
use log::warn;
use std::rc::Rc;

// This is the production alarms crate.
use alarms;

enum Services {
    Wake(ffta::WakeRequestStream),
}

#[fuchsia::main(logging_tags = ["test"])]
async fn main() -> Result<()> {
    log::debug!("starting fake wake alarms service");
    let mut fs = ServiceFs::new();
    fs.dir("svc").add_fidl_service(Services::Wake);
    fs.take_and_serve_directory_handle()
        .context("while trying to serve fuchsia.time.alarms/Wake")?;

    let timer_loop = alarms::connect_to_hrtimer_async()
        .map_err(|e| {
            warn!("could not connect to hrtimer: {}", &e);
            e
        })
        .map(|proxy| Rc::new(alarms::Loop::new(proxy)))?;
    fs.for_each_concurrent(/*limit=*/ None, move |connection| {
        let timer_loop = Rc::clone(&timer_loop);
        async move {
            match connection {
                Services::Wake(stream) => alarms::serve(timer_loop, stream).await,
            }
        }
    })
    .await;
    log::debug!("stopping fake wake alarms service");
    Ok(())
}
