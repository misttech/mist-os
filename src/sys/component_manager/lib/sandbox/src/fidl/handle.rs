// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{fidl::registry, OneShotHandle},
    fidl_fuchsia_component_sandbox as fsandbox,
};

impl crate::CapabilityTrait for OneShotHandle {}

impl From<OneShotHandle> for fsandbox::OneShotHandle {
    fn from(value: OneShotHandle) -> Self {
        fsandbox::OneShotHandle { token: registry::insert_token(value.into()) }
    }
}

impl From<OneShotHandle> for fsandbox::Capability {
    fn from(one_shot: OneShotHandle) -> Self {
        Self::Handle(one_shot.into())
    }
}
