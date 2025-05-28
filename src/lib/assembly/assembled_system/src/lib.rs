// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![deny(missing_docs)]

//! Constructing and parsing an assembled system, which contains all the
//! images necessary to boot Fuchsia.

mod assembled_system;
mod image;

pub use assembled_system::AssembledSystem;
pub use image::{BlobfsContents, Image, PackageMetadata, PackageSetMetadata, PackagesMetadata};
