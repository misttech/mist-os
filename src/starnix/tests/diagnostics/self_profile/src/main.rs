// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use component_events::events::{EventStream, ExitStatus, Stopped};
use component_events::matcher::EventMatcher;
use diagnostics_reader::ArchiveReader;
use fidl_fuchsia_component::BinderMarker;
use fuchsia_component::client::connect_to_protocol;
use self_profiles_report::SelfProfilesReport;

const ITERATION_COUNT: u64 = 10;

#[fuchsia::main]
async fn main() {
    let mut event_stream = EventStream::open().await.unwrap();

    // Run the program enough times to ensure that all of the measured durations are significantly
    // larger than our measurement error/overhead.
    for _ in 0..ITERATION_COUNT {
        run_pipe_writer(&mut event_stream).await;
    }

    let first_snapshot = ArchiveReader::inspect().snapshot().await.unwrap();
    let first_summaries = SelfProfilesReport::from_snapshot(&first_snapshot).unwrap();
    assert_ne!(first_summaries, &[], "summaries should not be empty");
    let first_summary = &first_summaries[0];
    println!("first run: {first_summary}"); // print this so that infra shows it for future debugging
    check_summary_expected(&first_summary);

    // Run the workload again, which should approximately double our metrics.
    for _ in 0..ITERATION_COUNT {
        run_pipe_writer(&mut event_stream).await;
    }

    let second_snapshot = ArchiveReader::inspect().snapshot().await.unwrap();
    let second_summaries = SelfProfilesReport::from_snapshot(&second_snapshot).unwrap();
    assert_ne!(second_summaries, &[], "summaries should not be empty");

    // Diff this report against the first one to see what changed.
    let second_summary = second_summaries[0].delta_from(&first_summary).unwrap();
    println!("second run: {second_summary}"); // print this so that infra shows it for future debugging

    // The second report should be approximately the same as the first and report the same number
    // of durations.
    check_summary_expected(&second_summary);
}

#[track_caller]
fn check_summary_expected(summary: &SelfProfilesReport) {
    let mut restricted = None;
    let mut normal = None;
    for (name, child) in summary.root_summary().children() {
        match name {
            "NormalMode" => normal = Some(child),
            "RestrictedMode" => restricted = Some(child),
            _ => (),
        }
    }
    let restricted = restricted.expect("kernel must have a restricted mode duration");
    assert_eq!(restricted.children().next(), None, "restricted duration should not have children");
    let normal = normal.expect("kernel must have a normal mode duration");
    assert!(normal.cpu_time() > 0);
    let execute_syscall = normal
        .children()
        .find(|(n, _)| *n == "ExecuteSyscall")
        .map(|(_, c)| c)
        .expect("any syscall should have been invoked");

    let pipe_syscall = execute_syscall
        .children()
        .find(|(n, _)| *n == "pipe2")
        .map(|(_, c)| c)
        .expect("pipe2 syscall should have been invoked");
    assert_eq!(pipe_syscall.count(), ITERATION_COUNT, "pipe2 should be called once per iteration");
    let write_syscall = execute_syscall
        .children()
        .find(|(n, _)| *n == "write")
        .map(|(_, c)| c)
        .expect("write syscall should have been invoked");

    // We invoke write() 1000x per iteration, and libc seems to invoke it once while bootstrapping.
    let expected_write_count = (ITERATION_COUNT * 1000) + ITERATION_COUNT;
    assert_eq!(write_syscall.count(), expected_write_count);

    let restricted_leaf = summary
        .leaf_durations()
        .into_iter()
        .find(|(n, _)| *n == "RestrictedMode")
        .map(|(_, d)| d)
        .expect("leaf durations must contain RestrictedMode");
    assert!(
        restricted_leaf.cpu_time() > 0,
        "restricted mode must have registered greather-than-zero cpu time"
    );
}

async fn run_pipe_writer(event_stream: &mut EventStream) {
    connect_to_protocol::<BinderMarker>().unwrap();
    assert_eq!(
        EventMatcher::ok()
            .moniker("pipe_writer")
            .wait::<Stopped>(event_stream)
            .await
            .unwrap()
            .result()
            .unwrap()
            .status,
        ExitStatus::Clean,
    );
}
