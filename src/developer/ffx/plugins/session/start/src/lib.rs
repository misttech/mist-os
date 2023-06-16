// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::{format_err, Result},
    ffx_core::ffx_plugin,
    ffx_session_start_args::SessionStartCommand,
    fidl_fuchsia_session::{LifecycleProxy, LifecycleStartRequest},
};

const STARTING_SESSION: &str = "Starting the default session\n";

#[ffx_plugin(LifecycleProxy = "core/session-manager:expose:fuchsia.session.Lifecycle")]
pub async fn start(lifecycle_proxy: LifecycleProxy, cmd: SessionStartCommand) -> Result<()> {
    start_impl(lifecycle_proxy, cmd, &mut std::io::stdout()).await
}

pub async fn start_impl<W: std::io::Write>(
    lifecycle_proxy: LifecycleProxy,
    _cmd: SessionStartCommand,
    writer: &mut W,
) -> Result<()> {
    write!(writer, "{}", STARTING_SESSION)?;
    lifecycle_proxy
        .start(&LifecycleStartRequest { ..Default::default() })
        .await?
        .map_err(|err| format_err!("{:?}", err))
}

#[cfg(test)]
mod test {
    use {super::*, fidl_fuchsia_session::LifecycleRequest};

    #[fuchsia::test]
    async fn test_start_session() -> Result<()> {
        let proxy = setup_fake_lifecycle_proxy(|req| match req {
            LifecycleRequest::Start { payload, responder, .. } => {
                assert_eq!(payload.session_url, None);
                let _ = responder.send(Ok(()));
            }
            _ => panic!("Unxpected Lifecycle request"),
        });

        let start_cmd = SessionStartCommand {};
        let mut writer = Vec::new();
        start_impl(proxy, start_cmd, &mut writer).await?;
        let output = String::from_utf8(writer).unwrap();
        assert_eq!(output, STARTING_SESSION);
        Ok(())
    }
}
