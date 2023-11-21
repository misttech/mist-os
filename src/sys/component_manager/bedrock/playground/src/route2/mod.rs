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
//! Every capability is not only lazy, but also changes with each unresolve/resolve.
//!
//! - The output of capability routing today is not really the underlying capabilities,
//! but a route (factory) to get a capability.
//!
//! - On unresolve/resolve, it's the route that changes - e.g. it can break if the source
//! component is destroyed, get routed to a different source, or apply a new
//! transformation like if the decl specifies a different subdir or availability.
//!
//! - The capability that travels through the route can change as a result of the new
//! route. e.g. an optional capability may be None if the route now points to void.
//!
//! If you got an output dictionary from a component, and that component got unresolved
//! and re-resolved, what happens to the output dictionary? The router design avoids
//! having to deal with this question, as the consumer never gets hold of the output
//! directory directly, but instead interacts with it via the router.
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
//! ### Providing capabilities to the program
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
//! ### Exposing capabilities from the program
//!
//! The output the program will be represented by a [`Dict`] of capabilities.
//! Each key corresponds to the name of the capability (e.g. `fuchsia.echo.Echo`). Each value
//! will be a capability that will open an appropriate path within the outgoing directory of
//! the program, itself represented by an [`Open`].
//!
//! A router is obtained from this [`Dict`].
//!
//! The parent will then set up routes from this program the same way as it does for components.

pub mod component;
pub mod program;
