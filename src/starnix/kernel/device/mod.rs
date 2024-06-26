// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#[cfg(not(feature = "starnix_lite"))]
mod binder;
mod device_init;
#[cfg(not(feature = "starnix_lite"))]
mod framebuffer_server;
mod registry;
#[cfg(not(feature = "starnix_lite"))]
mod remote_binder;

#[cfg(not(feature = "starnix_lite"))]
pub use binder::*;
pub use device_init::*;
pub use registry::*;

#[cfg(not(feature = "starnix_lite"))]
pub mod android;
#[cfg(not(feature = "starnix_lite"))]
pub mod ashmem;
pub mod device_mapper;
#[cfg(not(feature = "starnix_lite"))]
pub mod framebuffer;
pub mod kobject;
pub mod loop_device;
pub mod mem;
#[cfg(not(feature = "starnix_lite"))]
pub mod perfetto_consumer;
pub mod remote_block_device;
pub mod sync_fence_registry;
pub mod sync_file;
pub mod terminal;
#[cfg(not(feature = "starnix_lite"))]
pub mod touch_power_policy_device;
pub mod tun;
pub mod zram;
