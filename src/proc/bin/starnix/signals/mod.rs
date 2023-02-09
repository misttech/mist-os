// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

pub mod signal_handling;
pub mod signalfd;
pub mod syscalls;
pub mod types;

pub use signal_handling::*;
pub use types::*;

#[cfg(target_arch = "aarch64")]
pub mod signal_handling_arm64;

#[cfg(target_arch = "x86_64")]
pub mod signal_handling_x64;

#[cfg(target_arch = "aarch64")]
pub use signal_handling_arm64::*;

#[cfg(target_arch = "x86_64")]
pub use signal_handling_x64::*;
