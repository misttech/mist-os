// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::stash_store::StashStore;
use crate::storage_store::StorageStore;
use anyhow::{format_err, Context, Error};
use fidl_fuchsia_stash as fidl_stash;
use fuchsia_component::client::connect_to_protocol;

pub use wlan_stash_constants::{
    self, Credential, NetworkIdentifier, PersistentData, PersistentStorageData, SecurityType,
    POLICY_STORAGE_ID,
};

/// If the store ID is saved_networks, the file will be saved at /data/network-data.saved_networks
const FILE_PATH_FORMAT: &str = "/data/network-data.";

/// Manages access to the persistent storage. This layer on top of storage just migrates the legacy
/// persisted data if it hasn't been migrated yet.
pub struct PolicyStorage {
    /// This is the actual store.
    root: StorageStore,
    /// This is used to get legacy data, and should only ever be used once, when migrating the data
    /// the first time. The result is carried here because we only care about any errors if we end
    /// up migrating the data.
    legacy_stash: Result<StashStore, Error>,
}

impl PolicyStorage {
    /// Initialize new store with the ID provided by the Saved Networks Manager. The ID will
    /// identify stored values as being part of the same persistent storage.
    pub fn new_with_id(id: &str) -> Self {
        let path = format!("{FILE_PATH_FORMAT}{id}");
        let root = StorageStore::new(&path);

        let proxy = connect_to_protocol::<fidl_stash::SecureStoreMarker>();
        let legacy_stash =
            proxy.and_then(|p| StashStore::from_secure_store_proxy(id, p)).map_err(|e| e);

        Self { root, legacy_stash }
    }

    /// Initializer for tests outside of this module
    pub fn new_with_stash_proxy_and_id(
        stash_proxy: fidl_stash::SecureStoreProxy,
        id: &str,
    ) -> Self {
        let root = StorageStore::new(&id);
        let stash = StashStore::from_secure_store_proxy(id, stash_proxy);
        Self { root, legacy_stash: stash }
    }

    /// Initialize the storage wrapper and load all saved network configs from persistent storage.
    /// If there is an error loading from local storage, that may mean stash data hasn't been
    /// migrated yet. If so, the function will try to load from legacy stash data.
    pub async fn load(&mut self) -> Result<Vec<PersistentStorageData>, Error> {
        // If there is an error loading from the new version of storage, it means it hasn't been
        // create and should be loaded from stash.
        let load_err = match self.root.load() {
            Ok(networks) => return Ok(networks),
            Err(e) => e,
        };

        let stash_store: &mut StashStore = if let Ok(stash) = self.legacy_stash.as_mut() {
            stash
        } else {
            return Err(format_err!("error accessing stash"));
        };
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
            let write_succeeded = self
                .root
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
            // The backing file is only actually created when a write happens, but we
            // don't want to intentionally create a file if migrating stash fails since
            // then we will never try to read from stash again.
            tracing::info!(?load_err, "Failed to read saved networks from file and legacy stash, new file will be created when a network is saved",);
            Ok(Vec::new())
        }
    }

    /// Update the network configs of a given network identifier to persistent storage, deleting
    /// the key entirely if the new list of configs is empty.
    pub fn write(&self, network_configs: Vec<PersistentStorageData>) -> Result<(), Error> {
        self.root.write(network_configs)
    }

    /// Remove all saved values from the stash. It will delete everything under the root node,
    /// and anything else in the same stash but not under the root node would be ignored.
    pub fn clear(&mut self) -> Result<(), Error> {
        self.root.empty_store()
    }
}

#[cfg(test)]
mod tests {
    #![allow(unused_variables)]
    #![allow(unused_imports)]

    use super::*;
    use crate::tests::{network_id, rand_string};
    use fidl::endpoints::{create_proxy, create_request_stream};
    use fidl_fuchsia_stash::{SecureStoreRequest, StoreAccessorRequest};
    use fuchsia_async as fasync;
    use futures::StreamExt;
    use ieee80211::Ssid;
    use rand::distributions::{Alphanumeric, DistString as _};
    use rand::thread_rng;
    use std::convert::TryFrom;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use wlan_stash_constants::PersistentData;

    /// The PSK provided must be the bytes form of the 64 hexadecimal character hash. This is a
    /// duplicate of a definition in wlan/wlancfg/src, since I don't think there's a good way to
    /// import just that constant.
    pub const PSK_BYTE_LEN: usize = 32;

    #[fuchsia::test]
    async fn write_and_read() {
        let mut store = new_storage(&rand_string()).await;
        let stash_id = rand_string();
        let cfg = PersistentStorageData {
            ssid: Ssid::try_from("foo").unwrap().to_vec(),
            security_type: SecurityType::Wpa2,
            credential: Credential::Password(b"password".to_vec()),
            has_ever_connected: true,
        };

        // Save a network config to storage
        store.write(vec![cfg.clone()]).expect("Failed writing to storage");

        // Expect to read the same value back with the same key
        let cfgs_from_store = store.load().await.expect("Failed reading from storage");
        assert_eq!(1, cfgs_from_store.len());
        assert_eq!(vec![cfg.clone()], cfgs_from_store);

        // Overwrite the list of configs saved in storage
        let cfg_2 = PersistentStorageData {
            ssid: Ssid::try_from("foo").unwrap().to_vec(),
            security_type: SecurityType::Wpa2,
            credential: Credential::Password(b"other-password".to_vec()),
            has_ever_connected: false,
        };
        store.write(vec![cfg.clone(), cfg_2.clone()]).expect("Failed writing to storage");

        // Expect to read the saved value back
        let cfgs_from_store = store.load().await.expect("Failed reading from stash");
        assert_eq!(2, cfgs_from_store.len());
        assert!(cfgs_from_store.contains(&cfg));
        assert!(cfgs_from_store.contains(&cfg_2));
    }

    #[fuchsia::test]
    async fn write_read_security_types() {
        let mut store = new_storage(&rand_string()).await;
        let stash_id = rand_string();
        let password = Credential::Password(b"config-password".to_vec());

        // create and write configs with each security type
        let cfg_open = PersistentStorageData {
            ssid: Ssid::try_from("foo").unwrap().to_vec(),
            security_type: SecurityType::None,
            credential: Credential::None,
            has_ever_connected: false,
        };
        let cfg_wep = PersistentStorageData {
            ssid: Ssid::try_from("foo").unwrap().to_vec(),
            security_type: SecurityType::Wep,
            credential: password.clone(),
            has_ever_connected: false,
        };
        let cfg_wpa = PersistentStorageData {
            ssid: Ssid::try_from("foo").unwrap().to_vec(),
            security_type: SecurityType::Wpa,
            credential: password.clone(),
            has_ever_connected: false,
        };
        let cfg_wpa2 = PersistentStorageData {
            ssid: Ssid::try_from("foo").unwrap().to_vec(),
            security_type: SecurityType::Wpa2,
            credential: password.clone(),
            has_ever_connected: false,
        };
        let cfg_wpa3 = PersistentStorageData {
            ssid: Ssid::try_from("foo").unwrap().to_vec(),
            security_type: SecurityType::Wpa3,
            credential: password.clone(),
            has_ever_connected: false,
        };

        // Write the saved network list as it would be each time a new network was added.
        let mut saved_networks = vec![cfg_open.clone()];
        store.write(saved_networks.clone()).expect("failed to write config");
        saved_networks.push(cfg_wep.clone());
        store.write(saved_networks.clone()).expect("failed to write config");
        saved_networks.push(cfg_wpa.clone());
        store.write(saved_networks.clone()).expect("failed to write config");
        saved_networks.push(cfg_wpa2.clone());
        store.write(saved_networks.clone()).expect("failed to write config");
        saved_networks.push(cfg_wpa3.clone());
        store.write(saved_networks.clone()).expect("failed to write config");

        // Load storage and expect each config that we wrote.
        let configs = store.load().await.expect("failed loading from storage");
        assert_eq!(configs.len(), 5);
        assert!(configs.contains(&cfg_open));
        assert!(configs.contains(&cfg_wep));
        assert!(configs.contains(&cfg_wpa));
        assert!(configs.contains(&cfg_wpa2));
        assert!(configs.contains(&cfg_wpa3));
    }

    #[fuchsia::test]
    async fn write_read_credentials() {
        let mut store = new_storage(&rand_string()).await;
        let stash_id = rand_string();

        // Create and write configs with each type credential.
        let password = Credential::Password(b"config-password".to_vec());
        let psk = Credential::Psk([65; PSK_BYTE_LEN].to_vec());

        let cfg_none = PersistentStorageData {
            ssid: b"bar-none".to_vec(),
            security_type: SecurityType::None,
            credential: Credential::None,
            has_ever_connected: false,
        };
        let cfg_password = PersistentStorageData {
            ssid: b"bar-password".to_vec(),
            security_type: SecurityType::Wpa2,
            credential: password,
            has_ever_connected: false,
        };
        let cfg_psk = PersistentStorageData {
            ssid: b"bar-psk".to_vec(),
            security_type: SecurityType::Wpa2,
            credential: psk,
            has_ever_connected: false,
        };

        // Write configs to storage as they would be when saved.
        let mut saved_networks = vec![cfg_none.clone()];
        store.write(saved_networks.clone()).expect("failed to write");
        saved_networks.push(cfg_password.clone());
        store.write(saved_networks.clone()).expect("failed to write");
        saved_networks.push(cfg_psk.clone());
        store.write(saved_networks.clone()).expect("failed to write");

        // Check that the configs are loaded correctly.
        let configs = store.load().await.expect("failed loading from storage");
        assert_eq!(3, configs.len());
        assert!(configs.contains(&cfg_none));
        assert!(configs.contains(&cfg_password));
        assert!(configs.contains(&cfg_psk));
    }

    #[fuchsia::test]
    async fn write_persists() {
        let path = rand_string();
        let stash_id = rand_string();
        let store = new_storage(&path).await;
        let cfg = PersistentStorageData {
            ssid: Ssid::try_from("foo").unwrap().to_vec(),
            security_type: SecurityType::Wpa2,
            credential: Credential::Password(b"password".to_vec()),
            has_ever_connected: true,
        };

        // Save a network config to the stash
        store.write(vec![cfg.clone()]).expect("Failed writing to storage");

        // Create the storage again with same id
        let mut store = PolicyStorage::new_with_id(&path);

        // Expect to read the same value back with the same key, should exist in new stash
        let cfgs_from_store = store.load().await.expect("Failed reading from storage");
        assert_eq!(1, cfgs_from_store.len());
        assert!(cfgs_from_store.contains(&cfg));
    }

    #[fuchsia::test]
    async fn load_storage() {
        let mut store = new_storage(&rand_string()).await;
        let stash_id = rand_string();
        let cfg_foo = PersistentStorageData {
            ssid: Ssid::try_from("foo").unwrap().to_vec(),
            security_type: SecurityType::Wpa2,
            credential: Credential::Password(b"12345678".to_vec()),
            has_ever_connected: true,
        };
        let cfg_bar = PersistentStorageData {
            ssid: Ssid::try_from("bar").unwrap().to_vec(),
            security_type: SecurityType::Wpa2,
            credential: Credential::Password(b"qwertyuiop".to_vec()),
            has_ever_connected: true,
        };

        // Store two networks in our stash.
        store
            .write(vec![cfg_foo.clone(), cfg_bar.clone()])
            .expect("Failed to save configs to stash");

        // load should give us the two networks we saved
        let expected_cfgs = vec![cfg_foo, cfg_bar];
        assert_eq!(expected_cfgs, store.load().await.expect("Failed to load configs from stash"));
    }

    #[fuchsia::test]
    async fn load_empty_storage_does_loads_empty_list() {
        let store_id = &rand_string();
        let mut store = new_storage(&store_id).await;

        // write to storage an empty saved networks list
        store.write(vec![]).expect("failed to write value");

        // recreate the storage to load it
        let stash = new_storage(&store_id).await;
        let loaded_configs = store.load().await.expect("failed to load store");
        assert!(loaded_configs.is_empty());
    }

    #[fuchsia::test]
    pub async fn load_no_file_creates_file() {
        // Test what would happen if policy persistent storage is loaded twice - the first attempt
        // should initialize the backing file. The second attempt should not attempt to load from
        // the legacy stash.
        let store_id = &rand_string();
        let backing_file_path = format!("{}{}", FILE_PATH_FORMAT, store_id).to_string();
        let mut store = PolicyStorage::new_with_id(store_id);

        // The file should not exist yet, so reading it would give an error.
        std::fs::read(&backing_file_path).expect_err("The file for the store should not exist yet");

        // Load the store.
        let loaded_configs = store.load().await.expect("failed to load store");
        assert_eq!(loaded_configs, vec![]);

        // Check that the file is created. It should have some JSON structure even though there
        // are no saved networks.
        let file_contents = std::fs::read(&backing_file_path).expect(
            "Failed to read file that should have been created when loading non-existant file",
        );
        assert!(!file_contents.is_empty());

        // Load the store again, but with some values in the legacy stash which should be ignored.
        let cfg_id = NetworkIdentifier {
            ssid: Ssid::try_from(rand_string().as_str()).unwrap().to_vec(),
            security_type: SecurityType::Wpa2,
        };
        let cfg = PersistentData {
            credential: Credential::Password(rand_string().as_bytes().to_vec()),
            has_ever_connected: true,
        };

        match store.legacy_stash.as_mut() {
            Ok(stash) => {
                stash.write(&cfg_id, &[cfg]).await.expect("Failed writing to legacy stash");
                stash.flush().await.expect("Failed to flush legacy stash");
            }
            Err(e) => {
                panic!("error initializing legacy stash: {}", e);
            }
        }
        let loaded_configs = store.load().await.expect("failed to load store");
        assert!(loaded_configs.is_empty());

        // The file should still exist.
        let file_contents = std::fs::read(&backing_file_path).expect(
            "Failed to read file that should have been created when loading non-existant file",
        );
        assert!(!file_contents.is_empty());
    }

    #[fuchsia::test]
    async fn clear_storage() {
        let storage_id = &rand_string();
        let stash_id = rand_string();
        let mut storage = new_storage(&storage_id).await;

        // add some configs to the storage
        let cfg_foo = PersistentStorageData {
            ssid: Ssid::try_from("foo").unwrap().to_vec(),
            security_type: SecurityType::Wpa2,
            credential: Credential::Password(b"qwertyuio".to_vec()),
            has_ever_connected: true,
        };
        let cfg_bar = PersistentStorageData {
            ssid: Ssid::try_from("bar").unwrap().to_vec(),
            security_type: SecurityType::Wpa2,
            credential: Credential::Password(b"12345678".to_vec()),
            has_ever_connected: false,
        };
        storage.write(vec![cfg_foo.clone(), cfg_bar.clone()]).expect("Failed to write to storage");

        // verify that the configs are found in storage
        let configs_from_storage = storage.load().await.expect("Failed to read");
        assert_eq!(2, configs_from_storage.len());
        assert!(configs_from_storage.contains(&cfg_foo));
        assert!(configs_from_storage.contains(&cfg_bar));

        // clear the storage
        storage.clear().expect("Failed to clear storage");
        // verify that the configs are no longer in the storage
        let configs_from_storage = storage.load().await.expect("Failed to read");
        assert_eq!(0, configs_from_storage.len());

        // recreate storage and verify that clearing the storage persists
        let mut storage = PolicyStorage::new_with_id(storage_id);
        let configs_from_storage = storage.load().await.expect("Failed to read");
        assert_eq!(0, configs_from_storage.len());
    }

    #[fuchsia::test]
    async fn test_migration() {
        let storage_id = rand_string();
        let stash_client = connect_to_protocol::<fidl_stash::SecureStoreMarker>()
            .expect("failed to connect to store");
        let ssid = "foo";
        let security_type = SecurityType::Wpa2;
        let credential = Credential::Password(b"password".to_vec());
        let has_ever_connected = false;
        // This is the version used by the previous storage mechanism.
        let network_id = network_id(ssid, security_type);
        let previous_data = PersistentData { credential: credential.clone(), has_ever_connected };

        // This is the version used by the new storage mechanism.
        let network_config = vec![PersistentStorageData {
            ssid: ssid.into(),
            security_type: security_type,
            credential: credential.clone(),
            has_ever_connected,
        }];

        // Write the config to stash that storage will migrate from.
        let stash = StashStore::from_secure_store_proxy(&storage_id, stash_client.clone())
            .expect("failed to get stash proxy");
        stash.write(&network_id, &vec![previous_data]).await.expect("write failed");

        // Initialize storage, and give it the stash with the saved network data.
        let mut storage = PolicyStorage::new_with_id(&storage_id);
        storage.legacy_stash = Ok(stash);
        assert_eq!(storage.load().await.expect("load failed"), network_config);

        // The config should have been deleted from stash.
        // The stash connection can't be reused, or the stash store will fail to access stash.
        let stash_client = connect_to_protocol::<fidl_stash::SecureStoreMarker>()
            .expect("failed to connect to store");
        let stash = StashStore::from_secure_store_proxy(&storage_id, stash_client)
            .expect("failed to get stash proxy");
        assert!(stash.load().await.expect("load failed").is_empty());

        // And once more, but this time there should be no migration.
        let mut storage = PolicyStorage::new_with_id(&storage_id);
        assert_eq!(storage.load().await.expect("load failed"), network_config);
    }

    #[fuchsia::test]
    async fn test_migration_with_bad_stash() {
        let store_id = rand_string();

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

        // Initialize the store but switch out with the stash we made to act corrupted.
        let mut store = PolicyStorage::new_with_id(&store_id);
        let proxy_fn = client.into_proxy().unwrap();
        store.legacy_stash = StashStore::from_secure_store_proxy(&store_id, proxy_fn);

        // Try and load the config. It should provide empty config.
        assert!(&store.load().await.expect("load failed").is_empty());

        // Make sure there was an attempt to actually read from stash.
        assert!(read_from_stash.load(Ordering::Relaxed));
    }

    /// Creates a new persistent storage with a file bath based on the given ID and clears any
    /// values saved in the store.
    pub async fn new_storage(id: &str) -> PolicyStorage {
        let mut store = PolicyStorage::new_with_id(id);
        store.clear().expect("failed to clear stash");
        store
    }
}
