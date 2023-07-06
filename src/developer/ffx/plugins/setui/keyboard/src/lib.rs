// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::format_err;
use anyhow::Result;
use async_trait::async_trait;
use ffx_setui_keyboard_args::Keyboard;
use fho::{moniker, AvailabilityFlag, FfxMain, FfxTool, SimpleWriter};
use fidl_fuchsia_settings::{KeyboardProxy, KeyboardSettings};
use utils::handle_mixed_result;
use utils::{self, Either, WatchOrSetResult};

#[derive(FfxTool)]
#[check(AvailabilityFlag("setui"))]
pub struct KeyboardTool {
    #[command]
    cmd: Keyboard,
    #[with(moniker("/core/setui_service"))]
    keyboard_proxy: KeyboardProxy,
}

fho::embedded_plugin!(KeyboardTool);

#[async_trait(?Send)]
impl FfxMain for KeyboardTool {
    type Writer = SimpleWriter;
    async fn main(self, mut writer: Self::Writer) -> fho::Result<()> {
        run_command(self.keyboard_proxy, self.cmd, &mut writer).await?;
        Ok(())
    }
}

pub async fn run_command<W: std::io::Write>(
    keyboard_proxy: KeyboardProxy,
    keyboard: Keyboard,
    writer: &mut W,
) -> Result<()> {
    handle_mixed_result("Keyboard", command(keyboard_proxy, keyboard).await, writer).await
}

async fn command(proxy: KeyboardProxy, keyboard: Keyboard) -> WatchOrSetResult {
    if keyboard.autorepeat_delay.unwrap_or(0) < 0 || keyboard.autorepeat_period.unwrap_or(0) < 0 {
        return Err(format_err!("Negative values are invalid for autorepeat values."));
    }
    let settings = KeyboardSettings::from(keyboard);

    if settings == KeyboardSettings::default() {
        Ok(Either::Watch(utils::watch_to_stream(proxy, |p| p.watch())))
    } else {
        Ok(Either::Set(if let Err(err) = proxy.set(&settings).await? {
            format!("{:?}", err)
        } else {
            format!("Successfully set Keyboard to {:?}", keyboard)
        }))
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use fidl_fuchsia_settings::{KeyboardRequest, KeyboardSettings};
    use test_case::test_case;

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_run_command() {
        const NUM: i64 = 7;

        let proxy = fho::testing::fake_proxy(move |req| match req {
            KeyboardRequest::Set { responder, .. } => {
                let _ = responder.send(Ok(()));
            }
            KeyboardRequest::Watch { .. } => {
                panic!("Unexpected call to watch");
            }
        });

        let keyboard =
            Keyboard { keymap: None, autorepeat_delay: Some(NUM), autorepeat_period: Some(NUM) };
        let response = run_command(proxy, keyboard, &mut vec![]).await;
        assert!(response.is_ok());
    }

    #[test_case(
        Keyboard {
            keymap: Some(fidl_fuchsia_input::KeymapId::FrAzerty),
            autorepeat_delay: Some(-1),
            autorepeat_period: Some(-2),
        }; "Test keyboard invalid autorepeat inputs."
    )]
    #[fuchsia_async::run_singlethreaded(test)]
    async fn validate_keyboard_failure(expected_keyboard: Keyboard) -> Result<()> {
        let proxy = fho::testing::fake_proxy(move |req| match req {
            KeyboardRequest::Set { responder, .. } => {
                let _ = responder.send(Ok(()));
            }
            KeyboardRequest::Watch { responder } => {
                let _ = responder.send(&KeyboardSettings::from(expected_keyboard));
            }
        });

        let result = command(proxy, expected_keyboard).await;
        match result {
            Err(e) => {
                assert!(format!("{:?}", e)
                    .contains("Negative values are invalid for autorepeat values."))
            }
            _ => panic!("Should return errors."),
        }
        Ok(())
    }

    #[test_case(
        Keyboard {
            keymap: Some(fidl_fuchsia_input::KeymapId::UsQwerty),
            autorepeat_delay: Some(2),
            autorepeat_period: Some(3),
        }; "Test keyboard set() output."
    )]
    #[test_case(
        Keyboard {
            keymap: Some(fidl_fuchsia_input::KeymapId::UsDvorak),
            autorepeat_delay: Some(3),
            autorepeat_period: Some(4),
        }; "Test keyboard set() output with different values."
    )]
    #[fuchsia_async::run_singlethreaded(test)]
    async fn validate_keyboard_set_output(expected_keyboard: Keyboard) -> Result<()> {
        let proxy = fho::testing::fake_proxy(move |req| match req {
            KeyboardRequest::Set { responder, .. } => {
                let _ = responder.send(Ok(()));
            }
            KeyboardRequest::Watch { .. } => {
                panic!("Unexpected call to watch");
            }
        });

        let output = utils::assert_set!(command(proxy, expected_keyboard));
        assert_eq!(output, format!("Successfully set Keyboard to {:?}", expected_keyboard));
        Ok(())
    }

    #[test_case(
        Keyboard {
            keymap: None,
            autorepeat_delay: Some(0),
            autorepeat_period: Some(0),
        }; "Test keyboard watch() output with empty Keyboard."
    )]
    #[test_case(
        Keyboard {
            keymap: Some(fidl_fuchsia_input::KeymapId::UsDvorak),
            autorepeat_delay: Some(7),
            autorepeat_period: Some(8),
        }; "Test keyboard watch() output with non-empty Keyboard."
    )]
    #[fuchsia_async::run_singlethreaded(test)]
    async fn validate_keyboard_watch_output(expected_keyboard: Keyboard) -> Result<()> {
        let proxy = fho::testing::fake_proxy(move |req| match req {
            KeyboardRequest::Set { .. } => {
                panic!("Unexpected call to set");
            }
            KeyboardRequest::Watch { responder } => {
                let _ = responder.send(&KeyboardSettings::from(expected_keyboard));
            }
        });

        let output = utils::assert_watch!(command(
            proxy,
            Keyboard { keymap: None, autorepeat_delay: None, autorepeat_period: None }
        ));
        assert_eq!(output, format!("{:#?}", KeyboardSettings::from(expected_keyboard)));
        Ok(())
    }
}
