// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Result;
use logging::LogFormat;
use rand::Rng;

use tracing_subscriber::filter::{self, LevelFilter};
use tracing_subscriber::prelude::*;
use tracing_subscriber::Layer;

lazy_static::lazy_static! {
    static ref LOGGING_ID: u64 = generate_id();
}

fn generate_id() -> u64 {
    rand::thread_rng().gen::<u64>()
}

pub fn init(level: LevelFilter) -> Result<()> {
    configure_subscribers(level);

    Ok(())
}

fn target_levels() -> Vec<(String, LevelFilter)> {
    vec![]
}

fn configure_subscribers(level: LevelFilter) {
    let filter_targets = filter::Targets::new().with_targets(target_levels()).with_default(level);

    let include_spans = true;
    let stdio_layer = {
        let event_format = LogFormat::new(*LOGGING_ID, include_spans);
        let format =
            tracing_subscriber::fmt::layer().event_format(event_format).with_filter(filter_targets);
        Some(format)
    };

    tracing_subscriber::registry().with(stdio_layer).init();
}
