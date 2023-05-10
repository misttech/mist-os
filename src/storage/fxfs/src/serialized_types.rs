// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! # On-disk versioning
//!
//! This module manages serialization and deserialization of FxFS structures to disk.
//!
//! In FxFS, on-disk layout is deferred to serialization libraries (i.e.
//! [bincode](https://github.com/bincode-org/bincode#is-bincode-suitable-for-storage), serde).
//! Stability of these layouts depends on both struct/enum stability and serialization libraries.
//!
//! This module provides struct/enum stability by maintaining a generation of types and a
//! means to upgrade from older versions to newer ones.
//!
//! The trait mechanism used is flexible enough to allow specific versions to use differing
//! serialization code if we ever require it.
//!
//! ## Traits
//!
//! All serialization is done with serde so [Serialize] and [Deserialize] traits must be derived
//! for all types and sub-types.
//!
//! All versioned, serializable struct/enum type should have the [Versioned] trait.
//! The most recent version of a type should also have the [VersionedLatest] trait.
//! These traits are largely implemented for you via the `versioned_type!` macro as follows:
//!
//! ```ignore
//! versioned_type! {
//!   3.. => SuperBlockHeaderV3,
//!   2.. => SuperBlockHeaderV2,
//!   1.. => SuperBlockHeaderV1,
//! }
//!
//! // Note the reuse of SuperBlockRecordV1 for two versions.
//! versioned_type! {
//!   3.. => SuperBlockRecordV2,
//!   1.. => SuperBlockRecordV1,
//! }
//! ```
//!
//! The user is required to implement [From] to migrate from one version to the next.
//! The above macros will implement further [From] traits allowing direct upgrade from any version
//! to the latest. [VersionedLatest] provides a `deserialize_from_version` method that can be
//! used to deserialize any supported version and then upgrade it to the latest format.
//!
//! ## Conventions
//!
//! There are limits to how automated this process can be, so we rely heavily on conventions.
//!
//!  * Every versioned type should have a monotonically increasing `Vn` suffix for each generation.
//!  * Every versioned sub-type (e.g. child of a struct) should also have `Vn` suffix.
//!  * In type definitions, all versioned child types should be referred to explicitly with their
//!    `Vn` suffix. This prevents us from accidentally changing a version by changing a sub-type.

mod traits;

use serde::{Deserialize, Serialize};

pub const DEFAULT_MAX_SERIALIZED_RECORD_SIZE: u64 = 4096;

// Re-export the traits we need.
pub use fxfs_macros::{migrate_nodefault, migrate_to_version, versioned_type, Migrate, Versioned};
pub use traits::{Version, Versioned, VersionedLatest};

// For test use, we add [Versioned] and [VersionedLatest] to primitive integer types (i32, ...).
#[cfg(test)]
pub mod test_traits;

#[cfg(test)]
mod tests;

// Re-export all Fxfs types.
mod types;
pub use types::{
    EARLIEST_SUPPORTED_VERSION, INTERBLOCK_SEEK_VERSION, JOURNAL_BLOCK_SIZE_CHANGE_VERSION,
    LATEST_VERSION, PER_BLOCK_SEEK_VERSION,
};

// TODO(fxbug.dev/122125): This should be versioned.  Whether we reused serialized_types is up for
// debate (since this version might be better off as independent from the journal version).
// TODO(fxbug.dev/122125): Is this the best home for this?
#[derive(Serialize, Deserialize, Debug)]
pub struct BlobMetadata {
    pub hashes: Vec<[u8; 32]>,
    pub chunk_size: u64,
    pub compressed_offsets: Vec<u64>,
    pub uncompressed_size: u64,
}
