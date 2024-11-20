// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::error::RightsRoutingError;
use crate::walk_state::WalkStateUnit;
use fidl_fuchsia_io as fio;
use moniker::ExtendedMoniker;
#[cfg(feature = "serde")]
use serde::{de::Deserializer, ser::Serializer, Deserialize, Serialize};
use std::fmt;

/// All the fio rights required to represent fio::OpenFlags::RIGHT_READABLE.
const LEGACY_READABLE_RIGHTS: fio::Operations = fio::Operations::empty()
    .union(fio::Operations::READ_BYTES)
    .union(fio::Operations::GET_ATTRIBUTES)
    .union(fio::Operations::TRAVERSE)
    .union(fio::Operations::ENUMERATE);

/// All the fio rights required to represent fio::OpenFlags::RIGHT_WRITABLE.
const LEGACY_WRITABLE_RIGHTS: fio::Operations = fio::Operations::empty()
    .union(fio::Operations::WRITE_BYTES)
    .union(fio::Operations::UPDATE_ATTRIBUTES)
    .union(fio::Operations::MODIFY_DIRECTORY);

/// Performs rights validation for a routing step
#[derive(Debug, PartialEq, Eq, Clone)]
pub(super) struct RightsWalker {
    rights: Rights,
    moniker: ExtendedMoniker,
}

/// Opaque rights type to define new traits like PartialOrd on.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub struct Rights(fio::Operations);

impl RightsWalker {
    pub fn new(rights: impl Into<Rights>, moniker: impl Into<ExtendedMoniker>) -> Self {
        Self { rights: rights.into(), moniker: moniker.into() }
    }
}

impl Rights {
    /// Converts new fuchsia.io directory rights to legacy fuchsia.io compatible rights. This will
    /// be remove once new rights are supported by component manager.
    pub fn into_legacy(&self) -> fio::OpenFlags {
        let mut flags = fio::OpenFlags::empty();
        let Self(rights) = self;
        // The `intersects` below is intentional. The translation from io2 to io rights is lossy
        // in the sense that a single io2 right may require an io right with coarser permissions.
        if rights.intersects(LEGACY_READABLE_RIGHTS) {
            flags |= fio::OpenFlags::RIGHT_READABLE;
        }
        if rights.intersects(LEGACY_WRITABLE_RIGHTS) {
            flags |= fio::OpenFlags::RIGHT_WRITABLE;
        }
        if rights.contains(fio::Operations::EXECUTE) {
            flags |= fio::OpenFlags::RIGHT_EXECUTABLE;
        }
        flags
    }
}

/// Allows creating rights from fio::Operations.
impl From<fio::Operations> for Rights {
    fn from(rights: fio::Operations) -> Self {
        Rights(rights)
    }
}

impl Into<u64> for Rights {
    fn into(self) -> u64 {
        self.0.bits()
    }
}

impl fmt::Display for Rights {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self(rights) = &self;
        match *rights {
            fio::R_STAR_DIR => write!(f, "r*"),
            fio::W_STAR_DIR => write!(f, "w*"),
            fio::X_STAR_DIR => write!(f, "x*"),
            fio::RW_STAR_DIR => write!(f, "rw*"),
            fio::RX_STAR_DIR => write!(f, "rx*"),
            ops => write!(f, "{:?}", ops),
        }
    }
}

#[cfg(feature = "serde")]
impl Serialize for Rights {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let Self(rights) = self;
        rights.bits().serialize(serializer)
    }
}

#[cfg(feature = "serde")]
impl<'de> Deserialize<'de> for Rights {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let bits: u64 = Deserialize::deserialize(deserializer)?;
        let rights = fio::Operations::from_bits(bits)
            .ok_or_else(|| serde::de::Error::custom("invalid value for fuchsia.io/Operations"))?;
        Ok(Self(rights))
    }
}

impl WalkStateUnit for RightsWalker {
    type Error = RightsRoutingError;

    /// Ensures the next walk state of rights satisfies a monotonic increasing sequence. Used to
    /// verify the expectation that no right requested from a use, offer, or expose is missing as
    /// capability routing walks from the capability's consumer to its provider.
    fn validate_next(&self, next_rights: &RightsWalker) -> Result<(), Self::Error> {
        if next_rights.rights.0.contains(self.rights.0) {
            Ok(())
        } else {
            Err(RightsRoutingError::Invalid {
                moniker: self.moniker.clone(),
                requested: self.rights,
                provided: next_rights.rights,
            })
        }
    }

    fn finalize_error(&self) -> Self::Error {
        RightsRoutingError::MissingRightsSource { moniker: self.moniker.clone() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_matches::assert_matches;

    /// All the fio rights required to represent fio::OpenFlags::RIGHT_EXECUTABLE.
    const LEGACY_EXECUTABLE_RIGHTS: fio::Operations = fio::Operations::EXECUTE;

    #[test]
    fn validate_next() {
        assert_matches!(
            RightsWalker::new(fio::Operations::empty(), ExtendedMoniker::ComponentManager)
                .validate_next(&RightsWalker::new(
                    LEGACY_READABLE_RIGHTS,
                    ExtendedMoniker::ComponentManager
                )),
            Ok(())
        );
        assert_matches!(
            RightsWalker::new(
                fio::Operations::READ_BYTES | fio::Operations::GET_ATTRIBUTES,
                ExtendedMoniker::ComponentManager
            )
            .validate_next(&RightsWalker::new(
                LEGACY_READABLE_RIGHTS,
                ExtendedMoniker::ComponentManager
            )),
            Ok(())
        );
        let provided = fio::Operations::READ_BYTES | fio::Operations::GET_ATTRIBUTES;
        assert_eq!(
            RightsWalker::new(LEGACY_READABLE_RIGHTS, ExtendedMoniker::ComponentManager)
                .validate_next(&RightsWalker::new(provided, ExtendedMoniker::ComponentManager)),
            Err(RightsRoutingError::Invalid {
                moniker: ExtendedMoniker::ComponentManager,
                requested: Rights::from(LEGACY_READABLE_RIGHTS),
                provided: Rights::from(provided),
            })
        );
        let provided = fio::Operations::READ_BYTES | fio::Operations::GET_ATTRIBUTES;
        assert_eq!(
            RightsWalker::new(fio::Operations::WRITE_BYTES, ExtendedMoniker::ComponentManager)
                .validate_next(&RightsWalker::new(provided, ExtendedMoniker::ComponentManager)),
            Err(RightsRoutingError::Invalid {
                moniker: ExtendedMoniker::ComponentManager,
                requested: Rights::from(fio::Operations::WRITE_BYTES),
                provided: Rights::from(provided),
            })
        );
    }

    #[test]
    fn into_legacy() {
        assert_eq!(
            Rights::from(LEGACY_READABLE_RIGHTS).into_legacy(),
            fio::OpenFlags::RIGHT_READABLE
        );
        assert_eq!(
            Rights::from(LEGACY_WRITABLE_RIGHTS).into_legacy(),
            fio::OpenFlags::RIGHT_WRITABLE
        );
        assert_eq!(
            Rights::from(LEGACY_EXECUTABLE_RIGHTS).into_legacy(),
            fio::OpenFlags::RIGHT_EXECUTABLE
        );
        assert_eq!(
            Rights::from(LEGACY_READABLE_RIGHTS | LEGACY_WRITABLE_RIGHTS).into_legacy(),
            fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE
        );
        assert_eq!(
            Rights::from(
                LEGACY_READABLE_RIGHTS | LEGACY_WRITABLE_RIGHTS | LEGACY_EXECUTABLE_RIGHTS
            )
            .into_legacy(),
            fio::OpenFlags::RIGHT_READABLE
                | fio::OpenFlags::RIGHT_WRITABLE
                | fio::OpenFlags::RIGHT_EXECUTABLE
        );
        assert_eq!(
            Rights::from(fio::Operations::READ_BYTES).into_legacy(),
            fio::OpenFlags::RIGHT_READABLE
        );
        assert_eq!(
            Rights::from(fio::Operations::GET_ATTRIBUTES).into_legacy(),
            fio::OpenFlags::RIGHT_READABLE
        );
        assert_eq!(
            Rights::from(fio::Operations::TRAVERSE).into_legacy(),
            fio::OpenFlags::RIGHT_READABLE
        );
        assert_eq!(
            Rights::from(fio::Operations::ENUMERATE).into_legacy(),
            fio::OpenFlags::RIGHT_READABLE
        );
        assert_eq!(
            Rights::from(fio::Operations::WRITE_BYTES).into_legacy(),
            fio::OpenFlags::RIGHT_WRITABLE
        );
        assert_eq!(
            Rights::from(fio::Operations::UPDATE_ATTRIBUTES).into_legacy(),
            fio::OpenFlags::RIGHT_WRITABLE
        );
        assert_eq!(
            Rights::from(fio::Operations::MODIFY_DIRECTORY).into_legacy(),
            fio::OpenFlags::RIGHT_WRITABLE
        );
    }
}
