// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#[cfg(not(feature = "starnix_lite"))]
mod framebuffer_server;
mod registry;
#[cfg(not(feature = "starnix_lite"))]
mod remote_binder;

pub use registry::*;

#[cfg(not(feature = "starnix_lite"))]
pub mod android;
#[cfg(not(feature = "starnix_lite"))]
pub mod binder;
pub mod device_mapper;
#[cfg(not(feature = "starnix_lite"))]
pub mod framebuffer;
pub mod kobject;
pub mod mem;
#[cfg(not(feature = "starnix_lite"))]
pub mod perfetto_consumer;
pub mod remote_block_device;
pub mod terminal;
pub mod tun;
