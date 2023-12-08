// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fuchsia_inspect::Inspector;
use futures::future::BoxFuture;
use once_cell::sync::Lazy;
use starnix_lock::Mutex;
use std::{collections::HashMap, panic::Location};

static NOT_IMPLEMENTED_COUNTS: Lazy<
    Mutex<HashMap<&'static str, HashMap<&'static str, HashMap<Option<u64>, u64>>>>,
> = Lazy::new(|| Mutex::new(HashMap::new()));

#[macro_export]
macro_rules! not_implemented {
    ($message:expr, $context:expr) => {
        $crate::__not_implemented_inner(
            concat!($message, " ", stringify!($context)),
            Some($context.into()),
        );
    };
    ($message:expr) => {
        $crate::__not_implemented_inner($message, None);
    };
}

#[doc(hidden)]
#[inline]
#[track_caller]
pub fn __not_implemented_inner(message: &'static str, context: Option<u64>) {
    let mut counts = NOT_IMPLEMENTED_COUNTS.lock();
    let location = Location::caller();

    let file_counts = counts.entry(location.file()).or_default();
    let message_counts = file_counts.entry(message).or_default();
    let context_count = message_counts.entry(context).or_default();

    // Log the first time we see a particular file/message/context tuple but don't risk spamming.
    if *context_count == 0 {
        if let Some(context) = context {
            crate::log_warn!(tag = "not_implemented", %location, "{message}: 0x{context:x}");
        } else {
            crate::log_warn!(tag = "not_implemented", %location, "{message}");
        }
    }

    *context_count += 1;
}

pub fn not_implemented_lazy_node_callback() -> BoxFuture<'static, Result<Inspector, anyhow::Error>>
{
    Box::pin(async {
        let inspector = Inspector::default();
        for (file, message_counts) in NOT_IMPLEMENTED_COUNTS.lock().iter() {
            inspector.root().atomic_update(|root| {
                root.record_child(*file, |file_node| {
                    for (message, context_counts) in message_counts {
                        // Make a copy of the map so we can mutate it while recording values.
                        let mut context_counts = context_counts.clone();

                        if let Some(no_context_count) = context_counts.remove(&None) {
                            // If the not_implemented callsite doesn't provide any context,
                            // record the count as a property on the file node without an
                            // intermediate.
                            file_node.record_uint(*message, no_context_count);
                        }

                        if !context_counts.is_empty() {
                            file_node.record_child(*message, |message_node| {
                                for (context, count) in context_counts {
                                    if let Some(c) = context {
                                        message_node.record_uint(format!("0x{c:x}"), count);
                                    };
                                }
                            });
                        }
                    }
                });
            });
        }
        Ok(inspector)
    })
}
