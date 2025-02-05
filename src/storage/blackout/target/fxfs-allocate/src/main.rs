// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{Context, Result};
use async_trait::async_trait;
use blackout_target::{find_partition, set_up_partition, Test, TestServer};
use fidl::endpoints::{ClientEnd, Proxy as _};
use fidl_fuchsia_fs_startup::{CreateOptions, MountOptions};
use fidl_fuchsia_fxfs::{CryptManagementMarker, CryptMarker, KeyPurpose};
use fidl_fuchsia_io as fio;
use fs_management::Fxfs;
use fuchsia_component::client::connect_to_protocol;
use rand::rngs::StdRng;
use rand::seq::SliceRandom;
use rand::{distributions, Rng, SeedableRng};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

// Sort of arbitrary maximum file size just to put a cap on it.
const BLACKOUT_MAX_FILE_SIZE: usize = 1 << 16;

const DATA_KEY: [u8; 32] = [
    0xcf, 0x9e, 0x45, 0x2a, 0x22, 0xa5, 0x70, 0x31, 0x33, 0x3b, 0x4d, 0x6b, 0x6f, 0x78, 0x58, 0x29,
    0x04, 0x79, 0xc7, 0xd6, 0xa9, 0x4b, 0xce, 0x82, 0x04, 0x56, 0x5e, 0x82, 0xfc, 0xe7, 0x37, 0xa8,
];

const METADATA_KEY: [u8; 32] = [
    0x0f, 0x4d, 0xca, 0x6b, 0x35, 0x0e, 0x85, 0x6a, 0xb3, 0x8c, 0xdd, 0xe9, 0xda, 0x0e, 0xc8, 0x22,
    0x8e, 0xea, 0xd8, 0x05, 0xc4, 0xc9, 0x0b, 0xa8, 0xd8, 0x85, 0x87, 0x50, 0x75, 0x40, 0x1c, 0x4c,
];

#[derive(Copy, Clone)]
struct FxfsAllocate;

impl FxfsAllocate {
    fn connect_to_crypt_service(&self) -> Result<ClientEnd<CryptMarker>> {
        Ok(connect_to_protocol::<CryptMarker>()?.into_client_end().unwrap())
    }

    async fn setup_crypt_service(&self) -> Result<ClientEnd<CryptMarker>> {
        static INITIALIZED: AtomicBool = AtomicBool::new(false);
        if INITIALIZED.load(Ordering::SeqCst) {
            return self.connect_to_crypt_service();
        }
        let crypt_management = connect_to_protocol::<CryptManagementMarker>()?;
        let wrapping_key_id_0 = [0; 16];
        let mut wrapping_key_id_1 = [0; 16];
        wrapping_key_id_1[0] = 1;
        crypt_management
            .add_wrapping_key(&wrapping_key_id_0, &DATA_KEY)
            .await?
            .map_err(zx::Status::from_raw)?;
        crypt_management
            .add_wrapping_key(&wrapping_key_id_1, &METADATA_KEY)
            .await?
            .map_err(zx::Status::from_raw)?;
        crypt_management
            .set_active_key(KeyPurpose::Data, &wrapping_key_id_0)
            .await?
            .map_err(zx::Status::from_raw)?;
        crypt_management
            .set_active_key(KeyPurpose::Metadata, &wrapping_key_id_1)
            .await?
            .map_err(zx::Status::from_raw)?;
        INITIALIZED.store(true, Ordering::SeqCst);
        self.connect_to_crypt_service()
    }
}

#[derive(Clone, Copy, Debug)]
enum Op {
    Allocate,
    Write,
    Truncate,
    // Close the connection to this file and reopen it. This effectively forces a paged object
    // handle flush.
    Reopen,
    // Delete this file and make a new random one.
    Replace,
}

impl distributions::Distribution<Op> for distributions::Standard {
    fn sample<R: Rng + ?Sized>(&self, rng: &mut R) -> Op {
        match rng.gen_range(0..100) {
            0..25 => Op::Allocate,
            25..50 => Op::Write,
            50..75 => Op::Truncate,
            75..95 => Op::Reopen,
            95..100 => Op::Replace,
            _ => unreachable!(),
        }
    }
}

struct File {
    name: String,
    contents: Vec<u8>,
    oid: Option<u64>,
    proxy: Option<fio::FileProxy>,
}

impl std::fmt::Display for File {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "file '{}' (current len: {}) ", self.name, self.contents.len())?;
        if let Some(oid) = self.oid {
            write!(f, "oid: {}", oid)?;
        }
        Ok(())
    }
}

impl distributions::Distribution<File> for distributions::Standard {
    fn sample<R: Rng + ?Sized>(&self, rng: &mut R) -> File {
        static NAME: AtomicU64 = AtomicU64::new(0);
        let size = rng.gen_range(1..BLACKOUT_MAX_FILE_SIZE);
        let mut contents = vec![0; size];
        rng.fill(contents.as_mut_slice());
        let name = NAME.fetch_add(1, Ordering::Relaxed);
        File { name: name.to_string(), contents, oid: None, proxy: None }
    }
}

impl File {
    // Create this file on disk in the given directory.
    async fn create(&mut self, dir: &fio::DirectoryProxy) -> Result<()> {
        let file = fuchsia_fs::directory::open_file(
            dir,
            &self.name,
            fio::PERM_READABLE | fio::PERM_WRITABLE | fio::Flags::FLAG_MUST_CREATE,
        )
        .await?;
        fuchsia_fs::file::write(&file, &self.contents).await?;
        let (_, attrs) = file
            .get_attributes(fio::NodeAttributesQuery::ID)
            .await?
            .map_err(zx::Status::from_raw)?;
        self.oid = attrs.id;
        self.proxy = Some(file);
        Ok(())
    }

    async fn reopen(&mut self, dir: &fio::DirectoryProxy) -> Result<()> {
        let proxy = self.proxy.take().unwrap();
        proxy
            .close()
            .await
            .context("reopen close fidl error")?
            .map_err(zx::Status::from_raw)
            .context("reopen close returned error")?;
        self.proxy = Some(
            fuchsia_fs::directory::open_file(
                dir,
                &self.name,
                fio::PERM_READABLE | fio::PERM_WRITABLE,
            )
            .await?,
        );
        Ok(())
    }

    fn proxy(&self) -> &fio::FileProxy {
        self.proxy.as_ref().unwrap()
    }
}

#[async_trait]
impl Test for FxfsAllocate {
    async fn setup(
        self: Arc<Self>,
        device_label: String,
        device_path: Option<String>,
        _seed: u64,
    ) -> Result<()> {
        log::info!(device_label:%, device_path:?; "setting up");
        let partition_controller = set_up_partition(device_label, device_path).await?;

        let mut fxfs = Fxfs::new(partition_controller);
        fxfs.format().await?;
        let mut fs = fxfs.serve_multi_volume().await?;
        let crypt = Some(self.setup_crypt_service().await?);
        fs.create_volume(
            "default",
            CreateOptions::default(),
            MountOptions { crypt, ..MountOptions::default() },
        )
        .await?;

        log::info!("setup complete");
        Ok(())
    }

    async fn test(
        self: Arc<Self>,
        device_label: String,
        device_path: Option<String>,
        seed: u64,
    ) -> Result<()> {
        log::info!(device_label:%, device_path:?; "running load gen");
        let partition_controller =
            find_partition(device_label, device_path).await.context("find partition")?;

        let mut fxfs = Fxfs::new(partition_controller);
        let mut fs = fxfs.serve_multi_volume().await.context("serve multi volume")?;
        let crypt = Some(self.setup_crypt_service().await.context("set up crypt service")?);
        let volume = fs
            .open_volume("default", MountOptions { crypt, ..MountOptions::default() })
            .await
            .context("open volume")?;
        let root = volume.root();
        let mut rng = StdRng::seed_from_u64(seed);

        // Make a set of 16 possible files to mess with.
        let mut files: Vec<File> =
            (&mut rng).sample_iter(distributions::Standard).take(16).collect();
        log::debug!("xx: creating initial files");
        for file in &mut files {
            log::debug!("    creating {}", file);
            file.create(root)
                .await
                .with_context(|| format!("creating file {} during setup", file.name))?;
        }

        log::info!("generating load");
        let mut scan_tick = 0;
        loop {
            if scan_tick >= 20 {
                log::debug!("xx: full scan");
                let mut entries = fuchsia_fs::directory::readdir(&root)
                    .await?
                    .into_iter()
                    .map(|entry| entry.name)
                    .collect::<Vec<_>>();
                entries.sort();
                let mut expected_entries =
                    files.iter().map(|file| file.name.to_string()).collect::<Vec<_>>();
                expected_entries.sort();
                assert_eq!(entries, expected_entries);
                for file in &files {
                    log::debug!("    scanning {}", file);
                    // Make sure we reset seek, since read and write both move the seek pointer
                    let offset = file
                        .proxy()
                        .seek(fio::SeekOrigin::Start, 0)
                        .await
                        .context("scan seek fidl error")?
                        .map_err(zx::Status::from_raw)
                        .context("scan seek returned error")?;
                    assert_eq!(offset, 0);
                    let data =
                        fuchsia_fs::file::read(file.proxy()).await.context("scan read error")?;
                    assert_eq!(file.contents.len(), data.len());
                    assert_eq!(&file.contents, &data);
                }
                scan_tick = 0;
            } else {
                scan_tick += 1;
            }
            // unwrap: vec is always non-empty so this will never be None.
            let file = files.choose_mut(&mut rng).unwrap();
            match rng.gen::<Op>() {
                Op::Allocate => {
                    // len has to be bigger than zero so make sure there is at least one byte to
                    // request.
                    let offset = rng.gen_range(0..BLACKOUT_MAX_FILE_SIZE - 1);
                    let len = rng.gen_range(1..BLACKOUT_MAX_FILE_SIZE - offset);
                    log::debug!(
                        "op: {}, allocate range: {}..{}, len: {}",
                        file,
                        offset,
                        offset + len,
                        len
                    );
                    file.proxy()
                        .allocate(offset as u64, len as u64, fio::AllocateMode::empty())
                        .await
                        .context("allocate fidl error")?
                        .map_err(zx::Status::from_raw)
                        .context("allocate returned error")?;
                    if file.contents.len() < offset + len {
                        file.contents.resize(offset + len, 0);
                    }
                }
                Op::Write => {
                    // Make sure we are always writing at least one byte.
                    let offset = rng.gen_range(0..BLACKOUT_MAX_FILE_SIZE - 1);
                    let len =
                        rng.gen_range(1..std::cmp::min(8192, BLACKOUT_MAX_FILE_SIZE - offset));
                    log::debug!(
                        "op: {}, write range: {}..{}, len: {}",
                        file,
                        offset,
                        offset + len,
                        len
                    );
                    let mut data = vec![0u8; len];
                    rng.fill(data.as_mut_slice());
                    file.proxy()
                        .write_at(&data, offset as u64)
                        .await
                        .context("write fidl error")?
                        .map_err(zx::Status::from_raw)
                        .context("write returned error")?;
                    // It's possible we are extending the file with this call, so deal with that
                    // here by filling it with zeros and then replacing that with the new content,
                    // because any space between the new offset and the old end could be sparse
                    // zeros.
                    if file.contents.len() < offset + len {
                        file.contents.resize(offset + len, 0);
                    }
                    file.contents[offset..offset + len].copy_from_slice(&data);
                }
                Op::Truncate => {
                    let offset = rng.gen_range(0..BLACKOUT_MAX_FILE_SIZE);
                    log::debug!("op: {}, truncate offset: {}", file, offset);
                    file.proxy()
                        .resize(offset as u64)
                        .await
                        .context("truncate fidl error")?
                        .map_err(zx::Status::from_raw)
                        .context("truncate returned error")?;
                    file.contents.resize(offset, 0);
                }
                Op::Reopen => {
                    log::debug!("op: {}, sync and reopen", file);
                    file.reopen(&root).await?;
                }
                Op::Replace => {
                    log::debug!("op: {}, replace", file);
                    file.proxy()
                        .close()
                        .await
                        .context("replace close fidl error")?
                        .map_err(zx::Status::from_raw)
                        .context("replace close returned error")?;
                    root.unlink(&file.name, &fio::UnlinkOptions::default())
                        .await
                        .context("replace unlink fidl error")?
                        .map_err(zx::Status::from_raw)
                        .context("replace unlink returned error")?;
                    *file = rng.gen();
                    log::debug!("    {} is replacement", file);
                    file.create(&root)
                        .await
                        .with_context(|| format!("creating file {} as a replacement", file.name))?;
                }
            }
        }
    }

    async fn verify(
        self: Arc<Self>,
        device_label: String,
        device_path: Option<String>,
        _seed: u64,
    ) -> Result<()> {
        log::info!(device_label:%, device_path:?; "verifying disk consistency");
        let partition_controller = find_partition(device_label, device_path).await?;

        let mut fxfs = Fxfs::new(partition_controller);
        fxfs.fsck().await.context("overall fsck")?;

        let mut fs = fxfs.serve_multi_volume().await.context("failed to serve")?;
        let crypt = Some(self.setup_crypt_service().await.context("set up crypt service")?);
        fs.check_volume("default", crypt).await.context("default volume check")?;

        log::info!("verification complete");
        Ok(())
    }
}

#[fuchsia::main(logging_tags = ["blackout", "fxfs-allocate"])]
async fn main() -> Result<()> {
    let server = TestServer::new(FxfsAllocate)?;
    server.serve().await;

    Ok(())
}
