// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

/// An internal framework error.
#[repr(i32)]
pub enum FrameworkError {
    /// The protocol method was not recognized by the receiver.
    UnknownMethod = -2,
}
