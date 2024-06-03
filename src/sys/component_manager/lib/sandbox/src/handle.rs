// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl::handle::Handle;
use std::sync::{Arc, Mutex};

#[cfg(target_os = "fuchsia")]
use {crate::registry, fidl_fuchsia_component_sandbox as fsandbox};

/// A capability that vends a single Zircon handle.
#[derive(Clone, Debug)]
pub struct OneShotHandle(Arc<Mutex<Option<Handle>>>);

impl OneShotHandle {
    /// Creates a new [OneShotHandle] containing a Zircon `handle`.
    pub fn new(handle: Handle) -> Self {
        Self(Arc::new(Mutex::new(Some(handle))))
    }

    /// Returns the handle in this [OneShotHandle], taking it out.
    ///
    /// Subsequent calls will return `None`.
    pub fn take(&self) -> Option<Handle> {
        self.0.lock().unwrap().take()
    }
}

impl From<Handle> for OneShotHandle {
    fn from(handle: Handle) -> Self {
        OneShotHandle(Arc::new(Mutex::new(Some(handle))))
    }
}

#[cfg(target_os = "fuchsia")]
impl crate::CapabilityTrait for OneShotHandle {}

#[cfg(target_os = "fuchsia")]
impl From<OneShotHandle> for fsandbox::OneShotHandle {
    fn from(value: OneShotHandle) -> Self {
        fsandbox::OneShotHandle { token: registry::insert_token(value.into()) }
    }
}

#[cfg(target_os = "fuchsia")]
impl From<OneShotHandle> for fsandbox::Capability {
    fn from(one_shot: OneShotHandle) -> Self {
        Self::Handle(one_shot.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Capability;
    use anyhow::{Context, Result};
    use assert_matches::assert_matches;
    use fidl::handle::{AsHandleRef, Event, HandleBased, Rights};
    use fidl_fuchsia_component_sandbox as fsandbox;

    #[fuchsia::test]
    fn construction() {
        let event = Event::create();
        let one_shot = OneShotHandle::new(event.into_handle());
        assert_matches!(one_shot.take(), Some(_));
        assert_matches!(one_shot.take(), None);
    }

    // Tests converting the OneShotHandle to FIDL and back.
    #[fuchsia::test]
    async fn one_shot_into_fidl() -> Result<()> {
        let event = Event::create();
        let expected_koid = event.get_koid().unwrap();

        let one_shot = OneShotHandle::from(event.into_handle());

        // Convert the OneShotHandle to FIDL and back.
        let fidl_capability: fsandbox::Capability = one_shot.into();
        assert_matches!(&fidl_capability, fsandbox::Capability::Handle(_));

        let any: Capability = fidl_capability.try_into().context("failed to convert from FIDL")?;
        let one_shot = assert_matches!(any, Capability::OneShotHandle(h) => h);

        // Get the handle.
        let handle = one_shot.take().unwrap();

        // The handle should be for same Event that was in the original OneShotHandle.
        let got_koid = handle.get_koid().unwrap();
        assert_eq!(got_koid, expected_koid);

        Ok(())
    }

    /// Tests that a OneShotHandle can be cloned by cloning the token.
    #[fuchsia::test]
    async fn fidl_clone() -> Result<()> {
        let event = Event::create();
        let expected_koid = event.get_koid().unwrap();

        let one_shot = OneShotHandle::from(event.into_handle());
        let one_shot_clone = one_shot.clone();
        let fidl_handle: fsandbox::OneShotHandle = one_shot.into();

        // Clone the HandleCapability by cloning the token.
        let token_clone = fsandbox::OneShotHandle {
            token: fidl_handle.token.duplicate_handle(Rights::SAME_RIGHTS).unwrap(),
        };

        // Get the handle from the clone.
        let any: Capability = fsandbox::Capability::Handle(token_clone)
            .try_into()
            .context("failed to convert from FIDL")?;
        let one_shot = assert_matches!(any, Capability::OneShotHandle(h) => h);

        let handle = one_shot.take().expect("failed to call GetHandle");

        // The handle should be for same Event that was in the original OneShotHandle.
        let got_koid = handle.get_koid().unwrap();
        assert_eq!(got_koid, expected_koid);

        // The original OneShotHandle should now not have a handle because it was taken
        // out by the GetHandle call on the clone.
        assert_matches!(one_shot_clone.take(), None);

        Ok(())
    }
}
