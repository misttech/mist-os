// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use fidl_fuchsia_component_sandbox as fsandbox;
use std::fmt::Debug;

use crate::Capability;

#[derive(Capability, Debug, Clone, Default, PartialEq, Eq)]
pub struct Unit;

impl Capability for Unit {}

impl From<Unit> for fsandbox::UnitCapability {
    fn from(_unit: Unit) -> Self {
        fsandbox::UnitCapability {}
    }
}

impl From<Unit> for fsandbox::Capability {
    fn from(unit: Unit) -> Self {
        Self::Unit(unit.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_into_fidl() {
        let unit = Unit::default();
        let fidl_capability: fsandbox::Capability = unit.into();
        assert_eq!(fidl_capability, fsandbox::Capability::Unit(fsandbox::UnitCapability {}));
    }
}
