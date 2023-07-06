// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Result;
use async_trait::async_trait;
use ffx_setui_night_mode_args::NightMode;
use fho::{moniker, AvailabilityFlag, FfxMain, FfxTool, SimpleWriter};
use fidl_fuchsia_settings::{NightModeProxy, NightModeSettings};
use utils::handle_mixed_result;
use utils::{self, Either, WatchOrSetResult};

#[derive(FfxTool)]
#[check(AvailabilityFlag("setui"))]
pub struct NightModeTool {
    #[command]
    cmd: NightMode,
    #[with(moniker("/core/setui_service"))]
    night_mode_proxy: NightModeProxy,
}

fho::embedded_plugin!(NightModeTool);

#[async_trait(?Send)]
impl FfxMain for NightModeTool {
    type Writer = SimpleWriter;
    async fn main(self, mut writer: Self::Writer) -> fho::Result<()> {
        run_command(self.night_mode_proxy, self.cmd, &mut writer).await?;
        Ok(())
    }
}

async fn run_command<W: std::io::Write>(
    night_mode_proxy: NightModeProxy,
    night_mode: NightMode,
    writer: &mut W,
) -> Result<()> {
    handle_mixed_result(
        "NightMode",
        command(night_mode_proxy, night_mode.night_mode_enabled).await,
        writer,
    )
    .await
}

async fn command(proxy: NightModeProxy, night_mode_enabled: Option<bool>) -> WatchOrSetResult {
    let mut settings = NightModeSettings::default();
    settings.night_mode_enabled = night_mode_enabled;

    if settings == NightModeSettings::default() {
        Ok(Either::Watch(utils::watch_to_stream(proxy, |p| p.watch())))
    } else {
        Ok(Either::Set(if let Err(err) = proxy.set(&settings).await? {
            format!("{:?}", err)
        } else {
            format!("Successfully set night_mode_enabled to {:?}", night_mode_enabled)
        }))
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use fidl_fuchsia_settings::{NightModeRequest, NightModeSettings};
    use test_case::test_case;

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_run_command() {
        const ENABLED: bool = true;

        let proxy = fho::testing::fake_proxy(move |req| match req {
            NightModeRequest::Set { responder, .. } => {
                let _ = responder.send(Ok(()));
            }
            NightModeRequest::Watch { .. } => {
                panic!("Unexpected call to watch");
            }
        });

        let night_mode = NightMode { night_mode_enabled: Some(ENABLED) };
        let response = run_command(proxy, night_mode, &mut vec![]).await;
        assert!(response.is_ok());
    }

    #[test_case(
        true;
        "Test night mode set() output with night_mode_enabled as true."
    )]
    #[test_case(
        false;
        "Test night mode set() output with night_mode_enabled as false."
    )]
    #[fuchsia_async::run_singlethreaded(test)]
    async fn validate_night_mode_set_output(expected_night_mode_enabled: bool) -> Result<()> {
        let proxy = fho::testing::fake_proxy(move |req| match req {
            NightModeRequest::Set { responder, .. } => {
                let _ = responder.send(Ok(()));
            }
            NightModeRequest::Watch { .. } => {
                panic!("Unexpected call to watch");
            }
        });

        let output = utils::assert_set!(command(proxy, Some(expected_night_mode_enabled)));
        assert_eq!(
            output,
            format!(
                "Successfully set night_mode_enabled to {:?}",
                Some(expected_night_mode_enabled)
            )
        );
        Ok(())
    }

    #[test_case(
        None;
        "Test night mode watch() output with night_mode_enabled as None."
    )]
    #[test_case(
        Some(false);
        "Test night mode watch() output with night_mode_enabled as false."
    )]
    #[test_case(
        Some(true);
        "Test night mode watch() output with night_mode_enabled as true."
    )]
    #[fuchsia_async::run_singlethreaded(test)]
    async fn validate_night_mode_watch_output(
        expected_night_mode_enabled: Option<bool>,
    ) -> Result<()> {
        let proxy = fho::testing::fake_proxy(move |req| match req {
            NightModeRequest::Set { .. } => {
                panic!("Unexpected call to set");
            }
            NightModeRequest::Watch { responder } => {
                let _ = responder.send(&NightModeSettings {
                    night_mode_enabled: expected_night_mode_enabled,
                    ..Default::default()
                });
            }
        });

        let output = utils::assert_watch!(command(proxy, None));
        assert_eq!(
            output,
            format!(
                "{:#?}",
                NightModeSettings {
                    night_mode_enabled: expected_night_mode_enabled,
                    ..Default::default()
                }
            )
        );
        Ok(())
    }
}
