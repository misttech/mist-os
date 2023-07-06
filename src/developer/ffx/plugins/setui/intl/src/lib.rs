// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Result;
use async_trait::async_trait;
use ffx_setui_intl_args::Intl;
use fho::{moniker, AvailabilityFlag, FfxMain, FfxTool, SimpleWriter};
use fidl_fuchsia_settings::{IntlProxy, IntlSettings};
use utils::handle_mixed_result;
use utils::{self, Either, WatchOrSetResult};

#[derive(FfxTool)]
#[check(AvailabilityFlag("setui"))]
pub struct IntlTool {
    #[command]
    cmd: Intl,
    #[with(moniker("/core/setui_service"))]
    intl_proxy: IntlProxy,
}

fho::embedded_plugin!(IntlTool);

#[async_trait(?Send)]
impl FfxMain for IntlTool {
    type Writer = SimpleWriter;
    async fn main(self, mut writer: Self::Writer) -> fho::Result<()> {
        run_command(self.intl_proxy, self.cmd, &mut writer).await?;
        Ok(())
    }
}

async fn run_command<W: std::io::Write>(
    intl_proxy: IntlProxy,
    intl: Intl,
    writer: &mut W,
) -> Result<()> {
    handle_mixed_result("Intl", command(intl_proxy, IntlSettings::from(intl)).await, writer).await
}

async fn command(proxy: IntlProxy, settings: IntlSettings) -> WatchOrSetResult {
    if settings == IntlSettings::default() {
        Ok(Either::Watch(utils::watch_to_stream(proxy, |p| p.watch())))
    } else {
        Ok(Either::Set(if let Err(err) = proxy.set(&settings).await? {
            format!("{:?}", err)
        } else {
            format!("Successfully set Intl to {:?}", Intl::from(settings))
        }))
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use fidl_fuchsia_intl::{LocaleId, TemperatureUnit, TimeZoneId};
    use fidl_fuchsia_settings::{HourCycle, IntlRequest};
    use test_case::test_case;

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_run_command() {
        let proxy = fho::testing::fake_proxy(move |req| match req {
            IntlRequest::Set { responder, .. } => {
                let _ = responder.send(Ok(()));
            }
            IntlRequest::Watch { .. } => {
                panic!("Unexpected call to watch");
            }
        });

        let intl = Intl {
            time_zone: None,
            temperature_unit: Some(TemperatureUnit::Celsius),
            locales: vec![],
            hour_cycle: None,
            clear_locales: false,
        };
        let response = run_command(proxy, intl, &mut vec![]).await;
        assert!(response.is_ok());
    }

    #[test_case(
        Intl {
            time_zone: Some(TimeZoneId { id: "GMT".to_string() }),
            temperature_unit: Some(TemperatureUnit::Celsius),
            locales: vec![LocaleId { id: "fr-u-hc-h12".into() }],
            hour_cycle: Some(HourCycle::H12),
            clear_locales: false,
        };
        "Test intl set() output with non-empty input."
    )]
    #[test_case(
        Intl {
            time_zone: Some(TimeZoneId { id: "GMT".to_string() }),
            temperature_unit: Some(TemperatureUnit::Fahrenheit),
            locales: vec![],
            hour_cycle: Some(HourCycle::H24),
            clear_locales: true,
        };
        "Test intl set() output with a different non-empty input."
    )]
    #[fuchsia_async::run_singlethreaded(test)]
    async fn validate_intl_set_output(expected_intl: Intl) -> Result<()> {
        let proxy = fho::testing::fake_proxy(move |req| match req {
            IntlRequest::Set { responder, .. } => {
                let _ = responder.send(Ok(()));
            }
            IntlRequest::Watch { .. } => {
                panic!("Unexpected call to watch");
            }
        });

        let output = utils::assert_set!(command(proxy, IntlSettings::from(expected_intl.clone())));
        assert_eq!(output, format!("Successfully set Intl to {:?}", expected_intl));
        Ok(())
    }

    #[test_case(
        Intl {
            time_zone: None,
            temperature_unit: None,
            locales: vec![],
            hour_cycle: None,
            clear_locales: false,
        };
        "Test intl watch() output with empty input."
    )]
    #[test_case(
        Intl {
            time_zone: Some(TimeZoneId { id: "GMT".to_string() }),
            temperature_unit: Some(TemperatureUnit::Celsius),
            locales: vec![LocaleId { id: "fr-u-hc-h12".into() }],
            hour_cycle: Some(HourCycle::H12),
            clear_locales: false,
        };
        "Test intl watch() output with non-empty input."
    )]
    #[fuchsia_async::run_singlethreaded(test)]
    async fn validate_intl_watch_output(expected_intl: Intl) -> Result<()> {
        let expected_intl_clone = expected_intl.clone();
        let proxy = fho::testing::fake_proxy(move |req| match req {
            IntlRequest::Set { .. } => {
                panic!("Unexpected call to set");
            }
            IntlRequest::Watch { responder } => {
                let _ = responder.send(&IntlSettings::from(expected_intl.clone()));
            }
        });

        let output = utils::assert_watch!(command(
            proxy,
            IntlSettings::from(Intl {
                time_zone: None,
                temperature_unit: None,
                locales: vec![],
                hour_cycle: None,
                clear_locales: false,
            })
        ));
        assert_eq!(output, format!("{:#?}", IntlSettings::from(expected_intl_clone)));
        Ok(())
    }
}
