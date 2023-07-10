// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use async_trait::async_trait;
use ffx_target_get_time_args::GetTimeCommand;
use fho::{FfxContext, FfxMain, FfxTool, SimpleWriter};
use fidl_fuchsia_developer_remotecontrol as rcs;
use rcs::RemoteControlProxy;

#[derive(FfxTool)]
pub struct GetTimeTool {
    #[command]
    _cmd: GetTimeCommand,
    rcs_proxy: RemoteControlProxy,
}

fho::embedded_plugin!(GetTimeTool);

#[async_trait(?Send)]
impl FfxMain for GetTimeTool {
    type Writer = SimpleWriter;
    async fn main(self, mut writer: Self::Writer) -> fho::Result<()> {
        get_time_impl(self.rcs_proxy, &mut writer).await
    }
}

async fn get_time_impl<W>(rcs_proxy: RemoteControlProxy, mut writer: W) -> fho::Result<()>
where
    W: std::io::Write,
{
    let time = rcs_proxy.get_time().await.user_message("Failed to get time")?;
    writer.write_all(format!("{time}").as_bytes()).unwrap();
    Ok(())
}

#[cfg(test)]
mod test {
    use super::*;

    fn setup_fake_time_server_proxy() -> rcs::RemoteControlProxy {
        fho::testing::fake_proxy(move |req| match req {
            rcs::RemoteControlRequest::GetTime { responder } => {
                responder.send(123456789).unwrap();
            }
            _ => {}
        })
    }

    #[fuchsia::test]
    async fn test_get_monotonic() {
        let mut writer = Vec::new();
        get_time_impl(setup_fake_time_server_proxy(), &mut writer).await.unwrap();

        let output = String::from_utf8(writer).unwrap();
        assert_eq!(output, "123456789");
    }
}
