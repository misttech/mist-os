// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::dai::DaiFormatSet;
use crate::format_set::PcmFormatSet;
use camino::Utf8PathBuf;
use std::collections::BTreeMap;
use std::fmt::Display;
use std::str::FromStr;
use thiserror::Error;
use {
    fidl_fuchsia_audio_controller as fac, fidl_fuchsia_audio_device as fadevice,
    fidl_fuchsia_hardware_audio as fhaudio, fidl_fuchsia_io as fio, zx_types,
};

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
#[derive(Debug, Clone, Hash, PartialEq, Eq, PartialOrd, Ord)]
pub enum Selector {
    Devfs(DevfsSelector),
    Registry(RegistrySelector),
}

impl TryFrom<fac::DeviceSelector> for Selector {
    type Error = String;

    fn try_from(value: fac::DeviceSelector) -> Result<Self, Self::Error> {
        match value {
            fac::DeviceSelector::Devfs(devfs) => Ok(Self::Devfs(devfs.into())),
            fac::DeviceSelector::Registry(token_id) => Ok(Self::Registry(token_id.into())),
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
            Selector::Registry(registry_selector) => registry_selector.into(),
        }
    }
}

/// Identifies a device backed by a hardware driver protocol in devfs.
#[derive(Debug, Clone, Hash, PartialEq, Eq, PartialOrd, Ord)]
pub struct DevfsSelector(pub fac::Devfs);

impl DevfsSelector {
    /// Returns the full devfs path for this device.
    pub fn path(&self) -> Utf8PathBuf {
        Utf8PathBuf::from("/dev/class").join(self.relative_path())
    }

    /// Returns the path for this device relative to the /dev/class directory root.
    pub fn relative_path(&self) -> Utf8PathBuf {
        Utf8PathBuf::from(self.device_type().devfs_class()).join(self.0.name.clone())
    }

    /// Returns the type of this device.
    pub fn device_type(&self) -> Type {
        Type(self.0.device_type)
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

/// Identifies a device available through the `fuchsia.audio.device/Registry` protocol.
#[derive(Debug, Clone, Hash, PartialEq, Eq, PartialOrd, Ord)]
pub struct RegistrySelector(pub fadevice::TokenId);

impl RegistrySelector {
    pub fn token_id(&self) -> fadevice::TokenId {
        self.0
    }
}

impl Display for RegistrySelector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl TryFrom<fac::DeviceSelector> for RegistrySelector {
    type Error = String;

    fn try_from(value: fac::DeviceSelector) -> Result<Self, Self::Error> {
        match value {
            fac::DeviceSelector::Registry(token_id) => Ok(Self(token_id)),
            _ => Err("unknown selector type".to_string()),
        }
    }
}

impl From<fadevice::TokenId> for RegistrySelector {
    fn from(value: fadevice::TokenId) -> Self {
        Self(value)
    }
}

impl From<RegistrySelector> for fac::DeviceSelector {
    fn from(value: RegistrySelector) -> Self {
        Self::Registry(value.0)
    }
}

/// Device info from the `fuchsia.audio.device/Registry` protocol.
#[derive(Debug, Clone, PartialEq)]
pub struct Info(pub fadevice::Info);

impl Info {
    pub fn token_id(&self) -> fadevice::TokenId {
        self.0.token_id.expect("missing 'token_id'")
    }

    pub fn registry_selector(&self) -> RegistrySelector {
        RegistrySelector(self.token_id())
    }

    pub fn device_type(&self) -> Type {
        Type(self.0.device_type.expect("missing 'device_type'"))
    }

    pub fn device_name(&self) -> &str {
        self.0.device_name.as_ref().expect("missing 'device_name'")
    }

    pub fn unique_instance_id(&self) -> Option<UniqueInstanceId> {
        self.0.unique_instance_id.map(UniqueInstanceId)
    }

    pub fn plug_detect_capabilities(&self) -> Option<PlugDetectCapabilities> {
        self.0.plug_detect_caps.map(PlugDetectCapabilities::from)
    }

    pub fn gain_capabilities(&self) -> Option<GainCapabilities> {
        self.0.gain_caps.clone().and_then(|gain_caps| GainCapabilities::try_from(gain_caps).ok())
    }

    pub fn clock_domain(&self) -> Option<ClockDomain> {
        self.0.clock_domain.map(ClockDomain)
    }

    pub fn supported_ring_buffer_formats(
        &self,
    ) -> Result<BTreeMap<fadevice::ElementId, Vec<PcmFormatSet>>, String> {
        self.0
            .ring_buffer_format_sets
            .as_ref()
            .map_or_else(
                || Ok(BTreeMap::new()),
                |element_rb_format_sets| {
                    element_rb_format_sets
                        .iter()
                        .cloned()
                        .map(|element_rb_format_set| {
                            let element_id = element_rb_format_set
                                .element_id
                                .ok_or_else(|| "missing element_id".to_string())?;
                            let fidl_format_sets = element_rb_format_set
                                .format_sets
                                .ok_or_else(|| "missing format_sets".to_string())?;

                            let format_sets: Vec<PcmFormatSet> = fidl_format_sets
                                .into_iter()
                                .map(TryInto::try_into)
                                .collect::<Result<Vec<_>, _>>()
                                .map_err(|err| format!("invalid format set: {}", err))?;

                            Ok((element_id, format_sets))
                        })
                        .collect::<Result<BTreeMap<_, _>, String>>()
                },
            )
            .map_err(|err| format!("invalid ring buffer format sets: {}", err))
    }

    pub fn supported_dai_formats(
        &self,
    ) -> Result<BTreeMap<fadevice::ElementId, Vec<DaiFormatSet>>, String> {
        self.0
            .dai_format_sets
            .as_ref()
            .map_or_else(
                || Ok(BTreeMap::new()),
                |element_dai_format_sets| {
                    element_dai_format_sets
                        .iter()
                        .cloned()
                        .map(|element_dai_format_set| {
                            let element_id = element_dai_format_set
                                .element_id
                                .ok_or_else(|| "missing element_id".to_string())?;
                            let fidl_format_sets = element_dai_format_set
                                .format_sets
                                .ok_or_else(|| "missing format_sets".to_string())?;

                            let dai_format_sets: Vec<DaiFormatSet> = fidl_format_sets
                                .into_iter()
                                .map(TryInto::try_into)
                                .collect::<Result<Vec<_>, _>>()
                                .map_err(|err| format!("invalid DAI format set: {}", err))?;

                            Ok((element_id, dai_format_sets))
                        })
                        .collect::<Result<BTreeMap<_, _>, String>>()
                },
            )
            .map_err(|err| format!("invalid ring buffer format sets: {}", err))
    }
}

impl From<fadevice::Info> for Info {
    fn from(value: fadevice::Info) -> Self {
        Self(value)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct UniqueInstanceId(pub [u8; fadevice::UNIQUE_INSTANCE_ID_SIZE as usize]);

impl Display for UniqueInstanceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for byte in self.0 {
            write!(f, "{:02x}", byte)?;
        }
        Ok(())
    }
}

impl From<[u8; fadevice::UNIQUE_INSTANCE_ID_SIZE as usize]> for UniqueInstanceId {
    fn from(value: [u8; fadevice::UNIQUE_INSTANCE_ID_SIZE as usize]) -> Self {
        Self(value)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct PlugDetectCapabilities(pub fadevice::PlugDetectCapabilities);

impl Display for PlugDetectCapabilities {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self.0 {
            fadevice::PlugDetectCapabilities::Hardwired => "Hardwired",
            fadevice::PlugDetectCapabilities::Pluggable => "Pluggable (can async notify)",
            _ => "<unknown>",
        };
        f.write_str(s)
    }
}

impl From<fadevice::PlugDetectCapabilities> for PlugDetectCapabilities {
    fn from(value: fadevice::PlugDetectCapabilities) -> Self {
        Self(value)
    }
}

impl From<PlugDetectCapabilities> for fadevice::PlugDetectCapabilities {
    fn from(value: PlugDetectCapabilities) -> Self {
        value.0
    }
}

impl From<fhaudio::PlugDetectCapabilities> for PlugDetectCapabilities {
    fn from(value: fhaudio::PlugDetectCapabilities) -> Self {
        let plug_detect_caps = match value {
            fhaudio::PlugDetectCapabilities::Hardwired => {
                fadevice::PlugDetectCapabilities::Hardwired
            }
            fhaudio::PlugDetectCapabilities::CanAsyncNotify => {
                fadevice::PlugDetectCapabilities::Pluggable
            }
        };
        Self(plug_detect_caps)
    }
}

impl TryFrom<PlugDetectCapabilities> for fhaudio::PlugDetectCapabilities {
    type Error = String;

    fn try_from(value: PlugDetectCapabilities) -> Result<Self, Self::Error> {
        match value.0 {
            fadevice::PlugDetectCapabilities::Hardwired => Ok(Self::Hardwired),
            fadevice::PlugDetectCapabilities::Pluggable => Ok(Self::CanAsyncNotify),
            _ => Err("unsupported PlugDetectCapabilities value".to_string()),
        }
    }
}

/// Describes the plug state of a device or endpoint, and when it changed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlugEvent {
    pub state: PlugState,

    /// The Zircon monotonic time when the plug state changed.
    pub time: zx_types::zx_time_t,
}

impl From<(fadevice::PlugState, i64 /* time */)> for PlugEvent {
    fn from(value: (fadevice::PlugState, i64)) -> Self {
        let (state, time) = value;
        Self { state: state.into(), time }
    }
}

impl TryFrom<fhaudio::PlugState> for PlugEvent {
    type Error = String;

    fn try_from(value: fhaudio::PlugState) -> Result<Self, Self::Error> {
        let plugged = value.plugged.ok_or_else(|| "missing 'plugged'".to_string())?;
        let time = value.plug_state_time.ok_or_else(|| "missing 'plug_state_time'".to_string())?;
        let state = PlugState(if plugged {
            fadevice::PlugState::Plugged
        } else {
            fadevice::PlugState::Unplugged
        });
        Ok(Self { state, time })
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlugState(pub fadevice::PlugState);

impl Display for PlugState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self.0 {
            fadevice::PlugState::Plugged => "Plugged",
            fadevice::PlugState::Unplugged => "Unplugged",
            _ => "<unknown>",
        };
        f.write_str(s)
    }
}

impl From<fadevice::PlugState> for PlugState {
    fn from(value: fadevice::PlugState) -> Self {
        Self(value)
    }
}

impl From<PlugState> for fadevice::PlugState {
    fn from(value: PlugState) -> Self {
        value.0
    }
}

// TODO(https://fxbug.dev/102027): Remove legacy gain aspects once driver API does.
// Going forward, gain will be handled by `SignalProcessing`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GainState {
    pub gain_db: f32,
    pub muted: Option<bool>,
    pub agc_enabled: Option<bool>,
}

impl TryFrom<fadevice::GainState> for GainState {
    type Error = String;

    fn try_from(value: fadevice::GainState) -> Result<Self, Self::Error> {
        Ok(Self {
            gain_db: value.gain_db.ok_or_else(|| "missing 'gain_db'".to_string())?,
            muted: value.muted,
            agc_enabled: value.agc_enabled,
        })
    }
}

impl TryFrom<fhaudio::GainState> for GainState {
    type Error = String;

    fn try_from(value: fhaudio::GainState) -> Result<Self, Self::Error> {
        Ok(Self {
            gain_db: value.gain_db.ok_or_else(|| "missing 'gain_db'".to_string())?,
            muted: value.muted,
            agc_enabled: value.agc_enabled,
        })
    }
}

// TODO(https://fxbug.dev/102027): Remove legacy gain aspects once driver API does.
// Going forward, gain will be handled by `SignalProcessing`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GainCapabilities {
    pub min_gain_db: f32,
    pub max_gain_db: f32,
    pub gain_step_db: f32,
    pub can_mute: Option<bool>,
    pub can_agc: Option<bool>,
}

impl TryFrom<fadevice::GainCapabilities> for GainCapabilities {
    type Error = String;

    fn try_from(value: fadevice::GainCapabilities) -> Result<Self, Self::Error> {
        Ok(Self {
            min_gain_db: value.min_gain_db.ok_or_else(|| "missing 'min_gain_db'".to_string())?,
            max_gain_db: value.max_gain_db.ok_or_else(|| "missing 'max_gain_db'".to_string())?,
            gain_step_db: value.gain_step_db.ok_or_else(|| "missing 'gain_step_db'".to_string())?,
            can_mute: value.can_mute,
            can_agc: value.can_agc,
        })
    }
}

impl TryFrom<&fhaudio::StreamProperties> for GainCapabilities {
    type Error = String;

    fn try_from(value: &fhaudio::StreamProperties) -> Result<Self, Self::Error> {
        Ok(Self {
            min_gain_db: value.min_gain_db.ok_or_else(|| "missing 'min_gain_db'".to_string())?,
            max_gain_db: value.max_gain_db.ok_or_else(|| "missing 'max_gain_db'".to_string())?,
            gain_step_db: value.gain_step_db.ok_or_else(|| "missing 'gain_step_db'".to_string())?,
            can_mute: value.can_mute,
            can_agc: value.can_agc,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClockDomain(pub fhaudio::ClockDomain);

impl Display for ClockDomain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)?;
        match self.0 {
            fhaudio::CLOCK_DOMAIN_MONOTONIC => f.write_str(" (monotonic)"),
            fhaudio::CLOCK_DOMAIN_EXTERNAL => f.write_str(" (external)"),
            _ => Ok(()),
        }
    }
}

impl From<fhaudio::ClockDomain> for ClockDomain {
    fn from(value: fhaudio::ClockDomain) -> Self {
        Self(value)
    }
}

impl From<ClockDomain> for fhaudio::ClockDomain {
    fn from(value: ClockDomain) -> Self {
        value.0
    }
}

#[derive(Error, Debug)]
pub enum ListDevfsError {
    #[error("Failed to open directory {}: {:?}", name, err)]
    Open {
        name: String,
        #[source]
        err: fuchsia_fs::node::OpenError,
    },

    #[error("Failed to read directory {} entries: {:?}", name, err)]
    Readdir {
        name: String,
        #[source]
        err: fuchsia_fs::directory::EnumerateError,
    },
}

/// Returns selectors for all audio devices in devfs.
///
/// `dev_class` should be a proxy to the `/dev/class` directory.
pub async fn list_devfs(
    dev_class: &fio::DirectoryProxy,
) -> Result<Vec<DevfsSelector>, ListDevfsError> {
    const TYPES: &[Type] = &[
        Type(fadevice::DeviceType::Input),
        Type(fadevice::DeviceType::Output),
        Type(fadevice::DeviceType::Dai),
        Type(fadevice::DeviceType::Codec),
        Type(fadevice::DeviceType::Composite),
    ];

    let mut selectors = vec![];

    for device_type in TYPES {
        let subdir_name = device_type.devfs_class();
        let subdir =
            fuchsia_fs::directory::open_directory(dev_class, subdir_name, fio::Flags::empty())
                .await
                .map_err(|err| ListDevfsError::Open { name: subdir_name.to_string(), err })?;
        let entries = fuchsia_fs::directory::readdir(&subdir)
            .await
            .map_err(|err| ListDevfsError::Readdir { name: subdir_name.to_string(), err })?;
        selectors.extend(entries.into_iter().map(|entry| {
            DevfsSelector(fac::Devfs { name: entry.name, device_type: device_type.0 })
        }));
    }

    Ok(selectors)
}

#[derive(Error, Debug)]
pub enum ListRegistryError {
    #[error(transparent)]
    Fidl(#[from] fidl::Error),

    #[error("failed to get devices: {:?}", .0)]
    WatchDevicesAdded(fadevice::RegistryWatchDevicesAddedError),
}

/// Returns info for all audio devices in the `fuchsia.audio.device` registry.
pub async fn list_registry(
    registry: &fadevice::RegistryProxy,
) -> Result<Vec<Info>, ListRegistryError> {
    Ok(registry
        .watch_devices_added()
        .await
        .map_err(ListRegistryError::Fidl)?
        .map_err(ListRegistryError::WatchDevicesAdded)?
        .devices
        .expect("missing devices")
        .into_iter()
        .map(Info::from)
        .collect())
}

#[cfg(test)]
mod test {
    use super::*;
    use fidl::endpoints::spawn_stream_handler;
    use std::sync::Arc;
    use test_case::test_case;
    use vfs::pseudo_directory;

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
        fac::Devfs { name: "3d99d780".to_string(), device_type: fadevice::DeviceType::Input },
        "/dev/class/audio-input/3d99d780";
        "input"
    )]
    #[test_case(
        fac::Devfs { name: "3d99d780".to_string(), device_type: fadevice::DeviceType::Output },
        "/dev/class/audio-output/3d99d780";
        "output"
    )]
    #[test_case(
        fac::Devfs { name: "3d99d780".to_string(), device_type: fadevice::DeviceType::Dai },
        "/dev/class/dai/3d99d780";
        "dai"
    )]
    #[test_case(
        fac::Devfs { name: "3d99d780".to_string(), device_type: fadevice::DeviceType::Codec },
        "/dev/class/codec/3d99d780";
        "codec"
    )]
    #[test_case(
        fac::Devfs { name: "3d99d780".to_string(), device_type: fadevice::DeviceType::Composite },
        "/dev/class/audio-composite/3d99d780";
        "composite"
    )]
    fn test_devfs_selector_path(devfs: fac::Devfs, expected_path: &str) {
        assert_eq!(expected_path, DevfsSelector(devfs).path());
    }

    fn placeholder_node() -> Arc<vfs::service::Service> {
        vfs::service::endpoint(move |_scope, _channel| {
            // Just drop the channel.
        })
    }

    #[fuchsia::test]
    async fn test_list_devfs() {
        // Placeholder for serving the device protocol.
        // list_devfs doesn't connect to it, so we don't serve it.
        let placeholder = placeholder_node();

        let dev_class_vfs = pseudo_directory! {
            "audio-input" => pseudo_directory! {
                "input-0" => placeholder.clone(),
                "input-1" => placeholder.clone(),
            },
            "audio-output" => pseudo_directory! {
                "output-0" => placeholder.clone(),
            },
            "dai" => pseudo_directory! {
                "dai-0" => placeholder.clone(),
            },
            "codec" => pseudo_directory! {
                "codec-0" => placeholder.clone(),
            },
            "audio-composite" => pseudo_directory! {
                "composite-0" => placeholder.clone(),
            },
        };

        let dev_class = vfs::directory::spawn_directory(dev_class_vfs);
        let selectors = list_devfs(&dev_class).await.unwrap();

        assert_eq!(
            vec![
                DevfsSelector(fac::Devfs {
                    name: "input-0".to_string(),
                    device_type: fadevice::DeviceType::Input,
                }),
                DevfsSelector(fac::Devfs {
                    name: "input-1".to_string(),
                    device_type: fadevice::DeviceType::Input,
                }),
                DevfsSelector(fac::Devfs {
                    name: "output-0".to_string(),
                    device_type: fadevice::DeviceType::Output,
                }),
                DevfsSelector(fac::Devfs {
                    name: "dai-0".to_string(),
                    device_type: fadevice::DeviceType::Dai,
                }),
                DevfsSelector(fac::Devfs {
                    name: "codec-0".to_string(),
                    device_type: fadevice::DeviceType::Codec,
                }),
                DevfsSelector(fac::Devfs {
                    name: "composite-0".to_string(),
                    device_type: fadevice::DeviceType::Composite,
                }),
            ],
            selectors
        );
    }

    fn serve_registry(devices: Vec<fadevice::Info>) -> fadevice::RegistryProxy {
        let devices = Arc::new(devices);
        spawn_stream_handler(move |request| {
            let devices = devices.clone();
            async move {
                match request {
                    fadevice::RegistryRequest::WatchDevicesAdded { responder } => responder
                        .send(Ok(&fadevice::RegistryWatchDevicesAddedResponse {
                            devices: Some((*devices).clone()),
                            ..Default::default()
                        }))
                        .unwrap(),
                    _ => unimplemented!(),
                }
            }
        })
    }

    #[fuchsia::test]
    async fn test_list_registry() {
        let devices = vec![
            fadevice::Info { token_id: Some(1), ..Default::default() },
            fadevice::Info { token_id: Some(2), ..Default::default() },
            fadevice::Info { token_id: Some(3), ..Default::default() },
        ];

        let registry = serve_registry(devices);

        let infos = list_registry(&registry).await.unwrap();

        assert_eq!(
            infos,
            vec![
                Info::from(fadevice::Info { token_id: Some(1), ..Default::default() }),
                Info::from(fadevice::Info { token_id: Some(2), ..Default::default() }),
                Info::from(fadevice::Info { token_id: Some(3), ..Default::default() }),
            ]
        );
    }

    #[test]
    fn test_unique_instance_id_display() {
        let id = UniqueInstanceId([
            0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d,
            0x0e, 0x0f,
        ]);
        let expected = "000102030405060708090a0b0c0d0e0f";
        assert_eq!(id.to_string(), expected);
    }
}
