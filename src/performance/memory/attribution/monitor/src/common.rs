// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::collections::HashMap;

/// A principal identifier that is unique across the whole system. They should only be generated,
/// outside of tests, by a [GlobalPrincipalIdentifierFactory].
#[derive(PartialEq, Eq, Hash, Clone, Copy, Debug)]
pub struct GlobalPrincipalIdentifier(std::num::NonZeroU64);

#[cfg(test)]
impl From<u64> for GlobalPrincipalIdentifier {
    fn from(value: u64) -> Self {
        Self(std::num::NonZeroU64::new(value).unwrap())
    }
}

impl Into<attribution_processing::PrincipalIdentifier> for GlobalPrincipalIdentifier {
    fn into(self) -> attribution_processing::PrincipalIdentifier {
        attribution_processing::PrincipalIdentifier(self.0.get())
    }
}

/// Factory for GlobalPrincipalIdentifier, ensuring their uniqueness.
#[derive(Debug)]
pub struct GlobalPrincipalIdentifierFactory {
    next_id: std::num::NonZeroU64,
}

impl Default for GlobalPrincipalIdentifierFactory {
    fn default() -> GlobalPrincipalIdentifierFactory {
        GlobalPrincipalIdentifierFactory { next_id: std::num::NonZeroU64::new(1).unwrap() }
    }
}

impl GlobalPrincipalIdentifierFactory {
    pub fn next(&mut self) -> GlobalPrincipalIdentifier {
        let value = GlobalPrincipalIdentifier(self.next_id);
        // Fail loudly if we are no longer able to generate new [GlobalPrincipalIdentifier]s.
        self.next_id = self.next_id.checked_add(1).unwrap();
        return value;
    }
}

/// A principal identifier, provided by an attribution provider. This identifier is only unique
/// locally, for a given attribution provider.
#[derive(PartialEq, Eq, Hash, Clone, Copy, Debug)]
pub struct LocalPrincipalIdentifier(pub u64);

impl LocalPrincipalIdentifier {
    const SELF_PRINCIPAL_ID: u64 = 0;

    pub fn self_identifier() -> Self {
        LocalPrincipalIdentifier(LocalPrincipalIdentifier::SELF_PRINCIPAL_ID)
    }

    #[cfg(test)]
    pub fn new_for_tests(value: u64) -> Self {
        Self(value)
    }
}

/// Map between local and global PrincipalIdentifiers.
#[derive(Default)]
pub struct PrincipalIdMap(HashMap<LocalPrincipalIdentifier, GlobalPrincipalIdentifier>);

impl PrincipalIdMap {
    pub fn insert(
        &mut self,
        local_id: LocalPrincipalIdentifier,
        global_id: GlobalPrincipalIdentifier,
    ) {
        self.0.insert(local_id, global_id);
    }

    /// Returns the GlobalPrincipalIdentifier corresponding to `local_id`, provided by the
    /// Principal `parent_id`.
    pub fn get(
        &self,
        local_id: LocalPrincipalIdentifier,
        parent_id: GlobalPrincipalIdentifier,
    ) -> GlobalPrincipalIdentifier {
        if local_id == LocalPrincipalIdentifier::self_identifier() {
            parent_id
        } else {
            *self.0.get(&local_id).unwrap()
        }
    }
}
