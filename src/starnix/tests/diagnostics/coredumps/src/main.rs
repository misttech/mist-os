// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use diagnostics_reader::{ArchiveReader, Inspect};
use fuchsia_component_test::ScopedInstance;
use parse_starnix_inspect::CoredumpReport;

#[fuchsia::main]
async fn main() {
    // Iterate for a bit more than the max number of core dumps to check the rollout behavior.
    let report_capacity = 64usize;
    let max_idx = report_capacity + 8;

    for current_idx in 0..max_idx {
        let mut instance =
            ScopedInstance::new("coredumps".into(), "#meta/coredump.cm".into()).await.unwrap();

        eprintln!("starting coredump instance...");
        instance.start_with_binder_sync().await.unwrap();

        // Wait until the most recent coredump expected has been reported before we make assertions
        // about the observed diagnostics.
        eprintln!("retrieving coredump reports...");
        let observed_coredumps = loop {
            if let Some(observed_coredumps) = get_coredumps_from_inspect().await {
                if let Some(most_recent) = observed_coredumps.last() {
                    if most_recent.idx == current_idx {
                        break observed_coredumps;
                    }
                }
                eprintln!("waiting for coredump ({current_idx}/{max_idx}) to show up...");
            }
            std::thread::sleep(std::time::Duration::from_secs(1));
        };
        eprintln!("observed coredump {current_idx} in inspect, validating...");

        // The "earliest"/lowest index should be either 0 have advanced by how many were rolled out.
        let expected_min_idx = current_idx.saturating_sub(report_capacity - 1);
        // There should be as many reports as loop iterations unless we're at capacity.
        let expected_len = (current_idx + 1).min(report_capacity);
        assert_eq!(observed_coredumps.len(), expected_len);

        // Ensure that (once sorted by the inspect function below) we have reasonable pids and
        // indexes.
        let mut expected_idx = expected_min_idx;
        for coredump in observed_coredumps {
            assert_eq!(coredump.idx, expected_idx);
            assert!(
                coredump.pid >= expected_idx as i64 + 3,
                "coredumps starts at pid greater or equals to 3, nothing else runs here",
            );
            assert_eq!(
                coredump.argv,
                "data/tests/generate_linux_coredump AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA AAAAAAAAAAAAAAAAAAAAAAAA...",
                "coredump reports should truncate argv when its too long",
            );
            expected_idx += 1;
        }

        eprintln!("destroying child before another iteration of the test loop...");
        let on_destroy = instance.take_destroy_waiter();
        drop(instance);
        on_destroy.await.unwrap();
    }
}

// Snapshotting inspect can be a bit racy -- over enough runs we'll end up reading the node
// hierarchy at points where the node exists but none of its children, where nodes exist but not
// one of their properties, etc. Return an Option here for cases where we couldn't actually read the
// coredump report so the caller can try again..
async fn get_coredumps_from_inspect() -> Option<Vec<CoredumpReport>> {
    let kernel_inspect = ArchiveReader::new()
        .select_all_for_moniker("kernel")
        .with_minimum_schema_count(1)
        .snapshot::<Inspect>()
        .await
        .ok()?;
    assert_eq!(kernel_inspect.len(), 1);
    CoredumpReport::extract_from_snapshot(&kernel_inspect[0])
}
