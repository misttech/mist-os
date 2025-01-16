// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::arrays::FsUseType;
use super::extensible_bitmap::{MAP_NODE_BITS, MAX_BITMAP_ITEMS};
use super::metadata::{
    CONFIG_HANDLE_UNKNOWN_MASK, CONFIG_MLS_FLAG, POLICYDB_SIGNATURE, POLICYDB_STRING_MAX_LENGTH,
    POLICYDB_VERSION_MAX, POLICYDB_VERSION_MIN, SELINUX_MAGIC,
};
use super::symbols::{ClassDefault, ClassDefaultRange};

use bstr::BString;
use thiserror::Error;

/// Structured errors that may be encountered parsing a binary policy.
#[derive(Clone, Debug, Error, PartialEq)]
pub enum ParseError {
    #[error("expected MLS-enabled flag ({CONFIG_MLS_FLAG:#032b}), but found {found_config:#032b}")]
    ConfigMissingMlsFlag { found_config: u32 },
    #[error("expected handle-unknown config at most 1 bit set (mask {CONFIG_HANDLE_UNKNOWN_MASK:#032b}), but found {masked_bits:#032b}")]
    InvalidHandleUnknownConfigurationBits { masked_bits: u32 },
    #[error("expected end of policy, but found {num_bytes} additional bytes")]
    TrailingBytes { num_bytes: usize },
    #[error("expected data item of type {type_name} ({type_size} bytes), but found {num_bytes}")]
    MissingData { type_name: &'static str, type_size: usize, num_bytes: usize },
    #[error("expected {num_items} data item(s) of type {type_name} ({type_size} bytes), but found {num_bytes}")]
    MissingSliceData {
        type_name: &'static str,
        type_size: usize,
        num_items: usize,
        num_bytes: usize,
    },
    #[error("required parsing routine not implemented")]
    NotImplemented,
}

/// Structured errors that may be encountered validating a binary policy.
#[derive(Debug, Error, PartialEq)]
pub enum ValidateError {
    #[error("expected selinux magic value {SELINUX_MAGIC:#x}, but found {found_magic:#x}")]
    InvalidMagic { found_magic: u32 },
    #[error("expected signature length in range [0, {POLICYDB_STRING_MAX_LENGTH}], but found {found_length}")]
    InvalidSignatureLength { found_length: u32 },
    #[error("expected signature {POLICYDB_SIGNATURE:?}, but found {:?}", bstr::BStr::new(found_signature.as_slice()))]
    InvalidSignature { found_signature: Vec<u8> },
    #[error("expected policy version in range [{POLICYDB_VERSION_MIN}, {POLICYDB_VERSION_MAX}], but found {found_policy_version}")]
    InvalidPolicyVersion { found_policy_version: u32 },
    #[error("expected extensible bitmap item size to be exactly {MAP_NODE_BITS}, but found {found_size}")]
    InvalidExtensibleBitmapItemSize { found_size: u32 },
    #[error("expected extensible bitmap item high bit to be multiple of {found_size}, but found {found_high_bit}")]
    MisalignedExtensibleBitmapHighBit { found_size: u32, found_high_bit: u32 },
    #[error("expected extensible bitmap item high bit to be at most items_count + items_size = {found_count} + {found_size}, but found {found_high_bit}")]
    InvalidExtensibleBitmapHighBit { found_size: u32, found_high_bit: u32, found_count: u32 },
    #[error("expected extensible bitmap item count to be in range [0, {MAX_BITMAP_ITEMS}], but found {found_count}")]
    InvalidExtensibleBitmapCount { found_count: u32 },
    #[error("found extensible bitmap item count = 0, but high count != 0")]
    ExtensibleBitmapNonZeroHighBitAndZeroCount,
    #[error("expected extensible bitmap item start bit to be multiple of item size {found_size}, but found {found_start_bit}")]
    MisalignedExtensibleBitmapItemStartBit { found_start_bit: u32, found_size: u32 },
    #[error("expected extensible bitmap items to be in sorted order, but found item starting at {found_start_bit} after item that ends at {min_start}")]
    OutOfOrderExtensibleBitmapItems { found_start_bit: u32, min_start: u32 },
    #[error("expected extensible bitmap items to refer to bits in range [0, {found_high_bit}), but found item that ends at {found_items_end}")]
    ExtensibleBitmapItemOverflow { found_items_end: u32, found_high_bit: u32 },
    #[error(
        "expected class default binary value to be one of {}, {}, or {}, but found {value}",
        ClassDefault::DEFAULT_UNSPECIFIED,
        ClassDefault::DEFAULT_SOURCE,
        ClassDefault::DEFAULT_TARGET
    )]
    InvalidClassDefault { value: u32 },
    #[error(
        "expected class default binary value to be one of {:?}, but found {value}",
        [ClassDefaultRange::DEFAULT_UNSPECIFIED,
        ClassDefaultRange::DEFAULT_SOURCE_LOW,
        ClassDefaultRange::DEFAULT_SOURCE_HIGH,
        ClassDefaultRange::DEFAULT_SOURCE_LOW_HIGH,
        ClassDefaultRange::DEFAULT_TARGET_LOW,
        ClassDefaultRange::DEFAULT_TARGET_HIGH,
        ClassDefaultRange::DEFAULT_TARGET_LOW_HIGH,
        ClassDefaultRange::DEFAULT_UNKNOWN_USED_VALUE]
    )]
    InvalidClassDefaultRange { value: u32 },
    #[error("missing initial SID {initial_sid:?}")]
    MissingInitialSid { initial_sid: crate::InitialSid },
    #[error(
        "invalid SELinux fs_use type; expected one of {:?}, but found {value}",
        [FsUseType::Xattr as u32,
        FsUseType::Trans as u32,
        FsUseType::Task as u32]
    )]
    InvalidFsUseType { value: u32 },
    #[error("non-optional Id field is zero")]
    NonOptionalIdIsZero,
    #[error("required validation routine not implemented")]
    NotImplemented,
    #[error("undefined {kind} Id value {id}")]
    UnknownId { kind: &'static str, id: String },
    #[error("invalid MLS range: {low}-{high}")]
    InvalidMlsRange { low: BString, high: BString },
}

/// Structured errors that may be encountered querying a binary policy.
#[derive(Debug, Error, PartialEq)]
pub enum QueryError {
    #[error("the class {class_name:?} does not exist")]
    UnknownClass { class_name: String },
    #[error("the permission {permission_name:?} does not exist for class {class_name:?}")]
    UnknownPermission { class_name: String, permission_name: String },
    #[error("the source type {source_type_name:?} does not exist")]
    UnknownSourceType { source_type_name: String },
    #[error("the target type {target_type_name:?} does not exist")]
    UnknownTargetType { target_type_name: String },
}
