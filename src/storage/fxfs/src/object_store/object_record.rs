// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// TODO(https://fxbug.dev/42178223): need validation after deserialization.
use crate::checksum::Checksums;
use crate::lsm_tree::types::{
    FuzzyHash, Item, ItemRef, LayerKey, MergeType, OrdLowerBound, OrdUpperBound, RangeKey,
    SortByU64, Value,
};
use crate::object_store::extent_record::{
    ExtentKey, ExtentKeyPartitionIterator, ExtentKeyV32, ExtentValue, ExtentValueV32,
    ExtentValueV37, ExtentValueV38,
};
use crate::serialized_types::{migrate_nodefault, migrate_to_version, Migrate, Versioned};
use fprint::TypeFingerprint;
use fxfs_crypto::{WrappedKeysV32, WrappedKeysV40};
use fxfs_unicode::CasefoldString;
use rustc_hash::FxHasher;
use serde::{Deserialize, Serialize};
use std::default::Default;
use std::hash::{Hash, Hasher as _};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// ObjectDescriptor is the set of possible records in the object store.
pub type ObjectDescriptor = ObjectDescriptorV32;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, TypeFingerprint)]
#[cfg_attr(fuzz, derive(arbitrary::Arbitrary))]
pub enum ObjectDescriptorV32 {
    /// A file (in the generic sense; i.e. an object with some attributes).
    File,
    /// A directory (in the generic sense; i.e. an object with children).
    Directory,
    /// A volume, which is the root of a distinct object store containing Files and Directories.
    Volume,
    /// A symbolic link.
    Symlink,
}

/// For specifying what property of the project is being addressed.
pub type ProjectProperty = ProjectPropertyV32;

#[derive(
    Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize, TypeFingerprint,
)]
#[cfg_attr(fuzz, derive(arbitrary::Arbitrary))]
pub enum ProjectPropertyV32 {
    /// The configured limit for the project.
    Limit,
    /// The currently tracked usage for the project.
    Usage,
}

pub type ObjectKeyData = ObjectKeyDataV43;

#[derive(
    Clone, Debug, Eq, Hash, PartialEq, PartialOrd, Ord, Serialize, Deserialize, TypeFingerprint,
)]
#[cfg_attr(fuzz, derive(arbitrary::Arbitrary))]
pub enum ObjectKeyDataV43 {
    /// A generic, untyped object.  This must come first and sort before all other keys for a given
    /// object because it's also used as a tombstone and it needs to merge with all following keys.
    Object,
    /// Encryption keys for an object.
    Keys,
    /// An attribute associated with an object.  It has a 64-bit ID.
    Attribute(u64, AttributeKeyV32),
    /// A child of a directory.
    Child { name: String },
    /// A graveyard entry for an entire object.
    GraveyardEntry { object_id: u64 },
    /// Project ID info. This should only be attached to the volume's root node. Used to address the
    /// configured limit and the usage tracking which are ordered after the `project_id` to provide
    /// locality of the two related values.
    Project { project_id: u64, property: ProjectPropertyV32 },
    /// An extended attribute associated with an object. It stores the name used for the extended
    /// attribute, which has a maximum size of 255 bytes enforced by fuchsia.io.
    ExtendedAttribute { name: Vec<u8> },
    /// A graveyard entry for an attribute.
    GraveyardAttributeEntry { object_id: u64, attribute_id: u64 },
    /// A child of an encrypted directory.
    /// We store the filename in its encrypted form.
    /// casefold_hash is the hash of the casefolded human-readable name if a directory is
    /// also casefolded, otherwise 0.
    /// Legacy records may have a casefold_hash of 0 indicating "unknown".
    EncryptedChild { casefold_hash: u32, name: Vec<u8> },
    /// A child of a directory that uses the casefold feature.
    /// (i.e. case insensitive, case preserving names)
    CasefoldChild { name: CasefoldString },
}

impl From<ObjectKeyDataV40> for ObjectKeyDataV43 {
    fn from(item: ObjectKeyDataV40) -> Self {
        match item {
            ObjectKeyDataV40::Object => Self::Object,
            ObjectKeyDataV40::Keys => Self::Keys,
            ObjectKeyDataV40::Attribute(a, b) => Self::Attribute(a, b),
            ObjectKeyDataV40::Child { name } => Self::Child { name },
            ObjectKeyDataV40::GraveyardEntry { object_id } => Self::GraveyardEntry { object_id },
            ObjectKeyDataV40::Project { project_id, property } => {
                Self::Project { project_id, property }
            }
            ObjectKeyDataV40::ExtendedAttribute { name } => Self::ExtendedAttribute { name },
            ObjectKeyDataV40::GraveyardAttributeEntry { object_id, attribute_id } => {
                Self::GraveyardAttributeEntry { object_id, attribute_id }
            }
            ObjectKeyDataV40::EncryptedChild { name } => {
                Self::EncryptedChild { casefold_hash: 0, name }
            }
            ObjectKeyDataV40::CasefoldChild { name } => Self::CasefoldChild { name },
        }
    }
}

#[derive(Serialize, Deserialize, TypeFingerprint)]
#[cfg_attr(fuzz, derive(arbitrary::Arbitrary))]
pub enum ObjectKeyDataV40 {
    Object,
    Keys,
    Attribute(u64, AttributeKeyV32),
    Child { name: String },
    GraveyardEntry { object_id: u64 },
    Project { project_id: u64, property: ProjectPropertyV32 },
    ExtendedAttribute { name: Vec<u8> },
    GraveyardAttributeEntry { object_id: u64, attribute_id: u64 },
    EncryptedChild { name: Vec<u8> },
    CasefoldChild { name: CasefoldString },
}

#[derive(Migrate, Serialize, Deserialize, TypeFingerprint)]
#[migrate_to_version(ObjectKeyDataV40)]
pub enum ObjectKeyDataV32 {
    Object,
    Keys,
    Attribute(u64, AttributeKeyV32),
    Child { name: String },
    GraveyardEntry { object_id: u64 },
    Project { project_id: u64, property: ProjectPropertyV32 },
    ExtendedAttribute { name: Vec<u8> },
    GraveyardAttributeEntry { object_id: u64, attribute_id: u64 },
}

pub type AttributeKey = AttributeKeyV32;

#[derive(
    Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize, TypeFingerprint,
)]
#[cfg_attr(fuzz, derive(arbitrary::Arbitrary))]
pub enum AttributeKeyV32 {
    // Order here is important: code expects Attribute to precede Extent.
    Attribute,
    Extent(ExtentKeyV32),
}

/// ObjectKey is a key in the object store.
pub type ObjectKey = ObjectKeyV43;

#[derive(
    Clone,
    Debug,
    Eq,
    Ord,
    Hash,
    PartialEq,
    PartialOrd,
    Serialize,
    Deserialize,
    TypeFingerprint,
    Versioned,
)]
#[cfg_attr(fuzz, derive(arbitrary::Arbitrary))]
pub struct ObjectKeyV43 {
    /// The ID of the object referred to.
    pub object_id: u64,
    /// The type and data of the key.
    pub data: ObjectKeyDataV43,
}

#[derive(Migrate, Serialize, Deserialize, TypeFingerprint, Versioned)]
#[migrate_to_version(ObjectKeyV43)]
#[migrate_nodefault]
pub struct ObjectKeyV40 {
    pub object_id: u64,
    pub data: ObjectKeyDataV40,
}
#[derive(Migrate, Serialize, Deserialize, TypeFingerprint, Versioned)]
#[migrate_to_version(ObjectKeyV40)]
#[migrate_nodefault]
pub struct ObjectKeyV32 {
    pub object_id: u64,
    pub data: ObjectKeyDataV32,
}

impl SortByU64 for ObjectKey {
    fn get_leading_u64(&self) -> u64 {
        self.object_id
    }
}

impl ObjectKey {
    /// Creates a generic ObjectKey.
    pub fn object(object_id: u64) -> Self {
        Self { object_id: object_id, data: ObjectKeyData::Object }
    }

    /// Creates an ObjectKey for encryption keys.
    pub fn keys(object_id: u64) -> Self {
        Self { object_id, data: ObjectKeyData::Keys }
    }

    /// Creates an ObjectKey for an attribute.
    pub fn attribute(object_id: u64, attribute_id: u64, key: AttributeKey) -> Self {
        Self { object_id, data: ObjectKeyData::Attribute(attribute_id, key) }
    }

    /// Creates an ObjectKey for an extent.
    pub fn extent(object_id: u64, attribute_id: u64, range: std::ops::Range<u64>) -> Self {
        Self {
            object_id,
            data: ObjectKeyData::Attribute(
                attribute_id,
                AttributeKey::Extent(ExtentKey::new(range)),
            ),
        }
    }

    /// Creates an ObjectKey from an extent.
    pub fn from_extent(object_id: u64, attribute_id: u64, extent: ExtentKey) -> Self {
        Self {
            object_id,
            data: ObjectKeyData::Attribute(attribute_id, AttributeKey::Extent(extent)),
        }
    }

    /// Creates an ObjectKey for a child.
    pub fn child(object_id: u64, name: &str, casefold: bool) -> Self {
        if casefold {
            Self { object_id, data: ObjectKeyData::CasefoldChild { name: name.into() } }
        } else {
            Self { object_id, data: ObjectKeyData::Child { name: name.into() } }
        }
    }

    /// Creates an ObjectKey for an encrypted child.
    pub fn encrypted_child(object_id: u64, name: Vec<u8>, casefold_hash: u32) -> Self {
        Self { object_id, data: ObjectKeyData::EncryptedChild { casefold_hash, name } }
    }

    /// Creates a graveyard entry for an object.
    pub fn graveyard_entry(graveyard_object_id: u64, object_id: u64) -> Self {
        Self { object_id: graveyard_object_id, data: ObjectKeyData::GraveyardEntry { object_id } }
    }

    /// Creates a graveyard entry for an attribute.
    pub fn graveyard_attribute_entry(
        graveyard_object_id: u64,
        object_id: u64,
        attribute_id: u64,
    ) -> Self {
        Self {
            object_id: graveyard_object_id,
            data: ObjectKeyData::GraveyardAttributeEntry { object_id, attribute_id },
        }
    }

    /// Creates an ObjectKey for a ProjectLimit entry.
    pub fn project_limit(object_id: u64, project_id: u64) -> Self {
        Self {
            object_id,
            data: ObjectKeyData::Project { project_id, property: ProjectProperty::Limit },
        }
    }

    /// Creates an ObjectKey for a ProjectUsage entry.
    pub fn project_usage(object_id: u64, project_id: u64) -> Self {
        Self {
            object_id,
            data: ObjectKeyData::Project { project_id, property: ProjectProperty::Usage },
        }
    }

    pub fn extended_attribute(object_id: u64, name: Vec<u8>) -> Self {
        Self { object_id, data: ObjectKeyData::ExtendedAttribute { name } }
    }

    /// Returns the merge key for this key; that is, a key which is <= this key and any
    /// other possibly overlapping key, under Ord. This would be used for the hint in |merge_into|.
    pub fn key_for_merge_into(&self) -> Self {
        if let Self {
            object_id,
            data: ObjectKeyData::Attribute(attribute_id, AttributeKey::Extent(e)),
        } = self
        {
            Self::attribute(*object_id, *attribute_id, AttributeKey::Extent(e.key_for_merge_into()))
        } else {
            self.clone()
        }
    }
}

impl OrdUpperBound for ObjectKey {
    fn cmp_upper_bound(&self, other: &ObjectKey) -> std::cmp::Ordering {
        self.object_id.cmp(&other.object_id).then_with(|| match (&self.data, &other.data) {
            (
                ObjectKeyData::Attribute(left_attr_id, AttributeKey::Extent(ref left_extent)),
                ObjectKeyData::Attribute(right_attr_id, AttributeKey::Extent(ref right_extent)),
            ) => left_attr_id.cmp(right_attr_id).then(left_extent.cmp_upper_bound(right_extent)),
            _ => self.data.cmp(&other.data),
        })
    }
}

impl OrdLowerBound for ObjectKey {
    fn cmp_lower_bound(&self, other: &ObjectKey) -> std::cmp::Ordering {
        self.object_id.cmp(&other.object_id).then_with(|| match (&self.data, &other.data) {
            (
                ObjectKeyData::Attribute(left_attr_id, AttributeKey::Extent(ref left_extent)),
                ObjectKeyData::Attribute(right_attr_id, AttributeKey::Extent(ref right_extent)),
            ) => left_attr_id.cmp(right_attr_id).then(left_extent.cmp_lower_bound(right_extent)),
            _ => self.data.cmp(&other.data),
        })
    }
}

impl LayerKey for ObjectKey {
    fn merge_type(&self) -> MergeType {
        // This listing is intentionally exhaustive to force folks to think about how certain
        // subsets of the keyspace are merged.
        match self.data {
            ObjectKeyData::Object
            | ObjectKeyData::Keys
            | ObjectKeyData::Attribute(..)
            | ObjectKeyData::Child { .. }
            | ObjectKeyData::EncryptedChild { .. }
            | ObjectKeyData::CasefoldChild { .. }
            | ObjectKeyData::GraveyardEntry { .. }
            | ObjectKeyData::GraveyardAttributeEntry { .. }
            | ObjectKeyData::Project { property: ProjectProperty::Limit, .. }
            | ObjectKeyData::ExtendedAttribute { .. } => MergeType::OptimizedMerge,
            ObjectKeyData::Project { property: ProjectProperty::Usage, .. } => MergeType::FullMerge,
        }
    }

    fn next_key(&self) -> Option<Self> {
        match self.data {
            ObjectKeyData::Attribute(_, AttributeKey::Extent(_)) => {
                let mut key = self.clone();
                if let ObjectKey {
                    data: ObjectKeyData::Attribute(_, AttributeKey::Extent(ExtentKey { range })),
                    ..
                } = &mut key
                {
                    // We want a key such that cmp_lower_bound returns Greater for any key which
                    // starts after end, and a key such that if you search for it, you'll get an
                    // extent whose end > range.end.
                    *range = range.end..range.end + 1;
                }
                Some(key)
            }
            _ => None,
        }
    }

    fn search_key(&self) -> Self {
        if let Self {
            object_id,
            data: ObjectKeyData::Attribute(attribute_id, AttributeKey::Extent(e)),
        } = self
        {
            Self::attribute(*object_id, *attribute_id, AttributeKey::Extent(e.search_key()))
        } else {
            self.clone()
        }
    }
}

impl RangeKey for ObjectKey {
    fn overlaps(&self, other: &Self) -> bool {
        if self.object_id != other.object_id {
            return false;
        }
        match (&self.data, &other.data) {
            (
                ObjectKeyData::Attribute(left_attr_id, AttributeKey::Extent(left_key)),
                ObjectKeyData::Attribute(right_attr_id, AttributeKey::Extent(right_key)),
            ) if *left_attr_id == *right_attr_id => {
                left_key.range.end > right_key.range.start
                    && left_key.range.start < right_key.range.end
            }
            (a, b) => a == b,
        }
    }
}

pub enum ObjectKeyFuzzyHashIterator {
    ExtentKey(/* object_id */ u64, /* attribute_id */ u64, ExtentKeyPartitionIterator),
    NotExtentKey(/* hash */ Option<u64>),
}

impl Iterator for ObjectKeyFuzzyHashIterator {
    type Item = u64;

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::ExtentKey(oid, attr_id, extent_keys) => extent_keys.next().map(|range| {
                let mut hasher = FxHasher::default();
                ObjectKey::extent(*oid, *attr_id, range).hash(&mut hasher);
                hasher.finish()
            }),
            Self::NotExtentKey(hash) => hash.take(),
        }
    }
}

impl FuzzyHash for ObjectKey {
    type Iter = ObjectKeyFuzzyHashIterator;

    fn fuzzy_hash(&self) -> Self::Iter {
        match &self.data {
            ObjectKeyData::Attribute(attr_id, AttributeKey::Extent(extent)) => {
                ObjectKeyFuzzyHashIterator::ExtentKey(
                    self.object_id,
                    *attr_id,
                    extent.fuzzy_hash_partition(),
                )
            }
            _ => {
                let mut hasher = FxHasher::default();
                self.hash(&mut hasher);
                ObjectKeyFuzzyHashIterator::NotExtentKey(Some(hasher.finish()))
            }
        }
    }
}

/// UNIX epoch based timestamp in the UTC timezone.
pub type Timestamp = TimestampV32;

#[derive(
    Copy,
    Clone,
    Debug,
    Default,
    Eq,
    PartialEq,
    Ord,
    PartialOrd,
    Serialize,
    Deserialize,
    TypeFingerprint,
)]
#[cfg_attr(fuzz, derive(arbitrary::Arbitrary))]
pub struct TimestampV32 {
    pub secs: u64,
    pub nanos: u32,
}

impl Timestamp {
    const NSEC_PER_SEC: u64 = 1_000_000_000;

    pub fn now() -> Self {
        SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or(Duration::ZERO).into()
    }

    pub const fn zero() -> Self {
        Self { secs: 0, nanos: 0 }
    }

    pub const fn from_nanos(nanos: u64) -> Self {
        let subsec_nanos = (nanos % Self::NSEC_PER_SEC) as u32;
        Self { secs: nanos / Self::NSEC_PER_SEC, nanos: subsec_nanos }
    }

    pub fn as_nanos(&self) -> u64 {
        Self::NSEC_PER_SEC
            .checked_mul(self.secs)
            .and_then(|val| val.checked_add(self.nanos as u64))
            .unwrap_or(0u64)
    }
}

impl From<std::time::Duration> for Timestamp {
    fn from(duration: std::time::Duration) -> Timestamp {
        Timestamp { secs: duration.as_secs(), nanos: duration.subsec_nanos() }
    }
}

impl From<Timestamp> for std::time::Duration {
    fn from(timestamp: Timestamp) -> std::time::Duration {
        Duration::new(timestamp.secs, timestamp.nanos)
    }
}

pub type ObjectKind = ObjectKindV41;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, TypeFingerprint)]
#[cfg_attr(fuzz, derive(arbitrary::Arbitrary))]
pub enum ObjectKindV41 {
    File {
        /// The number of references to this file.
        refs: u64,
    },
    Directory {
        /// The number of sub-directories in this directory.
        sub_dirs: u64,
        /// If set, contains the wrapping key id used to encrypt the file contents and filenames in
        /// this directory.
        wrapping_key_id: Option<u128>,
        /// If true, all files and sub-directories created in this directory will support case
        /// insensitive (but case-preserving) file naming.
        casefold: bool,
    },
    Graveyard,
    Symlink {
        /// The number of references to this symbolic link.
        refs: u64,
        /// `link` is the target of the link and has no meaning within Fxfs; clients are free to
        /// interpret it however they like.
        link: Vec<u8>,
    },
}

#[derive(Debug, Serialize, Deserialize, PartialEq, TypeFingerprint)]
pub enum ObjectKindV40 {
    File { refs: u64, has_overwrite_extents: bool },
    Directory { sub_dirs: u64, wrapping_key_id: Option<u128>, casefold: bool },
    Graveyard,
    Symlink { refs: u64, link: Vec<u8> },
}

impl From<ObjectKindV40> for ObjectKindV41 {
    fn from(value: ObjectKindV40) -> Self {
        match value {
            // Ignore has_overwrite_extents - it wasn't used here and we are moving it.
            ObjectKindV40::File { refs, .. } => ObjectKindV41::File { refs },
            ObjectKindV40::Directory { sub_dirs, wrapping_key_id, casefold } => {
                ObjectKindV41::Directory { sub_dirs, wrapping_key_id, casefold }
            }
            ObjectKindV40::Graveyard => ObjectKindV41::Graveyard,
            ObjectKindV40::Symlink { refs, link } => ObjectKindV41::Symlink { refs, link },
        }
    }
}

#[derive(Debug, Serialize, Deserialize, PartialEq, TypeFingerprint)]
pub enum ObjectKindV38 {
    File { refs: u64, has_overwrite_extents: bool },
    Directory { sub_dirs: u64 },
    Graveyard,
    Symlink { refs: u64, link: Vec<u8> },
}

impl From<ObjectKindV38> for ObjectKindV40 {
    fn from(value: ObjectKindV38) -> Self {
        match value {
            ObjectKindV38::File { refs, has_overwrite_extents } => {
                ObjectKindV40::File { refs, has_overwrite_extents }
            }
            ObjectKindV38::Directory { sub_dirs } => {
                ObjectKindV40::Directory { sub_dirs, wrapping_key_id: None, casefold: false }
            }
            ObjectKindV38::Graveyard => ObjectKindV40::Graveyard,
            ObjectKindV38::Symlink { refs, link } => ObjectKindV40::Symlink { refs, link },
        }
    }
}

#[derive(Debug, Deserialize, Serialize, TypeFingerprint)]
pub enum ObjectKindV32 {
    File { refs: u64 },
    Directory { sub_dirs: u64 },
    Graveyard,
    Symlink { refs: u64, link: Vec<u8> },
}

impl From<ObjectKindV32> for ObjectKindV38 {
    fn from(value: ObjectKindV32) -> Self {
        match value {
            // Overwrite extents are introduced in the same version as this flag, so nothing before
            // it has these extents.
            ObjectKindV32::File { refs } => {
                ObjectKindV38::File { refs, has_overwrite_extents: false }
            }
            ObjectKindV32::Directory { sub_dirs } => ObjectKindV38::Directory { sub_dirs },
            ObjectKindV32::Graveyard => ObjectKindV38::Graveyard,
            ObjectKindV32::Symlink { refs, link } => ObjectKindV38::Symlink { refs, link },
        }
    }
}

pub type EncryptionKeys = EncryptionKeysV40;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, TypeFingerprint)]
pub enum EncryptionKeysV40 {
    AES256XTS(WrappedKeysV40),
}

#[derive(Migrate, Serialize, Deserialize, TypeFingerprint)]
pub enum EncryptionKeysV32 {
    AES256XTS(WrappedKeysV32),
}

#[cfg(fuzz)]
impl<'a> arbitrary::Arbitrary<'a> for EncryptionKeys {
    fn arbitrary(u: &mut arbitrary::Unstructured<'a>) -> arbitrary::Result<Self> {
        <u8>::arbitrary(u).and_then(|count| {
            let mut keys = vec![];
            for _ in 0..count {
                keys.push(<(u64, u128)>::arbitrary(u).map(|(id, wrapping_key_id)| {
                    (
                        id,
                        fxfs_crypto::WrappedKey {
                            wrapping_key_id,
                            // There doesn't seem to be much point to randomly generate crypto keys.
                            key: fxfs_crypto::WrappedKeyBytes::default(),
                        },
                    )
                })?);
            }
            Ok(EncryptionKeys::AES256XTS(fxfs_crypto::WrappedKeys::from(keys)))
        })
    }
}
/// This consists of POSIX attributes that are not used in Fxfs but it may be meaningful to some
/// clients to have the ability to to set and retrieve these values.
pub type PosixAttributes = PosixAttributesV32;

#[derive(Clone, Debug, Copy, Default, Serialize, Deserialize, PartialEq, TypeFingerprint)]
#[cfg_attr(fuzz, derive(arbitrary::Arbitrary))]
pub struct PosixAttributesV32 {
    /// The mode bits associated with this object
    pub mode: u32,
    /// User ID of owner
    pub uid: u32,
    /// Group ID of owner
    pub gid: u32,
    /// Device ID
    pub rdev: u64,
}

/// Object-level attributes.  Note that these are not the same as "attributes" in the
/// ObjectValue::Attribute sense, which refers to an arbitrary data payload associated with an
/// object.  This naming collision is unfortunate.
pub type ObjectAttributes = ObjectAttributesV32;

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, TypeFingerprint)]
#[cfg_attr(fuzz, derive(arbitrary::Arbitrary))]
pub struct ObjectAttributesV32 {
    /// The timestamp at which the object was created (i.e. crtime).
    pub creation_time: TimestampV32,
    /// The timestamp at which the object's data was last modified (i.e. mtime).
    pub modification_time: TimestampV32,
    /// The project id to associate this object's resource usage with. Zero means none.
    pub project_id: u64,
    /// Mode, uid, gid, and rdev
    pub posix_attributes: Option<PosixAttributesV32>,
    /// The number of bytes allocated to all extents across all attributes for this object.
    pub allocated_size: u64,
    /// The timestamp at which the object was last read (i.e. atime).
    pub access_time: TimestampV32,
    /// The timestamp at which the object's status was last modified (i.e. ctime).
    pub change_time: TimestampV32,
}

pub type ExtendedAttributeValue = ExtendedAttributeValueV32;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, TypeFingerprint)]
#[cfg_attr(fuzz, derive(arbitrary::Arbitrary))]
pub enum ExtendedAttributeValueV32 {
    /// The extended attribute value is stored directly in this object. If the value is above a
    /// certain size, it should be stored as an attribute with extents instead.
    Inline(Vec<u8>),
    /// The extended attribute value is stored as an attribute with extents. The attribute id
    /// should be chosen to be within the range of 64-512.
    AttributeId(u64),
}

/// Id and descriptor for a child entry.
pub type ChildValue = ChildValueV32;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, TypeFingerprint)]
#[cfg_attr(fuzz, derive(arbitrary::Arbitrary))]
pub struct ChildValueV32 {
    /// The ID of the child object.
    pub object_id: u64,
    /// Describes the type of the child.
    pub object_descriptor: ObjectDescriptorV32,
}

pub type RootDigest = RootDigestV33;

#[derive(
    Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize, TypeFingerprint,
)]
#[cfg_attr(fuzz, derive(arbitrary::Arbitrary))]
pub enum RootDigestV33 {
    Sha256([u8; 32]),
    Sha512(Vec<u8>),
}

pub type FsverityMetadata = FsverityMetadataV33;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TypeFingerprint, Versioned)]
#[cfg_attr(fuzz, derive(arbitrary::Arbitrary))]
pub struct FsverityMetadataV33 {
    pub root_digest: RootDigestV33,
    pub salt: Vec<u8>,
}

/// ObjectValue is the value of an item in the object store.
/// Note that the tree stores deltas on objects, so these values describe deltas. Unless specified
/// otherwise, a value indicates an insert/replace mutation.
pub type ObjectValue = ObjectValueV41;
impl Value for ObjectValue {
    const DELETED_MARKER: Self = Self::None;
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, TypeFingerprint, Versioned)]
#[cfg_attr(fuzz, derive(arbitrary::Arbitrary))]
pub enum ObjectValueV41 {
    /// Some keys have no value (this often indicates a tombstone of some sort).  Records with this
    /// value are always filtered when a major compaction is performed, so the meaning must be the
    /// same as if the item was not present.
    None,
    /// Some keys have no value but need to differentiate between a present value and no value
    /// (None) i.e. their value is really a boolean: None => false, Some => true.
    Some,
    /// The value for an ObjectKey::Object record.
    Object { kind: ObjectKindV41, attributes: ObjectAttributesV32 },
    /// Encryption keys for an object.
    Keys(EncryptionKeysV40),
    /// An attribute associated with a file object. |size| is the size of the attribute in bytes.
    Attribute { size: u64, has_overwrite_extents: bool },
    /// An extent associated with an object.
    Extent(ExtentValueV38),
    /// A child of an object.
    Child(ChildValueV32),
    /// Graveyard entries can contain these entries which will cause a file that has extents beyond
    /// EOF to be trimmed at mount time.  This is used in cases where shrinking a file can exceed
    /// the bounds of a single transaction.
    Trim,
    /// Added to support tracking Project ID usage and limits.
    BytesAndNodes { bytes: i64, nodes: i64 },
    /// A value for an extended attribute. Either inline or a redirection to an attribute with
    /// extents.
    ExtendedAttribute(ExtendedAttributeValueV32),
    /// An attribute associated with a verified file object. |size| is the size of the attribute
    /// in bytes. |fsverity_metadata| holds the descriptor for the fsverity-enabled file.
    VerifiedAttribute { size: u64, fsverity_metadata: FsverityMetadataV33 },
}

impl From<ObjectValueV40> for ObjectValueV41 {
    fn from(value: ObjectValueV40) -> Self {
        match value {
            ObjectValueV40::None => ObjectValueV41::None,
            ObjectValueV40::Some => ObjectValueV41::Some,
            ObjectValueV40::Object { kind, attributes } => {
                ObjectValueV41::Object { kind: kind.into(), attributes }
            }
            ObjectValueV40::Keys(keys) => ObjectValueV41::Keys(keys),
            ObjectValueV40::Attribute { size } => {
                ObjectValueV41::Attribute { size, has_overwrite_extents: false }
            }
            ObjectValueV40::Extent(extent_value) => ObjectValueV41::Extent(extent_value),
            ObjectValueV40::Child(child) => ObjectValueV41::Child(child),
            ObjectValueV40::Trim => ObjectValueV41::Trim,
            ObjectValueV40::BytesAndNodes { bytes, nodes } => {
                ObjectValueV41::BytesAndNodes { bytes, nodes }
            }
            ObjectValueV40::ExtendedAttribute(xattr) => ObjectValueV41::ExtendedAttribute(xattr),
            ObjectValueV40::VerifiedAttribute { size, fsverity_metadata } => {
                ObjectValueV41::VerifiedAttribute { size, fsverity_metadata }
            }
        }
    }
}

#[derive(Serialize, Deserialize, TypeFingerprint, Versioned)]
pub enum ObjectValueV40 {
    None,
    Some,
    Object { kind: ObjectKindV40, attributes: ObjectAttributesV32 },
    Keys(EncryptionKeysV40),
    Attribute { size: u64 },
    Extent(ExtentValueV38),
    Child(ChildValueV32),
    Trim,
    BytesAndNodes { bytes: i64, nodes: i64 },
    ExtendedAttribute(ExtendedAttributeValueV32),
    VerifiedAttribute { size: u64, fsverity_metadata: FsverityMetadataV33 },
}

#[derive(Migrate, Serialize, Deserialize, TypeFingerprint, Versioned)]
#[migrate_to_version(ObjectValueV40)]
pub enum ObjectValueV38 {
    None,
    Some,
    Object { kind: ObjectKindV38, attributes: ObjectAttributesV32 },
    Keys(EncryptionKeysV32),
    Attribute { size: u64 },
    Extent(ExtentValueV38),
    Child(ChildValueV32),
    Trim,
    BytesAndNodes { bytes: i64, nodes: i64 },
    ExtendedAttribute(ExtendedAttributeValueV32),
    VerifiedAttribute { size: u64, fsverity_metadata: FsverityMetadataV33 },
}

#[derive(Migrate, Serialize, Deserialize, TypeFingerprint, Versioned)]
#[migrate_to_version(ObjectValueV38)]
pub enum ObjectValueV37 {
    None,
    Some,
    Object { kind: ObjectKindV32, attributes: ObjectAttributesV32 },
    Keys(EncryptionKeysV32),
    Attribute { size: u64 },
    Extent(ExtentValueV37),
    Child(ChildValueV32),
    Trim,
    BytesAndNodes { bytes: i64, nodes: i64 },
    ExtendedAttribute(ExtendedAttributeValueV32),
    VerifiedAttribute { size: u64, fsverity_metadata: FsverityMetadataV33 },
}

#[derive(Serialize, Deserialize, Migrate, TypeFingerprint, Versioned)]
#[migrate_to_version(ObjectValueV37)]
pub enum ObjectValueV33 {
    None,
    Some,
    Object { kind: ObjectKindV32, attributes: ObjectAttributesV32 },
    Keys(EncryptionKeysV32),
    Attribute { size: u64 },
    Extent(ExtentValueV32),
    Child(ChildValueV32),
    Trim,
    BytesAndNodes { bytes: i64, nodes: i64 },
    ExtendedAttribute(ExtendedAttributeValueV32),
    VerifiedAttribute { size: u64, fsverity_metadata: FsverityMetadataV33 },
}

#[derive(Deserialize, Migrate, Serialize, Versioned, TypeFingerprint)]
#[migrate_to_version(ObjectValueV33)]
pub enum ObjectValueV32 {
    None,
    Some,
    Object { kind: ObjectKindV32, attributes: ObjectAttributesV32 },
    Keys(EncryptionKeysV32),
    Attribute { size: u64 },
    Extent(ExtentValueV32),
    Child(ChildValueV32),
    Trim,
    BytesAndNodes { bytes: i64, nodes: i64 },
    ExtendedAttribute(ExtendedAttributeValueV32),
}

impl ObjectValue {
    /// Creates an ObjectValue for a file object.
    pub fn file(
        refs: u64,
        allocated_size: u64,
        creation_time: Timestamp,
        modification_time: Timestamp,
        access_time: Timestamp,
        change_time: Timestamp,
        project_id: u64,
        posix_attributes: Option<PosixAttributes>,
    ) -> ObjectValue {
        ObjectValue::Object {
            kind: ObjectKind::File { refs },
            attributes: ObjectAttributes {
                creation_time,
                modification_time,
                project_id,
                posix_attributes,
                allocated_size,
                access_time,
                change_time,
            },
        }
    }
    pub fn keys(keys: EncryptionKeys) -> ObjectValue {
        ObjectValue::Keys(keys)
    }
    /// Creates an ObjectValue for an object attribute.
    pub fn attribute(size: u64, has_overwrite_extents: bool) -> ObjectValue {
        ObjectValue::Attribute { size, has_overwrite_extents }
    }
    /// Creates an ObjectValue for an object attribute of a verified file.
    pub fn verified_attribute(size: u64, fsverity_metadata: FsverityMetadata) -> ObjectValue {
        ObjectValue::VerifiedAttribute { size, fsverity_metadata }
    }
    /// Creates an ObjectValue for an insertion/replacement of an object extent.
    pub fn extent(device_offset: u64, key_id: u64) -> ObjectValue {
        ObjectValue::Extent(ExtentValue::new_raw(device_offset, key_id))
    }
    /// Creates an ObjectValue for an insertion/replacement of an object extent.
    pub fn extent_with_checksum(
        device_offset: u64,
        checksum: Checksums,
        key_id: u64,
    ) -> ObjectValue {
        ObjectValue::Extent(ExtentValue::with_checksum(device_offset, checksum, key_id))
    }
    /// Creates an ObjectValue for a deletion of an object extent.
    pub fn deleted_extent() -> ObjectValue {
        ObjectValue::Extent(ExtentValue::deleted_extent())
    }
    /// Creates an ObjectValue for an object child.
    pub fn child(object_id: u64, object_descriptor: ObjectDescriptor) -> ObjectValue {
        ObjectValue::Child(ChildValue { object_id, object_descriptor })
    }
    /// Creates an ObjectValue for an object symlink.
    pub fn symlink(
        link: impl Into<Vec<u8>>,
        creation_time: Timestamp,
        modification_time: Timestamp,
        project_id: u64,
    ) -> ObjectValue {
        ObjectValue::Object {
            kind: ObjectKind::Symlink { refs: 1, link: link.into() },
            attributes: ObjectAttributes {
                creation_time,
                modification_time,
                project_id,
                ..Default::default()
            },
        }
    }
    pub fn inline_extended_attribute(value: impl Into<Vec<u8>>) -> ObjectValue {
        ObjectValue::ExtendedAttribute(ExtendedAttributeValue::Inline(value.into()))
    }
    pub fn extended_attribute(attribute_id: u64) -> ObjectValue {
        ObjectValue::ExtendedAttribute(ExtendedAttributeValue::AttributeId(attribute_id))
    }
}

pub type ObjectItem = ObjectItemV43;
pub type ObjectItemV43 = Item<ObjectKeyV43, ObjectValueV41>;
pub type ObjectItemV41 = Item<ObjectKeyV40, ObjectValueV41>;
pub type ObjectItemV40 = Item<ObjectKeyV40, ObjectValueV40>;
pub type ObjectItemV38 = Item<ObjectKeyV32, ObjectValueV38>;
pub type ObjectItemV37 = Item<ObjectKeyV32, ObjectValueV37>;
pub type ObjectItemV33 = Item<ObjectKeyV32, ObjectValueV33>;
pub type ObjectItemV32 = Item<ObjectKeyV32, ObjectValueV32>;

impl From<ObjectItemV41> for ObjectItemV43 {
    fn from(item: ObjectItemV41) -> Self {
        Self { key: item.key.into(), value: item.value.into(), sequence: item.sequence }
    }
}
impl From<ObjectItemV40> for ObjectItemV41 {
    fn from(item: ObjectItemV40) -> Self {
        Self { key: item.key.into(), value: item.value.into(), sequence: item.sequence }
    }
}

impl From<ObjectItemV38> for ObjectItemV40 {
    fn from(item: ObjectItemV38) -> Self {
        Self { key: item.key.into(), value: item.value.into(), sequence: item.sequence }
    }
}

impl From<ObjectItemV37> for ObjectItemV38 {
    fn from(item: ObjectItemV37) -> Self {
        Self { key: item.key, value: item.value.into(), sequence: item.sequence }
    }
}

impl From<ObjectItemV33> for ObjectItemV37 {
    fn from(item: ObjectItemV33) -> Self {
        Self { key: item.key, value: item.value.into(), sequence: item.sequence }
    }
}

impl From<ObjectItemV32> for ObjectItemV33 {
    fn from(item: ObjectItemV32) -> Self {
        Self { key: item.key, value: item.value.into(), sequence: item.sequence }
    }
}

impl ObjectItem {
    pub fn is_tombstone(&self) -> bool {
        matches!(
            self,
            Item {
                key: ObjectKey { data: ObjectKeyData::Object, .. },
                value: ObjectValue::None,
                ..
            }
        )
    }
}

// If the given item describes an extent, unwraps it and returns the extent key/value.
impl<'a> From<ItemRef<'a, ObjectKey, ObjectValue>>
    for Option<(/*object-id*/ u64, /*attribute-id*/ u64, &'a ExtentKey, &'a ExtentValue)>
{
    fn from(item: ItemRef<'a, ObjectKey, ObjectValue>) -> Self {
        match item {
            ItemRef {
                key:
                    ObjectKey {
                        object_id,
                        data:
                            ObjectKeyData::Attribute(
                                attribute_id, //
                                AttributeKey::Extent(ref extent_key),
                            ),
                    },
                value: ObjectValue::Extent(ref extent_value),
                ..
            } => Some((*object_id, *attribute_id, extent_key, extent_value)),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ObjectKey;
    use crate::lsm_tree::types::{LayerKey, OrdLowerBound, OrdUpperBound, RangeKey};
    use std::cmp::Ordering;

    #[test]
    fn test_next_key() {
        let next_key = ObjectKey::extent(1, 0, 0..100).next_key().unwrap();
        assert_eq!(ObjectKey::extent(1, 0, 101..200).cmp_lower_bound(&next_key), Ordering::Greater);
        assert_eq!(ObjectKey::extent(1, 0, 100..200).cmp_lower_bound(&next_key), Ordering::Equal);
        assert_eq!(ObjectKey::extent(1, 0, 100..101).cmp_lower_bound(&next_key), Ordering::Equal);
        assert_eq!(ObjectKey::extent(1, 0, 99..100).cmp_lower_bound(&next_key), Ordering::Less);
        assert_eq!(ObjectKey::extent(1, 0, 0..100).cmp_upper_bound(&next_key), Ordering::Less);
        assert_eq!(ObjectKey::extent(1, 0, 99..100).cmp_upper_bound(&next_key), Ordering::Less);
        assert_eq!(ObjectKey::extent(1, 0, 100..101).cmp_upper_bound(&next_key), Ordering::Equal);
        assert_eq!(ObjectKey::extent(1, 0, 100..200).cmp_upper_bound(&next_key), Ordering::Greater);
        assert_eq!(ObjectKey::extent(1, 0, 50..101).cmp_upper_bound(&next_key), Ordering::Equal);
        assert_eq!(ObjectKey::extent(1, 0, 50..200).cmp_upper_bound(&next_key), Ordering::Greater);
    }
    #[test]
    fn test_range_key() {
        assert_eq!(ObjectKey::object(1).overlaps(&ObjectKey::object(1)), true);
        assert_eq!(ObjectKey::object(1).overlaps(&ObjectKey::object(2)), false);
        assert_eq!(ObjectKey::extent(1, 0, 0..100).overlaps(&ObjectKey::object(1)), false);
        assert_eq!(ObjectKey::object(1).overlaps(&ObjectKey::extent(1, 0, 0..100)), false);
        assert_eq!(
            ObjectKey::extent(1, 0, 0..100).overlaps(&ObjectKey::extent(2, 0, 0..100)),
            false
        );
        assert_eq!(
            ObjectKey::extent(1, 0, 0..100).overlaps(&ObjectKey::extent(1, 1, 0..100)),
            false
        );
        assert_eq!(
            ObjectKey::extent(1, 0, 0..100).overlaps(&ObjectKey::extent(1, 0, 0..100)),
            true
        );

        assert_eq!(
            ObjectKey::extent(1, 0, 0..50).overlaps(&ObjectKey::extent(1, 0, 49..100)),
            true
        );
        assert_eq!(
            ObjectKey::extent(1, 0, 49..100).overlaps(&ObjectKey::extent(1, 0, 0..50)),
            true
        );

        assert_eq!(
            ObjectKey::extent(1, 0, 0..50).overlaps(&ObjectKey::extent(1, 0, 50..100)),
            false
        );
        assert_eq!(
            ObjectKey::extent(1, 0, 50..100).overlaps(&ObjectKey::extent(1, 0, 0..50)),
            false
        );
    }
}
