// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{anyhow, Context as _, Error};
use block_client::{BlockClient, BufferSlice, MutableBufferSlice, RemoteBlockClient};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use zerocopy::{FromBytes as _, IntoBytes as _};

pub mod format;

/// GPT GUIDs are stored in mixed-endian format (see Appendix A of the EFI spec).  To ensure this is
/// correctly handled, wrap the Uuid type to hide methods that use the UUIDs inappropriately.
#[derive(Clone, Default, Debug)]
pub struct Guid(uuid::Uuid);

impl From<uuid::Uuid> for Guid {
    fn from(uuid: uuid::Uuid) -> Self {
        Self(uuid)
    }
}

impl Guid {
    pub fn from_bytes(bytes: [u8; 16]) -> Self {
        Self(uuid::Uuid::from_bytes_le(bytes))
    }

    pub fn to_bytes(&self) -> [u8; 16] {
        self.0.to_bytes_le()
    }

    pub fn to_string(&self) -> String {
        self.0.to_string()
    }

    pub fn nil() -> Self {
        Self(uuid::Uuid::nil())
    }

    pub fn generate() -> Self {
        Self(uuid::Uuid::new_v4())
    }
}

#[derive(Clone, Debug)]
pub struct PartitionInfo {
    pub label: String,
    pub type_guid: Guid,
    pub instance_guid: Guid,
    pub start_block: u64,
    pub num_blocks: u64,
    pub flags: u64,
}

impl PartitionInfo {
    pub fn from_entry(entry: &format::PartitionTableEntry) -> Result<Self, Error> {
        let label =
            String::from_utf16(entry.name.split(|v| *v == 0u16).next().unwrap())?.to_owned();
        Ok(Self {
            label,
            type_guid: Guid::from_bytes(entry.type_guid),
            instance_guid: Guid::from_bytes(entry.instance_guid),
            start_block: entry.first_lba,
            num_blocks: entry
                .last_lba
                .checked_add(1)
                .unwrap()
                .checked_sub(entry.first_lba)
                .unwrap(),
            flags: entry.flags,
        })
    }

    pub fn as_entry(&self) -> format::PartitionTableEntry {
        let mut name = [0u16; 36];
        let raw = self.label.encode_utf16().collect::<Vec<_>>();
        assert!(raw.len() <= name.len());
        name[..raw.len()].copy_from_slice(&raw[..]);
        format::PartitionTableEntry {
            type_guid: self.type_guid.to_bytes(),
            instance_guid: self.instance_guid.to_bytes(),
            first_lba: self.start_block,
            last_lba: self.start_block + self.num_blocks.saturating_sub(1),
            flags: self.flags,
            name,
        }
    }

    fn nil() -> Self {
        Self {
            label: String::default(),
            type_guid: Guid::default(),
            instance_guid: Guid::default(),
            start_block: 0,
            num_blocks: 0,
            flags: 0,
        }
    }

    fn is_nil(&self) -> bool {
        self.label == ""
            && self.type_guid.0.is_nil()
            && self.instance_guid.0.is_nil()
            && self.start_block == 0
            && self.num_blocks == 0
            && self.flags == 0
    }
}

enum WhichHeader {
    Primary,
    Backup,
}

impl WhichHeader {
    fn offset(&self, block_size: u64, block_count: u64) -> u64 {
        match self {
            Self::Primary => block_size,
            Self::Backup => (block_count - 1) * block_size,
        }
    }
}

async fn load_metadata(
    client: &RemoteBlockClient,
    which: WhichHeader,
) -> Result<(format::Header, BTreeMap<u32, PartitionInfo>), Error> {
    let bs = client.block_size() as usize;
    let mut header_block = vec![0u8; client.block_size() as usize];
    client
        .read_at(
            MutableBufferSlice::Memory(&mut header_block[..]),
            which.offset(bs as u64, client.block_count() as u64),
        )
        .await
        .context("Read header")?;
    let (header, _) = format::Header::ref_from_prefix(&header_block[..])
        .map_err(|_| anyhow!("Header has invalid size"))?;
    header.ensure_integrity(client.block_count(), client.block_size() as u64)?;
    let partition_table_offset = header.part_start * bs as u64;
    let partition_table_size = (header.num_parts * header.part_size) as usize;
    let partition_table_size_rounded = partition_table_size
        .checked_next_multiple_of(bs)
        .ok_or_else(|| anyhow!("Overflow when rounding up partition table size "))?;
    let mut partition_table = BTreeMap::new();
    if header.num_parts > 0 {
        let mut partition_table_blocks = vec![0u8; partition_table_size_rounded];
        client
            .read_at(
                MutableBufferSlice::Memory(&mut partition_table_blocks[..]),
                partition_table_offset,
            )
            .await
            .with_context(|| {
                format!(
                    "Failed to read partition table (sz {}) from offset {}",
                    partition_table_size, partition_table_offset
                )
            })?;
        let crc = crc::crc32::checksum_ieee(&partition_table_blocks[..partition_table_size]);
        anyhow::ensure!(header.crc32_parts == crc, "Invalid partition table checksum");

        for i in 0..header.num_parts as usize {
            let entry_raw = &partition_table_blocks
                [i * header.part_size as usize..(i + 1) * header.part_size as usize];
            let (entry, _) = format::PartitionTableEntry::ref_from_prefix(entry_raw)
                .map_err(|_| anyhow!("Failed to parse partition {i}"))?;
            if entry.is_empty() {
                continue;
            }
            entry.ensure_integrity().context("GPT partition table entry invalid!")?;

            partition_table.insert(i as u32, PartitionInfo::from_entry(entry)?);
        }
    }
    Ok((header.clone(), partition_table))
}

struct TransactionState {
    pending_id: u64,
    next_id: u64,
}

impl Default for TransactionState {
    fn default() -> Self {
        Self { pending_id: u64::MAX, next_id: 0 }
    }
}

/// Manages a connection to a GPT-formatted block device.
pub struct Gpt {
    client: Arc<RemoteBlockClient>,
    header: format::Header,
    partitions: BTreeMap<u32, PartitionInfo>,
    transaction_state: Arc<Mutex<TransactionState>>,
}

impl std::fmt::Debug for Gpt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> Result<(), std::fmt::Error> {
        f.debug_struct("Gpt")
            .field("header", &self.header)
            .field("partitions", &self.partitions)
            .finish()
    }
}

#[derive(Eq, thiserror::Error, Clone, Debug, PartialEq)]
pub enum TransactionCommitError {
    #[error("I/O error")]
    Io,
    #[error("Invalid arguments")]
    InvalidArguments,
    #[error("No space")]
    NoSpace,
}

impl From<format::FormatError> for TransactionCommitError {
    fn from(error: format::FormatError) -> Self {
        match error {
            format::FormatError::InvalidArguments => Self::InvalidArguments,
            format::FormatError::NoSpace => Self::NoSpace,
        }
    }
}

impl From<TransactionCommitError> for zx::Status {
    fn from(error: TransactionCommitError) -> zx::Status {
        match error {
            TransactionCommitError::Io => zx::Status::IO,
            TransactionCommitError::InvalidArguments => zx::Status::INVALID_ARGS,
            TransactionCommitError::NoSpace => zx::Status::NO_SPACE,
        }
    }
}

impl Gpt {
    /// Loads and validates a GPT-formatted block device.
    pub async fn open(client: Arc<RemoteBlockClient>) -> Result<Self, Error> {
        let mut restore_primary = false;
        let (header, partitions) = match load_metadata(&client, WhichHeader::Primary).await {
            Ok(v) => v,
            Err(error) => {
                tracing::warn!(?error, "Failed to load primary metadata; falling back to backup");
                restore_primary = true;
                load_metadata(&client, WhichHeader::Backup)
                    .await
                    .context("Failed to load backup metadata")?
            }
        };
        let mut this = Self {
            client,
            header,
            partitions,
            transaction_state: Arc::new(Mutex::new(TransactionState::default())),
        };
        if restore_primary {
            tracing::info!("Restoring primary metadata from backup!");
            this.header.backup_lba = this.header.current_lba;
            this.header.current_lba = 1;
            this.header.part_start = 2;
            this.header.crc32 = this.header.compute_checksum();
            let partition_table =
                this.flattened_partitions().into_iter().map(|v| v.as_entry()).collect::<Vec<_>>();
            let partition_table_raw = format::serialize_partition_table(
                &mut this.header,
                this.client.block_size() as usize,
                this.client.block_count(),
                &partition_table[..],
            )
            .context("Failed to serialize existing partition table")?;
            this.write_metadata(&this.header, &partition_table_raw[..])
                .await
                .context("Failed to restore primary metadata")?;
        }
        Ok(this)
    }

    /// Formats `client` as a new GPT with `partitions`.  Overwrites any existing GPT on the block
    /// device.
    pub async fn format(
        client: Arc<RemoteBlockClient>,
        partitions: Vec<PartitionInfo>,
    ) -> Result<Self, Error> {
        let header = format::Header::new(
            client.block_count(),
            client.block_size(),
            partitions.len() as u32,
        )?;
        let mut this = Self {
            client,
            header,
            partitions: BTreeMap::new(),
            transaction_state: Arc::new(Mutex::new(TransactionState::default())),
        };
        let mut transaction = this.create_transaction().unwrap();
        transaction.partitions = partitions;
        this.commit_transaction(transaction).await?;
        Ok(this)
    }

    pub fn client(&self) -> &Arc<RemoteBlockClient> {
        &self.client
    }

    #[cfg(test)]
    fn take_client(self) -> Arc<RemoteBlockClient> {
        self.client
    }

    pub fn header(&self) -> &format::Header {
        &self.header
    }

    pub fn partitions(&self) -> &BTreeMap<u32, PartitionInfo> {
        &self.partitions
    }

    // We only store valid partitions in memory.  This function allows us to flatten this back out
    // to a non-sparse array for serialization.
    fn flattened_partitions(&self) -> Vec<PartitionInfo> {
        let mut partitions = vec![PartitionInfo::nil(); self.header.num_parts as usize];
        for (idx, partition) in &self.partitions {
            partitions[*idx as usize] = partition.clone();
        }
        partitions
    }

    /// Returns None if there's already a pending transaction.
    pub fn create_transaction(&self) -> Option<Transaction> {
        {
            let mut state = self.transaction_state.lock().unwrap();
            if state.pending_id != u64::MAX {
                return None;
            } else {
                state.pending_id = state.next_id;
                state.next_id += 1;
            }
        }
        Some(Transaction {
            partitions: self.flattened_partitions(),
            transaction_state: self.transaction_state.clone(),
        })
    }

    pub async fn commit_transaction(
        &mut self,
        mut transaction: Transaction,
    ) -> Result<(), TransactionCommitError> {
        let mut new_header = self.header.clone();
        let entries =
            transaction.partitions.iter().map(|entry| entry.as_entry()).collect::<Vec<_>>();
        let partition_table_raw = format::serialize_partition_table(
            &mut new_header,
            self.client.block_size() as usize,
            self.client.block_count(),
            &entries[..],
        )?;

        let mut backup_header = new_header.clone();
        backup_header.current_lba = backup_header.backup_lba;
        backup_header.backup_lba = 1;
        backup_header.part_start = backup_header.last_usable + 1;
        backup_header.crc32 = backup_header.compute_checksum();

        // Per section 5.3.2 of the UEFI spec, the backup metadata must be written first.  The spec
        // permits the partition table entries and header to be written in either order.
        self.write_metadata(&backup_header, &partition_table_raw[..]).await.map_err(|err| {
            tracing::error!(?err, "Failed to write metadata");
            TransactionCommitError::Io
        })?;
        self.write_metadata(&new_header, &partition_table_raw[..]).await.map_err(|err| {
            tracing::error!(?err, "Failed to write metadata");
            TransactionCommitError::Io
        })?;

        self.header = new_header;
        self.partitions = BTreeMap::new();
        let mut idx = 0;
        for partition in std::mem::take(&mut transaction.partitions) {
            if !partition.is_nil() {
                self.partitions.insert(idx, partition);
            }
            idx += 1;
        }
        Ok(())
    }

    async fn write_metadata(
        &self,
        header: &format::Header,
        partition_table: &[u8],
    ) -> Result<(), Error> {
        let bs = self.client.block_size() as usize;
        let mut header_block = vec![0u8; bs];
        header.write_to_prefix(&mut header_block[..]).unwrap();
        self.client
            .write_at(BufferSlice::Memory(&header_block[..]), header.current_lba * bs as u64)
            .await
            .context("Failed to write header")?;
        if !partition_table.is_empty() {
            self.client
                .write_at(BufferSlice::Memory(partition_table), header.part_start * bs as u64)
                .await
                .context("Failed to write partition table")?;
        }
        Ok(())
    }
}

/// Pending changes to the GPT.
pub struct Transaction {
    pub partitions: Vec<PartitionInfo>,
    transaction_state: Arc<Mutex<TransactionState>>,
}

impl std::fmt::Debug for Transaction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> Result<(), std::fmt::Error> {
        f.debug_struct("Transaction").field("partitions", &self.partitions).finish()
    }
}

impl Drop for Transaction {
    fn drop(&mut self) {
        let mut state = self.transaction_state.lock().unwrap();
        debug_assert!(state.pending_id != u64::MAX);
        state.pending_id = u64::MAX;
    }
}

#[cfg(test)]
mod tests {
    use crate::{Gpt, Guid, PartitionInfo};
    use block_client::{BlockClient as _, BufferSlice, MutableBufferSlice, RemoteBlockClient};
    use fake_block_server::{FakeServer, FakeServerOptions};
    use std::ops::Range;
    use std::sync::Arc;
    use zx::HandleBased;
    use {fidl_fuchsia_hardware_block_volume as fvolume, fuchsia_async as fasync};

    #[fuchsia::test]
    async fn load_unformatted_gpt() {
        let vmo = zx::Vmo::create(4096).unwrap();
        let server = Arc::new(FakeServer::from_vmo(512, vmo));
        let (client, server_end) =
            fidl::endpoints::create_proxy::<fvolume::VolumeMarker>().unwrap();

        let _task =
            fasync::Task::spawn(
                async move { server.serve(server_end.into_stream().unwrap()).await },
            );
        let client = Arc::new(RemoteBlockClient::new(client).await.unwrap());
        Gpt::open(client).await.expect_err("load should fail");
    }

    #[fuchsia::test]
    async fn load_formatted_empty_gpt() {
        let vmo = zx::Vmo::create(4096).unwrap();
        let server = Arc::new(FakeServer::from_vmo(512, vmo));
        let (client, server_end) =
            fidl::endpoints::create_proxy::<fvolume::VolumeMarker>().unwrap();

        let _task =
            fasync::Task::spawn(
                async move { server.serve(server_end.into_stream().unwrap()).await },
            );
        let client = Arc::new(RemoteBlockClient::new(client).await.unwrap());
        Gpt::format(client.clone(), vec![]).await.expect("format failed");
        Gpt::open(client).await.expect("load should succeed");
    }

    #[fuchsia::test]
    async fn load_formatted_gpt_with_minimal_size() {
        const PART_TYPE_GUID: [u8; 16] = [2u8; 16];
        const PART_INSTANCE_GUID: [u8; 16] = [2u8; 16];
        const PART_NAME: &str = "part";

        let vmo = zx::Vmo::create(6 * 4096).unwrap();
        let server = Arc::new(FakeServer::from_vmo(4096, vmo));
        let (client, server_end) =
            fidl::endpoints::create_proxy::<fvolume::VolumeMarker>().unwrap();

        let _task =
            fasync::Task::spawn(
                async move { server.serve(server_end.into_stream().unwrap()).await },
            );
        let client = Arc::new(RemoteBlockClient::new(client).await.unwrap());
        Gpt::format(
            client.clone(),
            vec![PartitionInfo {
                label: PART_NAME.to_string(),
                type_guid: Guid::from_bytes(PART_TYPE_GUID),
                instance_guid: Guid::from_bytes(PART_INSTANCE_GUID),
                start_block: 3,
                num_blocks: 1,
                flags: 0,
            }],
        )
        .await
        .expect("format failed");
        let manager = Gpt::open(client).await.expect("load should succeed");
        assert_eq!(manager.header.first_usable, 3);
        assert_eq!(manager.header.last_usable, 3);
        let partition = manager.partitions().get(&0).expect("No entry found");
        assert_eq!(partition.start_block, 3);
        assert_eq!(partition.num_blocks, 1);
        assert!(manager.partitions().get(&1).is_none());
    }

    #[fuchsia::test]
    async fn load_formatted_gpt_with_one_partition() {
        const PART_TYPE_GUID: [u8; 16] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15];
        const PART_INSTANCE_GUID: [u8; 16] =
            [16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31];
        const PART_NAME: &str = "part";

        let vmo = zx::Vmo::create(4096).unwrap();
        let server = Arc::new(FakeServer::from_vmo(512, vmo));
        let (client, server_end) =
            fidl::endpoints::create_proxy::<fvolume::VolumeMarker>().unwrap();

        let _task =
            fasync::Task::spawn(
                async move { server.serve(server_end.into_stream().unwrap()).await },
            );
        let client = Arc::new(RemoteBlockClient::new(client).await.unwrap());
        Gpt::format(
            client.clone(),
            vec![PartitionInfo {
                label: PART_NAME.to_string(),
                type_guid: Guid::from_bytes(PART_TYPE_GUID),
                instance_guid: Guid::from_bytes(PART_INSTANCE_GUID),
                start_block: 4,
                num_blocks: 1,
                flags: 0,
            }],
        )
        .await
        .expect("format failed");
        let manager = Gpt::open(client).await.expect("load should succeed");
        let partition = manager.partitions().get(&0).expect("No entry found");
        assert_eq!(partition.label, "part");
        assert_eq!(partition.type_guid.to_bytes(), PART_TYPE_GUID);
        assert_eq!(partition.instance_guid.to_bytes(), PART_INSTANCE_GUID);
        assert_eq!(partition.start_block, 4);
        assert_eq!(partition.num_blocks, 1);
        assert!(manager.partitions().get(&1).is_none());
    }

    #[fuchsia::test]
    async fn load_formatted_gpt_with_two_partitions() {
        const PART_TYPE_GUID: [u8; 16] = [2u8; 16];
        const PART_INSTANCE_1_GUID: [u8; 16] = [2u8; 16];
        const PART_INSTANCE_2_GUID: [u8; 16] = [3u8; 16];
        const PART_1_NAME: &str = "part1";
        const PART_2_NAME: &str = "part2";

        let vmo = zx::Vmo::create(8192).unwrap();
        let server = Arc::new(FakeServer::from_vmo(512, vmo));
        let (client, server_end) =
            fidl::endpoints::create_proxy::<fvolume::VolumeMarker>().unwrap();

        let _task =
            fasync::Task::spawn(
                async move { server.serve(server_end.into_stream().unwrap()).await },
            );
        let client = Arc::new(RemoteBlockClient::new(client).await.unwrap());
        Gpt::format(
            client.clone(),
            vec![
                PartitionInfo {
                    label: PART_1_NAME.to_string(),
                    type_guid: Guid::from_bytes(PART_TYPE_GUID),
                    instance_guid: Guid::from_bytes(PART_INSTANCE_1_GUID),
                    start_block: 4,
                    num_blocks: 1,
                    flags: 0,
                },
                PartitionInfo {
                    label: PART_2_NAME.to_string(),
                    type_guid: Guid::from_bytes(PART_TYPE_GUID),
                    instance_guid: Guid::from_bytes(PART_INSTANCE_2_GUID),
                    start_block: 7,
                    num_blocks: 1,
                    flags: 0,
                },
            ],
        )
        .await
        .expect("format failed");
        let manager = Gpt::open(client).await.expect("load should succeed");
        let partition = manager.partitions().get(&0).expect("No entry found");
        assert_eq!(partition.label, PART_1_NAME);
        assert_eq!(partition.type_guid.to_bytes(), PART_TYPE_GUID);
        assert_eq!(partition.instance_guid.to_bytes(), PART_INSTANCE_1_GUID);
        assert_eq!(partition.start_block, 4);
        assert_eq!(partition.num_blocks, 1);
        let partition = manager.partitions().get(&1).expect("No entry found");
        assert_eq!(partition.label, PART_2_NAME);
        assert_eq!(partition.type_guid.to_bytes(), PART_TYPE_GUID);
        assert_eq!(partition.instance_guid.to_bytes(), PART_INSTANCE_2_GUID);
        assert_eq!(partition.start_block, 7);
        assert_eq!(partition.num_blocks, 1);
        assert!(manager.partitions().get(&2).is_none());
    }

    #[fuchsia::test]
    async fn load_formatted_gpt_with_extra_bytes_in_partition_name() {
        const PART_TYPE_GUID: [u8; 16] = [2u8; 16];
        const PART_INSTANCE_GUID: [u8; 16] = [2u8; 16];
        const PART_NAME: &str = "part\0extrastuff";

        let vmo = zx::Vmo::create(4096).unwrap();
        let server = Arc::new(FakeServer::from_vmo(512, vmo));
        let (client, server_end) =
            fidl::endpoints::create_proxy::<fvolume::VolumeMarker>().unwrap();

        let _task =
            fasync::Task::spawn(
                async move { server.serve(server_end.into_stream().unwrap()).await },
            );
        let client = Arc::new(RemoteBlockClient::new(client).await.unwrap());
        Gpt::format(
            client.clone(),
            vec![PartitionInfo {
                label: PART_NAME.to_string(),
                type_guid: Guid::from_bytes(PART_TYPE_GUID),
                instance_guid: Guid::from_bytes(PART_INSTANCE_GUID),
                start_block: 4,
                num_blocks: 1,
                flags: 0,
            }],
        )
        .await
        .expect("format failed");
        let manager = Gpt::open(client).await.expect("load should succeed");
        let partition = manager.partitions().get(&0).expect("No entry found");
        // The name should have everything after the first nul byte stripped.
        assert_eq!(partition.label, "part");
    }

    #[fuchsia::test]
    async fn load_formatted_gpt_with_empty_partition_name() {
        const PART_TYPE_GUID: [u8; 16] = [2u8; 16];
        const PART_INSTANCE_GUID: [u8; 16] = [2u8; 16];
        const PART_NAME: &str = "";

        let vmo = zx::Vmo::create(4096).unwrap();
        let server = Arc::new(FakeServer::from_vmo(512, vmo));
        let (client, server_end) =
            fidl::endpoints::create_proxy::<fvolume::VolumeMarker>().unwrap();

        let _task =
            fasync::Task::spawn(
                async move { server.serve(server_end.into_stream().unwrap()).await },
            );
        let client = Arc::new(RemoteBlockClient::new(client).await.unwrap());
        Gpt::format(
            client.clone(),
            vec![PartitionInfo {
                label: PART_NAME.to_string(),
                type_guid: Guid::from_bytes(PART_TYPE_GUID),
                instance_guid: Guid::from_bytes(PART_INSTANCE_GUID),
                start_block: 4,
                num_blocks: 1,
                flags: 0,
            }],
        )
        .await
        .expect("format failed");
        let manager = Gpt::open(client).await.expect("load should succeed");
        let partition = manager.partitions().get(&0).expect("No entry found");
        assert_eq!(partition.label, "");
    }

    #[fuchsia::test]
    async fn load_formatted_gpt_with_invalid_primary_header() {
        const PART_TYPE_GUID: [u8; 16] = [2u8; 16];
        const PART_INSTANCE_1_GUID: [u8; 16] = [2u8; 16];
        const PART_INSTANCE_2_GUID: [u8; 16] = [3u8; 16];
        const PART_1_NAME: &str = "part1";
        const PART_2_NAME: &str = "part2";

        let vmo = zx::Vmo::create(8192).unwrap();

        let server = Arc::new(FakeServer::from_vmo(512, vmo));
        let (client, server_end) =
            fidl::endpoints::create_proxy::<fvolume::VolumeMarker>().unwrap();

        let _task =
            fasync::Task::spawn(
                async move { server.serve(server_end.into_stream().unwrap()).await },
            );
        let client = Arc::new(RemoteBlockClient::new(client).await.unwrap());
        Gpt::format(
            client.clone(),
            vec![
                PartitionInfo {
                    label: PART_1_NAME.to_string(),
                    type_guid: Guid::from_bytes(PART_TYPE_GUID),
                    instance_guid: Guid::from_bytes(PART_INSTANCE_1_GUID),
                    start_block: 4,
                    num_blocks: 1,
                    flags: 0,
                },
                PartitionInfo {
                    label: PART_2_NAME.to_string(),
                    type_guid: Guid::from_bytes(PART_TYPE_GUID),
                    instance_guid: Guid::from_bytes(PART_INSTANCE_2_GUID),
                    start_block: 7,
                    num_blocks: 1,
                    flags: 0,
                },
            ],
        )
        .await
        .expect("format failed");
        // Clobber the primary header.  The backup should allow the GPT to be used.
        client.write_at(BufferSlice::Memory(&[0xffu8; 512]), 512).await.unwrap();
        let manager = Gpt::open(client).await.expect("load should succeed");
        let partition = manager.partitions().get(&0).expect("No entry found");
        assert_eq!(partition.label, PART_1_NAME);
        assert_eq!(partition.type_guid.to_bytes(), PART_TYPE_GUID);
        assert_eq!(partition.instance_guid.to_bytes(), PART_INSTANCE_1_GUID);
        assert_eq!(partition.start_block, 4);
        assert_eq!(partition.num_blocks, 1);
        let partition = manager.partitions().get(&1).expect("No entry found");
        assert_eq!(partition.label, PART_2_NAME);
        assert_eq!(partition.type_guid.to_bytes(), PART_TYPE_GUID);
        assert_eq!(partition.instance_guid.to_bytes(), PART_INSTANCE_2_GUID);
        assert_eq!(partition.start_block, 7);
        assert_eq!(partition.num_blocks, 1);
        assert!(manager.partitions().get(&2).is_none());
    }

    #[fuchsia::test]
    async fn load_formatted_gpt_with_invalid_primary_partition_table() {
        const PART_TYPE_GUID: [u8; 16] = [2u8; 16];
        const PART_INSTANCE_1_GUID: [u8; 16] = [2u8; 16];
        const PART_INSTANCE_2_GUID: [u8; 16] = [3u8; 16];
        const PART_1_NAME: &str = "part1";
        const PART_2_NAME: &str = "part2";

        let vmo = zx::Vmo::create(8192).unwrap();

        let server = Arc::new(FakeServer::from_vmo(512, vmo));
        let (client, server_end) =
            fidl::endpoints::create_proxy::<fvolume::VolumeMarker>().unwrap();

        let _task =
            fasync::Task::spawn(
                async move { server.serve(server_end.into_stream().unwrap()).await },
            );
        let client = Arc::new(RemoteBlockClient::new(client).await.unwrap());
        Gpt::format(
            client.clone(),
            vec![
                PartitionInfo {
                    label: PART_1_NAME.to_string(),
                    type_guid: Guid::from_bytes(PART_TYPE_GUID),
                    instance_guid: Guid::from_bytes(PART_INSTANCE_1_GUID),
                    start_block: 4,
                    num_blocks: 1,
                    flags: 0,
                },
                PartitionInfo {
                    label: PART_2_NAME.to_string(),
                    type_guid: Guid::from_bytes(PART_TYPE_GUID),
                    instance_guid: Guid::from_bytes(PART_INSTANCE_2_GUID),
                    start_block: 7,
                    num_blocks: 1,
                    flags: 0,
                },
            ],
        )
        .await
        .expect("format failed");
        // Clobber the primary partition table.  The backup should allow the GPT to be used.
        client.write_at(BufferSlice::Memory(&[0xffu8; 512]), 1024).await.unwrap();
        let manager = Gpt::open(client).await.expect("load should succeed");
        let partition = manager.partitions().get(&0).expect("No entry found");
        assert_eq!(partition.label, PART_1_NAME);
        assert_eq!(partition.type_guid.to_bytes(), PART_TYPE_GUID);
        assert_eq!(partition.instance_guid.to_bytes(), PART_INSTANCE_1_GUID);
        assert_eq!(partition.start_block, 4);
        assert_eq!(partition.num_blocks, 1);
        let partition = manager.partitions().get(&1).expect("No entry found");
        assert_eq!(partition.label, PART_2_NAME);
        assert_eq!(partition.type_guid.to_bytes(), PART_TYPE_GUID);
        assert_eq!(partition.instance_guid.to_bytes(), PART_INSTANCE_2_GUID);
        assert_eq!(partition.start_block, 7);
        assert_eq!(partition.num_blocks, 1);
        assert!(manager.partitions().get(&2).is_none());
    }

    #[fuchsia::test]
    async fn drop_transaction() {
        let vmo = zx::Vmo::create(8192).unwrap();
        let server = Arc::new(FakeServer::from_vmo(512, vmo));
        let (client, server_end) =
            fidl::endpoints::create_proxy::<fvolume::VolumeMarker>().unwrap();

        let _task =
            fasync::Task::spawn(
                async move { server.serve(server_end.into_stream().unwrap()).await },
            );
        let client = Arc::new(RemoteBlockClient::new(client).await.unwrap());
        Gpt::format(client.clone(), vec![]).await.expect("format failed");
        let manager = Gpt::open(client).await.expect("load should succeed");
        {
            let _transaction = manager.create_transaction().unwrap();
            assert!(manager.create_transaction().is_none());
        }
        let _transaction =
            manager.create_transaction().expect("Transaction dropped but not available");
    }

    #[fuchsia::test]
    async fn commit_empty_transaction() {
        let vmo = zx::Vmo::create(8192).unwrap();
        let server = Arc::new(FakeServer::from_vmo(512, vmo));
        let (client, server_end) =
            fidl::endpoints::create_proxy::<fvolume::VolumeMarker>().unwrap();

        let _task =
            fasync::Task::spawn(
                async move { server.serve(server_end.into_stream().unwrap()).await },
            );
        let client = Arc::new(RemoteBlockClient::new(client).await.unwrap());
        Gpt::format(client.clone(), vec![]).await.expect("format failed");
        let mut manager = Gpt::open(client).await.expect("load should succeed");
        let transaction = manager.create_transaction().unwrap();
        manager.commit_transaction(transaction).await.expect("Commit failed");

        // Check state before and after a reload, to ensure both the in-memory and on-disk
        // representation match.
        assert_eq!(manager.header().num_parts, 0);
        assert!(manager.partitions().is_empty());
        let manager = Gpt::open(manager.take_client()).await.expect("reload should succeed");
        assert_eq!(manager.header().num_parts, 0);
        assert!(manager.partitions().is_empty());
    }

    #[fuchsia::test]
    async fn add_partition_in_transaction() {
        const PART_TYPE_GUID: [u8; 16] = [2u8; 16];
        const PART_INSTANCE_1_GUID: [u8; 16] = [2u8; 16];
        const PART_INSTANCE_2_GUID: [u8; 16] = [3u8; 16];
        const PART_1_NAME: &str = "part1";
        const PART_2_NAME: &str = "part2";

        let vmo = zx::Vmo::create(8192).unwrap();
        let server = Arc::new(FakeServer::from_vmo(512, vmo));
        let (client, server_end) =
            fidl::endpoints::create_proxy::<fvolume::VolumeMarker>().unwrap();

        let _task =
            fasync::Task::spawn(
                async move { server.serve(server_end.into_stream().unwrap()).await },
            );
        let client = Arc::new(RemoteBlockClient::new(client).await.unwrap());
        Gpt::format(
            client.clone(),
            vec![PartitionInfo {
                label: PART_1_NAME.to_string(),
                type_guid: Guid::from_bytes(PART_TYPE_GUID),
                instance_guid: Guid::from_bytes(PART_INSTANCE_1_GUID),
                start_block: 4,
                num_blocks: 1,
                flags: 0,
            }],
        )
        .await
        .expect("format failed");
        let mut manager = Gpt::open(client).await.expect("load should succeed");
        let mut transaction = manager.create_transaction().unwrap();
        assert_eq!(transaction.partitions.len(), 1);
        transaction.partitions.push(crate::PartitionInfo {
            label: PART_2_NAME.to_string(),
            type_guid: crate::Guid::from_bytes(PART_TYPE_GUID),
            instance_guid: crate::Guid::from_bytes(PART_INSTANCE_2_GUID),
            start_block: 7,
            num_blocks: 1,
            flags: 0,
        });
        manager.commit_transaction(transaction).await.expect("Commit failed");

        // Check state before and after a reload, to ensure both the in-memory and on-disk
        // representation match.
        assert_eq!(manager.header().num_parts, 2);
        assert!(manager.partitions().get(&2).is_none());
        let manager = Gpt::open(manager.take_client()).await.expect("reload should succeed");
        assert_eq!(manager.header().num_parts, 2);
        let partition = manager.partitions().get(&0).expect("No entry found");
        assert_eq!(partition.label, PART_1_NAME);
        assert_eq!(partition.type_guid.to_bytes(), PART_TYPE_GUID);
        assert_eq!(partition.instance_guid.to_bytes(), PART_INSTANCE_1_GUID);
        assert_eq!(partition.start_block, 4);
        assert_eq!(partition.num_blocks, 1);
        let partition = manager.partitions().get(&1).expect("No entry found");
        assert_eq!(partition.label, PART_2_NAME);
        assert_eq!(partition.type_guid.to_bytes(), PART_TYPE_GUID);
        assert_eq!(partition.instance_guid.to_bytes(), PART_INSTANCE_2_GUID);
        assert_eq!(partition.start_block, 7);
        assert_eq!(partition.num_blocks, 1);
        assert!(manager.partitions().get(&2).is_none());
    }

    #[fuchsia::test]
    async fn remove_partition_in_transaction() {
        const PART_TYPE_GUID: [u8; 16] = [2u8; 16];
        const PART_INSTANCE_GUID: [u8; 16] = [2u8; 16];
        const PART_NAME: &str = "part1";

        let vmo = zx::Vmo::create(8192).unwrap();
        let server = Arc::new(FakeServer::from_vmo(512, vmo));
        let (client, server_end) =
            fidl::endpoints::create_proxy::<fvolume::VolumeMarker>().unwrap();

        let _task =
            fasync::Task::spawn(
                async move { server.serve(server_end.into_stream().unwrap()).await },
            );
        let client = Arc::new(RemoteBlockClient::new(client).await.unwrap());
        Gpt::format(
            client.clone(),
            vec![PartitionInfo {
                label: PART_NAME.to_string(),
                type_guid: Guid::from_bytes(PART_TYPE_GUID),
                instance_guid: Guid::from_bytes(PART_INSTANCE_GUID),
                start_block: 4,
                num_blocks: 1,
                flags: 0,
            }],
        )
        .await
        .expect("format failed");
        let mut manager = Gpt::open(client).await.expect("load should succeed");
        let mut transaction = manager.create_transaction().unwrap();
        assert_eq!(transaction.partitions.len(), 1);
        transaction.partitions.clear();
        manager.commit_transaction(transaction).await.expect("Commit failed");

        // Check state before and after a reload, to ensure both the in-memory and on-disk
        // representation match.
        assert_eq!(manager.header().num_parts, 0);
        assert!(manager.partitions().get(&0).is_none());
        let manager = Gpt::open(manager.take_client()).await.expect("reload should succeed");
        assert_eq!(manager.header().num_parts, 0);
        assert!(manager.partitions().get(&0).is_none());
    }

    #[fuchsia::test]
    async fn modify_partition_in_transaction() {
        const PART_TYPE_GUID: [u8; 16] = [2u8; 16];
        const PART_INSTANCE_1_GUID: [u8; 16] = [2u8; 16];
        const PART_INSTANCE_2_GUID: [u8; 16] = [3u8; 16];
        const PART_1_NAME: &str = "part1";
        const PART_2_NAME: &str = "part2";

        let vmo = zx::Vmo::create(8192).unwrap();
        let server = Arc::new(FakeServer::from_vmo(512, vmo));
        let (client, server_end) =
            fidl::endpoints::create_proxy::<fvolume::VolumeMarker>().unwrap();

        let _task =
            fasync::Task::spawn(
                async move { server.serve(server_end.into_stream().unwrap()).await },
            );
        let client = Arc::new(RemoteBlockClient::new(client).await.unwrap());
        Gpt::format(
            client.clone(),
            vec![PartitionInfo {
                label: PART_1_NAME.to_string(),
                type_guid: Guid::from_bytes(PART_TYPE_GUID),
                instance_guid: Guid::from_bytes(PART_INSTANCE_1_GUID),
                start_block: 4,
                num_blocks: 1,
                flags: 0,
            }],
        )
        .await
        .expect("format failed");
        let mut manager = Gpt::open(client).await.expect("load should succeed");
        let mut transaction = manager.create_transaction().unwrap();
        assert_eq!(transaction.partitions.len(), 1);
        transaction.partitions[0] = crate::PartitionInfo {
            label: PART_2_NAME.to_string(),
            type_guid: crate::Guid::from_bytes(PART_TYPE_GUID),
            instance_guid: crate::Guid::from_bytes(PART_INSTANCE_2_GUID),
            start_block: 7,
            num_blocks: 1,
            flags: 0,
        };
        manager.commit_transaction(transaction).await.expect("Commit failed");

        // Check state before and after a reload, to ensure both the in-memory and on-disk
        // representation match.
        assert_eq!(manager.header().num_parts, 1);
        let partition = manager.partitions().get(&0).expect("No entry found");
        assert_eq!(partition.label, PART_2_NAME);
        assert_eq!(partition.type_guid.to_bytes(), PART_TYPE_GUID);
        assert_eq!(partition.instance_guid.to_bytes(), PART_INSTANCE_2_GUID);
        assert_eq!(partition.start_block, 7);
        assert_eq!(partition.num_blocks, 1);
        let manager = Gpt::open(manager.take_client()).await.expect("reload should succeed");
        assert_eq!(manager.header().num_parts, 1);
        let partition = manager.partitions().get(&0).expect("No entry found");
        assert_eq!(partition.label, PART_2_NAME);
        assert_eq!(partition.type_guid.to_bytes(), PART_TYPE_GUID);
        assert_eq!(partition.instance_guid.to_bytes(), PART_INSTANCE_2_GUID);
        assert_eq!(partition.start_block, 7);
        assert_eq!(partition.num_blocks, 1);
        assert!(manager.partitions().get(&1).is_none());
    }

    #[fuchsia::test]
    async fn grow_partition_table_in_transaction() {
        let vmo = zx::Vmo::create(1024 * 1024).unwrap();
        let server = Arc::new(FakeServer::from_vmo(512, vmo));
        let (client, server_end) =
            fidl::endpoints::create_proxy::<fvolume::VolumeMarker>().unwrap();

        let _task =
            fasync::Task::spawn(
                async move { server.serve(server_end.into_stream().unwrap()).await },
            );
        let client = Arc::new(RemoteBlockClient::new(client).await.unwrap());
        Gpt::format(
            client.clone(),
            vec![PartitionInfo {
                label: "part".to_string(),
                type_guid: Guid::from_bytes([1u8; 16]),
                instance_guid: Guid::from_bytes([1u8; 16]),
                start_block: 34,
                num_blocks: 1,
                flags: 0,
            }],
        )
        .await
        .expect("format failed");
        let mut manager = Gpt::open(client).await.expect("load should succeed");
        assert_eq!(manager.header().num_parts, 1);
        assert_eq!(manager.header().first_usable, 3);
        let mut transaction = manager.create_transaction().unwrap();
        transaction.partitions.resize(128, crate::PartitionInfo::nil());
        manager.commit_transaction(transaction).await.expect("Commit failed");

        // Check state before and after a reload, to ensure both the in-memory and on-disk
        // representation match.
        assert_eq!(manager.header().num_parts, 128);
        assert_eq!(manager.header().first_usable, 34);
        let partition = manager.partitions().get(&0).expect("No entry found");
        assert_eq!(partition.label, "part");
        assert_eq!(partition.type_guid.to_bytes(), [1u8; 16]);
        assert_eq!(partition.instance_guid.to_bytes(), [1u8; 16]);
        assert_eq!(partition.start_block, 34);
        assert_eq!(partition.num_blocks, 1);
        assert!(manager.partitions().get(&1).is_none());
        let manager = Gpt::open(manager.take_client()).await.expect("reload should succeed");
        assert_eq!(manager.header().num_parts, 128);
        assert_eq!(manager.header().first_usable, 34);
        let partition = manager.partitions().get(&0).expect("No entry found");
        assert_eq!(partition.label, "part");
        assert_eq!(partition.type_guid.to_bytes(), [1u8; 16]);
        assert_eq!(partition.instance_guid.to_bytes(), [1u8; 16]);
        assert_eq!(partition.start_block, 34);
        assert_eq!(partition.num_blocks, 1);
        assert!(manager.partitions().get(&1).is_none());
    }

    #[fuchsia::test]
    async fn shrink_partition_table_in_transaction() {
        let vmo = zx::Vmo::create(1024 * 1024).unwrap();
        let mut partitions = vec![];
        for i in 0..128 {
            partitions.push(PartitionInfo {
                label: format!("part-{i}"),
                type_guid: Guid::from_bytes([i as u8 + 1; 16]),
                instance_guid: Guid::from_bytes([i as u8 + 1; 16]),
                start_block: 34 + i,
                num_blocks: 1,
                flags: 0,
            });
        }
        let server = Arc::new(FakeServer::from_vmo(512, vmo));
        let (client, server_end) =
            fidl::endpoints::create_proxy::<fvolume::VolumeMarker>().unwrap();

        let _task =
            fasync::Task::spawn(
                async move { server.serve(server_end.into_stream().unwrap()).await },
            );
        let client = Arc::new(RemoteBlockClient::new(client).await.unwrap());
        Gpt::format(client.clone(), partitions).await.expect("format failed");
        let mut manager = Gpt::open(client).await.expect("load should succeed");
        assert_eq!(manager.header().num_parts, 128);
        assert_eq!(manager.header().first_usable, 34);
        let mut transaction = manager.create_transaction().unwrap();
        transaction.partitions.clear();
        manager.commit_transaction(transaction).await.expect("Commit failed");

        // Check state before and after a reload, to ensure both the in-memory and on-disk
        // representation match.
        assert_eq!(manager.header().num_parts, 0);
        assert_eq!(manager.header().first_usable, 2);
        assert!(manager.partitions().get(&0).is_none());
        let manager = Gpt::open(manager.take_client()).await.expect("reload should succeed");
        assert_eq!(manager.header().num_parts, 0);
        assert_eq!(manager.header().first_usable, 2);
        assert!(manager.partitions().get(&0).is_none());
    }

    #[fuchsia::test]
    async fn invalid_transaction_rejected() {
        const PART_TYPE_GUID: [u8; 16] = [2u8; 16];
        const PART_INSTANCE_GUID: [u8; 16] = [2u8; 16];
        const PART_NAME: &str = "part1";

        let vmo = zx::Vmo::create(8192).unwrap();
        let server = Arc::new(FakeServer::from_vmo(512, vmo));
        let (client, server_end) =
            fidl::endpoints::create_proxy::<fvolume::VolumeMarker>().unwrap();

        let _task =
            fasync::Task::spawn(
                async move { server.serve(server_end.into_stream().unwrap()).await },
            );
        let client = Arc::new(RemoteBlockClient::new(client).await.unwrap());
        Gpt::format(
            client.clone(),
            vec![PartitionInfo {
                label: PART_NAME.to_string(),
                type_guid: Guid::from_bytes(PART_TYPE_GUID),
                instance_guid: Guid::from_bytes(PART_INSTANCE_GUID),
                start_block: 4,
                num_blocks: 1,
                flags: 0,
            }],
        )
        .await
        .expect("format failed");
        let mut manager = Gpt::open(client).await.expect("load should succeed");
        let mut transaction = manager.create_transaction().unwrap();
        assert_eq!(transaction.partitions.len(), 1);
        // This overlaps with the GPT metadata, so is invalid.
        transaction.partitions[0].start_block = 0;
        manager.commit_transaction(transaction).await.expect_err("Commit should have failed");

        // Ensure nothing changed. Check state before and after a reload, to ensure both the
        // in-memory and on-disk representation match.
        assert_eq!(manager.header().num_parts, 1);
        let partition = manager.partitions().get(&0).expect("No entry found");
        assert_eq!(partition.label, PART_NAME);
        assert_eq!(partition.type_guid.to_bytes(), PART_TYPE_GUID);
        assert_eq!(partition.instance_guid.to_bytes(), PART_INSTANCE_GUID);
        assert_eq!(partition.start_block, 4);
        assert_eq!(partition.num_blocks, 1);
        let manager = Gpt::open(manager.take_client()).await.expect("reload should succeed");
        assert_eq!(manager.header().num_parts, 1);
        let partition = manager.partitions().get(&0).expect("No entry found");
        assert_eq!(partition.label, PART_NAME);
        assert_eq!(partition.type_guid.to_bytes(), PART_TYPE_GUID);
        assert_eq!(partition.instance_guid.to_bytes(), PART_INSTANCE_GUID);
        assert_eq!(partition.start_block, 4);
        assert_eq!(partition.num_blocks, 1);
    }

    /// An Observer that discards all writes overlapping its range (specified in bytes, not blocks).
    struct DiscardingObserver {
        block_size: u64,
        discard_range: Range<u64>,
    }

    impl fake_block_server::Observer for DiscardingObserver {
        fn write(
            &self,
            device_block_offset: u64,
            block_count: u32,
            _vmo: &Arc<zx::Vmo>,
            _vmo_offset: u64,
            _opts: block_server::WriteOptions,
        ) -> fake_block_server::WriteAction {
            let write_range = (device_block_offset * self.block_size)
                ..(device_block_offset + block_count as u64) * self.block_size;
            if write_range.end <= self.discard_range.start
                || write_range.start >= self.discard_range.end
            {
                fake_block_server::WriteAction::Write
            } else {
                fake_block_server::WriteAction::Discard
            }
        }
    }

    #[fuchsia::test]
    async fn transaction_applied_if_primary_metadata_partially_written() {
        const PART_TYPE_GUID: [u8; 16] = [2u8; 16];
        const PART_INSTANCE_1_GUID: [u8; 16] = [2u8; 16];
        const PART_INSTANCE_2_GUID: [u8; 16] = [3u8; 16];
        const PART_1_NAME: &str = "part1";
        const PART_2_NAME: &str = "part2";

        let vmo = zx::Vmo::create(8192).unwrap();
        let server = Arc::new(FakeServer::from(FakeServerOptions {
            vmo: Some(vmo),
            block_size: 512,
            observer: Some(Box::new(DiscardingObserver {
                discard_range: 1024..1536,
                block_size: 512,
            })),
            ..Default::default()
        }));
        let (client, server_end) =
            fidl::endpoints::create_proxy::<fvolume::VolumeMarker>().unwrap();

        let _task =
            fasync::Task::spawn(
                async move { server.serve(server_end.into_stream().unwrap()).await },
            );
        let client = Arc::new(RemoteBlockClient::new(client).await.unwrap());
        Gpt::format(
            client.clone(),
            vec![PartitionInfo {
                label: PART_1_NAME.to_string(),
                type_guid: Guid::from_bytes(PART_TYPE_GUID),
                instance_guid: Guid::from_bytes(PART_INSTANCE_1_GUID),
                start_block: 4,
                num_blocks: 1,
                flags: 0,
            }],
        )
        .await
        .expect("format failed");
        let mut manager = Gpt::open(client).await.expect("load should succeed");
        let mut transaction = manager.create_transaction().unwrap();
        transaction.partitions.push(crate::PartitionInfo {
            label: PART_2_NAME.to_string(),
            type_guid: crate::Guid::from_bytes(PART_TYPE_GUID),
            instance_guid: crate::Guid::from_bytes(PART_INSTANCE_2_GUID),
            start_block: 7,
            num_blocks: 1,
            flags: 0,
        });
        manager.commit_transaction(transaction).await.expect("Commit failed");

        let manager = Gpt::open(manager.take_client()).await.expect("reload should succeed");
        assert_eq!(manager.header().num_parts, 2);
        let partition = manager.partitions().get(&0).expect("No entry found");
        assert_eq!(partition.label, PART_1_NAME);
        assert_eq!(partition.type_guid.to_bytes(), PART_TYPE_GUID);
        assert_eq!(partition.instance_guid.to_bytes(), PART_INSTANCE_1_GUID);
        assert_eq!(partition.start_block, 4);
        assert_eq!(partition.num_blocks, 1);
        let partition = manager.partitions().get(&1).expect("No entry found");
        assert_eq!(partition.label, PART_2_NAME);
        assert_eq!(partition.type_guid.to_bytes(), PART_TYPE_GUID);
        assert_eq!(partition.instance_guid.to_bytes(), PART_INSTANCE_2_GUID);
        assert_eq!(partition.start_block, 7);
        assert_eq!(partition.num_blocks, 1);
    }

    #[fuchsia::test]
    async fn transaction_not_applied_if_primary_metadata_not_written() {
        const PART_TYPE_GUID: [u8; 16] = [2u8; 16];
        const PART_INSTANCE_1_GUID: [u8; 16] = [2u8; 16];
        const PART_INSTANCE_2_GUID: [u8; 16] = [3u8; 16];
        const PART_1_NAME: &str = "part1";
        const PART_2_NAME: &str = "part2";

        let vmo = zx::Vmo::create(8192).unwrap();
        let vmo_dup = vmo.duplicate_handle(zx::Rights::SAME_RIGHTS).unwrap();
        {
            let (client, server_end) =
                fidl::endpoints::create_proxy::<fvolume::VolumeMarker>().unwrap();
            let server = Arc::new(FakeServer::from_vmo(512, vmo_dup));
            let _task = fasync::Task::spawn(async move {
                server.serve(server_end.into_stream().unwrap()).await
            });
            let client = Arc::new(RemoteBlockClient::new(client).await.unwrap());
            Gpt::format(
                client.clone(),
                vec![PartitionInfo {
                    label: PART_1_NAME.to_string(),
                    type_guid: Guid::from_bytes(PART_TYPE_GUID),
                    instance_guid: Guid::from_bytes(PART_INSTANCE_1_GUID),
                    start_block: 4,
                    num_blocks: 1,
                    flags: 0,
                }],
            )
            .await
            .expect("format failed");
        }
        let (client, server_end) =
            fidl::endpoints::create_proxy::<fvolume::VolumeMarker>().unwrap();
        let server = Arc::new(FakeServer::from(FakeServerOptions {
            vmo: Some(vmo),
            block_size: 512,
            observer: Some(Box::new(DiscardingObserver {
                discard_range: 0..2048,
                block_size: 512,
            })),
            ..Default::default()
        }));
        let _task =
            fasync::Task::spawn(
                async move { server.serve(server_end.into_stream().unwrap()).await },
            );
        let client = Arc::new(RemoteBlockClient::new(client).await.unwrap());

        let mut manager = Gpt::open(client).await.expect("load should succeed");
        let mut transaction = manager.create_transaction().unwrap();
        transaction.partitions.push(crate::PartitionInfo {
            label: PART_2_NAME.to_string(),
            type_guid: crate::Guid::from_bytes(PART_TYPE_GUID),
            instance_guid: crate::Guid::from_bytes(PART_INSTANCE_2_GUID),
            start_block: 7,
            num_blocks: 1,
            flags: 0,
        });
        manager.commit_transaction(transaction).await.expect("Commit failed");

        let manager = Gpt::open(manager.take_client()).await.expect("reload should succeed");
        assert_eq!(manager.header().num_parts, 1);
        let partition = manager.partitions().get(&0).expect("No entry found");
        assert_eq!(partition.label, PART_1_NAME);
        assert_eq!(partition.type_guid.to_bytes(), PART_TYPE_GUID);
        assert_eq!(partition.instance_guid.to_bytes(), PART_INSTANCE_1_GUID);
        assert_eq!(partition.start_block, 4);
        assert_eq!(partition.num_blocks, 1);
        assert!(manager.partitions().get(&1).is_none());
    }

    #[fuchsia::test]
    async fn transaction_not_applied_if_backup_metadata_partially_written() {
        const PART_TYPE_GUID: [u8; 16] = [2u8; 16];
        const PART_INSTANCE_1_GUID: [u8; 16] = [2u8; 16];
        const PART_INSTANCE_2_GUID: [u8; 16] = [3u8; 16];
        const PART_1_NAME: &str = "part1";
        const PART_2_NAME: &str = "part2";

        let vmo = zx::Vmo::create(8192).unwrap();
        let vmo_dup = vmo.duplicate_handle(zx::Rights::SAME_RIGHTS).unwrap();
        {
            let (client, server_end) =
                fidl::endpoints::create_proxy::<fvolume::VolumeMarker>().unwrap();
            let server = Arc::new(FakeServer::from_vmo(512, vmo_dup));
            let _task = fasync::Task::spawn(async move {
                server.serve(server_end.into_stream().unwrap()).await
            });
            let client = Arc::new(RemoteBlockClient::new(client).await.unwrap());
            Gpt::format(
                client.clone(),
                vec![PartitionInfo {
                    label: PART_1_NAME.to_string(),
                    type_guid: Guid::from_bytes(PART_TYPE_GUID),
                    instance_guid: Guid::from_bytes(PART_INSTANCE_1_GUID),
                    start_block: 4,
                    num_blocks: 1,
                    flags: 0,
                }],
            )
            .await
            .expect("format failed");
        }
        let (client, server_end) =
            fidl::endpoints::create_proxy::<fvolume::VolumeMarker>().unwrap();
        let server = Arc::new(FakeServer::from(FakeServerOptions {
            vmo: Some(vmo),
            block_size: 512,
            observer: Some(Box::new(DiscardingObserver {
                discard_range: 0..7680,
                block_size: 512,
            })),
            ..Default::default()
        }));
        let _task =
            fasync::Task::spawn(
                async move { server.serve(server_end.into_stream().unwrap()).await },
            );
        let client = Arc::new(RemoteBlockClient::new(client).await.unwrap());

        let mut manager = Gpt::open(client).await.expect("load should succeed");
        let mut transaction = manager.create_transaction().unwrap();
        transaction.partitions.push(crate::PartitionInfo {
            label: PART_2_NAME.to_string(),
            type_guid: crate::Guid::from_bytes(PART_TYPE_GUID),
            instance_guid: crate::Guid::from_bytes(PART_INSTANCE_2_GUID),
            start_block: 7,
            num_blocks: 1,
            flags: 0,
        });
        manager.commit_transaction(transaction).await.expect("Commit failed");

        let manager = Gpt::open(manager.take_client()).await.expect("reload should succeed");
        assert_eq!(manager.header().num_parts, 1);
        let partition = manager.partitions().get(&0).expect("No entry found");
        assert_eq!(partition.label, PART_1_NAME);
        assert_eq!(partition.type_guid.to_bytes(), PART_TYPE_GUID);
        assert_eq!(partition.instance_guid.to_bytes(), PART_INSTANCE_1_GUID);
        assert_eq!(partition.start_block, 4);
        assert_eq!(partition.num_blocks, 1);
        assert!(manager.partitions().get(&1).is_none());
    }

    #[fuchsia::test]
    async fn restore_primary_from_backup() {
        const PART_TYPE_GUID: [u8; 16] = [2u8; 16];
        const PART_INSTANCE_GUID: [u8; 16] = [2u8; 16];
        const PART_NAME: &str = "part1";

        let vmo = zx::Vmo::create(8192).unwrap();
        let server = Arc::new(FakeServer::from_vmo(512, vmo));
        let (client, server_end) =
            fidl::endpoints::create_proxy::<fvolume::VolumeMarker>().unwrap();

        let _task =
            fasync::Task::spawn(
                async move { server.serve(server_end.into_stream().unwrap()).await },
            );
        let client = Arc::new(RemoteBlockClient::new(client).await.unwrap());
        Gpt::format(
            client.clone(),
            vec![PartitionInfo {
                label: PART_NAME.to_string(),
                type_guid: Guid::from_bytes(PART_TYPE_GUID),
                instance_guid: Guid::from_bytes(PART_INSTANCE_GUID),
                start_block: 4,
                num_blocks: 1,
                flags: 0,
            }],
        )
        .await
        .expect("format failed");
        let mut old_metadata = vec![0u8; 2048];
        client.read_at(MutableBufferSlice::Memory(&mut old_metadata[..]), 0).await.unwrap();
        let mut buffer = vec![0u8; 2048];
        client.write_at(BufferSlice::Memory(&buffer[..]), 0).await.unwrap();

        let manager = Gpt::open(client).await.expect("load should succeed");
        let client = manager.take_client();

        client.read_at(MutableBufferSlice::Memory(&mut buffer[..]), 0).await.unwrap();
        assert_eq!(old_metadata, buffer);
    }

    #[fuchsia::test]
    async fn load_golden_gpt_linux() {
        let contents = std::fs::read("/pkg/data/gpt_golden/gpt.linux.blk").unwrap();
        let server = Arc::new(FakeServer::new(contents.len() as u64 / 512, 512, &contents));
        let (client, server_end) =
            fidl::endpoints::create_proxy::<fvolume::VolumeMarker>().unwrap();

        let _task =
            fasync::Task::spawn(
                async move { server.serve(server_end.into_stream().unwrap()).await },
            );
        let manager = Gpt::open(Arc::new(RemoteBlockClient::new(client).await.unwrap()))
            .await
            .expect("load should succeed");
        let partition = manager.partitions().get(&0).expect("No entry found");
        assert_eq!(partition.label, "ext");
        assert_eq!(partition.type_guid.to_string(), "0fc63daf-8483-4772-8e79-3d69d8477de4");
        assert_eq!(partition.start_block, 8);
        assert_eq!(partition.num_blocks, 1);
        assert!(manager.partitions().get(&1).is_none());
    }

    #[fuchsia::test]
    async fn load_golden_gpt_fuchsia() {
        let contents = std::fs::read("/pkg/data/gpt_golden/gpt.fuchsia.blk").unwrap();
        let server = Arc::new(FakeServer::new(contents.len() as u64 / 512, 512, &contents));
        let (client, server_end) =
            fidl::endpoints::create_proxy::<fvolume::VolumeMarker>().unwrap();

        struct ExpectedPartition {
            label: &'static str,
            type_guid: &'static str,
            blocks: Range<u64>,
        }
        const EXPECTED_PARTITIONS: [ExpectedPartition; 8] = [
            ExpectedPartition {
                label: "fuchsia-esp",
                type_guid: "c12a7328-f81f-11d2-ba4b-00a0c93ec93b",
                blocks: 11..12,
            },
            ExpectedPartition {
                label: "zircon-a",
                type_guid: "de30cc86-1f4a-4a31-93c4-66f147d33e05",
                blocks: 12..13,
            },
            ExpectedPartition {
                label: "zircon-b",
                type_guid: "23cc04df-c278-4ce7-8471-897d1a4bcdf7",
                blocks: 13..14,
            },
            ExpectedPartition {
                label: "zircon-r",
                type_guid: "a0e5cf57-2def-46be-a80c-a2067c37cd49",
                blocks: 14..15,
            },
            ExpectedPartition {
                label: "vbmeta_a",
                type_guid: "a13b4d9a-ec5f-11e8-97d8-6c3be52705bf",
                blocks: 15..16,
            },
            ExpectedPartition {
                label: "vbmeta_b",
                type_guid: "a288abf2-ec5f-11e8-97d8-6c3be52705bf",
                blocks: 16..17,
            },
            ExpectedPartition {
                label: "vbmeta_r",
                type_guid: "6a2460c3-cd11-4e8b-80a8-12cce268ed0a",
                blocks: 17..18,
            },
            ExpectedPartition {
                label: "misc",
                type_guid: "1d75395d-f2c6-476b-a8b7-45cc1c97b476",
                blocks: 18..19,
            },
        ];

        let _task =
            fasync::Task::spawn(
                async move { server.serve(server_end.into_stream().unwrap()).await },
            );
        let manager = Gpt::open(Arc::new(RemoteBlockClient::new(client).await.unwrap()))
            .await
            .expect("load should succeed");
        for i in 0..EXPECTED_PARTITIONS.len() as u32 {
            let partition = manager.partitions().get(&i).expect("No entry found");
            let expected = &EXPECTED_PARTITIONS[i as usize];
            assert_eq!(partition.label, expected.label);
            assert_eq!(partition.type_guid.to_string(), expected.type_guid);
            assert_eq!(partition.start_block, expected.blocks.start);
            assert_eq!(partition.num_blocks, expected.blocks.end - expected.blocks.start);
        }
    }
}
