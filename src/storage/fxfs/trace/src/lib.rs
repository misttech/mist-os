// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::future::Future;

#[cfg(feature = "tracing")]
mod fuchsia;
#[cfg(feature = "tracing")]
pub use fuchsia::*;

#[cfg(not(feature = "tracing"))]
mod portable;
#[cfg(not(feature = "tracing"))]
pub use portable::*;

pub use cstr::cstr;
pub use fxfs_trace_macros::*;

impl<T: Future + Sized> FxfsTraceFutureExt for T {}
