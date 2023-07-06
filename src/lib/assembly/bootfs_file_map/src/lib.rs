// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![deny(missing_docs)]

//! Map that ensures that all bootfs files have unique destinations and hashes.

mod bootfs_file_map;

pub use bootfs_file_map::BootfsFileMap;
