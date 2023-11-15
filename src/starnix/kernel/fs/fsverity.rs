// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{
        logging::not_implemented,
        mm::MemoryAccessorExt,
        task::CurrentTask,
        types::errno::{errno, error, Errno},
        types::user_address::UserAddress,
        types::{
            fsverity_descriptor, fsverity_enable_arg, fsverity_read_metadata_arg,
            FS_VERITY_HASH_ALG_SHA256, FS_VERITY_HASH_ALG_SHA512,
        },
    },
    mundane::hash::{Digest, Hasher, Sha256, Sha512},
    num_derive::FromPrimitive,
    num_traits::FromPrimitive,
    zerocopy::{AsBytes, FromBytes, FromZeroes},
};

#[derive(Copy, Clone, Debug, Eq, FromPrimitive, PartialEq)]
enum HashAlgorithm {
    SHA256 = FS_VERITY_HASH_ALG_SHA256 as isize,
    SHA512 = FS_VERITY_HASH_ALG_SHA512 as isize,
}

/// Create a new fsverity_descriptor from the ioctl `enable_verity`.
/// Copies salt and signature from the given user tasks memory space.
fn fsverity_descriptor_from_enable_arg(
    current_task: &CurrentTask,
    filesystem_block_size: u32,
    src: &fsverity_enable_arg,
) -> Result<fsverity_descriptor, Errno> {
    if src.version != 1 {
        return error!(EINVAL);
    }
    // block_size must be a power of 2 in [1024,min(page_size, filesystem_block_size)].
    if src.block_size.count_ones() != 1
        || src.block_size < 1024
        || src.block_size > std::cmp::min(4096, filesystem_block_size)
    {
        return error!(EINVAL);
    }
    if src.hash_algorithm != FS_VERITY_HASH_ALG_SHA256
        && src.hash_algorithm != FS_VERITY_HASH_ALG_SHA512
    {
        return error!(ENOTSUP);
    }
    if src.salt_size > 32 {
        return error!(EINVAL);
    }
    if src.sig_size > 0 {
        // TODO(fxbug.dev/302620572) Under linux, signatures are supported up to 16128 bytes.
        // We don't support these currently.
        not_implemented!("fsverity_enable signature");
        return error!(ENOTSUP);
    }
    let salt = current_task
        .mm
        .read_memory_to_vec(UserAddress::from(src.salt_ptr), src.salt_size as usize)?;
    let mut desc = fsverity_descriptor {
        version: 1,
        hash_algorithm: HashAlgorithm::from_u32(src.hash_algorithm).ok_or_else(|| errno!(EINVAL))?
            as u8,
        salt_size: salt.len() as u8,
        log_blocksize: (u32::BITS - 1 - src.block_size.leading_zeros()) as u8,
        ..Default::default()
    };
    desc.salt[..src.salt_size as usize].clone_from_slice(salt.as_slice());
    Ok(desc)
}

/// An fsverity 'measurement' is a hash of a descriptor that contains the root hash.
/// This ensures that the result captures file size, hash type and other relevant parameters.
fn fsverity_measurement(descriptor: &fsverity_descriptor) -> Result<Box<[u8]>, Errno> {
    match descriptor.hash_algorithm.into() {
        FS_VERITY_HASH_ALG_SHA256 => {
            let mut hasher = Sha256::default();
            if descriptor.salt_size > 0 {
                hasher.update(&descriptor.salt[..descriptor.salt_size as usize]);
            }
            hasher.update(descriptor.as_bytes());
            Ok(Box::new(hasher.finish().bytes()))
        }
        FS_VERITY_HASH_ALG_SHA512 => {
            let mut hasher = Sha512::default();
            if descriptor.salt_size > 0 {
                hasher.update(&descriptor.salt[..descriptor.salt_size as usize]);
            }
            hasher.update(descriptor.as_bytes());
            Ok(Box::new(hasher.finish().bytes()))
        }
        _ => {
            error!(EINVAL)
        }
    }
}

#[derive(FromBytes, FromZeroes)]
struct FsVerityDigestHeader {
    digest_algorithm: u16,
    digest_size: u16,
}

#[derive(Debug, FromPrimitive)]
enum MetadataType {
    MerkleTree = 1,
    Descriptor = 2,
    Signature = 3,
}

/// Per-file fsverity state stored in FsNode.
#[derive(Debug)]
pub enum FsVerityState {
    /// This file does not use fsverity.
    None,
    /// The state when building the merkle-tree for this file.
    ///
    /// When in the building state, the file is not writable and further attempts to
    /// ENABLE_VERITY will fail with EBUSY. It is possible for merkle-tree building to fail
    /// (e.g. if the file is too large) but it is not possible to cancel this state manually.
    Building,
    /// This file uses fsverity. Descriptor is attached.
    ///
    /// Files in this mode can never be opened as writable.
    /// There is no way to disable fsverity once enabled outside of deleting the file.
    FsVerity { descriptor: fsverity_descriptor },
}

impl FsVerityState {
    /// Returns an appropriate error if state is not writable.
    pub fn check_writable(&self) -> Result<(), Errno> {
        match self {
            FsVerityState::None => Ok(()),
            FsVerityState::Building => error!(ETXTBSY),
            FsVerityState::FsVerity { .. } => error!(EACCES),
        }
    }
}

pub mod ioctl {

    use crate::{
        fs::{
            fsverity::{
                fsverity_descriptor_from_enable_arg, fsverity_enable_arg, fsverity_measurement,
                fsverity_read_metadata_arg, FsVerityDigestHeader, FsVerityState, HashAlgorithm,
                MetadataType,
            },
            FileObject, FileWriteGuardMode,
        },
        log_warn,
        mm::{MemoryAccessor, MemoryAccessorExt},
        syscalls::{SyscallResult, SUCCESS},
        task::CurrentTask,
        types::errno::{errno, error, Errno},
        types::user_address::{UserAddress, UserRef},
    };
    use num_traits::FromPrimitive;
    use zerocopy::AsBytes;

    /// ioctl handler for FS_IOC_ENABLE_VERITY.
    pub fn enable(
        task: &CurrentTask,
        arg: UserAddress,
        file: &FileObject,
    ) -> Result<SyscallResult, Errno> {
        if file.can_write() {
            return error!(ETXTBSY);
        }
        let block_size = file.name.entry.node.fs().statfs(task)?.f_bsize as u32;
        // Nb: Lock order is important here.
        let args: fsverity_enable_arg = task.mm.read_object(arg.into())?;
        let mut descriptor = fsverity_descriptor_from_enable_arg(task, block_size, &args)?;
        descriptor.data_size = file.node().refresh_info(task)?.size as u64;
        // The "Exec" writeguard mode means 'no writers'.
        let guard = file.node().create_write_guard(FileWriteGuardMode::Exec)?;
        let entry = &file.name.entry;
        let mut fsverity = entry.node.fsverity.lock();
        match *fsverity {
            FsVerityState::Building => error!(EBUSY),
            FsVerityState::FsVerity { .. } => error!(EEXIST),
            FsVerityState::None => {
                // Notify the filesystem that we are going to enable verity on this node.
                // Note that this cannot be cancelled, but it may fail. In all cases we
                // require end_enable_verity() to be called.
                file.node().ops().begin_enable_fsverity()?;

                let node = file.node().clone();
                *fsverity = FsVerityState::Building;
                std::thread::spawn(move || {
                    // Explicitly move write guard into this thread.
                    let _guard = guard;
                    // TODO(fxbug.dev/302620512): Generate merkle data, write to vmo.
                    let merkle_data: [u8; 0] = [];
                    let merkle_tree_size = match node.ops().set_fsverity_merkle_data(&merkle_data) {
                        Ok(()) => merkle_data.len(),
                        Err(error) => {
                            log_warn!("set_fsverity_merkle_data failed: {:?}", error);
                            0
                        }
                    };
                    // TODO(fxbug.dev/302620512): Set descriptor.root_hash.
                    //   This hash is based on the contents "foo"
                    descriptor.root_hash[..32].copy_from_slice(&[
                        0xdf, 0xfd, 0xd9, 0x7c, 0xfb, 0xf2, 0x88, 0xa7, 0x29, 0xf6, 0xaf, 0x66,
                        0xf1, 0x2a, 0xc8, 0x88, 0x4f, 0xd7, 0x8d, 0xf3, 0xf1, 0x87, 0x6d, 0xcc,
                        0xc5, 0x8b, 0x5a, 0xb2, 0x36, 0x83, 0x9b, 0x49,
                    ]);
                    let mut fsverity_state = node.fsverity.lock();
                    match node.ops().end_enable_fsverity(&descriptor, merkle_tree_size) {
                        Ok(()) => {
                            *fsverity_state = FsVerityState::FsVerity { descriptor };
                        }
                        Err(error) => {
                            log_warn!("enable_fsverity failed: {:?}", error);
                        }
                    }
                });
                Ok(SUCCESS)
            }
        }
    }

    /// ioctl handler for FS_IOC_MEASURE_VERITY.
    pub fn measure(
        task: &CurrentTask,
        arg: UserAddress,
        file: &FileObject,
    ) -> Result<SyscallResult, Errno> {
        let header_ref = UserRef::<FsVerityDigestHeader>::new(arg);
        let digest_addr = header_ref.next().addr();
        let header = task.mm.read_object(header_ref)?;
        match &*file.node().fsverity.lock() {
            FsVerityState::FsVerity { descriptor } => {
                let digest_algorithm = HashAlgorithm::from_u8(descriptor.hash_algorithm)
                    .ok_or_else(|| errno!(EINVAL))?;
                if descriptor.hash_algorithm as u16 != header.digest_algorithm {
                    return error!(EINVAL);
                }
                let required_size = match digest_algorithm {
                    HashAlgorithm::SHA256 => 32,
                    HashAlgorithm::SHA512 => 64,
                };
                if (header.digest_size as usize) < required_size {
                    return error!(EOVERFLOW);
                }
                task.mm.write_memory(digest_addr, &fsverity_measurement(&descriptor)?)?;
                Ok(SUCCESS)
            }
            _ => error!(ENODATA),
        }
    }

    /// ioctl handler for FS_IOC_READ_VERITY_METADATA.
    pub fn read_metadata(
        task: &CurrentTask,
        arg: UserAddress,
        file: &FileObject,
    ) -> Result<SyscallResult, Errno> {
        let arg: fsverity_read_metadata_arg = task.mm.read_object(arg.into())?;
        match &*file.node().fsverity.lock() {
            FsVerityState::FsVerity { descriptor } => {
                match MetadataType::from_u64(arg.metadata_type).ok_or_else(|| errno!(EINVAL))? {
                    MetadataType::MerkleTree => {
                        // TODO(fxbug.dev/302620512): Add support for reading back merkle data.
                        error!(EOPNOTSUPP)
                    }
                    MetadataType::Descriptor => {
                        task.mm.write_memory(
                            UserAddress::from(arg.buf_ptr).into(),
                            &descriptor.as_bytes()
                                [arg.offset as usize..(arg.offset + arg.length) as usize],
                        )?;
                        Ok(SUCCESS)
                    }
                    MetadataType::Signature => {
                        error!(EOPNOTSUPP)
                    }
                }
            }
            _ => error!(ENODATA),
        }
    }
}
