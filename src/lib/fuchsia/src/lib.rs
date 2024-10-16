// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Macros for creating Fuchsia components and tests.
//!
//! These macros work on Fuchsia, and also on host with some limitations (that are called out
//! where they exist).

// Features from those macros are expected to be implemented by exactly one function in this
// module. We strive for simple, independent, single purpose functions as building blocks to allow
// dead code elimination to have the very best chance of removing unused code that might be
// otherwise pulled in here.

#![deny(missing_docs)]
pub use fidl_fuchsia_diagnostics::{Interest, Severity};
pub use fuchsia_macro::{main, test};
use std::future::Future;
#[doc(hidden)]
pub use tracing::error;

//
// LOGGING INITIALIZATION
//

/// Options used when initializing logging.
#[derive(Default, Clone)]
pub struct LoggingOptions<'a> {
    /// Tags with which to initialize the logging system. All logs will carry the tags configured
    /// here.
    pub tags: &'a [&'static str],

    /// Allows to configure the minimum severity of the logs being emitted. Logs of lower severity
    /// won't be emitted.
    pub interest: fidl_fuchsia_diagnostics::Interest,

    /// Whether or not logs will be blocking. By default logs are dropped when they can't be
    /// written to the socket. However, when this is set, the log statement will block until the
    /// log can be written to the socket or the socket is closed.
    ///
    /// NOTE: this is ignored on `host`.
    pub blocking: bool,

    /// String to include in logged panic messages.
    pub panic_prefix: &'static str,

    /// True to always log file/line information, false to only log
    /// when severity is ERROR or above.
    pub always_log_file_line: bool,
}

#[cfg(not(target_os = "fuchsia"))]
impl<'a> From<LoggingOptions<'a>> for diagnostics_log::PublishOptions<'a> {
    fn from(logging: LoggingOptions<'a>) -> Self {
        let mut options = diagnostics_log::PublishOptions::default().tags(logging.tags);
        if let Some(severity) = logging.interest.min_severity {
            options = options.minimum_severity(severity);
        }
        options = options.panic_prefix(logging.panic_prefix);
        options
    }
}

#[cfg(target_os = "fuchsia")]
impl<'a> From<LoggingOptions<'a>> for diagnostics_log::PublishOptions<'a> {
    fn from(logging: LoggingOptions<'a>) -> Self {
        let mut options = diagnostics_log::PublishOptions::default().tags(logging.tags);
        options = options.blocking(logging.blocking);
        if let Some(severity) = logging.interest.min_severity {
            options = options.minimum_severity(severity);
        }
        if logging.always_log_file_line {
            options = options.always_log_file_line();
        }
        options = options.panic_prefix(logging.panic_prefix);
        options
    }
}

/// Initialize logging
#[doc(hidden)]
pub fn init_logging_for_component_with_executor<'a, R>(
    func: impl FnOnce() -> R + 'a,
    logging: LoggingOptions<'a>,
) -> impl FnOnce() -> R + 'a {
    move || {
        diagnostics_log::initialize(logging.into()).expect("initialize_logging");
        func()
    }
}

/// Initialize logging
#[doc(hidden)]
pub fn init_logging_for_component_with_threads<'a, R>(
    func: impl FnOnce() -> R + 'a,
    logging: LoggingOptions<'a>,
) -> impl FnOnce() -> R + 'a {
    move || {
        let _guard = init_logging_with_threads(logging);
        func()
    }
}

/// Initialize logging
#[doc(hidden)]
pub fn init_logging_for_test_with_executor<'a, R>(
    func: impl Fn(usize) -> R + 'a,
    logging: LoggingOptions<'a>,
) -> impl Fn(usize) -> R + 'a {
    move |n| {
        diagnostics_log::initialize(logging.clone().into()).expect("initalize logging");
        func(n)
    }
}

/// Initialize logging
#[doc(hidden)]
pub fn init_logging_for_test_with_threads<'a, R>(
    func: impl Fn(usize) -> R + 'a,
    logging: LoggingOptions<'a>,
) -> impl Fn(usize) -> R + 'a {
    move |n| {
        let _guard = init_logging_with_threads(logging.clone());
        func(n)
    }
}

/// Initializes logging on a background thread, returning a guard which cancels interest listening
/// when dropped.
#[cfg(target_os = "fuchsia")]
fn init_logging_with_threads(logging: LoggingOptions<'_>) -> impl Drop {
    diagnostics_log::initialize_sync(logging.into())
}

#[cfg(not(target_os = "fuchsia"))]
fn init_logging_with_threads(logging: LoggingOptions<'_>) {
    diagnostics_log::initialize(logging.into()).expect("initialize logging");
}

//
// MAIN FUNCTION WRAPPERS
//

/// Run a non-async main function.
#[doc(hidden)]
pub fn main_not_async<F, R>(f: F) -> R
where
    F: FnOnce() -> R,
{
    f()
}

/// Run an async main function with a single threaded executor.
#[doc(hidden)]
pub fn main_singlethreaded<F, Fut, R>(f: F) -> R
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = R> + 'static,
{
    fuchsia_async::LocalExecutor::new().run_singlethreaded(f())
}

/// Run an async main function with a multi threaded executor (containing `num_threads`).
#[doc(hidden)]
pub fn main_multithreaded<F, Fut, R>(f: F, num_threads: usize) -> R
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = R> + Send + 'static,
    R: Send + 'static,
{
    fuchsia_async::SendExecutor::new(num_threads).run(f())
}

//
// TEST FUNCTION WRAPPERS
//

/// Run a non-async test function.
#[doc(hidden)]
pub fn test_not_async<F, R>(f: F) -> R
where
    F: FnOnce(usize) -> R,
{
    f(0)
}

/// Run an async test function with a single threaded executor.
#[doc(hidden)]
pub fn test_singlethreaded<F, Fut, R>(f: F) -> R
where
    F: Fn(usize) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = R> + 'static,
    R: fuchsia_async::test_support::TestResult,
{
    fuchsia_async::test_support::run_singlethreaded_test(f)
}

/// Run an async test function with a multi threaded executor (containing `num_threads`).
#[doc(hidden)]
pub fn test_multithreaded<F, Fut, R>(f: F, num_threads: usize) -> R
where
    F: Fn(usize) -> Fut + Send + 'static,
    Fut: Future<Output = R> + Send + 'static,
    R: fuchsia_async::test_support::MultithreadedTestResult,
{
    fuchsia_async::test_support::run_test(f, num_threads)
}

/// Run an async test function until it stalls. The executor will also use fake time.
#[doc(hidden)]
#[cfg(target_os = "fuchsia")]
pub fn test_until_stalled<F, Fut, R>(f: F) -> R
where
    F: 'static + Fn(usize) -> Fut,
    Fut: 'static + Future<Output = R>,
    R: fuchsia_async::test_support::TestResult,
{
    fuchsia_async::test_support::run_until_stalled_test(
        &mut fuchsia_async::TestExecutor::new_with_fake_time(),
        f,
    )
}

//
// FUNCTION ARGUMENT ADAPTERS
//

/// Take a main function `f` that takes an argument and return a function that takes none but calls
/// `f` with the arguments parsed via argh.
#[doc(hidden)]
pub fn adapt_to_parse_arguments<A, R>(f: impl FnOnce(A) -> R) -> impl FnOnce() -> R
where
    A: argh::TopLevelCommand,
{
    move || f(argh::from_env())
}

/// Take a test function `f` that takes no parameters and return a function that takes the run
/// number as required by our test runners.
#[doc(hidden)]
pub fn adapt_to_take_test_run_number<R>(f: impl Fn() -> R) -> impl Fn(usize) -> R {
    move |_| f()
}
