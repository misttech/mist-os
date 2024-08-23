// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Integration test that can help to check the connection to the server
//! and the fidl call.

mod daemon_work;
mod sag_work;

use anyhow::Result;
use argh::FromArgs;
use std::time::Instant;

#[derive(FromArgs, Debug)]
/// Command line argument for the tests
struct Options {
    /// number of times to repeat the test
    #[argh(option, default = "1000")]
    repeat: u32,

    /// switch used by rust test runner.
    #[argh(switch)]
    #[allow(unused)]
    nocapture: bool,
}

// The format for changing the argument on command line is `-- --repeat N`, e.g.
// fx test -o power-framework-bench-integration-tests --test-filter=*takewakelease -- --repeat 5000
fn determine_loop_count() -> u32 {
    let args: Options = argh::from_env::<Options>();
    args.repeat
}

#[fuchsia::test]
fn test_sag_takewakelease() -> Result<()> {
    let total: u32 = determine_loop_count();

    let sag_arc = sag_work::obtain_sag_proxy();
    let start = Instant::now();
    for _ in 0..total {
        sag_work::execute(&sag_arc);
    }
    let duration = start.elapsed();
    println!("Total execution time: {:?}", duration);
    println!("Average time for each call is {:?}", duration / total);

    Ok(())
}

#[fuchsia::test]
fn test_topologytestdaemon_toggle() -> Result<()> {
    let total: u32 = determine_loop_count();

    let (topology_control, status_channel) = daemon_work::prepare_work();
    let start = Instant::now();
    for _ in 0..total {
        daemon_work::execute(&topology_control, &status_channel);
    }
    let duration = start.elapsed();
    println!("Total execution time: {:?}", duration);
    println!("Average time for each call is {:?}", duration / total);
    Ok(())
}
