// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fuchsia_inspect::{Inspector, Node as InspectNode};
use fuchsia_sync::Mutex;
use futures::FutureExt;
use std::sync::Arc;
use windowed_stats::aggregations::{create_saturating_add_fn, SumAndCount};
use windowed_stats::{FifteenMinutelyWindows, HourlyWindows, MinutelyWindows, TimeSeries};

#[derive(Debug)]
pub struct Stats {
    pub(crate) reachability_lost_count: TimeSeries<u32>,
    pub(crate) internet_available_sec: TimeSeries<i32>,
    pub(crate) dns_active_sec: TimeSeries<i32>,
    pub(crate) http_active_sec: TimeSeries<i32>,
    pub(crate) total_duration_sec: TimeSeries<i32>,
    pub(crate) ipv4_state: TimeSeries<SumAndCount>,
    pub(crate) ipv6_state: TimeSeries<SumAndCount>,
}

impl Stats {
    pub(crate) fn new() -> Self {
        Self {
            reachability_lost_count: TimeSeries::new(create_saturating_add_fn),
            internet_available_sec: TimeSeries::new(create_saturating_add_fn),
            dns_active_sec: TimeSeries::new(create_saturating_add_fn),
            http_active_sec: TimeSeries::new(create_saturating_add_fn),

            // `total_duration_sec` is served as the denominator for duration stats like
            // `internet_available_sec` and `dns_active_sec`. This is really only needed
            // for the most recent window, so we just instantiate window sizes 1 to save
            // space. For preceding windows with fully elapsed time, it's already implied
            // that for example, the denominator for the minutely window would be 60 seconds.
            total_duration_sec: TimeSeries::with_n_windows(
                MinutelyWindows(1),
                FifteenMinutelyWindows(1),
                HourlyWindows(1),
                create_saturating_add_fn,
            ),

            ipv4_state: TimeSeries::new(create_saturating_add_fn),
            ipv6_state: TimeSeries::new(create_saturating_add_fn),
        }
    }

    pub(crate) fn log_inspect(&mut self, node: &InspectNode) {
        self.reachability_lost_count.log_inspect_uint_array(node, "reachability_lost_count");
        self.internet_available_sec.log_inspect_int_array(node, "internet_available_sec");
        self.dns_active_sec.log_inspect_int_array(node, "dns_active_sec");
        self.http_active_sec.log_inspect_int_array(node, "http_active_sec");
        self.total_duration_sec.log_inspect_int_array(node, "total_duration_sec");
        self.ipv4_state.log_avg_inspect_double_array(node, "ipv4_state");
        self.ipv6_state.log_avg_inspect_double_array(node, "ipv6_state");
    }
}

pub(crate) fn inspect_record_stats(
    inspect_node: &InspectNode,
    child_name: &str,
    stats: Arc<Mutex<Stats>>,
) {
    inspect_node.record_lazy_child(child_name, move || {
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
