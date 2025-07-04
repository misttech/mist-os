// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
pub mod kernel;

use std::ffi::CStr;

pub const CATEGORY_MEMORY_CAPTURE: &'static CStr = c"memory:capture";
