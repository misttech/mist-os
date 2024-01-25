// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! A crate used to provide the regular `alloc` crate with collections that
//! currently require `std` such as `HashMap` and `HashSet`.

// TODO(https://github.com/rust-lang-nursery/portability-wg/issues/11): remove this module.
// TODO(https://fxbug.dev/42084580): Use the `hashbrown` crate to provide a `HashMap`
// with a default hasher that requires the (to-be-imported) `ahash` crate.
extern crate alloc as rustalloc;

pub use ::rustalloc::*;

pub mod collections {
    pub use ::rustalloc::collections::*;

    pub use ::std::collections::{hash_map, HashMap, HashSet};
}
