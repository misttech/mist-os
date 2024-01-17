// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fuchsia_inspect::Inspector;
use futures::future::BoxFuture;
use once_cell::sync::Lazy;
use starnix_sync::Mutex;
use std::{collections::HashMap, panic::Location};

static NOT_IMPLEMENTED_COUNTS: Lazy<Mutex<HashMap<Invocation, Counts>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

#[macro_export]
macro_rules! not_implemented {
    (fxb@$bug_number:literal, $message:expr, $context:expr) => {
        $crate::__not_implemented_inner(Some($bug_number), $message, Some($context.into()));
    };
    (fxb@$bug_number:literal, $message:expr) => {
        $crate::__not_implemented_inner(Some($bug_number), $message, None);
    };
    ($message:expr, $context:expr) => {
        $crate::__not_implemented_inner(None, $message, Some($context.into()));
    };
    ($message:expr) => {
        $crate::__not_implemented_inner(None, $message, None);
    };
}

#[derive(Debug, Clone, Copy, Eq, Hash, Ord, PartialEq, PartialOrd)]
struct Invocation {
    location: &'static Location<'static>,
    message: &'static str,
    bug_number: Option<u64>,
}

#[derive(Default)]
struct Counts {
    by_context: HashMap<Option<u64>, u64>,
}

#[doc(hidden)]
#[inline]
#[track_caller]
pub fn __not_implemented_inner(
    bug_number: Option<u64>,
    message: &'static str,
    context: Option<u64>,
) {
    let mut counts = NOT_IMPLEMENTED_COUNTS.lock();
    let location = Location::caller();
    let message_counts = counts.entry(Invocation { location, message, bug_number }).or_default();
    let context_count = message_counts.by_context.entry(context).or_default();

    // Log the first time we see a particular file/message/context tuple but don't risk spamming.
    if *context_count == 0 {
        match (bug_number, context) {
            (Some(bug), Some(context)) => {
                crate::log_warn!(tag = "not_implemented", %location, "https://fxbug.dev/{bug} {message}: 0x{context:x}");
            }
            (Some(bug), None) => {
                crate::log_warn!(tag = "not_implemented", %location, "https://fxbug.dev/{bug} {message}");
            }
            (None, Some(context)) => {
                crate::log_warn!(tag = "not_implemented", %location, "{message}: 0x{context:x}");
            }
            (None, None) => {
                crate::log_warn!(tag = "not_implemented", %location, "{message}");
            }
        }
    }

    *context_count += 1;
}

pub fn not_implemented_lazy_node_callback() -> BoxFuture<'static, Result<Inspector, anyhow::Error>>
{
    Box::pin(async {
        let inspector = Inspector::default();
        for (Invocation { location, message, bug_number }, context_counts) in
            NOT_IMPLEMENTED_COUNTS.lock().iter()
        {
            inspector.root().atomic_update(|root| {
                root.record_child(*message, |message_node| {
                    message_node.record_string("file", location.file());
                    message_node.record_uint("line", location.line().into());
                    if let Some(bug) = bug_number {
                        message_node.record_string("bug", format!("https://fxbug.dev/{bug}"));
                    }

                    // Make a copy of the map so we can mutate it while recording values.
                    let mut context_counts = context_counts.by_context.clone();

                    if let Some(no_context_count) = context_counts.remove(&None) {
                        // If the not_implemented callsite doesn't provide any context,
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
