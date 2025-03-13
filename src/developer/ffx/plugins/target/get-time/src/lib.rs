// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use async_trait::async_trait;
use ffx_target_get_time_args::GetTimeCommand;
use ffx_writer::SimpleWriter;
use fho::{FfxContext, FfxMain, FfxTool};
use target_holders::RemoteControlProxyHolder;

#[derive(FfxTool)]
pub struct GetTimeTool {
    #[command]
    cmd: GetTimeCommand,
    rcs_proxy: RemoteControlProxyHolder,
}

fho::embedded_plugin!(GetTimeTool);

#[async_trait(?Send)]
impl FfxMain for GetTimeTool {
    type Writer = SimpleWriter;
    async fn main(self, mut writer: Self::Writer) -> fho::Result<()> {
        get_time_impl(self.rcs_proxy, &mut writer, self.cmd.boot).await
    }
}

async fn get_time_impl<W>(
    rcs_proxy: RemoteControlProxyHolder,
    mut writer: W,
    boot_time: bool,
) -> fho::Result<()>
where
    W: std::io::Write,
{
    let time = if boot_time {
        rcs_proxy.get_boot_time().await.user_message("Failed to get boot time")?.into_nanos()
    } else {
        rcs_proxy.get_time().await.user_message("Failed to get monotonic time")?.into_nanos()
    };
    writer.write_all(format!("{time}").as_bytes()).unwrap();
    Ok(())
}

#[cfg(test)]
mod test {
    use super::*;
    use fidl_fuchsia_developer_remotecontrol as rcs;
    use target_holders::fake_proxy;

    fn setup_fake_time_server_proxy() -> rcs::RemoteControlProxy {
        fake_proxy(move |req| match req {
            rcs::RemoteControlRequest::GetTime { responder } => {
                responder.send(fidl::MonotonicInstant::from_nanos(123456789)).unwrap();
            }
            rcs::RemoteControlRequest::GetBootTime { responder } => {
                responder.send(fidl::BootInstant::from_nanos(234567890)).unwrap();
            }
            _ => {}
        })
    }

    #[fuchsia::test]
    async fn test_get_monotonic() {
        let mut writer = Vec::new();
        get_time_impl(setup_fake_time_server_proxy().into(), &mut writer, false).await.unwrap();

        let output = String::from_utf8(writer).unwrap();
        assert_eq!(output, "123456789");
    }

    #[fuchsia::test]
    async fn test_get_boot() {
        let mut writer = Vec::new();
        get_time_impl(setup_fake_time_server_proxy().into(), &mut writer, true).await.unwrap();

        let output = String::from_utf8(writer).unwrap();
        assert_eq!(output, "234567890");
    }
}
