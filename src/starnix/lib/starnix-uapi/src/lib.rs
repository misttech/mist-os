// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

pub mod arc_key;
pub mod as_any;
pub mod auth;
pub mod device_type;
pub mod elf;
pub mod errors;
pub mod file_mode;
pub mod inotify_mask;
pub mod kcmp;
pub mod math;
pub mod mount_flags;
pub mod open_flags;
pub mod ownership;
pub mod personality;
pub mod range_ext;
pub mod resource_limits;
pub mod seal_flags;
pub mod signals;
pub mod stats;
pub mod syslog;
pub mod time;
pub mod uapi;
pub mod union;
pub mod user_address;
pub mod user_buffer;
pub mod vfs;

#[cfg(target_arch = "aarch64")]
pub mod arm64;

#[cfg(target_arch = "aarch64")]
pub use arm64::*;

#[cfg(target_arch = "x86_64")]
pub mod x64;

#[cfg(target_arch = "x86_64")]
pub use x64::*;

#[cfg(target_arch = "riscv64")]
pub mod riscv64;

#[cfg(target_arch = "riscv64")]
#[allow(unused_imports)]
pub use riscv64::*;

pub use uapi::*;

use fuchsia_zircon as zx;
use once_cell::sync::Lazy;
pub static PAGE_SIZE: Lazy<u64> = Lazy::new(|| zx::system_get_page_size() as u64);
