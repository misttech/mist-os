// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::{
    array::TryFromSliceError,
    convert::{TryFrom, TryInto},
    fmt,
};

use itertools::Itertools;

pub use version_history_shared::Status;

/// VERSION_HISTORY is an array of all the known SDK versions.  It is guaranteed
/// (at compile-time) by the proc_macro to be non-empty.
///
/// Deprecated. Use [HISTORY] instead.
///
/// TODO: https://fxbug.dev/326104955 - Delete this.
pub const VERSION_HISTORY: &[Version] = &version_history_macro::declare_version_history!();

/// LATEST_VERSION is the latest known SDK version.
///
/// Deprecated. Use [HISTORY] instead.
///
/// TODO: https://fxbug.dev/326104955 - Delete this.
pub const LATEST_VERSION: &Version = &version_history_macro::latest_sdk_version!();

/// SUPPORTED_API_LEVELS are the supported API levels.
///
/// Deprecated. Use [HISTORY] instead.
///
/// TODO: https://fxbug.dev/326104955 - Delete this.
pub const SUPPORTED_API_LEVELS: &[Version] = &version_history_macro::get_supported_versions!();

/// Global [VersionHistory] instance generated at build-time from the contents
/// of `//sdk/version_history.json`.
pub const HISTORY: VersionHistory =
    VersionHistory { versions: &version_history_macro::declare_version_history!() };

/// An `ApiLevel` represents an API level of the Fuchsia platform.
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Copy, Clone)]
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
        Ok(AbiRevision::from_u64(s.parse()?))
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
    ///
    /// TODO: https://fxbug.dev/326104955 - Make this private.
    pub fn is_supported(&self) -> bool {
        match self.status {
            Status::InDevelopment | Status::Supported => true,
            Status::Unsupported => false,
        }
    }
}

/// TODO: https://fxbug.dev/326104955 - Delete this.
pub fn version_from_abi_revision(abi_revision: AbiRevision) -> Option<Version> {
    // TODO(https://fxbug.dev/42068452): Store APIs and ABIs in a map instead of a list.
    VERSION_HISTORY.iter().find(|v| v.abi_revision == abi_revision).cloned()
}

/// Returns true if the given abi_revision is listed in the VERSION_HISTORY of
/// known SDK versions.
///
/// TODO: https://fxbug.dev/326104955 - Delete this.
pub fn is_valid_abi_revision(abi_revision: AbiRevision) -> bool {
    version_from_abi_revision(abi_revision).is_some()
}

/// Returns true if the given abi_revision is listed in SUPPORTED_API_LEVELS.
///
/// TODO: https://fxbug.dev/326104955 - Delete this.
pub fn is_supported_abi_revision(abi_revision: AbiRevision) -> bool {
    if let Some(version) = version_from_abi_revision(abi_revision) {
        version.is_supported()
    } else {
        false
    }
}

/// Returns a vector of the ABI revisions in SUPPORTED_API_LEVELS.
///
/// TODO: https://fxbug.dev/326104955 - Delete this.
pub fn get_supported_abi_revisions() -> Vec<AbiRevision> {
    let mut abi_revisions: Vec<AbiRevision> = Vec::new();
    for version in SUPPORTED_API_LEVELS {
        abi_revisions.push(version.abi_revision);
    }

    return abi_revisions;
}

/// TODO: https://fxbug.dev/326104955 - Delete this.
pub fn get_latest_abi_revision() -> AbiRevision {
    return LATEST_VERSION.abi_revision;
}

/// TODO: https://fxbug.dev/326104955 - Delete this.
pub fn check_abi_revision(abi_revision: Option<AbiRevision>) -> Result<(), AbiRevisionError> {
    let abi_revision = abi_revision.ok_or(AbiRevisionError::Absent)?;

    if let Some(version) = version_from_abi_revision(abi_revision) {
        if version.is_supported() {
            Ok(())
        } else {
            Err(AbiRevisionError::Unsupported {
                version,
                supported_versions: SUPPORTED_API_LEVELS.to_vec(),
            })
        }
    } else {
        Err(AbiRevisionError::Unknown {
            abi_revision,
            supported_versions: SUPPORTED_API_LEVELS.to_vec(),
        })
    }
}

// VersionHistory stores the history of Fuchsia API levels, and lets callers
// query the support status of API levels and ABI revisions.
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
    pub fn new_for_testing(versions: &'static [Version]) -> Self {
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

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    // Helper to convert from the shared crate's Version struct to this crate's
    // struct.
    fn version_history_from_shared() -> Vec<Version> {
        version_history_shared::version_history()
            .unwrap()
            .into_iter()
            .map(|v| Version {
                api_level: ApiLevel::from_u64(v.api_level),
                abi_revision: AbiRevision::from_u64(v.abi_revision.value),
                status: v.status,
            })
            .collect::<Vec<_>>()
    }

    #[test]
    fn test_proc_macro_worked() {
        let expected = version_history_from_shared();
        assert_eq!(expected, VERSION_HISTORY);
    }

    #[test]
    fn test_latest_version_proc_macro() {
        let shared_versions = version_history_from_shared();
        let expected = shared_versions.last().expect("version_history_shared was empty");
        assert_eq!(expected, &version_history_macro::latest_sdk_version!());
    }

    #[test]
    fn test_valid_abi_revision() {
        for version in VERSION_HISTORY {
            assert!(is_valid_abi_revision(version.abi_revision));
        }
    }

    #[test]
    fn test_is_supported_abi_revision() {
        for version in SUPPORTED_API_LEVELS {
            assert!(is_supported_abi_revision(version.abi_revision));
        }
    }

    // To ensure this test doesn't flake, proptest is used to generate u64
    // values which are not in the current VERSION_HISTORY list.
    proptest! {
        #[test]
        fn test_invalid_abi_revision(u in any::<u64>().prop_filter("using u64 that isn't in VERSION_HISTORY", |u|
            // The randomly chosen 'abi_revision' must not equal any of the
            // abi_revisions in the VERSION_HISTORY list.
            VERSION_HISTORY.iter().all(|v| v.abi_revision != AbiRevision::from_u64(*u))
        )) {
            assert!(!is_valid_abi_revision(AbiRevision::from_u64(u)))
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
    fn test_proc_macro_worked2() {
        let expected = version_history_from_shared();
        assert_eq!(expected, HISTORY.versions);
    }

    #[test]
    fn test_example_supported_abi_revision_is_supported() {
        let example_version = HISTORY.get_example_supported_version_for_tests();

        assert_eq!(
            HISTORY.check_api_level_for_build(example_version.api_level),
            Ok(example_version.abi_revision)
        );

        HISTORY
            .check_abi_revision_for_runtime(example_version.abi_revision)
            .expect("you said it was supported!");
    }

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
