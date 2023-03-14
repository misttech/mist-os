// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow as _; // all plugins are currently forced to include anyhow, so void it.
use ffx_overnet_plugin_args::OvernetCommand;
use fho::{FfxMain, FfxTool, SimpleWriter};

#[derive(FfxTool)]
pub struct OvernetTool {
    #[command]
    cmd: OvernetCommand,
}

fho::embedded_plugin!(OvernetTool);

#[async_trait::async_trait(?Send)]
impl FfxMain for OvernetTool {
    type Writer = SimpleWriter;

    async fn main(self, _writer: Self::Writer) -> fho::Result<()> {
        // todo(fxb/108692) remove this use of the global hoist when we put the main one in the environment context
        // instead.
        onet_tool::run_onet(hoist::hoist(), onet_tool::Opts { command: self.cmd.command })
            .await
            .map_err(Into::into)
    }
}
