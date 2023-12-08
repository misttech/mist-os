// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::{
    error::ParseError, extensible_bitmap::ExtensibleBitmap, Array, Counted, Parse, ParseSlice,
    Validate,
};

use anyhow::Context as _;
use std::{fmt::Debug, ops::Deref as _};
use zerocopy::{little_endian as le, AsBytes, ByteSlice, FromBytes, FromZeroes, Ref, Unaligned};

/// [`SymbolList`] is an [`Array`] of items with the count of items determined by [`Metadata`] as
/// [`Counted`].
pub(crate) type SymbolList<B, T> = Array<B, Ref<B, Metadata>, T>;

impl<B: ByteSlice + Debug + PartialEq, T: Debug + ParseSlice<B> + PartialEq + Validate> Validate
    for SymbolList<B, T>
{
    type Error = ParseError;

    /// [`SymbolList`] has no internal constraints beyond those imposed by [`Array`].
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

/// Binary metadata prefix to [`SymbolList`] objects.
#[derive(AsBytes, Debug, FromZeroes, FromBytes, PartialEq, Unaligned)]
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
    type Error = ParseError;

    /// TODO: Should there be an upper bound on `primary_names_count` or `count`?
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

pub(crate) type CommonSymbols<B> = Vec<CommonSymbol<B>>;

impl<B: ByteSlice + Debug + PartialEq> Validate for CommonSymbols<B> {
    type Error = <CommonSymbol<B> as Parse<B>>::Error;

    /// [`CommonSymbols`] have no internal constraints beyond those imposed by individual
    /// [`CommonSymbol`] objects.
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

/// [`CommonSymbol`] is an [`Array`] of items parsed via [`Permissions`] with the count of items
/// determined by [`CommonSymbolMetadata`] as [`Counted`].
pub(crate) type CommonSymbol<B> = Array<B, CommonSymbolMetadata<B>, Permissions<B>>;

impl<B: ByteSlice + Debug + PartialEq> Counted for CommonSymbol<B> {
    /// The count of items in the associated [`Permissions`] is exposed via
    /// `CommonSymbolMetadata::count()`.
    fn count(&self) -> u32 {
        self.metadata.count()
    }
}

impl<B: ByteSlice + Debug + PartialEq> Validate for CommonSymbol<B> {
    type Error = ParseError;

    /// [`CommonSymbol`] have no internal constraints beyond those imposed by [`Array`].
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

/// [`CommonSymbolMetadata`] is an [`Array`] of items parsed via `Ref<B, [u8]>` with the count of
/// itemsdetermined by [`CommonSymbolStaticMetadata`] as [`Counted`].
pub(crate) type CommonSymbolMetadata<B> =
    Array<B, Ref<B, CommonSymbolStaticMetadata>, Ref<B, [u8]>>;

impl<B: ByteSlice + Debug + PartialEq> Counted for CommonSymbolMetadata<B> {
    /// The count of items in the associated [`Permissions`] is stored in the associated
    /// `CommonSymbolStaticMetadata::count` field.
    fn count(&self) -> u32 {
        self.metadata.deref().count.get()
    }
}

impl<B: ByteSlice + Debug + PartialEq> Validate for CommonSymbolMetadata<B> {
    type Error = ParseError;

    /// Array of [`u8`] sized by [`CommonSymbolStaticMetadata`] requires no additional validation.
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

/// Static (that is, fixed-sized) metadata for a common symbol.
#[derive(AsBytes, Debug, FromZeroes, FromBytes, PartialEq, Unaligned)]
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
    type Error = ParseError;

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
pub(crate) type Permissions<B> = Vec<Permission<B>>;

impl<B: ByteSlice + Debug + PartialEq> Validate for Permissions<B> {
    type Error = <Permission<B> as Parse<B>>::Error;

    /// [`Permissions`] have no internal constraints beyond those imposed by individual
    /// [`Permission`] objects.
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

/// [`Permission`] is an [`Array`] of items parsed via `Ref<B, [u8]>` with the count of items
/// determined by a `Ref<B, PermissionMetadata>` as [`Counted`].
pub(crate) type Permission<B> = Array<B, Ref<B, PermissionMetadata>, Ref<B, [u8]>>;

impl<B: ByteSlice + Debug + PartialEq> Validate for Permission<B> {
    type Error = ParseError;

    /// [`Permission`] has no internal constraints beyond those imposed by [`Array`].
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(AsBytes, Debug, FromZeroes, FromBytes, PartialEq, Unaligned)]
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
    type Error = ParseError;

    /// TODO: Should there be an upper bound on `length`?
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

pub(crate) type ConstraintsList<B> = Vec<PermissionAndConstraints<B>>;

impl<B: ByteSlice + Debug + PartialEq> Validate for ConstraintsList<B> {
    type Error = <PermissionAndConstraints<B> as Parse<B>>::Error;

    /// [`ConstraintsList`] have no internal constraints beyond those imposed by individual
    /// [`PermissionAndConstraints`] objects.
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(Debug, PartialEq)]
pub(crate) struct PermissionAndConstraints<B: ByteSlice + Debug + PartialEq> {
    permission_bitset: Ref<B, le::U32>,
    constraints: ConstraintList<B>,
}

impl<B: ByteSlice + Debug + PartialEq> Parse<B> for PermissionAndConstraints<B> {
    type Error = anyhow::Error;

    fn parse(bytes: B) -> Result<(Self, B), Self::Error> {
        let tail = bytes;

        let num_bytes = tail.len();
        let (permission_bitset, tail) = Ref::<B, le::U32>::new_unaligned_from_prefix(tail).ok_or(
            Into::<anyhow::Error>::into(ParseError::MissingData {
                type_name: "PermissionBitset",
                type_size: std::mem::size_of::<le::U32>(),
                num_bytes,
            }),
        )?;
        let (constraints, tail) =
            ConstraintList::parse(tail).context("parsing constraint list in constraints list")?;

        Ok((Self { permission_bitset, constraints }, tail))
    }
}

impl<B: ByteSlice + Debug + PartialEq> Validate for PermissionAndConstraints<B> {
    type Error = ParseError;

    /// TODO: Should there be internal validation between `permission_bitset` and `constraints`?
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

pub(crate) type ConstraintList<B> = Array<B, Ref<B, ConstraintCount>, Constraints<B>>;

impl<B: ByteSlice + Debug + PartialEq> Validate for ConstraintList<B> {
    type Error = ParseError;

    /// [`ConstraintList`] has no internal constraints beyond those imposed by [`Array`].
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(AsBytes, Debug, FromZeroes, FromBytes, PartialEq, Unaligned)]
#[repr(C, packed)]
pub(crate) struct ConstraintCount(le::U32);

impl Counted for ConstraintCount {
    fn count(&self) -> u32 {
        self.0.get()
    }
}

impl Validate for ConstraintCount {
    type Error = ParseError;

    /// TODO: Should there be an upper bound on constraint count?
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

pub(crate) type Constraints<B> = Vec<Constraint<B>>;

impl<B: ByteSlice + Debug + PartialEq> Validate for Constraints<B> {
    type Error = ParseError;

    /// [`Permissions`] have no internal constraints beyond those imposed by individual
    /// [`Permission`] objects.
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(Debug, PartialEq)]
pub(crate) struct Constraint<B: ByteSlice + Debug + PartialEq> {
    metadata: Ref<B, ConstraintMetadata>,
    names: Option<ExtensibleBitmap<B>>,
    names_type_set: Option<TypeSet<B>>,
}

impl<B: ByteSlice + Debug + PartialEq> Validate for Constraint<B> {
    type Error = ParseError;

    /// TODO: Verify expected internal relationships between `metadata`, `names`, and
    /// `names_type_set`.
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl<B: ByteSlice + Debug + PartialEq> Parse<B> for Constraint<B> {
    type Error = anyhow::Error;

    fn parse(bytes: B) -> Result<(Self, B), Self::Error> {
        let tail = bytes;

        let (metadata, tail) =
            Ref::<B, ConstraintMetadata>::parse(tail).context("parsing constraint metadata")?;

        let (names, names_type_set, tail) = match metadata.deref().constraint_type.get() {
            5 => {
                let (names, tail) =
                    ExtensibleBitmap::parse(tail).context("parsing constraint names")?;
                let (names_type_set, tail) =
                    TypeSet::parse(tail).context("parsing constraint names type set")?;
                (Some(names), Some(names_type_set), tail)
            }
            _ => (None, None, tail),
        };

        Ok((Self { metadata, names, names_type_set }, tail))
    }
}

#[derive(AsBytes, Debug, FromZeroes, FromBytes, PartialEq, Unaligned)]
#[repr(C, packed)]
pub(crate) struct ConstraintMetadata {
    constraint_type: le::U32,
    attribute: le::U32,
    operands: le::U32,
}

impl Validate for ConstraintMetadata {
    type Error = ParseError;

    /// TODO: Verify meaningful encoding of `constraint_type`, `attribute`, `operands`.
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(Debug, PartialEq)]
pub(crate) struct TypeSet<B: ByteSlice + Debug + PartialEq> {
    types: ExtensibleBitmap<B>,
    negative_set: ExtensibleBitmap<B>,
    flags: Ref<B, le::U32>,
}

impl<B: ByteSlice + Debug + PartialEq> Parse<B> for TypeSet<B> {
    type Error = anyhow::Error;

    fn parse(bytes: B) -> Result<(Self, B), Self::Error> {
        let tail = bytes;

        let (types, tail) = ExtensibleBitmap::parse(tail).context("parsing type set types")?;

        let (negative_set, tail) =
            ExtensibleBitmap::parse(tail).context("parsing type set negative set")?;

        let num_bytes = tail.len();
        let (flags, tail) =
            Ref::<B, le::U32>::new_unaligned_from_prefix(tail).ok_or(
                Into::<anyhow::Error>::into(ParseError::MissingData {
                    type_name: "TypeSetFlags",
                    type_size: std::mem::size_of::<le::U32>(),
                    num_bytes,
                }),
            )?;

        Ok((Self { types, negative_set, flags }, tail))
    }
}

pub(crate) type Classes<B> = Vec<Class<B>>;

impl<B: ByteSlice + Debug + PartialEq> Validate for Classes<B> {
    type Error = ParseError;

    /// TODO: Validate internal consistency between consecutive [`Class`] instances.
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(Debug, PartialEq)]
pub(crate) struct Class<B: ByteSlice + Debug + PartialEq> {
    constraints: ClassConstraints<B>,
    validate_transitions: ClassValidateTransitions<B>,
    defaults: Ref<B, ClassDefaults>,
}

impl<B: ByteSlice + Debug + PartialEq> Parse<B> for Class<B> {
    type Error = anyhow::Error;

    fn parse(bytes: B) -> Result<(Self, B), Self::Error> {
        let tail = bytes;

        let (constraints, tail) =
            ClassConstraints::parse(tail).context("parsing class constraints")?;

        let (validate_transitions, tail) =
            ClassValidateTransitions::parse(tail).context("parsing class validate transitions")?;

        let (defaults, tail) =
            Ref::<B, ClassDefaults>::parse(tail).context("parsing class defaults")?;

        Ok((Self { constraints, validate_transitions, defaults }, tail))
    }
}

impl<B: ByteSlice + Debug + PartialEq> Validate for Class<B> {
    type Error = ParseError;

    /// TODO: Add validation of consistency between `constraints`, `validate_transitions`, and
    /// `defaults`.
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(AsBytes, Debug, FromZeroes, FromBytes, PartialEq, Unaligned)]
#[repr(C, packed)]
pub(crate) struct ClassDefaults {
    default_user: le::U32,
    default_role: le::U32,
    default_range: le::U32,
    default_type: le::U32,
}

impl Validate for ClassDefaults {
    type Error = ParseError;

    /// Default values may be arbitrary [`u32`] values.
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

pub(crate) type ClassValidateTransitions<B> =
    Array<B, Ref<B, ClassValidateTransitionsCount>, Constraints<B>>;

impl<B: ByteSlice + Debug + PartialEq> Validate for ClassValidateTransitions<B> {
    type Error = ParseError;

    /// [`ClassValidateTransitions`] has no internal constraints beyond those imposed by [`Array`].
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(AsBytes, Debug, FromZeroes, FromBytes, PartialEq, Unaligned)]
#[repr(C, packed)]
pub(crate) struct ClassValidateTransitionsCount(le::U32);

impl Counted for ClassValidateTransitionsCount {
    fn count(&self) -> u32 {
        self.0.get()
    }
}

impl Validate for ClassValidateTransitionsCount {
    type Error = ParseError;

    /// TODO: Should there be an upper bound on class validate transitions count?
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

pub(crate) type ClassConstraints<B> = Array<B, ClassPermissions<B>, ConstraintsList<B>>;

impl<B: ByteSlice + Debug + PartialEq> Validate for ClassConstraints<B> {
    type Error = ParseError;

    /// [`ClassConstraints`] has no internal constraints beyond those imposed by [`Array`].
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

pub(crate) type ClassPermissions<B> = Array<B, ClassCommonKey<B>, Permissions<B>>;

impl<B: ByteSlice + Debug + PartialEq> Validate for ClassPermissions<B> {
    type Error = ParseError;

    /// [`ClassPermissions`] has no internal constraints beyond those imposed by [`Array`].
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl<B: ByteSlice + Debug + PartialEq> Counted for ClassPermissions<B> {
    /// [`ClassPermissions`] acts as counted metadata for [`ClassConstraints`].
    fn count(&self) -> u32 {
        self.metadata.metadata.metadata.deref().constraint_count.get()
    }
}

pub(crate) type ClassCommonKey<B> = Array<B, ClassKey<B>, Ref<B, [u8]>>;

impl<B: ByteSlice + Debug + PartialEq> Validate for ClassCommonKey<B> {
    type Error = ParseError;

    /// [`ClassCommonKey`] has no internal constraints beyond those imposed by [`Array`].
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl<B: ByteSlice + Debug + PartialEq> Counted for ClassCommonKey<B> {
    /// [`ClassCommonKey`] acts as counted metadata for [`ClassPermissions`].
    fn count(&self) -> u32 {
        self.metadata.metadata.deref().elements_count.get()
    }
}

pub(crate) type ClassKey<B> = Array<B, Ref<B, ClassMetadata>, Ref<B, [u8]>>;

impl<B: ByteSlice + Debug + PartialEq> Validate for ClassKey<B> {
    type Error = ParseError;

    /// [`ClassKey`] has no internal constraints beyond those imposed by [`Array`].
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl<B: ByteSlice + Debug + PartialEq> Counted for ClassKey<B> {
    /// [`ClassKey`] acts as counted metadata for [`ClassCommonKey`].
    fn count(&self) -> u32 {
        self.metadata.deref().common_key_length.get()
    }
}

#[derive(AsBytes, Debug, FromZeroes, FromBytes, PartialEq, Unaligned)]
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
    type Error = ParseError;

    /// TODO: Should there be an upper bound `u32` values in [`ClassMetadata`]?
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

pub(crate) type Roles<B> = Vec<Role<B>>;

impl<B: ByteSlice + Debug + PartialEq> Validate for Roles<B> {
    type Error = ParseError;

    /// TODO: Validate internal consistency between consecutive [`Role`] instances.
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(Debug, PartialEq)]
pub(crate) struct Role<B: ByteSlice + Debug + PartialEq> {
    metadata: RoleMetadata<B>,
    role_dominates: ExtensibleBitmap<B>,
    role_types: ExtensibleBitmap<B>,
}

impl<B: ByteSlice + Debug + PartialEq> Parse<B> for Role<B> {
    type Error = anyhow::Error;

    fn parse(bytes: B) -> Result<(Self, B), Self::Error> {
        let tail = bytes;

        let (metadata, tail) = RoleMetadata::parse(tail).context("parsing role metadata")?;

        let (role_dominates, tail) =
            ExtensibleBitmap::parse(tail).context("parsing role dominates")?;

        let (role_types, tail) = ExtensibleBitmap::parse(tail).context("parsing role types")?;

        Ok((Self { metadata, role_dominates, role_types }, tail))
    }
}

impl<B: ByteSlice + Debug + PartialEq> Validate for Role<B> {
    type Error = ParseError;

    /// TODO: Should there be internal validation checks between `metadata`, `role_dominates`, and
    /// `role_types`?
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

pub(crate) type RoleMetadata<B> = Array<B, Ref<B, RoleStaticMetadata>, Ref<B, [u8]>>;

impl<B: ByteSlice + Debug + PartialEq> Validate for RoleMetadata<B> {
    type Error = ParseError;

    /// [`RoleMetadata`] has no internal constraints beyond those imposed by [`Array`].
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(AsBytes, Debug, FromZeroes, FromBytes, PartialEq, Unaligned)]
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
    type Error = ParseError;

    /// TODO: Should there be any constraints on `length`, `value`, or `bounds`?
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

pub(crate) type Types<B> = Vec<Type<B>>;

impl<B: ByteSlice + Debug + PartialEq> Validate for Types<B> {
    type Error = ParseError;

    /// TODO: Validate internal consistency between consecutive [`Type`] instances.
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

pub(crate) type Type<B> = Array<B, Ref<B, TypeMetadata>, Ref<B, [u8]>>;

impl<B: ByteSlice + Debug + PartialEq> Validate for Type<B> {
    type Error = ParseError;

    /// TODO: Validate that `self.data.deref()` is an ascii string that contains a valid type name.
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(AsBytes, Debug, FromZeroes, FromBytes, PartialEq, Unaligned)]
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
    type Error = ParseError;

    /// TODO: Validate [`TypeMetadata`] internals.
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

pub(crate) type Users<B> = Vec<User<B>>;

impl<B: ByteSlice + Debug + PartialEq> Validate for Users<B> {
    type Error = ParseError;

    /// TODO: Validate internal consistency between consecutive [`User`] instances.
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(Debug, PartialEq)]
pub(crate) struct User<B: ByteSlice + Debug + PartialEq> {
    user_data: UserData<B>,
    roles: ExtensibleBitmap<B>,
    expanded_range: MlsRange<B>,
    default_level: MLSLevel<B>,
}

impl<B: ByteSlice + Debug + PartialEq> Parse<B> for User<B> {
    type Error = anyhow::Error;

    fn parse(bytes: B) -> Result<(Self, B), Self::Error> {
        let tail = bytes;

        let (user_data, tail) = UserData::parse(tail).context("parsing user data")?;

        let (roles, tail) = ExtensibleBitmap::parse(tail).context("parsing user roles")?;

        let (expanded_range, tail) =
            MlsRange::parse(tail).context("parsing user expanded range")?;

        let (default_level, tail) = MLSLevel::parse(tail).context("parsing user default level")?;

        Ok((Self { user_data, roles, expanded_range, default_level }, tail))
    }
}

impl<B: ByteSlice + Debug + PartialEq> Validate for User<B> {
    type Error = ParseError;

    /// TODO: Validate internal consistency of [`User`].
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

pub(crate) type UserData<B> = Array<B, Ref<B, UserMetadata>, Ref<B, [u8]>>;

impl<B: ByteSlice + Debug + PartialEq> Validate for UserData<B> {
    type Error = ParseError;

    /// TODO: Validate consistency between [`UserMetadata`] in `self.metadata` and `[u8]` key in `self.data`.
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(AsBytes, Debug, FromZeroes, FromBytes, PartialEq, Unaligned)]
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
    type Error = ParseError;

    /// TODO: Validate [`UserMetadata`] internals.
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(Debug, PartialEq)]
pub(crate) struct MlsRange<B: ByteSlice + Debug + PartialEq> {
    count: Ref<B, le::U32>,
    sensitivity_low: Ref<B, le::U32>,
    sensitivity_high: Option<Ref<B, le::U32>>,
    low_categories: ExtensibleBitmap<B>,
    high_categories: Option<ExtensibleBitmap<B>>,
}

impl<B: ByteSlice + Debug + PartialEq> Parse<B> for MlsRange<B> {
    type Error = anyhow::Error;

    fn parse(bytes: B) -> Result<(Self, B), Self::Error> {
        let tail = bytes;

        let num_bytes = tail.len();
        let (count, tail) =
            Ref::<B, le::U32>::new_unaligned_from_prefix(tail).ok_or(ParseError::MissingData {
                type_name: "MLSRangeCount",
                type_size: std::mem::size_of::<le::U32>(),
                num_bytes,
            })?;

        let num_bytes = tail.len();
        let (sensitivity_low, tail) =
            Ref::<B, le::U32>::new_unaligned_from_prefix(tail).ok_or(ParseError::MissingData {
                type_name: "MLSRangeSensitivityLow",
                type_size: std::mem::size_of::<le::U32>(),
                num_bytes,
            })?;

        let (sensitivity_high, low_categories, high_categories, tail) = if count.deref().get() > 1 {
            let num_bytes = tail.len();
            let (sensitivity_high, tail) = Ref::<B, le::U32>::new_unaligned_from_prefix(tail)
                .ok_or(ParseError::MissingData {
                    type_name: "MLSRangeSensitivityHigh",
                    type_size: std::mem::size_of::<le::U32>(),
                    num_bytes,
                })?;
            let (low_categories, tail) =
                ExtensibleBitmap::parse(tail).context("parsing mls range low categories")?;
            let (high_categories, tail) =
                ExtensibleBitmap::parse(tail).context("parsing mls range high categories")?;

            (Some(sensitivity_high), low_categories, Some(high_categories), tail)
        } else {
            let (low_categories, tail) =
                ExtensibleBitmap::parse(tail).context("parsing mls range low categories")?;

            (None, low_categories, None, tail)
        };

        Ok((
            Self { count, sensitivity_low, sensitivity_high, low_categories, high_categories },
            tail,
        ))
    }
}

impl<B: ByteSlice + Debug + PartialEq> Validate for MlsRange<B> {
    type Error = anyhow::Error;

    /// TODO: Validate [`MLSRange`] internal consistency in addition to delegating to extensible
    /// bitmap metadata.
    fn validate(&self) -> Result<(), Self::Error> {
        self.low_categories.validate().context("validating mls range low categories")?;

        if let Some(high_categories) = &self.high_categories {
            high_categories.validate().context("validating mls range high categories")?;
        }

        Ok(())
    }
}

#[derive(AsBytes, Debug, FromZeroes, FromBytes, PartialEq, Unaligned)]
#[repr(C, packed)]
pub(crate) struct MLSRangeMetadata {
    count: le::U32,
    sensitivity_low: le::U32,
    sensitivity_high: le::U32,
}

impl Validate for MLSRangeMetadata {
    type Error = ParseError;

    /// TODO: Validate [`MLSRangeMetadata`] internals.
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(Debug, PartialEq)]
pub(crate) struct MLSLevel<B: ByteSlice + Debug + PartialEq> {
    sensitivity: Ref<B, le::U32>,
    categories: ExtensibleBitmap<B>,
}

impl<B: ByteSlice + Debug + PartialEq> Parse<B> for MLSLevel<B> {
    type Error = anyhow::Error;

    fn parse(bytes: B) -> Result<(Self, B), Self::Error> {
        let tail = bytes;

        let num_bytes = tail.len();
        let (sensitivity, tail) =
            Ref::<B, le::U32>::new_unaligned_from_prefix(tail).ok_or(ParseError::MissingData {
                type_name: "MLSLevelSensitivity",
                type_size: std::mem::size_of::<le::U32>(),
                num_bytes,
            })?;

        let (categories, tail) =
            ExtensibleBitmap::parse(tail).context("parsing mls level categories")?;
        categories.validate().context("validating mls level categories")?;

        Ok((Self { sensitivity, categories }, tail))
    }
}

impl<B: ByteSlice + Debug + PartialEq> Validate for MLSLevel<B> {
    type Error = anyhow::Error;

    /// TODO: Validate `self.sensitivity` and its relationship to `self.categories`.
    fn validate(&self) -> Result<(), Self::Error> {
        self.categories.validate().context("validating mls level categories")
    }
}

pub(crate) type ConditionalBooleans<B> = Vec<ConditionalBoolean<B>>;

impl<B: ByteSlice + Debug + PartialEq> Validate for ConditionalBooleans<B> {
    type Error = ParseError;

    /// TODO: Validate consistency of sequence of [`ConditionalBoolean`].
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

pub(crate) type ConditionalBoolean<B> = Array<B, Ref<B, ConditionalBooleanMetadata>, Ref<B, [u8]>>;

impl<B: ByteSlice + Debug + PartialEq> Validate for ConditionalBoolean<B> {
    type Error = ParseError;

    /// TODO: Validate consistency between [`ConditionalBooleanMetadata`] and `[u8]` key.
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(AsBytes, Debug, FromZeroes, FromBytes, PartialEq, Unaligned)]
#[repr(C, packed)]
pub(crate) struct ConditionalBooleanMetadata {
    value: le::U32,
    state: le::U32,
    length: le::U32,
}

impl Counted for ConditionalBooleanMetadata {
    /// [`ConditionalBooleanMetadata`] used as `M` in of `Array<B, Ref<B, M>, Ref<B, [u8]>>` with
    /// `self.length` denoting size of inner `[u8]`.
    fn count(&self) -> u32 {
        self.length.get()
    }
}

impl Validate for ConditionalBooleanMetadata {
    type Error = ParseError;

    /// TODO: Validate internal consistency of [`ConditionalBooleanMetadata`].
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

pub(crate) type Sensitivities<B> = Vec<Sensitivity<B>>;

impl<B: ByteSlice + Debug + PartialEq> Validate for Sensitivities<B> {
    type Error = ParseError;

    /// TODO: Validate consistency of sequence of [`Sensitivity`].
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(Debug, PartialEq)]
pub(crate) struct Sensitivity<B: ByteSlice + Debug + PartialEq> {
    metadata: SensitivityMetadata<B>,
    level: MLSLevel<B>,
}

impl<B: ByteSlice + Debug + PartialEq> Parse<B> for Sensitivity<B> {
    type Error = anyhow::Error;

    fn parse(bytes: B) -> Result<(Self, B), Self::Error> {
        let tail = bytes;

        let (metadata, tail) =
            SensitivityMetadata::parse(tail).context("parsing sensitivity metadata")?;

        let (level, tail) = MLSLevel::parse(tail).context("parsing sensitivity mls level")?;

        Ok((Self { metadata, level }, tail))
    }
}

impl<B: ByteSlice + Debug + PartialEq> Validate for Sensitivity<B> {
    type Error = ParseError;

    /// TODO: Validate internal consistency of `self.metadata` and `self.level`.
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

pub(crate) type SensitivityMetadata<B> = Array<B, Ref<B, SensitivityStaticMetadata>, Ref<B, [u8]>>;

impl<B: ByteSlice + Debug + PartialEq> Validate for SensitivityMetadata<B> {
    type Error = ParseError;

    /// TODO: Validate consistency between [`SensitivityMetadata`] and `[u8]` key.
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(AsBytes, Debug, FromZeroes, FromBytes, PartialEq, Unaligned)]
#[repr(C, packed)]
pub(crate) struct SensitivityStaticMetadata {
    length: le::U32,
    is_alias: le::U32,
}

impl Counted for SensitivityStaticMetadata {
    /// [`SensitivityStaticMetadata`] used as `M` in of `Array<B, Ref<B, M>, Ref<B, [u8]>>` with
    /// `self.length` denoting size of inner `[u8]`.
    fn count(&self) -> u32 {
        self.length.get()
    }
}

impl Validate for SensitivityStaticMetadata {
    type Error = ParseError;

    /// TODO: Validate internal consistency of [`SensitivityStaticMetadata`].
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

pub(crate) type Categories<B> = Vec<Category<B>>;

impl<B: ByteSlice + Debug + PartialEq> Validate for Categories<B> {
    type Error = ParseError;

    /// TODO: Validate consistency of sequence of [`Category`].
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

pub(crate) type Category<B> = Array<B, Ref<B, CategoryMetadata>, Ref<B, [u8]>>;

impl<B: ByteSlice + Debug + PartialEq> Validate for Category<B> {
    type Error = ParseError;

    /// TODO: Validate consistency between [`CategoryMetadata`] and `[u8]` key.
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(AsBytes, Debug, FromZeroes, FromBytes, PartialEq, Unaligned)]
#[repr(C, packed)]
pub(crate) struct CategoryMetadata {
    length: le::U32,
    value: le::U32,
    is_alias: le::U32,
}

impl Counted for CategoryMetadata {
    /// [`CategoryMetadata`] used as `M` in of `Array<B, Ref<B, M>, Ref<B, [u8]>>` with
    /// `self.length` denoting size of inner `[u8]`.
    fn count(&self) -> u32 {
        self.length.get()
    }
}

impl Validate for CategoryMetadata {
    type Error = ParseError;

    /// TODO: Validate internal consistency of [`CategoryMetadata`].
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}
