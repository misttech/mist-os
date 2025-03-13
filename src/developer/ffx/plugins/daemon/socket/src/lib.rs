// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use ffx_config::EnvironmentContext;
use ffx_daemon::{DaemonConfig, SocketDetails};
use ffx_daemon_socket_args::SocketCommand;
use ffx_writer::ToolIO as _;
use fho::{FfxContext, FfxMain, FfxTool, Result};

#[derive(FfxTool)]
pub struct DaemonSocketTool {
    #[command]
    _cmd: SocketCommand,
    context: EnvironmentContext,
}

fho::embedded_plugin!(DaemonSocketTool);

#[async_trait::async_trait(?Send)]
impl FfxMain for DaemonSocketTool {
    type Writer = ffx_writer::MachineWriter<SocketDetails>;

    async fn main(self, mut writer: Self::Writer) -> fho::Result<()> {
        let socket_path = self.context.get_ascendd_path().await?;

        let details = SocketDetails::new(socket_path);

        writer.item(&details).bug()
    }
}
