// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! File nodes backed by a VMO. These are useful for cases when individual read/write operation
//! actions need to be visible across all the connections to the same file.

// TODO(fxbug.dev/99448): The VmoFile interface now supports both synchronous and asynchronous Vmo
// construction. Move [`asynchronous`] module contents here after updating out of tree references.
pub mod asynchronous;

pub use asynchronous::{
    read_only, read_only_const, read_only_static, read_write, simple_init_vmo_with_capacity,
    VmoFile,
};
