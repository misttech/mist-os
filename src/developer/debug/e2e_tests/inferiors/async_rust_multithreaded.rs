// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fuchsia_async as fasync;

#[fasync::run(2)]
async fn main() {
    let _task_a = fasync::Task::spawn(async {});
    let _task_b = fasync::Task::spawn(async {});
    fasync::Timer::new(std::time::Duration::from_secs(1)).await;
    panic!();
}
