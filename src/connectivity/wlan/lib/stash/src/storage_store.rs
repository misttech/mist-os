// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::Store,
    anyhow::Error,
    async_trait::async_trait,
    std::{collections::HashMap, io::Write, os::fd::AsRawFd, sync::Mutex},
    wlan_stash_constants::{NetworkIdentifier, PersistentData},
};

// This is `Vec` rather than `HashMap` since json only supports strings for keys.
type NetworkPersistentData = Vec<(NetworkIdentifier, Vec<PersistentData>)>;

/// StorageStore stores the configuration in a file stored with the component.
pub struct StorageStore {
    path: std::path::PathBuf,
    data: Mutex<HashMap<NetworkIdentifier, Vec<PersistentData>>>,
}

impl StorageStore {
    /// Returns a new store initialized from an existing file.  Returns an error if the file doesn't
    /// exist or is corrupt.
    pub fn new(path: impl AsRef<std::path::Path>) -> Result<Self, Error> {
        let path = path.as_ref();
        let serialized_data = std::fs::read(path)?;
        let data: NetworkPersistentData = serde_json::from_slice(&serialized_data)?;
        Ok(Self { path: path.into(), data: Mutex::new(data.into_iter().collect()) })
    }

    /// Returns a new empty store.  The store will not be written until `flush` is called.
    pub fn empty(path: impl AsRef<std::path::Path>) -> Self {
        Self { path: path.as_ref().into(), data: Mutex::new(HashMap::new()) }
    }
}

#[async_trait]
impl Store for StorageStore {
    async fn flush(&self) -> Result<(), Error> {
        let mut tmp_path = self.path.clone();
        tmp_path.as_mut_os_string().push(".tmp");
        {
            let data: NetworkPersistentData =
                self.data.lock().unwrap().iter().map(|(k, v)| (k.clone(), v.clone())).collect();
            let serialized_data = serde_json::to_vec(&data)?;
            let mut file = std::fs::File::create(&tmp_path)?;
            file.write_all(&serialized_data)?;
            // This fsync is required because the storage stack doesn't guarantee data is flushed
            // before the rename.
            fuchsia_nix::unistd::fsync(file.as_raw_fd())?;
        }
        std::fs::rename(&tmp_path, &self.path)?;
        tracing::debug!("Successfully saved data to {:?}", self.path);
        Ok(())
    }

    async fn delete_store(&mut self) -> Result<(), Error> {
        match std::fs::remove_file(&self.path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
            _ => {
                *self.data.get_mut().unwrap() = HashMap::new();
                Ok(())
            }
        }
    }

    async fn write(
        &self,
        id: &NetworkIdentifier,
        network_configs: &[PersistentData],
    ) -> Result<(), Error> {
        let mut data = self.data.lock().unwrap();
        if network_configs.is_empty() {
            data.remove(id);
        } else {
            data.insert(id.clone(), network_configs.into());
        }
        Ok(())
    }

    async fn load(&self) -> Result<HashMap<NetworkIdentifier, Vec<PersistentData>>, Error> {
        Ok(self.data.lock().unwrap().clone())
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::tests::{network_id, new_stash_id},
        assert_matches::assert_matches,
        wlan_stash_constants::{Credential, SecurityType},
    };

    #[fuchsia::test]
    async fn test_flush() {
        let path = format!("/data/config.{}", new_stash_id());
        let store = StorageStore::empty(&path);
        let network_id = network_id("foo", SecurityType::Wpa2);
        let network_config = vec![PersistentData {
            credential: Credential::Password(b"password".to_vec()),
            has_ever_connected: false,
        }];
        store.write(&network_id, &network_config).await.expect("write failed");
        store.flush().await.expect("flush failed");
        let store = StorageStore::new(&path).expect("new failed");
        assert_eq!(
            store.load().await.expect("load failed"),
            HashMap::from([(network_id, network_config)])
        );
    }

    #[fuchsia::test]
    async fn test_delete_store() {
        let path = format!("/data/config.{}", new_stash_id());
        let mut store = StorageStore::empty(&path);
        let network_id = network_id("foo", SecurityType::Wpa2);
        let network_config = vec![PersistentData {
            credential: Credential::Password(b"password".to_vec()),
            has_ever_connected: false,
        }];
        store.write(&network_id, &network_config).await.expect("write failed");
        store.flush().await.expect("flush failed");
        store.delete_store().await.expect("delete_store failed");
        assert_eq!(store.load().await.expect("load failed"), HashMap::new());
        assert!(StorageStore::new(&path).is_err(), "new succeeded");
        assert_matches!(std::path::Path::new(&path).try_exists(), Ok(false));
    }
}
