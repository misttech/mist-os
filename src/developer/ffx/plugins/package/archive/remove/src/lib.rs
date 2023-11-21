// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Result;
use errors::ffx_error;
use ffx_core::ffx_plugin;
use package_tool::cmd_package_archive_remove;

pub use ffx_package_archive_remove_args::PackageArchiveRemoveCommand;

#[ffx_plugin()]
pub async fn cmd_package(cmd: PackageArchiveRemoveCommand) -> Result<()> {
    cmd_package_archive_remove(cmd)
        .await
        .map_err(|err| ffx_error!("Error: failed to remove from archive: {err:?}"))?;
    Ok(())
}
