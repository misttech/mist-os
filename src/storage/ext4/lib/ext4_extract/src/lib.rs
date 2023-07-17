// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::{Context, Error},
    ext4_metadata::{ExtendedAttributes, Metadata, ROOT_INODE_NUM},
    ext4_read_only::{
        parser::Parser as ExtParser,
        readers::{IoAdapter, Reader},
        structs::{DirEntry2, EntryType},
    },
    sparse::reader::SparseReader,
    std::collections::HashMap,
};

const METADATA_PATH: &str = "metadata.v1";

/// Extract the files from an ext4 image at `path` and return a map of the destination
/// to source pairs. Additionally, create a metadata file that provides information
/// necessary for mounting the files from a fuchsia package.
pub fn ext4_extract(path: &str, out_dir: &str) -> Result<HashMap<String, String>, Error> {
    let mut file = std::fs::File::open(path).context(format!("Unable to open `{:?}'", path))?;
    let reader = if sparse::is_sparse_image(&mut file) {
        Box::new(IoAdapter::new(SparseReader::new(Box::new(file))?)) as Box<dyn Reader>
    } else {
        Box::new(IoAdapter::new(file)) as Box<dyn Reader>
    };
    let parser = ExtParser::new(reader);
    let mut metadata = Metadata::new();

    // Insert the root entry.
    let inode = parser.inode(ROOT_INODE_NUM as u32).unwrap();
    metadata.insert_directory(
        ROOT_INODE_NUM,
        inode.e2di_mode.get(),
        inode.e2di_uid.get(),
        inode.e2di_gid.get(),
        ExtendedAttributes::new(),
    );

    let mut manifest = HashMap::new();

    parser.index(
        parser.root_inode()?,
        vec![],
        &mut |parser: &ExtParser, path: Vec<&str>, entry: &DirEntry2| {
            let xattr: ExtendedAttributes = parser
                .inode_xattrs(entry.e2d_ino.get())?
                .into_iter()
                .map(|(k, v)| (k.into(), v.into()))
                .collect();

            let inode_num = entry.e2d_ino.get();
            let inode = parser.inode(inode_num).unwrap();
            let mode = inode.e2di_mode.get();
            let uid = inode.e2di_uid.get();
            let gid = inode.e2di_gid.get();

            match EntryType::from_u8(entry.e2d_type)? {
                EntryType::RegularFile => {
                    let data = parser.read_data(inode_num)?;
                    let blob_path = format!("{out_dir}/{inode_num}");
                    std::fs::write(&blob_path, data).expect("Unable to write blob");
                    manifest.insert(format!("{inode_num}"), blob_path);
                    metadata.insert_file(inode_num as u64, mode, uid, gid, xattr);
                }
                EntryType::SymLink => {
                    let data = parser.read_data(inode_num)?;
                    let target = String::from_utf8(data).unwrap_or_else(|e| {
                        panic!(
                            "Symbolic link at {} has non-utf8 target: {:?}",
                            path.join("/"),
                            e.into_bytes()
                        )
                    });
                    metadata.insert_symlink(inode_num as u64, target, mode, uid, gid, xattr);
                }
                EntryType::Directory => {
                    metadata.insert_directory(entry.e2d_ino.get() as u64, mode, uid, gid, xattr);
                }
                _ => {}
            }
            metadata.add_child(&path, inode_num as u64);
            Ok(true)
        },
    )?;

    let metadata_path = format!("{out_dir}/{METADATA_PATH}");
    std::fs::write(&metadata_path, metadata.serialize())?;
    manifest.insert(METADATA_PATH.to_string(), metadata_path);
    Ok(manifest)
}
