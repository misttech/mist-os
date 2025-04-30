// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use anyhow::{bail, ensure, Context, Error};
use f2fs_reader::{F2fsReader, FileType, Flags, InlineFlags, Inode, BLOCK_SIZE};
use fxfs::filesystem::{FxFilesystemBuilder, OpenFxFilesystem};
use fxfs::object_handle::{ObjectHandle, ObjectProperties};
use fxfs::object_store::journal::super_block::SuperBlockInstance;
use fxfs::object_store::transaction::{lock_keys, LockKey, Mutation, Options};
use fxfs::object_store::volume::root_volume;
use fxfs::object_store::{
    AttributeKey, Directory, ExtentValue, HandleOptions, ObjectAttributes, ObjectDescriptor,
    ObjectKey, ObjectKind, ObjectStore, ObjectValue, PosixAttributes, Timestamp,
    DEFAULT_DATA_ATTRIBUTE_ID, NO_OWNER,
};
use std::collections::HashSet;
use storage_device::fake_device::FakeDevice;
use storage_device::DeviceHolder;

fn open_test_image(path: &str) -> FakeDevice {
    let path = std::path::PathBuf::from(path);
    FakeDevice::from_image(
        zstd::Decoder::new(std::fs::File::open(&path).expect("open image"))
            .expect("decompress image"),
        BLOCK_SIZE as u32,
    )
    .expect("open image")
}

fn inode_to_object_attributes(inode: &Inode, allocated_size: u64) -> ObjectAttributes {
    let mode = inode.header.mode;
    ObjectAttributes {
        creation_time: Timestamp { secs: inode.header.ctime, nanos: inode.header.ctime_nanos },
        modification_time: Timestamp { secs: inode.header.mtime, nanos: inode.header.mtime_nanos },
        project_id: 0,
        posix_attributes: Some(PosixAttributes {
            mode: mode.bits() as u32,
            uid: inode.header.uid,
            gid: inode.header.gid,
            rdev: 0,
        }),
        allocated_size,
        access_time: Timestamp { secs: inode.header.atime, nanos: inode.header.atime_nanos },
        change_time: Timestamp { secs: inode.header.ctime, nanos: inode.header.ctime_nanos },
    }
}

/// Migrates f2fs nodes to fxfs.
///
/// We preserve inode mappings (to object_id), attributes, xattr -- basically everything we can.
/// Some of these things are not easily achievable with standard fxfs interfaces like 'add_child'
/// so much of this work has to be done at the raw transaction/mutation level.
///
/// `existing_inodes` is used to handle hard links.
/// `f2fs_metadata_blocks` must be preserved to ensure that the resulting image is still parsable
/// as a valid f2fs image.
async fn recursively_migrate(
    f2fs: &F2fsReader,
    fxfs: &mut OpenFxFilesystem,
    ino: u32,
    dir: Directory<ObjectStore>,
    existing_inodes: &mut HashSet<u32>,
    inline_data: &mut Vec<(u64, Box<[u8]>)>,
    f2fs_metadata_blocks: &mut Vec<u32>,
) -> Result<(), Error> {
    // Any dentry blocks for this directory are f2fs metadata.
    let inode = f2fs.read_inode(ino).await?;
    f2fs_metadata_blocks.extend_from_slice(&inode.block_addrs);
    f2fs_metadata_blocks.append(&mut inode.data_blocks().map(|(_, x)| x).collect());

    for entry in f2fs.readdir(ino).await? {
        let object_id = entry.ino as u64;
        let inode = f2fs.read_inode(entry.ino).await?;
        let flags = inode.header.flags;
        let casefold = flags.contains(Flags::Casefold);
        let wrapping_key_id = None; // TODO(b/393449584): Add support

        let mut transaction = fxfs
            .clone()
            .new_transaction(
                lock_keys![
                    LockKey::object(dir.owner().store_object_id(), dir.object_id()),
                    LockKey::object(dir.owner().store_object_id(), object_id)
                ],
                Options::default(),
            )
            .await?;

        if !existing_inodes.insert(entry.ino) {
            // Hard link to an existing inode.
            ensure!(entry.file_type == FileType::RegularFile, "Hard link to non-file");
            transaction.add(
                dir.store().store_object_id(),
                Mutation::replace_or_insert_object(
                    ObjectKey::child(dir.object_id(), &entry.filename, casefold),
                    ObjectValue::child(object_id, ObjectDescriptor::File),
                ),
            );
            dir.store().adjust_refs(&mut transaction, object_id, 1).await?;
            transaction.commit().await?;
            continue;
        }

        // Both directories and files can have xattr.
        for xattr in &inode.xattr {
            // In f2fs, each xattr has an index byte that acts as a sort of namespace.
            // We will capture these verbatim and wire them into starnix.
            let mut name = vec![xattr.index as u8];
            name.extend_from_slice(&xattr.name);
            transaction.add(
                dir.store().store_object_id(),
                Mutation::replace_or_insert_object(
                    ObjectKey::extended_attribute(object_id, name),
                    ObjectValue::inline_extended_attribute(xattr.value.to_vec()),
                ),
            );
        }

        match entry.file_type {
            FileType::Directory => {
                transaction.add(
                    dir.owner().store_object_id(),
                    Mutation::insert_object(
                        ObjectKey::object(object_id),
                        ObjectValue::Object {
                            kind: ObjectKind::Directory { sub_dirs: 0, casefold, wrapping_key_id },
                            attributes: inode_to_object_attributes(&inode, 0),
                        },
                    ),
                );
                transaction.add(
                    dir.store().store_object_id(),
                    Mutation::replace_or_insert_object(
                        ObjectKey::child(dir.object_id(), &entry.filename, casefold),
                        ObjectValue::child(object_id, ObjectDescriptor::Directory),
                    ),
                );
                // Bump sub_dirs count in parent.
                let mut mutation =
                    dir.store().get_object_mutation(&transaction, dir.object_id()).await?;
                if let ObjectValue::Object {
                    kind: ObjectKind::Directory { sub_dirs, .. }, ..
                } = &mut mutation.item.value
                {
                    *sub_dirs = sub_dirs.saturating_add_signed(1);
                } else {
                    bail!("Parent is not a directory");
                };
                transaction.add(dir.store().store_object_id(), Mutation::ObjectStore(mutation));

                transaction.commit().await?;
                let new_dir = Directory::open_unchecked(
                    dir.owner().clone(),
                    object_id,
                    wrapping_key_id,
                    casefold,
                );
                Box::pin(recursively_migrate(
                    f2fs,
                    fxfs,
                    entry.ino,
                    new_dir,
                    existing_inodes,
                    inline_data,
                    f2fs_metadata_blocks,
                ))
                .await?;
            }
            FileType::RegularFile => {
                // Add inode block and related blocks to set of f2fs metadata blocks.
                f2fs_metadata_blocks.extend_from_slice(&inode.block_addrs);

                let mut allocated_size = 0;
                let inline_flags = inode.header.inline_flags;
                if inline_flags.contains(InlineFlags::Data) {
                    if inode.header.size > 0 {
                        // We can't allocate until finalize() so hold in RAM and write out last.
                        inline_data.push((object_id, inode.inline_data.as_ref().unwrap().clone()));
                        allocated_size = BLOCK_SIZE as u64;
                    }
                } else {
                    for (block_offset, block_addr) in inode.data_blocks() {
                        // TODO(b/393448875): Handle large fragmented files that
                        // might have us hit the transaction limit.
                        let device_range = block_addr as u64 * BLOCK_SIZE as u64
                            ..(block_addr as u64 + 1) * BLOCK_SIZE as u64;
                        let logical_range = block_offset as u64 * BLOCK_SIZE as u64
                            ..(block_offset as u64 + 1) * BLOCK_SIZE as u64;
                        dir.store()
                            .mark_allocated(
                                &mut transaction,
                                dir.store().store_object_id(),
                                device_range.clone(),
                            )
                            .await?;
                        let key_id = 0;
                        transaction.add(
                            dir.store().store_object_id(),
                            Mutation::merge_object(
                                ObjectKey::extent(
                                    object_id,
                                    DEFAULT_DATA_ATTRIBUTE_ID,
                                    logical_range,
                                ),
                                ObjectValue::Extent(ExtentValue::new_raw(
                                    device_range.start,
                                    key_id,
                                )),
                            ),
                        );
                        allocated_size += BLOCK_SIZE as u64;
                    }
                }

                transaction.add(
                    dir.owner().store_object_id(),
                    Mutation::insert_object(
                        ObjectKey::object(object_id),
                        ObjectValue::Object {
                            kind: ObjectKind::File { refs: 1 },
                            attributes: inode_to_object_attributes(&inode, allocated_size),
                        },
                    ),
                );
                transaction.add(
                    dir.owner().store_object_id(),
                    Mutation::insert_object(
                        ObjectKey::attribute(
                            object_id,
                            DEFAULT_DATA_ATTRIBUTE_ID,
                            AttributeKey::Attribute,
                        ),
                        ObjectValue::attribute(inode.header.size, false),
                    ),
                );
                transaction.add(
                    dir.owner().store_object_id(),
                    Mutation::replace_or_insert_object(
                        ObjectKey::child(dir.object_id(), &entry.filename, casefold),
                        ObjectValue::child(object_id, ObjectDescriptor::File),
                    ),
                );
                // TODO(b/393449584): Add encryption support
                transaction.commit().await?;
            }
            FileType::Symlink => {
                // Add inode block and related blocks to set of f2fs metadata blocks.
                f2fs_metadata_blocks.extend_from_slice(&inode.block_addrs);

                let link = f2fs.read_symlink(&inode)?;
                let object_attributes = inode_to_object_attributes(&inode, 0);
                transaction.add(
                    dir.owner().store_object_id(),
                    Mutation::insert_object(
                        ObjectKey::object(object_id),
                        ObjectValue::symlink(
                            link,
                            object_attributes.creation_time,
                            object_attributes.modification_time,
                            object_attributes.project_id,
                        ),
                    ),
                );
                transaction.add(
                    dir.owner().store_object_id(),
                    Mutation::replace_or_insert_object(
                        ObjectKey::child(dir.object_id(), &entry.filename, casefold),
                        ObjectValue::child(object_id, ObjectDescriptor::Symlink),
                    ),
                );
                transaction.commit().await?;
            }
            _ => unimplemented!(),
        }
    }
    Ok(())
}

async fn recursively_verify(
    f2fs: &F2fsReader,
    fxfs: &OpenFxFilesystem,
    ino: u32,
    dir: Directory<ObjectStore>,
) -> Result<(), Error> {
    for entry in f2fs.readdir(ino).await? {
        let object_id = entry.ino as u64;
        let inode = f2fs.read_inode(entry.ino).await.unwrap();
        let flags = inode.header.flags;
        let casefold = flags.contains(Flags::Casefold);
        let wrapping_key_id = None; // TODO(b/393449584): Add support

        match entry.file_type {
            FileType::Directory => {
                let dir = Directory::open_unchecked(
                    dir.owner().clone(),
                    object_id,
                    wrapping_key_id,
                    casefold,
                );

                for xattr in &inode.xattr {
                    let mut name = vec![xattr.index as u8];
                    name.extend_from_slice(&xattr.name);
                    let fxfs_xattr_value =
                        dir.get_extended_attribute(name).await.context("xattr read")?;
                    assert_eq!(&fxfs_xattr_value, xattr.value.as_ref());
                }

                let fxfs_properties = dir.get_properties().await?;
                let object_attributes = inode_to_object_attributes(&inode, 0);
                let f2fs_properties = ObjectProperties {
                    refs: 1,
                    allocated_size: 0,
                    data_attribute_size: 0,
                    creation_time: object_attributes.creation_time,
                    modification_time: object_attributes.modification_time,
                    access_time: object_attributes.access_time,
                    change_time: object_attributes.change_time,
                    sub_dirs: inode.header.links as u64 - 2,
                    posix_attributes: object_attributes.posix_attributes,
                    casefold,
                    wrapping_key_id,
                };
                let h = inode.header;
                assert_eq!(
                    fxfs_properties, f2fs_properties,
                    "entry {entry:?}, inode header: {h:?}"
                );

                Box::pin(recursively_verify(f2fs, fxfs, entry.ino, dir)).await.unwrap();
            }
            FileType::RegularFile => {
                let handle = ObjectStore::open_object(
                    &dir.owner(),
                    object_id,
                    HandleOptions::default(),
                    None,
                )
                .await?;

                for xattr in &inode.xattr {
                    let mut name = vec![xattr.index as u8];
                    name.extend_from_slice(&xattr.name);
                    let fxfs_xattr_value =
                        handle.get_extended_attribute(name).await.context("xattr read")?;
                    assert_eq!(&fxfs_xattr_value, xattr.value.as_ref());
                }

                let fxfs_properties = handle.get_properties().await?;
                let f2fs_allocated_size = if let Some(data) = inode.inline_data.as_ref() {
                    if data.len() > 0 {
                        BLOCK_SIZE as u64
                    } else {
                        0
                    }
                } else {
                    inode.data_blocks().count() as u64 * BLOCK_SIZE as u64
                };
                let object_attributes = inode_to_object_attributes(&inode, f2fs_allocated_size);
                let f2fs_properties = ObjectProperties {
                    refs: inode.header.links as u64,
                    allocated_size: object_attributes.allocated_size,
                    data_attribute_size: inode.header.size,
                    creation_time: object_attributes.creation_time,
                    modification_time: object_attributes.modification_time,
                    access_time: object_attributes.access_time,
                    change_time: object_attributes.change_time,
                    sub_dirs: 0,
                    posix_attributes: object_attributes.posix_attributes,
                    casefold,
                    wrapping_key_id,
                };
                assert_eq!(fxfs_properties, f2fs_properties);

                let inline_flags = inode.header.inline_flags;
                if inline_flags.contains(InlineFlags::Data) {
                    let mut buffer = handle.allocate_buffer(BLOCK_SIZE).await;
                    handle.read(0, 0, buffer.as_mut()).await?;
                    let f2fs_block = inode.inline_data.unwrap();
                    assert_eq!(
                        &buffer.as_slice()[..f2fs_block.len()],
                        f2fs_block.as_ref(),
                        "Inline data mismatch."
                    );
                } else {
                    for (block_offset, block_addr) in inode.data_blocks() {
                        let mut buffer = handle.allocate_buffer(BLOCK_SIZE).await;
                        handle
                            .read(0, block_offset as u64 * BLOCK_SIZE as u64, buffer.as_mut())
                            .await?;
                        let f2fs_block = f2fs.read_data(&inode, block_offset).await?.unwrap();
                        // Note that the whole block should match, but fxfs won't leak us the
                        // whole block if it's the last block.
                        let len = std::cmp::min(
                            BLOCK_SIZE,
                            inode.header.size as usize - BLOCK_SIZE * block_offset as usize,
                        );
                        assert_eq!(
                            buffer.as_slice()[..len],
                            f2fs_block.as_slice()[..len],
                            "Block mismatch at {block_offset} {block_addr} for {inode:?}"
                        );
                    }
                }
            }
            FileType::Symlink => {
                let f2fs_link = f2fs.read_symlink(&inode)?;
                let fxfs_link = dir.store().read_symlink(object_id).await?;
                assert_eq!(f2fs_link.as_ref(), &fxfs_link);
            }
            _ => unimplemented!(),
        }
    }
    Ok(())
}

/// Reserves disk regions in fxfs to ensure that we don't overwrite critical f2fs metadata.
async fn reserve_f2fs_metadata(
    fxfs: &mut OpenFxFilesystem,
    f2fs_main_blkaddr: u32, // Start of the 'data' region.
    blocks: &[u32],
) -> Result<(), Error> {
    let handle;
    let mut transaction = fxfs
        .clone()
        .new_transaction(lock_keys![], Options::default())
        .await
        .expect("new reserve f2fs metadata transaction");
    handle = ObjectStore::create_object(
        &fxfs.root_store(),
        &mut transaction,
        HandleOptions::default(),
        None,
    )
    .await
    .expect("failed to create object");
    // Region between first and second fxfs superblock.
    handle.extend(&mut transaction, 4096..128 * BLOCK_SIZE as u64).await?;
    // Region after second fxfs superblock to end of f2fs metadata region.
    handle
        .extend(
            &mut transaction,
            129 * BLOCK_SIZE as u64..f2fs_main_blkaddr as u64 * BLOCK_SIZE as u64,
        )
        .await?;
    for &block in blocks {
        let byte_range = block as u64 * BLOCK_SIZE as u64..(block as u64 + 1) * BLOCK_SIZE as u64;
        handle.extend(&mut transaction, byte_range).await?;
    }
    transaction.add(
        fxfs.root_store().store_object_id(),
        Mutation::replace_or_insert_object(
            ObjectKey::graveyard_entry(
                fxfs.root_store().graveyard_directory_object_id(),
                handle.object_id(),
            ),
            ObjectValue::Some,
        ),
    );
    transaction.commit().await?;
    Ok(())
}

#[fuchsia::test]
async fn test_fxfs_migration() {
    let device = DeviceHolder::new(open_test_image("/pkg/testdata/f2fs.img.zst"));
    let mut fxfs = FxFilesystemBuilder::new()
        .format(true)
        .trim_config(None)
        // F2fs superblock is stored in same block as Fxfs block A, so avoid that.
        .image_builder_mode(Some(SuperBlockInstance::B))
        .open(device)
        .await
        .expect("Failed to create fxfs filesystem builder");
    let original_superblock;

    {
        let f2fs = F2fsReader::open_device(fxfs.device()).await.expect("f2fs open ok");
        original_superblock = f2fs.superblock;

        // Create a "userdata" volume in fxfs.
        let root_volume = root_volume(fxfs.clone()).await.expect("Opening root volume");
        let vol = root_volume.new_volume("userdata", NO_OWNER, None).await.expect("Opening volume");
        let root_directory =
            Directory::open(&vol, vol.root_directory_object_id()).await.expect("open failed");

        assert!(vol.last_object_id() < 3, "inodes from volume overlap with f2fs.");

        // Copy everything from f2fs to userdata, reusing existing extents.
        let ino = f2fs.root_ino();
        let mut existing_inodes = HashSet::new();
        let mut inline_data = Vec::new();
        let mut f2fs_metadata_blocks = Vec::new();
        recursively_migrate(
            &f2fs,
            &mut fxfs,
            ino,
            root_directory,
            &mut existing_inodes,
            &mut inline_data,
            &mut f2fs_metadata_blocks,
        )
        .await
        .expect("walk");

        // TODO(b/393448875): We are using the graveyard here to reserve the extents containing f2fs
        // metadata until next boot. This could be avoided with a bit more work. Currently unclear
        // if this is worth the complexity though.
        //
        // The Fxfs allocator caps the number of free extents it holds in its free lists in RAM.
        // If it exhausts its memory-backed free lists, it will scan the allocator LSM tree to
        // find more extents. In this case we're reaching in and manipulating the in-memory
        // structure without associated LSM tree commitments so, while unlikely, there is a risk
        // that in very large filesystems we might run into this allocator 'rebuild' behavior.
        reserve_f2fs_metadata(&mut fxfs, original_superblock.main_blkaddr, &f2fs_metadata_blocks)
            .await
            .expect("reserve f2fs metadata");

        // Bump last_object_id to avoid an inode collision with data we just added.
        vol.maybe_bump_last_object_id(f2fs.max_ino() as u64).expect("bump last_object_id");

        fxfs.finalize().await.expect("finalize");
        // Now we're allowed to allocate, write out any inlined files.

        for (object_id, data) in inline_data {
            let mut transaction = fxfs
                .clone()
                .new_transaction(
                    lock_keys![LockKey::object(vol.store_object_id(), object_id)],
                    Options::default(),
                )
                .await
                .expect("new inline data transaction");
            let device_range = fxfs
                .allocator()
                .allocate(&mut transaction, vol.store_object_id(), BLOCK_SIZE as u64)
                .await
                .expect("allocate");
            {
                let device = fxfs.device();
                let mut buffer = device.allocate_buffer(BLOCK_SIZE).await;
                buffer.as_mut_slice()[..data.len()].copy_from_slice(data.as_ref());
                device.write(device_range.start, buffer.as_ref()).await.expect("write");
            }
            let key_id = 0;
            transaction.add(
                vol.store_object_id(),
                Mutation::merge_object(
                    ObjectKey::extent(object_id, DEFAULT_DATA_ATTRIBUTE_ID, 0..BLOCK_SIZE as u64),
                    ObjectValue::Extent(ExtentValue::new_raw(device_range.start, key_id)),
                ),
            );
            transaction.commit().await.expect("commit inline data");
        }

        fxfs.close().await.expect("close fxfs");
    }
    let actual_size = fxfs.allocator().maximum_offset();
    let device = fxfs.take_device().await;
    println!("Final filesystem size is {actual_size}.");

    // Reopen RW so we can mount Fxfs normally.
    device.reopen(false);
    let fxfs = FxFilesystemBuilder::new().read_only(true).open(device).await.expect("open failed");
    // Re-open as f2fs and do it all again, this time verifying.
    let f2fs = F2fsReader::open_device(fxfs.device().clone()).await.expect("f2fs open ok");
    assert_eq!(original_superblock, f2fs.superblock);

    fxfs::fsck::fsck(fxfs.clone()).await.expect("fsck failed");
    let root_volume = root_volume(fxfs.clone()).await.expect("Opening root volume");
    let vol = root_volume.volume("userdata", NO_OWNER, None).await.expect("Opening volume");
    fxfs::fsck::fsck_volume(&fxfs, vol.store_object_id(), None).await.expect("fsck volume");
    let root_directory =
        Directory::open(&vol, vol.root_directory_object_id()).await.expect("open failed");
    let ino = f2fs.root_ino();
    recursively_verify(&f2fs, &fxfs, ino, root_directory).await.expect("verify");

    fxfs.close().await.expect("close ok");
}
