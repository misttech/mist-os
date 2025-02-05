// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use async_trait::async_trait;
use ffx_echo_args::EchoCommand;
use fho::{return_bug, FfxMain, FfxTool, Result, VerifiedMachineWriter};
use fidl_fuchsia_developer_ffx::EchoProxy;
use schemars::JsonSchema;
use serde::Serialize;
use target_holders::daemon_protocol;

#[derive(Debug, Serialize, JsonSchema, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum EchoResult {
    /// Success
    Ok(String),
    /// Unexpected error with string denoting error message.
    UnexpectedError(String),
}

#[derive(FfxTool)]
pub struct EchoTool {
    #[command]
    cmd: EchoCommand,
    #[with(daemon_protocol())]
    echo_proxy: EchoProxy,
}

fho::embedded_plugin!(EchoTool);

#[async_trait(?Send)]
impl FfxMain for EchoTool {
    type Writer = VerifiedMachineWriter<EchoResult>;
    async fn main(self, mut writer: Self::Writer) -> Result<()> {
        match echo_impl(self.echo_proxy, self.cmd).await {
            Ok(message) => {
                writer.machine_or(&EchoResult::Ok(message.clone()), message)?;
                return Ok(());
            }
            Err(e) => {
                writer.machine_or(&EchoResult::UnexpectedError(e.to_string()), &e)?;
                return Err(e);
            }
        }
    }
}

async fn echo_impl(echo_proxy: EchoProxy, cmd: EchoCommand) -> Result<String> {
    let echo_text = cmd.text.unwrap_or_else(|| "Ffx".to_string());
    match echo_proxy.echo_string(&echo_text).await {
        Ok(r) => Ok(format!("SUCCESS: received {r:?}")),
        Err(e) => return_bug!("ERROR: {e:?}"),
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use fho::{Format, TestBuffers};
    use fidl_fuchsia_developer_ffx as ffx;
    use futures_lite::stream::StreamExt;
    use serde_json::json;

    fn setup_fake_echo_proxy() -> ffx::EchoProxy {
        let (proxy, mut stream) = fidl::endpoints::create_proxy_and_stream::<ffx::EchoMarker>();
        fuchsia_async::Task::local(async move {
            while let Ok(Some(req)) = stream.try_next().await {
                match req {
                    ffx::EchoRequest::EchoString { value, responder } => {
                        responder.send(value.as_ref()).unwrap();
                    }
                }
            }
        })
        .detach();
        proxy
    }

    async fn run_echo_test(cmd: EchoCommand) -> String {
        let echo_proxy = setup_fake_echo_proxy();

        let tool = EchoTool { cmd, echo_proxy };
        let buffers = TestBuffers::default();
        let writer = <EchoTool as FfxMain>::Writer::new_test(None, &buffers);

        let result = tool.main(writer).await;

        assert!(result.is_ok());
        buffers.into_stdout_str()
    }

    #[fuchsia::test]
    async fn test_echo_with_no_text() -> Result<()> {
        let output = run_echo_test(EchoCommand { text: None }).await;
        assert_eq!("SUCCESS: received \"Ffx\"\n".to_string(), output);
        Ok(())
    }

    #[fuchsia::test]
    async fn test_echo_with_text() -> Result<()> {
        let output = run_echo_test(EchoCommand { text: Some("test".to_string()) }).await;
        assert_eq!("SUCCESS: received \"test\"\n".to_string(), output);
        Ok(())
    }

    #[fuchsia::test]
    async fn test_echo_with_text_machine() {
        let echo_proxy = setup_fake_echo_proxy();

        let tool = EchoTool { cmd: EchoCommand { text: Some("test".to_string()) }, echo_proxy };
        let buffers = TestBuffers::default();
        let writer = <EchoTool as FfxMain>::Writer::new_test(Some(Format::Json), &buffers);

        let result = tool.main(writer).await;

        assert!(result.is_ok());
        let output = buffers.into_stdout_str();

        let err = format!("schema not valid {output}");
        let json = serde_json::from_str(&output).expect(&err);
        let err = format!("json must adhere to schema: {json}");
        <EchoTool as FfxMain>::Writer::verify_schema(&json).expect(&err);

        let want = EchoResult::Ok("SUCCESS: received \"test\"".into());
        assert_eq!(json, json!(want))
    }
}
