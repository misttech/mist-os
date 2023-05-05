// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

pub static DEFAULT_INIT_ARM64: &[u8] = include_bytes!(env!("DEFAULT_INIT_ARM64_PATH"));
pub static DEFAULT_INIT_X64: &[u8] = include_bytes!(env!("DEFAULT_INIT_X64_PATH"));
