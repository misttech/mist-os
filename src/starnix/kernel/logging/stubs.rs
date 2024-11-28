// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fuchsia_inspect::Inspector;
use futures::future::BoxFuture;
use once_cell::sync::Lazy;
use starnix_sync::Mutex;
use std::collections::HashMap;
use std::panic::Location;

static STUB_COUNTS: Lazy<Mutex<HashMap<Invocation, Counts>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

#[macro_export]
macro_rules! track_stub {
    (TODO($bug_url:literal), $message:expr, $flags:expr $(,)?) => {{
        $crate::__track_stub_inner(
            $crate::bug_ref!($bug_url),
            $message,
            Some($flags.into()),
            std::panic::Location::caller(),
        );
    }};
    (TODO($bug_url:literal), $message:expr $(,)?) => {{
        $crate::__track_stub_inner(
            $crate::bug_ref!($bug_url),
            $message,
            None,
            std::panic::Location::caller(),
        );
    }};
}

#[derive(Debug, Clone, Copy, Eq, Hash, Ord, PartialEq, PartialOrd)]
struct Invocation {
    location: &'static Location<'static>,
    message: &'static str,
    bug: BugRef,
}

#[derive(Default)]
struct Counts {
    by_flags: HashMap<Option<u64>, u64>,
}

#[doc(hidden)]
#[inline]
pub fn __track_stub_inner(
    bug: BugRef,
    message: &'static str,
    flags: Option<u64>,
    location: &'static Location<'static>,
) -> u64 {
    let mut counts = STUB_COUNTS.lock();
    let message_counts = counts.entry(Invocation { location, message, bug }).or_default();
    let context_count = message_counts.by_flags.entry(flags).or_default();

    // Log the first time we see a particular file/message/context tuple but don't risk spamming.
    if *context_count == 0 {
        match flags {
            Some(flags) => {
                crate::log_warn!(tag = "track_stub", %location, "{bug} {message}: 0x{flags:x}");
            }
            None => {
                crate::log_warn!(tag = "track_stub", %location, "{bug} {message}");
            }
        }
    }

    *context_count += 1;
    *context_count
}

pub fn track_stub_lazy_node_callback() -> BoxFuture<'static, Result<Inspector, anyhow::Error>> {
    Box::pin(async {
        let inspector = Inspector::default();
        for (Invocation { location, message, bug }, context_counts) in STUB_COUNTS.lock().iter() {
            inspector.root().atomic_update(|root| {
                root.record_child(*message, |message_node| {
                    message_node.record_string("file", location.file());
                    message_node.record_uint("line", location.line().into());
                    message_node.record_string("bug", bug.to_string());

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

#[macro_export]
macro_rules! bug_ref {
    ($bug_url:literal) => {{
        // Assign the value to a const to ensure we get compile-time validation of the URL.
        const __REF: $crate::BugRef = match $crate::BugRef::from_str($bug_url) {
            Some(b) => b,
            None => panic!("bug references must have the form `https://fxbug.dev/123456789`"),
        };
        __REF
    }};
}

#[derive(Debug, Clone, Copy, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct BugRef {
    number: u64,
}

impl BugRef {
    #[doc(hidden)] // use bug_ref!() instead
    pub const fn from_str(url: &'static str) -> Option<Self> {
        let expected_prefix = b"https://fxbug.dev/";
        let url = str::as_bytes(url);

        if url.len() < expected_prefix.len() {
            return None;
        }
        let (scheme_and_domain, number_str) = url.split_at(expected_prefix.len());
        if number_str.is_empty() {
            return None;
        }

        // The standard library doesn't seem to have a const string or slice equality function.
        {
            let mut i = 0;
            while i < scheme_and_domain.len() {
                if scheme_and_domain[i] != expected_prefix[i] {
                    return None;
                }
                i += 1;
            }
        }

        // The standard library doesn't seem to have a const base 10 string parser.
        let mut number = 0;
        {
            let mut i = 0;
            while i < number_str.len() {
                number *= 10;
                number += match number_str[i] {
                    b'0' => 0,
                    b'1' => 1,
                    b'2' => 2,
                    b'3' => 3,
                    b'4' => 4,
                    b'5' => 5,
                    b'6' => 6,
                    b'7' => 7,
                    b'8' => 8,
                    b'9' => 9,
                    _ => return None,
                };
                i += 1;
            }
        }

        Some(Self { number })
    }
}

impl std::fmt::Display for BugRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "https://fxbug.dev/{}", self.number)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_url_parses() {
        assert_eq!(BugRef::from_str("https://fxbug.dev/1234567890").unwrap().number, 1234567890);
    }

    #[test]
    fn missing_prefix_fails() {
        assert_eq!(BugRef::from_str("1234567890"), None);
    }

    #[test]
    fn missing_number_fails() {
        assert_eq!(BugRef::from_str("https://fxbug.dev/"), None);
    }

    #[test]
    fn short_prefixes_fail() {
        assert_eq!(BugRef::from_str("b/1234567890"), None);
        assert_eq!(BugRef::from_str("fxb/1234567890"), None);
        assert_eq!(BugRef::from_str("fxbug.dev/1234567890"), None);
    }
}
