// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Result;
use async_trait::async_trait;
use ffx_session_drop_power_lease_args::SessionDropPowerLeaseCommand;
use fho::{user_error, FfxMain, FfxTool, SimpleWriter};
use fidl_fuchsia_session_power::HandoffProxy;
use target_holders::moniker;

#[derive(FfxTool)]
pub struct DropPowerLeaseTool {
    #[command]
    cmd: SessionDropPowerLeaseCommand,
    #[with(moniker("/core/session-manager"))]
    handoff_proxy: HandoffProxy,
}

fho::embedded_plugin!(DropPowerLeaseTool);

#[async_trait(?Send)]
impl FfxMain for DropPowerLeaseTool {
    type Writer = SimpleWriter;
    async fn main(self, mut writer: Self::Writer) -> fho::Result<()> {
        drop_power_lease_impl(self.handoff_proxy, self.cmd, &mut writer).await?;
        Ok(())
    }
}

pub async fn drop_power_lease_impl<W: std::io::Write>(
    handoff_proxy: HandoffProxy,
    _cmd: SessionDropPowerLeaseCommand,
    writer: &mut W,
) -> Result<()> {
    writeln!(writer, "Requesting to dropping power lease on execution state")?;
    let lease = handoff_proxy
        .take()
        .await?
        .map_err(|err| user_error!("Failed to take power lease from session manager: {:?}", err))?;
    writeln!(writer, "Success!")?;
    drop(lease);
    Ok(())
}

#[cfg(test)]
mod test {
    use super::*;
    use fidl_fuchsia_session_power::HandoffRequest;

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_drop_power_lease() {
        let proxy = fho::testing::fake_proxy(|req| match req {
            HandoffRequest::Take { responder } => {
                let _ = responder.send(Ok(fidl::Event::create().into()));
            }
            x @ _ => unimplemented!("{x:?}"),
        });

        let drop_power_lease_cmd = SessionDropPowerLeaseCommand {};
        let mut writer = Vec::new();
        let result = drop_power_lease_impl(proxy, drop_power_lease_cmd, &mut writer).await;
        assert!(result.is_ok());
        let output = String::from_utf8(writer).unwrap();
        assert_eq!(output, "Requesting to dropping power lease on execution state\nSuccess!\n");
    }
}
