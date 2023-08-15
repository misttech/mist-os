// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::filesystems::FsManagementFilesystemInstance,
    async_trait::async_trait,
    diagnostics_reader::{ArchiveReader, Inspect},
    fidl_fuchsia_fxfs::{CryptManagementMarker, CryptMarker, KeyPurpose},
    fuchsia_component::client::connect_channel_to_protocol,
    fuchsia_zircon as zx,
    std::{
        path::Path,
        sync::{Arc, Once},
    },
    storage_benchmarks::{
        BlockDeviceConfig, BlockDeviceFactory, CacheClearableFilesystem, Filesystem,
        FilesystemConfig,
    },
};

/// Config object for starting Fxfs instances.
#[derive(Clone)]
pub struct Fxfs;

#[async_trait]
impl FilesystemConfig for Fxfs {
    type Filesystem = FxfsInstance;

    async fn start_filesystem(
        &self,
        block_device_factory: &dyn BlockDeviceFactory,
    ) -> FxfsInstance {
        let block_device = block_device_factory
            .create_block_device(&BlockDeviceConfig {
                use_zxcrypt: false,
                fvm_volume_size: Some(60 * 1024 * 1024),
            })
            .await;
        FxfsInstance {
            fxfs: FsManagementFilesystemInstance::new(
                fs_management::Fxfs::with_crypt_client(Arc::new(get_crypt_client)),
                block_device,
                /*as_blob=*/ false,
            )
            .await,
        }
    }

    fn name(&self) -> String {
        "fxfs".to_owned()
    }
}

fn get_crypt_client() -> zx::Channel {
    static CRYPT_CLIENT_INITIALIZER: Once = Once::new();
    CRYPT_CLIENT_INITIALIZER.call_once(|| {
        let (client_end, server_end) = zx::Channel::create();
        connect_channel_to_protocol::<CryptManagementMarker>(server_end)
            .expect("Failed to connect to the crypt management service");
        let crypt_management_service =
            fidl_fuchsia_fxfs::CryptManagementSynchronousProxy::new(client_end);

        let mut key = [0; 32];
        zx::cprng_draw(&mut key);
        match crypt_management_service
            .add_wrapping_key(0, &key, zx::Time::INFINITE)
            .expect("FIDL failed")
            .map_err(zx::Status::from_raw)
        {
            Ok(()) => {}
            Err(zx::Status::ALREADY_EXISTS) => {
                // In tests, the binary is run multiple times which gets around the `Once`. The fxfs
                // crypt component is not restarted for each test so it may already be initialized.
                return;
            }
            Err(e) => panic!("add_wrapping_key failed: {:?}", e),
        };
        zx::cprng_draw(&mut key);
        crypt_management_service
            .add_wrapping_key(1, &key, zx::Time::INFINITE)
            .expect("FIDL failed")
            .map_err(zx::Status::from_raw)
            .expect("add_wrapping_key failed");
        crypt_management_service
            .set_active_key(KeyPurpose::Data, 0, zx::Time::INFINITE)
            .expect("FIDL failed")
            .map_err(zx::Status::from_raw)
            .expect("set_active_key failed");
        crypt_management_service
            .set_active_key(KeyPurpose::Metadata, 1, zx::Time::INFINITE)
            .expect("FIDL failed")
            .map_err(zx::Status::from_raw)
            .expect("set_active_key failed");
    });
    let (client_end, server_end) = zx::Channel::create();
    connect_channel_to_protocol::<CryptMarker>(server_end)
        .expect("Failed to connect to crypt service");
    client_end
}

pub struct FxfsInstance {
    fxfs: FsManagementFilesystemInstance,
}

async fn get_flushes(moniker: String) -> (u64, u64) {
    // Escape the colon that shows up in the fs-collection moniker.
    let escaped = format!("{}:root", moniker.replace(":", "\\:"));
    let hierarchy = ArchiveReader::new()
        .add_selector(escaped.clone())
        .snapshot::<Inspect>()
        .await
        .expect("Inspect snapshot")
        .into_iter()
        .next()
        .and_then(|result| result.payload)
        .expect("Expected one inspect hierarchy");
    let flushes = hierarchy
        .get_property_by_path(&["stores", "default", "num_flushes"])
        .unwrap_or_else(|| panic!("No flush property_found: {:?}", hierarchy))
        .uint()
        .expect("Flush property should be uint");
    let transactions = hierarchy
        .get_property_by_path(&["fs.detail", "completed_transactions"])
        .unwrap_or_else(|| panic!("No transactions property_found: {:?}", hierarchy))
        .uint()
        .expect("Transactions property should be uint");
    (*flushes, *transactions)
}

impl FxfsInstance {
    async fn flush_journal(&self) {
        // Forces a flush by making metadata changes followed by a sync which will pad out the
        // journal to the next page until a flush is triggered, which is polled via inspect.
        let file_path =
            self.fxfs.benchmark_dir().to_path_buf().join(Path::new(".flusher").to_path_buf());
        let (old_flushes, transactions) =
            get_flushes(self.fxfs.get_component_moniker().unwrap()).await;
        // If nothing has changed since it mounted, no need to flush the journal.
        if transactions == 0 {
            return;
        }
        loop {
            {
                let file =
                    std::fs::File::create(file_path.clone()).expect("Failed to create flush file");
                std::fs::File::sync_all(&file).expect("Sync call");
            }
            std::fs::remove_file(file_path.clone()).expect("Removing flush file.");
            if get_flushes(self.fxfs.get_component_moniker().unwrap()).await.0 != old_flushes {
                break;
            }
        }
    }
}

#[async_trait]
impl Filesystem for FxfsInstance {
    async fn shutdown(self) {
        self.fxfs.shutdown().await
    }

    fn benchmark_dir(&self) -> &Path {
        self.fxfs.benchmark_dir()
    }
}

#[async_trait]
impl CacheClearableFilesystem for FxfsInstance {
    async fn clear_cache(&mut self) {
        // Flush the journal, since otherwise metadata stays in the mutable in-memory layer and
        // clearing cache doesn't remove it, even over remount. So to emulate accessing actually
        // cold data, we push it down into the on-disk layer by forcing a journal flush.
        self.flush_journal().await;
        self.fxfs.clear_cache().await
    }
}

#[cfg(test)]
mod tests {
    use {super::Fxfs, crate::filesystems::testing::check_filesystem};

    #[fuchsia::test]
    async fn start_fxfs() {
        check_filesystem(Fxfs).await;
    }
}
