// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

pub mod aggregations;

use {
    crate::aggregations::SumAndCount,
    fuchsia_inspect::{ArrayProperty, Node as InspectNode},
    fuchsia_inspect_contrib::nodes::NodeExt,
    std::{collections::VecDeque, default::Default},
};

pub struct WindowedStats<T> {
    stats: VecDeque<T>,
    capacity: usize,
    aggregation_fn: Box<dyn Fn(&T, &T) -> T + Send>,
}

impl<T: Default> WindowedStats<T> {
    pub fn new(capacity: usize, aggregation_fn: Box<dyn Fn(&T, &T) -> T + Send>) -> Self {
        let mut stats = VecDeque::with_capacity(capacity);
        stats.push_back(T::default());
        Self { stats, capacity, aggregation_fn }
    }

    /// Get stat of all the windows that are still kept if `n` is None.
    /// Otherwise, get stat for up to `n` windows.
    pub fn windowed_stat(&self, n: Option<usize>) -> T {
        let mut aggregated_value = T::default();
        let n = n.unwrap_or(self.stats.len());
        for value in self.stats.iter().rev().take(n) {
            aggregated_value = (self.aggregation_fn)(&aggregated_value, &value);
        }
        aggregated_value
    }

    pub fn slide_window(&mut self) {
        if !self.stats.is_empty() && self.stats.len() >= self.capacity {
            let _ = self.stats.pop_front();
        }
        self.stats.push_back(T::default());
    }
}

impl<T> WindowedStats<T> {
    pub fn add_value(&mut self, value: &T) {
        if let Some(latest) = self.stats.back_mut() {
            *latest = (self.aggregation_fn)(latest, value);
        }
    }
}

impl<T: Into<u64> + Clone> WindowedStats<T> {
    pub fn log_inspect_uint_array(&self, node: &InspectNode, property_name: &'static str) {
        let iter = self.stats.iter();
        let inspect_array = node.create_uint_array(property_name, iter.len());
        for (i, c) in iter.enumerate() {
            inspect_array.set(i, (*c).clone());
        }
        node.record(inspect_array);
    }
}

impl<T: Into<i64> + Clone> WindowedStats<T> {
    pub fn log_inspect_int_array(&self, node: &InspectNode, property_name: &'static str) {
        let iter = self.stats.iter();
        let inspect_array = node.create_int_array(property_name, iter.len());
        for (i, c) in iter.enumerate() {
            inspect_array.set(i, (*c).clone());
        }
        node.record(inspect_array);
    }
}

impl WindowedStats<SumAndCount> {
    pub fn log_avg_inspect_double_array(&self, node: &InspectNode, property_name: &'static str) {
        let iter = self.stats.iter();
        let inspect_array = node.create_double_array(property_name, iter.len());
        for (i, c) in iter.enumerate() {
            let value = if c.avg().is_finite() { c.avg() } else { 0f64 };
            inspect_array.set(i, value);
        }
        node.record(inspect_array);
    }
}

pub struct MinutelyWindows(pub usize);
pub struct FifteenMinutelyWindows(pub usize);
pub struct HourlyWindows(pub usize);

pub struct CombinedWindowedStats<T> {
    minutely: WindowedStats<T>,
    fifteen_minutely: WindowedStats<T>,
    hourly: WindowedStats<T>,
    tick: u64,
}

impl<T: Default> CombinedWindowedStats<T> {
    pub fn new(create_aggregation_fn: impl Fn() -> Box<dyn Fn(&T, &T) -> T + Send>) -> Self {
        Self::with_n_windows(
            MinutelyWindows(60),
            FifteenMinutelyWindows(24),
            HourlyWindows(24),
            create_aggregation_fn,
        )
    }

    pub fn with_n_windows(
        minutely_windows: MinutelyWindows,
        fifteen_minutely_windows: FifteenMinutelyWindows,
        hourly_windows: HourlyWindows,
        create_aggregation_fn: impl Fn() -> Box<dyn Fn(&T, &T) -> T + Send>,
    ) -> Self {
        Self {
            minutely: WindowedStats::new(minutely_windows.0, create_aggregation_fn()),
            fifteen_minutely: WindowedStats::new(
                fifteen_minutely_windows.0,
                create_aggregation_fn(),
            ),
            hourly: WindowedStats::new(hourly_windows.0, create_aggregation_fn()),
            tick: 0,
        }
    }

    pub fn slide_minute(&mut self) {
        self.tick += 1;

        self.minutely.slide_window();
        if self.tick % 15 == 0 {
            self.fifteen_minutely.slide_window();
        }
        if self.tick % 60 == 0 {
            self.hourly.slide_window();
        }
    }
}

impl<T> CombinedWindowedStats<T> {
    pub fn add_value(&mut self, item: &T) {
        self.minutely.add_value(item);
        self.fifteen_minutely.add_value(item);
        self.hourly.add_value(item);
    }
}

impl<T: Into<u64> + Clone> CombinedWindowedStats<T> {
    pub fn log_inspect_uint_array(&mut self, node: &InspectNode, child_name: &'static str) {
        let child = node.create_child(child_name);
        child.record_time("@time");
        self.minutely.log_inspect_uint_array(&child, "1m");
        self.fifteen_minutely.log_inspect_uint_array(&child, "15m");
        self.hourly.log_inspect_uint_array(&child, "1h");
        node.record(child);
    }
}

impl<T: Into<i64> + Clone> CombinedWindowedStats<T> {
    pub fn log_inspect_int_array(&mut self, node: &InspectNode, child_name: &'static str) {
        let child = node.create_child(child_name);
        child.record_time("@time");
        self.minutely.log_inspect_int_array(&child, "1m");
        self.fifteen_minutely.log_inspect_int_array(&child, "15m");
        self.hourly.log_inspect_int_array(&child, "1h");
        node.record(child);
    }
}

impl CombinedWindowedStats<SumAndCount> {
    pub fn log_avg_inspect_double_array(&mut self, node: &InspectNode, child_name: &'static str) {
        let child = node.create_child(child_name);
        child.record_time("@time");
        self.minutely.log_avg_inspect_double_array(&child, "1m");
        self.fifteen_minutely.log_avg_inspect_double_array(&child, "15m");
        self.hourly.log_avg_inspect_double_array(&child, "1h");
        node.record(child);
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::aggregations::create_saturating_add_fn,
        fuchsia_async as fasync,
        fuchsia_inspect::{assert_data_tree, Inspector},
    };

    #[test]
    fn windowed_stats_some_windows_populated() {
        let mut windowed_stats = WindowedStats::<u32>::new(3, create_saturating_add_fn());
        windowed_stats.add_value(&1u32);
        windowed_stats.add_value(&2u32);
        assert_eq!(windowed_stats.windowed_stat(None), 3u32);

        windowed_stats.slide_window();
        windowed_stats.add_value(&3u32);
        assert_eq!(windowed_stats.windowed_stat(None), 6u32);
    }

    #[test]
    fn windowed_stats_all_windows_populated() {
        let mut windowed_stats = WindowedStats::<u32>::new(3, create_saturating_add_fn());
        windowed_stats.add_value(&1u32);
        assert_eq!(windowed_stats.windowed_stat(None), 1u32);

        windowed_stats.slide_window();
        windowed_stats.add_value(&2u32);
        assert_eq!(windowed_stats.windowed_stat(None), 3u32);

        windowed_stats.slide_window();
        windowed_stats.add_value(&3u32);
        assert_eq!(windowed_stats.windowed_stat(None), 6u32);

        windowed_stats.slide_window();
        windowed_stats.add_value(&10u32);
        // Value 1 from the first window is excluded
        assert_eq!(windowed_stats.windowed_stat(None), 15u32);
    }

    #[test]
    fn windowed_stats_large_number() {
        let mut windowed_stats = WindowedStats::<u32>::new(3, create_saturating_add_fn());
        windowed_stats.add_value(&10u32);

        windowed_stats.slide_window();
        windowed_stats.add_value(&10u32);

        windowed_stats.slide_window();
        windowed_stats.add_value(&(u32::MAX - 20u32));
        assert_eq!(windowed_stats.windowed_stat(None), u32::MAX);

        windowed_stats.slide_window();
        windowed_stats.add_value(&9u32);
        assert_eq!(windowed_stats.windowed_stat(None), u32::MAX - 1);
    }

    #[test]
    fn windowed_stats_test_overflow() {
        let mut windowed_stats = WindowedStats::<u32>::new(3, create_saturating_add_fn());
        // Overflow in a single window
        windowed_stats.add_value(&u32::MAX);
        windowed_stats.add_value(&1u32);
        assert_eq!(windowed_stats.windowed_stat(None), u32::MAX);

        windowed_stats.slide_window();
        windowed_stats.add_value(&10u32);
        assert_eq!(windowed_stats.windowed_stat(None), u32::MAX);
        windowed_stats.slide_window();
        windowed_stats.add_value(&5u32);
        assert_eq!(windowed_stats.windowed_stat(None), u32::MAX);
        windowed_stats.slide_window();
        windowed_stats.add_value(&3u32);
        assert_eq!(windowed_stats.windowed_stat(None), 18u32);
    }

    #[test]
    fn windowed_stats_n_arg() {
        let mut windowed_stats = WindowedStats::<u32>::new(3, create_saturating_add_fn());
        windowed_stats.add_value(&1u32);
        assert_eq!(windowed_stats.windowed_stat(Some(0)), 0u32);
        assert_eq!(windowed_stats.windowed_stat(Some(1)), 1u32);
        assert_eq!(windowed_stats.windowed_stat(Some(2)), 1u32);

        windowed_stats.slide_window();
        windowed_stats.add_value(&2u32);
        assert_eq!(windowed_stats.windowed_stat(Some(1)), 2u32);
        assert_eq!(windowed_stats.windowed_stat(Some(2)), 3u32);
    }

    #[test]
    fn windowed_stats_log_inspect_uint_array() {
        let inspector = Inspector::default();
        let mut windowed_stats = WindowedStats::<u32>::new(3, create_saturating_add_fn());
        windowed_stats.add_value(&1u32);
        windowed_stats.slide_window();
        windowed_stats.add_value(&2u32);

        windowed_stats.log_inspect_uint_array(inspector.root(), "stats");

        assert_data_tree!(inspector, root: {
            stats: vec![1u64, 2],
        });
    }

    #[test]
    fn windowed_stats_log_inspect_int_array() {
        let inspector = Inspector::default();
        let mut windowed_stats = WindowedStats::<i32>::new(3, create_saturating_add_fn());
        windowed_stats.add_value(&1i32);
        windowed_stats.slide_window();
        windowed_stats.add_value(&2i32);

        windowed_stats.log_inspect_int_array(inspector.root(), "stats");

        assert_data_tree!(inspector, root: {
            stats: vec![1i64, 2],
        });
    }

    #[test]
    fn windowed_stats_sum_and_count_log_avg_inspect_double_array() {
        let inspector = Inspector::default();
        let mut windowed_stats = WindowedStats::<SumAndCount>::new(3, create_saturating_add_fn());
        windowed_stats.add_value(&SumAndCount { sum: 1u32, count: 1 });
        windowed_stats.add_value(&SumAndCount { sum: 2u32, count: 1 });

        windowed_stats.log_avg_inspect_double_array(inspector.root(), "stats");

        assert_data_tree!(inspector, root: {
            stats: vec![3f64 / 2f64],
        });
    }

    #[test]
    fn windowed_stats_sum_and_count_log_avg_inspect_double_array_with_nan() {
        let inspector = Inspector::default();
        let windowed_stats = WindowedStats::<SumAndCount>::new(3, create_saturating_add_fn());

        windowed_stats.log_avg_inspect_double_array(inspector.root(), "stats");

        assert_data_tree!(inspector, root: {
            stats: vec![0f64],
        });
    }

    #[test]
    fn combined_windowed_stats_log_inspect_uint_array() {
        let exec = fasync::TestExecutor::new_with_fake_time();
        exec.set_fake_time(fasync::Time::from_nanos(123456789));
        let inspector = Inspector::default();

        let mut windowed_stats = CombinedWindowedStats::<u32>::new(create_saturating_add_fn);
        windowed_stats.add_value(&1u32);
        windowed_stats.add_value(&2u32);
        windowed_stats.slide_minute();
        windowed_stats.add_value(&11u32);
        for _i in 0..14 {
            windowed_stats.slide_minute();
        }
        windowed_stats.add_value(&22u32);

        windowed_stats.log_inspect_uint_array(inspector.root(), "stats");

        assert_data_tree!(inspector, root: {
            stats: {
                "@time": 123456789i64,
                "1m": vec![3u64, 11, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 22],
                "15m": vec![14u64, 22],
                "1h": vec![36u64],
            }
        });
    }

    #[test]
    fn combined_windowed_stats_log_inspect_int_array() {
        let exec = fasync::TestExecutor::new_with_fake_time();
        exec.set_fake_time(fasync::Time::from_nanos(123456789));
        let inspector = Inspector::default();

        let mut windowed_stats = CombinedWindowedStats::<i32>::new(create_saturating_add_fn);
        windowed_stats.add_value(&1i32);

        windowed_stats.log_inspect_int_array(inspector.root(), "stats");

        assert_data_tree!(inspector, root: {
            stats: {
                "@time": 123456789i64,
                "1m": vec![1i64],
                "15m": vec![1i64],
                "1h": vec![1i64],
            }
        });
    }

    #[test]
    fn combined_windowed_stats_sum_and_count_log_avg_inspect_double_array() {
        let exec = fasync::TestExecutor::new_with_fake_time();
        exec.set_fake_time(fasync::Time::from_nanos(123456789));
        let inspector = Inspector::default();
        let mut windowed_stats =
            CombinedWindowedStats::<SumAndCount>::new(create_saturating_add_fn);
        windowed_stats.add_value(&SumAndCount { sum: 1u32, count: 1 });
        windowed_stats.add_value(&SumAndCount { sum: 2u32, count: 1 });

        windowed_stats.log_avg_inspect_double_array(inspector.root(), "stats");

        assert_data_tree!(inspector, root: {
            stats: {
                "@time": 123456789i64,
                "1m": vec![3f64 / 2f64],
                "15m": vec![3f64 / 2f64],
                "1h": vec![3f64 / 2f64],
            }
        });
    }
}
