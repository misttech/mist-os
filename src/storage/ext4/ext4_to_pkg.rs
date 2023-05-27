// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// ext4_to_pkg converts an ext4 image (embedded within an Android sparse image) into a format that
// can be included in a Fuchsia package.  The ext4 metadata is stored in a file named "metadata.v1"
// whilst all the files (not directories or symbolic links) are stored in files named with their
// inode numbers.

use {
    anyhow::{Context, Error},
    argh::FromArgs,
    ext4_metadata::{ExtendedAttributes, Metadata, ROOT_INODE_NUM},
    ext4_read_only::{
        parser::Parser as ExtParser,
        readers::{self as ext4_readers, IoAdapter},
        structs::{DirEntry2, EntryType},
    },
    std::{
        collections::{BTreeMap, HashMap},
        mem::size_of_val,
    },
    zerocopy::{AsBytes, FromBytes, FromZeroes},
};

const METADATA_PATH: &str = "metadata.v1";

fn ext4_to_pkg(
    path: &str,
    out_dir: &str,
    path_prefix: &str,
    dep_file: Option<&str>,
) -> Result<(), Error> {
    let file = std::fs::File::open(path).context(format!("Unable to open `{:?}'", path))?;
    let parser = ExtParser::new(AndroidSparseReader::new(IoAdapter::new(file))?);
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
        &mut |parser: &ExtParser<_>, path: Vec<&str>, entry: &DirEntry2| {
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

    // Write a fini manifest
    std::fs::write(
        format!("{out_dir}/manifest.fini"),
        manifest
            .iter()
            .map(|(inode_num, path)| format!("{path_prefix}/{inode_num}={path}\n"))
            .collect::<String>(),
    )?;

    if let Some(dep_file) = dep_file {
        let mut deps: String = manifest.iter().map(|(_, path)| format!("{path} ")).collect();
        deps += &format!(": {path}");
        std::fs::write(dep_file, &deps)?;
    }

    Ok(())
}

struct AndroidSparseReader<R: ext4_readers::Reader> {
    inner: R,
    header: SparseHeader,
    chunks: BTreeMap<usize, SparseChunk>,
}

/// Copied from system/core/libsparse/sparse_format.h
#[derive(AsBytes, FromZeroes, FromBytes, Default, Debug)]
#[repr(C)]
struct SparseHeader {
    /// 0xed26ff3a
    magic: u32,
    /// (0x1) - reject images with higher major versions
    major_version: u16,
    /// (0x0) - allow images with higher minor versions
    minor_version: u16,
    /// 28 bytes for first revision of the file format
    file_hdr_sz: u16,
    /// 12 bytes for first revision of the file format
    chunk_hdr_sz: u16,
    /// block size in bytes, must be a multiple of 4 (4096)
    blk_sz: u32,
    /// total blocks in the non-sparse output image
    total_blks: u32,
    /// total chunks in the sparse input image
    total_chunks: u32,
    /// CRC32 checksum of the original data, counting "don't care"
    /// as 0. Standard 802.3 polynomial, use a Public Domain
    /// table implementation
    image_checksum: u32,
}

const SPARSE_HEADER_MAGIC: u32 = 0xed26ff3a;

/// Copied from system/core/libsparse/sparse_format.h
#[derive(AsBytes, FromZeroes, FromBytes, Default)]
#[repr(C)]
struct RawChunkHeader {
    /// 0xCAC1 -> raw; 0xCAC2 -> fill; 0xCAC3 -> don't care
    chunk_type: u16,
    _reserved: u16,
    /// in blocks in output image
    chunk_sz: u32,
    /// in bytes of chunk input file including chunk header and data
    total_sz: u32,
}

#[derive(Debug)]
enum SparseChunk {
    Raw { in_offset: u64, in_size: u32 },
    Fill { fill: [u8; 4] },
    DontCare,
}

const CHUNK_TYPE_RAW: u16 = 0xCAC1;
const CHUNK_TYPE_FILL: u16 = 0xCAC2;
const CHUNK_TYPE_DONT_CARE: u16 = 0xCAC3;

impl<R: ext4_readers::Reader> AndroidSparseReader<R> {
    fn new(inner: R) -> Result<Self, anyhow::Error> {
        let mut header = SparseHeader::default();
        inner.read(0, header.as_bytes_mut())?;
        let mut chunks = BTreeMap::new();
        if header.magic == SPARSE_HEADER_MAGIC {
            if header.major_version != 1 {
                anyhow::bail!("unknown sparse image major version {}", header.major_version);
            }
            let mut in_offset = size_of_val(&header) as u64;
            let mut out_offset = 0;
            for _ in 0..header.total_chunks {
                let mut chunk_header = RawChunkHeader::default();
                inner.read(in_offset, chunk_header.as_bytes_mut())?;
                let data_offset = in_offset + size_of_val(&chunk_header) as u64;
                let data_size = chunk_header.total_sz - size_of_val(&chunk_header) as u32;
                in_offset += chunk_header.total_sz as u64;
                let chunk_out_offset = out_offset;
                out_offset += chunk_header.chunk_sz as usize * header.blk_sz as usize;
                let chunk = match chunk_header.chunk_type {
                    CHUNK_TYPE_RAW => {
                        SparseChunk::Raw { in_offset: data_offset, in_size: data_size }
                    }
                    CHUNK_TYPE_FILL => {
                        let mut fill = [0u8; 4];
                        if data_size as usize != size_of_val(&fill) {
                            anyhow::bail!(
                                "fill chunk of sparse image is the wrong size: {}, should be {}",
                                data_size,
                                size_of_val(&fill),
                            );
                        }
                        inner.read(data_offset, fill.as_bytes_mut())?;
                        SparseChunk::Fill { fill }
                    }
                    CHUNK_TYPE_DONT_CARE => SparseChunk::DontCare,
                    e => anyhow::bail!("Invalid chunk type: {:?}", e),
                };
                chunks.insert(chunk_out_offset, chunk);
            }
        }
        Ok(Self { inner, header, chunks })
    }
}

impl<R: ext4_readers::Reader> ext4_readers::Reader for AndroidSparseReader<R> {
    fn read(&self, offset: u64, data: &mut [u8]) -> Result<(), ext4_readers::ReaderError> {
        let offset_usize = offset as usize;
        if self.header.magic != SPARSE_HEADER_MAGIC {
            return self.inner.read(offset, data);
        }
        let total_size = self.header.total_blks as u64 * self.header.blk_sz as u64;

        let (chunk_start, chunk) = match self.chunks.range(..offset_usize + 1).next_back() {
            Some(x) => x,
            _ => return Err(ext4_readers::ReaderError::OutOfBounds(offset, total_size)),
        };
        match chunk {
            SparseChunk::Raw { in_offset, in_size } => {
                let chunk_offset = offset - *chunk_start as u64;
                if chunk_offset > *in_size as u64 {
                    return Err(ext4_readers::ReaderError::OutOfBounds(chunk_offset, total_size));
                }
                self.inner.read(*in_offset + chunk_offset, data)?;
            }
            SparseChunk::Fill { fill } => {
                for i in offset_usize..offset_usize + data.len() {
                    data[i - offset_usize] = fill[offset_usize % fill.len()];
                }
            }
            SparseChunk::DontCare => {}
        }
        Ok(())
    }
}

#[derive(FromArgs, PartialEq, Debug)]
/// ext4_to_pkg
struct Args {
    /// path to image.
    #[argh(positional)]
    image: String,
    /// output directory.
    #[argh(positional)]
    out_dir: String,
    /// path prefix used for the paths in the manifest.
    #[argh(positional)]
    path_prefix: String,
    /// a path to a dependency file that should be written.
    #[argh(option, short = 'd')]
    dep_file: Option<String>,
}

fn main() -> Result<(), Error> {
    let args: Args = argh::from_env();
    let _ = std::fs::create_dir(&args.out_dir);
    ext4_to_pkg(&args.image, &args.out_dir, &args.path_prefix, args.dep_file.as_deref())
}

#[cfg(test)]
mod tests {
    use {
        super::ROOT_INODE_NUM,
        assert_matches::assert_matches,
        ext4_metadata::{Metadata, NodeInfo, Symlink},
        std::collections::HashMap,
    };

    // To generate the test image:
    //
    //   $ sudo apt-get install android-sdk-libsparse-utils
    //   $ truncate /tmp/image.blk -s $((1024 * 1024))
    //   $ mke2fs /tmp/image.blk -t ext4 -O ^64bit
    //   # mkdir /tmp/mount
    //   $ sudo mount /tmp/image.blk /tmp/mount
    //   $ sudo chown <username> /tmp/mount
    //   $ cd /tmp/mount
    //   $ mkdir foo
    //   $ cd foo
    //   $ echo hello >file
    //   $ setfattr -n user.a -v apple file
    //   $ setfattr -n user.b -v ball file
    //   $ ln -s file symlink
    //   $ cd ~
    //   $ sudo umount /tmp/mount
    //   $ img2simg /tmp/image.blk /tmp/image.sparse
    #[test]
    fn test_read_image() {
        const UID: u16 = 49152; // This would need to change depending on who builds the image.
        const GID: u16 = 24403;

        // The test image should be present in the package at data/test-img.
        let m = Metadata::deserialize(
            &std::fs::read("/pkg/data/test-image/metadata.v1").expect("Failed to read metadata"),
        )
        .expect("Failed to deserialize metadata");

        let root = m.get(ROOT_INODE_NUM).expect("Missing root node");
        assert_eq!(root.mode, 0o40755);
        assert_eq!(root.uid, UID);
        assert_eq!(root.gid, 0);
        assert_eq!(root.extended_attributes, [].into());

        let dir_inode_num = m.lookup(ROOT_INODE_NUM, "foo").expect("foo not found");
        let node = m.get(dir_inode_num).expect("root not found");
        assert_matches!(node.info(), NodeInfo::Directory(_));
        assert_eq!(node.mode, 0o40750);
        assert_eq!(node.uid, UID);
        assert_eq!(node.gid, GID);
        assert_eq!(node.extended_attributes, [].into());

        let inode_num = m.lookup(dir_inode_num, "file").expect("foo not found");
        let node = m.get(inode_num).expect("root not found");
        assert_matches!(node.info(), NodeInfo::File(_));
        assert_eq!(node.mode, 0o100640);
        assert_eq!(node.uid, UID);
        assert_eq!(node.gid, GID);
        let xattr: HashMap<_, _> =
            [((*b"user.a").into(), (*b"apple").into()), ((*b"user.b").into(), (*b"ball").into())]
                .into();
        assert_eq!(&node.extended_attributes, &xattr);
        assert_eq!(
            &std::fs::read(format!("/pkg/data/test-image/{inode_num}"))
                .expect("Failed to read foo/file"),
            b"hello\n"
        );

        let inode_num = m.lookup(dir_inode_num, "symlink").expect("foo not found");
        let node = m.get(inode_num).expect("root not found");
        assert_matches!(node.info(), NodeInfo::Symlink(Symlink { target }) if target == "file");
        assert_eq!(node.mode, 0o120777);
        assert_eq!(node.uid, UID);
        assert_eq!(node.gid, GID);
        assert_eq!(node.extended_attributes, [].into());
    }
}
