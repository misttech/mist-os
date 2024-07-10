// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Bindings for the fuchsia driver framework C API
#![deny(unsafe_op_in_unsafe_fn)]

mod arena;
mod dispatcher;
mod fdf_sys;

pub use arena::*;
pub use dispatcher::*;
