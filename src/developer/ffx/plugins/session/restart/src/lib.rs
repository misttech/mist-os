// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::{format_err, Result},
    async_trait::async_trait,
    ffx_session_restart_args::SessionRestartCommand,
    fho::{moniker, FfxMain, FfxTool, SimpleWriter},
    fidl_fuchsia_session::RestarterProxy,
};

#[derive(FfxTool)]
pub struct RestartTool {
    #[command]
    cmd: SessionRestartCommand,
    #[with(moniker("/core/session-manager"))]
    restarter_proxy: RestarterProxy,
}

fho::embedded_plugin!(RestartTool);

#[async_trait(?Send)]
impl FfxMain for RestartTool {
    type Writer = SimpleWriter;
    async fn main(self, mut writer: Self::Writer) -> fho::Result<()> {
        restart_impl(self.restarter_proxy, self.cmd, &mut writer).await?;
        Ok(())
    }
}

pub async fn restart_impl<W: std::io::Write>(
    restarter_proxy: RestarterProxy,
    _cmd: SessionRestartCommand,
    writer: &mut W,
) -> Result<()> {
    writeln!(writer, "Restarting the current session")?;
    restarter_proxy.restart().await?.map_err(|err| format_err!("{:?}", err))
}

#[cfg(test)]
mod test {
    use {super::*, fidl_fuchsia_session::RestarterRequest};

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_restart_session() {
        let proxy = fho::testing::fake_proxy(|req| match req {
            RestarterRequest::Restart { responder } => {
                let _ = responder.send(Ok(()));
            }
        });

        let restart_cmd = SessionRestartCommand {};
        let mut writer = Vec::new();
        let result = restart_impl(proxy, restart_cmd, &mut writer).await;
        assert!(result.is_ok());
        let output = String::from_utf8(writer).unwrap();
        assert_eq!(output, "Restarting the current session\n");
    }
}
