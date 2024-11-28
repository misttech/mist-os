// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Special cases of `Array<Bytes, Metadata, Data>` and instances of `Metadata` and `Data` that
//! appear in binary SELinux policies.

use super::error::{ParseError, ValidateError};
use super::extensible_bitmap::ExtensibleBitmap;
use super::parser::ParseStrategy;
use super::symbols::{MlsLevel, MlsRange};
use super::{
    array_type, array_type_validate_deref_both, Array, ClassId, Counted, Parse, RoleId, TypeId,
    UserId, Validate, ValidateArray,
};

use anyhow::Context as _;
use std::fmt::Debug;
use std::num::NonZeroU32;
use zerocopy::{little_endian as le, FromBytes, Immutable, KnownLayout, Unaligned};

pub(super) const EXTENDED_PERMISSIONS_IS_SPECIFIED_DRIVER_PERMISSIONS_MASK: u16 = 0x0700;
pub(super) const MIN_POLICY_VERSION_FOR_INFINITIBAND_PARTITION_KEY: u32 = 31;

/// Mask for [`AccessVectorMetadata`] `access_vector_type` that indicates whether the access vector
/// comes from an `allow [source] [target]:[class] { [permissions] };` policy statement.
pub(super) const ACCESS_VECTOR_TYPE_ALLOW_MASK: u16 = 1;

/// Mask for [`AccessVectorMetadata`] `access_vector_type` that indicates whether the access vector
/// comes from an `auditallow [source] [target]:[class] { [permissions] };` policy statement.
pub(super) const ACCESS_VECTOR_TYPE_AUDITALLOW_MASK: u16 = 2;

/// Mask for [`AccessVectorMetadata`] `access_vector_type` that indicates whether the access vector
/// comes from a `dontaudit [source] [target]:[class] { [permissions] };` policy statement.
pub(super) const ACCESS_VECTOR_TYPE_DONTAUDIT_MASK: u16 = 4;

/// Mask for [`AccessVectorMetadata`] `access_vector_type` that indicates whether the access vector
/// comes from an `type_transition [source] [target]:[class] [new_type];` policy statement.
pub(super) const ACCESS_VECTOR_TYPE_TYPE_TRANSITION_MASK: u16 = 16;

#[allow(type_alias_bounds)]
pub(super) type SimpleArray<PS: ParseStrategy, T> = Array<PS, PS::Output<le::U32>, T>;

impl<PS: ParseStrategy, T: Validate> Validate for SimpleArray<PS, T> {
    type Error = <T as Validate>::Error;

    /// Defers to `self.data` for validation. `self.data` has access to all information, including
    /// size stored in `self.metadata`.
    fn validate(&self) -> Result<(), Self::Error> {
        self.data.validate()
    }
}

impl Counted for le::U32 {
    fn count(&self) -> u32 {
        self.get()
    }
}

pub(super) type ConditionalNodes<PS> = Vec<ConditionalNode<PS>>;

impl<PS: ParseStrategy> Validate for ConditionalNodes<PS> {
    type Error = anyhow::Error;

    /// TODO: Validate internal consistency between consecutive [`ConditionalNode`] instances.
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

array_type!(
    ConditionalNodeItems,
    PS,
    PS::Output<ConditionalNodeMetadata>,
    PS::Slice<ConditionalNodeDatum>
);

array_type_validate_deref_both!(ConditionalNodeItems);

impl<PS: ParseStrategy> ValidateArray<ConditionalNodeMetadata, ConditionalNodeDatum>
    for ConditionalNodeItems<PS>
{
    type Error = anyhow::Error;

    /// TODO: Validate internal consistency between [`ConditionalNodeMetadata`] consecutive
    /// [`ConditionalNodeDatum`].
    fn validate_array<'a>(
        _metadata: &'a ConditionalNodeMetadata,
        _data: &'a [ConditionalNodeDatum],
    ) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(Debug, PartialEq)]
pub(super) struct ConditionalNode<PS: ParseStrategy> {
    items: ConditionalNodeItems<PS>,
    true_list: SimpleArray<PS, AccessVectors<PS>>,
    false_list: SimpleArray<PS, AccessVectors<PS>>,
}

impl<PS: ParseStrategy> Parse<PS> for ConditionalNode<PS>
where
    ConditionalNodeItems<PS>: Parse<PS>,
    SimpleArray<PS, AccessVectors<PS>>: Parse<PS>,
{
    type Error = anyhow::Error;

    fn parse(bytes: PS) -> Result<(Self, PS), Self::Error> {
        let tail = bytes;

        let (items, tail) = ConditionalNodeItems::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing conditional node items")?;

        let (true_list, tail) = SimpleArray::<PS, AccessVectors<PS>>::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing conditional node true list")?;

        let (false_list, tail) = SimpleArray::<PS, AccessVectors<PS>>::parse(tail)
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
    fn validate(&self) -> Result<(), Self::Error> {
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
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

pub(super) type AccessVectors<PS> = Vec<AccessVector<PS>>;

impl<PS: ParseStrategy> Validate for AccessVectors<PS> {
    type Error = anyhow::Error;

    fn validate(&self) -> Result<(), Self::Error> {
        for access_vector in self {
            access_vector.validate()?;
        }
        Ok(())
    }
}

#[derive(Debug, PartialEq)]
pub(super) struct AccessVector<PS: ParseStrategy> {
    pub metadata: PS::Output<AccessVectorMetadata>,
    extended_permissions: ExtendedPermissions<PS>,
}

impl<PS: ParseStrategy> AccessVector<PS> {
    /// Returns whether this access vector comes from an
    /// `allow [source] [target]:[class] { [permissions] };` policy statement.
    pub fn is_allow(&self) -> bool {
        PS::deref(&self.metadata).is_allow()
    }

    /// Returns whether this access vector comes from an
    /// `auditallow [source] [target]:[class] { [permissions] };` policy statement.
    pub fn is_auditallow(&self) -> bool {
        PS::deref(&self.metadata).is_auditallow()
    }

    /// Returns whether this access vector comes from an
    /// `dontaudit [source] [target]:[class] { [permissions] };` policy statement.
    pub fn is_dontaudit(&self) -> bool {
        PS::deref(&self.metadata).is_dontaudit()
    }

    /// Returns whether this access vector compes from a
    /// `type_transtion [source] [target]:[class] [new_type];` policy statement.
    pub fn is_type_transition(&self) -> bool {
        PS::deref(&self.metadata).is_type_transition()
    }

    /// Returns the source type id in this access vector. This id corresponds to the
    /// [`super::symbols::Type`] `id()` of some type or attribute in the same policy.
    pub fn source_type(&self) -> TypeId {
        TypeId(NonZeroU32::new(PS::deref(&self.metadata).source_type.into()).unwrap())
    }

    /// Returns the target type id in this access vector. This id corresponds to the
    /// [`super::symbols::Type`] `id()` of some type or attribute in the same policy.
    pub fn target_type(&self) -> TypeId {
        TypeId(NonZeroU32::new(PS::deref(&self.metadata).target_type.into()).unwrap())
    }

    /// Returns the target class id in this access vector. This id corresponds to the
    /// [`super::symbols::Class`] `id()` of some class in the same policy.
    /// Although the index is returned as a 32-bit value, the field itself is 16-bit
    pub fn target_class(&self) -> ClassId {
        ClassId(NonZeroU32::new(PS::deref(&self.metadata).class.into()).unwrap())
    }

    /// A bit mask that corresponds to the permissions in this access vector. Permission bits are
    /// specified using a [`super::symbols::Permission`] `id()` as follows:
    /// `1 << (Permission::id() - 1)`.
    pub fn permission_mask(&self) -> Option<le::U32> {
        match &self.extended_permissions {
            ExtendedPermissions::PermissionMask(mask) => Some(*PS::deref(mask)),
            _ => None,
        }
    }

    /// A numeric type id that corresponds to a the `[new_type]` in a
    /// `type_transition [source] [target]:[class] [new_type];` policy statement.
    pub fn new_type(&self) -> Option<TypeId> {
        match &self.extended_permissions {
            ExtendedPermissions::NewType(new_type) => {
                Some(TypeId(NonZeroU32::new(PS::deref(new_type).get().into()).unwrap()))
            }
            _ => None,
        }
    }
}

impl<PS: ParseStrategy> Parse<PS> for AccessVector<PS> {
    type Error = anyhow::Error;

    fn parse(bytes: PS) -> Result<(Self, PS), Self::Error> {
        let tail = bytes;

        let num_bytes = tail.len();
        let (metadata, tail) =
            PS::parse::<AccessVectorMetadata>(tail).ok_or(ParseError::MissingData {
                type_name: std::any::type_name::<AccessVectorMetadata>(),
                type_size: std::mem::size_of::<AccessVectorMetadata>(),
                num_bytes,
            })?;

        let access_vector_type = PS::deref(&metadata).access_vector_type();
        let (extended_permissions, tail) = if (access_vector_type
            & EXTENDED_PERMISSIONS_IS_SPECIFIED_DRIVER_PERMISSIONS_MASK)
            != 0
        {
            let num_bytes = tail.len();
            let (specified_driver_permissions, tail) =
                PS::parse::<SpecifiedDriverPermissions>(tail).ok_or(ParseError::MissingData {
                    type_name: std::any::type_name::<SpecifiedDriverPermissions>(),
                    type_size: std::mem::size_of::<SpecifiedDriverPermissions>(),
                    num_bytes,
                })?;
            (ExtendedPermissions::SpecifiedDriverPermissions(specified_driver_permissions), tail)
        } else {
            let num_bytes = tail.len();
            let (extended_permissions, tail) =
                PS::parse::<le::U32>(tail).ok_or(ParseError::MissingData {
                    type_name: "ExtendedPermissions::PermissionMask",
                    type_size: std::mem::size_of::<le::U32>(),
                    num_bytes,
                })?;
            if (access_vector_type & ACCESS_VECTOR_TYPE_TYPE_TRANSITION_MASK) != 0 {
                (ExtendedPermissions::NewType(extended_permissions), tail)
            } else {
                (ExtendedPermissions::PermissionMask(extended_permissions), tail)
            }
        };

        Ok((Self { metadata, extended_permissions }, tail))
    }
}

impl<PS: ParseStrategy> Validate for AccessVector<PS> {
    type Error = anyhow::Error;

    fn validate(&self) -> Result<(), Self::Error> {
        if PS::deref(&self.metadata).class.get() == 0 {
            Err(ValidateError::NonOptionalIdIsZero.into())
        } else {
            Ok(())
        }
    }
}

#[derive(Clone, Debug, KnownLayout, FromBytes, Immutable, PartialEq, Unaligned)]
#[repr(C, packed)]
pub(super) struct AccessVectorMetadata {
    source_type: le::U16,
    target_type: le::U16,
    class: le::U16,
    access_vector_type: le::U16,
}

impl AccessVectorMetadata {
    /// Returns the access vector type field that indicates the type of policy statement this
    /// access vector comes from; for example `allow ...;`, `auditallow ...;`.
    pub fn access_vector_type(&self) -> u16 {
        self.access_vector_type.get()
    }

    /// Returns whether this access vector comes from an
    /// `allow [source] [target]:[class] { [permissions] };` policy statement.
    pub fn is_allow(&self) -> bool {
        (self.access_vector_type() & ACCESS_VECTOR_TYPE_ALLOW_MASK) != 0
    }

    /// Returns whether this access vector comes from an
    /// `auditallow [source] [target]:[class] { [permissions] };` policy statement.
    pub fn is_auditallow(&self) -> bool {
        (self.access_vector_type() & ACCESS_VECTOR_TYPE_AUDITALLOW_MASK) != 0
    }

    /// Returns whether this access vector comes from an
    /// `dontaudit [source] [target]:[class] { [permissions] };` policy statement.
    pub fn is_dontaudit(&self) -> bool {
        (self.access_vector_type() & ACCESS_VECTOR_TYPE_DONTAUDIT_MASK) != 0
    }

    /// Returns whether this access vector compes from a
    /// `type_transtion [source] [target]:[class] [new_type];` policy statement.
    pub fn is_type_transition(&self) -> bool {
        (self.access_vector_type() & ACCESS_VECTOR_TYPE_TYPE_TRANSITION_MASK) != 0
    }
}

#[derive(Debug, PartialEq)]
pub(super) enum ExtendedPermissions<PS: ParseStrategy> {
    SpecifiedDriverPermissions(PS::Output<SpecifiedDriverPermissions>),
    PermissionMask(PS::Output<le::U32>),
    NewType(PS::Output<le::U32>),
}

#[derive(Clone, Debug, KnownLayout, FromBytes, Immutable, PartialEq, Unaligned)]
#[repr(C, packed)]
pub(super) struct SpecifiedDriverPermissions {
    specified: u8,
    driver: u8,
    permissions: [le::U32; 8],
}

array_type!(RoleTransitions, PS, PS::Output<le::U32>, PS::Slice<RoleTransition>);

array_type_validate_deref_both!(RoleTransitions);

impl<PS: ParseStrategy> ValidateArray<le::U32, RoleTransition> for RoleTransitions<PS> {
    type Error = anyhow::Error;

    /// [`RoleTransitions`] have no additional metadata (beyond length encoding).
    fn validate_array<'a>(
        _metadata: &'a le::U32,
        _data: &'a [RoleTransition],
    ) -> Result<(), Self::Error> {
        _data.validate()
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

    fn validate(&self) -> Result<(), Self::Error> {
        for role_transition in self {
            if role_transition.tclass.get() == 0 {
                return Err(ValidateError::NonOptionalIdIsZero.into());
            }
        }
        Ok(())
    }
}

array_type!(RoleAllows, PS, PS::Output<le::U32>, PS::Slice<RoleAllow>);

array_type_validate_deref_both!(RoleAllows);

impl<PS: ParseStrategy> ValidateArray<le::U32, RoleAllow> for RoleAllows<PS> {
    type Error = anyhow::Error;

    /// [`RoleAllows`] have no additional metadata (beyond length encoding).
    fn validate_array<'a>(
        _metadata: &'a le::U32,
        _data: &'a [RoleAllow],
    ) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(Clone, Debug, KnownLayout, FromBytes, Immutable, PartialEq, Unaligned)]
#[repr(C, packed)]
pub(super) struct RoleAllow {
    role: le::U32,
    new_role: le::U32,
}

impl RoleAllow {
    #[allow(dead_code)]
    // TODO(http://b/334968228): fn to be used again when checking role allow rules separately from
    // SID calculation.
    pub(super) fn source_role(&self) -> RoleId {
        RoleId(NonZeroU32::new(self.role.get()).unwrap())
    }

    #[allow(dead_code)]
    // TODO(http://b/334968228): fn to be used again when checking role allow rules separately from
    // SID calculation.
    pub(super) fn new_role(&self) -> RoleId {
        RoleId(NonZeroU32::new(self.new_role.get()).unwrap())
    }
}

impl Validate for [RoleAllow] {
    type Error = anyhow::Error;

    /// TODO: Validate sequence of [`RoleAllow`].
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(Debug, PartialEq)]
pub(super) enum FilenameTransitionList<PS: ParseStrategy> {
    PolicyVersionGeq33(SimpleArray<PS, FilenameTransitions<PS>>),
    PolicyVersionLeq32(SimpleArray<PS, DeprecatedFilenameTransitions<PS>>),
}

impl<PS: ParseStrategy> Validate for FilenameTransitionList<PS> {
    type Error = anyhow::Error;

    fn validate(&self) -> Result<(), Self::Error> {
        match self {
            Self::PolicyVersionLeq32(list) => list.validate().map_err(Into::<anyhow::Error>::into),
            Self::PolicyVersionGeq33(list) => list.validate().map_err(Into::<anyhow::Error>::into),
        }
    }
}

pub(super) type FilenameTransitions<PS> = Vec<FilenameTransition<PS>>;

impl<PS: ParseStrategy> Validate for FilenameTransitions<PS> {
    type Error = anyhow::Error;

    /// TODO: Validate sequence of [`FilenameTransition`] objects.
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(Debug, PartialEq)]
pub(super) struct FilenameTransition<PS: ParseStrategy> {
    filename: SimpleArray<PS, PS::Slice<u8>>,
    transition_type: PS::Output<le::U32>,
    transition_class: PS::Output<le::U32>,
    items: SimpleArray<PS, FilenameTransitionItems<PS>>,
}

impl<PS: ParseStrategy> Parse<PS> for FilenameTransition<PS>
where
    SimpleArray<PS, PS::Slice<u8>>: Parse<PS>,
    SimpleArray<PS, FilenameTransitionItems<PS>>: Parse<PS>,
{
    type Error = anyhow::Error;

    fn parse(bytes: PS) -> Result<(Self, PS), Self::Error> {
        let tail = bytes;

        let (filename, tail) = SimpleArray::<PS, PS::Slice<u8>>::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing filename for filename transition")?;

        let num_bytes = tail.len();
        let (transition_type, tail) =
            PS::parse::<le::U32>(tail).ok_or(ParseError::MissingData {
                type_name: "FilenameTransition::transition_type",
                type_size: std::mem::size_of::<le::U32>(),
                num_bytes,
            })?;

        let num_bytes = tail.len();
        let (transition_class, tail) =
            PS::parse::<le::U32>(tail).ok_or(ParseError::MissingData {
                type_name: "FilenameTransition::transition_class",
                type_size: std::mem::size_of::<le::U32>(),
                num_bytes,
            })?;

        let (items, tail) = SimpleArray::<PS, FilenameTransitionItems<PS>>::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing items for filename transition")?;

        Ok((Self { filename, transition_type, transition_class, items }, tail))
    }
}

pub(super) type FilenameTransitionItems<PS> = Vec<FilenameTransitionItem<PS>>;

#[derive(Debug, PartialEq)]
pub(super) struct FilenameTransitionItem<PS: ParseStrategy> {
    stypes: ExtensibleBitmap<PS>,
    out_type: PS::Output<le::U32>,
}

impl<PS: ParseStrategy> Parse<PS> for FilenameTransitionItem<PS>
where
    ExtensibleBitmap<PS>: Parse<PS>,
{
    type Error = anyhow::Error;

    fn parse(bytes: PS) -> Result<(Self, PS), Self::Error> {
        let tail = bytes;

        let (stypes, tail) = ExtensibleBitmap::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing stypes extensible bitmap for file transition")?;

        let num_bytes = tail.len();
        let (out_type, tail) = PS::parse::<le::U32>(tail).ok_or(ParseError::MissingData {
            type_name: "FilenameTransitionItem::out_type",
            type_size: std::mem::size_of::<le::U32>(),
            num_bytes,
        })?;

        Ok((Self { stypes, out_type }, tail))
    }
}

pub(super) type DeprecatedFilenameTransitions<PS> = Vec<DeprecatedFilenameTransition<PS>>;

impl<PS: ParseStrategy> Validate for DeprecatedFilenameTransitions<PS> {
    type Error = anyhow::Error;

    /// TODO: Validate sequence of [`DeprecatedFilenameTransition`] objects.
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(Debug, PartialEq)]
pub(super) struct DeprecatedFilenameTransition<PS: ParseStrategy> {
    filename: SimpleArray<PS, PS::Slice<u8>>,
    metadata: PS::Output<DeprecatedFilenameTransitionMetadata>,
}

impl<PS: ParseStrategy> Parse<PS> for DeprecatedFilenameTransition<PS>
where
    SimpleArray<PS, PS::Slice<u8>>: Parse<PS>,
{
    type Error = anyhow::Error;

    fn parse(bytes: PS) -> Result<(Self, PS), Self::Error> {
        let tail = bytes;

        let (filename, tail) = SimpleArray::<PS, PS::Slice<u8>>::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing filename for deprecated filename transition")?;

        let num_bytes = tail.len();
        let (metadata, tail) = PS::parse::<DeprecatedFilenameTransitionMetadata>(tail).ok_or(
            ParseError::MissingData {
                type_name: "DeprecatedFilenameTransition::metadata",
                type_size: std::mem::size_of::<le::U32>(),
                num_bytes,
            },
        )?;

        Ok((Self { filename, metadata }, tail))
    }
}

#[derive(Clone, Debug, KnownLayout, FromBytes, Immutable, PartialEq, Unaligned)]
#[repr(C, packed)]
pub(super) struct DeprecatedFilenameTransitionMetadata {
    bit: le::U32,
    transition_type: le::U32,
    transition_class: le::U32,
    old_type: le::U32,
}

pub(super) type InitialSids<PS> = Vec<InitialSid<PS>>;

impl<PS: ParseStrategy> Validate for InitialSids<PS> {
    type Error = anyhow::Error;

    /// TODO: Validate consistency of sequence of [`InitialSid`] objects.
    fn validate(&self) -> Result<(), Self::Error> {
        for initial_sid in crate::InitialSid::all_variants() {
            self.iter()
                .find(|initial| initial.id().get() == initial_sid as u32)
                .ok_or(ValidateError::MissingInitialSid { initial_sid })?;
        }
        Ok(())
    }
}

#[derive(Debug, PartialEq)]
pub(super) struct InitialSid<PS: ParseStrategy> {
    id: PS::Output<le::U32>,
    context: Context<PS>,
}

impl<PS: ParseStrategy> InitialSid<PS> {
    pub(super) fn id(&self) -> le::U32 {
        *PS::deref(&self.id)
    }

    pub(super) fn context(&self) -> &Context<PS> {
        &self.context
    }
}

impl<PS: ParseStrategy> Parse<PS> for InitialSid<PS>
where
    Context<PS>: Parse<PS>,
{
    type Error = anyhow::Error;

    fn parse(bytes: PS) -> Result<(Self, PS), Self::Error> {
        let tail = bytes;

        let num_bytes = tail.len();
        let (id, tail) = PS::parse::<le::U32>(tail).ok_or(ParseError::MissingData {
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
pub(super) struct Context<PS: ParseStrategy> {
    metadata: PS::Output<ContextMetadata>,
    mls_range: MlsRange<PS>,
}

impl<PS: ParseStrategy> Context<PS> {
    pub(super) fn user_id(&self) -> UserId {
        UserId(NonZeroU32::new(PS::deref(&self.metadata).user.get()).unwrap())
    }
    pub(super) fn role_id(&self) -> RoleId {
        RoleId(NonZeroU32::new(PS::deref(&self.metadata).role.get()).unwrap())
    }
    pub(super) fn type_id(&self) -> TypeId {
        TypeId(NonZeroU32::new(PS::deref(&self.metadata).context_type.get()).unwrap())
    }
    pub(super) fn low_level(&self) -> &MlsLevel<PS> {
        self.mls_range.low()
    }
    pub(super) fn high_level(&self) -> &Option<MlsLevel<PS>> {
        self.mls_range.high()
    }
}

impl<PS: ParseStrategy> Parse<PS> for Context<PS>
where
    MlsRange<PS>: Parse<PS>,
{
    type Error = anyhow::Error;

    fn parse(bytes: PS) -> Result<(Self, PS), Self::Error> {
        let tail = bytes;

        let (metadata, tail) =
            PS::parse::<ContextMetadata>(tail).context("parsing metadata for context")?;

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

pub(super) type NamedContextPairs<PS> = Vec<NamedContextPair<PS>>;

impl<PS: ParseStrategy> Validate for NamedContextPairs<PS> {
    type Error = anyhow::Error;

    /// TODO: Validate consistency of sequence of [`NamedContextPairs`] objects.
    ///
    /// TODO: Is different validation required for `filesystems` and `network_interfaces`? If so,
    /// create wrapper types with different [`Validate`] implementations.
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(Debug, PartialEq)]
pub(super) struct NamedContextPair<PS: ParseStrategy> {
    name: SimpleArray<PS, PS::Slice<u8>>,
    context1: Context<PS>,
    context2: Context<PS>,
}

impl<PS: ParseStrategy> Parse<PS> for NamedContextPair<PS>
where
    SimpleArray<PS, PS::Slice<u8>>: Parse<PS>,
    Context<PS>: Parse<PS>,
{
    type Error = anyhow::Error;

    fn parse(bytes: PS) -> Result<(Self, PS), Self::Error> {
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

pub(super) type Ports<PS> = Vec<Port<PS>>;

impl<PS: ParseStrategy> Validate for Ports<PS> {
    type Error = anyhow::Error;

    /// TODO: Validate consistency of sequence of [`Ports`] objects.
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(Debug, PartialEq)]
pub(super) struct Port<PS: ParseStrategy> {
    metadata: PS::Output<PortMetadata>,
    context: Context<PS>,
}

impl<PS: ParseStrategy> Parse<PS> for Port<PS>
where
    Context<PS>: Parse<PS>,
{
    type Error = anyhow::Error;

    fn parse(bytes: PS) -> Result<(Self, PS), Self::Error> {
        let tail = bytes;

        let (metadata, tail) =
            PS::parse::<PortMetadata>(tail).context("parsing metadata for context")?;

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

pub(super) type Nodes<PS> = Vec<Node<PS>>;

impl<PS: ParseStrategy> Validate for Nodes<PS> {
    type Error = anyhow::Error;

    /// TODO: Validate consistency of sequence of [`Node`] objects.
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(Debug, PartialEq)]
pub(super) struct Node<PS: ParseStrategy> {
    address: PS::Output<le::U32>,
    mask: PS::Output<le::U32>,
    context: Context<PS>,
}

impl<PS: ParseStrategy> Parse<PS> for Node<PS>
where
    Context<PS>: Parse<PS>,
{
    type Error = anyhow::Error;

    fn parse(bytes: PS) -> Result<(Self, PS), Self::Error> {
        let tail = bytes;

        let num_bytes = tail.len();
        let (address, tail) = PS::parse::<le::U32>(tail).ok_or(ParseError::MissingData {
            type_name: "Node::address",
            type_size: std::mem::size_of::<le::U32>(),
            num_bytes,
        })?;

        let num_bytes = tail.len();
        let (mask, tail) = PS::parse::<le::U32>(tail).ok_or(ParseError::MissingData {
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

impl<PS: ParseStrategy> Validate for Node<PS> {
    type Error = anyhow::Error;

    /// TODO: Validate consistency between fields of [`Node`].
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

pub(super) type FsUses<PS> = Vec<FsUse<PS>>;

impl<PS: ParseStrategy> Validate for FsUses<PS> {
    type Error = anyhow::Error;

    fn validate(&self) -> Result<(), Self::Error> {
        for fs_use in self {
            fs_use.validate()?;
        }
        Ok(())
    }
}

#[derive(Debug, PartialEq)]
pub(super) struct FsUse<PS: ParseStrategy> {
    behavior_and_name: Array<PS, PS::Output<FsUseMetadata>, PS::Slice<u8>>,
    context: Context<PS>,
}

impl<PS: ParseStrategy> FsUse<PS> {
    pub fn fs_type(&self) -> &[u8] {
        PS::deref_slice(&self.behavior_and_name.data)
    }

    pub(super) fn behavior(&self) -> FsUseType {
        FsUseType::try_from(PS::deref(&self.behavior_and_name.metadata).behavior).unwrap()
    }

    pub(super) fn context(&self) -> &Context<PS> {
        &self.context
    }
}

impl<PS: ParseStrategy> Parse<PS> for FsUse<PS>
where
    Array<PS, PS::Output<FsUseMetadata>, PS::Slice<u8>>: Parse<PS>,
    Context<PS>: Parse<PS>,
{
    type Error = anyhow::Error;

    fn parse(bytes: PS) -> Result<(Self, PS), Self::Error> {
        let tail = bytes;

        let (behavior_and_name, tail) =
            Array::<PS, PS::Output<FsUseMetadata>, PS::Slice<u8>>::parse(tail)
                .map_err(Into::<anyhow::Error>::into)
                .context("parsing fs use metadata")?;

        let (context, tail) = Context::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing context for fs use")?;

        Ok((Self { behavior_and_name, context }, tail))
    }
}

impl<PS: ParseStrategy> Validate for FsUse<PS> {
    type Error = anyhow::Error;

    fn validate(&self) -> Result<(), Self::Error> {
        FsUseType::try_from(PS::deref(&self.behavior_and_name.metadata).behavior)?;

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

pub(super) type IPv6Nodes<PS> = Vec<IPv6Node<PS>>;

impl<PS: ParseStrategy> Validate for IPv6Nodes<PS> {
    type Error = anyhow::Error;

    /// TODO: Validate consistency of sequence of [`IPv6Node`] objects.
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(Debug, PartialEq)]
pub(super) struct IPv6Node<PS: ParseStrategy> {
    address: PS::Output<[le::U32; 4]>,
    mask: PS::Output<[le::U32; 4]>,
    context: Context<PS>,
}

impl<PS: ParseStrategy> Parse<PS> for IPv6Node<PS>
where
    Context<PS>: Parse<PS>,
{
    type Error = anyhow::Error;

    fn parse(bytes: PS) -> Result<(Self, PS), Self::Error> {
        let tail = bytes;

        let num_bytes = tail.len();
        let (address, tail) = PS::parse::<[le::U32; 4]>(tail).ok_or(ParseError::MissingData {
            type_name: "IPv6Node::address",
            type_size: std::mem::size_of::<le::U32>(),
            num_bytes,
        })?;

        let num_bytes = tail.len();
        let (mask, tail) = PS::parse::<[le::U32; 4]>(tail).ok_or(ParseError::MissingData {
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

pub(super) type InfinitiBandPartitionKeys<PS> = Vec<InfinitiBandPartitionKey<PS>>;

impl<PS: ParseStrategy> Validate for InfinitiBandPartitionKeys<PS> {
    type Error = anyhow::Error;

    /// TODO: Validate consistency of sequence of [`InfinitiBandPartitionKey`] objects.
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(Debug, PartialEq)]
pub(super) struct InfinitiBandPartitionKey<PS: ParseStrategy> {
    low: PS::Output<le::U32>,
    high: PS::Output<le::U32>,
    context: Context<PS>,
}

impl<PS: ParseStrategy> Parse<PS> for InfinitiBandPartitionKey<PS>
where
    Context<PS>: Parse<PS>,
{
    type Error = anyhow::Error;

    fn parse(bytes: PS) -> Result<(Self, PS), Self::Error> {
        let tail = bytes;

        let num_bytes = tail.len();
        let (low, tail) = PS::parse::<le::U32>(tail).ok_or(ParseError::MissingData {
            type_name: "InfinitiBandPartitionKey::low",
            type_size: std::mem::size_of::<le::U32>(),
            num_bytes,
        })?;

        let num_bytes = tail.len();
        let (high, tail) = PS::parse::<le::U32>(tail).ok_or(ParseError::MissingData {
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

impl<PS: ParseStrategy> Validate for InfinitiBandPartitionKey<PS> {
    type Error = anyhow::Error;

    /// TODO: Validate consistency between fields of [`InfinitiBandPartitionKey`].
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

pub(super) type InfinitiBandEndPorts<PS> = Vec<InfinitiBandEndPort<PS>>;

impl<PS: ParseStrategy> Validate for InfinitiBandEndPorts<PS> {
    type Error = anyhow::Error;

    /// TODO: Validate sequence of [`InfinitiBandEndPort`] objects.
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(Debug, PartialEq)]
pub(super) struct InfinitiBandEndPort<PS: ParseStrategy> {
    port_and_name: Array<PS, PS::Output<InfinitiBandEndPortMetadata>, PS::Slice<u8>>,
    context: Context<PS>,
}

impl<PS: ParseStrategy> Parse<PS> for InfinitiBandEndPort<PS>
where
    Array<PS, PS::Output<InfinitiBandEndPortMetadata>, PS::Slice<u8>>: Parse<PS>,
    Context<PS>: Parse<PS>,
{
    type Error = anyhow::Error;

    fn parse(bytes: PS) -> Result<(Self, PS), Self::Error> {
        let tail = bytes;

        let (port_and_name, tail) =
            Array::<PS, PS::Output<InfinitiBandEndPortMetadata>, PS::Slice<u8>>::parse(tail)
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

pub(super) type GenericFsContexts<PS> = Vec<GenericFsContext<PS>>;

impl<PS: ParseStrategy> Validate for GenericFsContexts<PS> {
    type Error = anyhow::Error;

    /// TODO: Validate sequence of  [`GenericFsContext`] objects.
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

/// Information parsed parsed from `genfscon [fs_type] [partial_path] [fs_context]` statements
/// about a specific filesystem type.
#[derive(Debug, PartialEq)]
pub(super) struct GenericFsContext<PS: ParseStrategy> {
    /// The filesystem type.
    fs_type: SimpleArray<PS, PS::Slice<u8>>,
    /// The set of contexts defined for this filesystem.
    contexts: SimpleArray<PS, FsContexts<PS>>,
}

impl<PS: ParseStrategy> GenericFsContext<PS> {
    pub(super) fn fs_type(&self) -> &[u8] {
        PS::deref_slice(&self.fs_type.data)
    }

    pub(super) fn contexts(&self) -> &FsContexts<PS> {
        &self.contexts.data
    }
}

impl<PS: ParseStrategy> Parse<PS> for GenericFsContext<PS>
where
    SimpleArray<PS, PS::Slice<u8>>: Parse<PS>,
    SimpleArray<PS, FsContexts<PS>>: Parse<PS>,
{
    type Error = anyhow::Error;

    fn parse(bytes: PS) -> Result<(Self, PS), Self::Error> {
        let tail = bytes;

        let (fs_type, tail) = SimpleArray::<PS, PS::Slice<u8>>::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing generic filesystem context name")?;

        let (contexts, tail) = SimpleArray::<PS, FsContexts<PS>>::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing generic filesystem contexts")?;

        Ok((Self { fs_type, contexts }, tail))
    }
}

pub(super) type FsContexts<PS> = Vec<FsContext<PS>>;

#[derive(Debug, PartialEq)]
pub(super) struct FsContext<PS: ParseStrategy> {
    /// The partial path, relative to the root of the filesystem. The partial path can only be set for
    /// virtual filesystems, like `proc/`. Otherwise, this must be `/`
    partial_path: SimpleArray<PS, PS::Slice<u8>>,
    /// Optional. When provided, the context will only be applied to files of this type. Allowed files
    /// types are: blk_file, chr_file, dir, fifo_file, lnk_file, sock_file, file. When set to 0, the
    /// context applies to all file types.
    class: PS::Output<le::U32>,
    /// The security context allocated to the filesystem.
    context: Context<PS>,
}

impl<PS: ParseStrategy> FsContext<PS> {
    pub(super) fn partial_path(&self) -> &[u8] {
        PS::deref_slice(&self.partial_path.data)
    }

    pub(super) fn context(&self) -> &Context<PS> {
        &self.context
    }

    pub(super) fn class(&self) -> Option<ClassId> {
        NonZeroU32::new((*PS::deref(&self.class)).into()).map(ClassId)
    }
}

impl<PS: ParseStrategy> Parse<PS> for FsContext<PS>
where
    SimpleArray<PS, PS::Slice<u8>>: Parse<PS>,
    Context<PS>: Parse<PS>,
{
    type Error = anyhow::Error;

    fn parse(bytes: PS) -> Result<(Self, PS), Self::Error> {
        let tail = bytes;

        let (partial_path, tail) = SimpleArray::<PS, PS::Slice<u8>>::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing filesystem context partial path")?;

        let num_bytes = tail.len();
        let (class, tail) = PS::parse::<le::U32>(tail).ok_or(ParseError::MissingData {
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

pub(super) type RangeTransitions<PS> = Vec<RangeTransition<PS>>;

impl<PS: ParseStrategy> Validate for RangeTransitions<PS> {
    type Error = anyhow::Error;

    /// TODO: Validate sequence of [`RangeTransition`] objects.
    fn validate(&self) -> Result<(), Self::Error> {
        for range_transition in self {
            if PS::deref(&range_transition.metadata).target_class.get() == 0 {
                return Err(ValidateError::NonOptionalIdIsZero.into());
            }
        }
        Ok(())
    }
}

#[derive(Debug, PartialEq)]
pub(super) struct RangeTransition<PS: ParseStrategy> {
    metadata: PS::Output<RangeTransitionMetadata>,
    mls_range: MlsRange<PS>,
}

impl<PS: ParseStrategy> RangeTransition<PS> {
    pub fn source_type(&self) -> TypeId {
        TypeId(NonZeroU32::new(PS::deref(&self.metadata).source_type.get()).unwrap())
    }

    pub fn target_type(&self) -> TypeId {
        TypeId(NonZeroU32::new(PS::deref(&self.metadata).target_type.get()).unwrap())
    }

    pub fn target_class(&self) -> ClassId {
        ClassId(NonZeroU32::new(PS::deref(&self.metadata).target_class.get()).unwrap())
    }

    pub fn mls_range(&self) -> &MlsRange<PS> {
        &self.mls_range
    }
}

impl<PS: ParseStrategy> Parse<PS> for RangeTransition<PS>
where
    MlsRange<PS>: Parse<PS>,
{
    type Error = anyhow::Error;

    fn parse(bytes: PS) -> Result<(Self, PS), Self::Error> {
        let tail = bytes;

        let (metadata, tail) = PS::parse::<RangeTransitionMetadata>(tail)
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
