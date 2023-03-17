// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![warn(clippy::all)]
#![warn(clippy::clone_on_ref_ptr)]
// TODO(fxbug.dev/123528): Remove unknown_lints after toolchain rolls.
#![allow(unknown_lints)]
// TODO(fxbug.dev/123778): Fix redundant async blocks.
#![allow(clippy::redundant_async_block)]

mod accessor;
pub mod archivist;
pub mod component_lifecycle;
mod configs;
pub mod constants;
mod diagnostics;
mod error;
pub mod events;
pub mod formatter;
mod identity;
mod inspect;
pub mod logs;
mod moniker_rewriter;
mod pipeline;
mod trie;
mod utils;

#[cfg(test)]
mod testing;

pub type ImmutableString = Box<str>;
