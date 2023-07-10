// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use async_trait::async_trait;
use ffx_off_args::OffCommand;
use fho::{moniker, FfxContext, FfxMain, FfxTool, SimpleWriter};
use fidl_fuchsia_hardware_power_statecontrol::AdminProxy;
use fuchsia_zircon_status as zx;

#[derive(FfxTool)]
pub struct OffTool {
    #[command]
    cmd: OffCommand,
    #[with(moniker("/bootstrap/shutdown_shim"))]
    admin_proxy: AdminProxy,
}

fho::embedded_plugin!(OffTool);

#[async_trait(?Send)]
impl FfxMain for OffTool {
    type Writer = SimpleWriter;
    async fn main(self, _writer: Self::Writer) -> fho::Result<()> {
        off(self.admin_proxy, self.cmd).await
    }
}

async fn off(admin_proxy: AdminProxy, _cmd: OffCommand) -> fho::Result<()> {
    admin_proxy
        .poweroff()
        .await
        .bug()?
        .map_err(zx::Status::from_raw)
        .user_message("Unexpected error from poweroff")
}

////////////////////////////////////////////////////////////////////////////////
// tests

#[cfg(test)]
mod test {
    use super::*;
    use fidl_fuchsia_hardware_power_statecontrol::AdminRequest;

    fn setup_fake_admin_server() -> AdminProxy {
        fho::testing::fake_proxy(|req| match req {
            AdminRequest::Poweroff { responder } => {
                responder.send(Ok(())).unwrap();
            }
            _ => assert!(false),
        })
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_off() {
        let admin_proxy = setup_fake_admin_server();
        let result = off(admin_proxy, OffCommand {}).await;
        assert!(result.is_ok());
    }
}
