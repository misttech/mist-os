// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Result;
use async_trait::async_trait;
use ffx_target_echo_args::EchoCommand;
use fho::{FfxMain, FfxTool, SimpleWriter};
use fidl_fuchsia_developer_remotecontrol::RemoteControlProxy;
use std::io::Write;

#[derive(FfxTool)]
pub struct EchoTool {
    #[command]
    cmd: EchoCommand,
    rcs_proxy: RemoteControlProxy,
}

fho::embedded_plugin!(EchoTool);

#[async_trait(?Send)]
impl FfxMain for EchoTool {
    type Writer = SimpleWriter;
    async fn main(self, writer: Self::Writer) -> fho::Result<()> {
        echo_impl(self.rcs_proxy, self.cmd, writer).await?;
        Ok(())
    }
}

async fn echo_impl<W: Write>(
    rcs_proxy: RemoteControlProxy,
    cmd: EchoCommand,
    mut writer: W,
) -> Result<()> {
    let echo_text = cmd.text.unwrap_or("Ffx".to_string());
    match rcs_proxy.echo_string(&echo_text).await {
        Ok(r) => {
            writeln!(writer, "SUCCESS: received {:?}", r)?;
            Ok(())
        }
        Err(e) => panic!("ERROR: {:?}", e),
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use anyhow::Context;
    use fidl_fuchsia_developer_remotecontrol::{RemoteControlMarker, RemoteControlRequest};

    fn setup_fake_service() -> RemoteControlProxy {
        use futures::TryStreamExt;
        let (proxy, mut stream) =
            fidl::endpoints::create_proxy_and_stream::<RemoteControlMarker>().unwrap();
        fuchsia_async::Task::local(async move {
            while let Ok(Some(req)) = stream.try_next().await {
                match req {
                    RemoteControlRequest::EchoString { value, responder } => {
                        responder
                            .send(value.as_ref())
                            .context("error sending response")
                            .expect("should send");
                    }
                    _ => panic!("unexpected request: {:?}", req),
                }
            }
        })
        .detach();
        proxy
    }

    async fn run_echo_test(cmd: EchoCommand) -> String {
        let mut output = Vec::new();
        let proxy = setup_fake_service();
        let result = echo_impl(proxy, cmd, &mut output).await;
        assert!(result.is_ok());
        String::from_utf8(output).expect("Invalid UTF-8 bytes")
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_echo_with_no_text() -> Result<()> {
        let output = run_echo_test(EchoCommand { text: None }).await;
        assert_eq!("SUCCESS: received \"Ffx\"\n".to_string(), output);
        Ok(())
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_echo_with_text() -> Result<()> {
        let output = run_echo_test(EchoCommand { text: Some("test".to_string()) }).await;
        assert_eq!("SUCCESS: received \"test\"\n".to_string(), output);
        Ok(())
    }
}
