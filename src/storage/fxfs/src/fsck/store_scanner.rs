// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{
        fsck::{
            errors::{FsckError, FsckFatal, FsckWarning},
            Fsck,
        },
        lsm_tree::types::{Item, ItemRef, LayerIterator, MutableLayer},
        object_handle::INVALID_OBJECT_ID,
        object_store::{
            allocator::{self, AllocatorKey, AllocatorValue},
            graveyard::Graveyard,
            AttributeKey, ChildValue, EncryptionKeys, ExtendedAttributeValue, ExtentKey,
            ExtentValue, ObjectAttributes, ObjectDescriptor, ObjectKey, ObjectKeyData, ObjectKind,
            ObjectStore, ObjectValue, ProjectProperty, DEFAULT_DATA_ATTRIBUTE_ID,
        },
        range::RangeExt,
        round::round_up,
    },
    anyhow::Error,
    fxfs_crypto::WrappedKeys,
    std::{
        cell::UnsafeCell,
        collections::{btree_map::BTreeMap, hash_set::HashSet},
        convert::TryInto,
        iter::Iterator,
        ops::{Bound, Range},
    },
};

#[derive(Debug)]
struct ScannedFile {
    // A list of attribute IDs found for the file, along with their logical size.
    attributes: Vec<(u64, u64)>,
    // A list of parent object IDs for the file.  INVALID_OBJECT_ID indicates a reference from
    // outside the object store (either the graveyard, or because the object is a root object of the
    // store and probably has a reference to it in e.g. the StoreInfo or superblock).
    parents: Vec<u64>,
    // The file's allocated size, according to its metadata.
    stored_allocated_size: u64,
    // The allocated size of the file (computed by summing up the extents for the file).
    observed_allocated_size: u64,
    // The object is in the graveyard which means extents beyond the end of the file are allowed.
    in_graveyard: bool,
    // The number of references to the file, according to its metadata.
    stored_refs: u64,
}

#[derive(Debug)]
struct ScannedDir {
    // The number of sub-directories of the dir, according to its metadata.
    stored_sub_dirs: u64,
    // The number of sub-directories we found for the dir.
    observed_sub_dirs: u64,
    // The parent object of the directory.  See ScannedFile::parents.  Note that directories can
    // only have one parent, hence this is just an Option (not a Vec).
    parent: Option<u64>,
    // Used to detect directory cycles.
    visited: UnsafeCell<bool>,
}

unsafe impl Sync for ScannedDir {}

#[derive(Debug)]
enum ScannedObject {
    Directory(ScannedDir),
    File(ScannedFile),
    Graveyard,
    Symlink,
    // A tombstoned object, which should have no other records associated with it.
    Tombstone,
}

struct ScannedStore<'a> {
    fsck: &'a Fsck<'a>,
    objects: BTreeMap<u64, ScannedObject>,
    root_objects: Vec<u64>,
    store_id: u64,
    is_root_store: bool,
    is_encrypted: bool,
    current_file: Option<CurrentFile>,
    root_dir_id: u64,
    stored_project_usages: BTreeMap<u64, (i64, i64)>,
    total_project_usages: BTreeMap<u64, (i64, i64)>,
    /// Tracks project ids used and an example node to examine if it should not have been included.
    used_project_ids: BTreeMap<u64, u64>,
}

struct CurrentFile {
    object_id: u64,
    key_ids: HashSet<u64>,
}

impl<'a> ScannedStore<'a> {
    fn new(
        fsck: &'a Fsck<'a>,
        root_objects: impl AsRef<[u64]>,
        store_id: u64,
        is_root_store: bool,
        is_encrypted: bool,
        root_dir_id: u64,
    ) -> Self {
        Self {
            fsck,
            objects: BTreeMap::new(),
            root_objects: root_objects.as_ref().into(),
            store_id,
            is_root_store,
            is_encrypted,
            current_file: None,
            root_dir_id,
            stored_project_usages: BTreeMap::new(),
            total_project_usages: BTreeMap::new(),
            used_project_ids: BTreeMap::new(),
        }
    }

    // Process an object store record, adding or updating any objects known to the ScannedStore.
    fn process(&mut self, key: &ObjectKey, value: &ObjectValue) -> Result<(), Error> {
        match key.data {
            ObjectKeyData::Object => {
                match value {
                    ObjectValue::None => {
                        if self.objects.insert(key.object_id, ScannedObject::Tombstone).is_some() {
                            self.fsck.error(FsckError::TombstonedObjectHasRecords(
                                self.store_id,
                                key.object_id,
                            ))?;
                        }
                    }
                    ObjectValue::Some => {
                        self.fsck.error(FsckError::UnexpectedRecordInObjectStore(
                            self.store_id,
                            key.into(),
                            value.into(),
                        ))?;
                    }
                    ObjectValue::Object {
                        kind: ObjectKind::File { refs },
                        attributes: ObjectAttributes { project_id, allocated_size, .. },
                    } => {
                        if *project_id > 0 {
                            self.used_project_ids.insert(*project_id, key.object_id);
                            let entry = self.total_project_usages.entry(*project_id).or_default();
                            entry.0 += i64::try_from(*allocated_size).unwrap();
                            entry.1 += 1;
                        }
                        self.current_file =
                            Some(CurrentFile { object_id: key.object_id, key_ids: HashSet::new() });
                        let parents = if self.root_objects.contains(&key.object_id) {
                            vec![INVALID_OBJECT_ID]
                        } else {
                            vec![]
                        };
                        self.objects.insert(
                            key.object_id,
                            ScannedObject::File(ScannedFile {
                                attributes: vec![],
                                parents,
                                stored_allocated_size: *allocated_size,
                                observed_allocated_size: 0,
                                in_graveyard: false,
                                stored_refs: *refs,
                            }),
                        );
                    }
                    ObjectValue::Object {
                        kind: ObjectKind::Directory { sub_dirs },
                        attributes: ObjectAttributes { project_id, .. },
                    } => {
                        if *project_id > 0 {
                            self.used_project_ids.insert(*project_id, key.object_id);
                            let entry = self.total_project_usages.entry(*project_id).or_default();
                            // Increment only nodes.
                            entry.1 += 1;
                        }
                        let parent = if self.root_objects.contains(&key.object_id) {
                            Some(INVALID_OBJECT_ID)
                        } else {
                            None
                        };
                        // We've verified no duplicate keys, and Object records come first,
                        // so this should always be the first time we encounter this object.
                        self.objects.insert(
                            key.object_id,
                            ScannedObject::Directory(ScannedDir {
                                stored_sub_dirs: *sub_dirs,
                                observed_sub_dirs: 0,
                                parent,
                                visited: UnsafeCell::new(false),
                            }),
                        );
                    }
                    ObjectValue::Object { kind: ObjectKind::Graveyard, attributes } => {
                        self.objects.insert(key.object_id, ScannedObject::Graveyard);
                        if attributes.project_id != 0 {
                            self.fsck.error(FsckError::ProjectOnGraveyard(
                                self.store_id,
                                attributes.project_id,
                                key.object_id,
                            ))?;
                        }
                    }
                    ObjectValue::Object {
                        kind: ObjectKind::Symlink { .. },
                        attributes: ObjectAttributes { project_id, .. },
                    } => {
                        if *project_id > 0 {
                            self.used_project_ids.insert(*project_id, key.object_id);
                            let entry = self.total_project_usages.entry(*project_id).or_default();
                            // Increment only nodes.
                            entry.1 += 1;
                        }
                        self.objects.insert(key.object_id, ScannedObject::Symlink);
                    }
                    _ => {
                        self.fsck.error(FsckError::MalformedObjectRecord(
                            self.store_id,
                            key.into(),
                            value.into(),
                        ))?;
                    }
                }
            }
            ObjectKeyData::Keys => {
                if let ObjectValue::Keys(keys) = value {
                    match keys {
                        EncryptionKeys::AES256XTS(WrappedKeys(keys)) => {
                            if let Some(current_file) = &mut self.current_file {
                                // Duplicates items should have already been checked, but not
                                // duplicate key IDs.
                                assert!(current_file.key_ids.is_empty());
                                for (key_id, _) in keys {
                                    if !current_file.key_ids.insert(*key_id) {
                                        self.fsck.error(FsckError::DuplicateKey(
                                            self.store_id,
                                            key.object_id,
                                            *key_id,
                                        ))?;
                                    }
                                }
                            } else {
                                self.fsck.warning(FsckWarning::OrphanedKeys(
                                    self.store_id,
                                    key.object_id,
                                ))?;
                            }
                        }
                    }
                } else {
                    self.fsck.error(FsckError::MalformedObjectRecord(
                        self.store_id,
                        key.into(),
                        value.into(),
                    ))?;
                }
            }
            ObjectKeyData::Attribute(attribute_id, AttributeKey::Attribute) => {
                match value {
                    ObjectValue::Attribute { size } => {
                        match self.objects.get_mut(&key.object_id) {
                            Some(ScannedObject::File(ScannedFile { attributes, .. })) => {
                                attributes.push((attribute_id, *size));
                            }
                            Some(ScannedObject::Directory(..) | ScannedObject::Symlink) => {
                                self.fsck.error(FsckError::AttributeNotOnFile(
                                    self.store_id,
                                    key.object_id,
                                ))?;
                            }
                            Some(ScannedObject::Graveyard) => { /* NOP */ }
                            Some(ScannedObject::Tombstone) => {
                                self.fsck.error(FsckError::TombstonedObjectHasRecords(
                                    self.store_id,
                                    key.object_id,
                                ))?;
                            }
                            None => {
                                // We verify key ordering elsewhere, and Object records come before
                                // Attribute records, so we should never find an attribute without
                                // its object already encountered.  Thus, this is an orphaned
                                // attribute.
                                self.fsck.warning(FsckWarning::OrphanedAttribute(
                                    self.store_id,
                                    key.object_id,
                                    attribute_id,
                                ))?;
                            }
                        }
                    }
                    _ => {
                        self.fsck.error(FsckError::MalformedObjectRecord(
                            self.store_id,
                            key.into(),
                            value.into(),
                        ))?;
                    }
                }
            }
            // Mostly ignore extents on this pass. We'll process them later.
            ObjectKeyData::Attribute(_, AttributeKey::Extent(_)) => {
                match value {
                    // Regular extent record.
                    ObjectValue::Extent(ExtentValue::Some { key_id, .. }) => {
                        if let Some(current_file) = &self.current_file {
                            if !self.is_encrypted && *key_id == 0 && current_file.key_ids.is_empty()
                            {
                                // Unencrypted files in unencrypted stores should use key ID 0.
                            } else if !current_file.key_ids.contains(key_id) {
                                self.fsck.error(FsckError::MissingKey(
                                    self.store_id,
                                    key.object_id,
                                    *key_id,
                                ))?;
                            }
                        } else {
                            // This must be an orphaned extent, which should get picked up later.
                        }
                    }
                    // Deleted extent.
                    ObjectValue::Extent(ExtentValue::None) => {}
                    _ => {
                        self.fsck.error(FsckError::MalformedObjectRecord(
                            self.store_id,
                            key.into(),
                            value.into(),
                        ))?;
                    }
                }
            }
            ObjectKeyData::Child { name: ref _name } => match value {
                ObjectValue::None => {}
                ObjectValue::Child(ChildValue { object_id: child_id, object_descriptor }) => {
                    if *child_id == INVALID_OBJECT_ID {
                        self.fsck.warning(FsckWarning::InvalidObjectIdInStore(
                            self.store_id,
                            key.into(),
                            value.into(),
                        ))?;
                    }
                    if self.root_objects.contains(child_id) {
                        self.fsck.error(FsckError::RootObjectHasParent(
                            self.store_id,
                            *child_id,
                            key.object_id,
                        ))?;
                    }
                    if object_descriptor == &ObjectDescriptor::Volume && !self.is_root_store {
                        self.fsck.error(FsckError::VolumeInChildStore(self.store_id, *child_id))?;
                    }
                }
                _ => {
                    self.fsck.error(FsckError::MalformedObjectRecord(
                        self.store_id,
                        key.into(),
                        value.into(),
                    ))?;
                }
            },
            ObjectKeyData::Project { project_id, property: ProjectProperty::Limit } => {
                // Should only be set on the root object store
                if self.root_dir_id != key.object_id {
                    self.fsck.error(FsckError::NonRootProjectIdMetadata(
                        self.store_id,
                        key.object_id,
                        project_id,
                    ))?;
                }
                match value {
                    ObjectValue::None | ObjectValue::BytesAndNodes { .. } => {}
                    _ => {
                        self.fsck.error(FsckError::MalformedObjectRecord(
                            self.store_id,
                            key.into(),
                            value.into(),
                        ))?;
                    }
                }
            }
            ObjectKeyData::Project { project_id, property: ProjectProperty::Usage } => {
                // Should only be set on the root object store
                if self.root_dir_id != key.object_id {
                    self.fsck.error(FsckError::NonRootProjectIdMetadata(
                        self.store_id,
                        key.object_id,
                        project_id,
                    ))?;
                }
                match value {
                    ObjectValue::None => {
                        self.stored_project_usages.remove(&project_id);
                    }
                    ObjectValue::BytesAndNodes { bytes, nodes } => {
                        self.stored_project_usages.insert(project_id, (*bytes, *nodes));
                    }
                    _ => {
                        self.fsck.error(FsckError::MalformedObjectRecord(
                            self.store_id,
                            key.into(),
                            value.into(),
                        ))?;
                    }
                }
            }
            ObjectKeyData::ExtendedAttribute { .. } => match value {
                ObjectValue::None => {}
                ObjectValue::ExtendedAttribute(ExtendedAttributeValue::Inline(_)) => {}
                // TODO(fxbug.dev/122123): do more validation on the non-inline values once we
                // support them.
                ObjectValue::ExtendedAttribute(ExtendedAttributeValue::AttributeId(_)) => {}
                _ => {
                    self.fsck.error(FsckError::MalformedObjectRecord(
                        self.store_id,
                        key.into(),
                        value.into(),
                    ))?;
                }
            },
            ObjectKeyData::GraveyardEntry { .. } => {}
        }
        Ok(())
    }

    // Performs some checks on the child link and records information such as the sub-directory
    // count that get verified later.
    fn process_child(
        &mut self,
        parent_id: u64,
        child_id: u64,
        object_descriptor: &ObjectDescriptor,
    ) -> Result<(), Error> {
        match (self.objects.get_mut(&child_id), object_descriptor) {
            (
                Some(ScannedObject::File(ScannedFile { parents, .. })),
                ObjectDescriptor::File | ObjectDescriptor::Volume,
            ) => {
                parents.push(parent_id);
            }
            (
                Some(ScannedObject::Directory(ScannedDir { parent, .. })),
                ObjectDescriptor::Directory,
            ) => {
                if parent.is_some() {
                    // TODO(fxbug.dev/87381): Accumulating and reporting all parents
                    // might be useful.
                    self.fsck
                        .error(FsckError::MultipleLinksToDirectory(self.store_id, child_id))?;
                }
                *parent = Some(parent_id);
            }
            (Some(ScannedObject::Tombstone), _) => {
                self.fsck.error(FsckError::TombstonedObjectHasRecords(self.store_id, parent_id))?;
                return Ok(());
            }
            (None, _) => {
                self.fsck.error(FsckError::MissingObjectInfo(self.store_id, child_id))?;
                return Ok(());
            }
            (Some(s), _) => {
                let expected = match s {
                    ScannedObject::Directory(_) => ObjectDescriptor::Directory,
                    ScannedObject::File(_) | ScannedObject::Graveyard => ObjectDescriptor::File,
                    ScannedObject::Symlink => ObjectDescriptor::Symlink,
                    ScannedObject::Tombstone => unreachable!(),
                };
                if &expected != object_descriptor {
                    self.fsck.error(FsckError::ConflictingTypeForLink(
                        self.store_id,
                        child_id,
                        expected.into(),
                        object_descriptor.into(),
                    ))?;
                }
            }
        }
        match self.objects.get_mut(&parent_id) {
            Some(ScannedObject::File(..) | ScannedObject::Graveyard | ScannedObject::Symlink) => {
                self.fsck.error(FsckError::ObjectHasChildren(self.store_id, parent_id))?;
            }
            Some(ScannedObject::Directory(ScannedDir { observed_sub_dirs, .. })) => {
                if *object_descriptor == ObjectDescriptor::Directory {
                    *observed_sub_dirs += 1;
                }
            }
            Some(ScannedObject::Tombstone) => {
                self.fsck.error(FsckError::TombstonedObjectHasRecords(self.store_id, parent_id))?;
            }
            None => self.fsck.error(FsckError::MissingObjectInfo(self.store_id, parent_id))?,
        }
        Ok(())
    }

    // Process an extent, performing some checks and building fsck.allocations.
    async fn process_extent(
        &mut self,
        object_id: u64,
        attribute_id: u64,
        range: &Range<u64>,
        device_offset: Option<u64>,
        bs: u64,
    ) -> Result<(), Error> {
        if range.start % bs > 0 || range.end % bs > 0 {
            self.fsck.error(FsckError::MisalignedExtent(
                self.store_id,
                object_id,
                range.clone(),
                0,
            ))?;
        }
        if range.start >= range.end {
            self.fsck.error(FsckError::MalformedExtent(
                self.store_id,
                object_id,
                range.clone(),
                0,
            ))?;
            return Ok(());
        }
        let len = range.end - range.start;
        match self.objects.get_mut(&object_id) {
            Some(ScannedObject::File(ScannedFile {
                attributes,
                observed_allocated_size: allocated_size,
                in_graveyard,
                ..
            })) => {
                match attributes.iter().find(|(attr_id, _)| *attr_id == attribute_id) {
                    Some((_, size)) => {
                        if device_offset.is_some()
                            && !*in_graveyard
                            && range.end > round_up(*size, bs).unwrap()
                        {
                            self.fsck.error(FsckError::ExtentExceedsLength(
                                self.store_id,
                                object_id,
                                attribute_id,
                                *size,
                                range.into(),
                            ))?;
                        }
                    }
                    None => {
                        self.fsck.warning(FsckWarning::ExtentForMissingAttribute(
                            self.store_id,
                            object_id,
                            attribute_id,
                        ))?;
                    }
                }
                if device_offset.is_some() {
                    *allocated_size += len;
                }
            }
            Some(ScannedObject::Directory(..)) => {
                self.fsck.warning(FsckWarning::ExtentForDirectory(self.store_id, object_id))?;
            }
            Some(_) => { /* NOP */ }
            None => {
                self.fsck
                    .warning(FsckWarning::ExtentForNonexistentObject(self.store_id, object_id))?;
            }
        }
        if let Some(device_offset) = device_offset {
            if device_offset % bs > 0 {
                self.fsck.error(FsckError::MisalignedExtent(
                    self.store_id,
                    object_id,
                    range.clone(),
                    device_offset,
                ))?;
            }
            let item = Item::new(
                AllocatorKey { device_range: device_offset..device_offset + len },
                AllocatorValue::Abs { count: 1, owner_object_id: self.store_id },
            );
            let lower_bound = item.key.lower_bound_for_merge_into();
            self.fsck.allocations.merge_into(item, &lower_bound, allocator::merge::merge).await;
        }
        Ok(())
    }

    // A graveyard entry can either be for tombstoning a file (if `tombstone` is true), or for
    // trimming a file.
    fn handle_graveyard_entry(&mut self, object_id: u64, tombstone: bool) -> Result<(), Error> {
        match self.objects.get_mut(&object_id) {
            Some(ScannedObject::File(ScannedFile { parents, in_graveyard, .. })) => {
                *in_graveyard = true;
                if tombstone {
                    parents.push(INVALID_OBJECT_ID)
                }
            }
            Some(_) => {
                self.fsck.error(FsckError::UnexpectedObjectInGraveyard(object_id))?;
            }
            None => {
                self.fsck.warning(FsckWarning::GraveyardRecordForAbsentObject(
                    self.store_id,
                    object_id,
                ))?;
            }
        }
        Ok(())
    }

    // Called when all items for the current file have been processed.
    fn finish_file(&mut self) -> Result<(), Error> {
        if let Some(current_file) = self.current_file.take() {
            // If the store is unencrypted, then the file might or might not have encryption keys
            // (e.g. the root store has encrypted layer files).
            if self.is_encrypted && current_file.key_ids.is_empty() {
                self.fsck.error(FsckError::MissingEncryptionKeys(
                    self.store_id,
                    current_file.object_id,
                ))?;
            }
        }
        Ok(())
    }
}

// Scans extents and directory child entries in the store, emitting synthesized allocations into
// |fsck.allocations|, updating the sizes for files in |scanned| and performing checks on directory
// children.
// TODO(fxbug.dev/95475): Roll the extent scanning back into main function.
async fn scan_extents_and_directory_children<'a>(
    store: &ObjectStore,
    scanned: &mut ScannedStore<'a>,
) -> Result<(), Error> {
    let bs = store.block_size();
    let layer_set = store.tree().layer_set();
    let mut merger = layer_set.merger();
    let mut iter = merger.seek(Bound::Unbounded).await?;
    let mut allocated_bytes = 0;
    while let Some(itemref) = iter.get() {
        match itemref {
            ItemRef {
                key:
                    ObjectKey {
                        object_id,
                        data:
                            ObjectKeyData::Attribute(
                                attribute_id,
                                AttributeKey::Extent(ExtentKey { range }),
                            ),
                    },
                value: ObjectValue::Extent(extent),
                ..
            } => {
                let device_offset = if let ExtentValue::Some { device_offset, .. } = extent {
                    allocated_bytes += range.length().unwrap_or(0);
                    Some(*device_offset)
                } else {
                    None
                };
                scanned.process_extent(*object_id, *attribute_id, range, device_offset, bs).await?
            }
            ItemRef {
                key: ObjectKey { object_id, data: ObjectKeyData::Child { .. } },
                value: ObjectValue::Child(ChildValue { object_id: child_id, object_descriptor }),
                ..
            } => scanned.process_child(*object_id, *child_id, object_descriptor)?,
            _ => {}
        }
        iter.advance().await?;
    }
    scanned.fsck.verbose(format!(
        "Store {} has {} bytes allocated",
        store.store_object_id(),
        allocated_bytes
    ));
    Ok(())
}

/// Scans an object store, accumulating all of its allocations into |fsck.allocations| and
/// validating various object properties.
pub(super) async fn scan_store(
    fsck: &Fsck<'_>,
    store: &ObjectStore,
    root_objects: impl AsRef<[u64]>,
) -> Result<(), Error> {
    let store_id = store.store_object_id();

    let mut scanned = ScannedStore::new(
        fsck,
        root_objects,
        store_id,
        store.is_root(),
        store.is_encrypted(),
        store.root_directory_object_id(),
    );

    // Scan the store for objects, attributes, and parent/child relationships.
    let layer_set = store.tree().layer_set();
    let mut merger = layer_set.merger();
    let mut iter = merger.seek(Bound::Unbounded).await?;
    let mut last_item: Option<Item<ObjectKey, ObjectValue>> = None;
    while let Some(item) = iter.get() {
        if let Some(last_item) = last_item {
            if last_item.key >= *item.key {
                fsck.fatal(FsckFatal::MisOrderedObjectStore(store_id))?;
            }
        }
        if item.key.object_id == INVALID_OBJECT_ID {
            fsck.warning(FsckWarning::InvalidObjectIdInStore(
                store_id,
                item.key.into(),
                item.value.into(),
            ))?;
        }
        if let Some(current_file) = &scanned.current_file {
            if item.key.object_id != current_file.object_id {
                scanned.finish_file()?;
            }
        }
        scanned.process(item.key, item.value)?;
        last_item = Some(item.cloned());
        iter.advance().await?;
    }
    scanned.finish_file()?;

    for (project_id, node_id) in scanned.used_project_ids.iter() {
        if !scanned.stored_project_usages.contains_key(project_id) {
            fsck.error(FsckError::ProjectUsedWithNoUsageTracking(store_id, *project_id, *node_id))?;
        }
    }
    for (project_id, (bytes_stored, nodes_stored)) in scanned.stored_project_usages.iter() {
        if let Some((bytes_used, nodes_used)) = scanned.total_project_usages.get(&project_id) {
            if *bytes_stored != *bytes_used || *nodes_stored != *nodes_used {
                fsck.warning(FsckWarning::ProjectUsageInconsistent(
                    store_id,
                    *project_id,
                    (*bytes_stored, *nodes_stored),
                    (*bytes_used, *nodes_used),
                ))?;
            }
        } else {
            if *bytes_stored > 0 || *nodes_stored > 0 {
                fsck.warning(FsckWarning::ProjectUsageInconsistent(
                    store_id,
                    *project_id,
                    (*bytes_stored, *nodes_stored),
                    (0, 0),
                ))?;
            }
        }
    }

    // Add a reference for files in the graveyard (which acts as the file's parent until it is
    // purged, leaving only the Object record in the original store and no links to the file).
    // This must be done after scanning the object store.
    let layer_set = store.tree().layer_set();
    let mut merger = layer_set.merger();
    let mut iter = fsck.assert(
        Graveyard::iter(store.graveyard_directory_object_id(), &mut merger).await,
        FsckFatal::MalformedGraveyard,
    )?;
    while let Some((object_id, _, value)) = iter.get() {
        match value {
            ObjectValue::Some => scanned.handle_graveyard_entry(object_id, true)?,
            ObjectValue::Trim => scanned.handle_graveyard_entry(object_id, false)?,
            _ => fsck.error(FsckError::BadGraveyardValue(store_id, object_id))?,
        }
        fsck.assert(iter.advance().await, FsckFatal::MalformedGraveyard)?;
    }

    scan_extents_and_directory_children(store, &mut scanned).await?;

    // At this point, we've provided all of the inputs to |scanned|.

    // Mark all the root directories as visited so that cycle detection below works.
    for oid in scanned.root_objects {
        if let Some(ScannedObject::Directory(ScannedDir { visited, .. })) =
            scanned.objects.get_mut(&oid)
        {
            *visited.get_mut() = true;
        }
    }

    // Iterate over all objects performing checks we were unable to perform earlier.
    let mut num_objects = 0;
    let mut files = 0;
    let mut directories = 0;
    let mut symlinks = 0;
    let mut tombstones = 0;
    let mut other = 0;
    let mut stack = Vec::new();
    for (object_id, object) in &scanned.objects {
        num_objects += 1;
        match object {
            ScannedObject::File(ScannedFile {
                attributes,
                parents,
                stored_allocated_size,
                observed_allocated_size,
                stored_refs,
                ..
            }) => {
                files += 1;
                let observed_refs = parents.len().try_into().unwrap();
                // observed_refs == 0 is handled separately to distinguish orphaned objects
                if observed_refs != *stored_refs && observed_refs > 0 {
                    fsck.error(FsckError::RefCountMismatch(
                        *object_id,
                        observed_refs,
                        *stored_refs,
                    ))?;
                }
                if observed_allocated_size != stored_allocated_size {
                    fsck.error(FsckError::AllocatedSizeMismatch(
                        store_id,
                        *object_id,
                        *observed_allocated_size,
                        *stored_allocated_size,
                    ))?;
                }
                if attributes
                    .iter()
                    .find(|(attr_id, _)| *attr_id == DEFAULT_DATA_ATTRIBUTE_ID)
                    .is_none()
                {
                    fsck.error(FsckError::MissingDataAttribute(store_id, *object_id))?;
                }
                if parents.is_empty() {
                    fsck.warning(FsckWarning::OrphanedObject(store_id, *object_id))?;
                }
                if parents.contains(&INVALID_OBJECT_ID) && parents.len() > 1 {
                    let parents = parents
                        .iter()
                        .filter(|oid| **oid != INVALID_OBJECT_ID)
                        .cloned()
                        .collect::<Vec<u64>>();
                    fsck.error(FsckError::ZombieFile(store_id, *object_id, parents))?;
                }
            }
            ScannedObject::Directory(ScannedDir {
                stored_sub_dirs,
                observed_sub_dirs,
                parent,
                visited,
                ..
            }) => {
                directories += 1;
                if *observed_sub_dirs != *stored_sub_dirs {
                    fsck.error(FsckError::SubDirCountMismatch(
                        store_id,
                        *object_id,
                        *observed_sub_dirs,
                        *stored_sub_dirs,
                    ))?;
                }
                if let Some(mut oid) = parent {
                    // Check this directory is attached to a root object.
                    // SAFETY: This is safe because here and below are the only places that we
                    // manipulate `visited`.
                    if !std::mem::replace(unsafe { &mut *visited.get() }, true) {
                        stack.push(*object_id);
                        loop {
                            if let Some(ScannedObject::Directory(ScannedDir {
                                parent: Some(parent),
                                visited,
                                ..
                            })) = scanned.objects.get(&oid)
                            {
                                stack.push(oid);
                                oid = *parent;
                                // SAFETY: See above.
                                if std::mem::replace(unsafe { &mut *visited.get() }, true) {
                                    break;
                                }
                            } else {
                                // This indicates an error (e.g. missing parent), but they should be
                                // reported elsewhere.
                                break;
                            }
                        }
                        // Check that the object we got to isn't one in our stack which would
                        // indicate a cycle.
                        for s in stack.drain(..) {
                            if s == oid {
                                fsck.error(FsckError::LinkCycle(store_id, oid))?;
                                break;
                            }
                        }
                    }
                } else {
                    fsck.warning(FsckWarning::OrphanedObject(store_id, *object_id))?;
                }
            }
            ScannedObject::Graveyard => other += 1,
            ScannedObject::Symlink => symlinks += 1,
            ScannedObject::Tombstone => {
                tombstones += 1;
                num_objects -= 1;
            }
        }
    }
    if num_objects != store.object_count() {
        fsck.error(FsckError::ObjectCountMismatch(store_id, num_objects, store.object_count()))?;
    }
    fsck.verbose(format!(
        "Store {store_id} has {files} files, {directories} dirs, {symlinks} symlinks, \
         {tombstones} tombstones, {other} other objects",
    ));

    Ok(())
}
