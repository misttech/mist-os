// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// TODO(fxbug.dev/120283): Remove. This is force-included as part of ffx_plugin.
use anyhow as _;
use async_trait::async_trait;
use errors::ffx_bail;
use ffx_config::EnvironmentContext;
use ffx_daemon::{DaemonConfig, SocketDetails};
use ffx_daemon_stop_args::StopCommand;
use fho::{FfxContext, FfxMain, FfxTool, Result, SimpleWriter};
use fidl_fuchsia_developer_ffx as ffx;
use fuchsia_async::{Time, Timer};
use std::io::Write;
use std::time::Duration;

const STOP_WAIT_POLL_TIME: Duration = Duration::from_millis(100);

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
    writer: &mut SimpleWriter,
    context: &EnvironmentContext,
    timeout: Option<u32>,
) -> Result<()> {
    let socket_path = context.get_ascendd_path().await?;
    let start_time = Time::now();
    let mut last_pid = None;
    loop {
        let details = SocketDetails::new(socket_path.clone());
        let pid = match details.get_running_pid() {
            Some(pid) => pid,
            // Note: either it's already dead, or we couldn't get it for some other reason
            // (e.g. the daemon was run by another user). Since it might have actually died
            // already, let's not print an error.
            None => return Ok(()),
        };
        // Catch if the daemon is being restarted for some reason
        if let Some(last_pid) = last_pid {
            if last_pid != pid {
                writeln!(writer, "Daemon pid changed, was {last_pid}, now {pid}").bug()?;
            }
        }
        last_pid = Some(pid);

        Timer::new(STOP_WAIT_POLL_TIME).await;
        if let Some(timeout_ms) = timeout {
            if Time::now() - start_time > Duration::from_millis(timeout_ms.into()) {
                writeln!(writer, "Daemon did not exit after {timeout_ms} ms").bug()?;
                writeln!(writer, "{details}").bug()?;
                writeln!(writer, "Killing daemon (pid {pid})").bug()?;
                ffx_daemon::try_to_kill_pid(pid).await?;
                return Ok(());
            }
        }
    }
}

enum WaitBehavior {
    Wait,
    Timeout(u32),
    NoWait,
}

impl StopTool {
    // Determine the wait behavior from the options provided. This forces at most
    // one option to be specified. It would be possible to enforce that with argh,
    // but it's nice for the user to be able to type the simple "-w" for waiting,
    // instead of "-b wait" or what-have-you.
    fn get_wait_behavior(&self, writer: &mut SimpleWriter) -> Result<WaitBehavior> {
        let wb = match (self.cmd.wait, self.cmd.no_wait, self.cmd.timeout_ms) {
            (true, false, None) => WaitBehavior::Wait,
            (false, true, None) => WaitBehavior::NoWait,
            (false, false, Some(t)) => WaitBehavior::Timeout(t),
            // TODO(126735) -- eventually change default behavior to Wait or Timeout
            (false, false, None) => {
                writeln!(writer, "No wait behavior specified -- not waiting for daemon to stop.")
                    .bug()?;
                writeln!(writer, "** The daemon may not have exited. To wait for it to exit,")
                    .bug()?;
                writeln!(writer, "** specify -w or -t timeout_ms. In the future, the default")
                    .bug()?;
                writeln!(writer, "** behavior may change. Specify --no-wait to avoid this message")
                    .bug()?;
                WaitBehavior::NoWait
            }
            _ => {
                ffx_bail!("Multiple wait behaviors specified.\nSpecify only one of -w, -t <timeout>, or --no-wait");
            }
        };
        Ok(wb)
    }
}

#[async_trait(?Send)]
impl FfxMain for StopTool {
    type Writer = SimpleWriter;
    async fn main(self, mut writer: SimpleWriter) -> Result<()> {
        let wb = self.get_wait_behavior(&mut writer)?;
        if let Some(d) = self.daemon_proxy {
            d.quit().await.bug()?;
            match wb {
                WaitBehavior::NoWait => (),
                WaitBehavior::Wait => {
                    wait_for_daemon_to_exit(&mut writer, &self.context, None).await?
                }
                WaitBehavior::Timeout(t) => {
                    wait_for_daemon_to_exit(&mut writer, &self.context, Some(t)).await?
                }
            }
        }
        // It might be worth considering informing the user that no daemon was running here.
        writeln!(writer, "Stopped daemon.").bug()?;
        Ok(())
    }
}

///////////////////////////////////////////////////////////////////////////////
// tests

#[cfg(test)]
mod test {
    use super::*;
    use fho::macro_deps::ffx_writer::TestBuffer;
    use futures_lite::StreamExt;

    fn setup_fake_daemon_server() -> ffx::DaemonProxy {
        let (proxy, mut stream) =
            fidl::endpoints::create_proxy_and_stream::<ffx::DaemonMarker>().unwrap();
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

    #[fuchsia_async::run_singlethreaded(test)]
    async fn run_stop_test() {
        let config_env = ffx_config::test_init().await.unwrap();
        let tool_env = fho::testing::ToolEnv::new()
            .try_daemon_closure(|| async { Ok(Some(setup_fake_daemon_server())) });
        let test_stdout = TestBuffer::default();
        let writer = SimpleWriter::new_buffers(test_stdout.clone(), Vec::new());
        let tool = tool_env
            .build_tool_from_cmd::<StopTool>(
                StopCommand { wait: false, no_wait: true, timeout_ms: None },
                config_env.context.clone(),
            )
            .await
            .unwrap();
        let result = tool.main(writer).await;
        assert!(result.is_ok());
        let output = test_stdout.into_string();
        assert!(output.ends_with("Stopped daemon.\n"));
    }
}
