// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fuchsia_zircon as zx;

mod beacon;

/// Common message-related definitions.
pub mod action_fuse;
pub mod base;
pub mod delegate;
pub mod message_client;
pub mod message_hub;
pub mod messenger;
pub mod receptor;

/// Representation of time used for logging.
pub type Timestamp = zx::Time;
