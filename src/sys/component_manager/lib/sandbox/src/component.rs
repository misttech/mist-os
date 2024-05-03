// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::CapabilityTrait;
use fidl_fuchsia_component_sandbox as fsandbox;
use fuchsia_async as fasync;
use fuchsia_zircon as zx;
use std::fmt::Debug;
use std::{any::Any, sync::Arc};

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

impl CapabilityTrait for WeakComponentToken {}

impl From<WeakComponentToken> for fsandbox::Capability {
    fn from(_component: WeakComponentToken) -> Self {
        todo!("b/337284929: Decide on if Component should be in Capability");
    }
}

impl WeakComponentToken {
    async fn serve(server: zx::EventPair) {
        fasync::OnSignals::new(&server, zx::Signals::OBJECT_PEER_CLOSED).await.ok();
    }

    pub fn register(self, koid: zx::Koid, server: zx::EventPair) {
        crate::registry::insert(self.into(), koid, WeakComponentToken::serve(server));
    }
}
