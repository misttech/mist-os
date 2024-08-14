// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fuchsia_inspect::{Inspector, Node as InspectNode};
use fuchsia_sync::Mutex;
use futures::FutureExt;
use std::sync::Arc;

pub trait TimeSeriesStats: std::fmt::Debug + Send {
    fn interpolate_data(&mut self);
    fn log_inspect(&mut self, node: &InspectNode);
}

pub(crate) fn inspect_attach_values(
    inspect_node: &InspectNode,
    callback_name: &str,
    stats: Arc<Mutex<dyn TimeSeriesStats>>,
) {
    inspect_node.record_lazy_values(callback_name, move || {
        let stats = Arc::clone(&stats);
        async move {
            let inspector = Inspector::default();
            {
                stats.lock().log_inspect(inspector.root());
            }
            Ok(inspector)
        }
        .boxed()
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use diagnostics_assertions::assert_data_tree;
    use fuchsia_async as fasync;

    #[derive(Debug)]
    struct TimeSeriesStatsTestImpl;
    impl TimeSeriesStats for TimeSeriesStatsTestImpl {
        fn interpolate_data(&mut self) {}
        fn log_inspect(&mut self, node: &InspectNode) {
            node.record_string("test_property_key", "test_property_value");
        }
    }

    #[test]
    fn test_inspect_create_stats() {
        let _exec = fasync::TestExecutor::new_with_fake_time();
        let inspector = Inspector::default();
        let inspect_node = inspector.root().create_child("test_stats");
        let time_series_stats = Arc::new(Mutex::new(TimeSeriesStatsTestImpl));
        inspect_attach_values(&inspect_node, "callback_name", time_series_stats.clone());
        assert_data_tree!(inspector, root: {
            test_stats: {
                test_property_key: "test_property_value",
            }
        })
    }
}
