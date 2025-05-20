// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use ext4_read_only::parser::Parser;
use ext4_read_only::readers::{BlockDeviceReader, Reader, VmoReader};
use ext4_read_only::structs::{self, MIN_EXT4_SIZE};
use fidl::endpoints::ClientEnd;
use fidl_fuchsia_hardware_block::BlockMarker;
use log::error;
use std::sync::Arc;

pub enum FsSourceType {
    BlockDevice(ClientEnd<BlockMarker>),
    Vmo(zx::Vmo),
}

#[derive(Debug, PartialEq)]
pub enum ConstructFsError {
    VmoReadError(zx::Status),
    ParsingError(structs::ParsingError),
}

pub fn construct_fs(
    source: FsSourceType,
) -> Result<Arc<vfs::directory::immutable::Simple>, ConstructFsError> {
    let reader: Box<dyn Reader> = match source {
        FsSourceType::BlockDevice(block_device) => {
            Box::new(BlockDeviceReader::from_client_end(block_device).map_err(|e| {
                error!("Error constructing file system: {}", e);
                ConstructFsError::VmoReadError(zx::Status::IO_INVALID)
            })?)
        }
        FsSourceType::Vmo(vmo) => {
            let size = vmo.get_size().map_err(ConstructFsError::VmoReadError)?;
            if size < MIN_EXT4_SIZE as u64 {
                // Too small to even fit the first copy of the ext4 Super Block.
                return Err(ConstructFsError::VmoReadError(zx::Status::NO_SPACE));
            }

            Box::new(VmoReader::new(Arc::new(vmo)))
        }
    };

    let parser = Parser::new(reader);

    match parser.build_fuchsia_tree() {
        Ok(tree) => Ok(tree),
        Err(e) => Err(ConstructFsError::ParsingError(e)),
    }
}

#[cfg(test)]
mod tests {
    use super::{construct_fs, FsSourceType};

    use ext4_read_only::structs::MIN_EXT4_SIZE;
    use fidl_fuchsia_io as fio;
    use fuchsia_fs::directory::{open_file, readdir, DirEntry, DirentKind};
    use fuchsia_fs::file::read_to_string;
    use std::fs;
    use zx::Vmo;

    #[fuchsia::test]
    fn image_too_small() {
        let vmo = Vmo::create(10).expect("VMO is created");
        vmo.write(b"too small", 0).expect("VMO write() succeeds");
        let buffer = FsSourceType::Vmo(vmo);

        assert!(construct_fs(buffer).is_err(), "Expected failed parsing of VMO.");
    }

    #[fuchsia::test]
    fn invalid_fs() {
        let vmo = Vmo::create(MIN_EXT4_SIZE as u64).expect("VMO is created");
        vmo.write(b"not ext4", 0).expect("VMO write() succeeds");
        let buffer = FsSourceType::Vmo(vmo);

        assert!(construct_fs(buffer).is_err(), "Expected failed parsing of VMO.");
    }

    #[fuchsia::test]
    async fn list_root() {
        let data = fs::read("/pkg/data/nest.img").expect("Unable to read file");
        let vmo = Vmo::create(data.len() as u64).expect("VMO is created");
        vmo.write(data.as_slice(), 0).expect("VMO write() succeeds");
        let buffer = FsSourceType::Vmo(vmo);

        let tree = construct_fs(buffer).expect("construct_fs parses the vmo");
        let root = vfs::directory::serve(tree, fio::PERM_READABLE);

        let expected = vec![
            DirEntry { name: String::from("file1"), kind: DirentKind::File },
            DirEntry { name: String::from("inner"), kind: DirentKind::Directory },
            DirEntry { name: String::from("lost+found"), kind: DirentKind::Directory },
        ];
        assert_eq!(readdir(&root).await.unwrap(), expected);

        let file = open_file(&root, "file1", fio::PERM_READABLE).await.unwrap();
        assert_eq!(read_to_string(&file).await.unwrap(), "file1 contents.\n");
        file.close().await.unwrap().map_err(zx::Status::from_raw).unwrap();
        root.close().await.unwrap().map_err(zx::Status::from_raw).unwrap();
    }
}
