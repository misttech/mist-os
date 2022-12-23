// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{error::RightsRoutingError, walk_state::WalkStateUnit},
    fidl_fuchsia_io as fio,
    std::convert::From,
};

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

/// Opaque rights type to define new traits like PartialOrd on.
#[derive(Debug, PartialEq, Eq, Clone)]
pub struct Rights(fio::Operations);

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
        // Since there is no direct translation for connect in CV1 we must explicitly define it
        // here as both flags.
        //
        // TODO(fxbug.dev/60673): Is this correct? ReadBytes | Connect seems like it should translate to
        // READABLE | WRITABLE, not empty rights.
        if flags.is_empty() && rights.contains(fio::Operations::CONNECT) {
            flags |= fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE;
        }
        flags
    }
}

/// Allows creating rights from fio::Operations.
impl From<fio::Operations> for Rights {
    fn from(operations: fio::Operations) -> Self {
        Rights(operations)
    }
}

impl WalkStateUnit for Rights {
    type Error = RightsRoutingError;

    /// Ensures the next walk state of rights satisfies a monotonic increasing sequence. Used to
    /// verify the expectation that no right requested from a use, offer, or expose is missing as
    /// capability routing walks from the capability's consumer to its provider.
    fn validate_next(&self, next_rights: &Rights) -> Result<(), Self::Error> {
        if next_rights.0.contains(self.0) {
            Ok(())
        } else {
            Err(RightsRoutingError::Invalid)
        }
    }

    fn finalize_error() -> Self::Error {
        RightsRoutingError::MissingRightsSource
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
            Rights::from(fio::Operations::empty())
                .validate_next(&Rights::from(LEGACY_READABLE_RIGHTS)),
            Ok(())
        );
        assert_matches!(
            Rights::from(fio::Operations::READ_BYTES | fio::Operations::GET_ATTRIBUTES)
                .validate_next(&Rights::from(LEGACY_READABLE_RIGHTS)),
            Ok(())
        );
        assert_eq!(
            Rights::from(Rights::from(LEGACY_READABLE_RIGHTS)).validate_next(&Rights::from(
                fio::Operations::READ_BYTES | fio::Operations::GET_ATTRIBUTES
            )),
            Err(RightsRoutingError::Invalid)
        );
        assert_eq!(
            Rights::from(fio::Operations::WRITE_BYTES).validate_next(&Rights::from(
                fio::Operations::READ_BYTES | fio::Operations::GET_ATTRIBUTES
            )),
            Err(RightsRoutingError::Invalid)
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
