// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use async_trait::async_trait;
use ffx_target_remove_args::RemoveCommand;
use fho::{bug, daemon_protocol, FfxMain, FfxTool, Result, ToolIO, VerifiedMachineWriter};
use fidl_fuchsia_developer_ffx::TargetCollectionProxy;
use schemars::JsonSchema;
use serde::Serialize;

#[derive(Debug, Serialize, JsonSchema)]
pub enum CommandStatus {
    /// Successful execution with an optional informational string.
    Ok { message: Option<String> },
    /// Unexpected error with string.
    UnexpectedError { message: String },
    /// A known kind of error that can be reported usefully to the user
    UserError { message: String },
}

#[derive(FfxTool)]
pub struct RemoveTool {
    #[command]
    cmd: RemoveCommand,
    #[with(daemon_protocol())]
    target_collection_proxy: TargetCollectionProxy,
}

fho::embedded_plugin!(RemoveTool);

#[async_trait(?Send)]
impl FfxMain for RemoveTool {
    type Writer = VerifiedMachineWriter<CommandStatus>;
    async fn main(self, mut writer: Self::Writer) -> fho::Result<()> {
        match remove_impl(self.target_collection_proxy, self.cmd).await {
            Ok(message) => {
                if writer.is_machine() {
                    writer.machine(&CommandStatus::Ok { message: Some(message) })?;
                } else {
                    writeln!(writer.stderr(), "{message}")
                        .map_err(|e| bug!("writing to stderr: {e}"))?;
                }
                Ok(())
            }
            Err(fho::Error::User(e)) => {
                writer.machine(&CommandStatus::UserError { message: e.to_string() })?;
                Err(fho::Error::User(e))
            }
            Err(e) => {
                writer.machine(&CommandStatus::UnexpectedError { message: e.to_string() })?;
                Err(e)
            }
        }
    }
}

async fn remove_impl(
    target_collection: TargetCollectionProxy,
    cmd: RemoveCommand,
) -> Result<String> {
    let RemoveCommand { name_or_addr, .. } = cmd;
    if target_collection
        .remove_target(&name_or_addr)
        .await
        .map_err(|e| bug!("Cannot remove target: {e}"))?
    {
        Ok("Removed.".to_string())
    } else {
        Ok("No matching target found.".to_string())
    }
}

////////////////////////////////////////////////////////////////////////////////
// tests

#[cfg(test)]
mod test {
    use super::*;
    use fho::{Format, TestBuffers};
    use fidl_fuchsia_developer_ffx as ffx;

    fn setup_fake_target_collection_proxy<T: 'static + Fn(String) -> bool + Send>(
        test: T,
    ) -> TargetCollectionProxy {
        fho::testing::fake_proxy(move |req| match req {
            ffx::TargetCollectionRequest::RemoveTarget { target_id, responder } => {
                let result = test(target_id);
                responder.send(result).unwrap();
            }
            _ => assert!(false),
        })
    }

    #[fuchsia::test]
    async fn test_remove_existing_target() {
        let server = setup_fake_target_collection_proxy(|id| {
            assert_eq!(id, "correct-horse-battery-staple".to_owned());
            true
        });
        let tool = RemoveTool {
            cmd: RemoveCommand { name_or_addr: "correct-horse-battery-staple".to_owned() },
            target_collection_proxy: server,
        };
        let test_buffers = TestBuffers::default();
        let writer = VerifiedMachineWriter::<CommandStatus>::new_test(None, &test_buffers);
        tool.main(writer).await.expect("run main");
        assert_eq!(test_buffers.into_stderr_str(), "Removed.\n");
    }

    #[fuchsia::test]
    async fn test_remove_nonexisting_target() {
        let server = setup_fake_target_collection_proxy(|_| false);
        let tool = RemoveTool {
            cmd: RemoveCommand { name_or_addr: "incorrect-donkey-battery-jazz".to_owned() },
            target_collection_proxy: server,
        };
        let test_buffers = TestBuffers::default();
        let writer = VerifiedMachineWriter::<CommandStatus>::new_test(None, &test_buffers);

        tool.main(writer).await.expect("run main");
        assert_eq!(test_buffers.into_stderr_str(), "No matching target found.\n");
    }

    #[fuchsia::test]
    async fn test_remove_machine_nonexisting_target() {
        let server = setup_fake_target_collection_proxy(|_| false);
        let tool = RemoveTool {
            cmd: RemoveCommand { name_or_addr: "incorrect-donkey-battery-jazz".to_owned() },
            target_collection_proxy: server,
        };
        let test_buffers = TestBuffers::default();
        let writer =
            VerifiedMachineWriter::<CommandStatus>::new_test(Some(Format::Json), &test_buffers);

        tool.main(writer).await.expect("run main");
        let (actual_stdout, actual_stderr) = test_buffers.into_strings();
        assert_eq!(actual_stderr, "");
        assert_eq!(actual_stdout, "{\"Ok\":{\"message\":\"No matching target found.\"}}\n");
    }
}
