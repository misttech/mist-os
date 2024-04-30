// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::bail;
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
#[derive(Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash, Copy, Clone)]
pub struct ApiLevel(u64);

impl ApiLevel {
    /// The `HEAD` pseudo-API level, representing the bleeding edge of
    /// development.
    pub const HEAD: ApiLevel = ApiLevel(0xFFFF_FFFF_FFFF_FFFE);

    /// The `LEGACY` pseudo-API level, which is used in platform builds.
    ///
    /// TODO: https://fxbug.dev/42085274 - Remove this once `LEGACY` is actually
    /// gone, and use `HEAD` instead.
    pub const LEGACY: ApiLevel = ApiLevel(0xFFFF_FFFF_FFFF_FFFF);

    pub const fn from_u64(value: u64) -> Self {
        Self(value)
    }

    pub fn as_u64(&self) -> u64 {
        self.0
    }
}

impl fmt::Debug for ApiLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            ApiLevel::HEAD => write!(f, "ApiLevel::HEAD"),
            ApiLevel::LEGACY => write!(f, "ApiLevel::LEGACY"),
            ApiLevel(l) => f.debug_tuple("ApiLevel").field(&l).finish(),
        }
    }
}

impl fmt::Display for ApiLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            ApiLevel::HEAD => write!(f, "HEAD"),
            ApiLevel::LEGACY => write!(f, "LEGACY"),
            ApiLevel(l) => write!(f, "{}", l),
        }
    }
}

impl std::str::FromStr for ApiLevel {
    type Err = std::num::ParseIntError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "HEAD" => Ok(ApiLevel::HEAD),
            "LEGACY" => Ok(ApiLevel::LEGACY),
            s => Ok(ApiLevel::from_u64(s.parse()?)),
        }
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
    /// An ABI revision that is never supported by the platform. To be used when
    /// an ABI revision is necessary, but none makes sense.
    pub const INVALID: AbiRevision = AbiRevision(0xFFFF_FFFF_FFFF_FFFF);

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

    /// The ABI revision for components and packages that are "part of the
    /// platform" and never "move between releases". For example:
    ///
    /// - Packages produced by the platform build have this ABI revision.
    /// - Components which are not packaged but are "part of the platform"
    ///   nonetheless (e.g. components loaded from bootfs) have this ABI
    ///   revision.
    /// - Most packages produced by assembly tools have this ABI revision.
    ///   - The `update` package is a noteworthy exception, since it "moves
    ///     between releases", in that it is produced by assembly tools from one
    ///     Fuchsia release, and then later read by the OS from a previous
    ///     release (that is, the one performing the update).
    pub fn get_abi_revision_for_platform_components(&self) -> AbiRevision {
        self.version_from_api_level(ApiLevel::HEAD).expect("API Level HEAD not found!").abi_revision
    }

    /// ffx currently presents information suggesting that the platform supports
    /// a _single_ API level and ABI revision. This is misleading and should be
    /// fixed. Until we do, we have to choose a particular Version for ffx to
    /// present.
    ///
    /// TODO: https://fxbug.dev/326096999 - Remove this, or turn it into
    /// something that makes more sense.
    pub fn get_misleading_version_for_ffx(&self) -> Version {
        self.supported_versions()
            .filter(|v| v.api_level != ApiLevel::HEAD && v.api_level != ApiLevel::LEGACY)
            .last()
            .unwrap()
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
    pub fn check_api_level_for_build(
        &self,
        api_level: ApiLevel,
    ) -> Result<AbiRevision, ApiLevelError> {
        let Some(version) = self.version_from_api_level(api_level) else {
            return Err(ApiLevelError::Unknown {
                api_level,
                supported: self.supported_versions().map(|v| v.api_level).collect(),
            });
        };

        if version.is_supported() {
            Ok(version.abi_revision)
        } else {
            Err(ApiLevelError::Unsupported {
                version,
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
                write!(f, "The following API levels are supported:")?;

                for version in supported_versions.iter() {
                    write!(f, "\n└── {} (0x{})", version.api_level, version.abi_revision)?;
                }
                Ok(())
            };

        match self {
            AbiRevisionError::Unknown { abi_revision, supported_versions } => {
                write!(
                    f,
                    "Unknown target ABI revision: 0x{}. Is the OS too old to support it? ",
                    abi_revision
                )?;
                write_supported_versions(f, supported_versions)
            }
            AbiRevisionError::Unsupported { version, supported_versions } => {
                write!(
                    f,
                    "API level {} (0x{}) is no longer supported. ",
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
                    "Unknown target API level: {}. Is the SDK too old to support it?
The following API levels are supported: {}",
                    api_level,
                    supported.iter().join(", ")
                )
            }
            ApiLevelError::Unsupported { version, supported } => {
                write!(
                    f,
                    "The SDK no longer supports API level {}.
The following API levels are supported: {}",
                    version.api_level,
                    supported.iter().join(", ")
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

pub fn version_history() -> anyhow::Result<Vec<Version>> {
    parse_version_history(VERSION_HISTORY_BYTES)
}

fn parse_version_history(bytes: &[u8]) -> anyhow::Result<Vec<Version>> {
    let v: VersionHistoryJson = serde_json::from_slice(bytes)?;

    if v.schema_id != VERSION_HISTORY_SCHEMA_ID {
        bail!("expected schema_id = {:?}; got {:?}", VERSION_HISTORY_SCHEMA_ID, v.schema_id)
    }
    if v.data.name != VERSION_HISTORY_NAME {
        bail!("expected data.name = {:?}; got {:?}", VERSION_HISTORY_NAME, v.data.name)
    }
    if v.data.element_type != VERSION_HISTORY_TYPE {
        bail!("expected data.type = {:?}; got {:?}", VERSION_HISTORY_TYPE, v.data.element_type,)
    }

    let mut versions = Vec::new();

    for (key, value) in v.data.api_levels {
        versions.push(Version {
            api_level: key.parse()?,
            abi_revision: value.abi_revision,
            status: value.status,
        });
    }

    versions.sort_by_key(|s| s.api_level);

    let Some(latest_api_version) = versions.last() else {
        bail!("there must be at least one API level")
    };

    if latest_api_version.status == Status::Unsupported {
        bail!("most recent API level must not be 'unsupported'")
    }

    // TODO: https://fxrev.dev/42082683 - the mutable pseudo-API-levels should
    // use an ABI revision that corresponds to a specific release, rather than
    // reusing the one from the "latest" API level.
    let latest_abi_revision = latest_api_version.abi_revision;

    // HEAD version.
    versions.push(Version {
        api_level: ApiLevel::HEAD,
        abi_revision: latest_abi_revision,
        status: Status::InDevelopment,
    });
    // LEGACY version.
    versions.push(Version {
        api_level: ApiLevel::LEGACY,
        abi_revision: latest_abi_revision,
        status: Status::InDevelopment,
    });

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
                Version {
                    api_level: ApiLevel::HEAD,
                    abi_revision: 0x50CBC6E8A39E1E2C.into(),
                    status: Status::InDevelopment
                },
                Version {
                    api_level: ApiLevel::LEGACY,
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
            r#"expected schema_id = "https://fuchsia.dev/schema/version_history-22rnd667.json"; got "some-schema""#
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
            r#"expected data.name = "Platform version map"; got "some-name""#
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
            r#"expected data.type = "version_history"; got "some-type""#
        );
    }

    #[test]
    fn test_parse_history_rejects_invalid_versions() {
        for (api_level, abi_revision, err) in [
            (
                "some-version",
                "1",
                "invalid digit found in string"                ,
            ),
            (
                "-1",
                "1",
                "invalid digit found in string"                ,
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
                abi_revision: AbiRevision::from_u64(0x58ea445e942a0004),
                status: Status::Unsupported,
            },
            Version {
                api_level: ApiLevel::from_u64(5),
                abi_revision: AbiRevision::from_u64(0x58ea445e942a0005),
                status: Status::Supported,
            },
            Version {
                api_level: ApiLevel::from_u64(6),
                abi_revision: AbiRevision::from_u64(0x58ea445e942a0006),
                status: Status::Supported,
            },
            Version {
                api_level: ApiLevel::from_u64(7),
                abi_revision: AbiRevision::from_u64(0x58ea445e942a0007),
                status: Status::InDevelopment,
            },
            Version {
                api_level: ApiLevel::HEAD,
                abi_revision: AbiRevision::from_u64(0x58ea445e942a0007),
                status: Status::InDevelopment,
            },
            Version {
                api_level: ApiLevel::LEGACY,
                abi_revision: AbiRevision::from_u64(0x58ea445e942a0007),
                status: Status::InDevelopment,
            },
        ],
    };

    #[test]
    fn test_check_abi_revision() {
        let supported_versions: Vec<Version> =
            FAKE_VERSION_HISTORY.versions[1..6].iter().cloned().collect();

        assert_eq!(
            FAKE_VERSION_HISTORY.check_abi_revision_for_runtime(0x1234.into()),
            Err(AbiRevisionError::Unknown {
                abi_revision: 0x1234.into(),

                supported_versions: supported_versions.clone()
            })
        );

        assert_eq!(
            FAKE_VERSION_HISTORY.check_abi_revision_for_runtime(0x58ea445e942a0004.into()),
            Err(AbiRevisionError::Unsupported {
                version: FAKE_VERSION_HISTORY.versions[0].clone(),

                supported_versions: supported_versions.clone()
            })
        );

        FAKE_VERSION_HISTORY
            .check_abi_revision_for_runtime(0x58ea445e942a0005.into())
            .expect("level 5 should be supported");
        FAKE_VERSION_HISTORY
            .check_abi_revision_for_runtime(0x58ea445e942a0007.into())
            .expect("level 7 should be supported");
    }

    #[test]
    fn test_pretty_print_abi_error() {
        let supported_versions: Vec<Version> =
            FAKE_VERSION_HISTORY.versions[1..6].iter().cloned().collect();

        assert_eq!(
                AbiRevisionError::Unknown {
                    abi_revision: 0x1234.into(),
                    supported_versions: supported_versions.clone()
                }.to_string(),
                "Unknown target ABI revision: 0x1234. Is the OS too old to support it? The following API levels are supported:
└── 5 (0x58ea445e942a0005)
└── 6 (0x58ea445e942a0006)
└── 7 (0x58ea445e942a0007)
└── HEAD (0x58ea445e942a0007)
└── LEGACY (0x58ea445e942a0007)"
            );
        assert_eq!(
                AbiRevisionError::Unsupported {
                    version: FAKE_VERSION_HISTORY.versions[0].clone(),
                    supported_versions: supported_versions.clone()
                }.to_string(),
                "API level 4 (0x58ea445e942a0004) is no longer supported. The following API levels are supported:
└── 5 (0x58ea445e942a0005)
└── 6 (0x58ea445e942a0006)
└── 7 (0x58ea445e942a0007)
└── HEAD (0x58ea445e942a0007)
└── LEGACY (0x58ea445e942a0007)"
            );
    }

    #[test]
    fn test_pretty_print_api_error() {
        let supported: Vec<ApiLevel> =
            vec![5.into(), 6.into(), 7.into(), ApiLevel::HEAD, ApiLevel::LEGACY];

        assert_eq!(
            ApiLevelError::Unknown { api_level: 42.into(), supported: supported.clone() }
                .to_string(),
            "Unknown target API level: 42. Is the SDK too old to support it?
The following API levels are supported: 5, 6, 7, HEAD, LEGACY",
        );
        assert_eq!(
            ApiLevelError::Unsupported {
                version: FAKE_VERSION_HISTORY.versions[0].clone(),
                supported: supported.clone()
            }
            .to_string(),
            "The SDK no longer supports API level 4.
The following API levels are supported: 5, 6, 7, HEAD, LEGACY"
        );
    }

    #[test]
    fn test_check_api_level() {
        let supported: Vec<ApiLevel> =
            vec![5.into(), 6.into(), 7.into(), ApiLevel::HEAD, ApiLevel::LEGACY];

        assert_eq!(
            FAKE_VERSION_HISTORY.check_api_level_for_build(42.into()),
            Err(ApiLevelError::Unknown { api_level: 42.into(), supported: supported.clone() })
        );

        assert_eq!(
            FAKE_VERSION_HISTORY.check_api_level_for_build(4.into()),
            Err(ApiLevelError::Unsupported {
                version: FAKE_VERSION_HISTORY.versions[0].clone(),
                supported: supported.clone()
            })
        );
        assert_eq!(
            FAKE_VERSION_HISTORY.check_api_level_for_build(6.into()),
            Ok(0x58ea445e942a0006.into())
        );
        assert_eq!(
            FAKE_VERSION_HISTORY.check_api_level_for_build(7.into()),
            Ok(0x58ea445e942a0007.into())
        );
        assert_eq!(
            FAKE_VERSION_HISTORY.check_api_level_for_build(ApiLevel::HEAD),
            Ok(0x58ea445e942a0007.into())
        );
        assert_eq!(
            FAKE_VERSION_HISTORY.check_api_level_for_build(ApiLevel::LEGACY),
            Ok(0x58ea445e942a0007.into())
        );
    }

    #[test]
    fn test_various_getters() {
        assert_eq!(
            FAKE_VERSION_HISTORY.get_abi_revision_for_platform_components(),
            0x58ea445e942a0007.into()
        );
        assert_eq!(
            FAKE_VERSION_HISTORY.get_example_supported_version_for_tests(),
            FAKE_VERSION_HISTORY.versions[5].clone()
        );
        assert_eq!(
            FAKE_VERSION_HISTORY.get_misleading_version_for_ffx(),
            FAKE_VERSION_HISTORY.versions[3].clone()
        );
    }
}
