// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

pub const VDSO_SIGRETURN_NAME: Option<&'static str> = Some("__kernel_rt_sigreturn");

pub fn calculate_ticks_offset() -> i64 {
    // Returns 0 as calculate_ticks_offset is currently unimplemented in this architecture.
    // This isn't a problem as vvar_data is currently unused in this architecture.
    // TODO(fxb/129367): Implement gettimeofday() in arm64.
    0
}
