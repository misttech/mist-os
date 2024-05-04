// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

pub mod escrow;
pub mod lifecycle;

/// This module tests the diagnostics code with the actual component hierarchy.
#[cfg(test)]
mod tests {
    use {
        crate::model::testing::routing_test_helpers::RoutingTest, cm_rust_testing::*,
        diagnostics_hierarchy::DiagnosticsHierarchy, fuchsia_inspect::DiagnosticsHierarchyGetter,
        fuchsia_zircon::AsHandleRef,
    };

    fn get_data(
        hierarchy: &DiagnosticsHierarchy,
        moniker: &str,
        task: Option<&str>,
    ) -> (Vec<i64>, Vec<i64>, Vec<i64>) {
        let mut path = vec!["stats", "measurements", "components", moniker];
        if let Some(task) = task {
            path.push(task);
        }
        get_data_at(&hierarchy, &path)
    }

    fn get_data_at(
        hierarchy: &DiagnosticsHierarchy,
        path: &[&str],
    ) -> (Vec<i64>, Vec<i64>, Vec<i64>) {
        let node = hierarchy.get_child_by_path(&path).expect("found stats node");
        let cpu_times = node
            .get_property("cpu_times")
            .expect("found cpu")
            .int_array()
            .expect("cpu are ints")
            .raw_values();
        let queue_times = node
            .get_property("queue_times")
            .expect("found queue")
            .int_array()
            .expect("queue are ints")
            .raw_values();
        let timestamps = node
            .get_property("timestamps")
            .expect("found timestamps")
            .int_array()
            .expect("timestamps are ints")
            .raw_values();
        (timestamps.into_owned(), cpu_times.into_owned(), queue_times.into_owned())
    }

    #[fuchsia::test]
    async fn component_manager_stats_are_tracked() {
        // Set up the test
        let test = RoutingTest::new(
            "root",
            vec![(
                "root",
                ComponentDeclBuilder::new()
                    .child(ChildBuilder::new().name("a").eager().build())
                    .build(),
            )],
        )
        .await;

        let koid =
            fuchsia_runtime::job_default().basic_info().expect("got basic info").koid.raw_koid();

        let hierarchy = test.builtin_environment.inspector().get_diagnostics_hierarchy();
        let (timestamps, cpu_times, queue_times) =
            get_data(&hierarchy, "<component_manager>", Some(&koid.to_string()));
        assert_eq!(timestamps.len(), 1);
        assert_eq!(cpu_times.len(), 1);
        assert_eq!(queue_times.len(), 1);
    }
}
