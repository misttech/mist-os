// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use crate::base::{SettingInfo, SettingType};
use crate::display::types::{LowLightMode, SetDisplayInfo, Theme, ThemeMode, ThemeType};
use crate::handler::base::Request;
use crate::ingress::{request, watch, Scoped};
use crate::job::source::{Error as JobError, ErrorResponder};
use crate::job::Job;
use fidl::endpoints::{ControlHandle, Responder};
use fidl_fuchsia_settings::{
    DisplayRequest, DisplaySetResponder, DisplaySetResult, DisplaySettings, DisplayWatchResponder,
    LowLightMode as FidlLowLightMode, Theme as FidlTheme, ThemeMode as FidlThemeMode,
    ThemeType as FidlThemeType,
};

use std::convert::TryFrom;

impl From<FidlThemeMode> for ThemeMode {
    fn from(fidl: FidlThemeMode) -> Self {
        ThemeMode::from_bits(FidlThemeMode::bits(&fidl))
            .expect("failed to convert FidlThemeMode to ThemeMode")
    }
}

impl From<ThemeMode> for FidlThemeMode {
    fn from(fidl: ThemeMode) -> Self {
        FidlThemeMode::from_bits(ThemeMode::bits(&fidl))
            .expect("failed to convert ThemeMode to FidlThemeMode")
    }
}

impl From<FidlLowLightMode> for LowLightMode {
    fn from(fidl_low_light_mode: FidlLowLightMode) -> Self {
        match fidl_low_light_mode {
            FidlLowLightMode::Disable => LowLightMode::Disable,
            FidlLowLightMode::DisableImmediately => LowLightMode::DisableImmediately,
            FidlLowLightMode::Enable => LowLightMode::Enable,
        }
    }
}

impl From<FidlThemeType> for ThemeType {
    fn from(fidl_theme_type: FidlThemeType) -> Self {
        match fidl_theme_type {
            FidlThemeType::Default => ThemeType::Default,
            FidlThemeType::Light => ThemeType::Light,
            FidlThemeType::Dark => ThemeType::Dark,
        }
    }
}

impl From<FidlTheme> for Theme {
    fn from(fidl_theme: FidlTheme) -> Self {
        Self {
            theme_type: fidl_theme.theme_type.map(Into::into),
            theme_mode: fidl_theme.theme_mode.map(Into::into).unwrap_or_else(ThemeMode::empty),
        }
    }
}

impl From<SettingInfo> for DisplaySettings {
    fn from(response: SettingInfo) -> Self {
        if let SettingInfo::Brightness(info) = response {
            fidl_fuchsia_settings::DisplaySettings {
                auto_brightness: Some(info.auto_brightness),
                adjusted_auto_brightness: Some(info.auto_brightness_value),
                brightness_value: Some(info.manual_brightness_value),
                screen_enabled: Some(info.screen_enabled),
                low_light_mode: Some(match info.low_light_mode {
                    LowLightMode::Enable => FidlLowLightMode::Enable,
                    LowLightMode::Disable => FidlLowLightMode::Disable,
                    LowLightMode::DisableImmediately => FidlLowLightMode::DisableImmediately,
                }),
                theme: Some(FidlTheme {
                    theme_type: match info.theme {
                        Some(Theme { theme_type: Some(theme_type), .. }) => match theme_type {
                            ThemeType::Unknown => None,
                            ThemeType::Default => Some(FidlThemeType::Default),
                            ThemeType::Light => Some(FidlThemeType::Light),
                            ThemeType::Dark => Some(FidlThemeType::Dark),
                        },
                        _ => None,
                    },
                    theme_mode: match info.theme {
                        Some(Theme { theme_mode, .. }) if !theme_mode.is_empty() => {
                            Some(FidlThemeMode::from(theme_mode))
                        }
                        _ => None,
                    },
                    ..Default::default()
                }),
                ..Default::default()
            }
        } else {
            panic!("incorrect value sent to display");
        }
    }
}

impl ErrorResponder for DisplaySetResponder {
    fn id(&self) -> &'static str {
        "Display_Set"
    }

    fn respond(self: Box<Self>, error: fidl_fuchsia_settings::Error) -> Result<(), fidl::Error> {
        self.send(Err(error))
    }
}

impl request::Responder<Scoped<DisplaySetResult>> for DisplaySetResponder {
    fn respond(self, Scoped(response): Scoped<DisplaySetResult>) {
        let _ = self.send(response);
    }
}

impl watch::Responder<DisplaySettings, fuchsia_zircon::Status> for DisplayWatchResponder {
    fn respond(self, response: Result<DisplaySettings, fuchsia_zircon::Status>) {
        match response {
            Ok(settings) => {
                let _ = self.send(&settings);
            }
            Err(error) => {
                self.control_handle().shutdown_with_epitaph(error);
            }
        }
    }
}

fn to_request(settings: DisplaySettings) -> Option<Request> {
    let set_display_info = SetDisplayInfo {
        manual_brightness_value: settings.brightness_value,
        auto_brightness_value: settings.adjusted_auto_brightness,
        auto_brightness: settings.auto_brightness,
        screen_enabled: settings.screen_enabled,
        low_light_mode: settings.low_light_mode.map(Into::into),
        theme: settings.theme.map(Into::into),
    };
    match set_display_info {
        // No values being set is invalid
        SetDisplayInfo {
            manual_brightness_value: None,
            auto_brightness_value: None,
            auto_brightness: None,
            screen_enabled: None,
            low_light_mode: None,
            theme: None,
        } => None,
        _ => Some(Request::SetDisplayInfo(set_display_info)),
    }
}

impl TryFrom<DisplayRequest> for Job {
    type Error = JobError;
    fn try_from(req: DisplayRequest) -> Result<Self, Self::Error> {
        // Support future expansion of FIDL
        #[allow(unreachable_patterns)]
        match req {
            DisplayRequest::Set { settings, responder } => match to_request(settings) {
                Some(request) => {
                    Ok(request::Work::new(SettingType::Display, request, responder).into())
                }
                None => Err(JobError::InvalidInput(Box::new(responder))),
            },
            DisplayRequest::Watch { responder } => {
                Ok(watch::Work::new_job(SettingType::Display, responder))
            }
            _ => {
                tracing::warn!("Received a call to an unsupported API: {:?}", req);
                Err(JobError::Unsupported)
            }
        }
    }
}
