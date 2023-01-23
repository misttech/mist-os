// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// TODO(fxbug.dev/120283): Remove. This is force-included as part of ffx_plugin.
use anyhow as _;
use async_trait::async_trait;
use ffx_daemon_stop_args::StopCommand;
use fho::{FfxContext, FfxMain, FfxTool, Result, SimpleWriter};
use fidl_fuchsia_developer_ffx as ffx;

#[derive(FfxTool)]
pub struct StopTool {
    #[command]
    _cmd: StopCommand,
    daemon_proxy: Option<ffx::DaemonProxy>,
}

fho::embedded_plugin!(StopTool);

#[async_trait(?Send)]
impl FfxMain for StopTool {
    type Writer = SimpleWriter;
    async fn main(self, writer: &SimpleWriter) -> Result<()> {
        if let Some(d) = self.daemon_proxy {
            d.quit().await.bug()?;
        }
        // It might be worth considering informing the user that no daemon was running here.
        writer.line("Stopped daemon.").bug()?;
        Ok(())
    }
}

///////////////////////////////////////////////////////////////////////////////
// tests

#[cfg(test)]
mod test {
    use super::*;
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
        let writer = SimpleWriter::new_test(None);
        let tool = tool_env
            .build_tool_from_cmd::<StopTool>(StopCommand {}, &config_env.context)
            .await
            .unwrap();
        let result = tool.main(&writer).await;
        assert!(result.is_ok());
        let output = writer.test_output().unwrap();
        assert_eq!(output, "Stopped daemon.\n");
    }
}
