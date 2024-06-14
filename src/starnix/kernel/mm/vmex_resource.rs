// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fuchsia_component::client::connect_to_protocol_sync;
use once_cell::sync::Lazy;
use {fidl_fuchsia_kernel as fkernel, fuchsia_zircon as zx};

pub static VMEX_RESOURCE: Lazy<zx::Resource> = Lazy::new(|| {
    connect_to_protocol_sync::<fkernel::VmexResourceMarker>()
        .expect("couldn't connect to fuchsia.kernel.VmexResource")
        .get(zx::Time::INFINITE)
        .expect("couldn't talk to fuchsia.kernel.VmexResource")
});
