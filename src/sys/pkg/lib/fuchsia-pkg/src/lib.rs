// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#[macro_use]
pub mod test;

mod build;
mod errors;
mod meta_contents;
mod meta_package;
mod meta_subpackages;
mod package;
mod package_build_manifest;
mod package_builder;
pub mod package_directory;
mod package_manifest;
mod package_manifest_list;
mod path;
mod path_to_string;
mod subpackages_build_manifest;

pub use {
    crate::{
        errors::{
            BuildError, MetaContentsError, MetaPackageError, MetaSubpackagesError,
            PackageBuildManifestError, PackageManifestError, ParsePackagePathError,
        },
        meta_contents::MetaContents,
        meta_package::MetaPackage,
        meta_subpackages::MetaSubpackages,
        package_build_manifest::PackageBuildManifest,
        package_builder::{PackageBuilder, ABI_REVISION_FILE_PATH},
        package_directory::{
            LoadAbiRevisionError, LoadMetaContentsError, OpenRights, PackageDirectory,
            ReadHashError,
        },
        package_manifest::{
            BlobInfo, PackageManifest, PackageManifestBuilder, RelativeTo, SubpackageInfo,
        },
        package_manifest_list::PackageManifestList,
        path::{PackageName, PackagePath, PackageVariant},
        subpackages_build_manifest::{
            SubpackagesBuildManifest, SubpackagesBuildManifestEntry,
            SubpackagesBuildManifestEntryKind,
        },
    },
    fuchsia_url::errors::PackagePathSegmentError,
    path_to_string::PathToStringExt,
};

pub(crate) use crate::package::{BlobEntry, Package, SubpackageEntry};
