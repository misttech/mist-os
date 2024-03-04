// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use itertools::Itertools;
use serde::{
    de::{Error, Unexpected},
    Deserialize, Deserializer, Serialize,
};
use std::{array::TryFromSliceError, collections::BTreeMap, fmt};

const VERSION_HISTORY_BYTES: &[u8] = include_bytes!(env!("SDK_VERSION_HISTORY"));
const VERSION_HISTORY_SCHEMA_ID: &str = "https://fuchsia.dev/schema/version_history-22rnd667.json";
const VERSION_HISTORY_NAME: &str = "Platform version map";
const VERSION_HISTORY_TYPE: &str = "version_history";

/// An `ApiLevel` represents an API level of the Fuchsia platform.
#[derive(Debug, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash, Copy, Clone)]
pub struct ApiLevel(u64);

impl ApiLevel {
    pub const fn from_u64(value: u64) -> Self {
        Self(value)
    }

    pub fn as_u64(&self) -> u64 {
        self.0
    }
}

impl fmt::Display for ApiLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::str::FromStr for ApiLevel {
    type Err = std::num::ParseIntError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(ApiLevel::from_u64(s.parse()?))
    }
}

impl From<u64> for ApiLevel {
    fn from(api_level: u64) -> ApiLevel {
        ApiLevel(api_level)
    }
}

impl From<&u64> for ApiLevel {
    fn from(api_level: &u64) -> ApiLevel {
        ApiLevel(*api_level)
    }
}

impl From<ApiLevel> for u64 {
    fn from(api_level: ApiLevel) -> u64 {
        api_level.0
    }
}

/// An `AbiRevision` is the 64-bit stamp representing an ABI revision of the
/// Fuchsia platform.
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Copy, Clone)]
pub struct AbiRevision(u64);

impl AbiRevision {
    pub const PATH: &'static str = "meta/fuchsia.abi/abi-revision";

    /// Parse the ABI revision from little-endian bytes.
    pub fn from_bytes(b: [u8; 8]) -> Self {
        AbiRevision(u64::from_le_bytes(b))
    }

    /// Encode the ABI revision into little-endian bytes.
    pub fn as_bytes(&self) -> [u8; 8] {
        self.0.to_le_bytes()
    }

    pub const fn from_u64(u: u64) -> AbiRevision {
        AbiRevision(u)
    }

    pub fn as_u64(&self) -> u64 {
        self.0
    }
}

impl fmt::Display for AbiRevision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:x}", self.0)
    }
}

impl std::str::FromStr for AbiRevision {
    type Err = std::num::ParseIntError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let parsed = if let Some(s) = s.strip_prefix("0x") {
            u64::from_str_radix(&s, 16)?
        } else {
            u64::from_str_radix(&s, 10)?
        };
        Ok(AbiRevision::from_u64(parsed))
    }
}

impl<'de> Deserialize<'de> for AbiRevision {
    fn deserialize<D>(deserializer: D) -> Result<AbiRevision, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        s.parse().map_err(|_| D::Error::invalid_value(Unexpected::Str(&s), &"an unsigned integer"))
    }
}

impl From<u64> for AbiRevision {
    fn from(abi_revision: u64) -> AbiRevision {
        AbiRevision(abi_revision)
    }
}

impl From<&u64> for AbiRevision {
    fn from(abi_revision: &u64) -> AbiRevision {
        AbiRevision(*abi_revision)
    }
}

impl From<AbiRevision> for u64 {
    fn from(abi_revision: AbiRevision) -> u64 {
        abi_revision.0
    }
}

impl From<[u8; 8]> for AbiRevision {
    fn from(abi_revision: [u8; 8]) -> AbiRevision {
        AbiRevision::from_bytes(abi_revision)
    }
}

impl TryFrom<&[u8]> for AbiRevision {
    type Error = TryFromSliceError;

    fn try_from(abi_revision: &[u8]) -> Result<AbiRevision, Self::Error> {
        let abi_revision: [u8; 8] = abi_revision.try_into()?;
        Ok(AbiRevision::from_bytes(abi_revision))
    }
}

/// Version represents an API level of the Fuchsia platform API.
///
/// See
/// https://fuchsia.dev/fuchsia-src/contribute/governance/rfcs/0239_platform_versioning_in_practice
/// for more details.
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Clone)]
pub struct Version {
    /// The number associated with this API level.
    pub api_level: ApiLevel,

    /// The ABI revision associated with this API level.
    pub abi_revision: AbiRevision,

    /// The Status denotes the current status of the API level.
    pub status: Status,
}

impl Version {
    /// Returns true if this version is supported - that is, whether components
    /// targeting this version will be able to run on this device.
    fn is_supported(&self) -> bool {
        match self.status {
            Status::InDevelopment | Status::Supported => true,
            Status::Unsupported => false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, PartialOrd, Ord, Hash)]
pub enum Status {
    #[serde(rename = "in-development")]
    InDevelopment,
    #[serde(rename = "supported")]
    Supported,
    #[serde(rename = "unsupported")]
    Unsupported,
}

/// VersionHistory stores the history of Fuchsia API levels, and lets callers
/// query the support status of API levels and ABI revisions.
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct VersionHistory {
    versions: &'static [Version],
}

impl VersionHistory {
    /// Outside of tests, callers should use the static [HISTORY]
    /// instance. However, for testing purposes, you may want to build your own
    /// hermetic "alternate history" that doesn't change over time.
    ///
    /// If you're not testing versioning in particular, and you just want an API
    /// level/ABI revision that works, see
    /// [get_example_supported_version_for_tests].
    pub const fn new(versions: &'static [Version]) -> Self {
        VersionHistory { versions }
    }

    /// Components which are not packaged but are "part of the platform"
    /// nonetheless (e.g. components loaded from bootfs) should be considered to
    /// have this ABI revision.
    pub fn get_abi_revision_for_platform_components(&self) -> AbiRevision {
        self.supported_versions().last().unwrap().abi_revision
    }

    /// ffx currently presents information suggesting that the platform supports
    /// a _single_ API level and ABI revision. This is misleading and should be
    /// fixed. Until we do, we have to choose a particular Version for ffx to
    /// present.
    ///
    /// TODO: https://fxbug.dev/326096999 - Remove this, or turn it into
    /// something that makes more sense.
    pub fn get_misleading_version_for_ffx(&self) -> Version {
        self.supported_versions().last().unwrap()
    }

    /// The packaging tools currently stamp packages with a default ABI revision
    /// if the API level or ABI revision were not included on the command line.
    /// This is dangerous, because it'll be difficult to notice if the wrong ABI
    /// stamp is being applied. We should remove this default, but until we do,
    /// we'll use this ABI revision.
    ///
    /// TODO: https://fxbug.dev/326095332 - Remove this.
    pub fn get_default_abi_revision_for_swd(&self) -> AbiRevision {
        self.supported_versions().last().unwrap().abi_revision
    }

    /// API level to be used in tests that create components on the fly and need
    /// to specify a supported API level or ABI revision, but don't particularly
    /// care which. The returned [Version] will be consistent within a given
    /// build, but may change from build to build.
    pub fn get_example_supported_version_for_tests(&self) -> Version {
        self.supported_versions().last().unwrap()
    }

    /// Check whether the platform supports building components that target the
    /// given API level, and if so, returns the ABI revision associated with
    /// that API level.
    ///
    /// TODO: https://fxbug.dev/326096347 - This doesn't actually check that the
    /// API level is supported, merely that it exists. At time of writing, this
    /// is the behavior implemented by ffx package, so I'm keeping it consistent
    /// in the interest of no-op refactoring.
    pub fn check_api_level_for_build(
        &self,
        api_level: ApiLevel,
    ) -> Result<AbiRevision, ApiLevelError> {
        if let Some(version) = self.version_from_api_level(api_level) {
            Ok(version.abi_revision)
        } else {
            Err(ApiLevelError::Unknown {
                api_level,
                supported: self.supported_versions().map(|v| v.api_level).collect(),
            })
        }
    }

    /// Check whether the operating system supports running components that
    /// target the given ABI revision.
    pub fn check_abi_revision_for_runtime(
        &self,
        abi_revision: AbiRevision,
    ) -> Result<(), AbiRevisionError> {
        if let Some(version) = self.version_from_abi_revision(abi_revision) {
            if version.is_supported() {
                Ok(())
            } else {
                Err(AbiRevisionError::Unsupported {
                    version,
                    supported_versions: self.supported_versions().collect(),
                })
            }
        } else {
            Err(AbiRevisionError::Unknown {
                abi_revision,
                supported_versions: self.supported_versions().collect(),
            })
        }
    }

    /// The packaging tools currently allow specifying an ABI revision directly
    /// on the command line, and validate that that ABI revision _exists_, but
    /// nothing else about it.
    ///
    /// TODO: https://fxbug.dev/326095523 - Figure out if this behavior makes
    /// sense, or if ffx package should even accept --abi-revision.
    pub fn abi_revision_matches_some_api_level(&self, abi_revision: AbiRevision) -> bool {
        self.version_from_abi_revision(abi_revision).is_some()
    }

    fn supported_versions(&self) -> impl Iterator<Item = Version> + '_ {
        self.versions.iter().filter(|v| v.is_supported()).cloned()
    }

    fn version_from_abi_revision(&self, abi_revision: AbiRevision) -> Option<Version> {
        self.versions.iter().find(|v| v.abi_revision == abi_revision).cloned()
    }

    pub fn version_from_api_level(&self, api_level: ApiLevel) -> Option<Version> {
        self.versions.iter().find(|v| v.api_level == api_level).cloned()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AbiRevisionError {
    /// A component tried to run, but it presented no ABI revision.
    Absent,

    /// A component tried to run, but its ABI revision was not recognized.
    Unknown { abi_revision: AbiRevision, supported_versions: Vec<Version> },

    /// A component tried to run, but the ABI revision it presented is not
    /// supported by this system.
    Unsupported { version: Version, supported_versions: Vec<Version> },
}

impl std::error::Error for AbiRevisionError {}

impl std::fmt::Display for AbiRevisionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let write_supported_versions =
            |f: &mut std::fmt::Formatter<'_>, supported_versions: &[Version]| -> std::fmt::Result {
                write!(f, "The following API levels are supported: ")?;

                for (idx, version) in supported_versions.iter().enumerate() {
                    write!(f, "\n└── {} ({})", version.api_level, version.abi_revision)?;
                    if idx != supported_versions.len() - 1 {
                        write!(f, ", ")?;
                    }
                }
                Ok(())
            };

        match self {
            AbiRevisionError::Absent => write!(f, "Missing target ABI revision."),
            AbiRevisionError::Unknown { abi_revision, supported_versions } => {
                write!(
                    f,
                    "Unknown target ABI revision: {}. The OS may be too old to support it? ",
                    abi_revision
                )?;
                write_supported_versions(f, supported_versions)
            }
            AbiRevisionError::Unsupported { version, supported_versions } => {
                write!(
                    f,
                    "Component targets API {} ({}), which is no longer supported. ",
                    version.api_level, version.abi_revision
                )?;
                write_supported_versions(f, supported_versions)
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApiLevelError {
    /// The user attempted to build a component targeting an API level that was
    /// not recognized.
    Unknown { api_level: ApiLevel, supported: Vec<ApiLevel> },

    /// The user attempted to build a component targeting an API level that was
    /// recognized, but is not supported for building.
    Unsupported { version: Version, supported: Vec<ApiLevel> },
}

impl std::error::Error for ApiLevelError {}

impl std::fmt::Display for ApiLevelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ApiLevelError::Unknown { api_level, supported } => {
                write!(
                    f,
                    "Unknown target API level: {}. The SDK may be too old to support it?
The following API levels are supported: {}",
                    api_level,
                    supported.iter().join(",")
                )
            }
            ApiLevelError::Unsupported { version, supported } => {
                write!(
                    f,
                    "Building components targeting API level {} is no longer supported.
The following API levels are supported: {}",
                    version.api_level,
                    supported.iter().join(",")
                )
            }
        }
    }
}

#[derive(Deserialize)]
struct VersionHistoryDataJson {
    name: String,
    #[serde(rename = "type")]
    element_type: String,
    api_levels: BTreeMap<String, ApiLevelJson>,
}

#[derive(Deserialize)]
struct VersionHistoryJson {
    schema_id: String,
    data: VersionHistoryDataJson,
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Clone, Deserialize)]
struct ApiLevelJson {
    pub abi_revision: AbiRevision,
    pub status: Status,
}

pub fn version_history() -> Result<Vec<Version>, serde_json::Error> {
    parse_version_history(VERSION_HISTORY_BYTES)
}

fn parse_version_history(bytes: &[u8]) -> Result<Vec<Version>, serde_json::Error> {
    let v: VersionHistoryJson = serde_json::from_slice(bytes)?;
    if v.schema_id != VERSION_HISTORY_SCHEMA_ID {
        return Err(serde_json::Error::invalid_value(
            Unexpected::Str(&v.schema_id),
            &VERSION_HISTORY_SCHEMA_ID,
        ));
    }
    if v.data.name != VERSION_HISTORY_NAME {
        return Err(serde_json::Error::invalid_value(
            Unexpected::Str(&v.data.name),
            &VERSION_HISTORY_NAME,
        ));
    }
    if v.data.element_type != VERSION_HISTORY_TYPE {
        return Err(serde_json::Error::invalid_value(
            Unexpected::Str(&v.data.element_type),
            &VERSION_HISTORY_TYPE,
        ));
    }

    let mut versions = Vec::new();

    for (key, value) in v.data.api_levels {
        let api_level = key
            .parse()
            .map_err(|_| serde::de::Error::invalid_value(Unexpected::Str(&key), &"an integer"))?;

        versions.push(Version {
            api_level,
            abi_revision: value.abi_revision,
            status: value.status,
        });
    }

    versions.sort_by_key(|s| s.api_level);

    Ok(versions)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_version_history_works() {
        let versions = version_history().unwrap();
        assert_eq!(
            versions[0],
            Version {
                api_level: 4.into(),
                abi_revision: 0x601665C5B1A89C7F.into(),
                status: Status::Unsupported,
            }
        )
    }

    #[test]
    fn test_parse_history_works() {
        let expected_bytes = br#"{
            "data": {
                "name": "Platform version map",
                "type": "version_history",
                "api_levels": {
                    "1":{
                        "abi_revision":"0xBEBE3F5CAAA046D2",
                        "status":"supported"
                    },
                    "2":{
                        "abi_revision":"0x50cbc6e8a39e1e2c",
                        "status":"in-development"
                    }
                }
            },
            "schema_id": "https://fuchsia.dev/schema/version_history-22rnd667.json"
        }"#;

        assert_eq!(
            parse_version_history(&expected_bytes[..]).unwrap(),
            vec![
                Version {
                    api_level: 1.into(),
                    abi_revision: 0xBEBE3F5CAAA046D2.into(),
                    status: Status::Supported
                },
                Version {
                    api_level: 2.into(),
                    abi_revision: 0x50CBC6E8A39E1E2C.into(),
                    status: Status::InDevelopment
                },
            ],
        );
    }

    #[test]
    fn test_parse_history_rejects_invalid_schema() {
        let expected_bytes = br#"{
            "data": {
                "name": "Platform version map",
                "type": "version_history",
                "api_levels": {}
            },
            "schema_id": "some-schema"
        }"#;

        assert_eq!(
            &parse_version_history(&expected_bytes[..]).unwrap_err().to_string(),
            "invalid value: string \"some-schema\", expected https://fuchsia.dev/schema/version_history-22rnd667.json"
        );
    }

    #[test]
    fn test_parse_history_rejects_invalid_name() {
        let expected_bytes = br#"{
            "data": {
                "name": "some-name",
                "type": "version_history",
                "api_levels": {}
            },
            "schema_id": "https://fuchsia.dev/schema/version_history-22rnd667.json"
        }"#;

        assert_eq!(
            &parse_version_history(&expected_bytes[..]).unwrap_err().to_string(),
            "invalid value: string \"some-name\", expected Platform version map"
        );
    }

    #[test]
    fn test_parse_history_rejects_invalid_type() {
        let expected_bytes = br#"{
            "data": {
                "name": "Platform version map",
                "type": "some-type",
                "api_levels": {}
            },
            "schema_id": "https://fuchsia.dev/schema/version_history-22rnd667.json"
        }"#;

        assert_eq!(
            &parse_version_history(&expected_bytes[..]).unwrap_err().to_string(),
            "invalid value: string \"some-type\", expected version_history"
        );
    }

    #[test]
    fn test_parse_history_rejects_invalid_versions() {
        for (api_level, abi_revision, err) in [
            (
                "some-version",
                "1",
                "invalid value: string \"some-version\", expected an integer",
            ),
            (
                "-1",
                "1",
                 "invalid value: string \"-1\", expected an integer",
            ),
            (
                "1",
                "some-revision",
                "invalid value: string \"some-revision\", expected an unsigned integer at line 1 column 58",
            ),
            (
                "1",
                "-1",
                "invalid value: string \"-1\", expected an unsigned integer at line 1 column 47",
            ),
        ] {
            let expected_bytes = serde_json::to_vec(&serde_json::json!({
                "data": {
                    "name": VERSION_HISTORY_NAME,
                    "type": VERSION_HISTORY_TYPE,
                    "api_levels": {
                        api_level:{
                            "abi_revision": abi_revision,
                            "status": Status::InDevelopment,
                        }
                    },
                },
                "schema_id": VERSION_HISTORY_SCHEMA_ID,
            }))
            .unwrap();

            assert_eq!(parse_version_history(&expected_bytes[..]).unwrap_err().to_string(), err);
        }
    }
    pub const FAKE_VERSION_HISTORY: VersionHistory = VersionHistory {
        versions: &[
            Version {
                api_level: ApiLevel::from_u64(4),
                abi_revision: AbiRevision::from_u64(0xabcd0004),
                status: Status::Unsupported,
            },
            Version {
                api_level: ApiLevel::from_u64(5),
                abi_revision: AbiRevision::from_u64(0xabcd0005),
                status: Status::Supported,
            },
            Version {
                api_level: ApiLevel::from_u64(6),
                abi_revision: AbiRevision::from_u64(0xabcd0006),
                status: Status::Supported,
            },
            Version {
                api_level: ApiLevel::from_u64(7),
                abi_revision: AbiRevision::from_u64(0xabcd0007),
                status: Status::InDevelopment,
            },
        ],
    };

    #[test]
    fn test_check_abi_revision() {
        let supported_versions: Vec<Version> =
            FAKE_VERSION_HISTORY.versions[1..4].iter().cloned().collect();

        assert_eq!(
            FAKE_VERSION_HISTORY.check_abi_revision_for_runtime(0x1234.into()),
            Err(AbiRevisionError::Unknown {
                abi_revision: 0x1234.into(),

                supported_versions: supported_versions.clone()
            })
        );

        assert_eq!(
            FAKE_VERSION_HISTORY.check_abi_revision_for_runtime(0xabcd0004.into()),
            Err(AbiRevisionError::Unsupported {
                version: FAKE_VERSION_HISTORY.versions[0].clone(),

                supported_versions: supported_versions.clone()
            })
        );

        FAKE_VERSION_HISTORY
            .check_abi_revision_for_runtime(0xabcd0005.into())
            .expect("level 5 should be supported");
        FAKE_VERSION_HISTORY
            .check_abi_revision_for_runtime(0xabcd0007.into())
            .expect("level 7 should be supported");
    }

    #[test]
    fn test_check_api_level() {
        let supported: Vec<ApiLevel> = vec![5.into(), 6.into(), 7.into()];

        assert_eq!(
            FAKE_VERSION_HISTORY.check_api_level_for_build(42.into()),
            Err(ApiLevelError::Unknown { api_level: 42.into(), supported: supported.clone() })
        );

        // This currently says API 4 is supported, but it shouldn't.
        //
        // TODO: https://fxbug.dev/326096347 - Uncomment this once it passes.
        //
        // assert_eq!(
        //     FAKE_VERSION_HISTORY.check_api_level_for_build(4.into()),
        //     Err(ApiLevelError::Unsupported {
        //         version: FAKE_VERSION_HISTORY.versions[0],

        //         supported: supported.clone()
        //     })
        // );

        assert_eq!(FAKE_VERSION_HISTORY.check_api_level_for_build(6.into()), Ok(0xabcd0006.into()));
        assert_eq!(FAKE_VERSION_HISTORY.check_api_level_for_build(7.into()), Ok(0xabcd0007.into()));
    }

    #[test]
    fn test_various_getters() {
        assert_eq!(
            FAKE_VERSION_HISTORY.get_abi_revision_for_platform_components(),
            0xabcd0007.into()
        );
        assert_eq!(FAKE_VERSION_HISTORY.get_default_abi_revision_for_swd(), 0xabcd0007.into());
        assert_eq!(
            FAKE_VERSION_HISTORY.get_example_supported_version_for_tests(),
            FAKE_VERSION_HISTORY.versions[3].clone()
        );
        assert_eq!(
            FAKE_VERSION_HISTORY.get_misleading_version_for_ffx(),
            FAKE_VERSION_HISTORY.versions[3].clone()
        );
    }

    #[test]
    fn test_valid_abi_checker() {
        for i in 0xabcd0004..=0xabcd0007 {
            assert!(
                FAKE_VERSION_HISTORY.abi_revision_matches_some_api_level(AbiRevision::from_u64(i))
            );
        }
        for i in 0xabcd0008..=0xabcd000a {
            assert!(
                !FAKE_VERSION_HISTORY.abi_revision_matches_some_api_level(AbiRevision::from_u64(i))
            );
        }
    }
}
