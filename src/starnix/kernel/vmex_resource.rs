// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl_fuchsia_kernel as fkernel;
use fuchsia_component::client::connect_channel_to_protocol;
use fuchsia_zircon as zx;
use once_cell::sync::Lazy;

pub static VMEX_RESOURCE: Lazy<zx::Resource> = Lazy::new(|| {
    let (client_end, server_end) = zx::Channel::create();
    connect_channel_to_protocol::<fkernel::VmexResourceMarker>(server_end)
        .expect("couldn't connect to fuchsia.kernel.VmexResource");
    let service = fkernel::VmexResourceSynchronousProxy::new(client_end);
    service.get(zx::Time::INFINITE).expect("couldn't talk to fuchsia.kernel.VmexResource")
});
