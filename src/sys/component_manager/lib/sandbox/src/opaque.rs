// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use fidl_fuchsia_component_sandbox as fsandbox;
use fuchsia_zircon::{self as zx, AsHandleRef};
use std::fmt;

use crate::{registry, Capability};

/// A capability that represents an arbitrary, internal Rust object.
///
/// The value only exists in the sandbox registry, and there is no way to use it over FIDL.
/// This means Opaque values can only be created and used within the process that hosts the
/// sandbox registry, i.e. component_manager.
///
/// This is as a workaround for code that used the `Data<T>` capability with a T value that cannot
/// be represented as a FIDL capability.
///
/// DO NOT use this type in new code. If really need to, please leave a TODO to replace it
/// a different type that is compatible with external clients.
#[derive(Capability, Clone)]
pub struct Opaque<T: Clone + Send + Sync + 'static>(pub T);

impl<T: Clone + Send + Sync + 'static> Capability for Opaque<T> {}

impl<T: Clone + Send + Sync + 'static> fmt::Debug for Opaque<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Opaque").finish()
    }
}

impl<T: Clone + Send + Sync + 'static> Into<fsandbox::Capability> for Opaque<T> {
    fn into(self) -> fsandbox::Capability {
        let token = zx::Event::create();

        // Move this capability into the registry.
        registry::insert(Box::new(self), token.get_koid().unwrap());

        fsandbox::Capability::Opaque(token)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AnyCapability;
    use anyhow::{Context, Result};

    #[derive(Clone, Debug, PartialEq, Eq)]
    struct SomeType;

    #[fuchsia::test]
    async fn opaque_into_fidl() -> Result<()> {
        let value = SomeType;

        let opaque = Opaque(value);

        // Convert the Opaque to FIDL and back.
        let fidl_capability: fsandbox::Capability = opaque.into();

        let any: AnyCapability =
            fidl_capability.try_into().context("failed to convert from FIDL")?;
        let opaque: Opaque<SomeType> = any.try_into().unwrap();

        // The value should be the same.
        let got_value: SomeType = opaque.0;
        assert_eq!(got_value, SomeType);

        Ok(())
    }
}
