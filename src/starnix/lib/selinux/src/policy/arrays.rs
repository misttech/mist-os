// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::parser::PolicyCursor;
use super::{
    array_type, array_type_validate_deref_both, AccessVector, Array, ClassId, Counted, Parse,
    PolicyValidationContext, RoleId, TypeId, Validate, ValidateArray,
};
use crate::policy::error::{ParseError, ValidateError};
use crate::policy::extensible_bitmap::ExtensibleBitmap;
use crate::policy::symbols::{MlsLevel, MlsRange};
use crate::policy::UserId;
use anyhow::Context as _;

use std::num::NonZeroU32;
use std::ops::Shl;
use zerocopy::{little_endian as le, FromBytes, Immutable, KnownLayout, Unaligned};

pub(super) const MIN_POLICY_VERSION_FOR_INFINITIBAND_PARTITION_KEY: u32 = 31;

/// Mask for [`AccessVectorRuleMetadata`]'s `access_vector_rule_type` that
/// indicates that the access vector rule's associated data is a type ID.
pub(super) const ACCESS_VECTOR_RULE_DATA_IS_TYPE_ID_MASK: u16 = 0x070;
/// Mask for [`AccessVectorRuleMetadata`]'s `access_vector_rule_type` that
/// indicates that the access vector rule's associated data is an extended
/// permission.
pub(super) const ACCESS_VECTOR_RULE_DATA_IS_XPERM_MASK: u16 = 0x0700;

/// ** Access vector rule types ***
///
/// Although these values each have a single bit set, they appear to be
/// used as enum values rather than as bit masks: i.e., the policy compiler
/// does not produce access vector rule structures that have more than
/// one of these types.
/// Value for [`AccessVectorRuleMetadata`] `access_vector_rule_type` that
/// indicates that the access vector rule comes from an `allow [source]
/// [target]:[class] { [permissions] };` policy statement.
pub(super) const ACCESS_VECTOR_RULE_TYPE_ALLOW: u16 = 0x1;
/// Value for [`AccessVectorRuleMetadata`] `access_vector_rule_type` that
/// indicates that the access vector rule comes from an `auditallow [source]
/// [target]:[class] { [permissions] };` policy statement.
pub(super) const ACCESS_VECTOR_RULE_TYPE_AUDITALLOW: u16 = 0x2;
/// Value for [`AccessVectorRuleMetadata`] `access_vector_rule_type` that
/// indicates that the access vector rule comes from a `dontaudit [source]
/// [target]:[class] { [permissions] };` policy statement.
pub(super) const ACCESS_VECTOR_RULE_TYPE_DONTAUDIT: u16 = 0x4;
/// Value for [`AccessVectorRuleMetadata`] `access_vector_rule_type` that
/// indicates that the access vector rule comes from a `type_transition
/// [source] [target]:[class] [new_type];` policy statement.
pub(super) const ACCESS_VECTOR_RULE_TYPE_TYPE_TRANSITION: u16 = 0x10;
/// Value for [`AccessVectorRuleMetadata`] `access_vector_rule_type` that
/// indicates that the access vector rule comes from a `type_member
/// [source] [target]:[class] [member_type];` policy statement.
#[allow(dead_code)]
pub(super) const ACCESS_VECTOR_RULE_TYPE_TYPE_MEMBER: u16 = 0x20;
/// Value for [`AccessVectorRuleMetadata`] `access_vector_rule_type` that
/// indicates that the access vector rule comes from a `type_change
/// [source] [target]:[class] [change_type];` policy statement.
#[allow(dead_code)]
pub(super) const ACCESS_VECTOR_RULE_TYPE_TYPE_CHANGE: u16 = 0x40;
/// Value for [`AccessVectorRuleMetadata`] `access_vector_rule_type`
/// that indicates that the access vector rule comes from an
/// `allowxperm [source] [target]:[class] [permission] {
/// [extended_permissions] };` policy statement.
pub(super) const ACCESS_VECTOR_RULE_TYPE_ALLOWXPERM: u16 = 0x100;
/// Value for [`AccessVectorRuleMetadata`] `access_vector_rule_type`
/// that indicates that the access vector rule comes from an
/// `auditallowxperm [source] [target]:[class] [permission] {
/// [extended_permissions] };` policy statement.
pub(super) const ACCESS_VECTOR_RULE_TYPE_AUDITALLOWXPERM: u16 = 0x200;
/// Value for [`AccessVectorRuleMetadata`] `access_vector_rule_type`
/// that indicates that the access vector rule comes from an
/// `dontauditxperm [source] [target]:[class] [permission] {
/// [extended_permissions] };` policy statement.
pub(super) const ACCESS_VECTOR_RULE_TYPE_DONTAUDITXPERM: u16 = 0x400;

/// ** Extended permissions types ***
///
/// Value for ['ExtendedPermissions'] `xperms_type` that indicates
/// that the xperms set is a proper subset of the 16-bit ioctl
/// xperms with a given high byte value.
pub(super) const XPERMS_TYPE_IOCTL_PREFIX_AND_POSTFIXES: u8 = 1;
/// Value for ['ExtendedPermissions'] `xperms_type` that indicates
/// that the xperms set consists of all 16-bit ioctl xperms with a
/// given high byte, for one or more high byte values.
pub(super) const XPERMS_TYPE_IOCTL_PREFIXES: u8 = 2;

#[allow(type_alias_bounds)]
pub(super) type SimpleArray<T> = Array<le::U32, T>;

impl<T: Validate> Validate for SimpleArray<T> {
    type Error = <T as Validate>::Error;

    /// Defers to `self.data` for validation. `self.data` has access to all information, including
    /// size stored in `self.metadata`.
    fn validate(&self, context: &mut PolicyValidationContext) -> Result<(), Self::Error> {
        self.data.validate(context)
    }
}

impl Counted for le::U32 {
    fn count(&self) -> u32 {
        self.get()
    }
}

pub(super) type ConditionalNodes = Vec<ConditionalNode>;

impl Validate for ConditionalNodes {
    type Error = anyhow::Error;

    /// TODO: Validate internal consistency between consecutive [`ConditionalNode`] instances.
    fn validate(&self, _context: &mut PolicyValidationContext) -> Result<(), Self::Error> {
        Ok(())
    }
}

array_type!(ConditionalNodeItems, ConditionalNodeMetadata, Vec<ConditionalNodeDatum>);

array_type_validate_deref_both!(ConditionalNodeItems);

impl ValidateArray<ConditionalNodeMetadata, ConditionalNodeDatum> for ConditionalNodeItems {
    type Error = anyhow::Error;

    /// TODO: Validate internal consistency between [`ConditionalNodeMetadata`] consecutive
    /// [`ConditionalNodeDatum`].
    fn validate_array(
        _context: &mut PolicyValidationContext,
        _metadata: &ConditionalNodeMetadata,
        _items: &[ConditionalNodeDatum],
    ) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(Debug, PartialEq)]
pub(super) struct ConditionalNode {
    items: ConditionalNodeItems,
    true_list: SimpleArray<AccessVectorRules>,
    false_list: SimpleArray<AccessVectorRules>,
}

impl Parse for ConditionalNode
where
    ConditionalNodeItems: Parse,
    SimpleArray<AccessVectorRules>: Parse,
{
    type Error = anyhow::Error;

    fn parse(bytes: PolicyCursor) -> Result<(Self, PolicyCursor), Self::Error> {
        let tail = bytes;

        let (items, tail) = ConditionalNodeItems::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing conditional node items")?;

        let (true_list, tail) = SimpleArray::<AccessVectorRules>::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing conditional node true list")?;

        let (false_list, tail) = SimpleArray::<AccessVectorRules>::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing conditional node false list")?;

        Ok((Self { items, true_list, false_list }, tail))
    }
}

#[derive(Clone, Debug, KnownLayout, FromBytes, Immutable, PartialEq, Unaligned)]
#[repr(C, packed)]
pub(super) struct ConditionalNodeMetadata {
    state: le::U32,
    count: le::U32,
}

impl Counted for ConditionalNodeMetadata {
    fn count(&self) -> u32 {
        self.count.get()
    }
}

impl Validate for ConditionalNodeMetadata {
    type Error = anyhow::Error;

    /// TODO: Validate [`ConditionalNodeMetadata`] internals.
    fn validate(&self, _context: &mut PolicyValidationContext) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(Clone, Debug, KnownLayout, FromBytes, Immutable, PartialEq, Unaligned)]
#[repr(C, packed)]
pub(super) struct ConditionalNodeDatum {
    node_type: le::U32,
    boolean: le::U32,
}

impl Validate for [ConditionalNodeDatum] {
    type Error = anyhow::Error;

    /// TODO: Validate sequence of [`ConditionalNodeDatum`].
    fn validate(&self, _context: &mut PolicyValidationContext) -> Result<(), Self::Error> {
        Ok(())
    }
}

/// The list of access control rules defined by policy statements of the
/// following kinds:
/// - `allow`, `dontaudit`, `auditallow`, and `neverallow`, which specify
///   an access vector describing a permission set.
/// - `allowxperm`, `auditallowxperm`, `dontaudit`, which specify a set
///   of extended permissions.
/// - `type_transition`, `type_change`, and `type_member', which include
///   a type id describing a permitted new type.
pub(super) type AccessVectorRules = Vec<AccessVectorRule>;

impl Validate for AccessVectorRules {
    type Error = anyhow::Error;

    fn validate(&self, context: &mut PolicyValidationContext) -> Result<(), Self::Error> {
        for access_vector_rule in self {
            access_vector_rule.validate(context)?;
        }
        Ok(())
    }
}

#[derive(Debug, PartialEq)]
pub(super) struct AccessVectorRule {
    pub metadata: AccessVectorRuleMetadata,
    permission_data: PermissionData,
}

impl AccessVectorRule {
    /// Returns whether this access vector rule comes from an
    /// `allow [source] [target]:[class] { [permissions] };` policy statement.
    pub fn is_allow(&self) -> bool {
        (self.metadata.access_vector_rule_type & ACCESS_VECTOR_RULE_TYPE_ALLOW) != 0
    }

    /// Returns whether this access vector rule comes from an
    /// `auditallow [source] [target]:[class] { [permissions] };` policy statement.
    pub fn is_auditallow(&self) -> bool {
        (self.metadata.access_vector_rule_type & ACCESS_VECTOR_RULE_TYPE_AUDITALLOW) != 0
    }

    /// Returns whether this access vector rule comes from an
    /// `dontaudit [source] [target]:[class] { [permissions] };` policy statement.
    pub fn is_dontaudit(&self) -> bool {
        (self.metadata.access_vector_rule_type & ACCESS_VECTOR_RULE_TYPE_DONTAUDIT) != 0
    }

    /// Returns whether this access vector rule comes from a
    /// `type_transition [source] [target]:[class] [new_type];` policy statement.
    pub fn is_type_transition(&self) -> bool {
        (self.metadata.access_vector_rule_type & ACCESS_VECTOR_RULE_TYPE_TYPE_TRANSITION) != 0
    }

    /// Returns whether this access vector rule comes from an
    /// `allowxperm [source] [target]:[class] [permission] {
    /// [extended_permissions] };` policy statement.
    pub fn is_allowxperm(&self) -> bool {
        (self.metadata.access_vector_rule_type & ACCESS_VECTOR_RULE_TYPE_ALLOWXPERM) != 0
    }

    /// Returns whether this access vector rule comes from an
    /// `auditallowxperm [source] [target]:[class] [permission] {
    /// [extended_permissions] };` policy statement.
    pub fn is_auditallowxperm(&self) -> bool {
        (self.metadata.access_vector_rule_type & ACCESS_VECTOR_RULE_TYPE_AUDITALLOWXPERM) != 0
    }

    /// Returns whether this access vector rule comes from a
    /// `dontauditxperm [source] [target]:[class] [permission] {
    /// [extended_permissions] };` policy statement.
    pub fn is_dontauditxperm(&self) -> bool {
        (self.metadata.access_vector_rule_type & ACCESS_VECTOR_RULE_TYPE_DONTAUDITXPERM) != 0
    }

    /// Returns the source type id in this access vector rule. This id
    /// corresponds to the [`super::symbols::Type`] `id()` of some type or
    /// attribute in the same policy.
    pub fn source_type(&self) -> TypeId {
        TypeId(NonZeroU32::new(self.metadata.source_type.into()).unwrap())
    }

    /// Returns the target type id in this access vector rule. This id
    /// corresponds to the [`super::symbols::Type`] `id()` of some type or
    /// attribute in the same policy.
    pub fn target_type(&self) -> TypeId {
        TypeId(NonZeroU32::new(self.metadata.target_type.into()).unwrap())
    }

    /// Returns the target class id in this access vector rule. This id
    /// corresponds to the [`super::symbols::Class`] `id()` of some class in the
    /// same policy. Although the index is returned as a 32-bit value, the field
    /// itself is 16-bit
    pub fn target_class(&self) -> ClassId {
        ClassId(NonZeroU32::new(self.metadata.class.into()).unwrap())
    }

    /// An access vector that corresponds to the `[access_vector]` in an
    /// `allow [source] [target]:[class] [access_vector]` policy statement,
    /// or similarly for an `auditallow` or `dontaudit` policy statement.
    /// Return value is `None` if this access vector rule corresponds to a
    /// different kind of policy statement.
    pub fn access_vector(&self) -> Option<AccessVector> {
        match &self.permission_data {
            PermissionData::AccessVector(access_vector_raw) => {
                Some(AccessVector(access_vector_raw.get()))
            }
            _ => None,
        }
    }

    /// A numeric type id that corresponds to the `[new_type]` in a
    /// `type_transition [source] [target]:[class] [new_type];` policy statement,
    /// or similarly for a `type_member` or `type_change` policy statement.
    /// Return value is `None` if this access vector rule corresponds to a
    /// different kind of policy statement.
    pub fn new_type(&self) -> Option<TypeId> {
        match &self.permission_data {
            PermissionData::NewType(new_type) => {
                Some(TypeId(NonZeroU32::new(new_type.get().into()).unwrap()))
            }
            _ => None,
        }
    }

    /// A set of extended permissions that corresponds to the `[xperms]` in an
    /// `allowxperm [source][target]:[class] [permission] [xperms]` policy
    /// statement, or similarly for an `auditallowxperm` or `dontauditxperm`
    /// policy statement. Return value is `None` if this access vector rule
    /// corresponds to a different kind of policy statement.
    pub fn extended_permissions(&self) -> Option<&ExtendedPermissions> {
        match &self.permission_data {
            PermissionData::ExtendedPermissions(xperms) => Some(xperms),
            _ => None,
        }
    }
}

impl Parse for AccessVectorRule {
    type Error = anyhow::Error;

    fn parse(bytes: PolicyCursor) -> Result<(Self, PolicyCursor), Self::Error> {
        let tail = bytes;

        let num_bytes = tail.len();
        let (metadata, tail) =
            PolicyCursor::parse::<AccessVectorRuleMetadata>(tail).ok_or_else(|| {
                ParseError::MissingData {
                    type_name: std::any::type_name::<AccessVectorRuleMetadata>(),
                    type_size: std::mem::size_of::<AccessVectorRuleMetadata>(),
                    num_bytes,
                }
            })?;
        let access_vector_rule_type = metadata.access_vector_rule_type;
        let num_bytes = tail.len();
        let (permission_data, tail) =
            if (access_vector_rule_type & ACCESS_VECTOR_RULE_DATA_IS_XPERM_MASK) != 0 {
                let (xperms, tail) = ExtendedPermissions::parse(tail)
                    .map_err(Into::<anyhow::Error>::into)
                    .context("parsing extended permissions")?;
                (PermissionData::ExtendedPermissions(xperms), tail)
            } else if (access_vector_rule_type & ACCESS_VECTOR_RULE_DATA_IS_TYPE_ID_MASK) != 0 {
                let (new_type, tail) =
                    PolicyCursor::parse::<le::U32>(tail).ok_or(ParseError::MissingData {
                        type_name: "PermissionData::NewType",
                        type_size: std::mem::size_of::<le::U32>(),
                        num_bytes,
                    })?;
                (PermissionData::NewType(new_type), tail)
            } else {
                let (access_vector, tail) =
                    PolicyCursor::parse::<le::U32>(tail).ok_or(ParseError::MissingData {
                        type_name: "PermissionData::AccessVector",
                        type_size: std::mem::size_of::<le::U32>(),
                        num_bytes,
                    })?;
                (PermissionData::AccessVector(access_vector), tail)
            };
        Ok((Self { metadata, permission_data }, tail))
    }
}

impl Validate for AccessVectorRule {
    type Error = anyhow::Error;

    fn validate(&self, _context: &mut PolicyValidationContext) -> Result<(), Self::Error> {
        if self.metadata.class.get() == 0 {
            return Err(ValidateError::NonOptionalIdIsZero.into());
        }
        if let PermissionData::ExtendedPermissions(xperms) = &self.permission_data {
            let xperms_type = xperms.xperms_type;
            if !(xperms_type == XPERMS_TYPE_IOCTL_PREFIX_AND_POSTFIXES
                || xperms_type == XPERMS_TYPE_IOCTL_PREFIXES)
            {
                return Err(
                    ValidateError::InvalidExtendedPermissionsType { type_: xperms_type }.into()
                );
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, KnownLayout, FromBytes, Immutable, PartialEq, Unaligned)]
#[repr(C, packed)]
pub(super) struct AccessVectorRuleMetadata {
    source_type: le::U16,
    target_type: le::U16,
    class: le::U16,
    access_vector_rule_type: le::U16,
}

#[derive(Debug, PartialEq)]
pub(super) enum PermissionData {
    AccessVector(le::U32),
    NewType(le::U32),
    ExtendedPermissions(ExtendedPermissions),
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct ExtendedPermissions {
    pub(super) xperms_type: u8,
    pub(super) xperms_optional_prefix: u8,
    pub(super) xperms_bitmap: XpermsBitmap,
}

impl Parse for ExtendedPermissions {
    type Error = anyhow::Error;

    fn parse(bytes: PolicyCursor) -> Result<(Self, PolicyCursor), Self::Error> {
        let tail = bytes;
        let num_bytes = tail.len();
        let (type_, tail) = PolicyCursor::parse::<u8>(tail).ok_or(ParseError::MissingData {
            type_name: "ExtendedPermissions::xperms_type",
            type_size: std::mem::size_of::<u8>(),
            num_bytes,
        })?;
        let xperms_type = type_;
        let num_bytes = tail.len();
        let (prefix, tail) = PolicyCursor::parse::<u8>(tail).ok_or(ParseError::MissingData {
            type_name: "ExtendedPermissions::xperms_optional_prefix",
            type_size: std::mem::size_of::<u8>(),
            num_bytes,
        })?;
        let xperms_optional_prefix = prefix;
        let num_bytes = tail.len();
        let (bitmap, tail) =
            PolicyCursor::parse::<[le::U32; 8]>(tail).ok_or(ParseError::MissingData {
                type_name: "ExtendedPermissions::xperms_bitmap",
                type_size: std::mem::size_of::<[le::U32; 8]>(),
                num_bytes,
            })?;
        Ok((
            ExtendedPermissions {
                xperms_type,
                xperms_optional_prefix,
                xperms_bitmap: XpermsBitmap(bitmap),
            },
            tail,
        ))
    }
}

impl ExtendedPermissions {
    #[cfg(test)]
    fn count(&self) -> u64 {
        let count = self
            .xperms_bitmap
            .0
            .iter()
            .fold(0, |count, block| (count as u64) + (block.get().count_ones() as u64));
        match self.xperms_type {
            XPERMS_TYPE_IOCTL_PREFIX_AND_POSTFIXES => count,
            XPERMS_TYPE_IOCTL_PREFIXES => count * 0x100,
            _ => unreachable!("invalid xperms_type in validated ExtendedPermissions"),
        }
    }

    #[cfg(test)]
    fn contains(&self, xperm: u16) -> bool {
        let [postfix, prefix] = xperm.to_le_bytes();
        if self.xperms_type == XPERMS_TYPE_IOCTL_PREFIX_AND_POSTFIXES
            && self.xperms_optional_prefix != prefix
        {
            return false;
        }
        let value = match self.xperms_type {
            XPERMS_TYPE_IOCTL_PREFIX_AND_POSTFIXES => postfix,
            XPERMS_TYPE_IOCTL_PREFIXES => prefix,
            _ => unreachable!("invalid xperms_type in validated ExtendedPermissions"),
        };
        self.xperms_bitmap.contains(value)
    }
}

// A bitmap representing a subset of `{0x0,...,0xff}`.
#[derive(Clone, Debug, PartialEq)]
pub struct XpermsBitmap([le::U32; 8]);

impl XpermsBitmap {
    const BITMAP_BLOCKS: usize = 8;
    pub const ALL: Self = Self([le::U32::MAX_VALUE; Self::BITMAP_BLOCKS]);
    pub const NONE: Self = Self([le::U32::ZERO; Self::BITMAP_BLOCKS]);

    #[cfg(test)]
    pub fn new(elements: [le::U32; 8]) -> Self {
        Self(elements)
    }

    pub fn contains(&self, value: u8) -> bool {
        let block_index = (value as usize) / 32;
        let bit_index = ((value as usize) % 32) as u32;
        self.0[block_index] & le::U32::new(1).shl(bit_index) != 0
    }
}

impl std::ops::BitOrAssign<&Self> for XpermsBitmap {
    fn bitor_assign(&mut self, rhs: &Self) {
        (0..Self::BITMAP_BLOCKS).for_each(|i| self.0[i] |= rhs.0[i])
    }
}

impl std::ops::SubAssign<&Self> for XpermsBitmap {
    fn sub_assign(&mut self, rhs: &Self) {
        (0..Self::BITMAP_BLOCKS).for_each(|i| self.0[i] = self.0[i] ^ (self.0[i] & rhs.0[i]))
    }
}

array_type!(RoleTransitions, le::U32, Vec<RoleTransition>);

array_type_validate_deref_both!(RoleTransitions);

impl ValidateArray<le::U32, RoleTransition> for RoleTransitions {
    type Error = anyhow::Error;

    /// [`RoleTransitions`] have no additional metadata (beyond length encoding).
    fn validate_array(
        context: &mut PolicyValidationContext,
        _metadata: &le::U32,
        items: &[RoleTransition],
    ) -> Result<(), Self::Error> {
        items.validate(context)
    }
}

#[derive(Clone, Debug, KnownLayout, FromBytes, Immutable, PartialEq, Unaligned)]
#[repr(C, packed)]
pub(super) struct RoleTransition {
    role: le::U32,
    role_type: le::U32,
    new_role: le::U32,
    tclass: le::U32,
}

impl RoleTransition {
    pub(super) fn current_role(&self) -> RoleId {
        RoleId(NonZeroU32::new(self.role.get()).unwrap())
    }

    pub(super) fn type_(&self) -> TypeId {
        TypeId(NonZeroU32::new(self.role_type.get()).unwrap())
    }

    pub(super) fn class(&self) -> ClassId {
        ClassId(NonZeroU32::new(self.tclass.get()).unwrap())
    }

    pub(super) fn new_role(&self) -> RoleId {
        RoleId(NonZeroU32::new(self.new_role.get()).unwrap())
    }
}

impl Validate for [RoleTransition] {
    type Error = anyhow::Error;

    fn validate(&self, _context: &mut PolicyValidationContext) -> Result<(), Self::Error> {
        for role_transition in self {
            NonZeroU32::new(role_transition.role.get())
                .ok_or(ValidateError::NonOptionalIdIsZero)?;
            NonZeroU32::new(role_transition.role_type.get())
                .ok_or(ValidateError::NonOptionalIdIsZero)?;
            NonZeroU32::new(role_transition.tclass.get())
                .ok_or(ValidateError::NonOptionalIdIsZero)?;
            NonZeroU32::new(role_transition.new_role.get())
                .ok_or(ValidateError::NonOptionalIdIsZero)?;
        }
        Ok(())
    }
}

array_type!(RoleAllows, le::U32, Vec<RoleAllow>);

array_type_validate_deref_both!(RoleAllows);

impl ValidateArray<le::U32, RoleAllow> for RoleAllows {
    type Error = anyhow::Error;

    /// [`RoleAllows`] have no additional metadata (beyond length encoding).
    fn validate_array(
        context: &mut PolicyValidationContext,
        _metadata: &le::U32,
        items: &[RoleAllow],
    ) -> Result<(), Self::Error> {
        items.validate(context)
    }
}

#[derive(Clone, Debug, KnownLayout, FromBytes, Immutable, PartialEq, Unaligned)]
#[repr(C, packed)]
pub(super) struct RoleAllow {
    role: le::U32,
    new_role: le::U32,
}

impl RoleAllow {
    pub(super) fn source_role(&self) -> RoleId {
        RoleId(NonZeroU32::new(self.role.get()).unwrap())
    }

    pub(super) fn new_role(&self) -> RoleId {
        RoleId(NonZeroU32::new(self.new_role.get()).unwrap())
    }
}

impl Validate for [RoleAllow] {
    type Error = anyhow::Error;

    fn validate(&self, _context: &mut PolicyValidationContext) -> Result<(), Self::Error> {
        for rule in self {
            NonZeroU32::new(rule.role.get()).ok_or(ValidateError::NonOptionalIdIsZero)?;
            NonZeroU32::new(rule.new_role.get()).ok_or(ValidateError::NonOptionalIdIsZero)?;
        }
        Ok(())
    }
}

#[derive(Debug, PartialEq)]
pub(super) enum FilenameTransitionList {
    PolicyVersionGeq33(SimpleArray<FilenameTransitions>),
    PolicyVersionLeq32(SimpleArray<DeprecatedFilenameTransitions>),
}

impl Validate for FilenameTransitionList {
    type Error = anyhow::Error;

    fn validate(&self, context: &mut PolicyValidationContext) -> Result<(), Self::Error> {
        match self {
            Self::PolicyVersionLeq32(list) => {
                list.validate(context).map_err(Into::<anyhow::Error>::into)
            }
            Self::PolicyVersionGeq33(list) => {
                list.validate(context).map_err(Into::<anyhow::Error>::into)
            }
        }
    }
}

pub(super) type FilenameTransitions = Vec<FilenameTransition>;

impl Validate for FilenameTransitions {
    type Error = anyhow::Error;

    /// TODO: Validate sequence of [`FilenameTransition`] objects.
    fn validate(&self, _context: &mut PolicyValidationContext) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(Debug, PartialEq)]
pub(super) struct FilenameTransition {
    filename: SimpleArray<Vec<u8>>,
    transition_type: le::U32,
    transition_class: le::U32,
    items: SimpleArray<FilenameTransitionItems>,
}

impl FilenameTransition {
    pub(super) fn name_bytes(&self) -> &[u8] {
        &self.filename.data
    }

    pub(super) fn target_type(&self) -> TypeId {
        TypeId(NonZeroU32::new(self.transition_type.get()).unwrap())
    }

    pub(super) fn target_class(&self) -> ClassId {
        ClassId(NonZeroU32::new(self.transition_class.get()).unwrap())
    }

    pub(super) fn outputs(&self) -> &[FilenameTransitionItem] {
        &self.items.data
    }
}

impl Parse for FilenameTransition
where
    SimpleArray<Vec<u8>>: Parse,
    SimpleArray<FilenameTransitionItems>: Parse,
{
    type Error = anyhow::Error;

    fn parse(bytes: PolicyCursor) -> Result<(Self, PolicyCursor), Self::Error> {
        let tail = bytes;

        let (filename, tail) = SimpleArray::<Vec<u8>>::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing filename for filename transition")?;

        let num_bytes = tail.len();
        let (transition_type, tail) =
            PolicyCursor::parse::<le::U32>(tail).ok_or(ParseError::MissingData {
                type_name: "FilenameTransition::transition_type",
                type_size: std::mem::size_of::<le::U32>(),
                num_bytes,
            })?;

        let num_bytes = tail.len();
        let (transition_class, tail) =
            PolicyCursor::parse::<le::U32>(tail).ok_or(ParseError::MissingData {
                type_name: "FilenameTransition::transition_class",
                type_size: std::mem::size_of::<le::U32>(),
                num_bytes,
            })?;

        let (items, tail) = SimpleArray::<FilenameTransitionItems>::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing items for filename transition")?;

        Ok((Self { filename, transition_type, transition_class, items }, tail))
    }
}

pub(super) type FilenameTransitionItems = Vec<FilenameTransitionItem>;

#[derive(Debug, PartialEq)]
pub(super) struct FilenameTransitionItem {
    stypes: ExtensibleBitmap,
    out_type: le::U32,
}

impl FilenameTransitionItem {
    pub(super) fn has_source_type(&self, source_type: TypeId) -> bool {
        self.stypes.is_set(source_type.0.get() - 1)
    }

    pub(super) fn out_type(&self) -> TypeId {
        TypeId(NonZeroU32::new(self.out_type.get()).unwrap())
    }
}

impl Parse for FilenameTransitionItem
where
    ExtensibleBitmap: Parse,
{
    type Error = anyhow::Error;

    fn parse(bytes: PolicyCursor) -> Result<(Self, PolicyCursor), Self::Error> {
        let tail = bytes;

        let (stypes, tail) = ExtensibleBitmap::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing stypes extensible bitmap for file transition")?;

        let num_bytes = tail.len();
        let (out_type, tail) =
            PolicyCursor::parse::<le::U32>(tail).ok_or(ParseError::MissingData {
                type_name: "FilenameTransitionItem::out_type",
                type_size: std::mem::size_of::<le::U32>(),
                num_bytes,
            })?;

        Ok((Self { stypes, out_type }, tail))
    }
}

pub(super) type DeprecatedFilenameTransitions = Vec<DeprecatedFilenameTransition>;

impl Validate for DeprecatedFilenameTransitions {
    type Error = anyhow::Error;

    /// TODO: Validate sequence of [`DeprecatedFilenameTransition`] objects.
    fn validate(&self, _context: &mut PolicyValidationContext) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(Debug, PartialEq)]
pub(super) struct DeprecatedFilenameTransition {
    filename: SimpleArray<Vec<u8>>,
    metadata: DeprecatedFilenameTransitionMetadata,
}

impl DeprecatedFilenameTransition {
    pub(super) fn name_bytes(&self) -> &[u8] {
        &self.filename.data
    }

    pub(super) fn source_type(&self) -> TypeId {
        TypeId(NonZeroU32::new(self.metadata.source_type.get()).unwrap())
    }

    pub(super) fn target_type(&self) -> TypeId {
        TypeId(NonZeroU32::new(self.metadata.transition_type.get()).unwrap())
    }

    pub(super) fn target_class(&self) -> ClassId {
        ClassId(NonZeroU32::new(self.metadata.transition_class.get()).unwrap())
    }

    pub(super) fn out_type(&self) -> TypeId {
        TypeId(NonZeroU32::new(self.metadata.out_type.get()).unwrap())
    }
}

impl Parse for DeprecatedFilenameTransition
where
    SimpleArray<Vec<u8>>: Parse,
{
    type Error = anyhow::Error;

    fn parse(bytes: PolicyCursor) -> Result<(Self, PolicyCursor), Self::Error> {
        let tail = bytes;

        let (filename, tail) = SimpleArray::<Vec<u8>>::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing filename for deprecated filename transition")?;

        let num_bytes = tail.len();
        let (metadata, tail) = PolicyCursor::parse::<DeprecatedFilenameTransitionMetadata>(tail)
            .ok_or({
                ParseError::MissingData {
                    type_name: "DeprecatedFilenameTransition::metadata",
                    type_size: std::mem::size_of::<le::U32>(),
                    num_bytes,
                }
            })?;

        Ok((Self { filename, metadata }, tail))
    }
}

#[derive(Clone, Debug, KnownLayout, FromBytes, Immutable, PartialEq, Unaligned)]
#[repr(C, packed)]
pub(super) struct DeprecatedFilenameTransitionMetadata {
    source_type: le::U32,
    transition_type: le::U32,
    transition_class: le::U32,
    out_type: le::U32,
}

pub(super) type InitialSids = Vec<InitialSid>;

impl Validate for InitialSids {
    type Error = anyhow::Error;

    /// TODO: Validate consistency of sequence of [`InitialSid`] objects.
    fn validate(&self, _context: &mut PolicyValidationContext) -> Result<(), Self::Error> {
        for initial_sid in crate::InitialSid::all_variants() {
            self.iter()
                .find(|initial| initial.id().get() == *initial_sid as u32)
                .ok_or(ValidateError::MissingInitialSid { initial_sid: *initial_sid })?;
        }
        Ok(())
    }
}

#[derive(Debug, PartialEq)]
pub(super) struct InitialSid {
    id: le::U32,
    context: Context,
}

impl InitialSid {
    pub(super) fn id(&self) -> le::U32 {
        self.id
    }

    pub(super) fn context(&self) -> &Context {
        &self.context
    }
}

impl Parse for InitialSid
where
    Context: Parse,
{
    type Error = anyhow::Error;

    fn parse(bytes: PolicyCursor) -> Result<(Self, PolicyCursor), Self::Error> {
        let tail = bytes;

        let num_bytes = tail.len();
        let (id, tail) = PolicyCursor::parse::<le::U32>(tail).ok_or(ParseError::MissingData {
            type_name: "InitialSid::sid",
            type_size: std::mem::size_of::<le::U32>(),
            num_bytes,
        })?;

        let (context, tail) = Context::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing context for initial sid")?;

        Ok((Self { id, context }, tail))
    }
}

#[derive(Debug, PartialEq)]
pub(super) struct Context {
    metadata: ContextMetadata,
    mls_range: MlsRange,
}

impl Context {
    pub(super) fn user_id(&self) -> UserId {
        UserId(NonZeroU32::new(self.metadata.user.get()).unwrap())
    }
    pub(super) fn role_id(&self) -> RoleId {
        RoleId(NonZeroU32::new(self.metadata.role.get()).unwrap())
    }
    pub(super) fn type_id(&self) -> TypeId {
        TypeId(NonZeroU32::new(self.metadata.context_type.get()).unwrap())
    }
    pub(super) fn low_level(&self) -> &MlsLevel {
        self.mls_range.low()
    }
    pub(super) fn high_level(&self) -> &Option<MlsLevel> {
        self.mls_range.high()
    }
}

impl Parse for Context
where
    MlsRange: Parse,
{
    type Error = anyhow::Error;

    fn parse(bytes: PolicyCursor) -> Result<(Self, PolicyCursor), Self::Error> {
        let tail = bytes;

        let (metadata, tail) =
            PolicyCursor::parse::<ContextMetadata>(tail).context("parsing metadata for context")?;

        let (mls_range, tail) = MlsRange::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing mls range for context")?;

        Ok((Self { metadata, mls_range }, tail))
    }
}

#[derive(Clone, Debug, KnownLayout, FromBytes, Immutable, PartialEq, Unaligned)]
#[repr(C, packed)]
pub(super) struct ContextMetadata {
    user: le::U32,
    role: le::U32,
    context_type: le::U32,
}

pub(super) type NamedContextPairs = Vec<NamedContextPair>;

impl Validate for NamedContextPairs {
    type Error = anyhow::Error;

    /// TODO: Validate consistency of sequence of [`NamedContextPairs`] objects.
    ///
    /// TODO: Is different validation required for `filesystems` and `network_interfaces`? If so,
    /// create wrapper types with different [`Validate`] implementations.
    fn validate(&self, _context: &mut PolicyValidationContext) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(Debug, PartialEq)]
pub(super) struct NamedContextPair {
    name: SimpleArray<Vec<u8>>,
    context1: Context,
    context2: Context,
}

impl Parse for NamedContextPair
where
    SimpleArray<Vec<u8>>: Parse,
    Context: Parse,
{
    type Error = anyhow::Error;

    fn parse(bytes: PolicyCursor) -> Result<(Self, PolicyCursor), Self::Error> {
        let tail = bytes;

        let (name, tail) = SimpleArray::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing filesystem context name")?;

        let (context1, tail) = Context::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing first context for filesystem context")?;

        let (context2, tail) = Context::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing second context for filesystem context")?;

        Ok((Self { name, context1, context2 }, tail))
    }
}

pub(super) type Ports = Vec<Port>;

impl Validate for Ports {
    type Error = anyhow::Error;

    /// TODO: Validate consistency of sequence of [`Ports`] objects.
    fn validate(&self, _context: &mut PolicyValidationContext) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(Debug, PartialEq)]
pub(super) struct Port {
    metadata: PortMetadata,
    context: Context,
}

impl Parse for Port
where
    Context: Parse,
{
    type Error = anyhow::Error;

    fn parse(bytes: PolicyCursor) -> Result<(Self, PolicyCursor), Self::Error> {
        let tail = bytes;

        let (metadata, tail) =
            PolicyCursor::parse::<PortMetadata>(tail).context("parsing metadata for context")?;

        let (context, tail) = Context::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing context for port")?;

        Ok((Self { metadata, context }, tail))
    }
}

#[derive(Clone, Debug, KnownLayout, FromBytes, Immutable, PartialEq, Unaligned)]
#[repr(C, packed)]
pub(super) struct PortMetadata {
    protocol: le::U32,
    low_port: le::U32,
    high_port: le::U32,
}

pub(super) type Nodes = Vec<Node>;

impl Validate for Nodes {
    type Error = anyhow::Error;

    /// TODO: Validate consistency of sequence of [`Node`] objects.
    fn validate(&self, _context: &mut PolicyValidationContext) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(Debug, PartialEq)]
pub(super) struct Node {
    address: le::U32,
    mask: le::U32,
    context: Context,
}

impl Parse for Node
where
    Context: Parse,
{
    type Error = anyhow::Error;

    fn parse(bytes: PolicyCursor) -> Result<(Self, PolicyCursor), Self::Error> {
        let tail = bytes;

        let num_bytes = tail.len();
        let (address, tail) =
            PolicyCursor::parse::<le::U32>(tail).ok_or(ParseError::MissingData {
                type_name: "Node::address",
                type_size: std::mem::size_of::<le::U32>(),
                num_bytes,
            })?;

        let num_bytes = tail.len();
        let (mask, tail) = PolicyCursor::parse::<le::U32>(tail).ok_or(ParseError::MissingData {
            type_name: "Node::mask",
            type_size: std::mem::size_of::<le::U32>(),
            num_bytes,
        })?;

        let (context, tail) = Context::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing context for node")?;

        Ok((Self { address, mask, context }, tail))
    }
}

impl Validate for Node {
    type Error = anyhow::Error;

    /// TODO: Validate consistency between fields of [`Node`].
    fn validate(&self, _context: &mut PolicyValidationContext) -> Result<(), Self::Error> {
        Ok(())
    }
}

pub(super) type FsUses = Vec<FsUse>;

impl Validate for FsUses {
    type Error = anyhow::Error;

    fn validate(&self, context: &mut PolicyValidationContext) -> Result<(), Self::Error> {
        for fs_use in self {
            fs_use.validate(context)?;
        }
        Ok(())
    }
}

#[derive(Debug, PartialEq)]
pub(super) struct FsUse {
    behavior_and_name: Array<FsUseMetadata, Vec<u8>>,
    context: Context,
}

impl FsUse {
    pub fn fs_type(&self) -> &[u8] {
        &self.behavior_and_name.data
    }

    pub(super) fn behavior(&self) -> FsUseType {
        FsUseType::try_from(self.behavior_and_name.metadata.behavior).unwrap()
    }

    pub(super) fn context(&self) -> &Context {
        &self.context
    }
}

impl Parse for FsUse
where
    Array<FsUseMetadata, Vec<u8>>: Parse,
    Context: Parse,
{
    type Error = anyhow::Error;

    fn parse(bytes: PolicyCursor) -> Result<(Self, PolicyCursor), Self::Error> {
        let tail = bytes;

        let (behavior_and_name, tail) = Array::<FsUseMetadata, Vec<u8>>::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing fs use metadata")?;

        let (context, tail) = Context::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing context for fs use")?;

        Ok((Self { behavior_and_name, context }, tail))
    }
}

impl Validate for FsUse {
    type Error = anyhow::Error;

    fn validate(&self, _context: &mut PolicyValidationContext) -> Result<(), Self::Error> {
        FsUseType::try_from(self.behavior_and_name.metadata.behavior)?;

        Ok(())
    }
}

#[derive(Clone, Debug, KnownLayout, FromBytes, Immutable, PartialEq, Unaligned)]
#[repr(C, packed)]
pub(super) struct FsUseMetadata {
    /// The type of `fs_use` statement.
    behavior: le::U32,
    /// The length of the name in the name_and_behavior field of FsUse.
    name_length: le::U32,
}

impl Counted for FsUseMetadata {
    fn count(&self) -> u32 {
        self.name_length.get()
    }
}

/// Discriminates among the different kinds of "fs_use_*" labeling statements in the policy; see
/// https://selinuxproject.org/page/FileStatements.
#[derive(Copy, Clone, Debug, Eq, Hash, PartialEq)]
pub enum FsUseType {
    Xattr = 1,
    Trans = 2,
    Task = 3,
}

impl TryFrom<le::U32> for FsUseType {
    type Error = anyhow::Error;

    fn try_from(value: le::U32) -> Result<Self, Self::Error> {
        match value.get() {
            1 => Ok(FsUseType::Xattr),
            2 => Ok(FsUseType::Trans),
            3 => Ok(FsUseType::Task),
            _ => Err(ValidateError::InvalidFsUseType { value: value.get() }.into()),
        }
    }
}

pub(super) type IPv6Nodes = Vec<IPv6Node>;

impl Validate for IPv6Nodes {
    type Error = anyhow::Error;

    /// TODO: Validate consistency of sequence of [`IPv6Node`] objects.
    fn validate(&self, _context: &mut PolicyValidationContext) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(Debug, PartialEq)]
pub(super) struct IPv6Node {
    address: [le::U32; 4],
    mask: [le::U32; 4],
    context: Context,
}

impl Parse for IPv6Node
where
    Context: Parse,
{
    type Error = anyhow::Error;

    fn parse(bytes: PolicyCursor) -> Result<(Self, PolicyCursor), Self::Error> {
        let tail = bytes;

        let num_bytes = tail.len();
        let (address, tail) =
            PolicyCursor::parse::<[le::U32; 4]>(tail).ok_or(ParseError::MissingData {
                type_name: "IPv6Node::address",
                type_size: std::mem::size_of::<le::U32>(),
                num_bytes,
            })?;

        let num_bytes = tail.len();
        let (mask, tail) =
            PolicyCursor::parse::<[le::U32; 4]>(tail).ok_or(ParseError::MissingData {
                type_name: "IPv6Node::mask",
                type_size: std::mem::size_of::<le::U32>(),
                num_bytes,
            })?;

        let (context, tail) = Context::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing context for ipv6 node")?;

        Ok((Self { address, mask, context }, tail))
    }
}

pub(super) type InfinitiBandPartitionKeys = Vec<InfinitiBandPartitionKey>;

impl Validate for InfinitiBandPartitionKeys {
    type Error = anyhow::Error;

    /// TODO: Validate consistency of sequence of [`InfinitiBandPartitionKey`] objects.
    fn validate(&self, _context: &mut PolicyValidationContext) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(Debug, PartialEq)]
pub(super) struct InfinitiBandPartitionKey {
    low: le::U32,
    high: le::U32,
    context: Context,
}

impl Parse for InfinitiBandPartitionKey
where
    Context: Parse,
{
    type Error = anyhow::Error;

    fn parse(bytes: PolicyCursor) -> Result<(Self, PolicyCursor), Self::Error> {
        let tail = bytes;

        let num_bytes = tail.len();
        let (low, tail) = PolicyCursor::parse::<le::U32>(tail).ok_or(ParseError::MissingData {
            type_name: "InfinitiBandPartitionKey::low",
            type_size: std::mem::size_of::<le::U32>(),
            num_bytes,
        })?;

        let num_bytes = tail.len();
        let (high, tail) = PolicyCursor::parse::<le::U32>(tail).ok_or(ParseError::MissingData {
            type_name: "InfinitiBandPartitionKey::high",
            type_size: std::mem::size_of::<le::U32>(),
            num_bytes,
        })?;

        let (context, tail) = Context::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing context for infiniti band partition key")?;

        Ok((Self { low, high, context }, tail))
    }
}

impl Validate for InfinitiBandPartitionKey {
    type Error = anyhow::Error;

    /// TODO: Validate consistency between fields of [`InfinitiBandPartitionKey`].
    fn validate(&self, _context: &mut PolicyValidationContext) -> Result<(), Self::Error> {
        Ok(())
    }
}

pub(super) type InfinitiBandEndPorts = Vec<InfinitiBandEndPort>;

impl Validate for InfinitiBandEndPorts {
    type Error = anyhow::Error;

    /// TODO: Validate sequence of [`InfinitiBandEndPort`] objects.
    fn validate(&self, _context: &mut PolicyValidationContext) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(Debug, PartialEq)]
pub(super) struct InfinitiBandEndPort {
    port_and_name: Array<InfinitiBandEndPortMetadata, Vec<u8>>,
    context: Context,
}

impl Parse for InfinitiBandEndPort
where
    Array<InfinitiBandEndPortMetadata, Vec<u8>>: Parse,
    Context: Parse,
{
    type Error = anyhow::Error;

    fn parse(bytes: PolicyCursor) -> Result<(Self, PolicyCursor), Self::Error> {
        let tail = bytes;

        let (port_and_name, tail) = Array::<InfinitiBandEndPortMetadata, Vec<u8>>::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing infiniti band end port metadata")?;

        let (context, tail) = Context::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing context for infiniti band end port")?;

        Ok((Self { port_and_name, context }, tail))
    }
}

#[derive(Clone, Debug, KnownLayout, FromBytes, Immutable, PartialEq, Unaligned)]
#[repr(C, packed)]
pub(super) struct InfinitiBandEndPortMetadata {
    length: le::U32,
    port: le::U32,
}

impl Counted for InfinitiBandEndPortMetadata {
    fn count(&self) -> u32 {
        self.length.get()
    }
}

pub(super) type GenericFsContexts = Vec<GenericFsContext>;

impl Validate for GenericFsContexts {
    type Error = anyhow::Error;

    /// TODO: Validate sequence of  [`GenericFsContext`] objects.
    fn validate(&self, _context: &mut PolicyValidationContext) -> Result<(), Self::Error> {
        Ok(())
    }
}

/// Information parsed parsed from `genfscon [fs_type] [partial_path] [fs_context]` statements
/// about a specific filesystem type.
#[derive(Debug, PartialEq)]
pub(super) struct GenericFsContext {
    /// The filesystem type.
    fs_type: SimpleArray<Vec<u8>>,
    /// The set of contexts defined for this filesystem.
    contexts: SimpleArray<FsContexts>,
}

impl GenericFsContext {
    pub(super) fn fs_type(&self) -> &[u8] {
        &self.fs_type.data
    }

    pub(super) fn contexts(&self) -> &FsContexts {
        &self.contexts.data
    }
}

impl Parse for GenericFsContext
where
    SimpleArray<Vec<u8>>: Parse,
    SimpleArray<FsContexts>: Parse,
{
    type Error = anyhow::Error;

    fn parse(bytes: PolicyCursor) -> Result<(Self, PolicyCursor), Self::Error> {
        let tail = bytes;

        let (fs_type, tail) = SimpleArray::<Vec<u8>>::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing generic filesystem context name")?;

        let (contexts, tail) = SimpleArray::<FsContexts>::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing generic filesystem contexts")?;

        Ok((Self { fs_type, contexts }, tail))
    }
}

pub(super) type FsContexts = Vec<FsContext>;

#[derive(Debug, PartialEq)]
pub(super) struct FsContext {
    /// The partial path, relative to the root of the filesystem. The partial path can only be set for
    /// virtual filesystems, like `proc/`. Otherwise, this must be `/`
    partial_path: SimpleArray<Vec<u8>>,
    /// Optional. When provided, the context will only be applied to files of this type. Allowed files
    /// types are: blk_file, chr_file, dir, fifo_file, lnk_file, sock_file, file. When set to 0, the
    /// context applies to all file types.
    class: le::U32,
    /// The security context allocated to the filesystem.
    context: Context,
}

impl FsContext {
    pub(super) fn partial_path(&self) -> &[u8] {
        &self.partial_path.data
    }

    pub(super) fn context(&self) -> &Context {
        &self.context
    }

    pub(super) fn class(&self) -> Option<ClassId> {
        NonZeroU32::new(self.class.into()).map(ClassId)
    }
}

impl Parse for FsContext
where
    SimpleArray<Vec<u8>>: Parse,
    Context: Parse,
{
    type Error = anyhow::Error;

    fn parse(bytes: PolicyCursor) -> Result<(Self, PolicyCursor), Self::Error> {
        let tail = bytes;

        let (partial_path, tail) = SimpleArray::<Vec<u8>>::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing filesystem context partial path")?;

        let num_bytes = tail.len();
        let (class, tail) =
            PolicyCursor::parse::<le::U32>(tail).ok_or(ParseError::MissingData {
                type_name: "FsContext::class",
                type_size: std::mem::size_of::<le::U32>(),
                num_bytes,
            })?;

        let (context, tail) = Context::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing context for filesystem context")?;

        Ok((Self { partial_path, class, context }, tail))
    }
}

pub(super) type RangeTransitions = Vec<RangeTransition>;

impl Validate for RangeTransitions {
    type Error = anyhow::Error;

    /// TODO: Validate sequence of [`RangeTransition`] objects.
    fn validate(&self, _context: &mut PolicyValidationContext) -> Result<(), Self::Error> {
        for range_transition in self {
            if range_transition.metadata.target_class.get() == 0 {
                return Err(ValidateError::NonOptionalIdIsZero.into());
            }
        }
        Ok(())
    }
}

#[derive(Debug, PartialEq)]
pub(super) struct RangeTransition {
    metadata: RangeTransitionMetadata,
    mls_range: MlsRange,
}

impl RangeTransition {
    pub fn source_type(&self) -> TypeId {
        TypeId(NonZeroU32::new(self.metadata.source_type.get()).unwrap())
    }

    pub fn target_type(&self) -> TypeId {
        TypeId(NonZeroU32::new(self.metadata.target_type.get()).unwrap())
    }

    pub fn target_class(&self) -> ClassId {
        ClassId(NonZeroU32::new(self.metadata.target_class.get()).unwrap())
    }

    pub fn mls_range(&self) -> &MlsRange {
        &self.mls_range
    }
}

impl Parse for RangeTransition
where
    MlsRange: Parse,
{
    type Error = anyhow::Error;

    fn parse(bytes: PolicyCursor) -> Result<(Self, PolicyCursor), Self::Error> {
        let tail = bytes;

        let (metadata, tail) = PolicyCursor::parse::<RangeTransitionMetadata>(tail)
            .context("parsing range transition metadata")?;

        let (mls_range, tail) = MlsRange::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing mls range for range transition")?;

        Ok((Self { metadata, mls_range }, tail))
    }
}

#[derive(Clone, Debug, KnownLayout, FromBytes, Immutable, PartialEq, Unaligned)]
#[repr(C, packed)]
pub(super) struct RangeTransitionMetadata {
    source_type: le::U32,
    target_type: le::U32,
    target_class: le::U32,
}

#[cfg(test)]
mod tests {
    use super::super::{find_class_by_name, parse_policy_by_value};

    #[test]
    fn parse_allowxperm_one_ioctl() {
        let policy_bytes = include_bytes!("../../testdata/micro_policies/allowxperm_policy.pp");
        let policy = parse_policy_by_value(policy_bytes.to_vec()).expect("parse policy");
        let parsed_policy = &policy.0;
        parsed_policy.validate().expect("validate policy");

        let class_id = find_class_by_name(parsed_policy.classes(), "class_one_ioctl")
            .expect("look up class_one_ioctl")
            .id();

        let rules: Vec<_> = parsed_policy
            .access_vector_rules()
            .into_iter()
            .filter(|rule| rule.target_class() == class_id)
            .collect();

        assert_eq!(rules.len(), 1);
        assert!(rules[0].is_allowxperm());
        if let Some(xperms) = rules[0].extended_permissions() {
            assert_eq!(xperms.count(), 1);
            assert!(xperms.contains(0xabcd));
        } else {
            panic!("unexpected permission data type")
        }
    }

    // Extended permissions that are declared in the same rule, and have the same
    // high byte, are stored in the same `AccessVectorRule` in the compiled policy.
    #[test]
    fn parse_allowxperm_two_ioctls_same_range() {
        let policy_bytes = include_bytes!("../../testdata/micro_policies/allowxperm_policy.pp");
        let policy = parse_policy_by_value(policy_bytes.to_vec()).expect("parse policy");
        let parsed_policy = &policy.0;
        parsed_policy.validate().expect("validate policy");

        let class_id = find_class_by_name(parsed_policy.classes(), "class_two_ioctls_same_range")
            .expect("look up class_two_ioctls_same_range")
            .id();

        let rules: Vec<_> = parsed_policy
            .access_vector_rules()
            .into_iter()
            .filter(|rule| rule.target_class() == class_id)
            .collect();

        assert_eq!(rules.len(), 1);
        assert!(rules[0].is_allowxperm());
        if let Some(xperms) = rules[0].extended_permissions() {
            assert_eq!(xperms.count(), 2);
            assert!(xperms.contains(0x1234));
            assert!(xperms.contains(0x1256));
        } else {
            panic!("unexpected permission data type")
        }
    }

    // Extended permissions that are declared in the same rule, and have different
    // high bytes, are stored in different `AccessVectorRule`s in the compiled policy.
    #[test]
    fn parse_allowxperm_two_ioctls_different_range() {
        let policy_bytes = include_bytes!("../../testdata/micro_policies/allowxperm_policy.pp");
        let policy = parse_policy_by_value(policy_bytes.to_vec()).expect("parse policy");
        let parsed_policy = &policy.0;
        parsed_policy.validate().expect("validate policy");

        let class_id = find_class_by_name(parsed_policy.classes(), "class_two_ioctls_diff_range")
            .expect("look up class_two_ioctls_diff_range")
            .id();

        let rules: Vec<_> = parsed_policy
            .access_vector_rules()
            .into_iter()
            .filter(|rule| rule.target_class() == class_id)
            .collect();

        assert_eq!(rules.len(), 2);
        assert!(rules[0].is_allowxperm());
        if let Some(xperms) = rules[0].extended_permissions() {
            assert_eq!(xperms.count(), 1);
            assert!(xperms.contains(0x5678));
        } else {
            panic!("unexpected permission data type")
        }
        assert!(rules[1].is_allowxperm());
        if let Some(xperms) = rules[1].extended_permissions() {
            assert_eq!(xperms.count(), 1);
            assert!(xperms.contains(0x1234));
        } else {
            panic!("unexpected permission data type")
        }
    }

    #[test]
    fn parse_allowxperm_one_driver_range() {
        let policy_bytes = include_bytes!("../../testdata/micro_policies/allowxperm_policy.pp");
        let policy = parse_policy_by_value(policy_bytes.to_vec()).expect("parse policy");
        let parsed_policy = &policy.0;
        parsed_policy.validate().expect("validate policy");

        let class_id = find_class_by_name(parsed_policy.classes(), "class_one_driver_range")
            .expect("look up class_one_driver_range")
            .id();

        let rules: Vec<_> = parsed_policy
            .access_vector_rules()
            .into_iter()
            .filter(|rule| rule.target_class() == class_id)
            .collect();

        assert_eq!(rules.len(), 1);
        assert!(rules[0].is_allowxperm());
        if let Some(xperms) = rules[0].extended_permissions() {
            assert_eq!(xperms.count(), 0x100);
            assert!(xperms.contains(0x1000));
            assert!(xperms.contains(0x10ab));
        } else {
            panic!("unexpected permission data type")
        }
    }

    #[test]
    fn parse_allowxperm_all_ioctls() {
        let policy_bytes = include_bytes!("../../testdata/micro_policies/allowxperm_policy.pp");
        let policy = parse_policy_by_value(policy_bytes.to_vec()).expect("parse policy");
        let parsed_policy = &policy.0;
        parsed_policy.validate().expect("validate policy");

        let class_id = find_class_by_name(parsed_policy.classes(), "class_all_ioctls")
            .expect("look up class_all_ioctls")
            .id();

        let rules: Vec<_> = parsed_policy
            .access_vector_rules()
            .into_iter()
            .filter(|rule| rule.target_class() == class_id)
            .collect();

        assert_eq!(rules.len(), 1);
        assert!(rules[0].is_allowxperm());
        if let Some(xperms) = rules[0].extended_permissions() {
            assert_eq!(xperms.count(), 0x10000);
        } else {
            panic!("unexpected permission data type")
        }
    }

    // Distinct xperm rules with the same source, target, class, and permission are
    // represented by distinct `AccessVectorRule`s in the compiled policy, even if
    // they have overlapping xperm ranges.
    #[test]
    fn parse_allowxperm_overlapping_ranges() {
        let policy_bytes = include_bytes!("../../testdata/micro_policies/allowxperm_policy.pp");
        let policy = parse_policy_by_value(policy_bytes.to_vec()).expect("parse policy");
        let parsed_policy = &policy.0;
        parsed_policy.validate().expect("validate policy");

        let class_id = find_class_by_name(parsed_policy.classes(), "class_overlapping_ranges")
            .expect("look up class_overlapping_ranges")
            .id();

        let rules: Vec<_> = parsed_policy
            .access_vector_rules()
            .into_iter()
            .filter(|rule| rule.target_class() == class_id)
            .collect();

        assert_eq!(rules.len(), 2);
        assert!(rules[0].is_allowxperm());
        if let Some(xperms) = rules[0].extended_permissions() {
            assert_eq!(xperms.count(), 0x100);
            // Any ioctl in the range 0x10?? should be in the set.
            assert!(xperms.contains(0x1000));
            assert!(xperms.contains(0x10ab));
        } else {
            panic!("unexpected permission data type")
        }
        assert!(rules[1].is_allowxperm());
        if let Some(xperms) = rules[1].extended_permissions() {
            assert_eq!(xperms.count(), 2);
            assert!(xperms.contains(0x1000));
            assert!(xperms.contains(0x1001));
        } else {
            panic!("unexpected permission data type")
        }
    }

    // The representation of extended permissions for `auditallowxperm` rules is
    // the same as for `allowxperm` rules.
    #[test]
    fn parse_auditallowxperm() {
        let policy_bytes = include_bytes!("../../testdata/micro_policies/allowxperm_policy.pp");
        let policy = parse_policy_by_value(policy_bytes.to_vec()).expect("parse policy");
        let parsed_policy = &policy.0;
        parsed_policy.validate().expect("validate policy");

        let class_id = find_class_by_name(parsed_policy.classes(), "class_auditallowxperm")
            .expect("look up class_auditallowxperm")
            .id();

        let rules: Vec<_> = parsed_policy
            .access_vector_rules()
            .into_iter()
            .filter(|rule| rule.target_class() == class_id)
            .collect();

        assert_eq!(rules.len(), 1);
        assert!(rules[0].is_auditallowxperm());
        if let Some(xperms) = rules[0].extended_permissions() {
            assert_eq!(xperms.count(), 1);
            assert!(xperms.contains(0x1000));
        } else {
            panic!("unexpected permission data type")
        }
    }

    // The representation of extended permissions for `dontauditxperm` rules is
    // the same as for `allowxperm` rules. In particular, the `AccessVectorRule`
    // contains the same set of extended permissions that appears in the text
    // policy. (This differs from the representation of the access vector in
    // `AccessVectorRule`s for `dontaudit` rules, where the `AccessVectorRule`
    // contains the complement of the access vector that appears in the text
    // policy.)
    #[test]
    fn parse_dontauditxperm() {
        let policy_bytes = include_bytes!("../../testdata/micro_policies/allowxperm_policy.pp");
        let policy = parse_policy_by_value(policy_bytes.to_vec()).expect("parse policy");
        let parsed_policy = &policy.0;
        parsed_policy.validate().expect("validate policy");

        let class_id = find_class_by_name(parsed_policy.classes(), "class_dontauditxperm")
            .expect("look up class_dontauditxperm")
            .id();

        let rules: Vec<_> = parsed_policy
            .access_vector_rules()
            .into_iter()
            .filter(|rule| rule.target_class() == class_id)
            .collect();

        assert_eq!(rules.len(), 1);
        assert!(rules[0].is_dontauditxperm());
        if let Some(xperms) = rules[0].extended_permissions() {
            assert_eq!(xperms.count(), 1);
            assert!(xperms.contains(0x1000));
        } else {
            panic!("unexpected permission data type")
        }
    }
}
