// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

/// `Mutex` type exercised in the `selinux` crate when built for integration with starnix.
#[cfg(feature = "selinux_starnix")]
pub(super) use starnix_sync::Mutex;

/// `Mutex` type exercised in the `selinux` crate when built for non-fuchsia platforms.
#[cfg(not(feature = "selinux_starnix"))]
pub(super) use parking_lot::Mutex;

/// `RwLock` type exercised in the `selinux` crate when built for integration with starnix.
#[cfg(feature = "selinux_starnix")]
pub(super) use starnix_sync::RwLock;

/// `RwLock` type exercised in the `selinux` crate when built for non-fuchsia platforms.
#[cfg(not(feature = "selinux_starnix"))]
pub(super) use parking_lot::RwLock;
