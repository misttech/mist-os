// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::{
    array_type, array_type_validate_deref_both, array_type_validate_deref_data,
    array_type_validate_deref_metadata_data_vec, array_type_validate_deref_none_data_vec,
    error::ParseError, extensible_bitmap::ExtensibleBitmap, parser::ParseStrategy, Array, Counted,
    Parse, ParseSlice, Validate, ValidateArray,
};

use anyhow::Context as _;
use std::{fmt::Debug, ops::Deref};
use zerocopy::{little_endian as le, FromBytes, FromZeroes, NoCell, Unaligned};

/// The `type` field value for a [`Constraint`] that contains an [`ExtensibleBitmap`] and
/// [`TypeSet`].
const CONSTRAINT_TYPE_HAS_EXTENSIBLE_BITMAP_AND_TYPE_SET: u32 = 5;

/// [`SymbolList`] is an [`Array`] of items with the count of items determined by [`Metadata`] as
/// [`Counted`].
#[derive(Debug, PartialEq)]
pub(crate) struct SymbolList<PS: ParseStrategy, T>(Array<PS, PS::Output<Metadata>, Vec<T>>);

impl<PS: ParseStrategy, T> Deref for SymbolList<PS, T> {
    type Target = Array<PS, PS::Output<Metadata>, Vec<T>>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<PS: ParseStrategy, T> Parse<PS> for SymbolList<PS, T>
where
    Array<PS, PS::Output<Metadata>, Vec<T>>: Parse<PS>,
{
    type Error = <Array<PS, PS::Output<Metadata>, Vec<T>> as Parse<PS>>::Error;

    fn parse(bytes: PS) -> Result<(Self, PS), Self::Error> {
        let (array, tail) = Array::<PS, PS::Output<Metadata>, Vec<T>>::parse(bytes)?;
        Ok((Self(array), tail))
    }
}

impl<PS: ParseStrategy, T> Validate for SymbolList<PS, T>
where
    [T]: Validate,
{
    type Error = anyhow::Error;

    /// [`SymbolList`] has no internal constraints beyond those imposed by [`Array`].
    fn validate(&self) -> Result<(), Self::Error> {
        PS::deref(&self.metadata).validate().map_err(Into::<anyhow::Error>::into)?;
        self.data.as_slice().validate().map_err(Into::<anyhow::Error>::into)?;

        Ok(())
    }
}

/// Binary metadata prefix to [`SymbolList`] objects.
#[derive(Clone, Debug, FromZeroes, FromBytes, NoCell, PartialEq, Unaligned)]
#[repr(C, packed)]
pub(crate) struct Metadata {
    /// The number of primary names referred to in the associated [`SymbolList`].
    primary_names_count: le::U32,
    /// The number of objects in the associated [`SymbolList`] [`Array`].
    count: le::U32,
}

impl Metadata {
    pub fn primary_names_count(&self) -> u32 {
        self.primary_names_count.get()
    }
}

impl Counted for Metadata {
    /// The number of items that follow a [`Metadata`] is the value stored in the `metadata.count`
    /// field.
    fn count(&self) -> u32 {
        self.count.get()
    }
}

impl Validate for Metadata {
    type Error = anyhow::Error;

    /// TODO: Should there be an upper bound on `primary_names_count` or `count`?
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl<PS: ParseStrategy> Validate for [CommonSymbol<PS>] {
    type Error = <CommonSymbol<PS> as Validate>::Error;

    /// [`CommonSymbols`] have no internal constraints beyond those imposed by individual
    /// [`CommonSymbol`] objects.
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

array_type!(CommonSymbol, PS, CommonSymbolMetadata<PS>, Permissions<PS>);

array_type_validate_deref_none_data_vec!(CommonSymbol);

impl<PS: ParseStrategy> Counted for CommonSymbol<PS>
where
    CommonSymbolMetadata<PS>: Parse<PS> + Validate,
    Array<PS, PS::Output<CommonSymbolStaticMetadata>, PS::Slice<u8>>: Parse<PS>,
    Array<PS, PS::Output<PermissionMetadata>, PS::Slice<u8>>: Parse<PS>,
    Array<PS, CommonSymbolMetadata<PS>, Vec<Permission<PS>>>: Parse<PS>,
    Vec<Permission<PS>>: ParseSlice<PS>,
{
    /// The count of items in the associated [`Permissions`] is exposed via
    /// `CommonSymbolMetadata::count()`.
    fn count(&self) -> u32 {
        self.metadata.count()
    }
}

impl<PS: ParseStrategy> ValidateArray<CommonSymbolMetadata<PS>, Permission<PS>>
    for CommonSymbol<PS>
{
    type Error = anyhow::Error;

    /// [`CommonSymbol`] have no internal constraints beyond those imposed by [`Array`].
    fn validate_array<'a>(
        _metadata: &'a CommonSymbolMetadata<PS>,
        _data: &'a [Permission<PS>],
    ) -> Result<(), Self::Error> {
        Ok(())
    }
}

array_type!(CommonSymbolMetadata, PS, PS::Output<CommonSymbolStaticMetadata>, PS::Slice<u8>);

array_type_validate_deref_both!(CommonSymbolMetadata);

impl<PS: ParseStrategy> Counted for CommonSymbolMetadata<PS> {
    /// The count of items in the associated [`Permissions`] is stored in the associated
    /// `CommonSymbolStaticMetadata::count` field.
    fn count(&self) -> u32 {
        PS::deref(&self.metadata).count.get()
    }
}

impl<PS: ParseStrategy> ValidateArray<CommonSymbolStaticMetadata, u8> for CommonSymbolMetadata<PS> {
    type Error = anyhow::Error;

    /// Array of [`u8`] sized by [`CommonSymbolStaticMetadata`] requires no additional validation.
    fn validate_array<'a>(
        _metadata: &'a CommonSymbolStaticMetadata,
        _data: &'a [u8],
    ) -> Result<(), Self::Error> {
        Ok(())
    }
}

/// Static (that is, fixed-sized) metadata for a common symbol.
#[derive(Clone, Debug, FromZeroes, FromBytes, NoCell, PartialEq, Unaligned)]
#[repr(C, packed)]
pub(crate) struct CommonSymbolStaticMetadata {
    /// The length of the `[u8]` key stored in the associated [`CommonSymbolMetadata`].
    length: le::U32,
    /// An integer that identifies this this common symbol, unique to this common symbol relative
    /// to all common symbols and classes in this policy.
    value: le::U32,
    /// The number of primary names referred to by the associated [`CommonSymbol`].
    primary_names_count: le::U32,
    /// The number of items stored in the [`Permissions`] in the associated [`CommonSymbol`].
    count: le::U32,
}

impl Validate for CommonSymbolStaticMetadata {
    type Error = anyhow::Error;

    /// TODO: Should there be an upper bound on `length`?
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl Counted for CommonSymbolStaticMetadata {
    /// The count of bytes in the `[u8]` in the associated [`CommonSymbolMetadata`].
    fn count(&self) -> u32 {
        self.length.get()
    }
}

/// [`Permissions`] is a dynamically allocated slice (that is, [`Vec`]) of [`Permission`].
pub(crate) type Permissions<PS> = Vec<Permission<PS>>;

impl<PS: ParseStrategy> Validate for Permissions<PS> {
    type Error = anyhow::Error;

    /// [`Permissions`] have no internal constraints beyond those imposed by individual
    /// [`Permission`] objects.
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

array_type!(Permission, PS, PS::Output<PermissionMetadata>, PS::Slice<u8>);

array_type_validate_deref_both!(Permission);

impl<PS: ParseStrategy> ValidateArray<PermissionMetadata, u8> for Permission<PS> {
    type Error = anyhow::Error;

    /// [`Permission`] has no internal constraints beyond those imposed by [`Array`].
    fn validate_array<'a>(
        _metadata: &'a PermissionMetadata,
        _data: &'a [u8],
    ) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(Clone, Debug, FromZeroes, FromBytes, NoCell, PartialEq, Unaligned)]
#[repr(C, packed)]
pub(crate) struct PermissionMetadata {
    /// The length of the `[u8]` in the associated [`Permission`].
    length: le::U32,
    value: le::U32,
}

impl Counted for PermissionMetadata {
    /// The count of bytes in the `[u8]` in the associated [`Permission`].
    fn count(&self) -> u32 {
        self.length.get()
    }
}

impl Validate for PermissionMetadata {
    type Error = anyhow::Error;

    /// TODO: Should there be an upper bound on `length`?
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

pub(crate) type ConstraintsList<PS> = Vec<PermissionAndConstraints<PS>>;

impl<PS: ParseStrategy> Validate for ConstraintsList<PS> {
    type Error = anyhow::Error;

    /// [`ConstraintsList`] have no internal constraints beyond those imposed by individual
    /// [`PermissionAndConstraints`] objects.
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(Debug, PartialEq)]
pub(crate) struct PermissionAndConstraints<PS: ParseStrategy>
where
    ConstraintList<PS>: Debug + PartialEq,
{
    permission_bitset: PS::Output<le::U32>,
    constraints: ConstraintList<PS>,
}

impl<PS: ParseStrategy> Parse<PS> for PermissionAndConstraints<PS>
where
    ConstraintList<PS>: Debug + PartialEq + Parse<PS>,
{
    type Error = anyhow::Error;

    fn parse(bytes: PS) -> Result<(Self, PS), Self::Error> {
        let tail = bytes;

        let num_bytes = tail.len();
        let (permission_bitset, tail) = PS::parse::<le::U32>(tail).ok_or(
            Into::<anyhow::Error>::into(ParseError::MissingData {
                type_name: "PermissionBitset",
                type_size: std::mem::size_of::<le::U32>(),
                num_bytes,
            }),
        )?;
        let (constraints, tail) = ConstraintList::parse(tail)
            .map_err(|error| error.into() as anyhow::Error)
            .context("parsing constraint list in constraints list")?;

        Ok((Self { permission_bitset, constraints }, tail))
    }
}

array_type!(ConstraintList, PS, PS::Output<ConstraintCount>, Constraints<PS>);

array_type_validate_deref_metadata_data_vec!(ConstraintList);

impl<PS: ParseStrategy> ValidateArray<ConstraintCount, Constraint<PS>> for ConstraintList<PS> {
    type Error = anyhow::Error;

    /// [`ConstraintList`] has no internal constraints beyond those imposed by [`Array`].
    /// [`Permission`] has no internal constraints beyond those imposed by [`Array`].
    fn validate_array<'a>(
        _metadata: &'a ConstraintCount,
        _data: &'a [Constraint<PS>],
    ) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(Clone, Debug, FromZeroes, FromBytes, NoCell, PartialEq, Unaligned)]
#[repr(C, packed)]
pub(crate) struct ConstraintCount(le::U32);

impl Counted for ConstraintCount {
    fn count(&self) -> u32 {
        self.0.get()
    }
}

impl Validate for ConstraintCount {
    type Error = anyhow::Error;

    /// TODO: Should there be an upper bound on constraint count?
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

pub(crate) type Constraints<PS> = Vec<Constraint<PS>>;

impl<PS: ParseStrategy> Validate for Constraints<PS> {
    type Error = anyhow::Error;

    /// [`Permissions`] have no internal constraints beyond those imposed by individual
    /// [`Permission`] objects.
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(Debug, PartialEq)]
pub(crate) struct Constraint<PS: ParseStrategy> {
    metadata: PS::Output<ConstraintMetadata>,
    names: Option<ExtensibleBitmap<PS>>,
    names_type_set: Option<TypeSet<PS>>,
}

impl<PS: ParseStrategy> Parse<PS> for Constraint<PS>
where
    ExtensibleBitmap<PS>: Parse<PS>,
{
    type Error = anyhow::Error;

    fn parse(bytes: PS) -> Result<(Self, PS), Self::Error> {
        let tail = bytes;

        let (metadata, tail) =
            PS::parse::<ConstraintMetadata>(tail).context("parsing constraint metadata")?;

        let (names, names_type_set, tail) = match PS::deref(&metadata).constraint_type.get() {
            CONSTRAINT_TYPE_HAS_EXTENSIBLE_BITMAP_AND_TYPE_SET => {
                let (names, tail) = ExtensibleBitmap::parse(tail)
                    .map_err(Into::<anyhow::Error>::into)
                    .context("parsing constraint names")?;
                let (names_type_set, tail) =
                    TypeSet::parse(tail).context("parsing constraint names type set")?;
                (Some(names), Some(names_type_set), tail)
            }
            _ => (None, None, tail),
        };

        Ok((Self { metadata, names, names_type_set }, tail))
    }
}

#[derive(Clone, Debug, FromZeroes, FromBytes, NoCell, PartialEq, Unaligned)]
#[repr(C, packed)]
pub(crate) struct ConstraintMetadata {
    constraint_type: le::U32,
    attribute: le::U32,
    operands: le::U32,
}

impl Validate for ConstraintMetadata {
    type Error = anyhow::Error;

    /// TODO: Verify meaningful encoding of `constraint_type`, `attribute`, `operands`.
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(Debug, PartialEq)]
pub(crate) struct TypeSet<PS: ParseStrategy> {
    types: ExtensibleBitmap<PS>,
    negative_set: ExtensibleBitmap<PS>,
    flags: PS::Output<le::U32>,
}

impl<PS: ParseStrategy> Parse<PS> for TypeSet<PS>
where
    ExtensibleBitmap<PS>: Parse<PS>,
{
    type Error = anyhow::Error;

    fn parse(bytes: PS) -> Result<(Self, PS), Self::Error> {
        let tail = bytes;

        let (types, tail) = ExtensibleBitmap::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing type set types")?;

        let (negative_set, tail) = ExtensibleBitmap::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing type set negative set")?;

        let num_bytes = tail.len();
        let (flags, tail) = PS::parse::<le::U32>(tail).ok_or(Into::<anyhow::Error>::into(
            ParseError::MissingData {
                type_name: "TypeSetFlags",
                type_size: std::mem::size_of::<le::U32>(),
                num_bytes,
            },
        ))?;

        Ok((Self { types, negative_set, flags }, tail))
    }
}

impl<PS: ParseStrategy> Validate for [Class<PS>] {
    type Error = anyhow::Error;

    /// TODO: Validate internal consistency between consecutive [`Class`] instances.
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(Debug, PartialEq)]
pub(crate) struct Class<PS: ParseStrategy> {
    constraints: ClassConstraints<PS>,
    validate_transitions: ClassValidateTransitions<PS>,
    defaults: PS::Output<ClassDefaults>,
}

impl<PS: ParseStrategy> Parse<PS> for Class<PS>
where
    ClassConstraints<PS>: Parse<PS>,
    ClassValidateTransitions<PS>: Parse<PS>,
{
    type Error = anyhow::Error;

    fn parse(bytes: PS) -> Result<(Self, PS), Self::Error> {
        let tail = bytes;

        let (constraints, tail) = ClassConstraints::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing class constraints")?;

        let (validate_transitions, tail) = ClassValidateTransitions::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing class validate transitions")?;

        let (defaults, tail) =
            PS::parse::<ClassDefaults>(tail).context("parsing class defaults")?;

        Ok((Self { constraints, validate_transitions, defaults }, tail))
    }
}

#[derive(Clone, Debug, FromZeroes, FromBytes, NoCell, PartialEq, Unaligned)]
#[repr(C, packed)]
pub(crate) struct ClassDefaults {
    default_user: le::U32,
    default_role: le::U32,
    default_range: le::U32,
    default_type: le::U32,
}

array_type!(
    ClassValidateTransitions,
    PS,
    PS::Output<ClassValidateTransitionsCount>,
    Constraints<PS>
);

array_type_validate_deref_metadata_data_vec!(ClassValidateTransitions);

impl<PS: ParseStrategy> ValidateArray<ClassValidateTransitionsCount, Constraint<PS>>
    for ClassValidateTransitions<PS>
{
    type Error = anyhow::Error;

    /// [`ClassValidateTransitions`] has no internal constraints beyond those imposed by [`Array`].
    fn validate_array<'a>(
        _metadata: &'a ClassValidateTransitionsCount,
        _data: &'a [Constraint<PS>],
    ) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(Clone, Debug, FromZeroes, FromBytes, NoCell, PartialEq, Unaligned)]
#[repr(C, packed)]
pub(crate) struct ClassValidateTransitionsCount(le::U32);

impl Counted for ClassValidateTransitionsCount {
    fn count(&self) -> u32 {
        self.0.get()
    }
}

impl Validate for ClassValidateTransitionsCount {
    type Error = anyhow::Error;

    /// TODO: Should there be an upper bound on class validate transitions count?
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

array_type!(ClassConstraints, PS, ClassPermissions<PS>, ConstraintsList<PS>);

array_type_validate_deref_none_data_vec!(ClassConstraints);

impl<PS: ParseStrategy> ValidateArray<ClassPermissions<PS>, PermissionAndConstraints<PS>>
    for ClassConstraints<PS>
{
    type Error = anyhow::Error;

    /// [`ClassConstraints`] has no internal constraints beyond those imposed by [`Array`].
    fn validate_array<'a>(
        _metadata: &'a ClassPermissions<PS>,
        _data: &'a [PermissionAndConstraints<PS>],
    ) -> Result<(), Self::Error> {
        Ok(())
    }
}

array_type!(ClassPermissions, PS, ClassCommonKey<PS>, Permissions<PS>);

array_type_validate_deref_none_data_vec!(ClassPermissions);

impl<PS: ParseStrategy> ValidateArray<ClassCommonKey<PS>, Permission<PS>> for ClassPermissions<PS> {
    type Error = anyhow::Error;

    /// [`ClassPermissions`] has no internal constraints beyond those imposed by [`Array`].
    fn validate_array<'a>(
        _metadata: &'a ClassCommonKey<PS>,
        _data: &'a [Permission<PS>],
    ) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl<PS: ParseStrategy> Counted for ClassPermissions<PS>
where
    ClassCommonKey<PS>: Parse<PS>,
    Array<PS, ClassKey<PS>, PS::Slice<u8>>: Parse<PS>,
    Array<PS, PS::Output<ClassMetadata>, PS::Slice<u8>>: Parse<PS>,
    ClassKey<PS>: Parse<PS>,
    Vec<Permission<PS>>: ParseSlice<PS>,
    Array<PS, PS::Output<PermissionMetadata>, PS::Slice<u8>>: Parse<PS>,
    Array<PS, ClassCommonKey<PS>, Vec<Permission<PS>>>: Parse<PS>,
{
    /// [`ClassPermissions`] acts as counted metadata for [`ClassConstraints`].
    fn count(&self) -> u32 {
        PS::deref(&self.metadata.metadata.metadata).constraint_count.get()
    }
}

array_type!(ClassCommonKey, PS, ClassKey<PS>, PS::Slice<u8>);

array_type_validate_deref_data!(ClassCommonKey);

impl<PS: ParseStrategy> ValidateArray<ClassKey<PS>, u8> for ClassCommonKey<PS> {
    type Error = anyhow::Error;

    /// [`ClassCommonKey`] has no internal constraints beyond those imposed by [`Array`].
    fn validate_array<'a>(_metadata: &'a ClassKey<PS>, _data: &'a [u8]) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl<PS: ParseStrategy> Counted for ClassCommonKey<PS>
where
    Array<PS, ClassKey<PS>, PS::Slice<u8>>: Parse<PS>,
    Array<PS, PS::Output<ClassMetadata>, PS::Slice<u8>>: Parse<PS>,
    ClassKey<PS>: Parse<PS>,
{
    /// [`ClassCommonKey`] acts as counted metadata for [`ClassPermissions`].
    fn count(&self) -> u32 {
        PS::deref(&self.metadata.metadata).elements_count.get()
    }
}

array_type!(ClassKey, PS, PS::Output<ClassMetadata>, PS::Slice<u8>);

array_type_validate_deref_both!(ClassKey);

impl<PS: ParseStrategy> ValidateArray<ClassMetadata, u8> for ClassKey<PS> {
    type Error = anyhow::Error;

    /// [`ClassKey`] has no internal constraints beyond those imposed by [`Array`].
    fn validate_array<'a>(
        _metadata: &'a ClassMetadata,
        _data: &'a [u8],
    ) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl<PS: ParseStrategy> Counted for ClassKey<PS>
where
    Array<PS, PS::Output<ClassMetadata>, PS::Slice<u8>>: Parse<PS>,
{
    /// [`ClassKey`] acts as counted metadata for [`ClassCommonKey`].
    fn count(&self) -> u32 {
        PS::deref(&self.metadata).common_key_length.get()
    }
}

#[derive(Clone, Debug, FromZeroes, FromBytes, NoCell, PartialEq, Unaligned)]
#[repr(C, packed)]
pub(crate) struct ClassMetadata {
    key_length: le::U32,
    common_key_length: le::U32,
    value: le::U32,
    primary_names_count: le::U32,
    elements_count: le::U32,
    constraint_count: le::U32,
}

impl Counted for ClassMetadata {
    fn count(&self) -> u32 {
        self.key_length.get()
    }
}

impl Validate for ClassMetadata {
    type Error = anyhow::Error;

    /// TODO: Should there be an upper bound `u32` values in [`ClassMetadata`]?
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl<PS: ParseStrategy> Validate for [Role<PS>] {
    type Error = anyhow::Error;

    /// TODO: Validate internal consistency between consecutive [`Role`] instances.
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(Debug, PartialEq)]
pub(crate) struct Role<PS: ParseStrategy> {
    metadata: RoleMetadata<PS>,
    role_dominates: ExtensibleBitmap<PS>,
    role_types: ExtensibleBitmap<PS>,
}

impl<PS: ParseStrategy> Parse<PS> for Role<PS>
where
    RoleMetadata<PS>: Parse<PS>,
    ExtensibleBitmap<PS>: Parse<PS>,
{
    type Error = anyhow::Error;

    fn parse(bytes: PS) -> Result<(Self, PS), Self::Error> {
        let tail = bytes;

        let (metadata, tail) = RoleMetadata::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing role metadata")?;

        let (role_dominates, tail) = ExtensibleBitmap::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing role dominates")?;

        let (role_types, tail) = ExtensibleBitmap::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing role types")?;

        Ok((Self { metadata, role_dominates, role_types }, tail))
    }
}

array_type!(RoleMetadata, PS, PS::Output<RoleStaticMetadata>, PS::Slice<u8>);

array_type_validate_deref_both!(RoleMetadata);

impl<PS: ParseStrategy> ValidateArray<RoleStaticMetadata, u8> for RoleMetadata<PS> {
    type Error = anyhow::Error;

    /// [`RoleMetadata`] has no internal constraints beyond those imposed by [`Array`].
    fn validate_array<'a>(
        _metadata: &'a RoleStaticMetadata,
        _data: &'a [u8],
    ) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(Clone, Debug, FromZeroes, FromBytes, NoCell, PartialEq, Unaligned)]
#[repr(C, packed)]
pub(crate) struct RoleStaticMetadata {
    length: le::U32,
    value: le::U32,
    bounds: le::U32,
}

impl Counted for RoleStaticMetadata {
    /// [`RoleStaticMetadata`] serves as [`Counted`] for a length-encoded `[u8]`.
    fn count(&self) -> u32 {
        self.length.get()
    }
}

impl Validate for RoleStaticMetadata {
    type Error = anyhow::Error;

    /// TODO: Should there be any constraints on `length`, `value`, or `bounds`?
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl<PS: ParseStrategy> Validate for [Type<PS>] {
    type Error = anyhow::Error;

    /// TODO: Validate internal consistency between consecutive [`Type`] instances.
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

array_type!(Type, PS, PS::Output<TypeMetadata>, PS::Slice<u8>);

array_type_validate_deref_both!(Type);

impl<PS: ParseStrategy> ValidateArray<TypeMetadata, u8> for Type<PS> {
    type Error = anyhow::Error;

    /// TODO: Validate that `self.data.deref()` is an ascii string that contains a valid type name.
    fn validate_array<'a>(_metadata: &'a TypeMetadata, _data: &'a [u8]) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(Clone, Debug, FromZeroes, FromBytes, NoCell, PartialEq, Unaligned)]
#[repr(C, packed)]
pub(crate) struct TypeMetadata {
    length: le::U32,
    value: le::U32,
    properties: le::U32,
    bounds: le::U32,
}

impl Counted for TypeMetadata {
    fn count(&self) -> u32 {
        self.length.get()
    }
}

impl Validate for TypeMetadata {
    type Error = anyhow::Error;

    /// TODO: Validate [`TypeMetadata`] internals.
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl<PS: ParseStrategy> Validate for [User<PS>] {
    type Error = anyhow::Error;

    /// TODO: Validate internal consistency between consecutive [`User`] instances.
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(Debug, PartialEq)]
pub(crate) struct User<PS: ParseStrategy> {
    user_data: UserData<PS>,
    roles: ExtensibleBitmap<PS>,
    expanded_range: MlsRange<PS>,
    default_level: MLSLevel<PS>,
}

impl<PS: ParseStrategy> Parse<PS> for User<PS>
where
    UserData<PS>: Parse<PS>,
    ExtensibleBitmap<PS>: Parse<PS>,
{
    type Error = anyhow::Error;

    fn parse(bytes: PS) -> Result<(Self, PS), Self::Error> {
        let tail = bytes;

        let (user_data, tail) = UserData::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing user data")?;

        let (roles, tail) = ExtensibleBitmap::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing user roles")?;

        let (expanded_range, tail) =
            MlsRange::parse(tail).context("parsing user expanded range")?;

        let (default_level, tail) = MLSLevel::parse(tail).context("parsing user default level")?;

        Ok((Self { user_data, roles, expanded_range, default_level }, tail))
    }
}

array_type!(UserData, PS, PS::Output<UserMetadata>, PS::Slice<u8>);

array_type_validate_deref_both!(UserData);

impl<PS: ParseStrategy> ValidateArray<UserMetadata, u8> for UserData<PS> {
    type Error = anyhow::Error;

    /// TODO: Validate consistency between [`UserMetadata`] in `self.metadata` and `[u8]` key in `self.data`.
    fn validate_array<'a>(_metadata: &'a UserMetadata, _data: &'a [u8]) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(Clone, Debug, FromZeroes, FromBytes, NoCell, PartialEq, Unaligned)]
#[repr(C, packed)]
pub(crate) struct UserMetadata {
    length: le::U32,
    value: le::U32,
    bounds: le::U32,
}

impl Counted for UserMetadata {
    fn count(&self) -> u32 {
        self.length.get()
    }
}

impl Validate for UserMetadata {
    type Error = anyhow::Error;

    /// TODO: Validate [`UserMetadata`] internals.
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(Debug, PartialEq)]
pub(crate) struct MlsRange<PS: ParseStrategy> {
    count: PS::Output<le::U32>,
    sensitivity_low: PS::Output<le::U32>,
    sensitivity_high: Option<PS::Output<le::U32>>,
    low_categories: ExtensibleBitmap<PS>,
    high_categories: Option<ExtensibleBitmap<PS>>,
}

impl<PS: ParseStrategy> Parse<PS> for MlsRange<PS>
where
    ExtensibleBitmap<PS>: Parse<PS>,
{
    type Error = anyhow::Error;

    fn parse(bytes: PS) -> Result<(Self, PS), Self::Error> {
        let tail = bytes;

        let num_bytes = tail.len();
        let (count, tail) = PS::parse::<le::U32>(tail).ok_or(ParseError::MissingData {
            type_name: "MLSRangeCount",
            type_size: std::mem::size_of::<le::U32>(),
            num_bytes,
        })?;

        let num_bytes = tail.len();
        let (sensitivity_low, tail) =
            PS::parse::<le::U32>(tail).ok_or(ParseError::MissingData {
                type_name: "MLSRangeSensitivityLow",
                type_size: std::mem::size_of::<le::U32>(),
                num_bytes,
            })?;

        let (sensitivity_high, low_categories, high_categories, tail) =
            if PS::deref(&count).get() > 1 {
                let num_bytes = tail.len();
                let (sensitivity_high, tail) =
                    PS::parse::<le::U32>(tail).ok_or(ParseError::MissingData {
                        type_name: "MLSRangeSensitivityHigh",
                        type_size: std::mem::size_of::<le::U32>(),
                        num_bytes,
                    })?;
                let (low_categories, tail) = ExtensibleBitmap::parse(tail)
                    .map_err(Into::<anyhow::Error>::into)
                    .context("parsing mls range low categories")?;
                let (high_categories, tail) = ExtensibleBitmap::parse(tail)
                    .map_err(Into::<anyhow::Error>::into)
                    .context("parsing mls range high categories")?;

                (Some(sensitivity_high), low_categories, Some(high_categories), tail)
            } else {
                let (low_categories, tail) = ExtensibleBitmap::parse(tail)
                    .map_err(Into::<anyhow::Error>::into)
                    .context("parsing mls range low categories")?;

                (None, low_categories, None, tail)
            };

        Ok((
            Self { count, sensitivity_low, sensitivity_high, low_categories, high_categories },
            tail,
        ))
    }
}

#[derive(Clone, Debug, FromZeroes, FromBytes, NoCell, PartialEq, Unaligned)]
#[repr(C, packed)]
pub(crate) struct MLSRangeMetadata {
    count: le::U32,
    sensitivity_low: le::U32,
    sensitivity_high: le::U32,
}

impl Validate for MLSRangeMetadata {
    type Error = anyhow::Error;

    /// TODO: Validate [`MLSRangeMetadata`] internals.
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(Debug, PartialEq)]
pub(crate) struct MLSLevel<PS: ParseStrategy> {
    sensitivity: PS::Output<le::U32>,
    categories: ExtensibleBitmap<PS>,
}

impl<PS: ParseStrategy> Parse<PS> for MLSLevel<PS>
where
    ExtensibleBitmap<PS>: Parse<PS>,
{
    type Error = anyhow::Error;

    fn parse(bytes: PS) -> Result<(Self, PS), Self::Error> {
        let tail = bytes;

        let num_bytes = tail.len();
        let (sensitivity, tail) = PS::parse::<le::U32>(tail).ok_or(ParseError::MissingData {
            type_name: "MLSLevelSensitivity",
            type_size: std::mem::size_of::<le::U32>(),
            num_bytes,
        })?;

        let (categories, tail) = ExtensibleBitmap::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing mls level categories")?;

        Ok((Self { sensitivity, categories }, tail))
    }
}

impl<PS: ParseStrategy> Validate for [ConditionalBoolean<PS>] {
    type Error = anyhow::Error;

    /// TODO: Validate consistency of sequence of [`ConditionalBoolean`].
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

array_type!(ConditionalBoolean, PS, PS::Output<ConditionalBooleanMetadata>, PS::Slice<u8>);

array_type_validate_deref_both!(ConditionalBoolean);

impl<PS: ParseStrategy> ValidateArray<ConditionalBooleanMetadata, u8> for ConditionalBoolean<PS> {
    type Error = anyhow::Error;

    /// TODO: Validate consistency between [`ConditionalBooleanMetadata`] and `[u8]` key.
    fn validate_array<'a>(
        _metadata: &'a ConditionalBooleanMetadata,
        _data: &'a [u8],
    ) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(Clone, Debug, FromZeroes, FromBytes, NoCell, PartialEq, Unaligned)]
#[repr(C, packed)]
pub(crate) struct ConditionalBooleanMetadata {
    value: le::U32,
    state: le::U32,
    length: le::U32,
}

impl Counted for ConditionalBooleanMetadata {
    /// [`ConditionalBooleanMetadata`] used as `M` in of `Array<PS, PS::Output<M>, PS::Slice<u8>>` with
    /// `self.length` denoting size of inner `[u8]`.
    fn count(&self) -> u32 {
        self.length.get()
    }
}

impl Validate for ConditionalBooleanMetadata {
    type Error = anyhow::Error;

    /// TODO: Validate internal consistency of [`ConditionalBooleanMetadata`].
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl<PS: ParseStrategy> Validate for [Sensitivity<PS>] {
    type Error = anyhow::Error;

    /// TODO: Validate consistency of sequence of [`Sensitivity`].
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(Debug, PartialEq)]
pub(crate) struct Sensitivity<PS: ParseStrategy> {
    metadata: SensitivityMetadata<PS>,
    level: MLSLevel<PS>,
}

impl<PS: ParseStrategy> Parse<PS> for Sensitivity<PS>
where
    SensitivityMetadata<PS>: Parse<PS>,
    MLSLevel<PS>: Parse<PS>,
{
    type Error = anyhow::Error;

    fn parse(bytes: PS) -> Result<(Self, PS), Self::Error> {
        let tail = bytes;

        let (metadata, tail) = SensitivityMetadata::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing sensitivity metadata")?;

        let (level, tail) = MLSLevel::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing sensitivity mls level")?;

        Ok((Self { metadata, level }, tail))
    }
}

impl<PS: ParseStrategy> Validate for Sensitivity<PS> {
    type Error = anyhow::Error;

    /// TODO: Validate internal consistency of `self.metadata` and `self.level`.
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

array_type!(SensitivityMetadata, PS, PS::Output<SensitivityStaticMetadata>, PS::Slice<u8>);

array_type_validate_deref_both!(SensitivityMetadata);

impl<PS: ParseStrategy> ValidateArray<SensitivityStaticMetadata, u8> for SensitivityMetadata<PS> {
    type Error = anyhow::Error;

    /// TODO: Validate consistency between [`SensitivityMetadata`] and `[u8]` key.
    fn validate_array<'a>(
        _metadata: &'a SensitivityStaticMetadata,
        _data: &'a [u8],
    ) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(Clone, Debug, FromZeroes, FromBytes, NoCell, PartialEq, Unaligned)]
#[repr(C, packed)]
pub(crate) struct SensitivityStaticMetadata {
    length: le::U32,
    is_alias: le::U32,
}

impl Counted for SensitivityStaticMetadata {
    /// [`SensitivityStaticMetadata`] used as `M` in of `Array<PS, PS::Output<M>, PS::Slice<u8>>` with
    /// `self.length` denoting size of inner `[u8]`.
    fn count(&self) -> u32 {
        self.length.get()
    }
}

impl Validate for SensitivityStaticMetadata {
    type Error = anyhow::Error;

    /// TODO: Validate internal consistency of [`SensitivityStaticMetadata`].
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl<PS: ParseStrategy> Validate for [Category<PS>] {
    type Error = anyhow::Error;

    /// TODO: Validate consistency of sequence of [`Category`].
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

array_type!(Category, PS, PS::Output<CategoryMetadata>, PS::Slice<u8>);

array_type_validate_deref_both!(Category);

impl<PS: ParseStrategy> ValidateArray<CategoryMetadata, u8> for Category<PS> {
    type Error = anyhow::Error;

    /// TODO: Validate consistency between [`CategoryMetadata`] and `[u8]` key.
    fn validate_array<'a>(
        _metadata: &'a CategoryMetadata,
        _data: &'a [u8],
    ) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(Clone, Debug, FromZeroes, FromBytes, NoCell, PartialEq, Unaligned)]
#[repr(C, packed)]
pub(crate) struct CategoryMetadata {
    length: le::U32,
    value: le::U32,
    is_alias: le::U32,
}

impl Counted for CategoryMetadata {
    /// [`CategoryMetadata`] used as `M` in of `Array<PS, PS::Output<M>, PS::Slice<u8>>` with
    /// `self.length` denoting size of inner `[u8]`.
    fn count(&self) -> u32 {
        self.length.get()
    }
}

impl Validate for CategoryMetadata {
    type Error = anyhow::Error;

    /// TODO: Validate internal consistency of [`CategoryMetadata`].
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}
