// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Miscellaneous test utilities used in netstack3.
//!
//! # Note to developers
//!
//! Please refrain from adding types to this module, keep this only to
//! freestanding functions. If you require a new type, create a module for it.

use alloc::vec::Vec;
use core::fmt::Debug;

/// Install a logger for tests.
///
/// Call this method at the beginning of the test for which logging is desired.
/// This function sets global program state, so all tests that run after this
/// function is called will use the logger.
pub fn set_logger_for_test() {
    use tracing::Subscriber;
    use tracing_subscriber::{
        fmt::{
            format::{self, FormatEvent, FormatFields},
            FmtContext,
        },
        registry::LookupSpan,
    };

    struct SimpleFormatter;

    impl<S, N> FormatEvent<S, N> for SimpleFormatter
    where
        S: Subscriber + for<'a> LookupSpan<'a>,
        N: for<'a> FormatFields<'a> + 'static,
    {
        fn format_event(
            &self,
            ctx: &FmtContext<'_, S, N>,
            mut writer: format::Writer<'_>,
            event: &tracing::Event<'_>,
        ) -> std::fmt::Result {
            ctx.format_fields(writer.by_ref(), event)?;
            writeln!(writer)
        }
    }

    tracing::subscriber::set_global_default(
        tracing_subscriber::fmt()
            .event_format(SimpleFormatter)
            .with_max_level(tracing::Level::TRACE)
            .with_test_writer()
            .finish(),
    )
    .unwrap_or({
        // Ignore errors caused by some other test invocation having already set
        // the global default subscriber.
    })
}

/// Asserts that an iterable object produces zero items.
///
/// `assert_empty` drains `into_iter.into_iter()` and asserts that zero
/// items are produced. It panics with a message which includes the produced
/// items if this assertion fails.
#[track_caller]
pub fn assert_empty<I: IntoIterator>(into_iter: I)
where
    I::Item: Debug,
{
    // NOTE: Collecting into a `Vec` is cheap in the happy path because
    // zero-capacity vectors are guaranteed not to allocate.
    let vec = into_iter.into_iter().collect::<Vec<_>>();
    assert!(vec.is_empty(), "vec={vec:?}");
}
