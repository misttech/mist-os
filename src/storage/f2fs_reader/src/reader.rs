// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use crate::checkpoint::*;
use crate::superblock::{SuperBlock, BLOCK_SIZE, SEGMENT_SIZE, SUPERBLOCK_OFFSET};
use anyhow::{ensure, Error};
use storage_device::Device;

pub struct F2fsReader {
    pub device: Box<dyn Device>,
    pub superblock: SuperBlock,     // 1kb, points at checkpoints
    pub checkpoint: CheckpointPack, // pair of a/b segments (alternating versions)
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
        Ok(Self { device, superblock, checkpoint })
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
}

#[cfg(test)]
mod test {
    use super::*;
    use storage_device::fake_device::FakeDevice;

    fn open_test_image(path: &str) -> FakeDevice {
        let base_path = std::env::vars()
            .find(|(k, _)| k == "FUCHSIA_DIR")
            .unwrap_or_else(|| {
                panic!("FUCHSIA_DIR environment variable is not set.");
            })
            .1;
        let path = std::path::PathBuf::from(base_path).join(path);
        println!("path is {path:?}");
        FakeDevice::from_image(
            zstd::Decoder::new(std::fs::File::open(&path).expect("open image"))
                .expect("decrypt image"),
            BLOCK_SIZE as u32,
        )
        .expect("open image")
    }

    #[fuchsia::test]
    async fn test_open_fs() {
        let device = open_test_image("src/storage/f2fs_reader/testdata/f2fs.img.zst");

        let f2fs = F2fsReader::open_device(Box::new(device)).await.expect("open ok");
        // Root inode is a known constant.
        assert_eq!(f2fs.root_ino(), 3);
        let superblock = &f2fs.superblock;
        let major_ver = superblock.major_ver;
        let minor_ver = superblock.minor_ver;
        assert_eq!(major_ver, 1);
        assert_eq!(minor_ver, 16);
        assert_eq!(superblock.get_total_size(), 64 << 20);
        assert_eq!(superblock.get_volume_name().expect("get volume name"), "testimage");
    }
}
