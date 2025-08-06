// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::constraints::{evaluate_constraint, ConstraintError};
use super::error::{ParseError, ValidateError};
use super::extensible_bitmap::{
    ExtensibleBitmap, ExtensibleBitmapSpan, ExtensibleBitmapSpansIterator,
};
use super::parser::PolicyCursor;
use super::security_context::{CategoryIterator, Level, SecurityContext};
use super::{
    array_type, array_type_validate_deref_both, array_type_validate_deref_data,
    array_type_validate_deref_metadata_data_vec, array_type_validate_deref_none_data_vec,
    AccessVector, Array, CategoryId, ClassId, ClassPermissionId, Counted, Parse,
    PolicyValidationContext, RoleId, SensitivityId, TypeId, UserId, Validate, ValidateArray,
};

use anyhow::{anyhow, Context as _};
use std::fmt::Debug;
use std::num::NonZeroU32;
use std::ops::Deref;
use zerocopy::{little_endian as le, FromBytes, Immutable, KnownLayout, Unaligned};

/// ** Constraint term types ***
///
/// The `constraint_term_type` metadata field value for a [`ConstraintTerm`]
/// that represents the "not" operator.
pub(super) const CONSTRAINT_TERM_TYPE_NOT_OPERATOR: u32 = 1;
/// The `constraint_term_type` metadata field value for a [`ConstraintTerm`]
/// that represents the "and" operator.
pub(super) const CONSTRAINT_TERM_TYPE_AND_OPERATOR: u32 = 2;
/// The `constraint_term_type` metadata field value for a [`ConstraintTerm`]
/// that represents the "or" operator.
pub(super) const CONSTRAINT_TERM_TYPE_OR_OPERATOR: u32 = 3;
/// The `constraint_term_type` metadata field value for a [`ConstraintTerm`]
/// that represents a boolean expression where both arguments are fields of
/// a source and/or target security context.
pub(super) const CONSTRAINT_TERM_TYPE_EXPR: u32 = 4;
/// The `constraint_term_type` metadata field value for a [`ConstraintTerm`]
/// that represents a boolean expression where:
///
/// - the left-hand side is the user, role, or type of the source or target
///   security context
/// - the right-hand side is a set of users, roles, or types that are
///   specified by name in the text policy, independent of the source
///   or target security context.
///
/// In this case, the [`ConstraintTerm`] contains an [`ExtensibleBitmap`] that
/// encodes the set of user, role, or type IDs corresponding to the names, and a
/// [`TypeSet`] encoding the corresponding set of types.
pub(super) const CONSTRAINT_TERM_TYPE_EXPR_WITH_NAMES: u32 = 5;

/// ** Constraint expression operator types ***
///
/// Valid `expr_operator_type` metadata field values for a [`ConstraintTerm`]
/// with `type` equal to `CONSTRAINT_TERM_TYPE_EXPR` or
/// `CONSTRAINT_TERM_TYPE_EXPR_WITH_NAMES`.
///
/// NB. `EXPR_OPERATOR_TYPE_{DOM,DOMBY,INCOMP}` were previously valid for
///      constraints on role IDs, but this was deprecated as of SELinux
///      policy version 26.
///
/// The `expr_operator_type` value for an expression of form "A == B".
/// Valid for constraints on user, role, and type IDs.
pub(super) const CONSTRAINT_EXPR_OPERATOR_TYPE_EQ: u32 = 1;
/// The `expr_operator_type` value for an expression of form "A != B".
/// Valid for constraints on user, role, and type IDs.
pub(super) const CONSTRAINT_EXPR_OPERATOR_TYPE_NE: u32 = 2;
/// The `expr_operator_type` value for an expression of form "A dominates B".
/// Valid for constraints on security levels.
pub(super) const CONSTRAINT_EXPR_OPERATOR_TYPE_DOM: u32 = 3;
/// The `expr_operator_type` value for an expression of form "A is dominated
/// by B".
/// Valid for constraints on security levels.
pub(super) const CONSTRAINT_EXPR_OPERATOR_TYPE_DOMBY: u32 = 4;
/// The `expr_operator_type` value for an expression of form "A is
/// incomparable to B".
/// Valid for constraints on security levels.
pub(super) const CONSTRAINT_EXPR_OPERATOR_TYPE_INCOMP: u32 = 5;

/// ** Constraint expression types ***
///
/// Although these values each have a single bit set, they appear to be
/// used as enum values rather than as bit masks: i.e., the policy compiler
/// does not produce access vector rule structures that have more than
/// one of these types.
///
/// Valid `expr_operand_type` metadata field values for a [`ConstraintTerm`]
/// with `constraint_term_type` equal to `CONSTRAINT_TERM_TYPE_EXPR` or
/// `CONSTRAINT_TERM_TYPE_EXPR_WITH_NAMES`.
///
/// When the `constraint_term_type` is equal to `CONSTRAINT_TERM_TYPE_EXPR` and
/// the `expr_operand_type` value is `EXPR_OPERAND_TYPE_{USER,ROLE,TYPE}`, the
/// expression compares the source's {user,role,type} ID to the target's
/// {user,role,type} ID.
///
/// When the `constraint_term_type` is equal to
/// `CONSTRAINT_TERM_TYPE_EXPR_WITH_NAMES`, then the right-hand side of the
/// expression is the set of IDs listed in the [`ConstraintTerm`]'s `names`
/// field. The left-hand side of the expression is the user, role, or type ID of
/// either the target security context, or the source security context,
/// depending on whether the `EXPR_WITH_NAMES_OPERAND_TYPE_TARGET_MASK` bit of
/// the `expr_operand_type` field is set (--> target) or not (--> source).
///
/// The `expr_operand_type` value for an expression comparing user IDs.
pub(super) const CONSTRAINT_EXPR_OPERAND_TYPE_USER: u32 = 0x1;
/// The `expr_operand_type` value for an expression comparing role IDs.
pub(super) const CONSTRAINT_EXPR_OPERAND_TYPE_ROLE: u32 = 0x2;
/// The `expr_operand_type` value for an expression comparing type IDs.
pub(super) const CONSTRAINT_EXPR_OPERAND_TYPE_TYPE: u32 = 0x4;
/// The `expr_operand_type` value for an expression comparing the source
/// context's low security level to the target context's low security level.
pub(super) const CONSTRAINT_EXPR_OPERAND_TYPE_L1_L2: u32 = 0x20;
/// The `expr_operand_type` value for an expression comparing the source
/// context's low security level to the target context's high security level.
pub(super) const CONSTRAINT_EXPR_OPERAND_TYPE_L1_H2: u32 = 0x40;
/// The `expr_operand_type` value for an expression comparing the source
/// context's high security level to the target context's low security level.
pub(super) const CONSTRAINT_EXPR_OPERAND_TYPE_H1_L2: u32 = 0x80;
/// The `expr_operand_type` value for an expression comparing the source
/// context's high security level to the target context's high security level.
pub(super) const CONSTRAINT_EXPR_OPERAND_TYPE_H1_H2: u32 = 0x100;
/// The `expr_operand_type` value for an expression comparing the source
/// context's low security level to the source context's high security level.
pub(super) const CONSTRAINT_EXPR_OPERAND_TYPE_L1_H1: u32 = 0x200;
/// The `expr_operand_type` value for an expression comparing the target
/// context's low security level to the target context's high security level.
pub(super) const CONSTRAINT_EXPR_OPERAND_TYPE_L2_H2: u32 = 0x400;

/// For a [`ConstraintTerm`] with `constraint_term_type` equal to
/// `CONSTRAINT_TERM_TYPE_EXPR_WITH_NAMES` the `expr_operand_type` may have the
/// `EXPR_WITH_NAMES_OPERAND_TYPE_TARGET_MASK` bit set in addition to one of the
/// `EXPR_OPERAND_TYPE_{USER,ROLE,TYPE}` bits.
///
/// If the `EXPR_WITH_NAMES_OPERAND_TYPE_TARGET_MASK` bit is set, then the
/// expression compares the target's {user,role,type} ID to the set of IDs
/// listed in the [`ConstraintTerm`]'s `names` field.
///
/// If the bit is not set, then the expression compares the source's
/// {user,role,type} ID to the set of IDs listed in the [`ConstraintTerm`]'s
/// `names` field.
pub(super) const CONSTRAINT_EXPR_WITH_NAMES_OPERAND_TYPE_TARGET_MASK: u32 = 0x8;

/// Exact value of [`Type`] `properties` when the underlying data refers to an SELinux type.
pub(super) const TYPE_PROPERTIES_TYPE: u32 = 1;

/// Exact value of [`Type`] `properties` when the underlying data refers to an SELinux alias.
pub(super) const TYPE_PROPERTIES_ALIAS: u32 = 0;

/// Exact value of [`Type`] `properties` when the underlying data refers to an SELinux attribute.
pub(super) const TYPE_PROPERTIES_ATTRIBUTE: u32 = 0;

/// [`SymbolList`] is an [`Array`] of items with the count of items determined by [`Metadata`] as
/// [`Counted`].
#[derive(Debug, PartialEq)]
pub(super) struct SymbolList<T>(Array<Metadata, Vec<T>>);

impl<T> Deref for SymbolList<T> {
    type Target = Array<Metadata, Vec<T>>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<T: Parse> Parse for SymbolList<T> {
    type Error = <Array<Metadata, Vec<T>> as Parse>::Error;

    fn parse(bytes: PolicyCursor) -> Result<(Self, PolicyCursor), Self::Error> {
        let (array, tail) = Array::<Metadata, Vec<T>>::parse(bytes)?;
        Ok((Self(array), tail))
    }
}

impl<T> Validate for SymbolList<T>
where
    [T]: Validate,
{
    type Error = anyhow::Error;

    /// [`SymbolList`] has no internal constraints beyond those imposed by [`Array`].
    fn validate(&self, context: &mut PolicyValidationContext) -> Result<(), Self::Error> {
        self.metadata.validate(context).map_err(Into::<anyhow::Error>::into)?;
        self.data.as_slice().validate(context).map_err(Into::<anyhow::Error>::into)?;

        Ok(())
    }
}

/// Binary metadata prefix to [`SymbolList`] objects.
#[derive(Clone, Debug, KnownLayout, FromBytes, Immutable, PartialEq, Unaligned)]
#[repr(C, packed)]
pub(super) struct Metadata {
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
    fn validate(&self, _context: &mut PolicyValidationContext) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl Validate for [CommonSymbol] {
    type Error = <CommonSymbol as Validate>::Error;

    /// [`CommonSymbols`] have no internal constraints beyond those imposed by individual
    /// [`CommonSymbol`] objects.
    fn validate(&self, _context: &mut PolicyValidationContext) -> Result<(), Self::Error> {
        Ok(())
    }
}

array_type!(CommonSymbol, CommonSymbolMetadata, Permissions);

array_type_validate_deref_none_data_vec!(CommonSymbol);

impl CommonSymbol {
    pub fn permissions(&self) -> &Permissions {
        &self.data
    }
}

pub(super) type CommonSymbols = Vec<CommonSymbol>;

impl CommonSymbol {
    /// Returns the name of this common symbol (a string), encoded a borrow of a byte slice. For
    /// example, the policy statement `common file { common_file_perm }` induces a [`CommonSymbol`]
    /// where `name_bytes() == "file".as_slice()`.
    pub fn name_bytes(&self) -> &[u8] {
        &self.metadata.data
    }
}

impl Counted for CommonSymbol {
    /// The count of items in the associated [`Permissions`] is exposed via
    /// `CommonSymbolMetadata::count()`.
    fn count(&self) -> u32 {
        self.metadata.count()
    }
}

impl ValidateArray<CommonSymbolMetadata, Permission> for CommonSymbol {
    type Error = anyhow::Error;

    /// [`CommonSymbol`] have no internal constraints beyond those imposed by [`Array`].
    fn validate_array(
        _context: &mut PolicyValidationContext,
        _metadata: &CommonSymbolMetadata,
        _items: &[Permission],
    ) -> Result<(), Self::Error> {
        Ok(())
    }
}

array_type!(CommonSymbolMetadata, CommonSymbolStaticMetadata, Vec<u8>);

array_type_validate_deref_both!(CommonSymbolMetadata);

impl Counted for CommonSymbolMetadata {
    /// The count of items in the associated [`Permissions`] is stored in the associated
    /// `CommonSymbolStaticMetadata::count` field.
    fn count(&self) -> u32 {
        self.metadata.count.get()
    }
}

impl ValidateArray<CommonSymbolStaticMetadata, u8> for CommonSymbolMetadata {
    type Error = anyhow::Error;

    /// Array of [`u8`] sized by [`CommonSymbolStaticMetadata`] requires no additional validation.
    fn validate_array(
        _context: &mut PolicyValidationContext,
        _metadata: &CommonSymbolStaticMetadata,
        _items: &[u8],
    ) -> Result<(), Self::Error> {
        Ok(())
    }
}

/// Static (that is, fixed-sized) metadata for a common symbol.
#[derive(Clone, Debug, KnownLayout, FromBytes, Immutable, PartialEq, Unaligned)]
#[repr(C, packed)]
pub(super) struct CommonSymbolStaticMetadata {
    /// The length of the `[u8]` key stored in the associated [`CommonSymbolMetadata`].
    length: le::U32,
    /// An integer that identifies this this common symbol, unique to this common symbol relative
    /// to all common symbols and classes in this policy.
    id: le::U32,
    /// The number of primary names referred to by the associated [`CommonSymbol`].
    primary_names_count: le::U32,
    /// The number of items stored in the [`Permissions`] in the associated [`CommonSymbol`].
    count: le::U32,
}

impl Validate for CommonSymbolStaticMetadata {
    type Error = anyhow::Error;

    /// TODO: Should there be an upper bound on `length`?
    fn validate(&self, _context: &mut PolicyValidationContext) -> Result<(), Self::Error> {
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
pub(super) type Permissions = Vec<Permission>;

impl Validate for Permissions {
    type Error = anyhow::Error;

    /// [`Permissions`] have no internal constraints beyond those imposed by individual
    /// [`Permission`] objects.
    fn validate(&self, _context: &mut PolicyValidationContext) -> Result<(), Self::Error> {
        Ok(())
    }
}

array_type!(Permission, PermissionMetadata, Vec<u8>);

array_type_validate_deref_both!(Permission);

impl Permission {
    /// Returns the name of this permission (a string), encoded a borrow of a byte slice. For
    /// example the class named `"file"` class has a permission named `"entrypoint"` and the
    /// `"process"` class has a permission named `"fork"`.
    pub fn name_bytes(&self) -> &[u8] {
        &self.data
    }

    /// Returns the ID of this permission in the scope of its associated class.
    pub fn id(&self) -> ClassPermissionId {
        ClassPermissionId(NonZeroU32::new(self.metadata.id.get()).unwrap())
    }
}

impl ValidateArray<PermissionMetadata, u8> for Permission {
    type Error = anyhow::Error;

    /// [`Permission`] has no internal constraints beyond those imposed by [`Array`].
    fn validate_array(
        _context: &mut PolicyValidationContext,
        _metadata: &PermissionMetadata,
        _items: &[u8],
    ) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(Clone, Debug, KnownLayout, FromBytes, Immutable, PartialEq, Unaligned)]
#[repr(C, packed)]
pub(super) struct PermissionMetadata {
    /// The length of the `[u8]` in the associated [`Permission`].
    length: le::U32,
    id: le::U32,
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
    fn validate(&self, _context: &mut PolicyValidationContext) -> Result<(), Self::Error> {
        Ok(())
    }
}

/// The list of [`Constraints`] associated with a class.
pub(super) type Constraints = Vec<Constraint>;

impl Validate for Constraints {
    type Error = anyhow::Error;

    /// [`Constraints`] has no internal constraints beyond those imposed by individual
    /// [`Constraint`] objects.
    fn validate(&self, _context: &mut PolicyValidationContext) -> Result<(), Self::Error> {
        Ok(())
    }
}

/// A set of permissions and a boolean expression giving a constraint on those
/// permissions, for a particular class. Corresponds to a single `constrain` or
/// `mlsconstrain` statement in policy language.
#[derive(Debug, PartialEq)]
pub(super) struct Constraint {
    access_vector: le::U32,
    constraint_expr: ConstraintExpr,
}

impl Constraint {
    pub(super) fn access_vector(&self) -> AccessVector {
        AccessVector(self.access_vector.get())
    }

    pub(super) fn constraint_expr(&self) -> &ConstraintExpr {
        &self.constraint_expr
    }
}

impl Parse for Constraint {
    type Error = anyhow::Error;

    fn parse(bytes: PolicyCursor) -> Result<(Self, PolicyCursor), Self::Error> {
        let tail = bytes;

        let num_bytes = tail.len();
        let (access_vector, tail) = PolicyCursor::parse::<le::U32>(tail).ok_or_else(|| {
            Into::<anyhow::Error>::into(ParseError::MissingData {
                type_name: "AccessVector",
                type_size: std::mem::size_of::<le::U32>(),
                num_bytes,
            })
        })?;
        let (constraint_expr, tail) = ConstraintExpr::parse(tail)
            .map_err(|error| anyhow!(error))
            .context("parsing constraint expression")?;

        Ok((Self { access_vector, constraint_expr }, tail))
    }
}

// A [`ConstraintExpr`] describes a constraint expression, represented as a
// postfix-ordered list of terms.
array_type!(ConstraintExpr, ConstraintTermCount, ConstraintTerms);

array_type_validate_deref_metadata_data_vec!(ConstraintExpr);

impl ValidateArray<ConstraintTermCount, ConstraintTerm> for ConstraintExpr {
    type Error = anyhow::Error;

    /// [`ConstraintExpr`] has no internal constraints beyond those imposed by
    /// [`Array`]. The `ParsedPolicy::validate()` function separately validates
    /// that the constraint expression is well-formed.
    fn validate_array(
        _context: &mut PolicyValidationContext,
        _metadata: &ConstraintTermCount,
        _items: &[ConstraintTerm],
    ) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl ConstraintExpr {
    pub(super) fn evaluate(
        &self,
        source_context: &SecurityContext,
        target_context: &SecurityContext,
    ) -> Result<bool, ConstraintError> {
        evaluate_constraint(&self, source_context, target_context)
    }

    pub(super) fn constraint_terms(&self) -> &[ConstraintTerm] {
        &self.data
    }
}

#[derive(Clone, Debug, KnownLayout, FromBytes, Immutable, PartialEq, Unaligned)]
#[repr(C, packed)]
pub(super) struct ConstraintTermCount(le::U32);

impl Counted for ConstraintTermCount {
    fn count(&self) -> u32 {
        self.0.get()
    }
}

impl Validate for ConstraintTermCount {
    type Error = anyhow::Error;

    fn validate(&self, _context: &mut PolicyValidationContext) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl Validate for ConstraintTerms {
    type Error = anyhow::Error;

    /// [`ConstraintTerms`] have no internal constraints beyond those imposed by
    /// individual [`ConstraintTerm`] objects. The `ParsedPolicy::validate()`
    /// function separately validates that the constraint expression is
    /// well-formed.
    fn validate(&self, _context: &mut PolicyValidationContext) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(Debug, PartialEq)]
pub(super) struct ConstraintTerm {
    metadata: ConstraintTermMetadata,
    names: Option<ExtensibleBitmap>,
    names_type_set: Option<TypeSet>,
}

pub(super) type ConstraintTerms = Vec<ConstraintTerm>;

impl Parse for ConstraintTerm {
    type Error = anyhow::Error;

    fn parse(bytes: PolicyCursor) -> Result<(Self, PolicyCursor), Self::Error> {
        let tail = bytes;

        let (metadata, tail) = PolicyCursor::parse::<ConstraintTermMetadata>(tail)
            .context("parsing constraint term metadata")?;

        let (names, names_type_set, tail) = match metadata.constraint_term_type.get() {
            CONSTRAINT_TERM_TYPE_EXPR_WITH_NAMES => {
                let (names, tail) = ExtensibleBitmap::parse(tail)
                    .map_err(Into::<anyhow::Error>::into)
                    .context("parsing constraint term names")?;
                let (names_type_set, tail) =
                    TypeSet::parse(tail).context("parsing constraint term names type set")?;
                (Some(names), Some(names_type_set), tail)
            }
            _ => (None, None, tail),
        };

        Ok((Self { metadata, names, names_type_set }, tail))
    }
}

impl ConstraintTerm {
    pub(super) fn constraint_term_type(&self) -> u32 {
        self.metadata.constraint_term_type.get()
    }

    pub(super) fn expr_operand_type(&self) -> u32 {
        self.metadata.expr_operand_type.get()
    }

    pub(super) fn expr_operator_type(&self) -> u32 {
        self.metadata.expr_operator_type.get()
    }

    pub(super) fn names(&self) -> Option<&ExtensibleBitmap> {
        self.names.as_ref()
    }

    // TODO: https://fxbug.dev/372400976 - Unused, unsure if needed.
    // Possibly becomes interesting when the policy contains type
    // attributes.
    #[allow(dead_code)]
    pub(super) fn names_type_set(&self) -> &Option<TypeSet> {
        &self.names_type_set
    }
}

#[derive(Clone, Debug, KnownLayout, FromBytes, Immutable, PartialEq, Unaligned)]
#[repr(C, packed)]
pub(super) struct ConstraintTermMetadata {
    constraint_term_type: le::U32,
    expr_operand_type: le::U32,
    expr_operator_type: le::U32,
}

impl Validate for ConstraintTermMetadata {
    type Error = anyhow::Error;

    /// Further validation is done by the `ParsedPolicy::validate()` function,
    /// which separately validates that constraint expressions are well-formed.
    fn validate(&self, _context: &mut PolicyValidationContext) -> Result<(), Self::Error> {
        if !(self.constraint_term_type > 0
            && self.constraint_term_type <= CONSTRAINT_TERM_TYPE_EXPR_WITH_NAMES)
        {
            return Err(anyhow!("invalid constraint term type"));
        }
        if !(self.constraint_term_type == CONSTRAINT_TERM_TYPE_EXPR
            || self.constraint_term_type == CONSTRAINT_TERM_TYPE_EXPR_WITH_NAMES)
        {
            if self.expr_operand_type != 0 {
                return Err(anyhow!(
                    "invalid operand type {} for constraint term type {}",
                    self.expr_operand_type,
                    self.constraint_term_type
                ));
            }
            if self.expr_operator_type != 0 {
                return Err(anyhow!(
                    "invalid operator type {} for constraint term type {}",
                    self.expr_operator_type,
                    self.constraint_term_type
                ));
            }
        }
        // TODO: https://fxbug.dev/372400976 - Consider validating operator
        // and operand types for expr and expr-with-names terms.
        Ok(())
    }
}

#[derive(Debug, PartialEq)]
pub(super) struct TypeSet {
    types: ExtensibleBitmap,
    negative_set: ExtensibleBitmap,
    flags: le::U32,
}

impl Parse for TypeSet {
    type Error = anyhow::Error;

    fn parse(bytes: PolicyCursor) -> Result<(Self, PolicyCursor), Self::Error> {
        let tail = bytes;

        let (types, tail) = ExtensibleBitmap::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing type set types")?;

        let (negative_set, tail) = ExtensibleBitmap::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing type set negative set")?;

        let num_bytes = tail.len();
        let (flags, tail) = PolicyCursor::parse::<le::U32>(tail).ok_or_else(|| {
            Into::<anyhow::Error>::into(ParseError::MissingData {
                type_name: "TypeSetFlags",
                type_size: std::mem::size_of::<le::U32>(),
                num_bytes,
            })
        })?;

        Ok((Self { types, negative_set, flags }, tail))
    }
}

/// Locates a class named `name` among `classes`. Returns the first such class found, though policy
/// validation should ensure that only one such class exists.
pub(super) fn find_class_by_name<'a>(classes: &'a Classes, name: &str) -> Option<&'a Class> {
    find_class_by_name_bytes(classes, name.as_bytes())
}

fn find_class_by_name_bytes<'a>(classes: &'a Classes, name_bytes: &[u8]) -> Option<&'a Class> {
    for cls in classes.into_iter() {
        if cls.name_bytes() == name_bytes {
            return Some(cls);
        }
    }

    None
}

/// Locates a symbol named `name_bytes` among `common_symbols`. Returns
/// the first such symbol found, though policy validation should ensure
/// that only one exists.
pub(super) fn find_common_symbol_by_name_bytes<'a>(
    common_symbols: &'a CommonSymbols,
    name_bytes: &[u8],
) -> Option<&'a CommonSymbol> {
    for common_symbol in common_symbols.into_iter() {
        if common_symbol.name_bytes() == name_bytes {
            return Some(common_symbol);
        }
    }

    None
}

impl Validate for [Class] {
    type Error = anyhow::Error;

    fn validate(&self, context: &mut PolicyValidationContext) -> Result<(), Self::Error> {
        // TODO: Validate internal consistency between consecutive [`Class`] instances.
        for class in self {
            // TODO: Validate `self.constraints` and `self.validate_transitions`.
            class.defaults().validate(context).context("class defaults")?;
        }
        Ok(())
    }
}

#[derive(Debug, PartialEq)]
pub(super) struct Class {
    constraints: ClassConstraints,
    validate_transitions: ClassValidateTransitions,
    defaults: ClassDefaults,
}

pub(super) type Classes = Vec<Class>;

impl Class {
    /// Returns the name of the `common` from which this `class` inherits as a borrow of a byte
    /// slice. For example, `common file { common_file_perm }`,
    /// `class file inherits file { file_perm }` yields two [`Class`] objects, one that refers to a
    /// permission named `"common_file_perm"` permission and has `self.common_name_bytes() == ""`,
    /// and another that refers to a permission named `"file_perm"` and has
    /// `self.common_name_bytes() == "file"`.
    pub fn common_name_bytes(&self) -> &[u8] {
        // `ClassCommonKey` is an `Array` of `[u8]` with metadata `ClassKey`, and
        // `ClassKey::count()` returns the `common_key_length` field. That is, the `[u8]` string
        // on `ClassCommonKey` is the "common key" (name in the inherited `common` statement) for
        // this class.
        let class_common_key: &ClassCommonKey = &self.constraints.metadata.metadata;
        &class_common_key.data
    }

    /// Returns the name of this class as a borrow of a byte slice.
    pub fn name_bytes(&self) -> &[u8] {
        // `ClassKey` is an `Array` of `[u8]` with metadata `ClassMetadata`, and
        // `ClassMetadata::count()` returns the `key_length` field. That is, the `[u8]` string on
        // `ClassKey` is the "class key" (name in the `class` or `common` statement) for this class.
        let class_key: &ClassKey = &self.constraints.metadata.metadata.metadata;
        &class_key.data
    }

    /// Returns the id associated with this class. The id is used to index into collections
    /// and bitmaps associated with this class. The id is 1-indexed, whereas most collections and
    /// bitmaps are 0-indexed, so clients of this API will usually use `id - 1`.
    pub fn id(&self) -> ClassId {
        let class_metadata: &ClassMetadata = &self.constraints.metadata.metadata.metadata.metadata;
        ClassId(NonZeroU32::new(class_metadata.id.get()).unwrap())
    }

    /// Returns the full listing of permissions used in this policy.
    pub fn permissions(&self) -> &Permissions {
        &self.constraints.metadata.data
    }

    /// Returns a list of permission masks and constraint expressions for this
    /// class. The permissions in a given mask may be granted if the
    /// corresponding constraint expression is satisfied.
    ///
    /// The same permission may appear in multiple entries in the returned list.
    // TODO: https://fxbug.dev/372400976 - Is it accurate to change "may be
    // granted to "are granted" above?
    pub fn constraints(&self) -> &Vec<Constraint> {
        &self.constraints.data
    }

    pub fn defaults(&self) -> &ClassDefaults {
        &self.defaults
    }
}

impl Parse for Class {
    type Error = anyhow::Error;

    fn parse(bytes: PolicyCursor) -> Result<(Self, PolicyCursor), Self::Error> {
        let tail = bytes;

        let (constraints, tail) = ClassConstraints::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing class constraints")?;

        let (validate_transitions, tail) = ClassValidateTransitions::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing class validate transitions")?;

        let (defaults, tail) =
            PolicyCursor::parse::<ClassDefaults>(tail).context("parsing class defaults")?;

        Ok((Self { constraints, validate_transitions, defaults }, tail))
    }
}

#[derive(Clone, Debug, KnownLayout, FromBytes, Immutable, PartialEq, Unaligned)]
#[repr(C, packed)]
pub(super) struct ClassDefaults {
    default_user: le::U32,
    default_role: le::U32,
    default_range: le::U32,
    default_type: le::U32,
}

impl ClassDefaults {
    pub fn user(&self) -> ClassDefault {
        self.default_user.get().into()
    }

    pub fn role(&self) -> ClassDefault {
        self.default_role.get().into()
    }

    pub fn range(&self) -> ClassDefaultRange {
        self.default_range.get().into()
    }

    pub fn type_(&self) -> ClassDefault {
        self.default_type.get().into()
    }
}

impl Validate for ClassDefaults {
    type Error = anyhow::Error;

    fn validate(&self, _context: &mut PolicyValidationContext) -> Result<(), Self::Error> {
        ClassDefault::validate(self.default_user.get()).context("default user")?;
        ClassDefault::validate(self.default_role.get()).context("default role")?;
        ClassDefault::validate(self.default_type.get()).context("default type")?;
        ClassDefaultRange::validate(self.default_range.get()).context("default range")?;
        Ok(())
    }
}

#[derive(PartialEq)]
pub(super) enum ClassDefault {
    Unspecified,
    Source,
    Target,
}

impl ClassDefault {
    pub(super) const DEFAULT_UNSPECIFIED: u32 = 0;
    pub(super) const DEFAULT_SOURCE: u32 = 1;
    pub(super) const DEFAULT_TARGET: u32 = 2;

    fn validate(value: u32) -> Result<(), ValidateError> {
        match value {
            Self::DEFAULT_UNSPECIFIED | Self::DEFAULT_SOURCE | Self::DEFAULT_TARGET => Ok(()),
            value => Err(ValidateError::InvalidClassDefault { value }),
        }
    }
}

impl From<u32> for ClassDefault {
    fn from(value: u32) -> Self {
        match value {
            Self::DEFAULT_UNSPECIFIED => Self::Unspecified,
            Self::DEFAULT_SOURCE => Self::Source,
            Self::DEFAULT_TARGET => Self::Target,
            v => panic!(
                "invalid SELinux class default; expected {}, {}, or {}, but got {}",
                Self::DEFAULT_UNSPECIFIED,
                Self::DEFAULT_SOURCE,
                Self::DEFAULT_TARGET,
                v
            ),
        }
    }
}

#[derive(PartialEq)]
pub(super) enum ClassDefaultRange {
    Unspecified,
    SourceLow,
    SourceHigh,
    SourceLowHigh,
    TargetLow,
    TargetHigh,
    TargetLowHigh,
}

impl ClassDefaultRange {
    pub(super) const DEFAULT_UNSPECIFIED: u32 = 0;
    pub(super) const DEFAULT_SOURCE_LOW: u32 = 1;
    pub(super) const DEFAULT_SOURCE_HIGH: u32 = 2;
    pub(super) const DEFAULT_SOURCE_LOW_HIGH: u32 = 3;
    pub(super) const DEFAULT_TARGET_LOW: u32 = 4;
    pub(super) const DEFAULT_TARGET_HIGH: u32 = 5;
    pub(super) const DEFAULT_TARGET_LOW_HIGH: u32 = 6;
    // TODO: Determine what this value means.
    pub(super) const DEFAULT_UNKNOWN_USED_VALUE: u32 = 7;

    fn validate(value: u32) -> Result<(), ValidateError> {
        match value {
            Self::DEFAULT_UNSPECIFIED
            | Self::DEFAULT_SOURCE_LOW
            | Self::DEFAULT_SOURCE_HIGH
            | Self::DEFAULT_SOURCE_LOW_HIGH
            | Self::DEFAULT_TARGET_LOW
            | Self::DEFAULT_TARGET_HIGH
            | Self::DEFAULT_TARGET_LOW_HIGH
            | Self::DEFAULT_UNKNOWN_USED_VALUE => Ok(()),
            value => Err(ValidateError::InvalidClassDefaultRange { value }),
        }
    }
}

impl From<u32> for ClassDefaultRange {
    fn from(value: u32) -> Self {
        match value {
            Self::DEFAULT_UNSPECIFIED => Self::Unspecified,
            Self::DEFAULT_SOURCE_LOW => Self::SourceLow,
            Self::DEFAULT_SOURCE_HIGH => Self::SourceHigh,
            Self::DEFAULT_SOURCE_LOW_HIGH => Self::SourceLowHigh,
            Self::DEFAULT_TARGET_LOW => Self::TargetLow,
            Self::DEFAULT_TARGET_HIGH => Self::TargetHigh,
            Self::DEFAULT_TARGET_LOW_HIGH => Self::TargetLowHigh,
            v => panic!(
                "invalid SELinux MLS range default; expected one of {:?}, but got {}",
                [
                    Self::DEFAULT_UNSPECIFIED,
                    Self::DEFAULT_SOURCE_LOW,
                    Self::DEFAULT_SOURCE_HIGH,
                    Self::DEFAULT_SOURCE_LOW_HIGH,
                    Self::DEFAULT_TARGET_LOW,
                    Self::DEFAULT_TARGET_HIGH,
                    Self::DEFAULT_TARGET_LOW_HIGH,
                ],
                v
            ),
        }
    }
}

array_type!(ClassValidateTransitions, ClassValidateTransitionsCount, ConstraintTerms);

array_type_validate_deref_metadata_data_vec!(ClassValidateTransitions);

impl ValidateArray<ClassValidateTransitionsCount, ConstraintTerm> for ClassValidateTransitions {
    type Error = anyhow::Error;

    /// [`ClassValidateTransitions`] has no internal constraints beyond those imposed by [`Array`].
    fn validate_array(
        _context: &mut PolicyValidationContext,
        _metadata: &ClassValidateTransitionsCount,
        _items: &[ConstraintTerm],
    ) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(Clone, Debug, KnownLayout, FromBytes, Immutable, PartialEq, Unaligned)]
#[repr(C, packed)]
pub(super) struct ClassValidateTransitionsCount(le::U32);

impl Counted for ClassValidateTransitionsCount {
    fn count(&self) -> u32 {
        self.0.get()
    }
}

impl Validate for ClassValidateTransitionsCount {
    type Error = anyhow::Error;

    /// TODO: Should there be an upper bound on class validate transitions count?
    fn validate(&self, _context: &mut PolicyValidationContext) -> Result<(), Self::Error> {
        Ok(())
    }
}

array_type!(ClassConstraints, ClassPermissions, Constraints);

array_type_validate_deref_none_data_vec!(ClassConstraints);

impl ValidateArray<ClassPermissions, Constraint> for ClassConstraints {
    type Error = anyhow::Error;

    /// [`ClassConstraints`] has no internal constraints beyond those imposed by [`Array`].
    fn validate_array(
        _context: &mut PolicyValidationContext,
        _metadata: &ClassPermissions,
        _items: &[Constraint],
    ) -> Result<(), Self::Error> {
        Ok(())
    }
}

array_type!(ClassPermissions, ClassCommonKey, Permissions);

array_type_validate_deref_none_data_vec!(ClassPermissions);

impl ValidateArray<ClassCommonKey, Permission> for ClassPermissions {
    type Error = anyhow::Error;

    /// [`ClassPermissions`] has no internal constraints beyond those imposed by [`Array`].
    fn validate_array(
        _context: &mut PolicyValidationContext,
        _metadata: &ClassCommonKey,
        _items: &[Permission],
    ) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl Counted for ClassPermissions {
    /// [`ClassPermissions`] acts as counted metadata for [`ClassConstraints`].
    fn count(&self) -> u32 {
        self.metadata.metadata.metadata.constraint_count.get()
    }
}

array_type!(ClassCommonKey, ClassKey, Vec<u8>);

array_type_validate_deref_data!(ClassCommonKey);

impl ValidateArray<ClassKey, u8> for ClassCommonKey {
    type Error = anyhow::Error;

    /// [`ClassCommonKey`] has no internal constraints beyond those imposed by [`Array`].
    fn validate_array(
        _context: &mut PolicyValidationContext,
        _metadata: &ClassKey,
        _items: &[u8],
    ) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl Counted for ClassCommonKey {
    /// [`ClassCommonKey`] acts as counted metadata for [`ClassPermissions`].
    fn count(&self) -> u32 {
        self.metadata.metadata.elements_count.get()
    }
}

array_type!(ClassKey, ClassMetadata, Vec<u8>);

array_type_validate_deref_both!(ClassKey);

impl ValidateArray<ClassMetadata, u8> for ClassKey {
    type Error = anyhow::Error;

    /// [`ClassKey`] has no internal constraints beyond those imposed by [`Array`].
    fn validate_array(
        _context: &mut PolicyValidationContext,
        _metadata: &ClassMetadata,
        _items: &[u8],
    ) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl Counted for ClassKey {
    /// [`ClassKey`] acts as counted metadata for [`ClassCommonKey`].
    fn count(&self) -> u32 {
        self.metadata.common_key_length.get()
    }
}

#[derive(Clone, Debug, KnownLayout, FromBytes, Immutable, PartialEq, Unaligned)]
#[repr(C, packed)]
pub(super) struct ClassMetadata {
    key_length: le::U32,
    common_key_length: le::U32,
    id: le::U32,
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
    fn validate(&self, _context: &mut PolicyValidationContext) -> Result<(), Self::Error> {
        if self.id.get() == 0 {
            return Err(ValidateError::NonOptionalIdIsZero.into());
        } else {
            Ok(())
        }
    }
}

impl Validate for [Role] {
    type Error = anyhow::Error;

    /// TODO: Validate internal consistency between consecutive [`Role`] instances.
    fn validate(&self, _context: &mut PolicyValidationContext) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(Debug, PartialEq)]
pub(super) struct Role {
    metadata: RoleMetadata,
    role_dominates: ExtensibleBitmap,
    role_types: ExtensibleBitmap,
}

impl Role {
    pub(super) fn id(&self) -> RoleId {
        RoleId(NonZeroU32::new(self.metadata.metadata.id.get()).unwrap())
    }

    pub(super) fn name_bytes(&self) -> &[u8] {
        &self.metadata.data
    }

    pub(super) fn types(&self) -> &ExtensibleBitmap {
        &self.role_types
    }
}

impl Parse for Role {
    type Error = anyhow::Error;

    fn parse(bytes: PolicyCursor) -> Result<(Self, PolicyCursor), Self::Error> {
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

array_type!(RoleMetadata, RoleStaticMetadata, Vec<u8>);

array_type_validate_deref_both!(RoleMetadata);

impl ValidateArray<RoleStaticMetadata, u8> for RoleMetadata {
    type Error = anyhow::Error;

    /// [`RoleMetadata`] has no internal constraints beyond those imposed by [`Array`].
    fn validate_array(
        _context: &mut PolicyValidationContext,
        _metadata: &RoleStaticMetadata,
        _items: &[u8],
    ) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(Clone, Debug, KnownLayout, FromBytes, Immutable, PartialEq, Unaligned)]
#[repr(C, packed)]
pub(super) struct RoleStaticMetadata {
    length: le::U32,
    id: le::U32,
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
    fn validate(&self, _context: &mut PolicyValidationContext) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl Validate for [Type] {
    type Error = anyhow::Error;

    /// TODO: Validate internal consistency between consecutive [`Type`] instances.
    fn validate(&self, _context: &mut PolicyValidationContext) -> Result<(), Self::Error> {
        Ok(())
    }
}

array_type!(Type, TypeMetadata, Vec<u8>);

array_type_validate_deref_both!(Type);

impl Type {
    /// Returns the name of this type.
    pub fn name_bytes(&self) -> &[u8] {
        &self.data
    }

    /// Returns the id associated with this type. The id is used to index into collections and
    /// bitmaps associated with this type. The id is 1-indexed, whereas most collections and
    /// bitmaps are 0-indexed, so clients of this API will usually use `id - 1`.
    pub fn id(&self) -> TypeId {
        TypeId(NonZeroU32::new(self.metadata.id.get()).unwrap())
    }

    /// Returns the Id of the bounding type, if any.
    pub fn bounded_by(&self) -> Option<TypeId> {
        NonZeroU32::new(self.metadata.bounds.get()).map(|id| TypeId(id))
    }

    /// Returns whether this type is from a `type [name];` policy statement.
    ///
    /// TODO: Eliminate `dead_code` guard.
    #[allow(dead_code)]
    pub fn is_type(&self) -> bool {
        self.metadata.properties.get() == TYPE_PROPERTIES_TYPE
    }

    /// Returns whether this type is from a `typealias [typename] alias [aliasname];` policy
    /// statement.
    ///
    /// TODO: Eliminate `dead_code` guard.
    #[allow(dead_code)]
    pub fn is_alias(&self) -> bool {
        self.metadata.properties.get() == TYPE_PROPERTIES_ALIAS
    }

    /// Returns whether this type is from an `attribute [name];` policy statement.
    ///
    /// TODO: Eliminate `dead_code` guard.
    #[allow(dead_code)]
    pub fn is_attribute(&self) -> bool {
        self.metadata.properties.get() == TYPE_PROPERTIES_ATTRIBUTE
    }
}

impl ValidateArray<TypeMetadata, u8> for Type {
    type Error = anyhow::Error;

    /// TODO: Validate that `PS::deref(&self.data)` is an ascii string that contains a valid type name.
    fn validate_array(
        _context: &mut PolicyValidationContext,
        _metadata: &TypeMetadata,
        _items: &[u8],
    ) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(Clone, Debug, KnownLayout, FromBytes, Immutable, PartialEq, Unaligned)]
#[repr(C, packed)]
pub(super) struct TypeMetadata {
    length: le::U32,
    id: le::U32,
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
    fn validate(&self, _context: &mut PolicyValidationContext) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl Validate for [User] {
    type Error = anyhow::Error;

    /// TODO: Validate internal consistency between consecutive [`User`] instances.
    fn validate(&self, _context: &mut PolicyValidationContext) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(Debug, PartialEq)]
pub(super) struct User {
    user_data: UserData,
    roles: ExtensibleBitmap,
    expanded_range: MlsRange,
    default_level: MlsLevel,
}

impl User {
    pub(super) fn id(&self) -> UserId {
        UserId(NonZeroU32::new(self.user_data.metadata.id.get()).unwrap())
    }

    pub(super) fn name_bytes(&self) -> &[u8] {
        &self.user_data.data
    }

    pub(super) fn roles(&self) -> &ExtensibleBitmap {
        &self.roles
    }

    pub(super) fn mls_range(&self) -> &MlsRange {
        &self.expanded_range
    }
}

impl Parse for User {
    type Error = anyhow::Error;

    fn parse(bytes: PolicyCursor) -> Result<(Self, PolicyCursor), Self::Error> {
        let tail = bytes;

        let (user_data, tail) = UserData::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing user data")?;

        let (roles, tail) = ExtensibleBitmap::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing user roles")?;

        let (expanded_range, tail) =
            MlsRange::parse(tail).context("parsing user expanded range")?;

        let (default_level, tail) = MlsLevel::parse(tail).context("parsing user default level")?;

        Ok((Self { user_data, roles, expanded_range, default_level }, tail))
    }
}

array_type!(UserData, UserMetadata, Vec<u8>);

array_type_validate_deref_both!(UserData);

impl ValidateArray<UserMetadata, u8> for UserData {
    type Error = anyhow::Error;

    /// TODO: Validate consistency between [`UserMetadata`] in `self.metadata` and `[u8]` key in `self.data`.
    fn validate_array(
        _context: &mut PolicyValidationContext,
        _metadata: &UserMetadata,
        _items: &[u8],
    ) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(Clone, Debug, KnownLayout, FromBytes, Immutable, PartialEq, Unaligned)]
#[repr(C, packed)]
pub(super) struct UserMetadata {
    length: le::U32,
    id: le::U32,
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
    fn validate(&self, _context: &mut PolicyValidationContext) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(Debug, PartialEq)]
pub(super) struct MlsLevel {
    sensitivity: le::U32,
    categories: ExtensibleBitmap,
}

impl MlsLevel {
    pub fn category_ids(&self) -> impl Iterator<Item = CategoryId> + use<'_> {
        self.categories.spans().flat_map(|span| {
            (span.low..=span.high).map(|i| CategoryId(NonZeroU32::new(i + 1).unwrap()))
        })
    }
}

impl Parse for MlsLevel {
    type Error = anyhow::Error;

    fn parse(bytes: PolicyCursor) -> Result<(Self, PolicyCursor), Self::Error> {
        let tail = bytes;

        let num_bytes = tail.len();
        let (sensitivity, tail) =
            PolicyCursor::parse::<le::U32>(tail).ok_or(ParseError::MissingData {
                type_name: "MlsLevelSensitivity",
                type_size: std::mem::size_of::<le::U32>(),
                num_bytes,
            })?;

        let (categories, tail) = ExtensibleBitmap::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing mls level categories")?;

        Ok((Self { sensitivity, categories }, tail))
    }
}

impl<'a> Level<'a, ExtensibleBitmapSpan, ExtensibleBitmapSpansIterator<'a>> for MlsLevel {
    fn sensitivity(&self) -> SensitivityId {
        SensitivityId(NonZeroU32::new(self.sensitivity.get()).unwrap())
    }

    fn category_spans(
        &'a self,
    ) -> CategoryIterator<ExtensibleBitmapSpan, ExtensibleBitmapSpansIterator<'a>> {
        CategoryIterator::new(self.categories.spans())
    }
}

#[derive(Debug, PartialEq)]
pub(super) struct MlsRange {
    count: le::U32,
    low: MlsLevel,
    high: Option<MlsLevel>,
}

impl MlsRange {
    pub fn low(&self) -> &MlsLevel {
        &self.low
    }

    pub fn high(&self) -> &Option<MlsLevel> {
        &self.high
    }
}

impl Parse for MlsRange {
    type Error = anyhow::Error;

    fn parse(bytes: PolicyCursor) -> Result<(Self, PolicyCursor), Self::Error> {
        let tail = bytes;

        let num_bytes = tail.len();
        let (count, tail) =
            PolicyCursor::parse::<le::U32>(tail).ok_or(ParseError::MissingData {
                type_name: "MlsLevelCount",
                type_size: std::mem::size_of::<le::U32>(),
                num_bytes,
            })?;

        // `MlsRange::parse()` cannot be implemented in terms of `MlsLevel::parse()` for the
        // low and optional high level, because of the order in which the sensitivity and
        // category bitmap fields appear.
        let num_bytes = tail.len();
        let (sensitivity_low, tail) =
            PolicyCursor::parse::<le::U32>(tail).ok_or(ParseError::MissingData {
                type_name: "MlsLevelSensitivityLow",
                type_size: std::mem::size_of::<le::U32>(),
                num_bytes,
            })?;

        let (low_categories, high_level, tail) = if count.get() > 1 {
            let num_bytes = tail.len();
            let (sensitivity_high, tail) =
                PolicyCursor::parse::<le::U32>(tail).ok_or(ParseError::MissingData {
                    type_name: "MlsLevelSensitivityHigh",
                    type_size: std::mem::size_of::<le::U32>(),
                    num_bytes,
                })?;
            let (low_categories, tail) = ExtensibleBitmap::parse(tail)
                .map_err(Into::<anyhow::Error>::into)
                .context("parsing mls range low categories")?;
            let (high_categories, tail) = ExtensibleBitmap::parse(tail)
                .map_err(Into::<anyhow::Error>::into)
                .context("parsing mls range high categories")?;

            (
                low_categories,
                Some(MlsLevel { sensitivity: sensitivity_high, categories: high_categories }),
                tail,
            )
        } else {
            let (low_categories, tail) = ExtensibleBitmap::parse(tail)
                .map_err(Into::<anyhow::Error>::into)
                .context("parsing mls range low categories")?;

            (low_categories, None, tail)
        };

        Ok((
            Self {
                count,
                low: MlsLevel { sensitivity: sensitivity_low, categories: low_categories },
                high: high_level,
            },
            tail,
        ))
    }
}

impl Validate for [ConditionalBoolean] {
    type Error = anyhow::Error;

    /// TODO: Validate consistency of sequence of [`ConditionalBoolean`].
    fn validate(&self, _context: &mut PolicyValidationContext) -> Result<(), Self::Error> {
        Ok(())
    }
}

array_type!(ConditionalBoolean, ConditionalBooleanMetadata, Vec<u8>);

array_type_validate_deref_both!(ConditionalBoolean);

impl ValidateArray<ConditionalBooleanMetadata, u8> for ConditionalBoolean {
    type Error = anyhow::Error;

    /// TODO: Validate consistency between [`ConditionalBooleanMetadata`] and `[u8]` key.
    fn validate_array(
        _context: &mut PolicyValidationContext,
        _metadata: &ConditionalBooleanMetadata,
        _items: &[u8],
    ) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(Clone, Debug, KnownLayout, FromBytes, Immutable, PartialEq, Unaligned)]
#[repr(C, packed)]
pub(super) struct ConditionalBooleanMetadata {
    id: le::U32,
    /// Current active value of this conditional boolean.
    active: le::U32,
    length: le::U32,
}

impl ConditionalBooleanMetadata {
    /// Returns the active value for the boolean.
    pub(super) fn active(&self) -> bool {
        self.active != le::U32::ZERO
    }
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
    fn validate(&self, _context: &mut PolicyValidationContext) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl Validate for [Sensitivity] {
    type Error = anyhow::Error;

    /// TODO: Validate consistency of sequence of [`Sensitivity`].
    fn validate(&self, _context: &mut PolicyValidationContext) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(Debug, PartialEq)]
pub(super) struct Sensitivity {
    metadata: SensitivityMetadata,
    level: MlsLevel,
}

impl Sensitivity {
    pub fn id(&self) -> SensitivityId {
        SensitivityId(NonZeroU32::new(self.level.sensitivity.get()).unwrap())
    }

    pub fn name_bytes(&self) -> &[u8] {
        &self.metadata.data
    }
}

impl Parse for Sensitivity {
    type Error = anyhow::Error;

    fn parse(bytes: PolicyCursor) -> Result<(Self, PolicyCursor), Self::Error> {
        let tail = bytes;

        let (metadata, tail) = SensitivityMetadata::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing sensitivity metadata")?;

        let (level, tail) = MlsLevel::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing sensitivity mls level")?;

        Ok((Self { metadata, level }, tail))
    }
}

impl Validate for Sensitivity {
    type Error = anyhow::Error;

    /// TODO: Validate internal consistency of `self.metadata` and `self.level`.
    fn validate(&self, _context: &mut PolicyValidationContext) -> Result<(), Self::Error> {
        NonZeroU32::new(self.level.sensitivity.get()).ok_or(ValidateError::NonOptionalIdIsZero)?;
        Ok(())
    }
}

array_type!(SensitivityMetadata, SensitivityStaticMetadata, Vec<u8>);

array_type_validate_deref_both!(SensitivityMetadata);

impl ValidateArray<SensitivityStaticMetadata, u8> for SensitivityMetadata {
    type Error = anyhow::Error;

    /// TODO: Validate consistency between [`SensitivityMetadata`] and `[u8]` key.
    fn validate_array(
        _context: &mut PolicyValidationContext,
        _metadata: &SensitivityStaticMetadata,
        _items: &[u8],
    ) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(Clone, Debug, KnownLayout, FromBytes, Immutable, PartialEq, Unaligned)]
#[repr(C, packed)]
pub(super) struct SensitivityStaticMetadata {
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
    fn validate(&self, _context: &mut PolicyValidationContext) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl Validate for [Category] {
    type Error = anyhow::Error;

    /// TODO: Validate consistency of sequence of [`Category`].
    fn validate(&self, _context: &mut PolicyValidationContext) -> Result<(), Self::Error> {
        Ok(())
    }
}

array_type!(Category, CategoryMetadata, Vec<u8>);

array_type_validate_deref_both!(Category);

impl Category {
    pub fn id(&self) -> CategoryId {
        CategoryId(NonZeroU32::new(self.metadata.id.get()).unwrap())
    }

    pub fn name_bytes(&self) -> &[u8] {
        &self.data
    }
}

impl ValidateArray<CategoryMetadata, u8> for Category {
    type Error = anyhow::Error;

    /// TODO: Validate consistency between [`CategoryMetadata`] and `[u8]` key.
    fn validate_array(
        _context: &mut PolicyValidationContext,
        _metadata: &CategoryMetadata,
        _items: &[u8],
    ) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(Clone, Debug, KnownLayout, FromBytes, Immutable, PartialEq, Unaligned)]
#[repr(C, packed)]
pub(super) struct CategoryMetadata {
    length: le::U32,
    id: le::U32,
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
    fn validate(&self, _context: &mut PolicyValidationContext) -> Result<(), Self::Error> {
        NonZeroU32::new(self.id.get()).ok_or(ValidateError::NonOptionalIdIsZero)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::security_context::Level;
    use super::super::{parse_policy_by_value, CategoryId, SensitivityId, UserId};
    use super::*;

    use std::num::NonZeroU32;

    #[test]
    fn mls_levels_for_user_context() {
        const TEST_POLICY: &[u8] = include_bytes! {"../../testdata/micro_policies/multiple_levels_and_categories_policy.pp"};
        let policy = parse_policy_by_value(TEST_POLICY.to_vec()).unwrap();
        let policy = policy.validate().unwrap();
        let parsed_policy = policy.0.parsed_policy();

        let user = parsed_policy.user(UserId(NonZeroU32::new(1).expect("user with id 1")));
        let mls_range = user.mls_range();
        let low_level = mls_range.low();
        let high_level = mls_range.high().as_ref().expect("user 1 has a high mls level");

        assert_eq!(low_level.sensitivity(), SensitivityId(NonZeroU32::new(1).unwrap()));
        assert_eq!(
            low_level.category_ids().collect::<Vec<_>>(),
            vec![CategoryId(NonZeroU32::new(1).unwrap())]
        );

        assert_eq!(high_level.sensitivity(), SensitivityId(NonZeroU32::new(2).unwrap()));
        assert_eq!(
            high_level.category_ids().collect::<Vec<_>>(),
            vec![
                CategoryId(NonZeroU32::new(1).unwrap()),
                CategoryId(NonZeroU32::new(2).unwrap()),
                CategoryId(NonZeroU32::new(3).unwrap()),
                CategoryId(NonZeroU32::new(4).unwrap()),
                CategoryId(NonZeroU32::new(5).unwrap()),
            ]
        );
    }

    #[test]
    fn parse_mls_constrain_statement() {
        let policy_bytes = include_bytes!("../../testdata/micro_policies/constraints_policy.pp");
        let policy = parse_policy_by_value(policy_bytes.to_vec()).expect("parse policy");
        let parsed_policy = &policy.0;
        parsed_policy.validate().expect("validate policy");

        let class = find_class_by_name(parsed_policy.classes(), "class_mls_constraints")
            .expect("look up class");
        let constraints = class.constraints();
        assert_eq!(constraints.len(), 6);
        // Expected (`constraint_term_type`, `expr_operator_type`,
        // `expr_operand_type`) values for the single term of the
        // corresponding class constraint.
        //
        // NB. Constraint statements appear in reverse order in binary policy
        // vs. text policy.
        let expected = [
            (
                CONSTRAINT_TERM_TYPE_EXPR,
                CONSTRAINT_EXPR_OPERATOR_TYPE_INCOMP,
                CONSTRAINT_EXPR_OPERAND_TYPE_L1_H1,
            ),
            (
                CONSTRAINT_TERM_TYPE_EXPR,
                CONSTRAINT_EXPR_OPERATOR_TYPE_INCOMP,
                CONSTRAINT_EXPR_OPERAND_TYPE_H1_H2,
            ),
            (
                CONSTRAINT_TERM_TYPE_EXPR,
                CONSTRAINT_EXPR_OPERATOR_TYPE_DOMBY,
                CONSTRAINT_EXPR_OPERAND_TYPE_L1_H2,
            ),
            (
                CONSTRAINT_TERM_TYPE_EXPR,
                CONSTRAINT_EXPR_OPERATOR_TYPE_DOM,
                CONSTRAINT_EXPR_OPERAND_TYPE_H1_L2,
            ),
            (
                CONSTRAINT_TERM_TYPE_EXPR,
                CONSTRAINT_EXPR_OPERATOR_TYPE_NE,
                CONSTRAINT_EXPR_OPERAND_TYPE_L2_H2,
            ),
            (
                CONSTRAINT_TERM_TYPE_EXPR,
                CONSTRAINT_EXPR_OPERATOR_TYPE_EQ,
                CONSTRAINT_EXPR_OPERAND_TYPE_L1_L2,
            ),
        ];
        for (i, constraint) in constraints.iter().enumerate() {
            assert_eq!(constraint.access_vector(), AccessVector(1), "constraint {}", i);
            let terms = constraint.constraint_expr().constraint_terms();
            assert_eq!(terms.len(), 1, "constraint {}", i);
            let term = &terms[0];
            assert_eq!(
                (term.constraint_term_type(), term.expr_operator_type(), term.expr_operand_type()),
                expected[i],
                "constraint {}",
                i
            );
        }
    }

    #[test]
    fn parse_constrain_statement() {
        let policy_bytes = include_bytes!("../../testdata/micro_policies/constraints_policy.pp");
        let policy = parse_policy_by_value(policy_bytes.to_vec()).expect("parse policy");
        let parsed_policy = &policy.0;
        parsed_policy.validate().expect("validate policy");

        let class = find_class_by_name(parsed_policy.classes(), "class_constraint_nested")
            .expect("look up class");
        let constraints = class.constraints();
        assert_eq!(constraints.len(), 1);
        let constraint = &constraints[0];
        assert_eq!(constraint.access_vector(), AccessVector(1));
        let terms = constraint.constraint_expr().constraint_terms();
        assert_eq!(terms.len(), 8);

        // Expected (`constraint_term_type`, `expr_operator_type`,
        // `expr_operand_type`) values for the constraint terms.
        //
        // NB. Constraint statements appear in reverse order in binary policy
        // vs. text policy.
        let expected = [
            (
                CONSTRAINT_TERM_TYPE_EXPR_WITH_NAMES,
                CONSTRAINT_EXPR_OPERATOR_TYPE_EQ,
                CONSTRAINT_EXPR_OPERAND_TYPE_USER
                    | CONSTRAINT_EXPR_WITH_NAMES_OPERAND_TYPE_TARGET_MASK,
            ),
            (
                CONSTRAINT_TERM_TYPE_EXPR,
                CONSTRAINT_EXPR_OPERATOR_TYPE_EQ,
                CONSTRAINT_EXPR_OPERAND_TYPE_ROLE,
            ),
            (CONSTRAINT_TERM_TYPE_AND_OPERATOR, 0, 0),
            (
                CONSTRAINT_TERM_TYPE_EXPR,
                CONSTRAINT_EXPR_OPERATOR_TYPE_EQ,
                CONSTRAINT_EXPR_OPERAND_TYPE_USER,
            ),
            (
                CONSTRAINT_TERM_TYPE_EXPR,
                CONSTRAINT_EXPR_OPERATOR_TYPE_EQ,
                CONSTRAINT_EXPR_OPERAND_TYPE_TYPE,
            ),
            (CONSTRAINT_TERM_TYPE_NOT_OPERATOR, 0, 0),
            (CONSTRAINT_TERM_TYPE_AND_OPERATOR, 0, 0),
            (CONSTRAINT_TERM_TYPE_OR_OPERATOR, 0, 0),
        ];
        for (i, term) in terms.iter().enumerate() {
            assert_eq!(
                (term.constraint_term_type(), term.expr_operator_type(), term.expr_operand_type()),
                expected[i],
                "term {}",
                i
            );
        }
    }
}
