// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! High level node abstraction on top of Fuchsia's Stash service.
//! Allows to insert structured, hierarchical data into a Stash backed
//! store.

use {
    anyhow::Error,
    async_trait::async_trait,
    std::collections::HashMap,
    wlan_stash_constants::{NetworkIdentifier, PersistentData},
};

pub mod policy;
mod stash_store;
mod storage_store;

#[async_trait]
trait Store: Send + Sync {
    /// Flushes all changes. Blocks until the flush is complete.
    async fn flush(&self) -> Result<(), Error>;

    /// Deletes the whole store.
    async fn delete_store(&mut self) -> Result<(), Error>;

    /// Update the network configs of a given network identifier to persistent storage, deleting the
    /// key entirely if the new list of configs is empty.  This does *not* flush the change.
    async fn write(
        &self,
        id: &NetworkIdentifier,
        network_configs: &[PersistentData],
    ) -> Result<(), Error>;

    /// Load all saved network configs from stash. Will create HashMap of network configs by SSID
    /// as saved in the stash. If something in stash can't be interpreted, we ignore it.
    async fn load(&self) -> Result<HashMap<NetworkIdentifier, Vec<PersistentData>>, Error>;
}

#[cfg(test)]
mod tests {
    use {
        fuchsia_zircon::AsHandleRef,
        rand::{
            distributions::{Alphanumeric, DistString as _},
            thread_rng,
        },
        std::sync::atomic::{AtomicU64, Ordering},
        wlan_stash_constants::{NetworkIdentifier, SecurityType, StashedSsid},
    };

    pub fn rand_string() -> String {
        Alphanumeric.sample_string(&mut thread_rng(), 20)
    }

    /// Returns a new ID that is guaranteed to be unique (which is required since the tests run in
    /// parallel).
    pub fn new_stash_id() -> String {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        format!(
            "{}-{}",
            fuchsia_runtime::process_self().get_koid().unwrap().raw_koid(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        )
    }

    pub fn network_id(
        ssid: impl Into<StashedSsid>,
        security_type: SecurityType,
    ) -> NetworkIdentifier {
        NetworkIdentifier { ssid: ssid.into(), security_type }
    }
}
