// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![warn(clippy::all)]
#![warn(clippy::clone_on_ref_ptr)]
// TODO(https://fxbug.dev/126170): remove after the lint is fixed
#![allow(unknown_lints, clippy::items_after_test_module)]

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
mod utils;

#[cfg(test)]
mod testing;

pub type ImmutableString = Box<str>;
