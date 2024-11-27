// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use async_trait::async_trait;
use ffx_config::EnvironmentContext;
use ffx_daemon::{DaemonConfig, SocketDetails};
use ffx_daemon_stop_args::StopCommand;
use fho::{
    bug, return_bug, return_user_error, Error, FfxContext, FfxMain, FfxTool, Result, ToolIO,
    VerifiedMachineWriter,
};
use fidl_fuchsia_developer_ffx as ffx;
use fuchsia_async::{MonotonicInstant, Timer};
use schemars::JsonSchema;
use serde::Serialize;
use std::time::Duration;

const STOP_WAIT_POLL_TIME: Duration = Duration::from_millis(100);

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
pub struct StopTool {
    #[command]
    cmd: StopCommand,
    daemon_proxy: Option<ffx::DaemonProxy>,
    context: EnvironmentContext,
}

fho::embedded_plugin!(StopTool);

// Similar to ffx_daemon::wait_for_daemon_to_exit(), but this version also
// provides status updates to the user.
async fn wait_for_daemon_to_exit(
    context: &EnvironmentContext,
    timeout: Option<u32>,
) -> Result<Option<String>> {
    let socket_path = context.get_ascendd_path().await?;
    let start_time = MonotonicInstant::now();
    let mut last_pid = None;
    loop {
        let details = SocketDetails::new(socket_path.clone());
        let pid = match details.get_running_pid() {
            Some(pid) => pid,
            // Note: either it's already dead, or we couldn't get it for some other reason
            // (e.g. the daemon was run by another user). Since it might have actually died
            // already, let's not print an error.
            None => return Ok(None),
        };
        // Catch if the daemon is being restarted for some reason
        if let Some(last_pid) = last_pid {
            if last_pid != pid {
                return_bug!("Daemon pid changed, was {last_pid}, now {pid}")
            }
        }
        last_pid = Some(pid);

        Timer::new(STOP_WAIT_POLL_TIME).await;
        if let Some(timeout_ms) = timeout {
            if MonotonicInstant::now() - start_time > Duration::from_millis(timeout_ms.into()) {
                let message = format!(
                    "Daemon {details} did not exit after {timeout_ms} ms. Killing daemon {pid}"
                );
                return match ffx_daemon::try_to_kill_pid(pid).await {
                    Ok(_) => Ok(Some(message)),
                    Err(e) => Err(bug!("{message}: {e:?}")),
                };
            }
        }
    }
}

enum WaitBehavior {
    Wait,
    Timeout(u32),
    NoWait(Option<String>),
}

impl StopTool {
    // Determine the wait behavior from the options provided. This forces at most
    // one option to be specified. It would be possible to enforce that with argh,
    // but it's nice for the user to be able to type the simple "-w" for waiting,
    // instead of "-b wait" or what-have-you.
    fn get_wait_behavior(&self) -> Result<WaitBehavior> {
        let wb = match (self.cmd.wait, self.cmd.no_wait, self.cmd.timeout_ms) {
            (true, false, None) => WaitBehavior::Wait,
            (false, true, None) => WaitBehavior::NoWait(None),
            (false, false, Some(t)) => WaitBehavior::Timeout(t),
            // TODO(126735) -- eventually change default behavior to Wait or Timeout
            (false, false, None) => WaitBehavior::NoWait(Some(
                "No wait behavior specified -- not waiting for daemon to stop.".into(),
            )),
            _ => {
                return_user_error!("Multiple wait behaviors specified.\nSpecify only one of -w, -t <timeout>, or --no-wait");
            }
        };
        Ok(wb)
    }
}

#[async_trait(?Send)]
impl FfxMain for StopTool {
    type Writer = VerifiedMachineWriter<CommandStatus>;
    async fn main(self, mut writer: Self::Writer) -> Result<()> {
        let wb = match self.get_wait_behavior() {
            Ok(behavior) => behavior,
            Err(e @ Error::User(_)) => {
                writer.machine(&CommandStatus::UserError { message: e.to_string() })?;
                return Err(e);
            }
            Err(e) => {
                writer.machine(&CommandStatus::UnexpectedError { message: e.to_string() })?;
                return Err(e);
            }
        };

        if let Some(d) = self.daemon_proxy {
            let timeout = match wb {
                WaitBehavior::Timeout(t) => Some(t),
                _ => None,
            };
            match d.quit().await.bug() {
                Ok(sent) => match wb {
                    WaitBehavior::NoWait(s) => {
                        if let Some(warning) = s {
                            writeln!(writer.stderr(), "{warning}").map_err(|e| bug!(e))?;
                        }
                        if !sent {
                            writeln!(writer.stderr(), "Daemon stop request not successful.")
                                .map_err(|e| bug!(e))?;
                        }
                        writer
                            .machine_or(&CommandStatus::Ok { message: None }, "Stopped daemon.")?;
                    }
                    _ => match wait_for_daemon_to_exit(&self.context, timeout).await {
                        Ok(Some(message)) => writer.machine_or(
                            &CommandStatus::Ok { message: Some(message.clone()) },
                            &message,
                        )?,
                        Ok(None) => writer
                            .machine_or(&CommandStatus::Ok { message: None }, "Stopped daemon.")?,
                        Err(e @ Error::User(_)) => {
                            writer.machine(&CommandStatus::UserError { message: e.to_string() })?;
                            return Err(e);
                        }
                        Err(e) => {
                            writer.machine(&CommandStatus::UnexpectedError {
                                message: e.to_string(),
                            })?;
                            return Err(e);
                        }
                    },
                },
                Err(e @ Error::User(_)) => {
                    writer.machine(&CommandStatus::UserError { message: e.to_string() })?;
                    return Err(e);
                }
                Err(e) => {
                    writer.machine(&CommandStatus::UnexpectedError { message: e.to_string() })?;
                    return Err(e);
                }
            }
        } else {
            let message = "No daemon was running.";
            writer
                .machine_or(&CommandStatus::Ok { message: Some(message.to_string()) }, message)?;
        }
        Ok(())
    }
}

///////////////////////////////////////////////////////////////////////////////
// tests

#[cfg(test)]
mod test {
    use super::*;
    use fho::macro_deps::ffx_writer::TestBuffer;
    use fho::{Format, TestBuffers};
    use futures_lite::StreamExt;
    use serde_json::json;

    fn setup_fake_daemon_server() -> ffx::DaemonProxy {
        let (proxy, mut stream) = fidl::endpoints::create_proxy_and_stream::<ffx::DaemonMarker>();
        fuchsia_async::Task::local(async move {
            while let Ok(Some(req)) = stream.try_next().await {
                match req {
                    ffx::DaemonRequest::Quit { responder } => {
                        responder.send(true).expect("sending quit response")
                    }
                    r => {
                        panic!("Unexpected request: {r:?}");
                    }
                }
            }
        })
        .detach();
        proxy
    }

    #[fuchsia::test]
    async fn run_stop_test() {
        let config_env = ffx_config::test_init().await.unwrap();
        let buffers = TestBuffers::default();
        let writer = <StopTool as FfxMain>::Writer::new_test(None, &buffers);
        let tool = StopTool {
            cmd: StopCommand { wait: false, no_wait: true, timeout_ms: None },
            daemon_proxy: Some(setup_fake_daemon_server()),
            context: config_env.context.clone(),
        };
        let result = tool.main(writer).await;
        assert!(result.is_ok());
        let output = buffers.into_stdout_str();
        assert!(output.ends_with("Stopped daemon.\n"), "Missing magic phrase in {output}");
    }

    #[fuchsia::test]
    async fn run_stop_test_machine() {
        let config_env = ffx_config::test_init().await.unwrap();
        let test_stdout = TestBuffer::default();
        let writer = <StopTool as FfxMain>::Writer::new_buffers(
            Some(Format::Json),
            test_stdout.clone(),
            Vec::new(),
        );
        let tool = StopTool {
            cmd: StopCommand { wait: false, no_wait: true, timeout_ms: None },
            daemon_proxy: Some(setup_fake_daemon_server()),
            context: config_env.context.clone(),
        };
        let result = tool.main(writer).await;
        assert!(result.is_ok());
        let output = test_stdout.into_string();
        let err = format!("schema not valid {output}");
        let json = serde_json::from_str(&output).expect(&err);
        let err = format!("json must adhere to schema: {json}");
        <StopTool as FfxMain>::Writer::verify_schema(&json).expect(&err);
        let want = CommandStatus::Ok { message: None };
        assert_eq!(json, json!(want));
    }
}
