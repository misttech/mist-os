// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! This binary spawns a child process in the same job which will install the kill_job_on_panic
//! hook, then sleeps ~forever. If the kill_job_on_panic hook fails to terminate this process' job
//! then the test should fail by timing out.

fn main() {
    std::process::Command::new("/pkg/bin/kill_parent_job_panicker").spawn().unwrap();
    std::thread::sleep(std::time::Duration::MAX);
}
