// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use ffx_audio_device_args::DeviceCommand;
use ffx_command_error::{FfxContext as _, Result};
use fuchsia_audio::device::{
    DevfsSelector, HardwareType as HardwareDeviceType, Info as DeviceInfo, Selector,
    Type as DeviceType,
};
use fuchsia_audio::Registry;
use serde::{Serialize, Serializer};
use std::fmt::Display;
use {
    fidl_fuchsia_audio_device as fadevice, fidl_fuchsia_hardware_audio as fhaudio,
    fidl_fuchsia_io as fio,
};

/// List of audio devices on the target.
///
/// This is not just `Vec<Selector>` because we need to keep more
/// detailed device info to match against command flags via [DeviceQuery].
/// For registry devices, the [RegistrySelector] only contains the
/// device's TokenId, so it's impossible to filter on device type, etc.
pub enum Devices {
    Devfs(Vec<DevfsSelector>),
    Registry(Vec<DeviceInfo>),
}

impl Devices {
    /// Returns the first device, if any.
    pub fn first(&self) -> Option<Selector> {
        match self {
            Devices::Devfs(selectors) => {
                selectors.first().map(|selector| Selector::Devfs(selector.clone()))
            }
            Devices::Registry(infos) => {
                infos.first().map(|info| Selector::Registry(info.registry_selector()))
            }
        }
    }
}

/// A query that matches device properties against a [DeviceSelector].
pub struct DeviceQuery {
    pub name: Option<String>,
    pub token_id: Option<fadevice::TokenId>,
    pub device_type: Option<DeviceType>,
}

pub trait QueryExt {
    /// Returns true if this value matches the query.
    ///
    /// A query matches when all Some fields are equal to the
    /// corresponding fields in the value. None query fields are ignored.
    fn matches(&self, query: &DeviceQuery) -> bool;
}

impl QueryExt for DevfsSelector {
    fn matches(&self, query: &DeviceQuery) -> bool {
        // Only registry devices have token IDs.
        if query.token_id.is_some() {
            return false;
        }
        let mut is_match = true;
        if let Some(name) = &query.name {
            is_match = is_match && (name == &self.0.name);
        }
        if let Some(device_type) = &query.device_type {
            is_match = is_match && (device_type.0 == self.0.device_type);
        }
        is_match
    }
}

impl QueryExt for DeviceInfo {
    fn matches(&self, query: &DeviceQuery) -> bool {
        let mut is_match = true;
        if let Some(name) = &query.name {
            is_match = is_match && (*name == self.device_name());
        }
        if let Some(token_id) = &query.token_id {
            is_match = is_match && (*token_id == self.token_id());
        }
        if let Some(device_type) = &query.device_type {
            is_match = is_match && (*device_type == self.device_type());
        }
        is_match
    }
}

impl TryFrom<&DeviceCommand> for DeviceQuery {
    type Error = String;

    fn try_from(cmd: &DeviceCommand) -> Result<Self, Self::Error> {
        let name = cmd.name.clone();
        let token_id = cmd.token_id.clone();
        let device_type = cmd
            .device_type
            .map(|hw_type| DeviceType::try_from((hw_type, cmd.device_direction)))
            .transpose()?;
        Ok(Self { name, token_id, device_type })
    }
}

/// Output of the `ffx audio device list` command.
#[derive(Debug, Clone, Serialize)]
pub struct ListResult {
    pub devices: Vec<ListResultDevice>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ListResultDevice {
    device_name: String,
    is_input: Option<bool>,
    #[serde(serialize_with = "serialize_hw_device_type")]
    device_type: HardwareDeviceType,
    path: Option<String>,
}

pub fn serialize_hw_device_type<S>(
    hw_device_type: &HardwareDeviceType,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    serializer.serialize_str(&hw_device_type.to_string().to_uppercase())
}

impl From<Devices> for ListResult {
    fn from(value: Devices) -> Self {
        let devices = match value {
            Devices::Devfs(selectors) => selectors.into_iter().map(Into::into).collect(),
            Devices::Registry(infos) => infos.into_iter().map(Into::into).collect(),
        };
        Self { devices }
    }
}

impl From<DevfsSelector> for ListResultDevice {
    fn from(value: DevfsSelector) -> Self {
        Self {
            device_name: value.0.name.clone(),
            // TODO(https://fxbug.dev/327490666): Fix incorrect STREAMCONFIG device_type
            device_type: HardwareDeviceType(fhaudio::DeviceType::StreamConfig),
            is_input: match value.0.device_type {
                fadevice::DeviceType::Input => Some(true),
                fadevice::DeviceType::Output => Some(false),
                _ => None,
            },
            path: Some(value.path().to_string()),
        }
    }
}

impl From<DeviceInfo> for ListResultDevice {
    fn from(value: DeviceInfo) -> Self {
        Self {
            device_type: HardwareDeviceType::from(value.device_type()),
            device_name: value.0.device_name.expect("missing 'device_name'"),
            is_input: value.0.is_input,
            path: None,
        }
    }
}

impl Display for ListResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.devices.is_empty() {
            return write!(f, "No devices found.");
        }
        let mut first = true;
        for device in &self.devices {
            if first {
                first = false;
            } else {
                writeln!(f)?;
            }
            let in_out = match device.is_input {
                Some(is_input) => {
                    if is_input {
                        "Input"
                    } else {
                        "Output"
                    }
                }
                None => "Input/Output not specified",
            };
            if let Some(path) = device.path.as_ref() {
                write!(f, "{:?} ", path)?;
            }
            write!(
                f,
                "Device name: {:?}, Device type: {}, {in_out}",
                device.device_name, device.device_type
            )?;
        }
        Ok(())
    }
}

/// Returns a list of devices on the target.
///
/// If the target is running audio_device_registry, i.e. when `registry` is Some, this method
/// returns registry devices.
///
/// Otherwise, this method returns devices from devfs.
pub async fn get_devices(
    dev_class: &fio::DirectoryProxy,
    registry: Option<&Registry>,
) -> Result<Devices> {
    // Try the registry first.
    if let Some(registry) = registry {
        let infos = registry.device_infos().await.into_values().collect();
        return Ok(Devices::Registry(infos));
    }

    // Fall back to devfs.
    let selectors = fuchsia_audio::device::list_devfs(dev_class)
        .await
        .bug_context("Failed to list devices in devfs")?;
    Ok(Devices::Devfs(selectors))
}

#[cfg(test)]
mod test {
    use super::*;
    use test_case::test_case;
    use {fidl_fuchsia_audio_controller as fac, fidl_fuchsia_audio_device as fadevice};

    #[test_case(
        DeviceQuery {
            name: None,
            token_id: None,
            device_type: None
        };
        "empty"
    )]
    #[test_case(
        DeviceQuery {
            name: Some("some-name".to_string()),
            token_id: None,
            device_type: None
        };
        "name"
    )]
    #[test_case(
        DeviceQuery {
            name: None,
            token_id: None,
            device_type: Some(fadevice::DeviceType::Input.into())
        };
        "device type"
    )]
    #[test_case(
        DeviceQuery {
            name: Some("some-name".to_string()),
            token_id: None,
            device_type: Some(DeviceType::from(fadevice::DeviceType::Input))
        };
        "name and device type"
    )]
    fn test_query_matches_devfsselector(query: DeviceQuery) {
        let selector = DevfsSelector(fac::Devfs {
            name: "some-name".to_string(),
            device_type: fadevice::DeviceType::Input,
        });
        assert!(selector.matches(&query));
    }

    #[test_case(
        DeviceQuery {
            name: Some("incorrect".to_string()),
            token_id: None,
            device_type: None
        };
        "wrong name"
    )]
    #[test_case(
        DeviceQuery {
            name: Some("some-name".to_string()),
            token_id: Some(123),
            device_type: Some(DeviceType::from(fadevice::DeviceType::Input))
        };
        "contains token id"
    )]
    #[test_case(
        DeviceQuery {
            name: None,
            token_id: None,
            device_type: Some(DeviceType::from(fadevice::DeviceType::Output))
        };
        "wrong device type"
    )]
    fn test_query_does_not_match_devfsselector(query: DeviceQuery) {
        let selector = DevfsSelector(fac::Devfs {
            name: "some-name".to_string(),
            device_type: fadevice::DeviceType::Input,
        });
        assert!(!selector.matches(&query));
    }

    #[test_case(
        DeviceQuery {
            name: None,
            token_id: None,
            device_type: None
        };
        "empty"
    )]
    #[test_case(
        DeviceQuery {
            name: None,
            token_id: Some(1),
            device_type: None
        };
        "token id"
    )]
    #[test_case(
        DeviceQuery {
            name: Some("some-name".to_string()),
            token_id: None,
            device_type: None
        };
        "name"
    )]
    #[test_case(
        DeviceQuery {
            name: Some("some-name".to_string()),
            token_id: Some(1),
            device_type: None
        };
        "token id and name"
    )]
    #[test_case(
        DeviceQuery {
            name: None,
            token_id: None,
            device_type: Some(fadevice::DeviceType::Input.into())
        };
        "device type"
    )]
    #[test_case(
        DeviceQuery {
            name: None,
            token_id: Some(1),
            device_type: Some(DeviceType::from(fadevice::DeviceType::Input))
        };
        "token id and device type"
    )]
    fn test_query_matches_deviceinfo(query: DeviceQuery) {
        let info = DeviceInfo::from(fadevice::Info {
            token_id: Some(1),
            device_name: Some("some-name".to_string()),
            device_type: Some(fadevice::DeviceType::Input),
            ..Default::default()
        });
        assert!(info.matches(&query));
    }

    #[test_case(
        DeviceQuery {
            name: None,
            token_id: Some(456),
            device_type: None
        };
        "wrong token id"
    )]
    #[test_case(
        DeviceQuery {
            name: Some("incorrect".to_string()),
            token_id: Some(1),
            device_type: Some(DeviceType::from(fadevice::DeviceType::Input))
        };
        "wrong name"
    )]
    #[test_case(
        DeviceQuery {
            name: None,
            token_id: None,
            device_type: Some(DeviceType::from(fadevice::DeviceType::Output))
        };
        "wrong device type"
    )]
    fn test_query_does_not_match_deviceinfo(query: DeviceQuery) {
        let info = DeviceInfo::from(fadevice::Info {
            token_id: Some(1),
            device_name: Some("some-name".to_string()),
            device_type: Some(fadevice::DeviceType::Input),
            ..Default::default()
        });
        assert!(!info.matches(&query));
    }
}
