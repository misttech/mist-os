// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
#![warn(missing_docs, unsafe_op_in_unsafe_fn)]
//! A transport-agnostic library for implementing a vsock bridge over a usb bulk device.

mod packet;

pub use packet::*;
