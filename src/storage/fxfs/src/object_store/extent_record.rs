// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// TODO(https://fxbug.dev/42178223): need validation after deserialization.

use {
    crate::{
        checksum::{Checksums, ChecksumsV32, ChecksumsV37, ChecksumsV38},
        lsm_tree::types::{OrdLowerBound, OrdUpperBound},
        serialized_types::{migrate_to_version, Migrate},
    },
    bit_vec::BitVec,
    fprint::TypeFingerprint,
    serde::{Deserialize, Serialize},
    std::{
        cmp::{max, min},
        hash::Hash,
        ops::Range,
    },
};

/// The common case for extents which cover the data payload of some object.
pub const DEFAULT_DATA_ATTRIBUTE_ID: u64 = 0;

/// For Blobs in Fxfs, we store the merkle tree at a well-known attribute.
/// TODO(https://fxbug.dev/42073113): Is this the best place to store the merkle tree?  What about inline with
/// data?
pub const BLOB_MERKLE_ATTRIBUTE_ID: u64 = 1;

/// For fsverity files in Fxfs, we store the merkle tree of the verified file at a well-known
/// attribute.
pub const FSVERITY_MERKLE_ATTRIBUTE_ID: u64 = 2;

/// ExtentKey is a child of ObjectKey for Object attributes that have attached extents
/// (at time of writing this was only the used for file contents).
pub type ExtentKey = ExtentKeyV32;

#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize, TypeFingerprint)]
#[cfg_attr(fuzz, derive(arbitrary::Arbitrary))]
pub struct ExtentKeyV32 {
    pub range: Range<u64>,
}

impl ExtentKey {
    /// Creates an ExtentKey.
    pub fn new(range: std::ops::Range<u64>) -> Self {
        Self { range }
    }

    /// Returns the range of bytes common between this extent and |other|.
    pub fn overlap(&self, other: &ExtentKey) -> Option<Range<u64>> {
        if self.range.end <= other.range.start || self.range.start >= other.range.end {
            None
        } else {
            Some(max(self.range.start, other.range.start)..min(self.range.end, other.range.end))
        }
    }

    /// Returns the search key for this extent; that is, a key which is <= this key under Ord and
    /// OrdLowerBound.
    /// This would be used when searching for an extent with |find| (when we want to find any
    /// overlapping extent, which could include extents that start earlier).
    /// For example, if the tree has extents 50..150 and 150..200 and we wish to read 100..200,
    /// we'd search for 0..101 which would set the iterator to 50..150.
    pub fn search_key(&self) -> Self {
        assert_ne!(self.range.start, self.range.end);
        ExtentKey::search_key_from_offset(self.range.start)
    }

    /// Similar to previous, but from an offset.  Returns a search key that will find the first
    /// extent that touches offset..
    pub fn search_key_from_offset(offset: u64) -> Self {
        Self { range: 0..offset + 1 }
    }

    /// Returns the merge key for this extent; that is, a key which is <= this extent and any other
    /// possibly overlapping extent, under Ord. This would be used to set the hint for |merge_into|.
    ///
    /// For example, if the tree has extents 0..50, 50..150 and 150..200 and we wish to insert
    /// 100..150, we'd use a merge hint of 0..100 which would set the iterator to 50..150 (the first
    /// element > 100..150 under Ord).
    pub fn key_for_merge_into(&self) -> Self {
        Self { range: 0..self.range.start }
    }
}

// The normal comparison uses the end of the range before the start of the range. This makes
// searching for records easier because it's easy to find K.. (where K is the key you are searching
// for), which is what we want since our search routines find items with keys >= a search key.
// OrdLowerBound orders by the start of an extent.
impl OrdUpperBound for ExtentKey {
    fn cmp_upper_bound(&self, other: &ExtentKey) -> std::cmp::Ordering {
        // The comparison uses the end of the range so that we can more easily do queries.  Whilst
        // it might be tempting to break ties by comparing the range start, next_key currently
        // relies on keys with the same end being equal, and since we do not support overlapping
        // keys within the same layer, ties can always be broken using layer index.  Insertions into
        // the mutable layer should always be done using merge_into, which will ensure keys don't
        // end up overlapping.
        self.range.end.cmp(&other.range.end)
    }
}

impl OrdLowerBound for ExtentKey {
    // Orders by the start of the range rather than the end, and doesn't include the end in the
    // comparison. This is used when merging, where we want to merge keys in lower-bound order.
    fn cmp_lower_bound(&self, other: &ExtentKey) -> std::cmp::Ordering {
        self.range.start.cmp(&other.range.start)
    }
}

impl Ord for ExtentKey {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // We expect cmp_upper_bound and cmp_lower_bound to be used mostly, but ObjectKey needs an
        // Ord method in order to compare other enum variants, and Transaction requires an ObjectKey
        // to implement Ord.
        self.range.start.cmp(&other.range.start).then(self.range.end.cmp(&other.range.end))
    }
}

impl PartialOrd for ExtentKey {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// The mode the extent is operating in. This changes how writes work to this region of the file.
pub type ExtentMode = ExtentModeV38;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TypeFingerprint)]
pub enum ExtentModeV38 {
    /// This extent doesn't have defined write semantics. The writer chooses how to handle the data
    /// here. Notable uses of this are things which have their own separate checksum mechanism,
    /// like the journal file and blobs.
    Raw,
    /// This extent uses copy-on-write semantics. We store the post-encryption checksums for data
    /// validation. New writes to this logical range are written to new extents.
    Cow(ChecksumsV38),
    /// This extent uses overwrite semantics. The bitmap keeps track of blocks which have been
    /// written to at least once. Blocks which haven't been written to at least once are logically
    /// zero, so the bitmap needs to be accounted for while reading. While this extent exists, new
    /// writes to this logical range will go to the same on-disk location.
    OverwritePartial(BitVec),
    /// This extent uses overwrite semantics. Every block in this extent has been written to at
    /// least once, so we don't store the block bitmap anymore. While this extent exists, new
    /// writes to this logical range will go to the same on-disk location.
    Overwrite,
}

#[cfg(fuzz)]
impl<'a> arbitrary::Arbitrary<'a> for ExtentMode {
    fn arbitrary(u: &mut arbitrary::Unstructured<'a>) -> arbitrary::Result<Self> {
        Ok(match u.int_in_range(0..=3)? {
            0 => ExtentMode::Raw,
            1 => ExtentMode::Cow(u.arbitrary()?),
            2 => ExtentMode::OverwritePartial(BitVec::from_bytes(u.arbitrary()?)),
            3 => ExtentMode::Overwrite,
            _ => unreachable!(),
        })
    }
}

/// ExtentValue is the payload for an extent in the object store, which describes where the extent
/// is physically located.
pub type ExtentValue = ExtentValueV38;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TypeFingerprint)]
#[cfg_attr(fuzz, derive(arbitrary::Arbitrary))]
pub enum ExtentValueV38 {
    /// Indicates a deleted extent; that is, the logical range described by the extent key is
    /// considered to be deleted.
    None,
    /// The location of the extent and other related information.  `key_id` identifies which of the
    /// object's keys should be used.  Unencrypted files should use 0 (which can also be used for
    /// encrypted files).  `mode` describes the write pattern for this extent.
    Some { device_offset: u64, mode: ExtentModeV38, key_id: u64 },
}

impl ExtentValue {
    pub fn new(device_offset: u64, mode: ExtentMode) -> ExtentValue {
        ExtentValue::Some { device_offset, mode, key_id: 0 }
    }

    pub fn new_raw(device_offset: u64) -> ExtentValue {
        Self::new(device_offset, ExtentMode::Raw)
    }

    /// Creates an ExtentValue with a checksum
    pub fn with_checksum(device_offset: u64, checksums: Checksums) -> ExtentValue {
        Self::new(device_offset, ExtentMode::Cow(checksums))
    }

    /// Creates an ExtentValue for an overwrite range with no blocks written to yet.
    pub fn blank_overwrite_extent(device_offset: u64, length: usize) -> ExtentValue {
        Self::new(device_offset, ExtentMode::OverwritePartial(BitVec::from_elem(length, false)))
    }

    /// Creates an ExtentValue for an overwrite range with all the blocks initialized.
    pub fn initialized_overwrite_extent(device_offset: u64) -> ExtentValue {
        Self::new(device_offset, ExtentMode::Overwrite)
    }

    /// Creates an ObjectValue for a deletion of an object extent.
    pub fn deleted_extent() -> ExtentValue {
        ExtentValue::None
    }

    pub fn is_deleted(&self) -> bool {
        if let ExtentValue::None = self {
            true
        } else {
            false
        }
    }

    /// Returns a new ExtentValue offset by `amount`.  Both `amount` and `extent_len` must be
    /// multiples of the underlying block size.
    pub fn offset_by(&self, amount: u64, extent_len: u64) -> Self {
        match self {
            ExtentValue::None => Self::deleted_extent(),
            ExtentValue::Some { device_offset, mode, key_id } => {
                let mode = match mode {
                    ExtentMode::Raw => ExtentMode::Raw,
                    ExtentMode::Cow(checksums) => {
                        if checksums.len() > 0 {
                            let index = (amount / (extent_len / checksums.len() as u64)) as usize;
                            ExtentMode::Cow(checksums.offset_by(index))
                        } else {
                            ExtentMode::Cow(Checksums::fletcher(Vec::new()))
                        }
                    }
                    ExtentMode::Overwrite => ExtentMode::Overwrite,
                    ExtentMode::OverwritePartial(bitmap) => {
                        debug_assert!(bitmap.len() > 0);
                        let index = (amount / (extent_len / bitmap.len() as u64)) as usize;
                        ExtentMode::OverwritePartial(bitmap.clone().split_off(index))
                    }
                };
                ExtentValue::Some { device_offset: device_offset + amount, mode, key_id: *key_id }
            }
        }
    }

    /// Returns a new ExtentValue after shrinking the extent from |original_len| to |new_len|.
    pub fn shrunk(&self, original_len: u64, new_len: u64) -> Self {
        match self {
            ExtentValue::None => Self::deleted_extent(),
            ExtentValue::Some { device_offset, mode, key_id } => {
                let mode = match mode {
                    ExtentMode::Raw => ExtentMode::Raw,
                    ExtentMode::Cow(checksums) => {
                        if checksums.len() > 0 {
                            let len = (new_len / (original_len / checksums.len() as u64)) as usize;
                            ExtentMode::Cow(checksums.shrunk(len))
                        } else {
                            ExtentMode::Cow(Checksums::fletcher(Vec::new()))
                        }
                    }
                    ExtentMode::Overwrite => ExtentMode::Overwrite,
                    ExtentMode::OverwritePartial(bitmap) => {
                        debug_assert!(bitmap.len() > 0);
                        let len = (new_len / (original_len / bitmap.len() as u64)) as usize;
                        let mut new_bitmap = bitmap.clone();
                        new_bitmap.truncate(len);
                        ExtentMode::OverwritePartial(new_bitmap)
                    }
                };
                ExtentValue::Some { device_offset: *device_offset, mode, key_id: *key_id }
            }
        }
    }
}

#[derive(Debug, Serialize, Deserialize, TypeFingerprint)]
#[migrate_to_version(ExtentValueV38)]
pub enum ExtentValueV37 {
    None,
    Some { device_offset: u64, checksums: ChecksumsV37, key_id: u64 },
}

#[derive(Debug, Serialize, Deserialize, Migrate, TypeFingerprint)]
#[migrate_to_version(ExtentValueV37)]
pub enum ExtentValueV32 {
    None,
    Some { device_offset: u64, checksums: ChecksumsV32, key_id: u64 },
}

impl From<ExtentValueV37> for ExtentValueV38 {
    fn from(value: ExtentValueV37) -> Self {
        match value {
            ExtentValueV37::None => ExtentValue::None,
            ExtentValueV37::Some { device_offset, checksums, key_id } => {
                let mode = match checksums.migrate() {
                    None => ExtentMode::Raw,
                    Some(checksums) => ExtentMode::Cow(checksums),
                };
                ExtentValue::Some { device_offset, mode, key_id }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use {
        super::{ExtentKey, ExtentMode, ExtentValue},
        crate::{
            checksum::Checksums,
            lsm_tree::types::{OrdLowerBound, OrdUpperBound},
        },
        bit_vec::BitVec,
        std::cmp::Ordering,
    };

    #[test]
    fn test_extent_cmp() {
        let extent = ExtentKey::new(100..150);
        assert_eq!(extent.cmp_upper_bound(&ExtentKey::new(0..100)), Ordering::Greater);
        assert_eq!(extent.cmp_upper_bound(&ExtentKey::new(0..110)), Ordering::Greater);
        assert_eq!(extent.cmp_upper_bound(&ExtentKey::new(0..150)), Ordering::Equal);
        assert_eq!(extent.cmp_upper_bound(&ExtentKey::new(99..150)), Ordering::Equal);
        assert_eq!(extent.cmp_upper_bound(&ExtentKey::new(100..150)), Ordering::Equal);
        assert_eq!(extent.cmp_upper_bound(&ExtentKey::new(0..151)), Ordering::Less);
        assert_eq!(extent.cmp_upper_bound(&ExtentKey::new(100..151)), Ordering::Less);
        assert_eq!(extent.cmp_upper_bound(&ExtentKey::new(150..1000)), Ordering::Less);
    }

    #[test]
    fn test_extent_cmp_lower_bound() {
        let extent = ExtentKey::new(100..150);
        assert_eq!(extent.cmp_lower_bound(&ExtentKey::new(0..100)), Ordering::Greater);
        assert_eq!(extent.cmp_lower_bound(&ExtentKey::new(0..110)), Ordering::Greater);
        assert_eq!(extent.cmp_lower_bound(&ExtentKey::new(0..150)), Ordering::Greater);
        assert_eq!(extent.cmp_lower_bound(&ExtentKey::new(0..1000)), Ordering::Greater);
        assert_eq!(extent.cmp_lower_bound(&ExtentKey::new(99..1000)), Ordering::Greater);
        assert_eq!(extent.cmp_lower_bound(&ExtentKey::new(100..150)), Ordering::Equal);
        // cmp_lower_bound does not check the upper bound of the range
        assert_eq!(extent.cmp_lower_bound(&ExtentKey::new(100..1000)), Ordering::Equal);
        assert_eq!(extent.cmp_lower_bound(&ExtentKey::new(101..102)), Ordering::Less);
    }

    #[test]
    fn test_extent_search_and_insertion_key() {
        let extent = ExtentKey::new(100..150);
        assert_eq!(extent.search_key(), ExtentKey::new(0..101));
        assert_eq!(extent.cmp_lower_bound(&extent.search_key()), Ordering::Greater);
        assert_eq!(extent.cmp_upper_bound(&extent.search_key()), Ordering::Greater);
        assert_eq!(extent.key_for_merge_into(), ExtentKey::new(0..100));
        assert_eq!(extent.cmp_lower_bound(&extent.key_for_merge_into()), Ordering::Greater);
    }

    #[test]
    fn extent_value_offset_by() {
        assert_eq!(ExtentValue::None.offset_by(1024, 2048), ExtentValue::None);
        assert_eq!(ExtentValue::new_raw(1024).offset_by(0, 2048), ExtentValue::new_raw(1024));
        assert_eq!(ExtentValue::new_raw(1024).offset_by(1024, 2048), ExtentValue::new_raw(2048));
        assert_eq!(ExtentValue::new_raw(1024).offset_by(2048, 2048), ExtentValue::new_raw(3072));

        let make_checksums = |range: std::ops::Range<u64>| Checksums::fletcher(range.collect());

        // In these tests we are making block size 256.
        assert_eq!(
            ExtentValue::with_checksum(1024, make_checksums(0..8)).offset_by(0, 2048),
            ExtentValue::with_checksum(1024, make_checksums(0..8))
        );
        assert_eq!(
            ExtentValue::with_checksum(1024, make_checksums(0..8)).offset_by(1024, 2048),
            ExtentValue::with_checksum(2048, make_checksums(4..8))
        );
        assert_eq!(
            ExtentValue::with_checksum(1024, make_checksums(0..8)).offset_by(2048, 2048),
            ExtentValue::with_checksum(3072, Checksums::fletcher(Vec::new()))
        );

        // Takes a place to switch from zeros to ones. The goal is to make sure there is exactly
        // one zero and then only ones where we expect offset to slice.
        let make_bitmap = |cut, length| {
            let mut begin_bitmap = BitVec::from_elem(cut, false);
            let mut end_bitmap = BitVec::from_elem(length - cut, true);
            begin_bitmap.append(&mut end_bitmap);
            ExtentMode::OverwritePartial(begin_bitmap)
        };
        let make_extent =
            |device_offset, mode| ExtentValue::Some { device_offset, mode, key_id: 0 };

        assert_eq!(
            make_extent(1024, make_bitmap(1, 8)).offset_by(0, 2048),
            make_extent(1024, make_bitmap(1, 8))
        );
        assert_eq!(
            make_extent(1024, make_bitmap(5, 8)).offset_by(1024, 2048),
            make_extent(2048, make_bitmap(1, 4))
        );
        assert_eq!(
            make_extent(1024, make_bitmap(0, 8)).offset_by(2048, 2048),
            make_extent(3072, ExtentMode::OverwritePartial(BitVec::new()))
        );

        assert_eq!(
            make_extent(1024, ExtentMode::Overwrite).offset_by(0, 2048),
            make_extent(1024, ExtentMode::Overwrite)
        );
        assert_eq!(
            make_extent(1024, ExtentMode::Overwrite).offset_by(1024, 2048),
            make_extent(2048, ExtentMode::Overwrite)
        );
        assert_eq!(
            make_extent(1024, ExtentMode::Overwrite).offset_by(2048, 2048),
            make_extent(3072, ExtentMode::Overwrite)
        );
    }

    #[test]
    fn extent_value_shrunk() {
        assert_eq!(ExtentValue::None.shrunk(2048, 1024), ExtentValue::None);
        assert_eq!(ExtentValue::new_raw(1024).shrunk(2048, 2048), ExtentValue::new_raw(1024));
        assert_eq!(ExtentValue::new_raw(1024).shrunk(2048, 1024), ExtentValue::new_raw(1024));
        assert_eq!(ExtentValue::new_raw(1024).shrunk(2048, 0), ExtentValue::new_raw(1024));

        let make_checksums = |range: std::ops::Range<u64>| Checksums::fletcher(range.collect());

        // In these tests we are making block size 256.
        assert_eq!(
            ExtentValue::with_checksum(1024, make_checksums(0..8)).shrunk(2048, 2048),
            ExtentValue::with_checksum(1024, make_checksums(0..8))
        );
        assert_eq!(
            ExtentValue::with_checksum(1024, make_checksums(0..8)).shrunk(2048, 1024),
            ExtentValue::with_checksum(1024, make_checksums(0..4))
        );
        assert_eq!(
            ExtentValue::with_checksum(1024, make_checksums(0..8)).shrunk(2048, 0),
            ExtentValue::with_checksum(1024, Checksums::fletcher(Vec::new()))
        );

        // Takes a place to switch from zeros to ones. The goal is to make sure there is exactly
        // one zero and then only ones where we expect offset to slice.
        let make_bitmap = |cut, length| {
            let mut begin_bitmap = BitVec::from_elem(cut, false);
            let mut end_bitmap = BitVec::from_elem(length - cut, true);
            begin_bitmap.append(&mut end_bitmap);
            ExtentMode::OverwritePartial(begin_bitmap)
        };
        let make_extent =
            |device_offset, mode| ExtentValue::Some { device_offset, mode, key_id: 0 };

        assert_eq!(
            make_extent(1024, make_bitmap(1, 8)).shrunk(2048, 2048),
            make_extent(1024, make_bitmap(1, 8))
        );
        assert_eq!(
            make_extent(1024, make_bitmap(3, 8)).shrunk(2048, 1024),
            make_extent(1024, make_bitmap(3, 4))
        );
        assert_eq!(
            make_extent(1024, make_bitmap(0, 8)).shrunk(2048, 0),
            make_extent(1024, ExtentMode::OverwritePartial(BitVec::new()))
        );

        assert_eq!(
            make_extent(1024, ExtentMode::Overwrite).shrunk(2048, 2048),
            make_extent(1024, ExtentMode::Overwrite)
        );
        assert_eq!(
            make_extent(1024, ExtentMode::Overwrite).shrunk(2048, 1024),
            make_extent(1024, ExtentMode::Overwrite)
        );
        assert_eq!(
            make_extent(1024, ExtentMode::Overwrite).shrunk(2048, 0),
            make_extent(1024, ExtentMode::Overwrite)
        );
    }
}
