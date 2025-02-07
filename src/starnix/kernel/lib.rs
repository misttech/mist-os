// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![recursion_limit = "512"]

use tracing_mutex as _;

use {async_utils as _, fidl_fuchsia_power_suspend as _};
pub mod arch;
pub mod bpf;
pub mod container_namespace;
pub mod device;
pub mod dynamic_thread_spawner;
pub mod execution;
pub mod fs;
pub mod loader;
pub mod memory_attribution;
pub mod mm;
pub mod mutable_state;
pub mod on_shutdown;
pub mod power;
pub mod security;
pub mod signals;
pub mod syscalls;
pub mod task;
pub mod time;
pub mod vdso;
pub mod vfs;

pub mod testing;

// This allows macros to use paths within this crate
// by referring to them by the external crate name.
extern crate self as starnix_core;
