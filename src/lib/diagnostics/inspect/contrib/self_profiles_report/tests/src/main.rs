// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use diagnostics_reader::ArchiveReader;
use fidl_inspect_selfprofile_test::PuppetMarker;
use fuchsia_component::client::connect_to_protocol;
use self_profiles_report::{DurationSummary, SelfProfilesReport};
use std::path::Path;
use std::sync::LazyLock;

/// All locations should be reported from the puppet's source file which is next to this one.
static EXPECTED_LOCATION_PREFIX: LazyLock<String> =
    LazyLock::new(|| Path::new(file!()).parent().unwrap().join("puppet.rs").display().to_string());

#[fuchsia::main]
async fn main() {
    let puppet = connect_to_protocol::<PuppetMarker>().unwrap();

    // Make sure nothing is recorded without profiling being started.
    for _ in 0..100 {
        puppet.run_profiled_function().await.unwrap();
    }
    let empty_snapshot = ArchiveReader::inspect().snapshot().await.unwrap();
    let empty_summaries = SelfProfilesReport::from_snapshot(&empty_snapshot).unwrap();
    assert_eq!(empty_summaries, &[], "summaries should be empty before profiling started");

    // Turn profiling on, observe the creation of the profile.
    puppet.start_profiling().await.unwrap();
    for _ in 0..100 {
        puppet.run_profiled_function().await.unwrap();
    }
    let first_snapshot = ArchiveReader::inspect().snapshot().await.unwrap();
    let first_summaries = SelfProfilesReport::from_snapshot(&first_snapshot).unwrap();
    assert_eq!(first_summaries.len(), 1, "must only be one summary, found {first_summaries:?}");
    let first_summary = &first_summaries[0];
    check_summary_expected(first_summary, 100);

    // Make sure the profile stays unchanged when profiling is stopped.
    puppet.stop_profiling().await.unwrap();
    for _ in 0..100 {
        puppet.run_profiled_function().await.unwrap();
    }
    let second_snapshot = ArchiveReader::inspect().snapshot().await.unwrap();
    let second_summaries = SelfProfilesReport::from_snapshot(&second_snapshot).unwrap();
    assert_eq!(
        second_summaries, first_summaries,
        "profiled code with profiling stopped should update profiles"
    );
    check_summary_expected(&second_summaries[0], 100);

    // Run the workload again with profiling on so we can compare against the first run as baseline.
    puppet.start_profiling().await.unwrap();
    for _ in 0..75 {
        puppet.run_profiled_function().await.unwrap();
    }
    let third_snapshot = ArchiveReader::inspect().snapshot().await.unwrap();
    let third_summaries = SelfProfilesReport::from_snapshot(&third_snapshot).unwrap();
    assert_eq!(third_summaries.len(), 1, "must only be one summary, found {third_summaries:?}");
    let third_summary = &third_summaries[0];

    // This report should have observed both the first and third runs.
    check_summary_expected(third_summary, 175);

    // Using the first as a baseline should show us only the runs from the last loop.
    check_summary_expected(&third_summary.delta_from(first_summary).unwrap(), 75);
}

#[track_caller]
fn check_summary_expected(summary: &SelfProfilesReport, num_runs: u64) {
    assert_eq!(summary.name(), "puppet");

    // The puppet creates its own root duration below the top-level catchall node.
    let (root_name, actual_root) = summary.root_summary().children().next().unwrap();
    assert_eq!(root_name, "RootDuration");
    assert_eq!(actual_root.count(), num_runs);
    assert!(
        actual_root.location().starts_with(&*EXPECTED_LOCATION_PREFIX),
        "location {} must start with {}",
        actual_root.location(),
        &*EXPECTED_LOCATION_PREFIX,
    );
    assert!(actual_root.cpu_time() > 0);
    assert!(actual_root.wall_time() > 0);

    // There are four nested/middle durations in the puppet.
    let mut actual_children = actual_root.children();
    let (first_name, first_duration) = actual_children.next().unwrap();
    assert_eq!(first_name, "FirstNestedDuration");
    check_duration(first_duration, num_runs * 10, num_runs * 10);

    let (second_name, second_duration) = actual_children.next().unwrap();
    assert_eq!(second_name, "SecondNestedDuration");
    check_duration(second_duration, num_runs, num_runs * 3);

    let (third_name, third_duration) = actual_children.next().unwrap();
    assert_eq!(third_name, "ThirdNestedDuration");
    check_duration(third_duration, num_runs, num_runs * 2);

    let (fourth_name, fourth_duration) = actual_children.next().unwrap();
    assert_eq!(fourth_name, "FourthNestedDuration");
    check_duration(fourth_duration, num_runs, num_runs);

    // There should be a single leaf duration shared across all parents.
    let mut leaves = summary.leaf_durations().into_iter();
    let (leaf_name, leaf_duration) = leaves.next().unwrap();
    assert_eq!(leaf_name, "LeafDuration");
    check_leaf_duration(&leaf_duration, num_runs * 16);
    assert_approx_eq(actual_root.cpu_time(), leaf_duration.cpu_time());
    assert_eq!(leaves.next(), None);

    // Make sure the relative proportions are about right. The fourth nested duration contains
    // a single call to burn_a_little_cpu so it's our base unit.
    assert_approx_eq(first_duration.cpu_time(), fourth_duration.cpu_time() * 10);
    assert_approx_eq(second_duration.cpu_time(), fourth_duration.cpu_time() * 3);
    assert_approx_eq(third_duration.cpu_time(), fourth_duration.cpu_time() * 2);
    assert_approx_eq(leaf_duration.cpu_time(), fourth_duration.cpu_time() * 16);
}

#[track_caller]
fn check_duration(duration: &DurationSummary, count: u64, leaf_count: u64) {
    assert_eq!(duration.count(), count, "duration must have expected count");
    assert!(duration.cpu_time() > 0, "duration must have recorded some time on-cpu");
    assert!(duration.wall_time() > 0, "duration must have recorded some time on wall clock");
    assert!(
        duration.location().starts_with(&*EXPECTED_LOCATION_PREFIX),
        "duration's location {} must start with {}",
        duration.location(),
        &*EXPECTED_LOCATION_PREFIX,
    );

    let (leaf_name, leaf_duration) =
        duration.children().next().expect("duration should have a leaf child");
    assert_eq!(leaf_name, "LeafDuration");
    check_leaf_duration(leaf_duration, leaf_count);

    assert_approx_eq(duration.cpu_time(), leaf_duration.cpu_time());
}

#[track_caller]
fn check_leaf_duration(leaf_duration: &DurationSummary, leaf_count: u64) {
    assert_eq!(leaf_duration.count(), leaf_count, "leaf duration must have expected count");
    assert!(leaf_duration.cpu_time() > 0, "leaf duration must have recorded some time on-cpu");
    assert!(
        leaf_duration.wall_time() > 0,
        "leaf duration must have recorded some time on wall clock"
    );
}

const MARGIN_PERCENT: f64 = 6.2;

#[track_caller]
fn assert_approx_eq(lhs: i64, rhs: i64) {
    assert!(
        ((lhs - rhs).abs() as f64 / lhs as f64) <= (MARGIN_PERCENT / 100.0),
        "{lhs} and {rhs} must be within {MARGIN_PERCENT}% of each other",
    );
}
