// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use assembly_util::{impl_path_type_marker, PathTypeMarker, TypedPathBuf};
use schemars::JsonSchema;

/// The marker trait for paths within a package
#[derive(JsonSchema)]
pub struct InternalPathMarker {}
impl_path_type_marker!(InternalPathMarker);

/// The semantic type for paths within a package
pub type PackageInternalPathBuf = TypedPathBuf<InternalPathMarker>;

/// The marker trait for the source path when that's ambiguous (like in a list
/// of source to destination paths)
#[derive(JsonSchema)]
pub struct SourcePathMarker {}
impl_path_type_marker!(SourcePathMarker);

/// The semantic type for paths that are the path to the source of a file to use
/// in some context.  Such as the source file for a blob in a package.
pub type SourcePathBuf = TypedPathBuf<SourcePathMarker>;

/// The marker trait for paths to a PackageManifest
#[derive(JsonSchema)]
pub struct PackageManifestPathMarker {}
impl_path_type_marker!(PackageManifestPathMarker);

/// The semantic type for paths that are the path to a package manifest.
pub type PackageManifestPathBuf = TypedPathBuf<PackageManifestPathMarker>;
