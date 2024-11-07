// Copyright 2024 The Fuchsia Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use android_system_microfuchsia_vm_service::{aidl::android::system::microfuchsia::vm_service::{IHostProxy::IHostProxy, IMicrofuchsia::{self, BnMicrofuchsia}}, binder::{BinderFeatures, SpIBinder}};
use binder;

pub struct Microfuchsia {}

impl binder::Interface for Microfuchsia {}

impl IMicrofuchsia::IMicrofuchsia for Microfuchsia {
    fn setHostProxy(&self, _arg_proxy: &binder::Strong<dyn IHostProxy>) -> binder::Result<()> {
        todo!()
    }
}

impl Microfuchsia {
    pub fn new() -> Self {
        Self {}
    }
}

pub fn new_binder() -> SpIBinder {
    let accessor = Microfuchsia::new();
    BnMicrofuchsia::new_binder(accessor, BinderFeatures::default()).as_binder()
}
