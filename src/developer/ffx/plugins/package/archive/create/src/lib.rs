// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Result;
use errors::ffx_error;
use ffx_core::ffx_plugin;
use package_tool::cmd_package_archive_create;

pub use ffx_package_archive_create_args::PackageArchiveCreateCommand;

#[ffx_plugin("ffx_package")]
pub async fn cmd_package(cmd: PackageArchiveCreateCommand) -> Result<()> {
    cmd_package_archive_create(cmd).await.map_err(|err| ffx_error!(err))?;
    Ok(())
}
