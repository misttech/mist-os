// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use async_trait::async_trait;
use ffx_daemon_disconnect_args::DisconnectCommand;
use fho::{FfxContext, FfxMain, FfxTool, SimpleWriter};
use fidl_fuchsia_developer_ffx as ffx;
use target_holders::TargetProxyHolder;

#[derive(FfxTool)]
pub struct DisconnectTool {
    #[command]
    cmd: DisconnectCommand,
    target_proxy: TargetProxyHolder,
}

fho::embedded_plugin!(DisconnectTool);

#[async_trait(?Send)]
impl FfxMain for DisconnectTool {
    type Writer = SimpleWriter;
    async fn main(self, _writer: Self::Writer) -> fho::Result<()> {
        disconnect_impl(&self.target_proxy, self.cmd).await
    }
}

async fn disconnect_impl(
    target_proxy: &ffx::TargetProxy,
    _cmd: DisconnectCommand,
) -> fho::Result<()> {
    target_proxy.disconnect().await.user_message("FIDL: disconnect failed")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup_fake_target_server() -> ffx::TargetProxy {
        fho::testing::fake_proxy(move |req| match req {
            ffx::TargetRequest::Disconnect { responder } => {
                responder.send().expect("disconnect failed");
            }
            r => panic!("got unexpected request: {:?}", r),
        })
    }

    #[fuchsia::test]
    async fn disconnect_invoked() {
        disconnect_impl(&setup_fake_target_server(), DisconnectCommand {})
            .await
            .expect("disconnect failed");
    }
}
