// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

mod abstract_socket_namespace;
mod iptables;
mod kernel;
mod pid_table;
mod process_group;
mod session;
#[allow(clippy::module_inception)]
mod task;
mod thread_group;
mod timers;
mod uts_namespace;
mod waiter;

#[cfg(target_arch = "x86_64")]
mod task_x64;

#[cfg(target_arch = "aarch64")]
mod task_arm64;

pub use abstract_socket_namespace::*;
pub use iptables::*;
pub use kernel::*;
pub use pid_table::*;
pub use process_group::*;
pub use session::*;
pub use task::*;
pub use thread_group::*;
pub use timers::*;
pub use uts_namespace::*;
pub use waiter::*;

pub mod syscalls;

#[cfg(target_arch = "x86_64")]
pub use task_x64::*;

#[cfg(target_arch = "aarch64")]
pub use task_arm64::*;
