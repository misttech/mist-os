// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use async_trait::async_trait;
use ffx_target_clear_preferred_ssh_address_args::ClearPreferredSshAddressCommand;
use fho::{FfxContext, FfxMain, FfxTool, SimpleWriter};
use fidl_fuchsia_developer_ffx as ffx;

#[derive(FfxTool)]
pub struct ClearPreferredSshAddressTool {
    #[command]
    cmd: ClearPreferredSshAddressCommand,
    target_proxy: ffx::TargetProxy,
}

fho::embedded_plugin!(ClearPreferredSshAddressTool);

#[async_trait(?Send)]
impl FfxMain for ClearPreferredSshAddressTool {
    type Writer = SimpleWriter;
    async fn main(self, _writer: Self::Writer) -> fho::Result<()> {
        clear_preferred_ssh_address_impl(self.target_proxy, self.cmd).await
    }
}

async fn clear_preferred_ssh_address_impl(
    target_proxy: ffx::TargetProxy,
    _cmd: ClearPreferredSshAddressCommand,
) -> fho::Result<()> {
    target_proxy
        .clear_preferred_ssh_address()
        .await
        .user_message("FIDL: clear_preferred_ssh_address failed")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup_fake_target_server() -> ffx::TargetProxy {
        fho::testing::fake_proxy(move |req| match req {
            ffx::TargetRequest::ClearPreferredSshAddress { responder } => {
                responder.send().expect("clear_preferred_ssh_address failed");
            }
            r => panic!("got unexpected request: {:?}", r),
        })
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn clear_preferred_ssh_address_invoked() {
        clear_preferred_ssh_address_impl(
            setup_fake_target_server(),
            ClearPreferredSshAddressCommand {},
        )
        .await
        .expect("clear_preferred_ssh_address failed");
    }
}
