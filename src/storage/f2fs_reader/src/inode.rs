// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use crate::dir::InlineDentry;
use crate::reader::{Reader, NULL_ADDR};
use crate::superblock::BLOCK_SIZE;
use crate::xattr::{decode_xattr, XattrEntry};
use anyhow::{anyhow, bail, ensure, Error};
use bitflags::bitflags;
use std::collections::HashMap;
use std::fmt::Debug;
use storage_device::buffer::Buffer;
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, Ref, Unaligned};

const NAME_MAX: usize = 255;
// The number of addresses that fit in an Inode block with a header and footer.
const INODE_BLOCK_MAX_ADDR: usize = 923;
// Hard coded constant from layout.h -- Number of 32-bit addresses that fit in an address block.
const ADDR_BLOCK_NUM_ADDR: u32 = 1018;

/// F2fs supports an extent tree and cached the largest extent for the file here.
/// (We don't make use of this.)
#[repr(C, packed)]
#[derive(Copy, Clone, Debug, Immutable, KnownLayout, FromBytes, IntoBytes, Unaligned)]
pub struct Extent {
    file_offset: u32,
    block_address: u32,
    len: u32,
}

#[derive(Copy, Clone, Debug, Immutable, FromBytes, IntoBytes)]
pub struct Mode(u16);
bitflags! {
    impl Mode: u16 {
        const RegularFile = 0o100000;
        const Directory = 0o040000;
    }
}

#[derive(Copy, Clone, Debug, Immutable, FromBytes, IntoBytes)]
pub struct AdviseFlags(u8);
bitflags! {
    impl AdviseFlags: u8 {
        const Encrypted = 0x04;
        const EncryptedName = 0x08;
        const Verity = 0x40;
    }
}

#[derive(Copy, Clone, Debug, Immutable, FromBytes, IntoBytes)]
pub struct InlineFlags(u8);
bitflags! {
    impl InlineFlags: u8 {
        const Xattr = 0b00000001;
        const Data = 0b00000010;
        const Dentry = 0b00000100;
        const ExtraAttr = 0b00100000;
    }
}

#[derive(Copy, Clone, Debug, Immutable, FromBytes, IntoBytes)]
pub struct Flags(u32);
bitflags! {
    impl Flags: u32 {
        const Casefold = 0x40000000;
    }
}

#[repr(C, packed)]
#[derive(Copy, Clone, Debug, Immutable, KnownLayout, FromBytes, IntoBytes, Unaligned)]
pub struct InodeHeader {
    pub mode: Mode,
    pub advise_flags: AdviseFlags,
    pub inline_flags: InlineFlags,
    pub uid: u32,
    pub gid: u32,
    pub links: u32,
    pub size: u64,
    pub block_size: u64,
    pub atime: u64,
    pub ctime: u64,
    pub mtime: u64,
    pub atime_nanos: u32,
    pub ctime_nanos: u32,
    pub mtime_nanos: u32,
    pub generation: u32,
    pub dir_depth: u32,
    pub xattr_nid: u32,
    pub flags: Flags,
    pub parent_inode: u32,
    pub name_len: u32,
    pub name: [u8; NAME_MAX],
    pub dir_level: u8,

    ext: Extent, // Holds the largest extent of this file, if using read extents. We ignore this.
}

/// This is optionally written after the header and before 'addr[0]' in Inode.
#[repr(C, packed)]
#[derive(Copy, Clone, Debug, Immutable, KnownLayout, FromBytes, IntoBytes, Unaligned)]
pub struct InodeExtraAttr {
    pub extra_size: u16,
    pub inline_xattr_size: u16,
    pub project_id: u32,
    pub inode_checksum: u32,
    pub creation_time: u64,
    pub creation_time_nanos: u32,
    pub compressed_blocks: u64,
    pub compression_algorithm: u8,
    pub log_cluster_size: u8,
    pub compression_flags: u16,
}

#[repr(C, packed)]
#[derive(Copy, Clone, Debug, Immutable, KnownLayout, FromBytes, IntoBytes, Unaligned)]
pub struct InodeFooter {
    pub nid: u32,
    pub ino: u32,
    pub flag: u32,
    pub cp_ver: u64,
    pub next_blkaddr: u32,
}

/// Inode represents a file or directory and consumes one 4kB block in the metadata region.
///
/// An Inode's basic layout is as follows:
///    +--------------+
///    | InodeHeader  |
///    +--------------+
///    | i_addrs[923] |
///    +--------------+
///    | nids[5]      |
///    +--------------+
///    | InodeFooter  |
///    +--------------+
///
/// The i_addrs region consists of 32-bit block addresses to data associated with the inode.
/// Some or all of this may be repurposed for optional structures based on header flags:
///
///   * extra: Contains additional metadata. Consumes the first 9 entries of i_addrs.
///   * xattr: Extended attributes. Consumes the last 50 entries of i_addrs.
///   * inline_data: Consumes all remaining i_addrs. If used, no external data blocks are used.
///   * inline_dentry: Consumes all remaining i_addrs. If used, no external data blocks are used.
///
/// For inodes that do not contain inline data or inline dentry, the remaining i_addrs[] list
/// the block offsets for data blocks that contain the contents of the inode. A value of NULL_ADDR
/// indicates a zero page. A value of NEW_ADDR indicates a page that has not yet been allocated and
/// should be treated the same as a zero page for our purposes.
///
/// If a file contains more data than available i_addrs[], nids[] will be used.
///
/// nids[0] and nids[1] are what F2fs called "direct node" blocks. These contain nids (i.e. NAT
/// translated block addresses) to RawAddrBlock. Each RawAddrBlock contains up to 1018 block
/// offsets to data blocks.
///
/// If that is insufficient, nids[2] and nids[3] contain what F2fs calls "indirect node" blocks.
/// This is the same format as RawAddrBlock but each entry contains the nid of another
/// RawAddrBlock, providing another layer of indirection and thus the ability to reference
/// 1018^2 further blocks.
///
/// Finally, nids[4] may point at a "double indirect node" block. This adds one more layer of
/// indirection, allowing for a further 1018^3 blocks.
///
/// For sparse files, any individual blocks or pages of blocks (at any indirection level) may be
/// replaced with NULL_ADDR.
///
/// Block addressing starts at i_addrs and flows through each of nids[0..5] in order.
pub struct Inode {
    pub header: InodeHeader,
    pub extra: Option<InodeExtraAttr>,
    pub inline_data: Option<Box<[u8]>>,
    pub inline_dentry: Option<InlineDentry>,
    pub i_addrs: Vec<u32>,
    pub nids: [u32; 5],
    pub footer: InodeFooter,

    // These are loaded from additional nodes.
    pub nid_pages: HashMap<u32, Box<RawAddrBlock>>,
    pub xattr: Vec<XattrEntry>,
}

/// Both direct and indirect node address pages use this same format.
/// In the case of direct nodes, the addrs point to data blocks.
/// In the case of indirect and double-indirect nodes, the addrs point to nids of the next layer.
#[repr(C, packed)]
#[derive(Copy, Clone, Debug, Immutable, KnownLayout, FromBytes, IntoBytes, Unaligned)]
pub struct RawAddrBlock {
    pub addrs: [u32; ADDR_BLOCK_NUM_ADDR as usize],
    _reserved:
        [u8; BLOCK_SIZE - std::mem::size_of::<InodeFooter>() - 4 * ADDR_BLOCK_NUM_ADDR as usize],
    pub footer: InodeFooter,
}

impl TryFrom<Buffer<'_>> for Box<RawAddrBlock> {
    type Error = Error;
    fn try_from(block: Buffer<'_>) -> Result<Self, Self::Error> {
        Ok(Box::new(
            RawAddrBlock::read_from_bytes(block.as_slice())
                .map_err(|_| anyhow!("RawAddrBlock read failed"))?,
        ))
    }
}

impl Debug for Inode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut out = f.debug_struct("Inode");
        out.field("header", &self.header);
        if let Some(extra) = &self.extra {
            out.field("extra", &extra);
        }
        if let Some(inline_dentry) = &self.inline_dentry {
            out.field("inline_dentry", &inline_dentry);
        }
        out.field("i_addrs", &self.i_addrs).field("footer", &self.footer);
        out.field("xattr", &self.xattr);
        out.finish()
    }
}

impl Inode {
    /// Attempt to load (and validate) an inode at a given nid.
    pub(super) async fn try_load(f2fs: &impl Reader, ino: u32) -> Result<Box<Inode>, Error> {
        let mut raw_xattr = vec![];
        let mut this = {
            let block = f2fs.read_node(ino).await?;
            // Layout:
            //   header: InodeHeader
            //   extra: InodeExtraAttr # optional, based on header flag.
            //   i_addr: [u32; N]       # N = <=923 addr data or repurposed for inline fields.
            //   [u8; 200]      # optional, inline_xattr.
            //   [u32; 5]       # nids (for large block maps)
            //   InodeFooter
            let (header, rest): (Ref<_, InodeHeader>, _) =
                Ref::from_prefix(block.as_slice()).unwrap();
            let (rest, footer): (_, Ref<_, InodeFooter>) = Ref::from_suffix(rest).unwrap();
            ensure!(footer.ino == ino, "Footer ino doesn't match.");

            // nids are additional nodes pointing to data blocks. index has a specific meaning:
            //  - 0..2 => nid of nodes that contain addresses to data blocks.
            //  - 2..4 => nid of nodes that contain addresses to addresses of data blocks.
            //  - 5 => nid of a node that contains double-indirect addresses ot data blocks.
            let mut nids = [0u32; 5];
            nids.as_mut_bytes()
                .copy_from_slice(&rest[INODE_BLOCK_MAX_ADDR * 4..(INODE_BLOCK_MAX_ADDR + 5) * 4]);
            let rest = &rest[..INODE_BLOCK_MAX_ADDR * 4];

            let (extra, rest) = if header.inline_flags.contains(InlineFlags::ExtraAttr) {
                let (extra, _): (Ref<_, InodeExtraAttr>, _) = Ref::from_prefix(rest).unwrap();
                let extra_size = extra.extra_size as usize;
                ensure!(extra_size <= rest.len(), "Bad extra_size in inode");
                (Some((*extra).clone()), &rest[extra_size..])
            } else {
                (None, rest)
            };
            let rest = if header.inline_flags.contains(InlineFlags::Xattr) {
                // xattr always take up the last 50 i_addr slots. i.e. 200 bytes.
                ensure!(
                    rest.len() >= 200,
                    "Insufficient space for inline xattr. Likely bad extra_size."
                );
                raw_xattr.extend_from_slice(&rest[rest.len() - 200..]);
                &rest[..rest.len() - 200]
            } else {
                rest
            };

            let mut inline_data = None;
            let mut inline_dentry = None;
            let mut i_addrs: Vec<u32> = Vec::new();

            if header.inline_flags.contains(InlineFlags::Data) {
                // Inline data skips the first address slot then repurposes the remainder as data.
                ensure!(header.size as usize + 4 < rest.len(), "Invalid or corrupt inode.");
                inline_data = Some(rest[4..4 + header.size as usize].to_vec().into_boxed_slice());
            } else if header.inline_flags.contains(InlineFlags::Dentry) {
                // Repurposes i_addr to store a set of directory entry records.
                inline_dentry = Some(InlineDentry::try_from_bytes(rest)?);
            } else {
                // &rest[..] is not necessarily 4-byte aligned so can't simply cast to [u32].
                i_addrs.resize(rest.len() / 4, 0);
                i_addrs.as_mut_bytes().copy_from_slice(&rest[..rest.len() / 4 * 4]);
            };

            Box::new(Self {
                header: (*header).clone(),
                extra,
                inline_data: inline_data.map(|x| x.into()).clone(),
                inline_dentry,
                i_addrs,
                nids,
                footer: (*footer).clone(),

                nid_pages: HashMap::new(),
                xattr: vec![],
            })
        };

        // Note that this call is done outside the above block to reduce the size of the future
        // that '.await' produces by ensuring any unnecessary local variables are out of scope.
        if this.header.xattr_nid != 0 {
            raw_xattr.extend_from_slice(f2fs.read_node(this.header.xattr_nid).await?.as_slice());
        }
        this.xattr = decode_xattr(&raw_xattr)?;

        // The set of blocks making up the file begin with i_addrs. If more blocks are required
        // nids[0..5] are used. Zero pages (nid == NULL_ADDR) can be omitted at any level.
        for (i, nid) in this.nids.into_iter().enumerate() {
            if nid == NULL_ADDR {
                continue;
            }
            match i {
                0..2 => {
                    this.nid_pages.insert(nid, f2fs.read_node(nid).await?.try_into()?);
                }
                2..4 => {
                    let indirect = Box::<RawAddrBlock>::try_from(f2fs.read_node(nid).await?)?;
                    for nid in indirect.addrs {
                        if nid != NULL_ADDR {
                            this.nid_pages.insert(nid, f2fs.read_node(nid).await?.try_into()?);
                        }
                    }
                    this.nid_pages.insert(nid, indirect);
                }
                4 => {
                    let double_indirect =
                        Box::<RawAddrBlock>::try_from(f2fs.read_node(nid).await?)?;
                    for nid in double_indirect.addrs {
                        if nid != NULL_ADDR {
                            let indirect =
                                Box::<RawAddrBlock>::try_from(f2fs.read_node(nid).await?)?;
                            for nid in indirect.addrs {
                                if nid != NULL_ADDR {
                                    this.nid_pages
                                        .insert(nid, f2fs.read_node(nid).await?.try_into()?);
                                }
                            }
                            this.nid_pages.insert(nid, indirect);
                        }
                    }
                    this.nid_pages.insert(nid, double_indirect);
                }
                _ => unreachable!(),
            }
        }

        Ok(this)
    }

    /// Calls 'f' for each non-zero data block in order, passing block_num and block_addr.
    pub fn for_each_data_block(&self, mut f: impl FnMut(u32, u32)) {
        let mut block_num = 0;

        for &addr in &self.i_addrs {
            if addr != NULL_ADDR {
                f(block_num as u32, addr);
            }
            block_num += 1;
        }

        for nid in &self.nids[0..2] {
            if let Some(addrs) = self.nid_pages.get(nid) {
                let addrs = addrs.addrs;
                for addr in addrs {
                    if addr != NULL_ADDR {
                        f(block_num as u32, addr);
                    }
                    block_num += 1;
                }
            } else {
                block_num += ADDR_BLOCK_NUM_ADDR as usize;
            };
        }
        for nid in &self.nids[2..4] {
            if let Some(addrs) = self.nid_pages.get(nid) {
                let addrs = addrs.addrs;
                for nid in addrs {
                    if let Some(addrs) = self.nid_pages.get(&nid) {
                        let addrs = addrs.addrs;
                        for addr in addrs {
                            if addr != NULL_ADDR {
                                f(block_num as u32, addr);
                            }
                            block_num += 1;
                        }
                    } else {
                        block_num += ADDR_BLOCK_NUM_ADDR as usize;
                    }
                }
            } else {
                block_num += ADDR_BLOCK_NUM_ADDR as usize * ADDR_BLOCK_NUM_ADDR as usize;
            }
        }

        if let Some(addrs) = self.nid_pages.get(&self.nids[4]) {
            let addrs = addrs.addrs;
            for &nid in &addrs {
                if let Some(addrs) = self.nid_pages.get(&nid) {
                    let addrs = addrs.addrs;
                    for nid in addrs {
                        if let Some(addrs) = self.nid_pages.get(&nid) {
                            let addrs = addrs.addrs;
                            for &addr in &addrs {
                                if addr != NULL_ADDR {
                                    f(block_num as u32, addr);
                                }
                                block_num += 1;
                            }
                        } else {
                            block_num += ADDR_BLOCK_NUM_ADDR as usize;
                        }
                    }
                } else {
                    block_num += ADDR_BLOCK_NUM_ADDR as usize * ADDR_BLOCK_NUM_ADDR as usize;
                }
            }
        }
    }

    /// Convenience function that enumerates and returns all data block addresses
    /// Note that performance of this method is poor.
    /// We shouldn't need this outside of tests.
    #[cfg(test)]
    pub fn data_blocks(&self) -> Vec<(u32, u32)> {
        let mut out = Vec::new();
        self.for_each_data_block(|block_num, block_addr| {
            out.push((block_num, block_addr));
        });
        out
    }

    /// Convenience function that reads and returns the data for a block.
    /// Note that performance of this method is poor.
    /// We shouldn't need this outside of tests.
    #[cfg(test)]
    pub(super) async fn read_data_block<'a>(
        &self,
        f2fs: &'a impl Reader,
        block_num: u32,
    ) -> Result<Buffer<'a>, Error> {
        let mut addr = None;
        self.for_each_data_block(|ix, block_addr| {
            if block_num == ix {
                addr = Some(block_addr);
            }
        });
        if let Some(block_addr) = addr {
            f2fs.read_raw_block(block_addr).await
        } else {
            bail!("Not found");
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::{reader, BLOCK_SIZE};
    use anyhow;
    use async_trait::async_trait;
    use storage_device::buffer_allocator::{BufferAllocator, BufferSource};
    use zerocopy::FromZeros;

    /// A simple reader that can be filled explicitly with blocks to exercise inode.
    struct FakeReader {
        data: HashMap<u32, Box<[u8; 4096]>>,
        nids: HashMap<u32, Box<[u8; 4096]>>,
        allocator: BufferAllocator,
    }

    #[async_trait]
    impl reader::Reader for FakeReader {
        async fn read_raw_block(&self, block_addr: u32) -> Result<Buffer<'_>, Error> {
            match self.data.get(&block_addr) {
                None => Err(anyhow!("unexpected block {block_addr}")),
                Some(value) => {
                    let mut block = self.allocator.allocate_buffer(BLOCK_SIZE).await;
                    block.as_mut_slice().copy_from_slice(value.as_ref());
                    Ok(block)
                }
            }
        }

        async fn read_node(&self, nid: u32) -> Result<Buffer<'_>, Error> {
            match self.nids.get(&nid) {
                None => Err(anyhow!("unexpected nid {nid}")),
                Some(value) => {
                    let mut block = self.allocator.allocate_buffer(BLOCK_SIZE).await;
                    block.as_mut_slice().copy_from_slice(value.as_ref());
                    Ok(block)
                }
            }
        }
    }

    // Builds a bare-bones inode block.
    fn build_inode(ino: u32) -> Box<[u8; BLOCK_SIZE]> {
        let mut header = InodeHeader::new_zeroed();
        let mut footer = InodeFooter::new_zeroed();
        let mut extra = InodeExtraAttr::new_zeroed();

        extra.extra_size = std::mem::size_of::<InodeExtraAttr>().try_into().unwrap();

        header.mode = Mode::RegularFile;
        header.inline_flags.set(InlineFlags::ExtraAttr, true);
        header.inline_flags.set(InlineFlags::Xattr, true);
        footer.ino = ino;

        let mut out = [0u8; BLOCK_SIZE];
        out[..std::mem::size_of::<InodeHeader>()].copy_from_slice(&header.as_bytes());
        out[std::mem::size_of::<InodeHeader>()
            ..std::mem::size_of::<InodeHeader>() + std::mem::size_of::<InodeExtraAttr>()]
            .copy_from_slice(&extra.as_bytes());
        out[BLOCK_SIZE - std::mem::size_of::<InodeFooter>()..].copy_from_slice(&footer.as_bytes());
        Box::new(out)
    }

    #[fuchsia::test]
    async fn test_xattr_bounds() {
        let mut reader = FakeReader {
            data: [].into(),
            nids: [(1, build_inode(1)), (2, [0u8; 4096].into()), (3, [0u8; 4096].into())].into(),
            allocator: BufferAllocator::new(BLOCK_SIZE, BufferSource::new(BLOCK_SIZE * 10)),
        };
        assert!(Inode::try_load(&reader, 1).await.is_ok());

        let header_len = std::mem::size_of::<InodeHeader>();
        let footer_len = std::mem::size_of::<InodeFooter>();
        let nids_len = std::mem::size_of::<u32>() * 5;
        let overheads = header_len + footer_len + nids_len;

        // Just enough room for xattrs.
        let mut extra = InodeExtraAttr::new_zeroed();
        extra.extra_size = (BLOCK_SIZE - overheads - 200) as u16;
        reader.nids.get_mut(&1).unwrap()[std::mem::size_of::<InodeHeader>()
            ..std::mem::size_of::<InodeHeader>() + std::mem::size_of::<InodeExtraAttr>()]
            .copy_from_slice(&extra.as_bytes());
        assert!(Inode::try_load(&reader, 1).await.is_ok());

        // No room for xattrs.
        let mut extra = InodeExtraAttr::new_zeroed();
        extra.extra_size = (BLOCK_SIZE - overheads - 199) as u16;
        reader.nids.get_mut(&1).unwrap()[std::mem::size_of::<InodeHeader>()
            ..std::mem::size_of::<InodeHeader>() + std::mem::size_of::<InodeExtraAttr>()]
            .copy_from_slice(&extra.as_bytes());
        assert!(Inode::try_load(&reader, 1).await.is_err());
    }
}
