// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Result;
use ffx_core::ffx_plugin;
use ffx_target_get_time_args::GetTimeCommand;
use ffx_writer::Writer;
use fidl_fuchsia_developer_remotecontrol as rcs;
use rcs::RemoteControlProxy;

async fn get_time_impl<W>(rcs_proxy: RemoteControlProxy, mut writer: W) -> Result<()>
where
    W: std::io::Write,
{
    writer.write_all(format!("{}", rcs_proxy.get_time().await?).as_bytes()).unwrap();
    Ok(())
}

// Need an invalid path so that the toolchain generates a
// setup_fake_remote_control_proxy method for testing.

#[ffx_plugin()]
pub async fn get_time(
    rcs_proxy: Option<RemoteControlProxy>,
    mut writer: Writer,
    _cmd: GetTimeCommand,
) -> Result<()> {
    get_time_impl(rcs_proxy.unwrap(), &mut writer).await
}

#[cfg(test)]
mod test {
    use super::*;

    fn setup_fake_time_server_proxy() -> rcs::RemoteControlProxy {
        setup_fake_rcs_proxy(move |req| match req {
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
