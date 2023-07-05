// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![deny(missing_docs)]

//! Library that holds a set of packages and ensures a consistent iteration order.

mod package_set;

pub use package_set::{PackageEntry, PackageSet};
