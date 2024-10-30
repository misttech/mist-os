// Copyright 2024 The Fuchsia Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{self, Error};
use android_system_microfuchsia_vm_service::aidl::android::system::microfuchsia::vm_service::IMicrofuchsia::GUEST_PORT;

mod binder_proxy;
mod microfuchsia_control;

extern "C" {
    fn register_dev_urandom_compat() -> zx::sys::zx_status_t;
}

#[fuchsia::main]
fn main() -> Result<(), Error> {
    tracing::info!("binder-proxy main");
    // Call register_dev_urandom_compat
    let register_status = zx::Status::from_raw(unsafe { register_dev_urandom_compat() });
    if register_status != zx::Status::OK {
        anyhow::bail!("Could not register /dev/urandom compatibility device: {register_status}");
    }

    let binder_proxy = binder_proxy::BinderProxy::new(GUEST_PORT as u32)?;
    binder_proxy.run()?;
    Ok(())
}
