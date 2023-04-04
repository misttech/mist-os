// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fuchsia_zircon::sys::zx_thread_state_general_regs_t;

use crate::loader::ThreadStartInfo;

impl From<ThreadStartInfo> for zx_thread_state_general_regs_t {
    fn from(val: ThreadStartInfo) -> Self {
        zx_thread_state_general_regs_t {
            rip: val.entry.ptr() as u64,
            rsp: val.stack.ptr() as u64,
            ..Default::default()
        }
    }
}
