// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::{format_err, Result},
    ffx_core::ffx_plugin,
    ffx_session_stop_args::SessionStopCommand,
    fidl_fuchsia_session::LifecycleProxy,
};

const STOPPING_SESSION: &str = "Stopping the session\n";

#[ffx_plugin(LifecycleProxy = "core/session-manager:expose:fuchsia.session.Lifecycle")]
pub async fn stop(lifecycle_proxy: LifecycleProxy, cmd: SessionStopCommand) -> Result<()> {
    stop_impl(lifecycle_proxy, cmd, &mut std::io::stdout()).await
}

pub async fn stop_impl<W: std::io::Write>(
    lifecycle_proxy: LifecycleProxy,
    _cmd: SessionStopCommand,
    writer: &mut W,
) -> Result<()> {
    write!(writer, "{}", STOPPING_SESSION)?;
    lifecycle_proxy.stop().await?.map_err(|err| format_err!("{:?}", err))
}

#[cfg(test)]
mod test {
    use {super::*, fidl_fuchsia_session::LifecycleRequest};

    #[fuchsia::test]
    async fn test_stop_session() -> Result<()> {
        let proxy = setup_fake_lifecycle_proxy(|req| match req {
            LifecycleRequest::Stop { responder } => {
                let _ = responder.send(Ok(()));
            }
            _ => panic!("Unxpected Lifecycle request"),
        });

        let stop_cmd = SessionStopCommand {};
        let mut writer = Vec::new();
        stop_impl(proxy, stop_cmd, &mut writer).await?;
        let output = String::from_utf8(writer).unwrap();
        assert_eq!(output, STOPPING_SESSION);
        Ok(())
    }
}
