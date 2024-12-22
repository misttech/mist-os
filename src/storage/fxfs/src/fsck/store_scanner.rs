// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::fsck::errors::{FsckError, FsckFatal, FsckWarning};
use crate::fsck::{FragmentationStats, Fsck, FsckResult};
use crate::lsm_tree::types::{Item, ItemRef, LayerIterator};
use crate::lsm_tree::Query;
use crate::object_handle::INVALID_OBJECT_ID;
use crate::object_store::allocator::{self, AllocatorKey, AllocatorValue};
use crate::object_store::graveyard::Graveyard;
use crate::object_store::{
    AttributeKey, ChildValue, EncryptionKeys, ExtendedAttributeValue, ExtentKey, ExtentMode,
    ExtentValue, ObjectAttributes, ObjectDescriptor, ObjectKey, ObjectKeyData, ObjectKind,
    ObjectStore, ObjectValue, ProjectProperty, RootDigest, DEFAULT_DATA_ATTRIBUTE_ID,
    EXTENDED_ATTRIBUTE_RANGE_END, EXTENDED_ATTRIBUTE_RANGE_START, FSVERITY_MERKLE_ATTRIBUTE_ID,
};
use crate::range::RangeExt;
use crate::round::round_up;
use anyhow::{bail, Error};
use rustc_hash::FxHashSet as HashSet;
use std::cell::UnsafeCell;
use std::collections::btree_map::BTreeMap;
use std::ops::Range;

// Information about a specific attribute.
#[derive(Debug)]
struct ScannedAttribute {
    // ID of the attribute. Most commonly zero, which is the attribute value regular data is stored
    // in, but others are used for extended attributes and merkle trees.
    attribute_id: u64,
    // The logical size of this attribute according to its metadata.
    size: u64,
    // The attribute metadata claims to have at least one extents with an Overwrite or
    // OverwritePartial ExtentMode. None for attributes that don't care about overwrite extents,
    // like VerifiedAttribute.
    has_overwrite_extents_flag: Option<bool>,
    // An overwrite extent was found for this attribute.
    observed_overwrite_extents: bool,
}

// Information for scanned objects about their allocated attributes.
#[derive(Debug)]
struct ScannedAttributes {
    attributes: Vec<ScannedAttribute>,
    // A list of attributes that have been tombstoned.
    tombstoned_attributes: Vec<u64>,
    // The object's allocated size, according to its metadata.
    stored_allocated_size: u64,
    // The allocated size of the object (computed by summing up the extents for the file).
    observed_allocated_size: u64,
    // The object is in the graveyard which means extents beyond the end of the file are allowed.
    in_graveyard: bool,
    // Any extended attributes which point at an fxfs attribute for this object.
    extended_attributes: Vec<u64>,
}

#[derive(Debug)]
struct ScannedFile {
    // A list of parent object IDs for the file.  INVALID_OBJECT_ID indicates a reference from
    // outside the object store (either the graveyard, or because the object is a root object of the
    // store and probably has a reference to it in e.g. the StoreInfo or superblock).
    parents: Vec<u64>,
    // The number of references to the file, according to its metadata.
    stored_refs: u64,
    // Attributes for this file.
    attributes: ScannedAttributes,
    // If fsverity-enabled, contains the hash size.
    is_verified: Option<usize>,
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
    // If set, stores the wrapping_key_id that the directory was encrypted with.
    wrapping_key_id: Option<u128>,
    // Attributes for this directory.
    attributes: ScannedAttributes,
    // True if directory uses casefold
    casefold: bool,
}

unsafe impl Sync for ScannedDir {}

#[derive(Debug)]
struct ScannedSymlink {
    // A list of parent object IDs for the symlink.
    parents: Vec<u64>,
    // The number of references to the file, according to its metadata.
    stored_refs: u64,
    // Attributes for this symlink
    attributes: ScannedAttributes,
}

#[derive(Debug)]
enum ScannedObject {
    Directory(ScannedDir),
    File(ScannedFile),
    Graveyard,
    Symlink(ScannedSymlink),
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
    current_object: Option<CurrentObject>,
    root_dir_id: u64,
    stored_project_usages: BTreeMap<u64, (i64, i64)>,
    total_project_usages: BTreeMap<u64, (i64, i64)>,
    /// Tracks project ids used and an example node to examine if it should not have been included.
    used_project_ids: BTreeMap<u64, u64>,
}

struct CurrentObject {
    object_id: u64,
    key_ids: HashSet<u64>,
    lazy_keys: bool,
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
            current_object: None,
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
                        self.current_object = Some(CurrentObject {
                            object_id: key.object_id,
                            key_ids: HashSet::default(),
                            lazy_keys: false,
                        });
                        let parents = if self.root_objects.contains(&key.object_id) {
                            vec![INVALID_OBJECT_ID]
                        } else {
                            vec![]
                        };
                        self.objects.insert(
                            key.object_id,
                            ScannedObject::File(ScannedFile {
                                parents,
                                stored_refs: *refs,
                                attributes: ScannedAttributes {
                                    attributes: Vec::new(),
                                    tombstoned_attributes: Vec::new(),
                                    stored_allocated_size: *allocated_size,
                                    observed_allocated_size: 0,
                                    in_graveyard: false,
                                    extended_attributes: Vec::new(),
                                },
                                is_verified: None,
                            }),
                        );
                    }
                    ObjectValue::Object {
                        // TODO: https://fxbug.dev/356897866: Add validation for fscrypt.
                        kind: ObjectKind::Directory { sub_dirs, casefold, wrapping_key_id },
                        attributes: ObjectAttributes { project_id, allocated_size, .. },
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
                        self.current_object = Some(CurrentObject {
                            object_id: key.object_id,
                            key_ids: HashSet::default(),
                            lazy_keys: true,
                        });
                        // We've verified no duplicate keys, and Object records come first,
                        // so this should always be the first time we encounter this object.
                        self.objects.insert(
                            key.object_id,
                            ScannedObject::Directory(ScannedDir {
                                stored_sub_dirs: *sub_dirs,
                                observed_sub_dirs: 0,
                                parent,
                                visited: UnsafeCell::new(false),
                                wrapping_key_id: *wrapping_key_id,
                                attributes: ScannedAttributes {
                                    attributes: Vec::new(),
                                    tombstoned_attributes: Vec::new(),
                                    stored_allocated_size: *allocated_size,
                                    observed_allocated_size: 0,
                                    in_graveyard: false,
                                    extended_attributes: Vec::new(),
                                },
                                casefold: *casefold,
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
                        kind: ObjectKind::Symlink { refs, .. },
                        attributes: ObjectAttributes { project_id, allocated_size, .. },
                    } => {
                        if *project_id > 0 {
                            self.used_project_ids.insert(*project_id, key.object_id);
                            let entry = self.total_project_usages.entry(*project_id).or_default();
                            // Increment only nodes.
                            entry.1 += 1;
                        }
                        self.current_object = Some(CurrentObject {
                            object_id: key.object_id,
                            key_ids: HashSet::default(),
                            lazy_keys: true,
                        });
                        self.objects.insert(
                            key.object_id,
                            ScannedObject::Symlink(ScannedSymlink {
                                parents: vec![],
                                stored_refs: *refs,
                                attributes: ScannedAttributes {
                                    attributes: Vec::new(),
                                    tombstoned_attributes: Vec::new(),
                                    stored_allocated_size: *allocated_size,
                                    observed_allocated_size: 0,
                                    in_graveyard: false,
                                    extended_attributes: Vec::new(),
                                },
                            }),
                        );
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
                        EncryptionKeys::AES256XTS(keys) => {
                            let keys = &**keys;
                            if let Some(current_file) = &mut self.current_object {
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
                    ObjectValue::Attribute { size, has_overwrite_extents } => {
                        match self.objects.get_mut(&key.object_id) {
                            Some(
                                ScannedObject::File(ScannedFile { attributes, .. })
                                | ScannedObject::Directory(ScannedDir { attributes, .. })
                                | ScannedObject::Symlink(ScannedSymlink { attributes, .. }),
                            ) => {
                                attributes.attributes.push(ScannedAttribute {
                                    attribute_id,
                                    size: *size,
                                    has_overwrite_extents_flag: Some(*has_overwrite_extents),
                                    observed_overwrite_extents: false,
                                });
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
                    ObjectValue::VerifiedAttribute { size, fsverity_metadata } => {
                        match self.objects.get_mut(&key.object_id) {
                            Some(ScannedObject::File(ScannedFile {
                                attributes,
                                is_verified,
                                ..
                            })) => {
                                attributes.attributes.push(ScannedAttribute {
                                    attribute_id,
                                    size: *size,
                                    has_overwrite_extents_flag: None,
                                    observed_overwrite_extents: false,
                                });
                                let hash_size = match &fsverity_metadata.root_digest {
                                    RootDigest::Sha256(root_hash) => root_hash.len(),
                                    RootDigest::Sha512(root_hash) => root_hash.len(),
                                };
                                *is_verified = Some(hash_size);
                            }
                            Some(ScannedObject::Directory(..) | ScannedObject::Symlink(..)) => {
                                self.fsck.error(FsckError::NonFileMarkedAsVerified(
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
                                self.fsck.warning(FsckWarning::OrphanedAttribute(
                                    self.store_id,
                                    key.object_id,
                                    attribute_id,
                                ))?;
                            }
                        }
                    }
                    // Deleted attribute.
                    ObjectValue::None => (),
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
                        if let Some(current_file) = &self.current_object {
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
            // TODO(b/365631616): Check that the child type matches the directory metadata.
            ObjectKeyData::Child { .. }
            | ObjectKeyData::CasefoldChild { .. }
            | ObjectKeyData::EncryptedChild { .. } => match value {
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
                ObjectValue::ExtendedAttribute(ExtendedAttributeValue::Inline(_)) => {
                    if self.objects.get(&key.object_id).is_none() {
                        self.fsck.warning(FsckWarning::OrphanedExtendedAttributeRecord(
                            self.store_id,
                            key.object_id,
                        ))?;
                    }
                }
                ObjectValue::ExtendedAttribute(ExtendedAttributeValue::AttributeId(id)) => {
                    match self.objects.get_mut(&key.object_id) {
                        Some(
                            ScannedObject::File(ScannedFile { attributes, .. })
                            | ScannedObject::Directory(ScannedDir { attributes, .. })
                            | ScannedObject::Symlink(ScannedSymlink { attributes, .. }),
                        ) => {
                            attributes.extended_attributes.push(*id);
                        }
                        Some(ScannedObject::Graveyard) => { /* NOP */ }
                        Some(ScannedObject::Tombstone) => {
                            self.fsck.error(FsckError::TombstonedObjectHasRecords(
                                self.store_id,
                                key.object_id,
                            ))?;
                        }
                        None => {
                            self.fsck.warning(FsckWarning::OrphanedExtendedAttributeRecord(
                                self.store_id,
                                key.object_id,
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
            },
            ObjectKeyData::GraveyardEntry { .. } => {}
            ObjectKeyData::GraveyardAttributeEntry { .. } => {}
        }
        Ok(())
    }

    /// Performs some checks on the child link and records information such as the sub-directory
    /// count that get verified later.
    fn process_child(
        &mut self,
        parent_id: u64,
        child_id: u64,
        object_descriptor: &ObjectDescriptor,
        object_key_data: &ObjectKeyData,
    ) -> Result<(), Error> {
        let mut child_wrapping_key_id = None;
        if let Some(ScannedObject::Directory(dir)) = self.objects.get(&parent_id) {
            match object_key_data {
                ObjectKeyData::Child { .. } => {
                    if dir.casefold {
                        self.fsck.error(FsckError::CasefoldInconsistency(
                            self.store_id,
                            parent_id,
                            child_id,
                        ))?;
                    }
                }
                ObjectKeyData::CasefoldChild { .. } => {
                    if !dir.casefold {
                        self.fsck.error(FsckError::CasefoldInconsistency(
                            self.store_id,
                            parent_id,
                            child_id,
                        ))?;
                    }
                }
                ObjectKeyData::EncryptedChild { .. } => {
                    if dir.wrapping_key_id.is_none() {
                        self.fsck.error(FsckError::UnencryptedDirectoryHasEncryptedChild(
                            self.store_id,
                            parent_id,
                            child_id,
                        ))?;
                    }
                }
                _ => {
                    bail!(
                        "Unexpected object_key_data type in process_child: {:?}",
                        object_key_data
                    );
                }
            };
        }
        match (self.objects.get_mut(&child_id), object_descriptor) {
            (
                Some(ScannedObject::File(ScannedFile { parents, .. })),
                ObjectDescriptor::File | ObjectDescriptor::Volume,
            ) => {
                parents.push(parent_id);
            }
            (
                Some(ScannedObject::Directory(ScannedDir { parent, wrapping_key_id, .. })),
                ObjectDescriptor::Directory,
            ) => {
                if matches!(object_key_data, ObjectKeyData::EncryptedChild { .. }) {
                    if let Some(id) = wrapping_key_id {
                        child_wrapping_key_id = Some(*id);
                    } else {
                        self.fsck.error(FsckError::EncryptedChildDirectoryNoWrappingKey(
                            self.store_id,
                            child_id,
                        ))?;
                    }
                }
                if parent.is_some() {
                    // TODO(https://fxbug.dev/42168496): Accumulating and reporting all parents
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
                    ScannedObject::Symlink(ScannedSymlink { parents, .. }) => {
                        parents.push(parent_id);
                        ObjectDescriptor::Symlink
                    }
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
            Some(
                ScannedObject::File(..) | ScannedObject::Graveyard | ScannedObject::Symlink(_),
            ) => {
                self.fsck.error(FsckError::ObjectHasChildren(self.store_id, parent_id))?;
            }
            Some(ScannedObject::Directory(ScannedDir {
                observed_sub_dirs,
                wrapping_key_id,
                ..
            })) => {
                if let Some(parent_wrapping_key_id) = *wrapping_key_id {
                    if !matches!(object_key_data, ObjectKeyData::EncryptedChild { .. }) {
                        self.fsck.error(FsckError::EncryptedDirectoryHasUnencryptedChild(
                            self.store_id,
                            parent_id,
                            child_id,
                        ))?;
                    } else {
                        if let Some(child_wrapping_key_id) = child_wrapping_key_id {
                            if child_wrapping_key_id != parent_wrapping_key_id {
                                self.fsck.error(
                                    FsckError::ChildEncryptedWithDifferentWrappingKeyThanParent(
                                        self.store_id,
                                        parent_id,
                                        child_id,
                                        parent_wrapping_key_id,
                                        child_wrapping_key_id,
                                    ),
                                )?;
                            }
                        }
                    }
                }
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
        device_offset: u64,
        bs: u64,
        is_overwrite_extent: bool,
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
            Some(
                ScannedObject::File(ScannedFile { attributes, .. })
                | ScannedObject::Directory(ScannedDir { attributes, .. })
                | ScannedObject::Symlink(ScannedSymlink { attributes, .. }),
            ) => {
                let ScannedAttributes {
                    attributes,
                    tombstoned_attributes,
                    observed_allocated_size: allocated_size,
                    in_graveyard,
                    ..
                } = attributes;
                match attributes.iter_mut().find(|attribute| attribute.attribute_id == attribute_id)
                {
                    Some(attribute) => {
                        if !*in_graveyard
                            && !tombstoned_attributes.contains(&attribute_id)
                            && range.end > round_up(attribute.size, bs).unwrap()
                        {
                            self.fsck.error(FsckError::ExtentExceedsLength(
                                self.store_id,
                                object_id,
                                attribute_id,
                                attribute.size,
                                range.into(),
                            ))?;
                        }
                        attribute.observed_overwrite_extents =
                            attribute.observed_overwrite_extents || is_overwrite_extent;
                    }
                    None => {
                        self.fsck.warning(FsckWarning::ExtentForMissingAttribute(
                            self.store_id,
                            object_id,
                            attribute_id,
                        ))?;
                    }
                }
                *allocated_size += len;
            }
            Some(ScannedObject::Graveyard | ScannedObject::Tombstone) => { /* NOP */ }
            None => {
                self.fsck
                    .warning(FsckWarning::ExtentForNonexistentObject(self.store_id, object_id))?;
            }
        }
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
        let lower_bound: AllocatorKey = item.key.lower_bound_for_merge_into();
        self.fsck.allocations.merge_into(item, &lower_bound, allocator::merge::merge);
        Ok(())
    }

    // A graveyard entry can either be for tombstoning a file/attribute, or for trimming a file.
    fn handle_graveyard_entry(
        &mut self,
        object_id: u64,
        attribute_id: Option<u64>,
        tombstone: bool,
    ) -> Result<(), Error> {
        match self.objects.get_mut(&object_id) {
            Some(ScannedObject::File(ScannedFile {
                parents,
                attributes: ScannedAttributes { in_graveyard, tombstoned_attributes, .. },
                ..
            })) => {
                if attribute_id.is_none() {
                    *in_graveyard = true;
                }
                if tombstone {
                    if let Some(attribute_id) = attribute_id {
                        tombstoned_attributes.push(attribute_id);
                    } else {
                        parents.push(INVALID_OBJECT_ID)
                    }
                }
            }
            Some(
                ScannedObject::Directory(ScannedDir { attributes, .. })
                | ScannedObject::Symlink(ScannedSymlink { attributes, .. }),
            ) => {
                if tombstone {
                    if let Some(attribute_id) = attribute_id {
                        attributes.tombstoned_attributes.push(attribute_id);
                    } else {
                        attributes.in_graveyard = true;
                    }
                } else {
                    self.fsck.error(FsckError::UnexpectedObjectInGraveyard(object_id))?;
                }
            }
            Some(ScannedObject::Graveyard | ScannedObject::Tombstone) => {
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
        if let Some(current_file) = self.current_object.take() {
            if self.is_encrypted {
                let mut key_ids = vec![];
                for id in current_file.key_ids.iter() {
                    key_ids.push(*id);
                }

                // If the store is unencrypted, then the file might or might not have encryption
                // keys (e.g. the root store has encrypted layer files). Also, if the object has
                // lazily generated keys, like directories and symlinks, it will only have keys if
                // an attribute has been written, in which case we check the existence of the key
                // then.
                if key_ids.is_empty() && !current_file.lazy_keys {
                    self.fsck.error(FsckError::MissingEncryptionKeys(
                        self.store_id,
                        current_file.object_id,
                    ))?;
                }

                match self.objects.get_mut(&current_file.object_id) {
                    Some(ScannedObject::Directory(ScannedDir {
                        wrapping_key_id,
                        attributes,
                        ..
                    })) => {
                        if !attributes.extended_attributes.is_empty() {
                            if !key_ids.contains(&0) {
                                self.fsck.error(FsckError::MissingKey(
                                    self.store_id,
                                    current_file.object_id,
                                    0,
                                ))?;
                            }
                        }
                        if wrapping_key_id.is_some() {
                            if !key_ids.contains(&1) {
                                self.fsck.error(FsckError::MissingKey(
                                    self.store_id,
                                    current_file.object_id,
                                    1,
                                ))?;
                            }
                        }
                    }
                    Some(_) => {}
                    None => self.fsck.error(FsckError::MissingObjectInfo(
                        self.store_id,
                        current_file.object_id,
                    ))?,
                }
            }
        }
        Ok(())
    }
}

// Scans extents and directory child entries in the store, emitting synthesized allocations into
// |fsck.allocations|, updating the sizes for files in |scanned| and performing checks on directory
// children.
// TODO(https://fxbug.dev/42177485): Roll the extent scanning back into main function.
async fn scan_extents_and_directory_children<'a>(
    store: &ObjectStore,
    scanned: &mut ScannedStore<'a>,
    result: &mut FsckResult,
) -> Result<(), Error> {
    let bs = store.block_size();
    let layer_set = store.tree().layer_set();
    let mut merger = layer_set.merger();
    let mut iter = merger.query(Query::FullScan).await?;
    let mut allocated_bytes = 0;
    let mut extent_count = 0;
    let mut previous_object_id = INVALID_OBJECT_ID;
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
                // Ignore deleted extents.
                if let ExtentValue::Some { device_offset, mode, .. } = extent {
                    let size = range.length().unwrap_or(0);
                    allocated_bytes += size;

                    if previous_object_id != *object_id {
                        if previous_object_id != INVALID_OBJECT_ID {
                            result.fragmentation.extent_count
                                [FragmentationStats::get_histogram_bucket_for_count(
                                    extent_count,
                                )] += 1;
                        }
                        extent_count = 0;
                        previous_object_id = *object_id;
                    }
                    result.fragmentation.extent_size
                        [FragmentationStats::get_histogram_bucket_for_size(size)] += 1;
                    extent_count += 1;

                    let is_overwrite_extent =
                        matches!(mode, ExtentMode::Overwrite | ExtentMode::OverwritePartial(_));

                    scanned
                        .process_extent(
                            *object_id,
                            *attribute_id,
                            range,
                            *device_offset,
                            bs,
                            is_overwrite_extent,
                        )
                        .await?;
                };
            }
            ItemRef {
                key: ObjectKey { object_id, data: object_key_data @ ObjectKeyData::Child { .. } },
                value: ObjectValue::Child(ChildValue { object_id: child_id, object_descriptor }),
                ..
            }
            | ItemRef {
                key:
                    ObjectKey {
                        object_id,
                        data: object_key_data @ ObjectKeyData::EncryptedChild { .. },
                    },
                value: ObjectValue::Child(ChildValue { object_id: child_id, object_descriptor }),
                ..
            }
            | ItemRef {
                key:
                    ObjectKey { object_id, data: object_key_data @ ObjectKeyData::CasefoldChild { .. } },
                value: ObjectValue::Child(ChildValue { object_id: child_id, object_descriptor }),
                ..
            } => {
                scanned.process_child(*object_id, *child_id, object_descriptor, object_key_data)?
            }
            _ => {}
        }
        iter.advance().await?;
    }
    if extent_count != 0 && previous_object_id != INVALID_OBJECT_ID {
        result.fragmentation.extent_count
            [FragmentationStats::get_histogram_bucket_for_count(extent_count)] += 1;
    }
    scanned.fsck.verbose(format!(
        "Store {} has {} bytes allocated",
        store.store_object_id(),
        allocated_bytes
    ));
    Ok(())
}

fn validate_attributes(
    fsck: &Fsck<'_>,
    store_id: u64,
    object_id: u64,
    attributes: &ScannedAttributes,
    is_file: bool,
    is_verified: Option<usize>,
    block_size: u64,
) -> Result<(), Error> {
    let ScannedAttributes {
        attributes,
        tombstoned_attributes,
        observed_allocated_size,
        stored_allocated_size,
        extended_attributes,
        ..
    } = attributes;
    if observed_allocated_size != stored_allocated_size {
        fsck.error(FsckError::AllocatedSizeMismatch(
            store_id,
            object_id,
            *observed_allocated_size,
            *stored_allocated_size,
        ))?;
    }

    if is_file {
        let data_attribute =
            attributes.iter().find(|attribute| attribute.attribute_id == DEFAULT_DATA_ATTRIBUTE_ID);
        match data_attribute {
            None => fsck.error(FsckError::MissingDataAttribute(store_id, object_id))?,
            Some(data_attribute) => {
                let merkle_attribute = attributes
                    .iter()
                    .find(|attribute| attribute.attribute_id == FSVERITY_MERKLE_ATTRIBUTE_ID);

                // Note a merkle attribute can exist for a non-verified file in the case that the
                // power cut while we were writing the merkle attribute across multiple txns. In
                // this case, the merkle attribute will be cleaned up on reboot.
                if is_verified.is_some() && merkle_attribute.is_none() {
                    fsck.error(FsckError::VerifiedFileDoesNotHaveAMerkleAttribute(
                        store_id, object_id,
                    ))?;
                }

                if let (Some(merkle_attribute), Some(hash_size)) = (merkle_attribute, is_verified) {
                    // If the file is empty, the merkle tree should just be a single hash.
                    let expected_size = if data_attribute.size == 0 {
                        hash_size as u64
                    // Else, use ceiling integer division in case data_size is not a multiple of
                    // block size.
                    } else {
                        ((data_attribute.size + (block_size - 1)) / block_size) * hash_size as u64
                    };
                    if merkle_attribute.size != expected_size {
                        fsck.error(FsckError::IncorrectMerkleTreeSize(
                            store_id,
                            object_id,
                            expected_size,
                            merkle_attribute.size,
                        ))?;
                    }
                }
            }
        }
    }

    // Attributes queued for tombstoning must exist.
    for attr in tombstoned_attributes {
        if attributes.iter().find(|attribute| attribute.attribute_id == *attr).is_none() {
            fsck.error(FsckError::TombstonedAttributeDoesNotExist(store_id, object_id, *attr))?
        }
    }

    for expected_attribute_id in extended_attributes {
        if attributes
            .iter()
            .find(|attribute| attribute.attribute_id == *expected_attribute_id)
            .is_none()
        {
            fsck.error(FsckError::MissingAttributeForExtendedAttribute(
                store_id,
                object_id,
                *expected_attribute_id,
            ))?;
        }
    }

    for attribute in attributes {
        if attribute.attribute_id >= EXTENDED_ATTRIBUTE_RANGE_START
            && attribute.attribute_id < EXTENDED_ATTRIBUTE_RANGE_END
        {
            // For all the attributes in the extended attribute range, make sure there is an
            // extended attribute record for them.
            if extended_attributes
                .iter()
                .find(|xattr_id| attribute.attribute_id == **xattr_id)
                .is_none()
            {
                fsck.warning(FsckWarning::OrphanedExtendedAttribute(
                    store_id,
                    object_id,
                    attribute.attribute_id,
                ))?;
            }
        }

        if attribute
            .has_overwrite_extents_flag
            .is_some_and(|has_flag| has_flag && !attribute.observed_overwrite_extents)
        {
            fsck.error(FsckError::MissingOverwriteExtents(
                store_id,
                object_id,
                attribute.attribute_id,
            ))?;
        }
        if attribute
            .has_overwrite_extents_flag
            .is_some_and(|has_flag| !has_flag && attribute.observed_overwrite_extents)
        {
            fsck.error(FsckError::OverwriteExtentFlagUnset(
                store_id,
                object_id,
                attribute.attribute_id,
            ))?;
        }
    }

    Ok(())
}

/// Scans an object store, accumulating all of its allocations into |fsck.allocations| and
/// validating various object properties.
pub(super) async fn scan_store(
    fsck: &Fsck<'_>,
    store: &ObjectStore,
    root_objects: impl AsRef<[u64]>,
    result: &mut FsckResult,
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
    let mut iter = merger.query(Query::FullScan).await?;
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
        if let Some(current_file) = &scanned.current_object {
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
    while let Some(info) = iter.get() {
        match info.value() {
            ObjectValue::Some => {
                scanned.handle_graveyard_entry(info.object_id(), info.attribute_id(), true)?
            }
            ObjectValue::Trim => {
                if let Some(attribute_id) = info.attribute_id() {
                    fsck.error(FsckError::TrimValueForGraveyardAttributeEntry(
                        store_id,
                        info.object_id(),
                        attribute_id,
                    ))?
                } else {
                    scanned.handle_graveyard_entry(info.object_id(), None, false)?
                }
            }
            _ => fsck.error(FsckError::BadGraveyardValue(store_id, info.object_id()))?,
        }
        fsck.assert(iter.advance().await, FsckFatal::MalformedGraveyard)?;
    }

    scan_extents_and_directory_children(store, &mut scanned, result).await?;

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
                parents,
                stored_refs,
                attributes,
                is_verified,
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
                validate_attributes(
                    fsck,
                    store_id,
                    *object_id,
                    attributes,
                    true,
                    *is_verified,
                    store.block_size(),
                )?;
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
                attributes,
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
                validate_attributes(
                    fsck,
                    store_id,
                    *object_id,
                    attributes,
                    false,
                    None,
                    store.block_size(),
                )?;
                if let Some(mut oid) = parent {
                    if attributes.in_graveyard {
                        fsck.error(FsckError::ZombieDir(store_id, *object_id, oid))?;
                    }
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
                } else if !attributes.in_graveyard {
                    fsck.warning(FsckWarning::OrphanedObject(store_id, *object_id))?;
                }
            }
            ScannedObject::Graveyard => other += 1,
            ScannedObject::Symlink(ScannedSymlink { parents, stored_refs, attributes }) => {
                symlinks += 1;
                let observed_refs = parents.len().try_into().unwrap();
                // observed_refs == 0 is handled separately to distinguish orphaned objects
                if observed_refs != *stored_refs && observed_refs > 0 {
                    fsck.error(FsckError::RefCountMismatch(
                        *object_id,
                        observed_refs,
                        *stored_refs,
                    ))?;
                }
                validate_attributes(
                    fsck,
                    store_id,
                    *object_id,
                    attributes,
                    false,
                    None,
                    store.block_size(),
                )?;
                if attributes.in_graveyard {
                    if !parents.is_empty() {
                        fsck.error(FsckError::ZombieSymlink(
                            store_id,
                            *object_id,
                            parents.clone(),
                        ))?;
                    }
                } else if parents.is_empty() {
                    fsck.warning(FsckWarning::OrphanedObject(store_id, *object_id))?;
                }
            }
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
