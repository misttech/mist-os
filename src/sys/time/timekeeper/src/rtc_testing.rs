// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Testing-only code for persistent Timekeeper behavior changes around
//! real-time clock (RTC) handling.

use anyhow::Result;
use futures::StreamExt;
use log::{debug, error};
use persistence::State;
use std::cell::Cell;
use std::rc::Rc;
use {fidl_fuchsia_time_test as fftt, persistence};

/// Serves `fuchsia.time.test/Rtc`.
///
/// Args:
/// - `persistent_enabled_bit`: the state bit to manage.
/// - `stream`: the request stream from the test fixture.
pub async fn serve(
    persist_enabled: Rc<Cell<bool>>,
    mut stream: fftt::RtcRequestStream,
) -> Result<()> {
    debug!("rtc_testing::serve: entering serving loop");
    while let Some(request) = stream.next().await {
        match request {
            Ok(fftt::RtcRequest::PersistentEnable { responder, .. }) => {
                debug!("received: fuchsia.time.test/Rtc.PersistentEnable");
                persist_enabled.set(true);
                responder.send(Ok(()))?;
                persistence::write_state(&State::new(true));
            }
            Ok(fftt::RtcRequest::PersistentDisable { responder, .. }) => {
                debug!("received: fuchsia.time.test/Rtc.PersistentDisable");
                persist_enabled.set(false);
                responder.send(Ok(()))?;
                persistence::write_state(&State::new(false));
            }
            Err(e) => {
                error!("FIDL error: {:?}", e);
            }
        };
    }
    debug!("rtc_testing::serve: exited  serving loop");
    Ok(())
}
