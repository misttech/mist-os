// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![deny(clippy::correctness)]
#![warn(clippy::suspicious)]
#![warn(clippy::complexity)]
// TODO(fxbug.dev/123528): Remove unknown_lints after toolchain rolls.
#![allow(unknown_lints)]
// TODO(fxbug.dev/123778): Fix redundant async blocks.
#![allow(clippy::redundant_async_block)]
// The complexity of a separate struct doesn't seem universally better than having many arguments
#![allow(clippy::too_many_arguments)]
// Turn on a couple selected lints from clippy::style
#![warn(clippy::collapsible_else_if)]
#![warn(clippy::from_over_into)]
#![warn(clippy::len_zero)]
#![warn(clippy::manual_map)]
#![warn(clippy::needless_borrow)]
#![warn(clippy::wrong_self_convention)]

pub mod access_point;
pub mod client;
pub mod config_management;
pub mod legacy;
pub mod mode_management;
pub mod regulatory_manager;
pub mod telemetry;
#[cfg(test)]
mod tests;
pub mod util;
