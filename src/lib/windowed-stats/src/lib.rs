// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

pub mod aggregations;
pub mod experimental;

use crate::aggregations::SumAndCount;
use fuchsia_async as fasync;
use fuchsia_inspect::{ArrayProperty, Node as InspectNode};

use std::collections::VecDeque;
use std::fmt::{self, Debug};

pub struct WindowedStats<T> {
    stats: VecDeque<T>,
    capacity: usize,
    aggregation_fn: Box<dyn Fn(&T, &T) -> T + Send>,
}

impl<T: Debug> Debug for WindowedStats<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WindowedStats")
            .field("stats", &self.stats)
            .field("capacity", &self.capacity)
            .finish_non_exhaustive()
    }
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
    pub fn log_value(&mut self, value: &T) {
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

/// Given `timestamp`, return the start bound of its enclosing window with the specified
/// `granularity`.
fn get_start_bound(
    timestamp: fasync::BootInstant,
    granularity: zx::BootDuration,
) -> fasync::BootInstant {
    timestamp - zx::BootDuration::from_nanos(timestamp.into_nanos() % granularity.into_nanos())
}

/// Given the `prev` and `current` timestamps, return how many windows need to be slided
/// for if each window has the specified `granularity`.
fn get_num_slides_needed(
    prev: fasync::BootInstant,
    current: fasync::BootInstant,
    granularity: zx::BootDuration,
) -> usize {
    let prev_start_bound = get_start_bound(prev, granularity);
    let current_start_bound = get_start_bound(current, granularity);
    ((current_start_bound - prev_start_bound).into_nanos() / granularity.into_nanos())
        .try_into()
        .unwrap_or(0)
}

pub struct MinutelyWindows(pub usize);
pub struct FifteenMinutelyWindows(pub usize);
pub struct HourlyWindows(pub usize);

pub struct TimeSeries<T> {
    minutely: WindowedStats<T>,
    fifteen_minutely: WindowedStats<T>,
    hourly: WindowedStats<T>,
    /// Time time when the `TimeSeries` was first created
    created_timestamp: fasync::BootInstant,
    /// The time when data in the current `TimeSeries` was last updated
    last_timestamp: fasync::BootInstant,
}

impl<T: Debug> Debug for TimeSeries<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TimeSeries")
            .field("minutely", &self.minutely)
            .field("fifteen_minutely", &self.fifteen_minutely)
            .field("hourly", &self.hourly)
            .field("created_timestamp", &self.created_timestamp)
            .field("last_timestamp", &self.last_timestamp)
            .finish()
    }
}

impl<T: Default> TimeSeries<T> {
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
        let now = fasync::BootInstant::now();
        Self {
            minutely: WindowedStats::new(minutely_windows.0, create_aggregation_fn()),
            fifteen_minutely: WindowedStats::new(
                fifteen_minutely_windows.0,
                create_aggregation_fn(),
            ),
            hourly: WindowedStats::new(hourly_windows.0, create_aggregation_fn()),
            created_timestamp: now,
            last_timestamp: now,
        }
    }

    /// Check whether the current time has exceeded the bound of the existing windows. If yes
    /// then slide windows as many times as required until the window encompasses the current time.
    pub fn update_windows(&mut self) {
        let now = fasync::BootInstant::now();
        for _i in
            0..get_num_slides_needed(self.last_timestamp, now, zx::BootDuration::from_minutes(1))
        {
            self.minutely.slide_window();
        }
        for _i in
            0..get_num_slides_needed(self.last_timestamp, now, zx::BootDuration::from_minutes(15))
        {
            self.fifteen_minutely.slide_window();
        }
        for _i in
            0..get_num_slides_needed(self.last_timestamp, now, zx::BootDuration::from_hours(1))
        {
            self.hourly.slide_window();
        }
        self.last_timestamp = now;
    }

    /// Log the value into `TimeSeries`. This operation automatically updates the windows.
    pub fn log_value(&mut self, item: &T) {
        self.update_windows();
        self.minutely.log_value(item);
        self.fifteen_minutely.log_value(item);
        self.hourly.log_value(item);
    }

    /// Get Iterator to traverse the minutely windows
    pub fn minutely_iter<'a>(&'a self) -> impl ExactSizeIterator<Item = &'a T> {
        self.minutely.stats.iter()
    }

    /// Get the aggregated value of the data that are still kept
    pub fn get_aggregated_value(&mut self) -> T {
        self.hourly.windowed_stat(None)
    }

    pub fn record_schema(&mut self, node: &InspectNode) {
        let schema = node.create_child("schema");
        schema.record_int("created_timestamp", self.created_timestamp.into_nanos());
        schema.record_int("last_timestamp", self.last_timestamp.into_nanos());
        node.record(schema);
    }
}

impl<T: Into<u64> + Clone + Default> TimeSeries<T> {
    pub fn log_inspect_uint_array(&mut self, node: &InspectNode, child_name: &'static str) {
        self.update_windows();

        let child = node.create_child(child_name);
        self.record_schema(&child);
        self.minutely.log_inspect_uint_array(&child, "1m");
        self.fifteen_minutely.log_inspect_uint_array(&child, "15m");
        self.hourly.log_inspect_uint_array(&child, "1h");
        node.record(child);
    }
}

impl<T: Into<i64> + Clone + Default> TimeSeries<T> {
    pub fn log_inspect_int_array(&mut self, node: &InspectNode, child_name: &'static str) {
        self.update_windows();

        let child = node.create_child(child_name);
        self.record_schema(&child);
        self.minutely.log_inspect_int_array(&child, "1m");
        self.fifteen_minutely.log_inspect_int_array(&child, "15m");
        self.hourly.log_inspect_int_array(&child, "1h");
        node.record(child);
    }
}

impl TimeSeries<SumAndCount> {
    pub fn log_avg_inspect_double_array(&mut self, node: &InspectNode, child_name: &'static str) {
        self.update_windows();

        let child = node.create_child(child_name);
        self.record_schema(&child);
        self.minutely.log_avg_inspect_double_array(&child, "1m");
        self.fifteen_minutely.log_avg_inspect_double_array(&child, "15m");
        self.hourly.log_avg_inspect_double_array(&child, "1h");
        node.record(child);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aggregations::create_saturating_add_fn;
    use diagnostics_assertions::assert_data_tree;
    use fuchsia_inspect::Inspector;

    #[test]
    fn windowed_stats_some_windows_populated() {
        let mut windowed_stats = WindowedStats::<u32>::new(3, create_saturating_add_fn());
        windowed_stats.log_value(&1u32);
        windowed_stats.log_value(&2u32);
        assert_eq!(windowed_stats.windowed_stat(None), 3u32);

        windowed_stats.slide_window();
        windowed_stats.log_value(&3u32);
        assert_eq!(windowed_stats.windowed_stat(None), 6u32);
    }

    #[test]
    fn windowed_stats_all_windows_populated() {
        let mut windowed_stats = WindowedStats::<u32>::new(3, create_saturating_add_fn());
        windowed_stats.log_value(&1u32);
        assert_eq!(windowed_stats.windowed_stat(None), 1u32);

        windowed_stats.slide_window();
        windowed_stats.log_value(&2u32);
        assert_eq!(windowed_stats.windowed_stat(None), 3u32);

        windowed_stats.slide_window();
        windowed_stats.log_value(&3u32);
        assert_eq!(windowed_stats.windowed_stat(None), 6u32);

        windowed_stats.slide_window();
        windowed_stats.log_value(&10u32);
        // Value 1 from the first window is excluded
        assert_eq!(windowed_stats.windowed_stat(None), 15u32);
    }

    #[test]
    fn windowed_stats_large_number() {
        let mut windowed_stats = WindowedStats::<u32>::new(3, create_saturating_add_fn());
        windowed_stats.log_value(&10u32);

        windowed_stats.slide_window();
        windowed_stats.log_value(&10u32);

        windowed_stats.slide_window();
        windowed_stats.log_value(&(u32::MAX - 20u32));
        assert_eq!(windowed_stats.windowed_stat(None), u32::MAX);

        windowed_stats.slide_window();
        windowed_stats.log_value(&9u32);
        assert_eq!(windowed_stats.windowed_stat(None), u32::MAX - 1);
    }

    #[test]
    fn windowed_stats_test_overflow() {
        let mut windowed_stats = WindowedStats::<u32>::new(3, create_saturating_add_fn());
        // Overflow in a single window
        windowed_stats.log_value(&u32::MAX);
        windowed_stats.log_value(&1u32);
        assert_eq!(windowed_stats.windowed_stat(None), u32::MAX);

        windowed_stats.slide_window();
        windowed_stats.log_value(&10u32);
        assert_eq!(windowed_stats.windowed_stat(None), u32::MAX);
        windowed_stats.slide_window();
        windowed_stats.log_value(&5u32);
        assert_eq!(windowed_stats.windowed_stat(None), u32::MAX);
        windowed_stats.slide_window();
        windowed_stats.log_value(&3u32);
        assert_eq!(windowed_stats.windowed_stat(None), 18u32);
    }

    #[test]
    fn windowed_stats_n_arg() {
        let mut windowed_stats = WindowedStats::<u32>::new(3, create_saturating_add_fn());
        windowed_stats.log_value(&1u32);
        assert_eq!(windowed_stats.windowed_stat(Some(0)), 0u32);
        assert_eq!(windowed_stats.windowed_stat(Some(1)), 1u32);
        assert_eq!(windowed_stats.windowed_stat(Some(2)), 1u32);

        windowed_stats.slide_window();
        windowed_stats.log_value(&2u32);
        assert_eq!(windowed_stats.windowed_stat(Some(1)), 2u32);
        assert_eq!(windowed_stats.windowed_stat(Some(2)), 3u32);
    }

    #[test]
    fn windowed_stats_log_inspect_uint_array() {
        let inspector = Inspector::default();
        let mut windowed_stats = WindowedStats::<u32>::new(3, create_saturating_add_fn());
        windowed_stats.log_value(&1u32);
        windowed_stats.slide_window();
        windowed_stats.log_value(&2u32);

        windowed_stats.log_inspect_uint_array(inspector.root(), "stats");

        assert_data_tree!(inspector, root: {
            stats: vec![1u64, 2],
        });
    }

    #[test]
    fn windowed_stats_log_inspect_int_array() {
        let inspector = Inspector::default();
        let mut windowed_stats = WindowedStats::<i32>::new(3, create_saturating_add_fn());
        windowed_stats.log_value(&1i32);
        windowed_stats.slide_window();
        windowed_stats.log_value(&2i32);

        windowed_stats.log_inspect_int_array(inspector.root(), "stats");

        assert_data_tree!(inspector, root: {
            stats: vec![1i64, 2],
        });
    }

    #[test]
    fn windowed_stats_sum_and_count_log_avg_inspect_double_array() {
        let inspector = Inspector::default();
        let mut windowed_stats = WindowedStats::<SumAndCount>::new(3, create_saturating_add_fn());
        windowed_stats.log_value(&SumAndCount { sum: 1u32, count: 1 });
        windowed_stats.log_value(&SumAndCount { sum: 2u32, count: 1 });

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
    fn time_series_window_transition() {
        let exec = fasync::TestExecutor::new_with_fake_time();
        exec.set_fake_time(fasync::MonotonicInstant::from_nanos(3599_000_000_000));
        let inspector = Inspector::default();

        let mut time_series = TimeSeries::<u32>::new(create_saturating_add_fn);
        time_series.log_value(&1u32);

        // This should create a new windows for all of `1m`, `15m`, `1h` because we
        // just crossed the 3600th second mark.
        exec.set_fake_time(fasync::MonotonicInstant::from_nanos(3600_000_000_001));
        time_series.log_value(&2u32);

        time_series.log_inspect_uint_array(inspector.root(), "stats");
        assert_data_tree!(inspector, root: {
            stats: {
                "1m": vec![1u64, 2],
                "15m": vec![1u64, 2],
                "1h": vec![1u64, 2],
                schema: {
                    "created_timestamp": 3599_000_000_000i64,
                    "last_timestamp": 3600_000_000_001i64,
                }
            }
        });

        exec.set_fake_time(fasync::MonotonicInstant::from_nanos(3659_000_000_000));
        time_series.log_value(&3u32);
        time_series.log_inspect_uint_array(inspector.root(), "stats2");

        // No new window transition because we have not crossed the 3660th second mark
        assert_data_tree!(inspector, root: contains {
            stats2: {
                "1m": vec![1u64, 5],
                "15m": vec![1u64, 5],
                "1h": vec![1u64, 5],
                schema: {
                    "created_timestamp": 3599_000_000_000i64,
                    "last_timestamp": 3659_000_000_000i64,
                }
            }
        });

        exec.set_fake_time(fasync::MonotonicInstant::from_nanos(3660_000_000_000));
        time_series.log_value(&4u32);
        time_series.log_inspect_uint_array(inspector.root(), "stats3");

        // Only the `1m` window has a new transition because 3660th second marks a new
        // minutely threshold, but not hourly or fifteen-minutely threshold.
        assert_data_tree!(inspector, root: contains {
            stats3: {
                "1m": vec![1u64, 5, 4],
                "15m": vec![1u64, 9],
                "1h": vec![1u64, 9],
                schema: {
                    "created_timestamp": 3599_000_000_000i64,
                    "last_timestamp": 3660_000_000_000i64,
                }
            }
        });

        exec.set_fake_time(fasync::MonotonicInstant::from_nanos(4500_000_000_001));
        time_series.log_value(&5u32);
        time_series.log_inspect_uint_array(inspector.root(), "stats4");

        // Now the `15m` window has a new transition, too, because 3900th second marks a new
        // fifteen-minutely threshold (but not hourly threshold).
        // Also, the `1m` window has multiple transitions.
        assert_data_tree!(inspector, root: contains {
            stats4: {
                "1m": vec![1u64, 5, 4, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 5],
                "15m": vec![1u64, 9, 5],
                "1h": vec![1u64, 14],
                schema: {
                    "created_timestamp": 3599_000_000_000i64,
                    "last_timestamp": 4500_000_000_001i64,
                }
            }
        });
    }

    #[test]
    fn time_series_minutely_iter() {
        let exec = fasync::TestExecutor::new_with_fake_time();
        exec.set_fake_time(fasync::MonotonicInstant::from_nanos(59_000_000_000));
        let mut time_series = TimeSeries::<u32>::new(create_saturating_add_fn);
        time_series.log_value(&1u32);
        exec.set_fake_time(fasync::MonotonicInstant::from_nanos(60_000_000_000));
        time_series.log_value(&2u32);
        let minutely_data: Vec<_> = time_series.minutely_iter().map(|v| *v).collect();
        assert_eq!(minutely_data, [1, 2]);
    }

    #[test]
    fn time_series_get_aggregated_value() {
        let exec = fasync::TestExecutor::new_with_fake_time();
        exec.set_fake_time(fasync::MonotonicInstant::from_nanos(3599_000_000_000));
        let mut time_series = TimeSeries::<u32>::new(create_saturating_add_fn);
        time_series.log_value(&1u32);
        exec.set_fake_time(fasync::MonotonicInstant::from_nanos(3600_000_000_001));
        time_series.log_value(&2u32);
        assert_eq!(time_series.get_aggregated_value(), 3);
    }

    #[test]
    fn time_series_log_inspect_uint_array() {
        let exec = fasync::TestExecutor::new_with_fake_time();
        exec.set_fake_time(fasync::MonotonicInstant::from_nanos(961_000_000_000));
        let inspector = Inspector::default();

        let mut time_series = TimeSeries::<u32>::new(create_saturating_add_fn);
        time_series.log_value(&1u32);

        time_series.log_inspect_uint_array(inspector.root(), "stats");

        assert_data_tree!(inspector, root: {
            stats: {
                "1m": vec![1u64],
                "15m": vec![1u64],
                "1h": vec![1u64],
                schema: {
                    "created_timestamp": 961_000_000_000i64,
                    "last_timestamp": 961_000_000_000i64,
                }
            }
        });
    }

    #[test]
    fn time_series_log_inspect_uint_array_automatically_update_windows() {
        let exec = fasync::TestExecutor::new_with_fake_time();
        exec.set_fake_time(fasync::MonotonicInstant::from_nanos(961_000_000_000));
        let inspector = Inspector::default();

        let mut time_series = TimeSeries::<u32>::new(create_saturating_add_fn);
        time_series.log_value(&1u32);

        exec.set_fake_time(fasync::MonotonicInstant::from_nanos(1021_000_000_000));
        time_series.log_inspect_uint_array(inspector.root(), "stats");

        assert_data_tree!(inspector, root: {
            stats: {
                "1m": vec![1u64, 0],
                "15m": vec![1u64],
                "1h": vec![1u64],
                schema: {
                    "created_timestamp": 961_000_000_000i64,
                    "last_timestamp": 1021_000_000_000i64,
                }
            }
        });
    }

    #[test]
    fn time_series_stats_log_inspect_int_array() {
        let exec = fasync::TestExecutor::new_with_fake_time();
        exec.set_fake_time(fasync::MonotonicInstant::from_nanos(961_000_000_000));
        let inspector = Inspector::default();

        let mut time_series = TimeSeries::<i32>::new(create_saturating_add_fn);
        time_series.log_value(&1i32);

        time_series.log_inspect_int_array(inspector.root(), "stats");

        assert_data_tree!(inspector, root: {
            stats: {
                "1m": vec![1i64],
                "15m": vec![1i64],
                "1h": vec![1i64],
                schema: {
                    "created_timestamp": 961_000_000_000i64,
                    "last_timestamp": 961_000_000_000i64,
                }
            }
        });
    }

    #[test]
    fn time_series_log_inspect_int_array_automatically_update_windows() {
        let exec = fasync::TestExecutor::new_with_fake_time();
        exec.set_fake_time(fasync::MonotonicInstant::from_nanos(961_000_000_000));
        let inspector = Inspector::default();

        let mut time_series = TimeSeries::<i32>::new(create_saturating_add_fn);
        time_series.log_value(&1i32);

        exec.set_fake_time(fasync::MonotonicInstant::from_nanos(1021_000_000_000));
        time_series.log_inspect_int_array(inspector.root(), "stats");

        assert_data_tree!(inspector, root: {
            stats: {
                "1m": vec![1i64, 0],
                "15m": vec![1i64],
                "1h": vec![1i64],
                schema: {
                    "created_timestamp": 961_000_000_000i64,
                    "last_timestamp": 1021_000_000_000i64,
                }
            }
        });
    }

    #[test]
    fn time_series_sum_and_count_log_avg_inspect_double_array() {
        let exec = fasync::TestExecutor::new_with_fake_time();
        exec.set_fake_time(fasync::MonotonicInstant::from_nanos(961_000_000_000));
        let inspector = Inspector::default();
        let mut time_series = TimeSeries::<SumAndCount>::new(create_saturating_add_fn);
        time_series.log_value(&SumAndCount { sum: 1u32, count: 1 });
        time_series.log_value(&SumAndCount { sum: 2u32, count: 1 });

        time_series.log_avg_inspect_double_array(inspector.root(), "stats");

        assert_data_tree!(inspector, root: {
            stats: {
                "1m": vec![3f64 / 2f64],
                "15m": vec![3f64 / 2f64],
                "1h": vec![3f64 / 2f64],
                schema: {
                    "created_timestamp": 961_000_000_000i64,
                    "last_timestamp": 961_000_000_000i64,
                }
            }
        });
    }

    #[test]
    fn time_series_sum_and_count_log_avg_inspect_double_array_automatically_update_windows() {
        let exec = fasync::TestExecutor::new_with_fake_time();
        exec.set_fake_time(fasync::MonotonicInstant::from_nanos(961_000_000_000));
        let inspector = Inspector::default();
        let mut time_series = TimeSeries::<SumAndCount>::new(create_saturating_add_fn);
        time_series.log_value(&SumAndCount { sum: 1u32, count: 1 });
        time_series.log_value(&SumAndCount { sum: 2u32, count: 1 });

        exec.set_fake_time(fasync::MonotonicInstant::from_nanos(1021_000_000_000));
        time_series.log_avg_inspect_double_array(inspector.root(), "stats");

        assert_data_tree!(inspector, root: {
            stats: {
                "1m": vec![3f64 / 2f64, 0f64],
                "15m": vec![3f64 / 2f64],
                "1h": vec![3f64 / 2f64],
                schema: {
                    "created_timestamp": 961_000_000_000i64,
                    "last_timestamp": 1021_000_000_000i64,
                }
            }
        });
    }
}
