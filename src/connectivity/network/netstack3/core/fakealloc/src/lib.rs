// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// TODO(https://github.com/rust-lang-nursery/portability-wg/issues/11): remove this module.

extern crate alloc as rustalloc;

pub use ::rustalloc::*;

pub mod collections {
    pub use ::rustalloc::collections::*;

    pub use ::std::collections::{hash_map, HashMap, HashSet};
}
