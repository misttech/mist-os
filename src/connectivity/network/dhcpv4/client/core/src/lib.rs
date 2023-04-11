// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![deny(missing_docs)]
//! Defines the platform-agnostic "core" of a DHCP client, including a state
//! machine and abstractions for sockets and time.

/// Defines the client core state machine.
pub mod client;

/// Defines abstractions for platform dependencies such as sockets and time.
pub mod deps;

mod parse;
