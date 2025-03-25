// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use crate::checkpoint::*;
use crate::dir::{DentryBlock, DirEntry};
use crate::inode::{Inode, Mode};
use crate::nat::{Nat, NatJournal, RawNatEntry, SummaryBlock};
use crate::superblock::{
    f2fs_crc32, SuperBlock, BLOCKS_PER_SEGMENT, BLOCK_SIZE, F2FS_MAGIC, SEGMENT_SIZE,
    SUPERBLOCK_OFFSET,
};
use anyhow::{anyhow, bail, ensure, Error};
use std::collections::HashMap;
use storage_device::buffer::Buffer;
use storage_device::Device;
use zerocopy::FromBytes;

// Used to indicate zero pages (when used as block_addr) and end of list (when used as nid).
pub const NULL_ADDR: u32 = 0;
// Used to indicate a new page that hasn't been allocated yet.
#[allow(unused)]
pub const NEW_ADDR: u32 = 0xffffffff;

pub struct F2fsReader {
    pub device: Box<dyn Device>,
    pub superblock: SuperBlock,     // 1kb, points at checkpoints
    pub checkpoint: CheckpointPack, // pair of a/b segments (alternating versions)
    pub nat: Option<Nat>,
}

impl F2fsReader {
    pub async fn open_device(device: Box<dyn Device>) -> Result<Self, Error> {
        let (superblock, checkpoint) =
            match Self::try_from_superblock(device.as_ref(), SUPERBLOCK_OFFSET).await {
                Ok(x) => x,
                Err(e) => Self::try_from_superblock(device.as_ref(), SUPERBLOCK_OFFSET * 2)
                    .await
                    .map_err(|_| e)?,
            };
        let mut this = Self { device, superblock, checkpoint, nat: None };
        let nat_journal = this.read_nat_journal().await?;
        this.nat = Some(Nat::new(
            this.superblock.nat_blkaddr,
            this.checkpoint.nat_bitmap.clone(),
            nat_journal,
        ));
        Ok(this)
    }

    async fn try_from_superblock(
        device: &dyn Device,
        superblock_offset: u64,
    ) -> Result<(SuperBlock, CheckpointPack), Error> {
        let superblock = SuperBlock::read_from_device(device, superblock_offset).await?;
        let checkpoint_addr = superblock.cp_blkaddr;
        let checkpoint_a_offset = BLOCK_SIZE as u64 * checkpoint_addr as u64;
        let checkpoint_b_offset = checkpoint_a_offset + SEGMENT_SIZE as u64;
        // There are two checkpoint packs in consecutive segments.
        let checkpoint = match (
            CheckpointPack::read_from_device(device, checkpoint_a_offset).await,
            CheckpointPack::read_from_device(device, checkpoint_b_offset).await,
        ) {
            (Ok(a), Ok(b)) => {
                Ok(if a.header.checkpoint_ver > b.header.checkpoint_ver { a } else { b })
            }
            (Ok(a), Err(_b)) => Ok(a),
            (Err(_), Ok(b)) => Ok(b),
            (Err(a), Err(_b)) => Err(a),
        }?;

        // Min metadata segment count is 1 superblock, 1 ssa, (ckpt + sit + nat) * 2
        const MIN_METADATA_SEGMENT_COUNT: u32 = 8;

        // Make sure the metadata fits on the device (according to the superblock)
        let metadata_segment_count = superblock.segment_count_sit
            + superblock.segment_count_nat
            + checkpoint.header.rsvd_segment_count
            + superblock.segment_count_ssa
            + superblock.segment_count_ckpt;
        ensure!(
            metadata_segment_count <= superblock.segment_count
                && metadata_segment_count >= MIN_METADATA_SEGMENT_COUNT,
            "Bad segment counts in checkpoint"
        );
        Ok((superblock, checkpoint))
    }

    pub fn root_ino(&self) -> u32 {
        self.superblock.root_ino
    }

    /// Returns the block address that the checkpoint starts at.
    fn checkpoint_start_addr(&self) -> u32 {
        self.superblock.cp_blkaddr
            + if self.checkpoint.header.checkpoint_ver % 2 == 1 {
                0
            } else {
                BLOCKS_PER_SEGMENT as u32
            }
    }

    fn nat(&self) -> &Nat {
        self.nat.as_ref().unwrap()
    }

    async fn read_nat_journal(&mut self) -> Result<HashMap<u32, RawNatEntry>, Error> {
        if self.checkpoint.header.ckpt_flags & CKPT_FLAG_COMPACT_SUMMARY != 0 {
            // The "compact summary" feature packs NAT/SIT/summary into one block.
            // The NAT journal entries come first.
            let block = self
                .read_raw_block(
                    self.checkpoint_start_addr() + self.checkpoint.header.cp_pack_start_sum,
                )
                .await?;
            let n_nats = u16::read_from_bytes(&block.as_slice()[..2]).unwrap();
            let nat_journal = NatJournal::read_from_bytes(
                &block.as_slice()[2..2 + std::mem::size_of::<NatJournal>()],
            )
            .unwrap();
            ensure!(
                (n_nats as usize) <= nat_journal.entries.len(),
                "n_nats larger than block size"
            );
            Ok(HashMap::from_iter(
                nat_journal.entries[..n_nats as usize].into_iter().map(|e| (e.ino, e.entry)),
            ))
        } else {
            // Read the default summary block location from the "hot data" segment.
            let blk_addr = if self.checkpoint.header.ckpt_flags & CKPT_FLAG_UNMOUNT != 0 {
                self.checkpoint_start_addr() + self.checkpoint.header.cp_pack_total_block_count - 5
            } else {
                self.checkpoint_start_addr() + self.checkpoint.header.cp_pack_total_block_count - 2
            };
            let block = self.read_raw_block(blk_addr).await?;
            let summary = SummaryBlock::read_from_bytes(block.as_slice()).unwrap();
            ensure!(summary.footer.entry_type == 0u8, "sum_type != 0 in summary footer");
            let actual_checksum = f2fs_crc32(F2FS_MAGIC, &block.as_slice()[..BLOCK_SIZE - 4]);
            let expected_checksum = summary.footer.check_sum;
            ensure!(actual_checksum == expected_checksum, "Summary block has invalid checksum");
            let mut out = HashMap::new();
            for i in 0..summary.n_nats as usize {
                out.insert(
                    summary.nat_journal.entries[i].ino,
                    summary.nat_journal.entries[i].entry,
                );
            }
            Ok(out)
        }
    }

    /// Look up a raw NAT entry given a node ID.
    pub(super) async fn get_nat_entry(&self, nid: u32) -> Result<RawNatEntry, Error> {
        if let Some(entry) = self.nat().nat_journal.get(&nid) {
            return Ok(*entry);
        }
        let nat_block_addr = self.nat().get_nat_block_for_entry(nid)?;
        let offset = self.nat().get_nat_block_offset_for_entry(nid);
        let block = self.read_raw_block(nat_block_addr).await?;
        Ok(RawNatEntry::read_from_bytes(
            &block.as_slice()[offset..offset + std::mem::size_of::<RawNatEntry>()],
        )
        .unwrap())
    }

    /// Read a raw block from disk.
    /// `block_addr` is the physical block offset on the device.
    pub(super) async fn read_raw_block(&self, block_addr: u32) -> Result<Buffer<'_>, Error> {
        let mut block = self.device.allocate_buffer(BLOCK_SIZE).await;
        self.device
            .read(block_addr as u64 * BLOCK_SIZE as u64, block.as_mut())
            .await
            .map_err(|_| anyhow!("device read failed"))?;
        Ok(block)
    }

    /// Reads a logical 'node' block from the disk (i.e. via NAT indirection)
    pub(super) async fn read_node(&self, nid: u32) -> Result<Buffer<'_>, Error> {
        let nat_entry = self.get_nat_entry(nid).await?;
        self.read_raw_block(nat_entry.block_addr).await
    }

    /// Read an inode for a directory and return entries.
    pub async fn readdir(&self, ino: u32) -> Result<Vec<DirEntry>, Error> {
        let inode = Inode::try_load(&self, ino).await?;
        let mode = inode.header.mode;
        ensure!(mode.contains(Mode::Directory), "not a directory");
        if let Some(entries) = inode.get_inline_dir_entries()? {
            Ok(entries)
        } else {
            let mut entries = Vec::new();

            // Entries are stored in a series of increasingly larger hash tables.
            // The number of these that exist are based on inode.dir_depth.
            // Thankfully, we don't need to worry about this as the total number of blocks is
            // bound to inode.header.size and we can just skip NULL blocks.

            //let num_dir_blocks = (inode.header.size as usize + BLOCK_SIZE - 1) / BLOCK_SIZE;
            for (_block_num, block_addr) in inode.data_blocks() {
                let dentry_block =
                    DentryBlock::read_from_bytes(self.read_raw_block(block_addr).await?.as_slice())
                        .unwrap();
                entries.append(&mut dentry_block.get_entries()?);
            }
            Ok(entries)
        }
    }

    pub fn read_symlink(&self, inode: &Inode) -> Result<Box<[u8]>, Error> {
        if let Some(inline_data) = inode.inline_data.as_deref() {
            let mut filename = inline_data.to_vec();
            while let Some(b'\0') = filename.last() {
                filename.pop();
            }
            Ok(filename.into_boxed_slice())
        } else {
            bail!("Not a valid symlink");
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::dir::FileType;
    use std::path::PathBuf;

    use storage_device::fake_device::FakeDevice;

    fn open_test_image(path: &str) -> FakeDevice {
        let path = std::path::PathBuf::from(path);
        println!("path is {path:?}");
        FakeDevice::from_image(
            zstd::Decoder::new(std::fs::File::open(&path).expect("open image"))
                .expect("decompress image"),
            BLOCK_SIZE as u32,
        )
        .expect("open image")
    }

    #[fuchsia::test]
    async fn test_open_fs() {
        let device = open_test_image("/pkg/testdata/f2fs.img.zst");

        let f2fs = F2fsReader::open_device(Box::new(device)).await.expect("open ok");
        // Root inode is a known constant.
        assert_eq!(f2fs.root_ino(), 3);
        let superblock = &f2fs.superblock;
        let major_ver = superblock.major_ver;
        let minor_ver = superblock.minor_ver;
        assert_eq!(major_ver, 1);
        assert_eq!(minor_ver, 16);
        assert_eq!(superblock.get_total_size(), 256 << 20);
        assert_eq!(superblock.get_volume_name().expect("get volume name"), "testimage");
    }

    // Helper method to walk paths.
    async fn resolve_inode_path(f2fs: &F2fsReader, path: &str) -> Result<u32, Error> {
        let path = PathBuf::from(path.strip_prefix("/").unwrap());
        let mut ino = f2fs.root_ino();
        for filename in &path {
            let entries = f2fs.readdir(ino).await?;
            if let Some(entry) = entries.iter().filter(|e| *e.filename == *filename).next() {
                ino = entry.ino;
            } else {
                bail!("Not found.");
            }
        }
        Ok(ino)
    }

    #[fuchsia::test]
    async fn test_basic_dirs() {
        let device = open_test_image("/pkg/testdata/f2fs.img.zst");

        let f2fs = F2fsReader::open_device(Box::new(device)).await.expect("open ok");
        let root_ino = f2fs.root_ino();
        let root_entries = f2fs.readdir(root_ino).await.expect("readdir");
        assert_eq!(root_entries.len(), 4);
        assert_eq!(root_entries[0].filename, "a");
        assert_eq!(root_entries[0].file_type, FileType::Directory);
        assert_eq!(root_entries[1].filename, "large_dir");
        assert_eq!(root_entries[2].filename, "large_dir2");
        assert_eq!(root_entries[3].filename, "sparse.dat");

        let inlined_file_ino =
            resolve_inode_path(&f2fs, "/a/b/c/inlined").await.expect("resolve inlined");
        let inode = Inode::try_load(&f2fs, inlined_file_ino).await.expect("load inode");
        let block_size = inode.header.block_size;
        let size = inode.header.size;
        assert_eq!(block_size, 1);
        assert_eq!(size, 12);
        assert_eq!(inode.inline_data.unwrap().as_ref(), "inline_data\n".as_bytes());

        const REG_FILE_SIZE: u64 = 8 * BLOCK_SIZE as u64 + 8;
        const REG_FILE_BLOCKS: u64 = 9 + 1;
        let regular_file_ino =
            resolve_inode_path(&f2fs, "/a/b/c/regular").await.expect("resolve regular");
        let inode = Inode::try_load(&f2fs, regular_file_ino).await.expect("load inode");
        let block_size = inode.header.block_size;
        let size = inode.header.size;
        assert_eq!(block_size, REG_FILE_BLOCKS);
        assert_eq!(size, REG_FILE_SIZE);
        assert!(inode.inline_data.is_none());
        for i in 0..8 {
            assert_eq!(
                inode.read_data_block(&f2fs, i).await.expect("read data").as_slice(),
                &[0u8; BLOCK_SIZE]
            );
        }
        assert_eq!(
            &inode.read_data_block(&f2fs, 8).await.expect("read data").as_slice()[..9],
            b"01234567\0"
        );

        let symlink_ino =
            resolve_inode_path(&f2fs, "/a/b/c/symlink").await.expect("resolve symlink");
        let inode = Inode::try_load(&f2fs, symlink_ino).await.expect("load inode");
        assert_eq!(f2fs.read_symlink(&inode).expect("read_symlink").as_ref(), b"regular");

        let hardlink_ino =
            resolve_inode_path(&f2fs, "/a/b/c/hardlink").await.expect("resolve hardlink");
        let inode = Inode::try_load(&f2fs, hardlink_ino).await.expect("load inode");
        let block_size = inode.header.block_size;
        let size = inode.header.size;
        assert_eq!(block_size, REG_FILE_BLOCKS);
        assert_eq!(size, REG_FILE_SIZE);

        let chowned_ino =
            resolve_inode_path(&f2fs, "/a/b/c/chowned").await.expect("resolve chowned");
        let inode = Inode::try_load(&f2fs, chowned_ino).await.expect("load inode");
        let uid = inode.header.uid;
        let gid = inode.header.gid;
        assert_eq!(uid, 999);
        assert_eq!(gid, 999);

        let large_dir = resolve_inode_path(&f2fs, "/large_dir").await.expect("resolve large_dir");
        assert_eq!(f2fs.readdir(large_dir).await.expect("readdir").len(), 2001);

        let large_dir2 = resolve_inode_path(&f2fs, "/large_dir2").await.expect("resolve large_dir");
        assert_eq!(f2fs.readdir(large_dir2).await.expect("readdir").len(), 1);

        let sparse_dat =
            resolve_inode_path(&f2fs, "/sparse.dat").await.expect("resolve sparse.dat");
        let inode = Inode::try_load(&f2fs, sparse_dat).await.expect("load inode");
        let data_blocks = inode.data_blocks();
        assert_eq!(data_blocks.len(), 7);
        assert_eq!(data_blocks[0].0, 0);
        let block = f2fs.read_raw_block(data_blocks[0].1).await.expect("read sparse");
        assert_eq!(&block.as_slice()[..3], b"foo");
        assert_eq!(data_blocks[1].0, 923);
        assert_eq!(data_blocks[2].0, 1941);
        assert_eq!(data_blocks[3].0, 2959);
        assert_eq!(data_blocks[4].0, 1039283);
        assert_eq!(data_blocks[5].0, 104671683);
        let block = f2fs.read_raw_block(data_blocks[5].1).await.expect("read sparse");
        assert_eq!(block.as_slice(), &[0; BLOCK_SIZE]);
        assert_eq!(data_blocks[6].0, 104671684);
        let block = f2fs.read_raw_block(data_blocks[6].1).await.expect("read sparse");
        assert_eq!(&block.as_slice()[..3], b"bar");
    }
}
