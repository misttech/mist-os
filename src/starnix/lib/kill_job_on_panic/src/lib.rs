// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fuchsia_runtime::job_default;
use fuchsia_zircon::Task;
use tracing::error;

/// Install a panic hook which will:
///
/// 1. log the provided `message` at ERROR severity
/// 2. invoke any previously installed panic hooks
/// 3. kill the *job* of the current process
///
/// This will result in any other processes in the same job being killed in addition to the caller
/// of this function.
pub fn install_hook(message: &'static str) {
    let prev_panic_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        error!("{message}");
        prev_panic_hook(info);
        job_default().kill().ok();
    }));
}
