// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#[cfg(feature = "tracing")]
mod fuchsia;
#[cfg(feature = "tracing")]
pub use fuchsia::*;

#[cfg(not(feature = "tracing"))]
mod portable;
#[cfg(not(feature = "tracing"))]
pub use portable::*;

pub use fxfs_trace_macros::*;
