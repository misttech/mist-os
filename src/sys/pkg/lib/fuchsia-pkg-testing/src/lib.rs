// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Test tools for building Fuchsia packages and TUF repositories.

#![warn(clippy::all)]
#![allow(clippy::let_unit_value)]
#![deny(missing_docs)]
// TODO(fxbug.dev/123528): Remove unknown_lints after toolchain rolls.
#![allow(unknown_lints)]
// TODO(fxbug.dev/123778): Fix redundant async blocks.
#![allow(clippy::redundant_async_block)]

mod package;
pub use crate::package::{BlobContents, Package, PackageBuilder, PackageDir, VerificationError};

mod repo;
pub use crate::repo::{PackageEntry, Repository, RepositoryBuilder};
pub mod serve;

mod inspect;
pub use crate::inspect::get_inspect_hierarchy;

mod fake_pkg_local_mirror;
pub use crate::fake_pkg_local_mirror::FakePkgLocalMirror;

mod system_image;
pub use crate::system_image::SystemImageBuilder;

mod update_package;
pub use crate::update_package::{
    make_current_epoch_json, make_epoch_json, make_packages_json, TestUpdatePackage, SOURCE_EPOCH,
};

mod process;

pub mod blobfs;

mod delivery_blob;
pub use crate::delivery_blob::generate_delivery_blob;
