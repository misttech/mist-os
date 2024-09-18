// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use block_client::{BlockClient as _, RemoteBlockClient, VmoId};
use std::collections::HashMap;
use std::ops::Deref;
use std::sync::{Arc, Mutex, Weak};
use {fuchsia_async as fasync, fuchsia_zircon as zx};

/// Wraps `RemoteBlockClient` and manages registered VMO ids.
pub struct Device {
    client: RemoteBlockClient,
    vmo_ids: Mutex<HashMap<usize, Arc<VmoIdWrapper>>>,
}

impl Device {
    pub fn new(client: RemoteBlockClient) -> Self {
        Self { client, vmo_ids: Mutex::default() }
    }

    /// Ataches `vmo`.  NOTE: This assumes that the pointer &zx::Vmo will remain stable.
    pub async fn attach_vmo(self: &Arc<Self>, vmo: &zx::Vmo) -> Result<(), zx::Status> {
        let vmo_id = self.client.attach_vmo(vmo).await?;
        assert!(
            self.vmo_ids
                .lock()
                .unwrap()
                .insert(
                    vmo as *const _ as usize,
                    Arc::new(VmoIdWrapper(Arc::downgrade(self), vmo_id))
                )
                .is_none(),
            "VMO already attached!"
        );
        Ok(())
    }

    /// Deteaches `vmo`.  NOTE: The pointer `&zx::Vmo` must match that used in `attach_vmo`.
    pub fn detach_vmo(&self, vmo: &zx::Vmo) {
        // This won't immediately detach because it might still be in-use, but as soon as all uses
        // finish, it will get detached.
        self.vmo_ids.lock().unwrap().remove(&(vmo as *const _ as usize));
    }

    /// Returns the VMO ID registered the given vmo.
    ///
    /// # Panics
    ///
    /// Panics if we don't know about `vmo` i.e. `attach_vmo` above was not called.
    pub fn get_vmo_id(&self, vmo: &zx::Vmo) -> Arc<VmoIdWrapper> {
        self.vmo_ids.lock().unwrap()[&(vmo as *const _ as usize)].clone()
    }
}

impl Deref for Device {
    type Target = RemoteBlockClient;
    fn deref(&self) -> &Self::Target {
        &self.client
    }
}

pub struct VmoIdWrapper(Weak<Device>, VmoId);

impl Drop for VmoIdWrapper {
    fn drop(&mut self) {
        // Turn it into an ID so that if the spawned task is dropped, an assertion doesn't fire.  It
        // will mean the ID is leaked, but it's most likely that the server is being shut down
        // anyway so it shouldn't matter.
        let vmo_id = self.1.take().into_id();
        if let Some(device) = self.0.upgrade() {
            fasync::Task::spawn(async move {
                let _ = device.client.detach_vmo(VmoId::new(vmo_id)).await;
            })
            .detach();
        }
    }
}

impl Deref for VmoIdWrapper {
    type Target = VmoId;
    fn deref(&self) -> &Self::Target {
        &self.1
    }
}
