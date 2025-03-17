// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

mod framebuffer_server;
mod registry;
mod remote_binder;

pub use registry::*;

pub mod android;
pub mod binder;
pub mod framebuffer;
pub mod kobject;
pub mod kobject_store;
pub mod mem;
pub mod remote_block_device;
pub mod serial;
pub mod terminal;
