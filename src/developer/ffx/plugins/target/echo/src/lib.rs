// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Result;
use async_trait::async_trait;
use ffx_target_echo_args::EchoCommand;
use fho::{Connector, FfxMain, FfxTool, SimpleWriter};
use fidl_fuchsia_developer_remotecontrol::RemoteControlProxy;
use std::io::Write;

#[derive(FfxTool)]
pub struct EchoTool {
    #[command]
    cmd: EchoCommand,
    rcs_proxy: Connector<RemoteControlProxy>,
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
    rcs_proxy_connector: Connector<RemoteControlProxy>,
    cmd: EchoCommand,
    mut writer: W,
) -> Result<()> {
    let echo_text = cmd.text.unwrap_or("Ffx".to_string());
    // This outer loop retries connecting to the target every time the
    // connection fails. If we only connect once it only runs once.
    loop {
        // Get a connection to the target. This will fail with a timeout if the
        // target isn't there in time, so the loop will still break if the
        // target goes missing, but it should always connect to the daemon as it
        // has the power to start the daemon if it is missing.
        //
        // If the daemon is disabled from auto-starting with `daemon.autostart =
        // false` then this will still fail and exit the tool. Workflows that
        // need tools to auto-reconnect but still need to manually manage the
        // daemon aren't known to us at this time.
        //
        // Daemonless workflows should behave as though the daemon is always
        // reachable as far as this command is concerned, but daemonless is
        // experimental/unimplemented as of now so this isn't tested.
        let rcs_proxy = rcs_proxy_connector.try_connect().await?;

        // The inner loop handles the repetition part of the --repeat argument.
        // If that argument wasn't specified then this too only runs once.
        loop {
            match rcs_proxy.echo_string(&echo_text).await {
                Ok(r) => {
                    writeln!(writer, "SUCCESS: received {r:?}")?;
                }
                Err(e) => {
                    if cmd.repeat {
                        writeln!(writer, "ERROR: {e:?}")?;
                        break;
                    } else {
                        panic!("ERROR: {e:?}")
                    }
                }
            }

            if cmd.repeat {
                fuchsia_async::Timer::new(std::time::Duration::from_secs(1)).await;
            } else {
                return Ok(());
            }
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use anyhow::Context;
    use fho::testing::ToolEnv;
    use fho::TryFromEnv;
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
        let tool_env = ToolEnv::new().remote_factory_closure(|| async { Ok(setup_fake_service()) });

        let env = tool_env.make_environment(ffx_config::EnvironmentContext::no_context(
            ffx_config::environment::ExecutableKind::Test,
            Default::default(),
            None,
        ));
        let connector = Connector::try_from_env(&env).await.expect("Could not make test connector");

        let mut output = Vec::new();
        let result = echo_impl(connector, cmd, &mut output).await;
        assert!(result.is_ok());
        String::from_utf8(output).expect("Invalid UTF-8 bytes")
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_echo_with_no_text() -> Result<()> {
        let output = run_echo_test(EchoCommand { text: None, repeat: false }).await;
        assert_eq!("SUCCESS: received \"Ffx\"\n".to_string(), output);
        Ok(())
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_echo_with_text() -> Result<()> {
        let output =
            run_echo_test(EchoCommand { text: Some("test".to_string()), repeat: false }).await;
        assert_eq!("SUCCESS: received \"test\"\n".to_string(), output);
        Ok(())
    }
}
