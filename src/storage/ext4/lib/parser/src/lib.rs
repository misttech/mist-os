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

    // macros
    use vfs::{
        assert_close, assert_event, assert_read, assert_read_dirents,
        open_as_vmo_file_assert_content, open_get_proxy_assert, open_get_vmo_file_proxy_assert_ok,
    };

    use ext4_read_only::structs::MIN_EXT4_SIZE;
    use fidl_fuchsia_io as fio;
    use std::fs;
    use vfs::directory::test_utils::{run_server_client, DirentsSameInodeBuilder};
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
    fn list_root() {
        let data = fs::read("/pkg/data/nest.img").expect("Unable to read file");
        let vmo = Vmo::create(data.len() as u64).expect("VMO is created");
        vmo.write(data.as_slice(), 0).expect("VMO write() succeeds");
        let buffer = FsSourceType::Vmo(vmo);

        let tree = construct_fs(buffer).expect("construct_fs parses the vmo");

        run_server_client(fio::OpenFlags::RIGHT_READABLE, tree, |root| async move {
            {
                let mut expected = DirentsSameInodeBuilder::new(fio::INO_UNKNOWN);
                expected.add(fio::DirentType::Directory, b".");
                expected.add(fio::DirentType::File, b"file1");
                expected.add(fio::DirentType::Directory, b"inner");
                expected.add(fio::DirentType::Directory, b"lost+found");

                assert_read_dirents!(root, 1000, expected.into_vec());
            }

            let flags = fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::DESCRIBE;
            let compare = "file1 contents.\n";
            open_as_vmo_file_assert_content!(&root, flags, "file1", compare);

            assert_close!(root);
        });
    }
}
