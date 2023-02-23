// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use num_traits::SaturatingAdd;
use std::collections::VecDeque;
use std::default::Default;

pub struct WindowedStats<T> {
    stats: VecDeque<T>,
    capacity: usize,
    aggregation_fn: Box<dyn Fn(&T, &T) -> T>,
}

impl<T: Default> WindowedStats<T> {
    pub fn new(capacity: usize, aggregation_fn: Box<dyn Fn(&T, &T) -> T>) -> Self {
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

    pub fn add_value(&mut self, value: &T) {
        if let Some(latest) = self.stats.back_mut() {
            *latest = (self.aggregation_fn)(latest, value);
        }
    }

    pub fn slide_window(&mut self) {
        if !self.stats.is_empty() && self.stats.len() >= self.capacity {
            let _ = self.stats.pop_front();
        }
        self.stats.push_back(T::default());
    }
}

pub fn saturating_add_fn<T: SaturatingAdd>() -> Box<dyn Fn(&T, &T) -> T> {
    Box::new(|value1, value2| value1.saturating_add(value2))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windowed_stats_some_windows_populated() {
        let mut windowed_stats = WindowedStats::<u32>::new(3, saturating_add_fn());
        windowed_stats.add_value(&1u32);
        windowed_stats.add_value(&2u32);
        assert_eq!(windowed_stats.windowed_stat(None), 3u32);

        windowed_stats.slide_window();
        windowed_stats.add_value(&3u32);
        assert_eq!(windowed_stats.windowed_stat(None), 6u32);
    }

    #[test]
    fn windowed_stats_all_windows_populated() {
        let mut windowed_stats = WindowedStats::<u32>::new(3, saturating_add_fn());
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
        let mut windowed_stats = WindowedStats::<u32>::new(3, saturating_add_fn());
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
        let mut windowed_stats = WindowedStats::<u32>::new(3, saturating_add_fn());
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
        let mut windowed_stats = WindowedStats::<u32>::new(3, saturating_add_fn());
        windowed_stats.add_value(&1u32);
        assert_eq!(windowed_stats.windowed_stat(Some(0)), 0u32);
        assert_eq!(windowed_stats.windowed_stat(Some(1)), 1u32);
        assert_eq!(windowed_stats.windowed_stat(Some(2)), 1u32);

        windowed_stats.slide_window();
        windowed_stats.add_value(&2u32);
        assert_eq!(windowed_stats.windowed_stat(Some(1)), 2u32);
        assert_eq!(windowed_stats.windowed_stat(Some(2)), 3u32);
    }
}
