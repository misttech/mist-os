// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![warn(clippy::all)]
#![allow(clippy::result_large_err)]
#![allow(clippy::let_unit_value)]
// TODO(fxbug.dev/123528): Remove unknown_lints after toolchain rolls.
#![allow(unknown_lints)]
// TODO(fxbug.dev/123778): Fix redundant async blocks.
#![allow(clippy::redundant_async_block)]

pub mod manager;
pub mod package_manifest_watcher;
pub mod range;
pub mod repo_builder;
pub mod repo_client;
pub mod repo_keys;
pub mod repository;
pub mod resolve;
pub mod resource;
pub mod server;
pub mod test_utils;

mod async_spooled;
mod util;
