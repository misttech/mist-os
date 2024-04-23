// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{stash_store::StashStore, storage_store::StorageStore, Store},
    anyhow::{Context, Error},
    fidl_fuchsia_stash as fidl_stash,
    fuchsia_component::client::connect_to_protocol,
    std::collections::HashMap,
    wlan_stash_constants::POLICY_STASH_PREFIX,
};

pub use wlan_stash_constants::{
    self, Credential, NetworkIdentifier, PersistentData, PersistentStorageData, SecurityType,
    POLICY_STORAGE_ID,
};

/// Manages access to the persistent storage or saved network configs through Stash
pub struct PolicyStorage {
    root: StashStore,
}

impl PolicyStorage {
    /// Initialize new store with the ID provided by the Saved Networks Manager. The ID will
    /// identify stored values as being part of the same persistent storage.  `proxy_fn` is used to
    /// connect to stash if necessary.
    pub async fn new_with_id_and_proxy(
        id: &str,
        proxy_fn: impl FnOnce() -> Result<fidl_stash::SecureStoreProxy, Error>,
    ) -> Result<Self, Error> {
        let root = StashStore::from_secure_store_proxy(id, proxy_fn()?)?;
        Ok(Self { root })
    }

    #[allow(unused)]
    /// The id is both the ID for the new file backed storage and for stash.
    async fn load_storage_or_migrate(
        id: &str,
        proxy_fn: impl FnOnce() -> Result<fidl_stash::SecureStoreProxy, Error>,
    ) -> Result<Vec<PersistentStorageData>, Error> {
        let path = format!("/data/network-data.{id}");
        let store = StorageStore::new(&path);

        // If there is an error loading from the new version of storage, it means it hasn't been
        // create and should be loaded from stash.
        let load_err = match store.load() {
            Ok(networks) => return Ok(networks),
            Err(e) => e,
        };
        let mut stash_store = StashStore::from_secure_store_proxy(id, proxy_fn()?)?;
        // Try and read from Stash since store doesn't exist yet
        if let Ok(config) = stash_store.load().await {
            // Read the stash data and convert it to a flattened list of config data.
            let mut networks_list = Vec::new();
            for (id, legacy_configs) in config.into_iter() {
                let mut new_configs = legacy_configs
                    .into_iter()
                    .map(|c| PersistentStorageData::new_from_legacy_data(id.clone(), c));
                networks_list.extend(&mut new_configs);
            }

            // Write the data to the new storage.
            let write_succeeded = store
                .write(networks_list.clone())
                .inspect_err(|e| tracing::info!(?e, "Failed to write migrated saved networks"))
                .is_ok();
            if write_succeeded {
                // Delete from stash if writing was successful.
                stash_store
                    .delete_store()
                    .await
                    .context("Failed to delete stash store after migration")?;
                tracing::info!("Migrated saved networks from stash");
            }
            Ok(networks_list)
        } else {
            tracing::info!(?load_err, "Failed to read saved networks from file, will create new. ",);
            Ok(Vec::new())
        }
    }

    /// Like `new_with_id_and_proxy` but provides a default `proxy_fn`.
    pub async fn new_with_id(id: &str) -> Result<Self, Error> {
        Self::new_with_id_and_proxy(id, default_stash_proxy).await
    }

    /// Initialize new Stash with a provided proxy in order to mock stash in unit tests.
    pub fn new_with_stash(proxy: fidl_stash::StoreAccessorProxy) -> Self {
        Self { root: StashStore::new(proxy, POLICY_STASH_PREFIX) }
    }

    /// Update the network configs of a given network identifier to persistent storage, deleting
    /// the key entirely if the new list of configs is empty.
    pub async fn write(
        &self,
        id: &NetworkIdentifier,
        network_configs: &[PersistentData],
    ) -> Result<(), Error> {
        self.root.write(id, network_configs).await?;
        self.root.flush().await
    }

    /// Load all saved network configs from stash. Will create HashMap of network configs by SSID
    /// as saved in the stash. If something in stash can't be interpreted, we ignore it.
    pub async fn load(&self) -> Result<HashMap<NetworkIdentifier, Vec<PersistentData>>, Error> {
        self.root.load().await
    }

    /// Remove all saved values from the stash. It will delete everything under the root node,
    /// and anything else in the same stash but not under the root node would be ignored.
    pub async fn clear(&mut self) -> Result<(), Error> {
        self.root.delete_store().await
    }
}

fn default_stash_proxy() -> Result<fidl_stash::SecureStoreProxy, Error> {
    connect_to_protocol::<fidl_stash::SecureStoreMarker>().map_err(|e| e.into())
}

#[cfg(test)]
mod tests {
    #![allow(unused_variables)]
    #![allow(unused_imports)]

    use {
        super::*,
        crate::tests::{network_id, new_stash_id},
        fidl::endpoints::{create_proxy, create_request_stream},
        fidl_fuchsia_stash::{SecureStoreRequest, StoreAccessorRequest},
        fuchsia_async as fasync,
        futures::StreamExt,
        ieee80211::Ssid,
        rand::{
            distributions::{Alphanumeric, DistString as _},
            thread_rng,
        },
        std::{
            convert::TryFrom,
            sync::{
                atomic::{AtomicBool, Ordering},
                Arc,
            },
        },
    };

    /// The PSK provided must be the bytes form of the 64 hexadecimal character hash. This is a
    /// duplicate of a definition in wlan/wlancfg/src, since I don't think there's a good way to
    /// import just that constant.
    pub const PSK_BYTE_LEN: usize = 32;

    #[fuchsia::test]
    async fn write_and_read() {
        let stash = new_stash(&new_stash_id()).await;
        let cfg_id = NetworkIdentifier {
            ssid: Ssid::try_from("foo").unwrap().to_vec(),
            security_type: SecurityType::Wpa2,
        };
        let cfg = PersistentData {
            credential: Credential::Password(b"password".to_vec()),
            has_ever_connected: true,
        };

        // Save a network config to the stash
        stash.write(&cfg_id, &vec![cfg.clone()]).await.expect("Failed writing to stash");

        // Expect to read the same value back with the same key
        let cfgs_from_stash = stash.load().await.expect("Failed reading from stash");
        assert_eq!(1, cfgs_from_stash.len());
        assert_eq!(Some(&vec![cfg.clone()]), cfgs_from_stash.get(&cfg_id));

        // Overwrite the list of configs saved in stash
        let cfg_2 = PersistentData {
            credential: Credential::Password(b"other-password".to_vec()),
            has_ever_connected: false,
        };
        stash
            .write(&cfg_id, &vec![cfg.clone(), cfg_2.clone()])
            .await
            .expect("Failed writing to stash");

        // Expect to read the saved value back with the same key
        let cfgs_from_stash = stash.load().await.expect("Failed reading from stash");
        assert_eq!(1, cfgs_from_stash.len());
        let actual_configs = cfgs_from_stash.get(&cfg_id).unwrap();
        assert_eq!(2, actual_configs.len());
        assert!(actual_configs.contains(&cfg));
        assert!(actual_configs.contains(&cfg_2));
    }

    #[fuchsia::test]
    async fn write_read_security_types() {
        let stash = new_stash(&new_stash_id()).await;
        let password = Credential::Password(b"config-password".to_vec());

        // create and write configs with each security type
        let net_id_open = network_id("foo", SecurityType::None);
        let net_id_wep = network_id("foo", SecurityType::Wep);
        let net_id_wpa = network_id("foo", SecurityType::Wpa);
        let net_id_wpa2 = network_id("foo", SecurityType::Wpa2);
        let net_id_wpa3 = network_id("foo", SecurityType::Wpa3);
        let cfg_open = PersistentData { credential: Credential::None, has_ever_connected: false };
        let cfg_wep = PersistentData { credential: password.clone(), has_ever_connected: false };
        let cfg_wpa = PersistentData { credential: password.clone(), has_ever_connected: false };
        let cfg_wpa2 = PersistentData { credential: password.clone(), has_ever_connected: false };
        let cfg_wpa3 = PersistentData { credential: password.clone(), has_ever_connected: false };

        stash.write(&net_id_open, &vec![cfg_open.clone()]).await.expect("failed to write config");
        stash.write(&net_id_wep, &vec![cfg_wep.clone()]).await.expect("failed to write config");
        stash.write(&net_id_wpa, &vec![cfg_wpa.clone()]).await.expect("failed to write config");
        stash.write(&net_id_wpa2, &vec![cfg_wpa2.clone()]).await.expect("failed to write config");
        stash.write(&net_id_wpa3, &vec![cfg_wpa3.clone()]).await.expect("failed to write config");

        // load stash and expect each config that we wrote
        let configs = stash.load().await.expect("failed loading from stash");
        assert_eq!(Some(&vec![cfg_open]), configs.get(&net_id_open));
        assert_eq!(Some(&vec![cfg_wep]), configs.get(&net_id_wep));
        assert_eq!(Some(&vec![cfg_wpa]), configs.get(&net_id_wpa));
        assert_eq!(Some(&vec![cfg_wpa2]), configs.get(&net_id_wpa2));
        assert_eq!(Some(&vec![cfg_wpa3]), configs.get(&net_id_wpa3));
    }

    #[fuchsia::test]
    async fn write_read_credentials() {
        let stash = new_stash(&new_stash_id()).await;

        let net_id_none = network_id("bar-none", SecurityType::None);
        let net_id_password = network_id("bar-password", SecurityType::Wpa2);
        let net_id_psk = network_id("bar-psk", SecurityType::Wpa2);

        // create and write configs with each type credential
        let password = Credential::Password(b"config-password".to_vec());
        let psk = Credential::Psk([65; PSK_BYTE_LEN].to_vec());

        let cfg_none = PersistentData { credential: Credential::None, has_ever_connected: false };
        let cfg_password = PersistentData { credential: password, has_ever_connected: false };
        let cfg_psk = PersistentData { credential: psk, has_ever_connected: false };

        // write each config to stash, then check that we see them when we load
        stash.write(&net_id_none, &vec![cfg_none.clone()]).await.expect("failed to write");
        stash.write(&net_id_password, &vec![cfg_password.clone()]).await.expect("failed to write");
        stash.write(&net_id_psk, &vec![cfg_psk.clone()]).await.expect("failed to write");

        let configs = stash.load().await.expect("failed loading from stash");
        assert_eq!(Some(&vec![cfg_none]), configs.get(&net_id_none));
        assert_eq!(Some(&vec![cfg_password]), configs.get(&net_id_password));
        assert_eq!(Some(&vec![cfg_psk]), configs.get(&net_id_psk));
    }

    #[fuchsia::test]
    async fn write_persists() {
        let stash_id = &new_stash_id();
        let stash = new_stash(&stash_id).await;
        let cfg_id = NetworkIdentifier {
            ssid: Ssid::try_from("foo").unwrap().to_vec(),
            security_type: SecurityType::Wpa2,
        };
        let cfg = PersistentData {
            credential: Credential::Password(b"password".to_vec()),
            has_ever_connected: true,
        };

        // Save a network config to the stash
        stash.write(&cfg_id, &vec![cfg.clone()]).await.expect("Failed writing to stash");

        // Create the stash again with same id
        let stash = PolicyStorage::new_with_id(stash_id).await.expect("Failed to create new stash");

        // Expect to read the same value back with the same key, should exist in new stash
        let cfgs_from_stash = stash.load().await.expect("Failed reading from stash");
        assert_eq!(1, cfgs_from_stash.len());
        assert_eq!(Some(&vec![cfg.clone()]), cfgs_from_stash.get(&cfg_id));
    }

    #[fuchsia::test]
    async fn load_stash() {
        let store = new_stash(&new_stash_id()).await;
        let foo_net_id = NetworkIdentifier {
            ssid: Ssid::try_from("foo").unwrap().to_vec(),
            security_type: SecurityType::Wpa2,
        };
        let cfg_foo = PersistentData {
            credential: Credential::Password(b"12345678".to_vec()),
            has_ever_connected: true,
        };
        let bar_net_id = NetworkIdentifier {
            ssid: Ssid::try_from("bar").unwrap().to_vec(),
            security_type: SecurityType::Wpa2,
        };
        let cfg_bar = PersistentData {
            credential: Credential::Password(b"qwertyuiop".to_vec()),
            has_ever_connected: true,
        };

        // Store two networks in our stash.
        store
            .write(&foo_net_id, &vec![cfg_foo.clone()])
            .await
            .expect("Failed to save config to stash");
        store
            .write(&bar_net_id, &vec![cfg_bar.clone()])
            .await
            .expect("Failed to save config to stash");

        // load should give us a hashmap with the two networks we saved
        let mut expected_cfgs = HashMap::new();
        expected_cfgs.insert(foo_net_id.clone(), vec![cfg_foo]);
        expected_cfgs.insert(bar_net_id.clone(), vec![cfg_bar]);
        assert_eq!(expected_cfgs, store.load().await.expect("Failed to load configs from stash"));
    }

    #[fuchsia::test]
    async fn load_stash_does_not_load_empty_list() {
        let stash_id = &new_stash_id();
        let stash = new_stash(&stash_id).await;

        // add a network identifier with no configs
        let net_id = NetworkIdentifier {
            ssid: Ssid::try_from(rand_string()).unwrap().to_vec(),
            security_type: SecurityType::Wpa2,
        };
        stash.write(&net_id, &vec![]).await.expect("failed to write value");

        // recreate the stash to load it
        let stash = new_stash(&stash_id).await;
        let loaded_configs = stash.load().await.expect("failed to load stash");
        assert!(loaded_configs.is_empty());
    }

    #[fuchsia::test]
    async fn clear_stash() {
        let stash_id = &new_stash_id();
        let mut stash = new_stash(&stash_id).await;

        // add some configs to the stash
        let net_id_foo = NetworkIdentifier {
            ssid: Ssid::try_from("foo").unwrap().to_vec(),
            security_type: SecurityType::Wpa2,
        };
        let cfg_foo = PersistentData {
            credential: Credential::Password(b"qwertyuio".to_vec()),
            has_ever_connected: true,
        };
        let net_id_bar = NetworkIdentifier {
            ssid: Ssid::try_from("bar").unwrap().to_vec(),
            security_type: SecurityType::Wpa2,
        };
        let cfg_bar = PersistentData {
            credential: Credential::Password(b"12345678".to_vec()),
            has_ever_connected: false,
        };
        stash.write(&net_id_foo, &vec![cfg_foo.clone()]).await.expect("Failed to write to stash");
        stash.write(&net_id_bar, &vec![cfg_bar.clone()]).await.expect("Failed to write to stash");

        // verify that the configs are found in stash
        let configs_from_stash = stash.load().await.expect("Failed to read");
        assert_eq!(2, configs_from_stash.len());
        assert_eq!(Some(&vec![cfg_foo.clone()]), configs_from_stash.get(&net_id_foo));
        assert_eq!(Some(&vec![cfg_bar.clone()]), configs_from_stash.get(&net_id_bar));

        // clear the stash
        stash.clear().await.expect("Failed to clear stash");
        // verify that the configs are no longer in the stash
        let configs_from_stash = stash.load().await.expect("Failed to read");
        assert_eq!(0, configs_from_stash.len());

        // recreate stash and verify that clearing the stash persists
        let stash = PolicyStorage::new_with_id(stash_id).await.expect("Failed to create new stash");
        let configs_from_stash = stash.load().await.expect("Failed to read");
        assert_eq!(0, configs_from_stash.len());
    }

    #[fuchsia::test]
    async fn test_migration() {
        let stash_id = new_stash_id();
        let store_client = connect_to_protocol::<fidl_stash::SecureStoreMarker>()
            .expect("failed to connect to store");
        store_client.identify(&stash_id).expect("failed to identify client to store");
        let (store_proxy, accessor_server) =
            create_proxy().expect("failed to create accessor proxy");
        store_client.create_accessor(false, accessor_server).expect("failed to create accessor");

        let ssid = "foo";
        let security_type = SecurityType::Wpa2;
        let credential = Credential::Password(b"password".to_vec());
        let has_ever_connected = false;
        // This is the version used by the previous storage mechanism.
        let network_id = network_id(ssid, security_type);
        let previous_data = PersistentData { credential: credential.clone(), has_ever_connected };

        let expected = vec![PersistentStorageData {
            ssid: ssid.into(),
            security_type: security_type,
            credential: credential.clone(),
            has_ever_connected,
        }];

        {
            let stash = PolicyStorage::new_with_stash(store_proxy);
            stash.write(&network_id, &vec![previous_data]).await.expect("write failed");
        }

        // Migrate the config.
        {
            let data = PolicyStorage::load_storage_or_migrate(&stash_id, default_stash_proxy)
                .await
                .expect("migrating stash data to create store failed");
            assert_eq!(data, expected);
        }

        // The config should have been deleted from Stash.
        {
            let (store_proxy, accessor_server) =
                create_proxy().expect("failed to create accessor proxy");
            store_client
                .create_accessor(false, accessor_server)
                .expect("failed to create accessor");
            let stash = PolicyStorage::new_with_stash(store_proxy);
            assert_eq!(stash.load().await.expect("load failed"), HashMap::new());
        }

        // And once more, but this time there should be no migration.
        {
            let configs = PolicyStorage::load_storage_or_migrate(&stash_id, default_stash_proxy)
                .await
                .expect("load failed");
            assert_eq!(configs, expected);
        }
    }

    #[fuchsia::test]
    async fn test_migration_with_bad_stash() {
        let stash_id = new_stash_id();

        let (client, mut request_stream) = create_request_stream::<fidl_stash::SecureStoreMarker>()
            .expect("create_request_stream failed");

        let read_from_stash = Arc::new(AtomicBool::new(false));

        let _task = {
            let read_from_stash = read_from_stash.clone();
            fasync::Task::spawn(async move {
                while let Some(request) = request_stream.next().await {
                    match request.unwrap() {
                        SecureStoreRequest::Identify { .. } => {}
                        SecureStoreRequest::CreateAccessor { accessor_request, .. } => {
                            let read_from_stash = read_from_stash.clone();
                            fuchsia_async::Task::spawn(async move {
                                let mut request_stream = accessor_request.into_stream().unwrap();
                                while let Some(request) = request_stream.next().await {
                                    match request.unwrap() {
                                        StoreAccessorRequest::ListPrefix { .. } => {
                                            read_from_stash.store(true, Ordering::Relaxed);
                                            // If we just drop the iterator, it should trigger a
                                            // read error.
                                        }
                                        _ => unreachable!(),
                                    }
                                }
                            })
                            .detach();
                        }
                    }
                }
            })
        };

        // Try and load the config.  It should provide empty config.
        let loaded_configs =
            PolicyStorage::load_storage_or_migrate(&stash_id, || Ok(client.into_proxy().unwrap()))
                .await
                .expect("new_with_id_and_proxy failed");
        assert_eq!(loaded_configs, Vec::new());

        // Make sure there was an attempt to actually read from stash.
        assert!(read_from_stash.load(Ordering::Relaxed));
    }

    /// Creates a new stash with the given ID and clears the values saved in the stash.
    pub async fn new_stash(stash_id: &str) -> PolicyStorage {
        let mut stash =
            PolicyStorage::new_with_id(stash_id).await.expect("Failed to create new stash");
        stash.clear().await.expect("failed to clear stash");
        stash
    }

    fn rand_string() -> String {
        Alphanumeric.sample_string(&mut thread_rng(), 20)
    }
}
