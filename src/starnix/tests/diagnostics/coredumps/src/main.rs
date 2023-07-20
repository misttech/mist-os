// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use diagnostics_reader::{ArchiveReader, DiagnosticsHierarchy, Inspect};
use fuchsia_component_test::ScopedInstance;

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
            let observed_coredumps = get_coredumps_from_inspect().await;
            if let Some(most_recent) = observed_coredumps.last() {
                if most_recent.idx == current_idx {
                    break observed_coredumps;
                }
            }
            eprintln!("waiting for coredump ({current_idx}/{max_idx}) to show up...");
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
            assert_eq!(
                coredump.pid,
                expected_idx as i64 + 3,
                "init is pid 1, kthread gets pid 2, coredumps start at 3, nothing else runs here",
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

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
struct CoredumpReport {
    idx: usize,
    pid: i64,
    argv: String,
}

async fn get_coredumps_from_inspect() -> Vec<CoredumpReport> {
    let kernel_inspect = ArchiveReader::new()
        .select_all_for_moniker("kernel")
        .with_minimum_schema_count(1)
        .retry_if_empty(true)
        .snapshot::<Inspect>()
        .await
        .unwrap();
    assert_eq!(kernel_inspect.len(), 1);

    // Examine all of the properties and children so that we are alerted that this test may need to
    // be updated if coredump reports gain new information.
    let DiagnosticsHierarchy { name, properties, children, .. } = kernel_inspect[0]
        .payload
        .as_ref()
        .unwrap()
        .get_child_by_path(&["container", "kernel", "coredumps"])
        .cloned()
        .unwrap();
    assert_eq!(name, "coredumps");
    assert_eq!(properties, vec![]);

    let mut reports = vec![];
    for DiagnosticsHierarchy {
        name: idx_str,
        properties: coredump_properties,
        children: coredump_children,
        ..
    } in children
    {
        assert_eq!(coredump_children, vec![], "coredump reports aren't expected to have children");

        let mut argv = None;
        let mut pid = None;
        for property in coredump_properties {
            match property.name() {
                "argv" => argv = Some(property.string().unwrap().to_string()),

                // TODO(https://fxbug.dev/130834) i64/int in kernel shows up as u64/uint here
                "pid" => pid = Some(*property.uint().unwrap() as i64),
                other => panic!("unrecognized coredump report property `{other}`"),
            }
        }

        reports.push(CoredumpReport {
            idx: idx_str.parse().unwrap(),
            pid: pid.unwrap(),
            argv: argv.unwrap(),
        });
    }
    reports.sort();

    reports
}
