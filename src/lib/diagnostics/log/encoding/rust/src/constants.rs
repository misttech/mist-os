// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

pub(crate) const PID: &str = "pid";
pub(crate) const TID: &str = "tid";
pub(crate) const TAG: &str = "tag";
pub(crate) const NUM_DROPPED: &str = "num_dropped";
pub(crate) const MESSAGE: &str = "message";
pub(crate) const FILE: &str = "file";
pub(crate) const LINE: &str = "line";

/// Size of the FXT header.
pub const FXT_HEADER_SIZE: usize = 8;

/// The tracing format supports many types of records, we're sneaking in as a log message.
pub const TRACING_FORMAT_LOG_RECORD_TYPE: u8 = 9;
