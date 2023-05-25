// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! A networking stack.
#![deny(clippy::unused_async)]
#![deny(missing_docs, unreachable_patterns, unused)]
#![recursion_limit = "256"]

#[cfg(feature = "instrumented")]
extern crate netstack3_core_instrumented as netstack3_core;

mod bindings;

use bindings::NetstackSeed;

#[fuchsia::main(logging_minimum_severity = "debug")]
fn main() -> Result<(), anyhow::Error> {
    // TOOD(https://fxbug.dev/125388): Support running with multiple threads.
    // This is currently blocked on fixing race conditions when concurrent
    // operations are allowed.
    let mut executor = fuchsia_async::SendExecutor::new(1 /* num_threads */);

    fuchsia_trace_provider::trace_provider_create_with_fdio();

    let seed = NetstackSeed::default();
    executor.run(seed.serve())
}
