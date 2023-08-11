// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![deny(missing_docs)]

//! Library for reading and writing a config describing which images to
//! generate and how.

mod board_filesystem_config;
mod images_config;
mod product_filesystem_config;

pub use images_config::Fvm;
pub use images_config::Fxfs;
pub use images_config::VBMeta;
pub use images_config::Zbi;
pub use images_config::{BlobFS, EmptyData, FvmFilesystem, Reserved};
pub use images_config::{FvmOutput, NandFvm, SparseFvm, StandardFvm};
pub use images_config::{Image, ImagesConfig};

pub use board_filesystem_config::{
    BoardFilesystemConfig, PostProcessingScript, VBMetaDescriptor, ZbiCompression,
};
pub use product_filesystem_config::{BlobfsLayout, ImageName, ProductFilesystemConfig};
