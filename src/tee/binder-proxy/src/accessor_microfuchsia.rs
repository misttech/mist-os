// Copyright 2024 The Fuchsia Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use android_system_microfuchsia_vm_service::{aidl::android::system::microfuchsia::vm_service::IAccessorMicrofuchsia::{self, BnAccessorMicrofuchsia}, binder::{BinderFeatures, SpIBinder}};
use binder;

pub struct AccessorMicrofuchsia {}

impl binder::Interface for AccessorMicrofuchsia {}

impl IAccessorMicrofuchsia::IAccessorMicrofuchsia for AccessorMicrofuchsia {}

impl AccessorMicrofuchsia {
    pub fn new() -> Self {
        Self {}
    }
}

pub fn new_binder() -> SpIBinder {
    let accessor = AccessorMicrofuchsia::new();
    BnAccessorMicrofuchsia::new_binder(accessor, BinderFeatures::default()).as_binder()
}
