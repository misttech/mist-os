// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Integration test that can help to check the connection to the server
//! and the fidl call.

mod daemon_work;
mod sag_work;

use anyhow::{format_err, Error, Result};
use argh::FromArgs;
use diagnostics_hierarchy::DiagnosticsHierarchy;
use diagnostics_reader::ArchiveReader;
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
async fn test_sag_takewakelease() {
    let total: u32 = determine_loop_count();

    let sag_arc = sag_work::obtain_sag_proxy();
    let start = Instant::now();
    for iteration in 0..total {
        sag_work::execute(&sag_arc);
        if iteration > 0 && iteration % 1000 == 0 {
            print_power_broker_inspect_stats(iteration).await;
        }
    }
    let duration = start.elapsed();
    println!("Total execution time: {:?}", duration);
    println!("Average time for each call is {:?}", duration / total);

    // Check how much PB Inspect VMO we used.
    print_power_broker_inspect_stats(total).await;
    ()
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

async fn get_power_broker_inspect() -> Result<DiagnosticsHierarchy, Error> {
    ArchiveReader::inspect()
        .select_all_for_moniker("test-power-broker")
        .snapshot()
        .await?
        .into_iter()
        .next()
        .and_then(|result| result.payload)
        .ok_or(format_err!("expected one inspect hierarchy"))
}

fn get_inspect_vmo_bytes(inspect: &DiagnosticsHierarchy) -> (u64, u64) {
    let curr = inspect
        .get_property_by_path(&vec!["fuchsia.inspect.Stats", "current_size"])
        .unwrap()
        .uint()
        .unwrap();
    let max = inspect
        .get_property_by_path(&vec!["fuchsia.inspect.Stats", "maximum_size"])
        .unwrap()
        .uint()
        .unwrap();
    return (curr, max);
}

async fn print_power_broker_inspect_stats(iteration: u32) {
    let pb_inspect = get_power_broker_inspect().await.expect("Inspect data");
    let (used, max) = get_inspect_vmo_bytes(&pb_inspect);
    println!(
        "{} - Power Broker inspect used {} / {} bytes, {:.0} % utilization",
        iteration,
        used,
        max,
        (used as f64 / max as f64) * 100.0
    );
    ()
}
