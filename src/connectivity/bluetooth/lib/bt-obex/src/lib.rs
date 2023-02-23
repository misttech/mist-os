// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

/// The error type used throughout this library.
mod error;
pub use error::Error as ObexError;

/// Definitions of the OBEX packet Header types.
pub mod header;
