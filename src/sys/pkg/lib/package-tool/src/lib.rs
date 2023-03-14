// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// TODO(fxb/104019): Consider enabling globally.
#![deny(unused_crate_dependencies)]
#![warn(clippy::all)]

use {
    serde::Serialize,
    std::io::{BufWriter, Write},
};

mod args;
mod package_archive;
mod package_build;
mod repo_create;
mod repo_publish;

pub use crate::{
    args::{
        PackageArchiveCreateCommand, PackageArchiveExtractCommand, PackageBuildCommand,
        RepoCreateCommand, RepoMergeCommand, RepoPublishCommand,
    },
    package_archive::{cmd_package_archive_create, cmd_package_archive_extract},
    package_build::cmd_package_build,
    repo_create::cmd_repo_create,
    repo_publish::{cmd_repo_merge, cmd_repo_publish},
};

pub(crate) const PACKAGE_MANIFEST_NAME: &str = "package_manifest.json";
pub(crate) const META_FAR_MERKLE_NAME: &str = "meta.far.merkle";
pub(crate) const BLOBS_JSON_NAME: &str = "blobs.json";

pub(crate) fn to_writer_json_pretty(
    writer: impl Write,
    value: impl Serialize,
) -> serde_json::Result<()> {
    let mut ser = serde_json::ser::Serializer::with_formatter(
        BufWriter::new(writer),
        serde_json::ser::PrettyFormatter::with_indent(b"    "),
    );

    value.serialize(&mut ser)
}
