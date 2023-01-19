// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {anyhow::Result, ffx_daemon_stop_args::StopCommand, fidl_fuchsia_developer_ffx as ffx};

// If you're looking at this command as an example, don't follow this pattern. A better
// one would be the echo command.
pub async fn ffx_plugin_impl(injector: &dyn ffx_core::Injector, cmd: StopCommand) -> Result<()> {
    stop_impl(injector.try_daemon().await?, cmd, &mut std::io::stdout()).await
}

pub fn ffx_plugin_writer_output() -> String {
    "Not supported".to_owned()
}

pub fn ffx_plugin_is_machine_supported() -> bool {
    false
}

async fn stop_impl<W: std::io::Write>(
    daemon_proxy: Option<ffx::DaemonProxy>,
    _cmd: StopCommand,
    writer: &mut W,
) -> Result<()> {
    if let Some(daemon) = daemon_proxy {
        daemon.quit().await?;
    }
    // It might be worth considering informing the user that no daemon was running here.
    writeln!(writer, "Stopped daemon.")?;
    Ok(())
}

///////////////////////////////////////////////////////////////////////////////
// tests

#[cfg(test)]
mod test {
    use {super::*, futures_lite::StreamExt};

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
        let proxy = setup_fake_daemon_server();
        let mut writer = Vec::new();
        let result = stop_impl(Some(proxy), StopCommand {}, &mut writer).await;
        assert!(result.is_ok());
        let output = String::from_utf8(writer).unwrap();
        assert_eq!(output, "Stopped daemon.\n");
    }
}
