// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl_fuchsia_component_sandbox as fsandbox;
use std::fmt::Debug;
use std::{any::Any, sync::Arc};

#[cfg(target_os = "fuchsia")]
use {
    fidl::handle::{EventPair, Signals},
    fuchsia_async as fasync,
    fuchsia_zircon::Koid,
};

/// The trait that `WeakComponentToken` holds.
pub trait WeakComponentTokenAny: Debug + Send + Sync {
    fn as_any(&self) -> &dyn Any;
}

/// A type representing a weak pointer to a component.
/// This is type erased because the bedrock library shouldn't depend on
/// Component Manager types.
#[derive(Clone, Debug)]
pub struct WeakComponentToken {
    pub inner: Arc<dyn WeakComponentTokenAny>,
}

#[cfg(target_os = "fuchsia")]
impl crate::CapabilityTrait for WeakComponentToken {}

impl From<WeakComponentToken> for fsandbox::Capability {
    fn from(_component: WeakComponentToken) -> Self {
        todo!("b/337284929: Decide on if Component should be in Capability");
    }
}

#[cfg(target_os = "fuchsia")]
impl WeakComponentToken {
    async fn serve(server: EventPair) {
        fasync::OnSignals::new(&server, Signals::OBJECT_PEER_CLOSED).await.ok();
    }

    pub fn register(self, koid: Koid, server: EventPair) {
        crate::registry::insert(self.into(), koid, WeakComponentToken::serve(server));
    }
}
