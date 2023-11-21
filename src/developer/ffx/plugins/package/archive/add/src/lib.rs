// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Result;
use errors::ffx_error;
use ffx_core::ffx_plugin;
use package_tool::cmd_package_archive_add;

pub use ffx_package_archive_add_args::PackageArchiveAddCommand;

#[ffx_plugin()]
pub async fn cmd_package(cmd: PackageArchiveAddCommand) -> Result<()> {
    cmd_package_archive_add(cmd)
        .await
        .map_err(|err| ffx_error!("Error: failed to add to archive: {err:?}"))?;
    Ok(())
}
