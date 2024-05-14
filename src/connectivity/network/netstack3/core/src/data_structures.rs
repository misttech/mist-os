// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Common data structures.

pub(crate) mod ref_counted_hash_map {
    pub(crate) use netstack3_base::ref_counted_hash_map::{
        InsertResult, RefCountedHashMap, RefCountedHashSet, RemoveResult,
    };
}

pub(crate) mod socketmap;
pub(crate) mod token_bucket {
    pub(crate) use netstack3_base::TokenBucket;
}
