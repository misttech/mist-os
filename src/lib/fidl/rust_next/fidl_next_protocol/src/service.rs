// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use core::error::Error;

use crate::Transport;

/// An instance of a FIDL service.
pub trait ServiceInstance {
    /// The error type for the instance.
    type Error: Error + Send + Sync + 'static;

    /// The transport type created by connecting to a member.
    type Transport: Transport;

    /// Attempts to connect the given member.
    fn connect(&mut self, member: &str) -> Result<Self::Transport, Self::Error>;
}
