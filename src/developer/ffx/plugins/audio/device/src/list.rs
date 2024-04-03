// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use ffx_audio_device_args::DeviceCommand;
use ffx_command::FfxContext;
use fidl_fuchsia_audio_device as fadevice;
use fidl_fuchsia_hardware_audio as fhaudio;
use fidl_fuchsia_io as fio;
use fuchsia_audio::device::{
    DevfsSelector, HardwareType as HardwareDeviceType, Selector, Type as DeviceType,
};
use serde::{Serialize, Serializer};
use std::fmt::Display;

/// A query that matches device properties against a [DeviceSelector].
pub struct DeviceQuery {
    pub id: Option<String>,
    pub device_type: Option<DeviceType>,
}

impl DeviceQuery {
    /// Returns true if this selector matches the query.
    ///
    /// A query matches when all Some fields are equal to the
    /// corresponding fields in the selector. None fields are ignored.
    pub fn matches(&self, selector: &Selector) -> bool {
        let mut is_match = true;
        match selector {
            Selector::Devfs(DevfsSelector(devfs)) => {
                if let Some(id) = &self.id {
                    is_match = is_match && (id == &devfs.id);
                }
                if let Some(device_type) = &self.device_type {
                    is_match = is_match && (device_type.0 == devfs.device_type);
                }
            }
        }
        is_match
    }
}

impl TryFrom<&DeviceCommand> for DeviceQuery {
    type Error = String;

    fn try_from(cmd: &DeviceCommand) -> Result<Self, Self::Error> {
        let id = cmd.id.clone();
        let device_type = cmd
            .device_type
            .map(|hw_type| DeviceType::try_from((hw_type, cmd.device_direction)))
            .transpose()?;
        Ok(Self { id, device_type })
    }
}

/// Output of the `ffx audio device list` command.
#[derive(Debug, Clone, Serialize)]
pub struct ListResult {
    pub devices: Vec<ListResultDevice>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ListResultDevice {
    device_id: String,
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

impl From<Vec<Selector>> for ListResult {
    fn from(value: Vec<Selector>) -> Self {
        Self { devices: value.into_iter().map(Into::into).collect() }
    }
}

impl From<Selector> for ListResultDevice {
    fn from(value: Selector) -> Self {
        let Selector::Devfs(devfs) = value;

        Self {
            device_id: devfs.0.id.clone(),
            // TODO(https://fxbug.dev/327490666): Fix incorrect STREAMCONFIG device_type
            device_type: HardwareDeviceType(fhaudio::DeviceType::StreamConfig),
            is_input: match devfs.0.device_type {
                fadevice::DeviceType::Input => Some(true),
                fadevice::DeviceType::Output => Some(false),
                _ => None,
            },
            path: Some(devfs.path().to_string()),
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
            write!(
                f,
                "{:?} Device id: {:?}, Device type: {}, {in_out}",
                device.path.as_ref().unwrap(),
                device.device_id,
                device.device_type
            )?;
        }
        Ok(())
    }
}

pub async fn get_devices(dev_class: &fio::DirectoryProxy) -> fho::Result<Vec<Selector>> {
    Ok(fuchsia_audio::device::list_devfs(dev_class)
        .await
        .bug_context("Failed to list devices in devfs")?
        .into_iter()
        .map(Selector::Devfs)
        .collect::<Vec<_>>())
}

#[cfg(test)]
mod test {
    use super::*;
    use fidl_fuchsia_audio_controller as fac;
    use fidl_fuchsia_audio_device as fadevice;
    use test_case::test_case;

    #[test_case(
        DeviceQuery {
            id: None,
            device_type: None
        };
        "empty"
    )]
    #[test_case(
        DeviceQuery {
            id: Some("some-id".to_string()),
            device_type: None
        };
        "id"
    )]
    #[test_case(
        DeviceQuery {
            id: None,
            device_type: Some(fadevice::DeviceType::Input.into())
        };
        "device type"
    )]
    #[test_case(
        DeviceQuery {
            id: Some("some-id".to_string()),
            device_type: Some(DeviceType::from(fadevice::DeviceType::Input))
        };
        "id and device type"
    )]
    fn test_query_matches(query: DeviceQuery) {
        let selector = Selector::from(fac::Devfs {
            id: "some-id".to_string(),
            device_type: fadevice::DeviceType::Input,
        });
        assert!(query.matches(&selector));
    }

    #[test_case(
        DeviceQuery {
            id: Some("incorrect".to_string()),
            device_type: None
        };
        "wrong id"
    )]
    #[test_case(
        DeviceQuery {
            id: None,
            device_type: Some(DeviceType::from(fadevice::DeviceType::Output))
        };
        "wrong device type"
    )]
    fn test_query_does_not_match(query: DeviceQuery) {
        let selector = Selector::from(fac::Devfs {
            id: "some-id".to_string(),
            device_type: fadevice::DeviceType::Input,
        });
        assert!(!query.matches(&selector));
    }
}
