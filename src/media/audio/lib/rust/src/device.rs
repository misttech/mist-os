// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use camino::Utf8PathBuf;
use fidl_fuchsia_audio_controller as fac;
use fidl_fuchsia_audio_device as fadevice;
use fidl_fuchsia_hardware_audio as fhaudio;
use std::fmt::Display;
use std::str::FromStr;

/// The type of an audio device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Type(pub fadevice::DeviceType);

impl Type {
    /// Returns the devfs class for this device.
    ///
    /// e.g. /dev/class/{class}/some_device
    pub fn devfs_class(&self) -> &str {
        match self.0 {
            fadevice::DeviceType::Input => "audio-input",
            fadevice::DeviceType::Output => "audio-output",
            fadevice::DeviceType::Dai => "dai",
            fadevice::DeviceType::Codec => "codec",
            fadevice::DeviceType::Composite => "audio-composite",
            _ => panic!("Unexpected device type"),
        }
    }
}

impl From<fadevice::DeviceType> for Type {
    fn from(value: fadevice::DeviceType) -> Self {
        Self(value)
    }
}

impl FromStr for Type {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let device_type = match s.to_lowercase().as_str() {
            "input" => Ok(fadevice::DeviceType::Input),
            "output" => Ok(fadevice::DeviceType::Output),
            "composite" => Ok(fadevice::DeviceType::Composite),
            _ => Err(format!(
                "Invalid device type: {}. Expected one of: Input, Output, Composite",
                s
            )),
        }?;

        Ok(Self(device_type))
    }
}

impl Display for Type {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self.0 {
            fadevice::DeviceType::Input => "Input",
            fadevice::DeviceType::Output => "Output",
            fadevice::DeviceType::Dai => "Dai",
            fadevice::DeviceType::Codec => "Codec",
            fadevice::DeviceType::Composite => "Composite",
            _ => "<unknown>",
        };
        f.write_str(s)
    }
}

impl TryFrom<(HardwareType, Option<Direction>)> for Type {
    type Error = String;

    fn try_from(value: (HardwareType, Option<Direction>)) -> Result<Self, Self::Error> {
        let (type_, direction) = value;
        let device_type = match type_.0 {
            fhaudio::DeviceType::StreamConfig => Ok(
                match direction
                    .ok_or_else(|| format!("direction is missing for StreamConfig type"))?
                {
                    Direction::Input => fadevice::DeviceType::Input,
                    Direction::Output => fadevice::DeviceType::Output,
                },
            ),
            fhaudio::DeviceType::Dai => Ok(fadevice::DeviceType::Dai),
            fhaudio::DeviceType::Codec => Ok(fadevice::DeviceType::Codec),
            fhaudio::DeviceType::Composite => Ok(fadevice::DeviceType::Composite),
            _ => Err(format!("unknown device type")),
        }?;
        Ok(Self(device_type))
    }
}

/// The type of an audio device driver.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HardwareType(pub fhaudio::DeviceType);

impl FromStr for HardwareType {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let device_type = match s.to_lowercase().as_str() {
            "streamconfig" => Ok(fhaudio::DeviceType::StreamConfig),
            "dai" => Ok(fhaudio::DeviceType::Dai),
            "codec" => Ok(fhaudio::DeviceType::Codec),
            "composite" => Ok(fhaudio::DeviceType::Composite),
            _ => Err(format!(
                "Invalid type: {}. Expected one of: StreamConfig, Dai, Codec, Composite",
                s
            )),
        }?;
        Ok(Self(device_type))
    }
}

impl Display for HardwareType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self.0 {
            fhaudio::DeviceType::StreamConfig => "StreamConfig",
            fhaudio::DeviceType::Dai => "Dai",
            fhaudio::DeviceType::Codec => "Codec",
            fhaudio::DeviceType::Composite => "Composite",
            _ => "<unknown>",
        };
        f.write_str(s)
    }
}

impl From<Type> for HardwareType {
    fn from(value: Type) -> Self {
        let hw_type = match value.0 {
            fadevice::DeviceType::Input | fadevice::DeviceType::Output => {
                fhaudio::DeviceType::StreamConfig
            }
            fadevice::DeviceType::Dai => fhaudio::DeviceType::Dai,
            fadevice::DeviceType::Codec => fhaudio::DeviceType::Codec,
            fadevice::DeviceType::Composite => fhaudio::DeviceType::Composite,
            _ => panic!("Unexpected device type"),
        };
        Self(hw_type)
    }
}

/// The direction in which audio flows through a device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Device is a source of streamed audio.
    Input,

    /// Device is a destination for streamed audio.
    Output,
}

impl FromStr for Direction {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "input" => Ok(Self::Input),
            "output" => Ok(Self::Output),
            _ => Err(format!("Invalid direction: {}. Expected one of: input, output", s)),
        }
    }
}

/// Identifies a single audio device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Selector {
    Devfs(DevfsSelector),
}

impl TryFrom<fac::DeviceSelector> for Selector {
    type Error = String;

    fn try_from(value: fac::DeviceSelector) -> Result<Self, Self::Error> {
        match value {
            fac::DeviceSelector::Devfs(devfs) => Ok(Self::Devfs(devfs.into())),
            _ => Err("unknown selector variant".to_string()),
        }
    }
}

impl From<fac::Devfs> for Selector {
    fn from(value: fac::Devfs) -> Self {
        Self::Devfs(value.into())
    }
}

impl From<Selector> for fac::DeviceSelector {
    fn from(value: Selector) -> Self {
        match value {
            Selector::Devfs(devfs_selector) => devfs_selector.into(),
        }
    }
}

/// Identifies a device backed by a hardware driver protocol in devfs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DevfsSelector(pub fac::Devfs);

impl DevfsSelector {
    /// Returns the full devfs path for this device.
    pub fn path(&self) -> Utf8PathBuf {
        Utf8PathBuf::from("/dev/class")
            .join(Type(self.0.device_type).devfs_class())
            .join(self.0.id.clone())
    }
}

impl Display for DevfsSelector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.path().as_str())
    }
}

impl TryFrom<fac::DeviceSelector> for DevfsSelector {
    type Error = String;

    fn try_from(value: fac::DeviceSelector) -> Result<Self, Self::Error> {
        match value {
            fac::DeviceSelector::Devfs(devfs) => Ok(Self(devfs.into())),
            _ => Err("unknown selector type".to_string()),
        }
    }
}

impl From<fac::Devfs> for DevfsSelector {
    fn from(value: fac::Devfs) -> Self {
        Self(value)
    }
}

impl From<DevfsSelector> for fac::DeviceSelector {
    fn from(value: DevfsSelector) -> Self {
        Self::Devfs(value.0)
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use test_case::test_case;

    #[test_case("input", fadevice::DeviceType::Input; "input")]
    #[test_case("output", fadevice::DeviceType::Output; "output")]
    #[test_case("composite", fadevice::DeviceType::Composite; "composite")]
    fn test_parse_type(s: &str, expected_type: fadevice::DeviceType) {
        assert_eq!(Type(expected_type), s.parse::<Type>().unwrap());
    }

    #[test]
    fn test_parse_type_invalid() {
        assert!("not a valid device type".parse::<Type>().is_err());
    }

    #[test_case("StreamConfig", fhaudio::DeviceType::StreamConfig; "StreamConfig")]
    #[test_case("Dai", fhaudio::DeviceType::Dai; "Dai")]
    #[test_case("Codec", fhaudio::DeviceType::Codec; "Codec")]
    #[test_case("Composite", fhaudio::DeviceType::Composite; "Composite")]
    fn test_parse_hardware_type(s: &str, expected_type: fhaudio::DeviceType) {
        assert_eq!(HardwareType(expected_type), s.parse::<HardwareType>().unwrap());
    }

    #[test]
    fn test_parse_hardware_type_invalid() {
        assert!("not a valid hardware device type".parse::<Type>().is_err());
    }

    #[test_case(
        fhaudio::DeviceType::StreamConfig,
        Some(Direction::Input),
        fadevice::DeviceType::Input;
        "StreamConfig input"
    )]
    #[test_case(
        fhaudio::DeviceType::StreamConfig,
        Some(Direction::Output),
        fadevice::DeviceType::Output;
        "StreamConfig output"
    )]
    #[test_case(fhaudio::DeviceType::Dai, None, fadevice::DeviceType::Dai; "Dai")]
    #[test_case(fhaudio::DeviceType::Codec, None, fadevice::DeviceType::Codec; "Codec")]
    #[test_case(fhaudio::DeviceType::Composite, None, fadevice::DeviceType::Composite; "Composite")]
    fn test_from_hardware_type_with_direction(
        hardware_type: fhaudio::DeviceType,
        direction: Option<Direction>,
        expected_type: fadevice::DeviceType,
    ) {
        assert_eq!(
            Type(expected_type),
            (HardwareType(hardware_type), direction).try_into().unwrap()
        )
    }

    #[test_case(
        fac::Devfs { id: "3d99d780".to_string(), device_type: fadevice::DeviceType::Input },
        "/dev/class/audio-input/3d99d780";
        "input"
    )]
    #[test_case(
        fac::Devfs { id: "3d99d780".to_string(), device_type: fadevice::DeviceType::Output },
        "/dev/class/audio-output/3d99d780";
        "output"
    )]
    #[test_case(
        fac::Devfs { id: "3d99d780".to_string(), device_type: fadevice::DeviceType::Dai },
        "/dev/class/dai/3d99d780";
        "dai"
    )]
    #[test_case(
        fac::Devfs { id: "3d99d780".to_string(), device_type: fadevice::DeviceType::Codec },
        "/dev/class/codec/3d99d780";
        "codec"
    )]
    #[test_case(
        fac::Devfs { id: "3d99d780".to_string(), device_type: fadevice::DeviceType::Composite },
        "/dev/class/audio-composite/3d99d780";
        "composite"
    )]
    fn test_devfs_selector_path(devfs: fac::Devfs, expected_path: &str) {
        assert_eq!(expected_path, DevfsSelector(devfs).path());
    }
}
