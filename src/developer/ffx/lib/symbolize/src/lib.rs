// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![warn(missing_docs)]

//! Library for symbolizing addresses from Fuchsia programs.

mod parse;
mod resolver;
mod symbolizer;

pub use parse::*;
pub use resolver::*;
pub use symbolizer::*;
