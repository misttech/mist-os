// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

/// Logs an error message if the passed in `result` is an error.
#[macro_export]
macro_rules! log_if_err {
    ($result:expr, $log_prefix:expr) => {
        if let Err(e) = $result.as_ref() {
            log::error!("{}: {}", $log_prefix, e);
        }
    };
}

/// Logs an error message if the provided `cond` evaluates to false. Also passes the same expression
/// and message into `debug_assert!`, which will panic if debug assertions are enabled.
#[macro_export]
macro_rules! log_if_false_and_debug_assert {
    ($cond:expr, $msg:expr) => {
        if !($cond) {
            log::error!($msg);
            debug_assert!($cond, $msg);
        }
    };
    ($cond:expr, $fmt:expr, $($arg:tt)+) => {
        if !($cond) {
            log::error!($fmt, $($arg)+);
            debug_assert!($cond, $fmt, $($arg)+);
        }
    };
}

pub mod result_debug_panic {
    /// Convenience wrapper to cause a debug panic if a Result is Err.
    ///
    /// Example:
    ///     let ok_val = result.or_debug_panic()?;
    ///
    /// In the example, the try operator is used to unwrap an Ok value or return if it is Err, while
    /// also debug panicking if it is Err.
    pub trait ResultDebugPanic<T, E> {
        fn or_debug_panic(self) -> Result<T, E>;
    }

    impl<T, E> ResultDebugPanic<T, E> for Result<T, E> {
        fn or_debug_panic(self) -> Result<T, E> {
            self.or_else(|e| {
                debug_assert!(false);
                Err(e)
            })
        }
    }

    #[cfg(test)]
    mod tests {
        use super::ResultDebugPanic;

        #[test]
        fn test_ok_result() {
            let res: Result<(), ()> = Ok(());
            assert_eq!(res.or_debug_panic(), Ok(()));
        }

        #[test]
        #[should_panic]
        #[cfg(debug_assertions)]
        fn test_err_debug() {
            let res: Result<(), ()> = Err(());
            let _ = res.or_debug_panic(); // panics
        }

        #[test]
        #[cfg(not(debug_assertions))]
        fn test_err_nondebug() {
            let res: Result<(), ()> = Err(());
            assert_eq!(res.or_debug_panic(), Err(()));
        }
    }
}

#[cfg(test)]
mod log_err_with_debug_assert_tests {
    use crate::log_if_false_and_debug_assert;

    /// Tests that `log_if_false_and_debug_assert` panics for a false expression when debug
    /// assertions are enabled.
    #[test]
    #[should_panic(expected = "this will panic")]
    #[cfg(debug_assertions)]
    fn test_debug_assert() {
        log_if_false_and_debug_assert!(true, "this will not panic");
        log_if_false_and_debug_assert!(false, "this will panic");
    }

    /// Tests that `log_if_false_and_debug_assert` does not panic for a false expression when debug
    /// assertions are not enabled.
    #[test]
    #[cfg(not(debug_assertions))]
    fn test_non_debug_assert() {
        log_if_false_and_debug_assert!(true, "this will not panic");
        log_if_false_and_debug_assert!(false, "this will not panic either");
    }
}

/// Exports a convenience macro, `ok_or_default_err`, which acts as a wrapper for `Option::ok_or` to
/// provide a default Err value. The macro transforms an Option<T> into a Result<T, E>, mapping
/// Some(v) to Ok(v) and None to Err("$var_name is None").
#[macro_use]
pub mod ok_or_default_err {
    #[macro_export]
    macro_rules! ok_or_default_err {
        ($result:expr) => {
            $result.ok_or(anyhow::format_err!("{} is None", stringify!($result)))
        };
    }

    #[cfg(test)]
    mod tests {
        #[test]
        fn test_some() {
            let foo = Some(1);
            assert_eq!(ok_or_default_err!(foo).unwrap(), 1);
        }

        #[test]
        fn test_none() {
            let foo: Option<()> = None;
            assert_eq!(ok_or_default_err!(foo).unwrap_err().to_string(), "foo is None")
        }
    }
}

/// The number of nanoseconds since the system was powered on.
pub fn get_current_timestamp() -> crate::types::Nanoseconds {
    crate::types::Nanoseconds(fuchsia_async::Time::now().into_nanos())
}

use fidl_fuchsia_metrics::HistogramBucket;

/// Convenient wrapper for creating and storing an integer histogram to use with Cobalt.
pub struct CobaltIntHistogram {
    /// Underlying histogram data storage.
    data: Vec<HistogramBucket>,

    /// Number of data values that have been added to the histogram.
    data_count: u32,

    /// Histogram configuration parameters.
    config: CobaltIntHistogramConfig,
}

/// Histogram configuration parameters used by CobaltIntHistogram.
pub struct CobaltIntHistogramConfig {
    pub floor: i64,
    pub num_buckets: u32,
    pub step_size: u32,
}

impl CobaltIntHistogram {
    /// Create a new CobaltIntHistogram.
    pub fn new(config: CobaltIntHistogramConfig) -> Self {
        Self { data: Self::new_vec(config.num_buckets), data_count: 0, config }
    }

    /// Create a new Vec<HistogramBucket> that represents the underlying histogram storage. Two
    /// extra buckets are added for underflow and overflow.
    fn new_vec(num_buckets: u32) -> Vec<HistogramBucket> {
        (0..num_buckets + 2).map(|i| HistogramBucket { index: i, count: 0 }).collect()
    }

    /// Add a data value to the histogram.
    pub fn add_data(&mut self, n: i64) {
        // Add one to index to account for underflow bucket at index 0
        let mut index = 1 + (n - self.config.floor) / self.config.step_size as i64;

        // Clamp index to 0 and self.data.len() - 1, which Cobalt uses for underflow and overflow,
        // respectively
        index = num_traits::clamp(index, 0, self.data.len() as i64 - 1);

        self.data[index as usize].count += 1;
        self.data_count += 1;
    }

    /// Get the number of data elements that have been added to the histogram.
    pub fn count(&self) -> u32 {
        self.data_count
    }

    /// Clear the histogram.
    pub fn clear(&mut self) {
        self.data = Self::new_vec(self.config.num_buckets);
        self.data_count = 0;
    }

    /// Get the underlying Vec<HistogramBucket> of the histogram.
    pub fn get_data(&self) -> Vec<HistogramBucket> {
        self.data.clone()
    }
}

// Finds all of the node config files under the test package's "/config/data" directory. The node
// config files are identified by a suffix of "node_config.json5". The function then calls the
// provided `test_config_file` function for each found config file, passing the JSON structure in as
// an argument. The function returns success if each call to `test_config_file` succeeds. Otherwise,
// the first error encountered is returned.
#[cfg(test)]
pub fn test_each_node_config_file(
    test_config_file: impl Fn(&Vec<serde_json::Value>) -> Result<(), anyhow::Error>,
) -> Result<(), anyhow::Error> {
    use anyhow::Context as _;
    use std::fs;

    let suffix = "node_config.json5";
    let config_files = fs::read_dir("/pkg/config")
        .unwrap()
        .filter(|f| f.as_ref().unwrap().file_name().to_str().unwrap().ends_with(suffix))
        .map(|f| {
            let path = f.unwrap().path();
            let file_path = path.to_str().unwrap().to_string();
            let contents = std::fs::read_to_string(&file_path).unwrap();
            let json = serde_json5::from_str(&contents)
                .unwrap_or_else(|e| panic!("Failed to parse file {}: {:?}", file_path, e));

            (file_path, json)
        })
        .collect::<Vec<_>>();
    assert!(config_files.len() > 0, "No config files found");

    for (file_path, config_file) in config_files {
        test_config_file(&config_file).context(format!("Failed for file {}", file_path))?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// CobaltIntHistogram: tests that data added to the CobaltIntHistogram is correctly counted and
    /// bucketed.
    #[test]
    fn test_cobalt_histogram_data() {
        // Create the histogram and verify initial data count is 0
        let mut hist = CobaltIntHistogram::new(CobaltIntHistogramConfig {
            floor: 50,
            step_size: 10,
            num_buckets: 3,
        });
        assert_eq!(hist.count(), 0);

        // Add some arbitrary values, making sure some do not land on the bucket boundary to further
        // verify the bucketing logic
        hist.add_data(50);
        hist.add_data(65);
        hist.add_data(75);
        hist.add_data(79);

        // Verify the values were counted and bucketed properly
        assert_eq!(hist.count(), 4);
        assert_eq!(
            hist.get_data(),
            vec![
                HistogramBucket { index: 0, count: 0 }, // underflow
                HistogramBucket { index: 1, count: 1 },
                HistogramBucket { index: 2, count: 1 },
                HistogramBucket { index: 3, count: 2 },
                HistogramBucket { index: 4, count: 0 } // overflow
            ]
        );

        // Verify `clear` works as expected
        hist.clear();
        assert_eq!(hist.count(), 0);
        assert_eq!(
            hist.get_data(),
            vec![
                HistogramBucket { index: 0, count: 0 }, // underflow
                HistogramBucket { index: 1, count: 0 },
                HistogramBucket { index: 2, count: 0 },
                HistogramBucket { index: 3, count: 0 },
                HistogramBucket { index: 4, count: 0 }, // overflow
            ]
        );
    }

    /// CobaltIntHistogram: tests that invalid data values are logged in the correct
    /// underflow/overflow buckets.
    #[test]
    fn test_cobalt_histogram_invalid_data() {
        let mut hist = CobaltIntHistogram::new(CobaltIntHistogramConfig {
            floor: 0,
            step_size: 1,
            num_buckets: 2,
        });

        hist.add_data(-2);
        hist.add_data(-1);
        hist.add_data(0);
        hist.add_data(1);
        hist.add_data(2);

        assert_eq!(
            hist.get_data(),
            vec![
                HistogramBucket { index: 0, count: 2 }, // underflow
                HistogramBucket { index: 1, count: 1 },
                HistogramBucket { index: 2, count: 1 },
                HistogramBucket { index: 3, count: 1 } // overflow
            ]
        );
    }

    /// Tests that the `get_current_timestamp` function returns the expected current timestamp.
    #[test]
    fn test_get_current_timestamp() {
        use crate::types::Nanoseconds;

        let exec = fuchsia_async::TestExecutor::new_with_fake_time();

        exec.set_fake_time(fuchsia_async::Time::from_nanos(0));
        assert_eq!(get_current_timestamp(), Nanoseconds(0));

        exec.set_fake_time(fuchsia_async::Time::from_nanos(1000));
        assert_eq!(get_current_timestamp(), Nanoseconds(1000));
    }
}

#[cfg(test)]
pub mod run_all_tasks_until_stalled {
    /// Runs all tasks known to the current executor until stalled.
    ///
    /// This is a useful convenience function to run `fuchsia_async::Task`s where directly polling
    /// them is either inconvenient or not possible (e.g., detached).
    pub fn run_all_tasks_until_stalled(executor: &mut fuchsia_async::TestExecutor) {
        assert!(executor.run_until_stalled(&mut futures::future::pending::<()>()).is_pending());
    }

    mod tests {
        use super::run_all_tasks_until_stalled;
        use std::cell::Cell;
        use std::rc::Rc;

        #[test]
        fn test_run_all_tasks_until_stalled() {
            let mut executor = fuchsia_async::TestExecutor::new_with_fake_time();

            let completed = Rc::new(Cell::new(false));
            let completed_clone = completed.clone();

            let _task = fuchsia_async::Task::local(async move {
                completed_clone.set(true);
            });

            assert_eq!(completed.get(), false);
            run_all_tasks_until_stalled(&mut executor);
            assert_eq!(completed.get(), true);
        }
    }
}
