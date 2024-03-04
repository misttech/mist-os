// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Base type definitions for netstack3 core.
//!
//! This crate contains definitions common to the other netstack3 core crates
//! and is the base dependency for most of them.

mod context;

pub use context::ContextPair;
