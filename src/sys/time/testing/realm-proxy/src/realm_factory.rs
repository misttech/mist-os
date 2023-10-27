// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Result;
use fidl_test_time_realm::{RealmOptions, RtcOptions};
use fuchsia_component_test::RealmInstance;
use fuchsia_zircon as zx;
use std::sync::Arc;
use timekeeper_integration_lib::NestedTimekeeper;

pub async fn create_realm(
    options: RealmOptions,
    fake_utc_clock: zx::Clock,
) -> Result<RealmInstance> {
    tracing::debug!("create_realm: options: {:?}, fake_utc_clock={:?}", options, fake_utc_clock);
    let rtc: Option<zx::Time> = options
        .rtc
        .map(|t| match t {
            RtcOptions::InitialRtcTime(t) => Some(zx::Time::from_nanos(t)),
            _ => {
                // TODO: b/306213772 - Needs a way to serve all the *other* updates here.
                tracing::debug!("create_realm: no support yet for all realm options");
                unimplemented!()
            }
        })
        .flatten();

    // TODO: b/306213772 - Figure out how to use all these return values.
    let (
        nested_timekeeper,
        _push_source_puppet,
        _rtc_updates,
        _metric_proxy,
        _fake_clock_controller,
    ) = NestedTimekeeper::new(
        Arc::new(fake_utc_clock),
        rtc,
        !options.use_real_monotonic_clock.unwrap_or(false),
    )
    .await;
    Ok(nested_timekeeper.into())
}
