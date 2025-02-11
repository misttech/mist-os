// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use async_trait::async_trait;
use ffx_target_remove_args::RemoveCommand;
use ffx_writer::{ToolIO as _, VerifiedMachineWriter};
use fho::{bug, return_bug, return_user_error, FfxMain, FfxTool, Result};
use fidl_fuchsia_developer_ffx as ffx;
use manual_targets::{Config, ManualTargets};
use schemars::JsonSchema;
use serde::Serialize;
use target_holders::daemon_protocol;

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
    target_collection_proxy: ffx::TargetCollectionProxy,
}

fho::embedded_plugin!(RemoveTool);

#[async_trait(?Send)]
impl FfxMain for RemoveTool {
    type Writer = VerifiedMachineWriter<CommandStatus>;
    async fn main(self, mut writer: Self::Writer) -> fho::Result<()> {
        match Self::remove_impl(self.target_collection_proxy, self.cmd, &mut writer).await {
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

impl RemoveTool {
    async fn remove_impl(
        target_collection: ffx::TargetCollectionProxy,
        cmd: RemoveCommand,
        writer: &mut <Self as FfxMain>::Writer,
    ) -> Result<String> {
        if cmd.all {
            let cfg = Config();
            Self::remove_all_targets(writer, &target_collection, &cfg).await
        } else if let Some(name_or_addr) = cmd.name_or_addr {
            if target_collection
                .remove_target(&name_or_addr)
                .await
                .map_err(|e| bug!("Cannot remove target: {e}"))?
            {
                Ok("Removed.".to_string())
            } else {
                Ok("No matching target found.".to_string())
            }
        } else {
            return_user_error!("need to specify a target name or address or use the --all option")
        }
    }

    async fn remove_all_targets(
        writer: &mut <Self as FfxMain>::Writer,
        target_collection: &ffx::TargetCollectionProxy,
        cfg: &Config,
    ) -> Result<String> {
        let list = match cfg.storage_get().await {
            Ok(v) => v,
            Err(e) => {
                if format!("{e:?}").contains("no value") {
                    return Ok("No manual targets found.".into());
                } else {
                    return_bug!(e)
                }
            }
        };

        if let Some(arr) = list.as_object() {
            for (k, _) in arr {
                if target_collection
                    .remove_target(&k)
                    .await
                    .map_err(|e| bug!("Cannot remove target: {e}"))?
                {
                    writeln!(writer.stderr(), "Removed {k}").map_err(|e| bug!(e))?;
                } else {
                    // This most likely happens when the daemon is restarted when running
                    // this command and the manual target collection has not been loaded yet.
                    // It will work the second time.
                    writeln!(writer.stderr(),"No matching target for {k} found. {}",
                     "This is most likely because the daemon just started. Please run this command again.").map_err(|e| bug!(e))?;
                }
            }
        }
        Ok(String::from(""))
    }
}

////////////////////////////////////////////////////////////////////////////////
// tests

#[cfg(test)]
mod test {
    use super::*;
    use ffx_config::ConfigLevel;
    use ffx_writer::{Format, TestBuffers};
    use serde_json::json;
    use target_holders::fake_proxy;

    fn setup_fake_target_collection_proxy<T: 'static + Fn(String) -> bool + Send>(
        test: T,
    ) -> ffx::TargetCollectionProxy {
        fake_proxy(move |req| match req {
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
            cmd: RemoveCommand {
                all: false,
                name_or_addr: Some("correct-horse-battery-staple".to_owned()),
            },
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
            cmd: RemoveCommand {
                all: false,
                name_or_addr: Some("incorrect-donkey-battery-jazz".to_owned()),
            },
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
            cmd: RemoveCommand {
                all: false,
                name_or_addr: Some("incorrect-donkey-battery-jazz".to_owned()),
            },
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

    #[fuchsia::test]
    async fn test_remove_all_targets_some() {
        let env = ffx_config::test_init().await.unwrap();
        const MANUAL_TARGETS: &'static str = "targets.manual";
        env.context
            .query(MANUAL_TARGETS)
            .level(Some(ConfigLevel::User))
            .set(json!({"127.0.0.1:8022": 0, "127.0.0.1:8023": 12345}))
            .await
            .unwrap();
        let server = setup_fake_target_collection_proxy(|_| true);
        let tool = RemoveTool {
            cmd: RemoveCommand { all: true, name_or_addr: None },
            target_collection_proxy: server,
        };
        let test_buffers = TestBuffers::default();
        let writer =
            VerifiedMachineWriter::<CommandStatus>::new_test(Some(Format::Json), &test_buffers);

        tool.main(writer).await.expect("run main");
        let (actual_stdout, actual_stderr) = test_buffers.into_strings();
        assert_eq!(actual_stderr, "Removed 127.0.0.1:8022\nRemoved 127.0.0.1:8023\n");
        assert_eq!(actual_stdout, "{\"Ok\":{\"message\":\"\"}}\n");
    }

    #[fuchsia::test]
    async fn test_remove_all_targets_none() {
        let _env = ffx_config::test_init().await.unwrap();

        let server = setup_fake_target_collection_proxy(|_| panic!("should not be called"));
        let tool = RemoveTool {
            cmd: RemoveCommand { all: true, name_or_addr: None },
            target_collection_proxy: server,
        };
        let test_buffers = TestBuffers::default();
        let writer =
            VerifiedMachineWriter::<CommandStatus>::new_test(Some(Format::Json), &test_buffers);

        tool.main(writer).await.expect("run main");
        let (actual_stdout, actual_stderr) = test_buffers.into_strings();
        assert_eq!(actual_stderr, "");
        assert_eq!(actual_stdout, "{\"Ok\":{\"message\":\"No manual targets found.\"}}\n");
    }
}
