// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Implementation of Network Management File System (nmfs).
//!
//! Nmfs is a Unix-compatible filesystem that receives properties
//! of installed networks and communicates property updates to the
//! Fuchsia Network Policy socket proxy.

mod fs;
mod manager;

pub use fs::*;
pub use manager::*;
