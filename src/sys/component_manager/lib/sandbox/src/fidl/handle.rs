// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::Handle;
use fidl_fuchsia_component_sandbox as fsandbox;

impl crate::RemotableCapability for Handle {}

impl From<Handle> for fsandbox::Capability {
    fn from(handle: Handle) -> Self {
        Self::Handle(handle.into())
    }
}
