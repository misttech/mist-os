// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use ffx_audio_device_args::DeviceCommand;
use fuchsia_audio::device::{DevfsSelector, Selector, Type as DeviceType};

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
