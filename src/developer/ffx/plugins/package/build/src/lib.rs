// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

pub use ffx_package_build_args::PackageBuildCommand;
use ffx_writer::SimpleWriter;
use fho::{user_error, FfxMain, FfxTool, Result};
use package_tool::cmd_package_build;

#[derive(FfxTool)]
pub struct PackageBuildTool {
    #[command]
    pub cmd: PackageBuildCommand,
}

fho::embedded_plugin!(PackageBuildTool);

#[async_trait::async_trait(?Send)]
impl FfxMain for PackageBuildTool {
    type Writer = SimpleWriter;
    async fn main(self, mut _writer: <Self as fho::FfxMain>::Writer) -> fho::Result<()> {
        cmd_package_build(self.cmd)
            .await
            .map_err(|err| user_error!("Error: failed to build package: {err:?}"))?;
        Ok(())
    }
}
