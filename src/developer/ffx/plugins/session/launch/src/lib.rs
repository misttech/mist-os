// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::{format_err, Result},
    async_trait::async_trait,
    ffx_session_launch_args::SessionLaunchCommand,
    fho::{moniker, FfxMain, FfxTool, SimpleWriter},
    fidl_fuchsia_session::{LaunchConfiguration, LauncherProxy},
};

#[derive(FfxTool)]
pub struct LaunchTool {
    #[command]
    cmd: SessionLaunchCommand,
    #[with(moniker("/core/session-manager"))]
    launcher_proxy: LauncherProxy,
}

fho::embedded_plugin!(LaunchTool);

#[async_trait(?Send)]
impl FfxMain for LaunchTool {
    type Writer = SimpleWriter;
    async fn main(self, mut writer: Self::Writer) -> fho::Result<()> {
        launch_impl(self.launcher_proxy, self.cmd, &mut writer).await?;
        Ok(())
    }
}

pub async fn launch_impl<W: std::io::Write>(
    launcher_proxy: LauncherProxy,
    cmd: SessionLaunchCommand,
    writer: &mut W,
) -> Result<()> {
    writeln!(writer, "Launching session: {}", cmd.url)?;
    let config = LaunchConfiguration { session_url: Some(cmd.url), ..Default::default() };
    launcher_proxy.launch(&config).await?.map_err(|err| format_err!("{:?}", err))
}

#[cfg(test)]
mod test {
    use {super::*, fidl_fuchsia_session::LauncherRequest};

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_launch_session() {
        const SESSION_URL: &str = "Session URL";

        let proxy = fho::testing::fake_proxy(|req| match req {
            LauncherRequest::Launch { configuration, responder } => {
                assert!(configuration.session_url.is_some());
                let session_url = configuration.session_url.unwrap();
                assert!(session_url == SESSION_URL.to_string());
                let _ = responder.send(Ok(()));
            }
        });

        let launch_cmd = SessionLaunchCommand { url: SESSION_URL.to_string() };
        let result = launch_impl(proxy, launch_cmd, &mut std::io::stdout()).await;
        assert!(result.is_ok());
    }
}
