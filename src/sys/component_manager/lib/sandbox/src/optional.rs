// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use fidl_fuchsia_component_sandbox as fsandbox;

use crate::{Capability, CapabilityTrait};

/// A capability that contains an Option of a capability.
#[derive(Clone, Debug)]
pub struct Optional(pub Option<Box<Capability>>);

impl Optional {
    /// Returns an optional that holds None.
    pub fn void() -> Self {
        Optional(None)
    }
}

impl CapabilityTrait for Optional {}

impl From<Optional> for fsandbox::OptionalCapability {
    fn from(optional: Optional) -> Self {
        Self { value: optional.0.map(|value| Box::new(value.into_fidl())) }
    }
}

impl From<Optional> for fsandbox::Capability {
    fn from(optional: Optional) -> Self {
        Self::Optional(optional.into())
    }
}

#[cfg(test)]
mod test {
    use super::Optional;
    use crate::{Capability, Unit};
    use assert_matches::assert_matches;
    use fidl_fuchsia_component_sandbox as fsandbox;

    #[test]
    fn test_void() {
        let void = Optional::void();
        assert!(void.0.is_none());
    }

    #[test]
    fn test_void_into_fidl() {
        let unit = Optional::void();
        let any: Capability = unit.into();
        let fidl_capability: fsandbox::Capability = any.into();
        assert_eq!(
            fidl_capability,
            fsandbox::Capability::Optional(fsandbox::OptionalCapability { value: None })
        );
    }

    #[test]
    fn test_some_into_fidl() {
        let cap: Capability = Unit::default().into();
        let optional_any: Capability = Optional(Some(Box::new(cap))).into();
        let fidl_capability: fsandbox::Capability = optional_any.into();
        assert_matches!(
            fidl_capability,
            fsandbox::Capability::Optional(fsandbox::OptionalCapability {
                value: Some(value)
            })
            if *value == fsandbox::Capability::Unit(fsandbox::UnitCapability {})
        );
    }
}
