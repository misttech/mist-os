// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! This binary kills itself *and* the process which launched it by panicking with the
//! kill_job_on_panic hook.

#[fuchsia::main]
fn main() {
    kill_job_on_panic::install_hook("patricide");
    panic!("intentional panic to kill parent job");
}
