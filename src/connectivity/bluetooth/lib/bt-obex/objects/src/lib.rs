// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

/// The error type used throughout this library.
mod error;
pub mod folder_listing;
pub use error::Error as ObexObjectError;

/// A builder type can build a Document Type object into raw bytes of encoded data.
pub trait Builder {
    type Error;

    // Returns the MIME type of the raw bytes of data.
    fn mime_type(&self) -> String;

    /// Builds self into raw bytes of the specific Document Type.
    fn build(&self, buf: &mut Vec<u8>) -> ::core::result::Result<(), Self::Error>;
}
