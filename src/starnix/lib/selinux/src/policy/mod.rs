// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

pub mod arrays;
pub mod error;
pub mod index;
pub mod metadata;
pub mod parsed_policy;
pub mod parser;

mod extensible_bitmap;
mod security_context;
mod symbols;

pub use arrays::FsUseType;
pub use index::FsUseLabelAndType;
pub use security_context::{SecurityContext, SecurityContextError};

use crate::{self as sc, FileClass, NullessByteStr, ObjectClass};
use anyhow::Context as _;
use error::ParseError;
use index::PolicyIndex;
use metadata::HandleUnknown;
use parsed_policy::ParsedPolicy;
use parser::{ByRef, ByValue, ParseStrategy};
use std::fmt::Debug;
use std::marker::PhantomData;
use std::num::NonZeroU32;
use std::ops::Deref;
use symbols::{find_class_by_name, find_common_symbol_by_name_bytes};
use zerocopy::{
    little_endian as le, FromBytes, Immutable, KnownLayout, Ref, SplitByteSlice, Unaligned,
};

/// Maximum SELinux policy version supported by this implementation.
pub const SUPPORTED_POLICY_VERSION: u32 = 33;

/// Identifies a user within a policy.
#[derive(Copy, Clone, Debug, Hash, Eq, PartialEq)]
pub struct UserId(NonZeroU32);

/// Identifies a role within a policy.
#[derive(Copy, Clone, Debug, Hash, Eq, PartialEq)]
pub struct RoleId(NonZeroU32);

/// Identifies a type within a policy.
#[derive(Copy, Clone, Debug, Hash, Eq, PartialEq)]
pub struct TypeId(NonZeroU32);

/// Identifies a sensitivity level within a policy.
#[derive(Copy, Clone, Debug, Hash, Eq, Ord, PartialEq, PartialOrd)]
pub struct SensitivityId(NonZeroU32);

/// Identifies a security category within a policy.
#[derive(Copy, Clone, Debug, Hash, Eq, Ord, PartialEq, PartialOrd)]
pub struct CategoryId(NonZeroU32);

/// Identifies a class within a policy.
#[derive(Copy, Clone, Debug, Hash, Eq, PartialEq)]
pub struct ClassId(NonZeroU32);

impl Into<u32> for ClassId {
    fn into(self) -> u32 {
        self.0.into()
    }
}

/// Encapsulates the result of a permissions calculation, between source & target domains, for a
/// specific class. Decisions describe which permissions are allowed, and whether permissions should
/// be audit-logged when allowed, and when denied.
#[derive(Debug, Clone, PartialEq)]
pub struct AccessDecision {
    pub allow: AccessVector,
    pub auditallow: AccessVector,
    pub auditdeny: AccessVector,
    pub flags: u32,
}

impl Default for AccessDecision {
    fn default() -> Self {
        Self::allow(AccessVector::NONE)
    }
}

impl AccessDecision {
    /// Returns an [`AccessDecision`] with the specified permissions to `allow`, and default audit
    /// behaviour.
    pub(super) const fn allow(allow: AccessVector) -> Self {
        Self { allow, auditallow: AccessVector::NONE, auditdeny: AccessVector::ALL, flags: 0 }
    }
}

/// [`AccessDecision::flags`] value indicating that the policy marks the source domain permissive.
pub(super) const SELINUX_AVD_FLAGS_PERMISSIVE: u32 = 1;

/// The set of permissions that may be granted to sources accessing targets of a particular class,
/// as defined in an SELinux policy.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AccessVector(u32);

impl AccessVector {
    pub const NONE: AccessVector = AccessVector(0);
    pub const ALL: AccessVector = AccessVector(std::u32::MAX);

    pub(super) fn from_raw(access_vector: u32) -> Self {
        Self(access_vector)
    }
}

impl std::ops::BitAnd for AccessVector {
    type Output = Self;

    fn bitand(self, rhs: Self) -> Self::Output {
        AccessVector(self.0 & rhs.0)
    }
}

impl std::ops::BitOr for AccessVector {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self::Output {
        AccessVector(self.0 | rhs.0)
    }
}

impl std::ops::BitAndAssign for AccessVector {
    fn bitand_assign(&mut self, rhs: Self) {
        self.0 &= rhs.0
    }
}

impl std::ops::BitOrAssign for AccessVector {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0
    }
}

/// Parses `binary_policy` by value; that is, copies underlying binary data out in addition to
/// building up parser output structures. This function returns
/// `(unvalidated_parser_output, binary_policy)` on success, or an error if parsing failed. Note
/// that the second component of the success case contains precisely the same bytes as the input.
/// This function depends on a uniformity of interface between the "by value" and "by reference"
/// strategies, but also requires an `unvalidated_parser_output` type that is independent of the
/// `binary_policy` lifetime. Taken together, these requirements demand the "move-in + move-out"
/// interface for `binary_policy`.
///
/// If the caller does not need access to the binary policy when parsing fails, but does need to
/// retain both the parsed output and the binary policy when parsing succeeds, the code will look
/// something like:
///
/// ```rust,ignore
/// let (unvalidated_policy, binary_policy) = parse_policy_by_value(binary_policy)?;
/// ```
///
/// If the caller does need access to the binary policy when parsing fails and needs to retain both
/// parsed output and the binary policy when parsing succeeds, the code will look something like:
///
/// ```rust,ignore
/// let (unvalidated_policy, _) = parse_policy_by_value(binary_policy.clone())?;
/// ```
///
/// If the caller does not need to retain both the parsed output and the binary policy, then
/// [`parse_policy_by_reference`] should be used instead.
pub fn parse_policy_by_value(
    binary_policy: Vec<u8>,
) -> Result<(Unvalidated<ByValue<Vec<u8>>>, Vec<u8>), anyhow::Error> {
    let (parsed_policy, binary_policy) =
        ParsedPolicy::parse(ByValue::new(binary_policy)).context("parsing policy")?;
    Ok((Unvalidated(parsed_policy), binary_policy))
}

/// Parses `binary_policy` by reference; that is, constructs parser output structures that contain
/// _references_ to data in `binary_policy`. This function returns `unvalidated_parser_output` on
/// success, or an error if parsing failed.
///
/// If the caller does needs to retain both the parsed output and the binary policy, then
/// [`parse_policy_by_value`] should be used instead.
pub fn parse_policy_by_reference<'a>(
    binary_policy: &'a [u8],
) -> Result<Unvalidated<ByRef<&'a [u8]>>, anyhow::Error> {
    let (parsed_policy, _) =
        ParsedPolicy::parse(ByRef::new(binary_policy)).context("parsing policy")?;
    Ok(Unvalidated(parsed_policy))
}

/// Information on a Class. This struct is used for sharing Class information outside this crate.
pub struct ClassInfo<'a> {
    /// The name of the class.
    pub class_name: &'a [u8],
    /// The class identifier.
    pub class_id: ClassId,
}

#[derive(Debug)]
pub struct Policy<PS: ParseStrategy>(PolicyIndex<PS>);

impl<PS: ParseStrategy> Policy<PS> {
    /// The policy version stored in the underlying binary policy.
    pub fn policy_version(&self) -> u32 {
        self.0.parsed_policy().policy_version()
    }

    /// The way "unknown" policy decisions should be handed according to the underlying binary
    /// policy.
    pub fn handle_unknown(&self) -> HandleUnknown {
        self.0.parsed_policy().handle_unknown()
    }

    pub fn conditional_booleans<'a>(&'a self) -> Vec<(&'a [u8], bool)> {
        self.0
            .parsed_policy()
            .conditional_booleans()
            .iter()
            .map(|boolean| (PS::deref_slice(&boolean.data), PS::deref(&boolean.metadata).active()))
            .collect()
    }

    /// The set of class names and their respective class identifiers.
    pub fn classes<'a>(&'a self) -> Vec<ClassInfo<'a>> {
        self.0
            .parsed_policy()
            .classes()
            .iter()
            .map(|class| ClassInfo { class_name: class.name_bytes(), class_id: class.id() })
            .collect()
    }

    /// Returns the set of permissions for the given class, including both the explicitly owned permissions
    /// and the inherited ones from common symbols. Each permission is a tuple of the permission identifier
    /// and it's name.
    pub fn find_class_permissions_by_name(
        &self,
        class_name: &str,
    ) -> Result<Vec<(u32, Vec<u8>)>, ()> {
        let class = find_class_by_name(self.0.parsed_policy().classes(), class_name).ok_or(())?;
        let owned_permissions = class.permissions();

        let mut result: Vec<_> = owned_permissions
            .iter()
            .map(|permission| (permission.id(), permission.name_bytes().to_vec()))
            .collect();

        // common_name_bytes() is empty when the class doesn't inherit from a CommonSymbol.
        if class.common_name_bytes().is_empty() {
            return Ok(result);
        }

        let common_symbol_permissions = find_common_symbol_by_name_bytes(
            self.0.parsed_policy().common_symbols(),
            class.common_name_bytes(),
        )
        .ok_or(())?
        .permissions();

        result.append(
            &mut common_symbol_permissions
                .iter()
                .map(|permission| (permission.id(), permission.name_bytes().to_vec()))
                .collect(),
        );

        Ok(result)
    }

    /// If there is an fs_use statement for the given filesystem type, returns the associated
    /// [`SecurityContext`] and [`FsUseType`].
    pub fn fs_use_label_and_type(&self, fs_type: NullessByteStr<'_>) -> Option<FsUseLabelAndType> {
        self.0.fs_use_label_and_type(fs_type)
    }

    /// If there is a genfscon statement for the given filesystem type, returns the associated
    /// [`SecurityContext`].
    pub fn genfscon_label_for_fs_and_path(
        &self,
        fs_type: NullessByteStr<'_>,
        node_path: NullessByteStr<'_>,
        class_id: Option<ClassId>,
    ) -> Option<SecurityContext> {
        self.0.genfscon_label_for_fs_and_path(fs_type, node_path, class_id)
    }

    /// Returns the [`SecurityContext`] defined by this policy for the specified
    /// well-known (or "initial") Id.
    pub fn initial_context(&self, id: sc::InitialSid) -> security_context::SecurityContext {
        self.0.initial_context(id)
    }

    /// Returns a [`SecurityContext`] with fields parsed from the supplied Security Context string.
    pub fn parse_security_context(
        &self,
        security_context: NullessByteStr<'_>,
    ) -> Result<security_context::SecurityContext, security_context::SecurityContextError> {
        security_context::SecurityContext::parse(&self.0, security_context)
    }

    /// Validates a [`SecurityContext`] against this policy's constraints.
    pub fn validate_security_context(
        &self,
        security_context: &SecurityContext,
    ) -> Result<(), SecurityContextError> {
        security_context.validate(&self.0)
    }

    /// Returns a byte string describing the supplied [`SecurityContext`].
    pub fn serialize_security_context(&self, security_context: &SecurityContext) -> Vec<u8> {
        security_context.serialize(&self.0)
    }

    /// Returns the security context that should be applied to a newly created file-like SELinux
    /// object according to `source` and `target` security contexts, as well as the new object's
    /// `class`. Returns an error if the security context for such an object is not well-defined
    /// by this [`Policy`].
    pub fn new_file_security_context(
        &self,
        source: &SecurityContext,
        target: &SecurityContext,
        class: &FileClass,
    ) -> SecurityContext {
        self.0.new_file_security_context(source, target, class)
    }

    /// Returns the security context that should be applied to a newly created SELinux
    /// object according to `source` and `target` security contexts, as well as the new object's
    /// `class`.
    /// Defaults to the `source` security context if the policy does not specify transitions or
    /// defaults for the `source`, `target` or `class` components.
    ///
    /// Returns an error if the security context for such an object is not well-defined
    /// by this [`Policy`].
    pub fn new_security_context(
        &self,
        source: &SecurityContext,
        target: &SecurityContext,
        class: &ObjectClass,
    ) -> SecurityContext {
        self.0.new_security_context(
            source,
            target,
            class,
            source.role(),
            source.type_(),
            source.low_level(),
            source.high_level(),
        )
    }

    /// Computes the access vector that associates type `source_type_name` and `target_type_name`
    /// via an explicit `allow [...];` statement in the binary policy. Computes `AccessVector::NONE`
    /// if no such statement exists.
    pub fn compute_explicitly_allowed(
        &self,
        source_type: TypeId,
        target_type: TypeId,
        object_class: sc::ObjectClass,
    ) -> AccessDecision {
        if let Some(target_class) = self.0.class(&object_class) {
            self.0.parsed_policy().compute_explicitly_allowed(
                source_type,
                target_type,
                target_class,
            )
        } else {
            AccessDecision::allow(AccessVector::NONE)
        }
    }

    /// Computes the access vector that associates type `source_type_name` and `target_type_name`
    /// via an explicit `allow [...];` statement in the binary policy. Computes `AccessVector::NONE`
    /// if no such statement exists. This is the "custom" form of this API because
    /// `target_class_name` is associated with a [`crate::AbstractObjectClass::Custom`]
    /// value.
    pub fn compute_explicitly_allowed_custom(
        &self,
        source_type: TypeId,
        target_type: TypeId,
        target_class_name: &str,
    ) -> AccessDecision {
        self.0.parsed_policy().compute_explicitly_allowed_custom(
            source_type,
            target_type,
            target_class_name,
        )
    }

    pub fn is_bounded_by(&self, bounded_type: TypeId, parent_type: TypeId) -> bool {
        let type_ = self.0.parsed_policy().type_(bounded_type);
        type_.bounded_by() == Some(parent_type)
    }

    /// Returns true if the policy has the marked the type/domain for permissive checks.
    pub fn is_permissive(&self, type_: TypeId) -> bool {
        self.0.parsed_policy().permissive_types().is_set(type_.0.get())
    }
}

impl<PS: ParseStrategy> AccessVectorComputer for Policy<PS> {
    fn access_vector_from_permissions<
        P: sc::ClassPermission + Into<sc::Permission> + Clone + 'static,
    >(
        &self,
        permissions: &[P],
    ) -> Option<AccessVector> {
        let mut access_vector = AccessVector::NONE;
        for permission in permissions {
            if let Some(permission_info) = self.0.permission(&permission.clone().into()) {
                // Compute bit flag associated with permission.
                // Use `permission.id() - 1` below because ids start at `1` to refer to the
                // "shift `1` by 0 bits".
                //
                // id=1 => bits:0...001, id=2 => bits:0...010, etc.
                access_vector |= AccessVector(1 << (permission_info.id() - 1));
            } else {
                // The permission is unknown so defer to the policy-define unknown handling behaviour.
                if self.0.parsed_policy().handle_unknown() != HandleUnknown::Allow {
                    return None;
                }
            }
        }
        Some(access_vector)
    }
}

impl<PS: ParseStrategy> Validate for Policy<PS> {
    type Error = anyhow::Error;

    fn validate(&self) -> Result<(), Self::Error> {
        self.0.parsed_policy().validate()
    }
}

/// A [`Policy`] that has been successfully parsed, but not validated.
pub struct Unvalidated<PS: ParseStrategy>(ParsedPolicy<PS>);

impl<PS: ParseStrategy> Unvalidated<PS> {
    pub fn validate(self) -> Result<Policy<PS>, anyhow::Error> {
        Validate::validate(&self.0).context("validating parsed policy")?;
        let index = PolicyIndex::new(self.0).context("building index")?;
        Ok(Policy(index))
    }
}

/// An owner of policy information that can translate [`sc::Permission`] values into
/// [`AccessVector`] values that are consistent with the owned policy.
pub trait AccessVectorComputer {
    /// Returns an [`AccessVector`] containing the supplied kernel `permissions`.
    ///
    /// The loaded policy's "handle unknown" configuration determines how `permissions`
    /// entries not explicitly defined by the policy are handled. Allow-unknown will
    /// result in unknown `permissions` being ignored, while deny-unknown will cause
    /// `None` to be returned if one or more `permissions` are unknown.
    fn access_vector_from_permissions<
        P: sc::ClassPermission + Into<sc::Permission> + Clone + 'static,
    >(
        &self,
        permissions: &[P],
    ) -> Option<AccessVector>;
}

/// A data structure that can be parsed as a part of a binary policy.
pub trait Parse<PS: ParseStrategy>: Sized {
    /// The type of error that may be returned from `parse()`, usually [`ParseError`] or
    /// [`anyhow::Error`].
    type Error: Into<anyhow::Error>;

    /// Parses a `Self` from `bytes`, returning the `Self` and trailing bytes, or an error if
    /// bytes corresponding to a `Self` are malformed.
    fn parse(bytes: PS) -> Result<(Self, PS), Self::Error>;
}

/// Parse a data as a slice of inner data structures from a prefix of a [`ByteSlice`].
pub(super) trait ParseSlice<PS: ParseStrategy>: Sized {
    /// The type of error that may be returned from `parse()`, usually [`ParseError`] or
    /// [`anyhow::Error`].
    type Error: Into<anyhow::Error>;

    /// Parses a `Self` as `count` of internal itemsfrom `bytes`, returning the `Self` and trailing
    /// bytes, or an error if bytes corresponding to a `Self` are malformed.
    fn parse_slice(bytes: PS, count: usize) -> Result<(Self, PS), Self::Error>;
}

/// Validate a parsed data structure.
pub(super) trait Validate {
    /// The type of error that may be returned from `validate()`, usually [`ParseError`] or
    /// [`anyhow::Error`].
    type Error: Into<anyhow::Error>;

    /// Validates a `Self`, returning a `Self::Error` if `self` is internally inconsistent.
    fn validate(&self) -> Result<(), Self::Error>;
}

pub(super) trait ValidateArray<M, D> {
    /// The type of error that may be returned from `validate()`, usually [`ParseError`] or
    /// [`anyhow::Error`].
    type Error: Into<anyhow::Error>;

    /// Validates a `Self`, returning a `Self::Error` if `self` is internally inconsistent.
    fn validate_array<'a>(metadata: &'a M, data: &'a [D]) -> Result<(), Self::Error>;
}

/// Treat a type as metadata that contains a count of subsequent data.
pub(super) trait Counted {
    /// Returns the count of subsequent data items.
    fn count(&self) -> u32;
}

impl<T: Validate> Validate for Option<T> {
    type Error = <T as Validate>::Error;

    fn validate(&self) -> Result<(), Self::Error> {
        match self {
            Some(value) => value.validate(),
            None => Ok(()),
        }
    }
}

impl Validate for le::U32 {
    type Error = anyhow::Error;

    /// Using a raw `le::U32` implies no additional constraints on its value. To operate with
    /// constraints, define a `struct T(le::U32);` and `impl Validate for T { ... }`.
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl Validate for u8 {
    type Error = anyhow::Error;

    /// Using a raw `u8` implies no additional constraints on its value. To operate with
    /// constraints, define a `struct T(u8);` and `impl Validate for T { ... }`.
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl Validate for [u8] {
    type Error = anyhow::Error;

    /// Using a raw `[u8]` implies no additional constraints on its value. To operate with
    /// constraints, define a `struct T([u8]);` and `impl Validate for T { ... }`.
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl<B: SplitByteSlice, T: Validate + FromBytes + KnownLayout + Immutable> Validate for Ref<B, T> {
    type Error = <T as Validate>::Error;

    fn validate(&self) -> Result<(), Self::Error> {
        self.deref().validate()
    }
}

impl<B: SplitByteSlice, T: Counted + FromBytes + KnownLayout + Immutable> Counted for Ref<B, T> {
    fn count(&self) -> u32 {
        self.deref().count()
    }
}

/// A length-encoded array that contains metadata in `M` and a slice of data items internally
/// managed by `D`.
#[derive(Clone, Debug, PartialEq)]
struct Array<PS, M, D> {
    metadata: M,
    data: D,
    _marker: PhantomData<PS>,
}

impl<PS: ParseStrategy, M: Counted + Parse<PS>, D: ParseSlice<PS>> Parse<PS> for Array<PS, M, D> {
    /// [`Array`] abstracts over two types (`M` and `D`) that may have different [`Parse::Error`]
    /// types. Unify error return type via [`anyhow::Error`].
    type Error = anyhow::Error;

    /// Parses [`Array`] by parsing *and validating* `metadata`, `data`, and `self`.
    fn parse(bytes: PS) -> Result<(Self, PS), Self::Error> {
        let tail = bytes;

        let (metadata, tail) = M::parse(tail).map_err(Into::<anyhow::Error>::into)?;

        let (data, tail) =
            D::parse_slice(tail, metadata.count() as usize).map_err(Into::<anyhow::Error>::into)?;

        let array = Self { metadata, data, _marker: PhantomData };

        Ok((array, tail))
    }
}

impl<
        T: Clone + Debug + FromBytes + KnownLayout + Immutable + PartialEq + Unaligned,
        PS: ParseStrategy<Output<T> = T>,
    > Parse<PS> for T
{
    type Error = anyhow::Error;

    fn parse(bytes: PS) -> Result<(Self, PS), Self::Error> {
        let num_bytes = bytes.len();
        let (data, tail) = PS::parse::<T>(bytes).ok_or(ParseError::MissingData {
            type_name: std::any::type_name::<T>(),
            type_size: std::mem::size_of::<T>(),
            num_bytes,
        })?;

        Ok((data, tail))
    }
}

/// Defines a at type that wraps an [`Array`], implementing `Deref`-as-`Array` and [`Parse`]. This
/// macro should be used in contexts where using a general [`Array`] implementation may introduce
/// conflicting implementations on account of general [`Array`] type parameters.
macro_rules! array_type {
    ($type_name:ident, $parse_strategy:ident, $metadata_type:ty, $data_type:ty, $metadata_type_name:expr, $data_type_name:expr) => {
        #[doc = "An [`Array`] with [`"]
        #[doc = $metadata_type_name]
        #[doc = "`] metadata and [`"]
        #[doc = $data_type_name]
        #[doc = "`] data items."]
        #[derive(Debug, PartialEq)]
        pub(super) struct $type_name<$parse_strategy: crate::policy::parser::ParseStrategy>(
            crate::policy::Array<PS, $metadata_type, $data_type>,
        );

        impl<PS: crate::policy::parser::ParseStrategy> std::ops::Deref for $type_name<PS> {
            type Target = crate::policy::Array<PS, $metadata_type, $data_type>;

            fn deref(&self) -> &Self::Target {
                &self.0
            }
        }

        impl<PS: crate::policy::parser::ParseStrategy> crate::policy::Parse<PS> for $type_name<PS>
        where
            crate::policy::Array<PS, $metadata_type, $data_type>: crate::policy::Parse<PS>,
        {
            type Error = <Array<PS, $metadata_type, $data_type> as crate::policy::Parse<PS>>::Error;

            fn parse(bytes: PS) -> Result<(Self, PS), Self::Error> {
                let (array, tail) = Array::<PS, $metadata_type, $data_type>::parse(bytes)?;
                Ok((Self(array), tail))
            }
        }
    };

    ($type_name:ident, $parse_strategy:ident, $metadata_type:ty, $data_type:ty) => {
        array_type!(
            $type_name,
            $parse_strategy,
            $metadata_type,
            $data_type,
            stringify!($metadata_type),
            stringify!($data_type)
        );
    };
}

pub(super) use array_type;

macro_rules! array_type_validate_deref_both {
    ($type_name:ident) => {
        impl<PS: crate::policy::parser::ParseStrategy> Validate for $type_name<PS> {
            type Error = anyhow::Error;

            fn validate(&self) -> Result<(), Self::Error> {
                let metadata = PS::deref(&self.metadata);
                metadata.validate()?;

                let data = PS::deref_slice(&self.data);
                data.validate()?;

                Self::validate_array(metadata, data).map_err(Into::<anyhow::Error>::into)
            }
        }
    };
}

pub(super) use array_type_validate_deref_both;

macro_rules! array_type_validate_deref_data {
    ($type_name:ident) => {
        impl<PS: crate::policy::parser::ParseStrategy> Validate for $type_name<PS> {
            type Error = anyhow::Error;

            fn validate(&self) -> Result<(), Self::Error> {
                let metadata = &self.metadata;
                metadata.validate()?;

                let data = PS::deref_slice(&self.data);
                data.validate()?;

                Self::validate_array(metadata, data)
            }
        }
    };
}

pub(super) use array_type_validate_deref_data;

macro_rules! array_type_validate_deref_metadata_data_vec {
    ($type_name:ident) => {
        impl<PS: crate::policy::parser::ParseStrategy> Validate for $type_name<PS> {
            type Error = anyhow::Error;

            fn validate(&self) -> Result<(), Self::Error> {
                let metadata = PS::deref(&self.metadata);
                metadata.validate()?;

                let data = &self.data;
                data.validate()?;

                Self::validate_array(metadata, data.as_slice())
            }
        }
    };
}

pub(super) use array_type_validate_deref_metadata_data_vec;

macro_rules! array_type_validate_deref_none_data_vec {
    ($type_name:ident) => {
        impl<PS: crate::policy::parser::ParseStrategy> Validate for $type_name<PS> {
            type Error = anyhow::Error;

            fn validate(&self) -> Result<(), Self::Error> {
                let metadata = &self.metadata;
                metadata.validate()?;

                let data = &self.data;
                data.validate()?;

                Self::validate_array(metadata, data.as_slice())
            }
        }
    };
}

pub(super) use array_type_validate_deref_none_data_vec;

impl<
        B: Debug + SplitByteSlice + PartialEq,
        T: Clone + Debug + FromBytes + KnownLayout + Immutable + PartialEq + Unaligned,
    > Parse<ByRef<B>> for Ref<B, T>
{
    type Error = anyhow::Error;

    fn parse(bytes: ByRef<B>) -> Result<(Self, ByRef<B>), Self::Error> {
        let num_bytes = bytes.len();
        let (data, tail) = ByRef::<B>::parse::<T>(bytes).ok_or(ParseError::MissingData {
            type_name: std::any::type_name::<T>(),
            type_size: std::mem::size_of::<T>(),
            num_bytes,
        })?;

        Ok((data, tail))
    }
}

impl<
        B: Debug + SplitByteSlice + PartialEq,
        T: Clone + Debug + FromBytes + Immutable + PartialEq + Unaligned,
    > ParseSlice<ByRef<B>> for Ref<B, [T]>
{
    /// [`Ref`] may return a [`ParseError`] internally, or `<T as Parse>::Error`. Unify error return
    /// type via [`anyhow::Error`].
    type Error = anyhow::Error;

    /// Parses [`Ref`] by consuming it as an unaligned prefix as a slice, then validating the slice
    /// via `Ref::deref`.
    fn parse_slice(bytes: ByRef<B>, count: usize) -> Result<(Self, ByRef<B>), Self::Error> {
        let num_bytes = bytes.len();
        let (data, tail) =
            ByRef::<B>::parse_slice::<T>(bytes, count).ok_or(ParseError::MissingSliceData {
                type_name: std::any::type_name::<T>(),
                type_size: std::mem::size_of::<T>(),
                num_items: count,
                num_bytes,
            })?;

        Ok((data, tail))
    }
}

impl<PS: ParseStrategy, T: Parse<PS>> ParseSlice<PS> for Vec<T> {
    /// `Vec<T>` may return a [`ParseError`] internally, or `<T as Parse>::Error`. Unify error
    /// return type via [`anyhow::Error`].
    type Error = anyhow::Error;

    /// Parses `Vec<T>` by parsing individual `T` instances, then validating them.
    fn parse_slice(bytes: PS, count: usize) -> Result<(Self, PS), Self::Error> {
        let mut slice = Vec::with_capacity(count);
        let mut tail = bytes;

        for _ in 0..count {
            let (item, next_tail) = T::parse(tail).map_err(Into::<anyhow::Error>::into)?;
            slice.push(item);
            tail = next_tail;
        }

        Ok((slice, tail))
    }
}

#[cfg(test)]
pub(super) mod testing {
    use crate::policy::error::ValidateError;
    use crate::policy::{AccessVector, ParseError};

    pub const ACCESS_VECTOR_0001: AccessVector = AccessVector(0b0001u32);
    pub const ACCESS_VECTOR_0010: AccessVector = AccessVector(0b0010u32);

    /// Downcasts an [`anyhow::Error`] to a [`ParseError`] for structured error comparison in tests.
    pub(super) fn as_parse_error(error: anyhow::Error) -> ParseError {
        error.downcast::<ParseError>().expect("parse error")
    }

    /// Downcasts an [`anyhow::Error`] to a [`ParseError`] for structured error comparison in tests.
    pub(super) fn as_validate_error(error: anyhow::Error) -> ValidateError {
        error.downcast::<ValidateError>().expect("validate error")
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;

    use crate::policy::error::QueryError;
    use crate::policy::metadata::HandleUnknown;
    use crate::policy::{parse_policy_by_reference, parse_policy_by_value, SecurityContext};
    use crate::{
        ClassPermission as _, FileClass, InitialSid, ObjectClass, Permission, ProcessPermission,
    };

    use serde::Deserialize;

    /// Returns whether the input types are explicitly granted `permission` via an `allow [...];`
    /// policy statement.
    ///
    /// # Panics
    /// If supplied with type Ids not previously obtained from the `Policy` itself; validation
    /// ensures that all such Ids have corresponding definitions.
    fn is_explicitly_allowed<PS: ParseStrategy>(
        policy: &Policy<PS>,
        source_type: TypeId,
        target_type: TypeId,
        permission: sc::Permission,
    ) -> Result<bool, QueryError> {
        let object_class = permission.class();
        if let (Some(target_class), Some(permission)) =
            (policy.0.class(&object_class), policy.0.permission(&permission))
        {
            policy.0.parsed_policy().class_permission_is_explicitly_allowed(
                source_type,
                target_type,
                target_class,
                permission,
            )
        } else {
            Ok(false)
        }
    }

    fn type_id_by_name<PS: ParseStrategy>(parsed_policy: &ParsedPolicy<PS>, name: &str) -> TypeId {
        parsed_policy.type_by_name(name).unwrap().id()
    }

    #[derive(Debug, Deserialize)]
    struct Expectations {
        expected_policy_version: u32,
        expected_handle_unknown: LocalHandleUnknown,
    }

    #[derive(Debug, Deserialize, PartialEq)]
    #[serde(rename_all = "snake_case")]
    enum LocalHandleUnknown {
        Deny,
        Reject,
        Allow,
    }

    impl PartialEq<HandleUnknown> for LocalHandleUnknown {
        fn eq(&self, other: &HandleUnknown) -> bool {
            match self {
                LocalHandleUnknown::Deny => *other == HandleUnknown::Deny,
                LocalHandleUnknown::Reject => *other == HandleUnknown::Reject,
                LocalHandleUnknown::Allow => *other == HandleUnknown::Allow,
            }
        }
    }

    #[test]
    fn known_policies() {
        let policies_and_expectations = [
            [
                b"testdata/policies/emulator".to_vec(),
                include_bytes!("../../testdata/policies/emulator").to_vec(),
                include_bytes!("../../testdata/expectations/emulator").to_vec(),
            ],
            [
                b"testdata/policies/selinux_testsuite".to_vec(),
                include_bytes!("../../testdata/policies/selinux_testsuite").to_vec(),
                include_bytes!("../../testdata/expectations/selinux_testsuite").to_vec(),
            ],
        ];

        for [policy_path, policy_bytes, expectations_bytes] in policies_and_expectations {
            let expectations = serde_json5::from_reader::<Expectations, _>(
                &mut std::io::Cursor::new(expectations_bytes),
            )
            .expect("deserialize expectations");

            // Test parse-by-value.

            let (policy, returned_policy_bytes) =
                parse_policy_by_value(policy_bytes.clone()).expect("parse policy");

            let policy = policy
                .validate()
                .with_context(|| {
                    format!(
                        "policy path: {:?}",
                        std::str::from_utf8(policy_path.as_slice()).unwrap()
                    )
                })
                .expect("validate policy");

            assert_eq!(expectations.expected_policy_version, policy.policy_version());
            assert_eq!(expectations.expected_handle_unknown, policy.handle_unknown());

            // Returned policy bytes must be identical to input policy bytes.
            assert_eq!(policy_bytes, returned_policy_bytes);

            // Test parse-by-reference.

            let policy = parse_policy_by_reference(policy_bytes.as_slice()).expect("parse policy");
            let policy = policy.validate().expect("validate policy");

            assert_eq!(expectations.expected_policy_version, policy.policy_version());
            assert_eq!(expectations.expected_handle_unknown, policy.handle_unknown());
        }
    }

    #[test]
    fn policy_lookup() {
        let policy_bytes = include_bytes!("../../testdata/policies/selinux_testsuite");
        let (policy, _) = parse_policy_by_value(policy_bytes.to_vec()).expect("parse policy");
        let policy = policy.validate().expect("validate selinux testsuite policy");

        let unconfined_t = type_id_by_name(policy.0.parsed_policy(), "unconfined_t");

        is_explicitly_allowed(
            &policy,
            unconfined_t,
            unconfined_t,
            Permission::Process(ProcessPermission::Fork),
        )
        .expect("check for `allow unconfined_t unconfined_t:process fork;` in policy");
    }

    #[test]
    fn initial_contexts() {
        let policy_bytes = include_bytes!(
            "../../testdata/micro_policies/multiple_levels_and_categories_policy.pp"
        );
        let (policy, _) = parse_policy_by_value(policy_bytes.to_vec()).expect("parse policy");
        let policy = policy.validate().expect("validate policy");

        let kernel_context = policy.initial_context(InitialSid::Kernel);
        assert_eq!(
            policy.serialize_security_context(&kernel_context),
            b"user0:object_r:type0:s0:c0-s1:c0.c2,c4"
        )
    }

    #[test]
    fn explicit_allow_type_type() {
        let policy_bytes =
            include_bytes!("../../testdata/micro_policies/allow_a_t_b_t_class0_perm0_policy.pp");
        let policy = parse_policy_by_reference(policy_bytes.as_slice()).expect("parse policy");
        let parsed_policy = &policy.0;
        Validate::validate(parsed_policy).expect("validate policy");

        let a_t = type_id_by_name(parsed_policy, "a_t");
        let b_t = type_id_by_name(parsed_policy, "b_t");

        assert!(parsed_policy
            .is_explicitly_allowed_custom(a_t, b_t, "class0", "perm0")
            .expect("query well-formed"));
    }

    #[test]
    fn no_explicit_allow_type_type() {
        let policy_bytes =
            include_bytes!("../../testdata/micro_policies/no_allow_a_t_b_t_class0_perm0_policy.pp");
        let policy = parse_policy_by_reference(policy_bytes.as_slice()).expect("parse policy");
        let parsed_policy = &policy.0;
        Validate::validate(parsed_policy).expect("validate policy");

        let a_t = type_id_by_name(parsed_policy, "a_t");
        let b_t = type_id_by_name(parsed_policy, "b_t");

        assert!(!parsed_policy
            .is_explicitly_allowed_custom(a_t, b_t, "class0", "perm0")
            .expect("query well-formed"));
    }

    #[test]
    fn explicit_allow_type_attr() {
        let policy_bytes =
            include_bytes!("../../testdata/micro_policies/allow_a_t_b_attr_class0_perm0_policy.pp");
        let policy = parse_policy_by_reference(policy_bytes.as_slice()).expect("parse policy");
        let parsed_policy = &policy.0;
        Validate::validate(parsed_policy).expect("validate policy");

        let a_t = type_id_by_name(parsed_policy, "a_t");
        let b_t = type_id_by_name(parsed_policy, "b_t");

        assert!(parsed_policy
            .is_explicitly_allowed_custom(a_t, b_t, "class0", "perm0")
            .expect("query well-formed"));
    }

    #[test]
    fn no_explicit_allow_type_attr() {
        let policy_bytes = include_bytes!(
            "../../testdata/micro_policies/no_allow_a_t_b_attr_class0_perm0_policy.pp"
        );
        let policy = parse_policy_by_reference(policy_bytes.as_slice()).expect("parse policy");
        let parsed_policy = &policy.0;
        Validate::validate(parsed_policy).expect("validate policy");

        let a_t = type_id_by_name(parsed_policy, "a_t");
        let b_t = type_id_by_name(parsed_policy, "b_t");

        assert!(!parsed_policy
            .is_explicitly_allowed_custom(a_t, b_t, "class0", "perm0")
            .expect("query well-formed"));
    }

    #[test]
    fn explicit_allow_attr_attr() {
        let policy_bytes = include_bytes!(
            "../../testdata/micro_policies/allow_a_attr_b_attr_class0_perm0_policy.pp"
        );
        let policy = parse_policy_by_reference(policy_bytes.as_slice()).expect("parse policy");
        let parsed_policy = &policy.0;
        Validate::validate(parsed_policy).expect("validate policy");

        let a_t = type_id_by_name(parsed_policy, "a_t");
        let b_t = type_id_by_name(parsed_policy, "b_t");

        assert!(parsed_policy
            .is_explicitly_allowed_custom(a_t, b_t, "class0", "perm0")
            .expect("query well-formed"));
    }

    #[test]
    fn no_explicit_allow_attr_attr() {
        let policy_bytes = include_bytes!(
            "../../testdata/micro_policies/no_allow_a_attr_b_attr_class0_perm0_policy.pp"
        );
        let policy = parse_policy_by_reference(policy_bytes.as_slice()).expect("parse policy");
        let parsed_policy = &policy.0;
        Validate::validate(parsed_policy).expect("validate policy");

        let a_t = type_id_by_name(parsed_policy, "a_t");
        let b_t = type_id_by_name(parsed_policy, "b_t");

        assert!(!parsed_policy
            .is_explicitly_allowed_custom(a_t, b_t, "class0", "perm0")
            .expect("query well-formed"));
    }

    #[test]
    fn compute_explicitly_allowed_multiple_attributes() {
        let policy_bytes = include_bytes!("../../testdata/micro_policies/allow_a_t_a1_attr_class0_perm0_a2_attr_class0_perm1_policy.pp");
        let policy = parse_policy_by_reference(policy_bytes.as_slice()).expect("parse policy");
        let parsed_policy = &policy.0;
        Validate::validate(parsed_policy).expect("validate policy");

        let a_t = type_id_by_name(parsed_policy, "a_t");

        let raw_access_vector =
            parsed_policy.compute_explicitly_allowed_custom(a_t, a_t, "class0").allow.0;

        // Two separate attributes are each allowed one permission on `[attr] self:class0`. Both
        // attributes are associated with "a_t". No other `allow` statements appear in the policy
        // in relation to "a_t". Therefore, we expect exactly two 1's in the access vector for
        // query `("a_t", "a_t", "class0")`.
        assert_eq!(2, raw_access_vector.count_ones());
    }

    #[test]
    fn new_file_security_context_minimal() {
        let policy_bytes =
            include_bytes!("../../testdata/composite_policies/compiled/minimal_policy.pp");
        let policy = parse_policy_by_reference(policy_bytes.as_slice())
            .expect("parse policy")
            .validate()
            .expect("validate policy");
        let source = policy
            .parse_security_context(b"source_u:source_r:source_t:s0:c0-s2:c0.c1".into())
            .expect("valid source security context");
        let target = policy
            .parse_security_context(b"target_u:target_r:target_t:s1:c1".into())
            .expect("valid target security context");

        let actual = policy.new_file_security_context(&source, &target, &FileClass::File);
        let expected: SecurityContext = policy
            .parse_security_context(b"source_u:object_r:target_t:s0:c0".into())
            .expect("valid expected security context");

        assert_eq!(expected, actual);
    }

    #[test]
    fn new_security_context_minimal() {
        let policy_bytes =
            include_bytes!("../../testdata/composite_policies/compiled/minimal_policy.pp");
        let policy = parse_policy_by_reference(policy_bytes.as_slice())
            .expect("parse policy")
            .validate()
            .expect("validate policy");
        let source = policy
            .parse_security_context(b"source_u:source_r:source_t:s0:c0-s2:c0.c1".into())
            .expect("valid source security context");
        let target = policy
            .parse_security_context(b"target_u:target_r:target_t:s1:c1".into())
            .expect("valid target security context");

        let actual = policy.new_security_context(&source, &target, &ObjectClass::Process);

        assert_eq!(source, actual);
    }

    #[test]
    fn new_file_security_context_class_defaults() {
        let policy_bytes =
            include_bytes!("../../testdata/composite_policies/compiled/class_defaults_policy.pp");
        let policy = parse_policy_by_reference(policy_bytes.as_slice())
            .expect("parse policy")
            .validate()
            .expect("validate policy");
        let source = policy
            .parse_security_context(b"source_u:source_r:source_t:s0:c0-s2:c0.c1".into())
            .expect("valid source security context");
        let target = policy
            .parse_security_context(b"target_u:target_r:target_t:s1:c0-s1:c0.c1".into())
            .expect("valid target security context");

        let actual = policy.new_file_security_context(&source, &target, &FileClass::File);
        let expected: SecurityContext = policy
            .parse_security_context(b"target_u:source_r:source_t:s1:c0-s1:c0.c1".into())
            .expect("valid expected security context");

        assert_eq!(expected, actual);
    }

    #[test]
    fn new_security_context_class_defaults() {
        let policy_bytes =
            include_bytes!("../../testdata/composite_policies/compiled/class_defaults_policy.pp");
        let policy = parse_policy_by_reference(policy_bytes.as_slice())
            .expect("parse policy")
            .validate()
            .expect("validate policy");
        let source = policy
            .parse_security_context(b"source_u:source_r:source_t:s0:c0-s2:c0.c1".into())
            .expect("valid source security context");
        let target = policy
            .parse_security_context(b"target_u:target_r:target_t:s1:c0-s1:c0.c1".into())
            .expect("valid target security context");

        let actual = policy.new_security_context(&source, &target, &ObjectClass::Process);
        let expected: SecurityContext = policy
            .parse_security_context(b"target_u:source_r:source_t:s1:c0-s1:c0.c1".into())
            .expect("valid expected security context");

        assert_eq!(expected, actual);
    }

    #[test]
    fn new_file_security_context_role_transition() {
        let policy_bytes =
            include_bytes!("../../testdata/composite_policies/compiled/role_transition_policy.pp");
        let policy = parse_policy_by_reference(policy_bytes.as_slice())
            .expect("parse policy")
            .validate()
            .expect("validate policy");
        let source = policy
            .parse_security_context(b"source_u:source_r:source_t:s0:c0-s2:c0.c1".into())
            .expect("valid source security context");
        let target = policy
            .parse_security_context(b"target_u:target_r:target_t:s1:c1".into())
            .expect("valid target security context");

        let actual = policy.new_file_security_context(&source, &target, &FileClass::File);
        let expected: SecurityContext = policy
            .parse_security_context(b"source_u:transition_r:target_t:s0:c0".into())
            .expect("valid expected security context");

        assert_eq!(expected, actual);
    }

    #[test]
    fn new_security_context_role_transition() {
        let policy_bytes =
            include_bytes!("../../testdata/composite_policies/compiled/role_transition_policy.pp");
        let policy = parse_policy_by_reference(policy_bytes.as_slice())
            .expect("parse policy")
            .validate()
            .expect("validate policy");
        let source = policy
            .parse_security_context(b"source_u:source_r:source_t:s0:c0-s2:c0.c1".into())
            .expect("valid source security context");
        let target = policy
            .parse_security_context(b"target_u:target_r:target_t:s1:c1".into())
            .expect("valid target security context");

        let actual = policy.new_security_context(&source, &target, &ObjectClass::Process);
        let expected: SecurityContext = policy
            .parse_security_context(b"source_u:transition_r:source_t:s0:c0-s2:c0.c1".into())
            .expect("valid expected security context");

        assert_eq!(expected, actual);
    }

    #[test]
    // TODO(http://b/334968228): Determine whether allow-role-transition check belongs in `new_file_security_context()`, or in the calling hooks, or `PermissionCheck::has_permission()`.
    #[ignore]
    fn new_file_security_context_role_transition_not_allowed() {
        let policy_bytes = include_bytes!(
            "../../testdata/composite_policies/compiled/role_transition_not_allowed_policy.pp"
        );
        let policy = parse_policy_by_reference(policy_bytes.as_slice())
            .expect("parse policy")
            .validate()
            .expect("validate policy");
        let source = policy
            .parse_security_context(b"source_u:source_r:source_t:s0:c0-s2:c0.c1".into())
            .expect("valid source security context");
        let target = policy
            .parse_security_context(b"target_u:target_r:target_t:s1:c1".into())
            .expect("valid target security context");

        let actual = policy.new_file_security_context(&source, &target, &FileClass::File);

        // TODO(http://b/334968228): Update expectation once role validation is implemented.
        assert!(policy.validate_security_context(&actual).is_err());
    }

    #[test]
    fn new_file_security_context_type_transition() {
        let policy_bytes =
            include_bytes!("../../testdata/composite_policies/compiled/type_transition_policy.pp");
        let policy = parse_policy_by_reference(policy_bytes.as_slice())
            .expect("parse policy")
            .validate()
            .expect("validate policy");
        let source = policy
            .parse_security_context(b"source_u:source_r:source_t:s0:c0-s2:c0.c1".into())
            .expect("valid source security context");
        let target = policy
            .parse_security_context(b"target_u:target_r:target_t:s1:c1".into())
            .expect("valid target security context");

        let actual = policy.new_file_security_context(&source, &target, &FileClass::File);
        let expected: SecurityContext = policy
            .parse_security_context(b"source_u:object_r:transition_t:s0:c0".into())
            .expect("valid expected security context");

        assert_eq!(expected, actual);
    }

    #[test]
    fn new_security_context_type_transition() {
        let policy_bytes =
            include_bytes!("../../testdata/composite_policies/compiled/type_transition_policy.pp");
        let policy = parse_policy_by_reference(policy_bytes.as_slice())
            .expect("parse policy")
            .validate()
            .expect("validate policy");
        let source = policy
            .parse_security_context(b"source_u:source_r:source_t:s0:c0-s2:c0.c1".into())
            .expect("valid source security context");
        let target = policy
            .parse_security_context(b"target_u:target_r:target_t:s1:c1".into())
            .expect("valid target security context");

        let actual = policy.new_security_context(&source, &target, &ObjectClass::Process);
        let expected: SecurityContext = policy
            .parse_security_context(b"source_u:source_r:transition_t:s0:c0-s2:c0.c1".into())
            .expect("valid expected security context");

        assert_eq!(expected, actual);
    }

    #[test]
    fn new_file_security_context_range_transition() {
        let policy_bytes =
            include_bytes!("../../testdata/composite_policies/compiled/range_transition_policy.pp");
        let policy = parse_policy_by_reference(policy_bytes.as_slice())
            .expect("parse policy")
            .validate()
            .expect("validate policy");
        let source = policy
            .parse_security_context(b"source_u:source_r:source_t:s0:c0-s2:c0.c1".into())
            .expect("valid source security context");
        let target = policy
            .parse_security_context(b"target_u:target_r:target_t:s1:c1".into())
            .expect("valid target security context");

        let actual = policy.new_file_security_context(&source, &target, &FileClass::File);
        let expected: SecurityContext = policy
            .parse_security_context(b"source_u:object_r:target_t:s1:c1-s2:c1.c2".into())
            .expect("valid expected security context");

        assert_eq!(expected, actual);
    }

    #[test]
    fn new_security_context_range_transition() {
        let policy_bytes =
            include_bytes!("../../testdata/composite_policies/compiled/range_transition_policy.pp");
        let policy = parse_policy_by_reference(policy_bytes.as_slice())
            .expect("parse policy")
            .validate()
            .expect("validate policy");
        let source = policy
            .parse_security_context(b"source_u:source_r:source_t:s0:c0-s2:c0.c1".into())
            .expect("valid source security context");
        let target = policy
            .parse_security_context(b"target_u:target_r:target_t:s1:c1".into())
            .expect("valid target security context");

        let actual = policy.new_security_context(&source, &target, &ObjectClass::Process);
        let expected: SecurityContext = policy
            .parse_security_context(b"source_u:source_r:source_t:s1:c1-s2:c1.c2".into())
            .expect("valid expected security context");

        assert_eq!(expected, actual);
    }
}
