// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! This module contains yet another implementation of routing. The primary distinction
//! is that it is closer to how routing currently behaves in the framework, as opposed
//! to how we think routing should behave in terms of some idealized invariants.
//! As a result, this should be easier to implement and generalize to all CFv2
//! capability types than passing around or reclaiming receivers.
//!
//! ## Why router?
//!
//! - Every capability is not only lazy, but also changes with each unresolve/resolve.
//! - If you got an output dictionary from a component, and that component got unresolved
//!   and re-resolved, what happens to the output dictionary? The router design avoids
//!   having to deal with this question.
//!
//! ## Data flow
//!
//! ```rust
//! fn program(input: Dict) -> Router;
//! fn component(input: Dict) -> Router;
//! fn resolve(input: Dict) -> Dict;
//! ```
//!
//! ## A hypothetical interfacing with CFv2 runners
//!
//! When starting the program, we need to do these:
//!
//! - For handles that are already in a dictionary, just get the handle:
//!
//! ```rust
//! let capability = dict.get("stdin")?;
//! let handle = capability.to_zx_handle();
//! ```
//!
//! - For handles that are routed on-demand, asynchronously obtain the one-shot:
//!
//! ```rust
//! let result = route(router, request).await;
//! let capability = result?;   // Can add routing failure logging here.
//! let handle = capability.to_zx_handle();
//! ```
//!
//! - For pipelined multi-shot, transform the router to an Open, so that a pipelined
//! request is sent when the program opens that path. We can then add these Open
//! capabilities as namespaced objects.
//!
//! ```rust
//! let open_fn = |open_args| {
//!   let result = route(router, request).await;
//!   let capability = result?;   // Can add routing failure logging here.
//!   let open = capability.try_into::<Open>()?;   // Void source can fail and get logged here.
//!   open(open_args);
//! };
//! Open::new(open_fn)
//! ```
//!

pub mod component;
pub mod program;
