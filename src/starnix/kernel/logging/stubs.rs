// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fuchsia_inspect::Inspector;
use futures::future::BoxFuture;
use once_cell::sync::Lazy;
use starnix_sync::Mutex;
use std::{collections::HashMap, panic::Location};

static STUB_COUNTS: Lazy<Mutex<HashMap<Invocation, Counts>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

#[macro_export]
macro_rules! track_stub {
    (TODO($bug_url:literal), $message:expr, $flags:expr $(,)?) => {{
        $crate::__track_stub_inner(
            Some($bug_url),
            $message,
            Some($flags.into()),
            std::panic::Location::caller(),
        );
    }};
    (TODO($bug_url:literal), $message:expr $(,)?) => {{
        $crate::__track_stub_inner(Some($bug_url), $message, None, std::panic::Location::caller());
    }};
    ($message:expr, $flags:expr $(,)?) => {{
        $crate::__track_stub_inner(
            None,
            $message,
            Some($flags.into()),
            std::panic::Location::caller(),
        );
    }};
    ($message:expr $(,)?) => {{
        $crate::__track_stub_inner(None, $message, None, std::panic::Location::caller());
    }};
}

#[derive(Debug, Clone, Copy, Eq, Hash, Ord, PartialEq, PartialOrd)]
struct Invocation {
    location: &'static Location<'static>,
    message: &'static str,
    bug_url: Option<&'static str>,
}

#[derive(Default)]
struct Counts {
    by_flags: HashMap<Option<u64>, u64>,
}

#[doc(hidden)]
#[inline]
pub fn __track_stub_inner(
    bug_url: Option<&'static str>,
    message: &'static str,
    flags: Option<u64>,
    location: &'static Location<'static>,
) {
    let mut counts = STUB_COUNTS.lock();
    let message_counts = counts.entry(Invocation { location, message, bug_url }).or_default();
    let context_count = message_counts.by_flags.entry(flags).or_default();

    // Log the first time we see a particular file/message/context tuple but don't risk spamming.
    if *context_count == 0 {
        match (bug_url, flags) {
            (Some(bug_url), Some(flags)) => {
                crate::log_warn!(tag = "track_stub", %location, "{bug_url} {message}: 0x{flags:x}");
            }
            (Some(bug_url), None) => {
                crate::log_warn!(tag = "track_stub", %location, "{bug_url} {message}");
            }
            (None, Some(flags)) => {
                crate::log_warn!(tag = "track_stub", %location, "{message}: 0x{flags:x}");
            }
            (None, None) => {
                crate::log_warn!(tag = "track_stub", %location, "{message}");
            }
        }
    }

    *context_count += 1;
}

pub fn track_stub_lazy_node_callback() -> BoxFuture<'static, Result<Inspector, anyhow::Error>> {
    Box::pin(async {
        let inspector = Inspector::default();
        for (Invocation { location, message, bug_url }, context_counts) in STUB_COUNTS.lock().iter()
        {
            inspector.root().atomic_update(|root| {
                root.record_child(*message, |message_node| {
                    message_node.record_string("file", location.file());
                    message_node.record_uint("line", location.line().into());
                    if let Some(bug_url) = bug_url {
                        message_node.record_string("bug", bug_url);
                    }

                    // Make a copy of the map so we can mutate it while recording values.
                    let mut context_counts = context_counts.by_flags.clone();

                    if let Some(no_context_count) = context_counts.remove(&None) {
                        // If the track_stub callsite doesn't provide any context,
                        // record the count as a property on the node without an intermediate.
                        message_node.record_uint("count", no_context_count);
                    }

                    if !context_counts.is_empty() {
                        message_node.record_child("counts", |counts_node| {
                            for (context, count) in context_counts {
                                if let Some(c) = context {
                                    counts_node.record_uint(format!("0x{c:x}"), count);
                                }
                            }
                        });
                    }
                });
            });
        }
        Ok(inspector)
    })
}
