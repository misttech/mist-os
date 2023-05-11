// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    argh::FromArgs,
    fuchsia_async as fasync,
    system_validation_lib::{
        app_monitor::AppMonitor, screencapture::take_screenshot,
        single_session_trace::SingleSessionTrace,
    },
    tracing::info,
};

/// Args to control how to run system validation test.
/// Specify them through *_system_validation.cml
#[derive(FromArgs, PartialEq, Debug)]
struct RunTestCmd {
    /// trace configurations, input is a comma delimited string
    /// example: --trace-config "system_metrics,input,gfx"
    #[argh(option, default = "vec![]", from_str_fn(to_vector))]
    trace_config: Vec<String>,

    /// number of seconds to run example app, default is 5 seconds
    #[argh(option, default = "fasync::Duration::from_seconds(5)", from_str_fn(to_duration))]
    run_duration_sec: fasync::Duration,
}

// Note: Maybe we want to make this an argument.
const SAMPLE_APP_MONIKER: &str = "./sample-app";

#[fuchsia::main]
async fn main() {
    let args: RunTestCmd = argh::from_env();

    let app_monitor = AppMonitor::new(SAMPLE_APP_MONIKER.to_string());

    app_monitor.wait_for_start_event().await;
    app_monitor.add_monitor_for_stop_event();

    // Collect trace.
    let trace = SingleSessionTrace::new();
    let collect_trace = !args.trace_config.is_empty();
    if collect_trace {
        info!("Collecting trace: {:?}", args.trace_config);
        trace.initialize(args.trace_config).await.unwrap();
        trace.start().await.unwrap();
    }
    // Let the UI App run for [run_duration_sec], then the test_runner will shutdown the sample_app
    let duration_sec = args.run_duration_sec;
    info!("Running sample app for {:?} sec", duration_sec);
    fasync::Timer::new(duration_sec).await;

    if collect_trace {
        trace.stop().await.unwrap();
        trace.terminate().await.unwrap();
    }

    info!("Taking screenshot.");
    take_screenshot().await;

    info!("Checking that sample-app did not crash.");
    if app_monitor.has_seen_stop_event() {
        panic!("Sample app unexpectedly stopped");
    }
}

fn to_duration(duration_sec: &str) -> Result<fasync::Duration, String> {
    Ok(fasync::Duration::from_seconds(duration_sec.parse::<i64>().unwrap()))
}

fn to_vector(configs: &str) -> Result<Vec<String>, String> {
    Ok(configs.split(',').map(|v| v.trim().to_string()).collect())
}
