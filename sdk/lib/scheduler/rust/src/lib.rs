// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::{Context, Error},
    fidl_fuchsia_scheduler::{
        RoleManagerMarker, RoleManagerSetRoleRequest, RoleManagerSynchronousProxy, RoleName,
        RoleTarget,
    },
    fuchsia_component::client::connect_to_protocol_sync,
    fuchsia_runtime,
    fuchsia_sync::RwLock,
    fuchsia_zircon::{HandleBased, Rights, Status, Thread, Time},
    std::sync::Arc,
};

static ROLE_MANAGER: RwLock<Option<Arc<RoleManagerSynchronousProxy>>> = RwLock::new(None);

fn connect() -> Result<Arc<RoleManagerSynchronousProxy>, Error> {
    // If the proxy has been connected already, return.
    if let Some(ref proxy) = *ROLE_MANAGER.read() {
        return Ok(Arc::clone(&proxy));
    }

    // Acquire the write lock and make sure no other thread connected to the proxy while we
    // were waiting on the write lock.
    let mut proxy = ROLE_MANAGER.write();
    if let Some(ref proxy) = *proxy {
        return Ok(Arc::clone(&proxy));
    }

    // Connect to the synchronous proxy.
    let p = Arc::new(connect_to_protocol_sync::<RoleManagerMarker>()?);
    *proxy = Some(Arc::clone(&p));
    Ok(p)
}

fn disconnect() {
    // Drop our connection to the proxy by setting it to None.
    let mut proxy = ROLE_MANAGER.write();
    *proxy = None;
}

pub fn set_role_for_thread(thread: &Thread, role_name: &str) -> Result<(), Error> {
    let role_manager = connect()?;
    let thread = thread
        .duplicate_handle(Rights::SAME_RIGHTS)
        .context("Failed to duplicate thread handle")?;
    let request = RoleManagerSetRoleRequest {
        target: Some(RoleTarget::Thread(thread)),
        role: Some(RoleName { role: role_name.to_string() }),
        ..Default::default()
    };
    let _ = role_manager
        .set_role(request, Time::INFINITE)
        .context("fuchsia.scheduler.RoleManager::SetRole failed")
        .and_then(|result| {
            match result {
                Ok(_) => Ok(()),
                Err(status) => {
                    // If the server responded with ZX_ERR_PEER_CLOSED, mark the synchronous proxy as
                    // disconnected so future invocations of this function reconnect to the RoleManager.
                    if status == Status::PEER_CLOSED.into_raw() {
                        disconnect();
                    }
                    Status::ok(status).context(format!(
                        "fuchsia.scheduler.RoleManager::SetRole returned error: {:?}",
                        status
                    ))
                }
            }
        })?;
    Ok(())
}

pub fn set_role_for_this_thread(role_name: &str) -> Result<(), Error> {
    set_role_for_thread(&fuchsia_runtime::thread_self(), role_name)
}
