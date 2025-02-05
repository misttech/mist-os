// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{format_err, Result};
use async_trait::async_trait;
use ffx_session_start_args::SessionStartCommand;
use fho::{FfxMain, FfxTool, SimpleWriter};
use fidl_fuchsia_session::{LifecycleProxy, LifecycleStartRequest};
use target_holders::moniker;

const STARTING_SESSION: &str = "Starting the default session\n";

#[derive(FfxTool)]
pub struct StartTool {
    #[command]
    cmd: SessionStartCommand,
    #[with(moniker("/core/session-manager"))]
    lifecycle_proxy: LifecycleProxy,
}

fho::embedded_plugin!(StartTool);

#[async_trait(?Send)]
impl FfxMain for StartTool {
    type Writer = SimpleWriter;
    async fn main(self, mut writer: Self::Writer) -> fho::Result<()> {
        start_impl(self.lifecycle_proxy, self.cmd, &mut writer).await?;
        Ok(())
    }
}

pub async fn start_impl<W: std::io::Write>(
    lifecycle_proxy: LifecycleProxy,
    _cmd: SessionStartCommand,
    writer: &mut W,
) -> Result<()> {
    write!(writer, "{}", STARTING_SESSION)?;
    lifecycle_proxy
        .start(&LifecycleStartRequest { ..Default::default() })
        .await?
        .map_err(|err| format_err!("{:?}", err))
}

#[cfg(test)]
mod test {
    use super::*;
    use fidl_fuchsia_session::LifecycleRequest;
    use target_holders::fake_proxy;

    #[fuchsia::test]
    async fn test_start_session() -> Result<()> {
        let proxy = fake_proxy(|req| match req {
            LifecycleRequest::Start { payload, responder, .. } => {
                assert_eq!(payload.session_url, None);
                let _ = responder.send(Ok(()));
            }
            _ => panic!("Unxpected Lifecycle request"),
        });

        let start_cmd = SessionStartCommand {};
        let mut writer = Vec::new();
        start_impl(proxy, start_cmd, &mut writer).await?;
        let output = String::from_utf8(writer).unwrap();
        assert_eq!(output, STARTING_SESSION);
        Ok(())
    }
}
