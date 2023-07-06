// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::{format_err, Result},
    async_trait::async_trait,
    ffx_session_start_args::SessionStartCommand,
    fho::{moniker, FfxMain, FfxTool, SimpleWriter},
    fidl_fuchsia_session::{LifecycleProxy, LifecycleStartRequest},
};

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
    use {super::*, fidl_fuchsia_session::LifecycleRequest};

    #[fuchsia::test]
    async fn test_start_session() -> Result<()> {
        let proxy = fho::testing::fake_proxy(|req| match req {
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
