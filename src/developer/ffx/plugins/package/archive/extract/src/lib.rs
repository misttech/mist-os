// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Result;
use errors::ffx_error;
use ffx_core::ffx_plugin;
use package_tool::cmd_package_archive_extract;

pub use ffx_package_archive_extract_args::PackageArchiveExtractCommand;

#[ffx_plugin("ffx_package")]
pub async fn cmd_package(cmd: PackageArchiveExtractCommand) -> Result<()> {
    cmd_package_archive_extract(cmd).await.map_err(|err| ffx_error!(err))?;
    Ok(())
}
