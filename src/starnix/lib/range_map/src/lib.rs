// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

pub mod btree;
pub mod classic;

#[cfg(not(feature = "use_cowmap"))]
pub use classic::RangeMap;

pub use btree::Gap;
#[cfg(feature = "use_cowmap")]
pub use btree::RangeMap2 as RangeMap;
