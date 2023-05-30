// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    lsm_tree::LayerInfo,
    object_store::{
        allocator::AllocatorInfoV18,
        transaction::{Mutation, MutationV20, MutationV25, MutationV29},
        AllocatorInfo, AllocatorKey, AllocatorValue, EncryptedMutations, JournalRecord,
        JournalRecordV20, JournalRecordV25, JournalRecordV29, ObjectKey, ObjectKeyV25, ObjectKeyV5,
        ObjectValue, ObjectValueV25, ObjectValueV29, ObjectValueV5, StoreInfo, SuperBlockHeader,
        SuperBlockRecord, SuperBlockRecordV25, SuperBlockRecordV29, SuperBlockRecordV5,
    },
    serialized_types::{versioned_type, Version, Versioned, VersionedLatest},
};

/// The latest version of on-disk filesystem format.
///
/// If all layer files are compacted the the journal flushed, and super-block
/// both rewritten, all versions should match this value.
///
/// If making a breaking change, please see EARLIEST_SUPPORTED_VERSION (below).
///
/// IMPORTANT: When changing this (major or minor), update the list of possible versions at
/// https://cs.opensource.google/fuchsia/fuchsia/+/main:third_party/cobalt_config/fuchsia/local_storage/versions.txt.
pub const LATEST_VERSION: Version = Version { major: 30, minor: 0 };

/// The version where the journal block size changed.
pub const JOURNAL_BLOCK_SIZE_CHANGE_VERSION: Version = Version { major: 26, minor: 0 };
// The version at which per-block seek tables were added.
pub const PER_BLOCK_SEEK_VERSION: Version = Version { major: 27, minor: 0 };
// The version at which layerfile-wide seek tables allowed approximate searching of blocks.
pub const INTERBLOCK_SEEK_VERSION: Version = Version { major: 28, minor: 0 };

/// The earliest supported version of the on-disk filesystem format.
///
/// When a breaking change is made:
/// 1) LATEST_VERSION should have it's major component increased (see above).
/// 2) EARLIEST_SUPPORTED_VERSION should be set to the new LATEST_VERSION.
/// 3) The SuperBlockHeader version (below) should also be set to the new LATEST_VERSION.
pub const EARLIEST_SUPPORTED_VERSION: Version = Version { major: 21, minor: 0 };

versioned_type! {
    24.. => AllocatorInfo,
    18.. => AllocatorInfoV18,
}
versioned_type! {
    1.. => AllocatorKey,
}
versioned_type! {
    12.. => AllocatorValue,
}
versioned_type! {
    5.. => EncryptedMutations,
}
versioned_type! {
    30.. => JournalRecord,
    29.. => JournalRecordV29,
    25.. => JournalRecordV25,
    20.. => JournalRecordV20,
}
versioned_type! {
    1.. => LayerInfo,
}
versioned_type! {
    30.. => Mutation,
    29.. => MutationV29,
    25.. => MutationV25,
    20.. => MutationV20,
}
versioned_type! {
    30.. => ObjectKey,
    25.. => ObjectKeyV25,
    5.. => ObjectKeyV5,
}
versioned_type! {
    30.. => ObjectValue,
    29.. => ObjectValueV29,
    25.. => ObjectValueV25,
    5.. => ObjectValueV5,
}
versioned_type! {
    17.. => StoreInfo,
}
versioned_type! {
    21.. => SuperBlockHeader,
}
versioned_type! {
    30.. => SuperBlockRecord,
    29.. => SuperBlockRecordV29,
    25.. => SuperBlockRecordV25,
    5.. => SuperBlockRecordV5,
}

#[cfg(test)]
fn assert_type_fprint<T: fprint::TypeFingerprint>(fp: &str) -> bool {
    if T::fingerprint() != fp {
        eprintln!(
            "====> {} fingerprint changed to {}",
            std::any::type_name::<T>(),
            T::fingerprint()
        );
        false
    } else {
        true
    }
}

#[test]
fn type_fprint_latest_version() {
    // These should only ever change when adding a new version.
    // The checks below are to ensure that we don't inadvertently change a serialized type.
    // Every versioned_type above should have a corresponding line entry here.
    let mut success = true;
    success &= assert_type_fprint::<AllocatorInfo>("struct {layers:Vec<u64>,allocated_bytes:BTreeMap<u64,u64>,marked_for_deletion:HashSet<u64>,limit_bytes:BTreeMap<u64,u64>}");
    success &= assert_type_fprint::<AllocatorKey>("struct {device_range:Range<u64>}");
    success &=
        assert_type_fprint::<AllocatorValue>("enum {None,Abs(count:u64,owner_object_id:u64)}");
    success &= assert_type_fprint::<EncryptedMutations>("struct {transactions:Vec<(struct {file_offset:u64,checksum:u64,version:struct {major:u32,minor:u8}},u64,)>,data:Vec<u8>,mutations_key_roll:Vec<(usize,struct {wrapping_key_id:u64,key:WrappedKeyBytes},)>}");
    success &= assert_type_fprint::<JournalRecord>("enum {EndBlock,Mutation(object_id:u64,mutation:enum {ObjectStore(struct {item:struct {key:struct {object_id:u64,data:enum {Object,Keys,Attribute(u64,enum {Attribute,Extent(struct {range:Range<u64>})}),Child(name:String),GraveyardEntry(object_id:u64),Project(project_id:u64,property:enum {Limit,Usage}),ExtendedAttribute(name:Vec<u8>)}},value:enum {None,Some,Object(kind:enum {File(refs:u64,allocated_size:u64),Directory(sub_dirs:u64),Graveyard,Symlink(refs:u64,link:Vec<u8>)},attributes:struct {creation_time:struct {secs:u64,nanos:u32},modification_time:struct {secs:u64,nanos:u32},project_id:u64,posix_attributes:Option<struct {mode:u32,uid:u32,gid:u32,rdev:u64}>}),Keys(enum {AES256XTS(struct {Vec<(u64,struct {wrapping_key_id:u64,key:WrappedKeyBytes},)>})}),Attribute(size:u64),Extent(enum {None,Some(device_offset:u64,checksums:enum {None,Fletcher(Vec<u64>)},key_id:u64)}),Child(object_id:u64,object_descriptor:enum {File,Directory,Volume,Symlink}),Trim,BytesAndNodes(bytes:i64,nodes:i64),ExtendedAttribute(enum {Inline(Vec<u8>),AttributeId(u64)})},sequence:u64},op:enum {Insert,ReplaceOrInsert,Merge}}),EncryptedObjectStore(Box<[u8]>),Allocator(enum {Allocate(device_range:struct {Range<u64>},owner_object_id:u64),Deallocate(device_range:struct {Range<u64>},owner_object_id:u64),SetLimit(owner_object_id:u64,bytes:u64),MarkForDeletion(u64)}),BeginFlush,EndFlush,DeleteVolume,UpdateBorrowed(u64),UpdateMutationsKey(struct {struct {wrapping_key_id:u64,key:WrappedKeyBytes}})}),Commit,Discard(u64),DidFlushDevice(u64)}");
    success &= assert_type_fprint::<LayerInfo>(
        "struct {key_value_version:struct {major:u32,minor:u8},block_size:u64}",
    );
    success &= assert_type_fprint::<Mutation>("enum {ObjectStore(struct {item:struct {key:struct {object_id:u64,data:enum {Object,Keys,Attribute(u64,enum {Attribute,Extent(struct {range:Range<u64>})}),Child(name:String),GraveyardEntry(object_id:u64),Project(project_id:u64,property:enum {Limit,Usage}),ExtendedAttribute(name:Vec<u8>)}},value:enum {None,Some,Object(kind:enum {File(refs:u64,allocated_size:u64),Directory(sub_dirs:u64),Graveyard,Symlink(refs:u64,link:Vec<u8>)},attributes:struct {creation_time:struct {secs:u64,nanos:u32},modification_time:struct {secs:u64,nanos:u32},project_id:u64,posix_attributes:Option<struct {mode:u32,uid:u32,gid:u32,rdev:u64}>}),Keys(enum {AES256XTS(struct {Vec<(u64,struct {wrapping_key_id:u64,key:WrappedKeyBytes},)>})}),Attribute(size:u64),Extent(enum {None,Some(device_offset:u64,checksums:enum {None,Fletcher(Vec<u64>)},key_id:u64)}),Child(object_id:u64,object_descriptor:enum {File,Directory,Volume,Symlink}),Trim,BytesAndNodes(bytes:i64,nodes:i64),ExtendedAttribute(enum {Inline(Vec<u8>),AttributeId(u64)})},sequence:u64},op:enum {Insert,ReplaceOrInsert,Merge}}),EncryptedObjectStore(Box<[u8]>),Allocator(enum {Allocate(device_range:struct {Range<u64>},owner_object_id:u64),Deallocate(device_range:struct {Range<u64>},owner_object_id:u64),SetLimit(owner_object_id:u64,bytes:u64),MarkForDeletion(u64)}),BeginFlush,EndFlush,DeleteVolume,UpdateBorrowed(u64),UpdateMutationsKey(struct {struct {wrapping_key_id:u64,key:WrappedKeyBytes}})}");
    success &= assert_type_fprint::<ObjectKey>("struct {object_id:u64,data:enum {Object,Keys,Attribute(u64,enum {Attribute,Extent(struct {range:Range<u64>})}),Child(name:String),GraveyardEntry(object_id:u64),Project(project_id:u64,property:enum {Limit,Usage}),ExtendedAttribute(name:Vec<u8>)}}");
    success &= assert_type_fprint::<ObjectValue>("enum {None,Some,Object(kind:enum {File(refs:u64,allocated_size:u64),Directory(sub_dirs:u64),Graveyard,Symlink(refs:u64,link:Vec<u8>)},attributes:struct {creation_time:struct {secs:u64,nanos:u32},modification_time:struct {secs:u64,nanos:u32},project_id:u64,posix_attributes:Option<struct {mode:u32,uid:u32,gid:u32,rdev:u64}>}),Keys(enum {AES256XTS(struct {Vec<(u64,struct {wrapping_key_id:u64,key:WrappedKeyBytes},)>})}),Attribute(size:u64),Extent(enum {None,Some(device_offset:u64,checksums:enum {None,Fletcher(Vec<u64>)},key_id:u64)}),Child(object_id:u64,object_descriptor:enum {File,Directory,Volume,Symlink}),Trim,BytesAndNodes(bytes:i64,nodes:i64),ExtendedAttribute(enum {Inline(Vec<u8>),AttributeId(u64)})}");
    success &= assert_type_fprint::<StoreInfo>("struct {guid:[u8;16],last_object_id:u64,layers:Vec<u64>,root_directory_object_id:u64,graveyard_directory_object_id:u64,object_count:u64,mutations_key:Option<struct {wrapping_key_id:u64,key:WrappedKeyBytes}>,mutations_cipher_offset:u64,encrypted_mutations_object_id:u64,object_id_key:Option<struct {wrapping_key_id:u64,key:WrappedKeyBytes}>}");
    success &= assert_type_fprint::<SuperBlockHeader>("struct {guid:<[u8;16]>,generation:u64,root_parent_store_object_id:u64,root_parent_graveyard_directory_object_id:u64,root_store_object_id:u64,allocator_object_id:u64,journal_object_id:u64,journal_checkpoint:struct {file_offset:u64,checksum:u64,version:struct {major:u32,minor:u8}},super_block_journal_file_offset:u64,journal_file_offsets:HashMap<u64,u64>,borrowed_metadata_space:u64,earliest_version:struct {major:u32,minor:u8}}");
    success &= assert_type_fprint::<SuperBlockRecord>("enum {Extent(Range<u64>),ObjectItem(struct {key:struct {object_id:u64,data:enum {Object,Keys,Attribute(u64,enum {Attribute,Extent(struct {range:Range<u64>})}),Child(name:String),GraveyardEntry(object_id:u64),Project(project_id:u64,property:enum {Limit,Usage}),ExtendedAttribute(name:Vec<u8>)}},value:enum {None,Some,Object(kind:enum {File(refs:u64,allocated_size:u64),Directory(sub_dirs:u64),Graveyard,Symlink(refs:u64,link:Vec<u8>)},attributes:struct {creation_time:struct {secs:u64,nanos:u32},modification_time:struct {secs:u64,nanos:u32},project_id:u64,posix_attributes:Option<struct {mode:u32,uid:u32,gid:u32,rdev:u64}>}),Keys(enum {AES256XTS(struct {Vec<(u64,struct {wrapping_key_id:u64,key:WrappedKeyBytes},)>})}),Attribute(size:u64),Extent(enum {None,Some(device_offset:u64,checksums:enum {None,Fletcher(Vec<u64>)},key_id:u64)}),Child(object_id:u64,object_descriptor:enum {File,Directory,Volume,Symlink}),Trim,BytesAndNodes(bytes:i64,nodes:i64),ExtendedAttribute(enum {Inline(Vec<u8>),AttributeId(u64)})},sequence:u64}),End}");
    assert!(success, "One or more versioned types have different type fingerprint.");
}

#[test]
fn type_fprint_v29() {
    let mut success = true;
    success &= assert_type_fprint::<AllocatorInfo>("struct {layers:Vec<u64>,allocated_bytes:BTreeMap<u64,u64>,marked_for_deletion:HashSet<u64>,limit_bytes:BTreeMap<u64,u64>}");
    success &= assert_type_fprint::<AllocatorKey>("struct {device_range:Range<u64>}");
    success &=
        assert_type_fprint::<AllocatorValue>("enum {None,Abs(count:u64,owner_object_id:u64)}");
    success &= assert_type_fprint::<EncryptedMutations>("struct {transactions:Vec<(struct {file_offset:u64,checksum:u64,version:struct {major:u32,minor:u8}},u64,)>,data:Vec<u8>,mutations_key_roll:Vec<(usize,struct {wrapping_key_id:u64,key:WrappedKeyBytes},)>}");
    success &= assert_type_fprint::<JournalRecordV29>("enum {EndBlock,Mutation(object_id:u64,mutation:enum {ObjectStore(struct {item:struct {key:struct {object_id:u64,data:enum {Object,Keys,Attribute(u64,enum {Attribute,Extent(struct {range:Range<u64>})}),Child(name:String),GraveyardEntry(object_id:u64),Project(project_id:u64,property:enum {Limit,Usage})}},value:enum {None,Some,Object(kind:enum {File(refs:u64,allocated_size:u64),Directory(sub_dirs:u64),Graveyard,Symlink(refs:u64,link:Vec<u8>)},attributes:struct {creation_time:struct {secs:u64,nanos:u32},modification_time:struct {secs:u64,nanos:u32},project_id:u64,posix_attributes:Option<struct {mode:u32,uid:u32,gid:u32,rdev:u64}>}),Keys(enum {AES256XTS(struct {Vec<(u64,struct {wrapping_key_id:u64,key:WrappedKeyBytes},)>})}),Attribute(size:u64),Extent(enum {None,Some(device_offset:u64,checksums:enum {None,Fletcher(Vec<u64>)},key_id:u64)}),Child(object_id:u64,object_descriptor:enum {File,Directory,Volume,Symlink}),Trim,BytesAndNodes(bytes:i64,nodes:i64)},sequence:u64},op:enum {Insert,ReplaceOrInsert,Merge}}),EncryptedObjectStore(Box<[u8]>),Allocator(enum {Allocate(device_range:struct {Range<u64>},owner_object_id:u64),Deallocate(device_range:struct {Range<u64>},owner_object_id:u64),SetLimit(owner_object_id:u64,bytes:u64),MarkForDeletion(u64)}),BeginFlush,EndFlush,DeleteVolume,UpdateBorrowed(u64),UpdateMutationsKey(struct {struct {wrapping_key_id:u64,key:WrappedKeyBytes}})}),Commit,Discard(u64),DidFlushDevice(u64)}");
    success &= assert_type_fprint::<LayerInfo>(
        "struct {key_value_version:struct {major:u32,minor:u8},block_size:u64}",
    );
    success &= assert_type_fprint::<MutationV29>("enum {ObjectStore(struct {item:struct {key:struct {object_id:u64,data:enum {Object,Keys,Attribute(u64,enum {Attribute,Extent(struct {range:Range<u64>})}),Child(name:String),GraveyardEntry(object_id:u64),Project(project_id:u64,property:enum {Limit,Usage})}},value:enum {None,Some,Object(kind:enum {File(refs:u64,allocated_size:u64),Directory(sub_dirs:u64),Graveyard,Symlink(refs:u64,link:Vec<u8>)},attributes:struct {creation_time:struct {secs:u64,nanos:u32},modification_time:struct {secs:u64,nanos:u32},project_id:u64,posix_attributes:Option<struct {mode:u32,uid:u32,gid:u32,rdev:u64}>}),Keys(enum {AES256XTS(struct {Vec<(u64,struct {wrapping_key_id:u64,key:WrappedKeyBytes},)>})}),Attribute(size:u64),Extent(enum {None,Some(device_offset:u64,checksums:enum {None,Fletcher(Vec<u64>)},key_id:u64)}),Child(object_id:u64,object_descriptor:enum {File,Directory,Volume,Symlink}),Trim,BytesAndNodes(bytes:i64,nodes:i64)},sequence:u64},op:enum {Insert,ReplaceOrInsert,Merge}}),EncryptedObjectStore(Box<[u8]>),Allocator(enum {Allocate(device_range:struct {Range<u64>},owner_object_id:u64),Deallocate(device_range:struct {Range<u64>},owner_object_id:u64),SetLimit(owner_object_id:u64,bytes:u64),MarkForDeletion(u64)}),BeginFlush,EndFlush,DeleteVolume,UpdateBorrowed(u64),UpdateMutationsKey(struct {struct {wrapping_key_id:u64,key:WrappedKeyBytes}})}");
    success &= assert_type_fprint::<ObjectKeyV25>("struct {object_id:u64,data:enum {Object,Keys,Attribute(u64,enum {Attribute,Extent(struct {range:Range<u64>})}),Child(name:String),GraveyardEntry(object_id:u64),Project(project_id:u64,property:enum {Limit,Usage})}}");
    success &= assert_type_fprint::<ObjectValueV29>("enum {None,Some,Object(kind:enum {File(refs:u64,allocated_size:u64),Directory(sub_dirs:u64),Graveyard,Symlink(refs:u64,link:Vec<u8>)},attributes:struct {creation_time:struct {secs:u64,nanos:u32},modification_time:struct {secs:u64,nanos:u32},project_id:u64,posix_attributes:Option<struct {mode:u32,uid:u32,gid:u32,rdev:u64}>}),Keys(enum {AES256XTS(struct {Vec<(u64,struct {wrapping_key_id:u64,key:WrappedKeyBytes},)>})}),Attribute(size:u64),Extent(enum {None,Some(device_offset:u64,checksums:enum {None,Fletcher(Vec<u64>)},key_id:u64)}),Child(object_id:u64,object_descriptor:enum {File,Directory,Volume,Symlink}),Trim,BytesAndNodes(bytes:i64,nodes:i64)}");
    success &= assert_type_fprint::<StoreInfo>("struct {guid:[u8;16],last_object_id:u64,layers:Vec<u64>,root_directory_object_id:u64,graveyard_directory_object_id:u64,object_count:u64,mutations_key:Option<struct {wrapping_key_id:u64,key:WrappedKeyBytes}>,mutations_cipher_offset:u64,encrypted_mutations_object_id:u64,object_id_key:Option<struct {wrapping_key_id:u64,key:WrappedKeyBytes}>}");
    success &= assert_type_fprint::<SuperBlockHeader>("struct {guid:<[u8;16]>,generation:u64,root_parent_store_object_id:u64,root_parent_graveyard_directory_object_id:u64,root_store_object_id:u64,allocator_object_id:u64,journal_object_id:u64,journal_checkpoint:struct {file_offset:u64,checksum:u64,version:struct {major:u32,minor:u8}},super_block_journal_file_offset:u64,journal_file_offsets:HashMap<u64,u64>,borrowed_metadata_space:u64,earliest_version:struct {major:u32,minor:u8}}");
    success &= assert_type_fprint::<SuperBlockRecordV29>("enum {Extent(Range<u64>),ObjectItem(struct {key:struct {object_id:u64,data:enum {Object,Keys,Attribute(u64,enum {Attribute,Extent(struct {range:Range<u64>})}),Child(name:String),GraveyardEntry(object_id:u64),Project(project_id:u64,property:enum {Limit,Usage})}},value:enum {None,Some,Object(kind:enum {File(refs:u64,allocated_size:u64),Directory(sub_dirs:u64),Graveyard,Symlink(refs:u64,link:Vec<u8>)},attributes:struct {creation_time:struct {secs:u64,nanos:u32},modification_time:struct {secs:u64,nanos:u32},project_id:u64,posix_attributes:Option<struct {mode:u32,uid:u32,gid:u32,rdev:u64}>}),Keys(enum {AES256XTS(struct {Vec<(u64,struct {wrapping_key_id:u64,key:WrappedKeyBytes},)>})}),Attribute(size:u64),Extent(enum {None,Some(device_offset:u64,checksums:enum {None,Fletcher(Vec<u64>)},key_id:u64)}),Child(object_id:u64,object_descriptor:enum {File,Directory,Volume,Symlink}),Trim,BytesAndNodes(bytes:i64,nodes:i64)},sequence:u64}),End}");
    assert!(success, "One or more versioned types have different type fingerprint.");
}

#[test]
fn type_fprint_v25() {
    // Whenever we bump LATEST_VERSION with struct changes, we should copy
    // `type_fprint_latest_version` and rename it as done here.
    // The names of structs will change but fingerprints should *not* need to change.
    // This ensures we don't break older versions when we add a new one.
    let mut success = true;
    success &= assert_type_fprint::<AllocatorInfoV18>("struct {layers:Vec<u64>,allocated_bytes:BTreeMap<u64,u64>,marked_for_deletion:HashSet<u64>}");
    success &= assert_type_fprint::<JournalRecordV25>("enum {EndBlock,Mutation(object_id:u64,mutation:enum {ObjectStore(struct {item:struct {key:struct {object_id:u64,data:enum {Object,Keys,Attribute(u64,enum {Attribute,Extent(struct {range:Range<u64>})}),Child(name:String),GraveyardEntry(object_id:u64),Project(project_id:u64,property:enum {Limit,Usage})}},value:enum {None,Some,Object(kind:enum {File(refs:u64,allocated_size:u64),Directory(sub_dirs:u64),Graveyard,Symlink(refs:u64,link:Vec<u8>)},attributes:struct {creation_time:struct {secs:u64,nanos:u32},modification_time:struct {secs:u64,nanos:u32},project_id:u64}),Keys(enum {AES256XTS(struct {Vec<(u64,struct {wrapping_key_id:u64,key:WrappedKeyBytes},)>})}),Attribute(size:u64),Extent(enum {None,Some(device_offset:u64,checksums:enum {None,Fletcher(Vec<u64>)},key_id:u64)}),Child(object_id:u64,object_descriptor:enum {File,Directory,Volume,Symlink}),Trim,BytesAndNodes(bytes:i64,nodes:i64)},sequence:u64},op:enum {Insert,ReplaceOrInsert,Merge}}),EncryptedObjectStore(Box<[u8]>),Allocator(enum {Allocate(device_range:struct {Range<u64>},owner_object_id:u64),Deallocate(device_range:struct {Range<u64>},owner_object_id:u64),SetLimit(owner_object_id:u64,bytes:u64),MarkForDeletion(u64)}),BeginFlush,EndFlush,DeleteVolume,UpdateBorrowed(u64),UpdateMutationsKey(struct {struct {wrapping_key_id:u64,key:WrappedKeyBytes}})}),Commit,Discard(u64),DidFlushDevice(u64)}");
    success &= assert_type_fprint::<JournalRecordV20>("enum {EndBlock,Mutation(object_id:u64,mutation:enum {ObjectStore(struct {item:struct {key:struct {object_id:u64,data:enum {Object,Keys,Attribute(u64,enum {Attribute,Extent(struct {range:Range<u64>})}),Child(name:String),GraveyardEntry(object_id:u64)}},value:enum {None,Some,Object(kind:enum {File(refs:u64,allocated_size:u64),Directory(sub_dirs:u64),Graveyard,Symlink(refs:u64,link:Vec<u8>)},attributes:struct {creation_time:struct {secs:u64,nanos:u32},modification_time:struct {secs:u64,nanos:u32}}),Keys(enum {AES256XTS(struct {Vec<(u64,struct {wrapping_key_id:u64,key:WrappedKeyBytes},)>})}),Attribute(size:u64),Extent(enum {None,Some(device_offset:u64,checksums:enum {None,Fletcher(Vec<u64>)},key_id:u64)}),Child(object_id:u64,object_descriptor:enum {File,Directory,Volume,Symlink}),Trim,BytesAndNodes(bytes:i64,nodes:i64)},sequence:u64},op:enum {Insert,ReplaceOrInsert,Merge}}),EncryptedObjectStore(Box<[u8]>),Allocator(enum {Allocate(device_range:struct {Range<u64>},owner_object_id:u64),Deallocate(device_range:struct {Range<u64>},owner_object_id:u64),SetLimit(owner_object_id:u64,bytes:u64),MarkForDeletion(u64)}),BeginFlush,EndFlush,DeleteVolume,UpdateBorrowed(u64),UpdateMutationsKey(struct {struct {wrapping_key_id:u64,key:WrappedKeyBytes}})}),Commit,Discard(u64),DidFlushDevice(u64)}");
    success &= assert_type_fprint::<MutationV25>("enum {ObjectStore(struct {item:struct {key:struct {object_id:u64,data:enum {Object,Keys,Attribute(u64,enum {Attribute,Extent(struct {range:Range<u64>})}),Child(name:String),GraveyardEntry(object_id:u64),Project(project_id:u64,property:enum {Limit,Usage})}},value:enum {None,Some,Object(kind:enum {File(refs:u64,allocated_size:u64),Directory(sub_dirs:u64),Graveyard,Symlink(refs:u64,link:Vec<u8>)},attributes:struct {creation_time:struct {secs:u64,nanos:u32},modification_time:struct {secs:u64,nanos:u32},project_id:u64}),Keys(enum {AES256XTS(struct {Vec<(u64,struct {wrapping_key_id:u64,key:WrappedKeyBytes},)>})}),Attribute(size:u64),Extent(enum {None,Some(device_offset:u64,checksums:enum {None,Fletcher(Vec<u64>)},key_id:u64)}),Child(object_id:u64,object_descriptor:enum {File,Directory,Volume,Symlink}),Trim,BytesAndNodes(bytes:i64,nodes:i64)},sequence:u64},op:enum {Insert,ReplaceOrInsert,Merge}}),EncryptedObjectStore(Box<[u8]>),Allocator(enum {Allocate(device_range:struct {Range<u64>},owner_object_id:u64),Deallocate(device_range:struct {Range<u64>},owner_object_id:u64),SetLimit(owner_object_id:u64,bytes:u64),MarkForDeletion(u64)}),BeginFlush,EndFlush,DeleteVolume,UpdateBorrowed(u64),UpdateMutationsKey(struct {struct {wrapping_key_id:u64,key:WrappedKeyBytes}})}");
    success &= assert_type_fprint::<MutationV20>("enum {ObjectStore(struct {item:struct {key:struct {object_id:u64,data:enum {Object,Keys,Attribute(u64,enum {Attribute,Extent(struct {range:Range<u64>})}),Child(name:String),GraveyardEntry(object_id:u64)}},value:enum {None,Some,Object(kind:enum {File(refs:u64,allocated_size:u64),Directory(sub_dirs:u64),Graveyard,Symlink(refs:u64,link:Vec<u8>)},attributes:struct {creation_time:struct {secs:u64,nanos:u32},modification_time:struct {secs:u64,nanos:u32}}),Keys(enum {AES256XTS(struct {Vec<(u64,struct {wrapping_key_id:u64,key:WrappedKeyBytes},)>})}),Attribute(size:u64),Extent(enum {None,Some(device_offset:u64,checksums:enum {None,Fletcher(Vec<u64>)},key_id:u64)}),Child(object_id:u64,object_descriptor:enum {File,Directory,Volume,Symlink}),Trim,BytesAndNodes(bytes:i64,nodes:i64)},sequence:u64},op:enum {Insert,ReplaceOrInsert,Merge}}),EncryptedObjectStore(Box<[u8]>),Allocator(enum {Allocate(device_range:struct {Range<u64>},owner_object_id:u64),Deallocate(device_range:struct {Range<u64>},owner_object_id:u64),SetLimit(owner_object_id:u64,bytes:u64),MarkForDeletion(u64)}),BeginFlush,EndFlush,DeleteVolume,UpdateBorrowed(u64),UpdateMutationsKey(struct {struct {wrapping_key_id:u64,key:WrappedKeyBytes}})}");
    success &= assert_type_fprint::<ObjectKeyV5>("struct {object_id:u64,data:enum {Object,Keys,Attribute(u64,enum {Attribute,Extent(struct {range:Range<u64>})}),Child(name:String),GraveyardEntry(object_id:u64)}}");
    success &= assert_type_fprint::<ObjectValueV25>("enum {None,Some,Object(kind:enum {File(refs:u64,allocated_size:u64),Directory(sub_dirs:u64),Graveyard,Symlink(refs:u64,link:Vec<u8>)},attributes:struct {creation_time:struct {secs:u64,nanos:u32},modification_time:struct {secs:u64,nanos:u32},project_id:u64}),Keys(enum {AES256XTS(struct {Vec<(u64,struct {wrapping_key_id:u64,key:WrappedKeyBytes},)>})}),Attribute(size:u64),Extent(enum {None,Some(device_offset:u64,checksums:enum {None,Fletcher(Vec<u64>)},key_id:u64)}),Child(object_id:u64,object_descriptor:enum {File,Directory,Volume,Symlink}),Trim,BytesAndNodes(bytes:i64,nodes:i64)}");
    success &= assert_type_fprint::<ObjectValueV5>("enum {None,Some,Object(kind:enum {File(refs:u64,allocated_size:u64),Directory(sub_dirs:u64),Graveyard,Symlink(refs:u64,link:Vec<u8>)},attributes:struct {creation_time:struct {secs:u64,nanos:u32},modification_time:struct {secs:u64,nanos:u32}}),Keys(enum {AES256XTS(struct {Vec<(u64,struct {wrapping_key_id:u64,key:WrappedKeyBytes},)>})}),Attribute(size:u64),Extent(enum {None,Some(device_offset:u64,checksums:enum {None,Fletcher(Vec<u64>)},key_id:u64)}),Child(object_id:u64,object_descriptor:enum {File,Directory,Volume,Symlink}),Trim,BytesAndNodes(bytes:i64,nodes:i64)}");
    success &= assert_type_fprint::<SuperBlockRecordV25>("enum {Extent(Range<u64>),ObjectItem(struct {key:struct {object_id:u64,data:enum {Object,Keys,Attribute(u64,enum {Attribute,Extent(struct {range:Range<u64>})}),Child(name:String),GraveyardEntry(object_id:u64),Project(project_id:u64,property:enum {Limit,Usage})}},value:enum {None,Some,Object(kind:enum {File(refs:u64,allocated_size:u64),Directory(sub_dirs:u64),Graveyard,Symlink(refs:u64,link:Vec<u8>)},attributes:struct {creation_time:struct {secs:u64,nanos:u32},modification_time:struct {secs:u64,nanos:u32},project_id:u64}),Keys(enum {AES256XTS(struct {Vec<(u64,struct {wrapping_key_id:u64,key:WrappedKeyBytes},)>})}),Attribute(size:u64),Extent(enum {None,Some(device_offset:u64,checksums:enum {None,Fletcher(Vec<u64>)},key_id:u64)}),Child(object_id:u64,object_descriptor:enum {File,Directory,Volume,Symlink}),Trim,BytesAndNodes(bytes:i64,nodes:i64)},sequence:u64}),End}");
    success &= assert_type_fprint::<SuperBlockRecordV5>("enum {Extent(Range<u64>),ObjectItem(struct {key:struct {object_id:u64,data:enum {Object,Keys,Attribute(u64,enum {Attribute,Extent(struct {range:Range<u64>})}),Child(name:String),GraveyardEntry(object_id:u64)}},value:enum {None,Some,Object(kind:enum {File(refs:u64,allocated_size:u64),Directory(sub_dirs:u64),Graveyard,Symlink(refs:u64,link:Vec<u8>)},attributes:struct {creation_time:struct {secs:u64,nanos:u32},modification_time:struct {secs:u64,nanos:u32}}),Keys(enum {AES256XTS(struct {Vec<(u64,struct {wrapping_key_id:u64,key:WrappedKeyBytes},)>})}),Attribute(size:u64),Extent(enum {None,Some(device_offset:u64,checksums:enum {None,Fletcher(Vec<u64>)},key_id:u64)}),Child(object_id:u64,object_descriptor:enum {File,Directory,Volume,Symlink}),Trim,BytesAndNodes(bytes:i64,nodes:i64)},sequence:u64}),End}");
    assert!(success, "One or more versioned types have different type fingerprint.");
}
