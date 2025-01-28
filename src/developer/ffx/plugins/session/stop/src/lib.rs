// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{format_err, Result};
use async_trait::async_trait;
use ffx_session_stop_args::SessionStopCommand;
use fho::{FfxMain, FfxTool, SimpleWriter};
use fidl_fuchsia_session::LifecycleProxy;
use target_holders::moniker;

const STOPPING_SESSION: &str = "Stopping the session\n";

#[derive(FfxTool)]
pub struct StopTool {
    #[command]
    cmd: SessionStopCommand,
    #[with(moniker("/core/session-manager"))]
    lifecycle_proxy: LifecycleProxy,
}

fho::embedded_plugin!(StopTool);

#[async_trait(?Send)]
impl FfxMain for StopTool {
    type Writer = SimpleWriter;
    async fn main(self, mut writer: Self::Writer) -> fho::Result<()> {
        stop_impl(self.lifecycle_proxy, self.cmd, &mut writer).await?;
        Ok(())
    }
}

pub async fn stop_impl<W: std::io::Write>(
    lifecycle_proxy: LifecycleProxy,
    _cmd: SessionStopCommand,
    writer: &mut W,
) -> Result<()> {
    write!(writer, "{}", STOPPING_SESSION)?;
    lifecycle_proxy.stop().await?.map_err(|err| format_err!("{:?}", err))
}

#[cfg(test)]
mod test {
    use super::*;
    use fidl_fuchsia_session::LifecycleRequest;

    #[fuchsia::test]
    async fn test_stop_session() -> Result<()> {
        let proxy = fho::testing::fake_proxy(|req| match req {
            LifecycleRequest::Stop { responder } => {
                let _ = responder.send(Ok(()));
            }
            _ => panic!("Unxpected Lifecycle request"),
        });

        let stop_cmd = SessionStopCommand {};
        let mut writer = Vec::new();
        stop_impl(proxy, stop_cmd, &mut writer).await?;
        let output = String::from_utf8(writer).unwrap();
        assert_eq!(output, STOPPING_SESSION);
        Ok(())
    }
}
