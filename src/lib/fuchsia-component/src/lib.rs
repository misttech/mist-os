// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Connect to or provide Fuchsia services.

#![deny(missing_docs)]

/// The name of the default instance of a FIDL service.
pub const DEFAULT_SERVICE_INSTANCE: &'static str = "default";

pub mod client;
pub mod directory;
pub mod server;
