// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

pub const VDSO_SIGRETURN_NAME: Option<&'static str> = Some("__kernel_rt_sigreturn");

extern "C" {
    // This method is implemented by //src/starnix/kernel/vdso/get_raw_ticks_arm.cc.
    fn get_raw_ticks() -> u64;
}

pub fn raw_ticks() -> u64 {
    unsafe { get_raw_ticks() }
}
