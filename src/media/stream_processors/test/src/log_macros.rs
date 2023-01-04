// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#[macro_export]
macro_rules! vlog {
    ($v:expr, $($arg:tt)+) => (
        ::fuchsia_syslog::fx_vlog!(tag: "stream_processor_tests", $v, $($arg)+)
    )
}

#[macro_export]
macro_rules! info {
    ($($arg:tt)+) => (
        ::fuchsia_syslog::fx_log_info!(tag: "stream_processor_tests", $($arg)+)
    )
}
