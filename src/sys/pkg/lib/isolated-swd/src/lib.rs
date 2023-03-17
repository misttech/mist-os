// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![warn(clippy::all)]
#![allow(clippy::let_unit_value)]
#![allow(clippy::too_many_arguments)]
// TODO(fxbug.dev/123528): Remove unknown_lints after toolchain rolls.
#![allow(unknown_lints)]
// TODO(fxbug.dev/123778): Fix redundant async blocks.
#![allow(clippy::redundant_async_block)]

pub mod cache;
pub mod omaha;
pub mod resolver;
pub mod updater;
