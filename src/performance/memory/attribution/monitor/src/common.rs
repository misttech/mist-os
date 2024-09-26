// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl_fuchsia_memory_attribution_plugin as fplugin;

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

impl Into<fplugin::PrincipalIdentifier> for GlobalPrincipalIdentifier {
    fn into(self) -> fplugin::PrincipalIdentifier {
        fplugin::PrincipalIdentifier { id: self.0.get() }
    }
}

/// Factory for GlobalPrincipalIdentifier, ensuring their uniqueness.
#[derive(Debug)]
pub struct GlobalPrincipalIdentifierFactory {
    next_id: std::num::NonZeroU64,
}

impl GlobalPrincipalIdentifierFactory {
    pub fn new() -> GlobalPrincipalIdentifierFactory {
        GlobalPrincipalIdentifierFactory { next_id: std::num::NonZeroU64::new(1).unwrap() }
    }

    pub fn next(&mut self) -> GlobalPrincipalIdentifier {
        let value = GlobalPrincipalIdentifier(self.next_id);
        // Fail loudly if we are no longer able to generate new [GlobalPrincipalIdentifier]s.
        self.next_id = self.next_id.checked_add(1).unwrap();
        return value;
    }
}
