// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{format_err, Result};
use async_trait::async_trait;
use component_debug::config::RawConfigEntry;
use errors::ffx_error;
use ffx_session_launch_args::SessionLaunchCommand;
use fho::{FfxMain, FfxTool, SimpleWriter};
use fidl_fuchsia_session::{LaunchConfiguration, LauncherProxy};
use moniker::Moniker;
use target_holders::moniker;
use {fidl_fuchsia_component_decl as fdecl, fidl_fuchsia_developer_remotecontrol as rc};

const SESSION_MANAGER_MONIKER: &str = "/core/session-manager";

#[derive(FfxTool)]
pub struct LaunchTool {
    #[command]
    cmd: SessionLaunchCommand,
    rcs: rc::RemoteControlProxy,
    #[with(moniker(SESSION_MANAGER_MONIKER))]
    launcher_proxy: LauncherProxy,
}

fho::embedded_plugin!(LaunchTool);

#[async_trait(?Send)]
impl FfxMain for LaunchTool {
    type Writer = SimpleWriter;
    async fn main(self, mut writer: Self::Writer) -> fho::Result<()> {
        launch_impl(self.launcher_proxy, self.rcs, self.cmd, &mut writer).await?;
        Ok(())
    }
}

pub async fn launch_impl<W: std::io::Write>(
    launcher_proxy: LauncherProxy,
    rcs: rc::RemoteControlProxy,
    cmd: SessionLaunchCommand,
    writer: &mut W,
) -> Result<()> {
    writeln!(writer, "Launching session: {}", cmd.url)?;
    // A moniker is needed to resolve the component declaration because resolution is
    // context dependent on the location within the topology (e.g. to find resolver).
    // But the child name doesn't matter so we can use `placeholder`.
    let moniker = format!("{SESSION_MANAGER_MONIKER}/session:placeholder").parse().unwrap();
    let config_capabilities =
        resolve_config_capabilities(&cmd.url, &moniker, &rcs, cmd.config).await?;
    let config = LaunchConfiguration {
        session_url: Some(cmd.url),
        config_capabilities: Some(config_capabilities),
        ..Default::default()
    };
    launcher_proxy.launch(&config).await?.map_err(|err| format_err!("{:?}", err))
}

/// Parse raw config entries from the command line into appropriately typed
/// config values by looking up the component declaration from `url`.
async fn resolve_config_capabilities(
    url: &str,
    moniker: &Moniker,
    rcs: &rc::RemoteControlProxy,
    raw_capabilities: Vec<RawConfigEntry>,
) -> Result<Vec<fdecl::Configuration>> {
    if raw_capabilities.is_empty() {
        return Ok(vec![]);
    }
    let realm_query = rcs::root_realm_query(rcs, std::time::Duration::from_secs(15))
        .await
        .map_err(|err| ffx_error!("Could not open RealmQuery: {err}"))?;
    let resolved_capabilities = component_debug::config::resolve_raw_config_capabilities(
        &realm_query,
        moniker,
        url,
        &raw_capabilities,
    )
    .await?;
    Ok(resolved_capabilities)
}

#[cfg(test)]
mod test {
    use super::*;
    use fidl_fuchsia_session::LauncherRequest;

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
        let rcs = fho::testing::fake_proxy(|_req| unimplemented!());

        let launch_cmd = SessionLaunchCommand { url: SESSION_URL.to_string(), config: vec![] };
        let result = launch_impl(proxy, rcs, launch_cmd, &mut std::io::stdout()).await;
        assert!(result.is_ok());
    }
}
