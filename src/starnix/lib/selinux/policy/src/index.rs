// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::{
    error::NewSecurityContextError,
    extensible_bitmap::ExtensibleBitmapSpan,
    parser::ParseStrategy,
    security_context::{self, SecurityLevel},
    symbols::{
        Class, ClassDefault, ClassDefaultRange, Classes, CommonSymbol, CommonSymbols, MlsLevel,
        Permission,
    },
    CategoryId, ParsedPolicy, RoleId, SecurityContext, TypeId,
};

use selinux_common::{self as sc, ClassPermission as _};
use std::{collections::HashMap, num::NonZeroU32};
use zerocopy::little_endian as le;

/// An index for facilitating fast lookup of common abstractions inside parsed binary policy data
/// structures. Typically, data is indexed by an enum that describes a well-known value and the
/// index stores the offset of the data in the binary policy to avoid scanning a collection to find
/// an element that contains a matching string. For example, the policy contains a collection of
/// classes that are identified by string names included in each collection entry. However,
/// `policy_index.classes(ObjectClass::Process).unwrap()` yields the offset in the policy's
/// collection of classes where the "process" class resides.
#[derive(Debug)]
pub(crate) struct PolicyIndex<PS: ParseStrategy> {
    /// Map from well-known classes to their offsets in the associate policy's
    /// [`crate::symbols::Classes`] collection.
    classes: HashMap<sc::ObjectClass, usize>,
    /// Map from well-known permissions to their class's associated [`crate::symbols::Permissions`]
    /// collection.
    permissions: HashMap<sc::Permission, PermissionIndex>,
    /// The parsed binary policy.
    parsed_policy: ParsedPolicy<PS>,
    /// The "object_r" role used as a fallback for new file context transitions.
    cached_object_r_role: RoleId,
}

impl<PS: ParseStrategy> PolicyIndex<PS> {
    /// Constructs a [`PolicyIndex`] that indexes over well-known policy fragment names. For
    /// example, the "process" class and its "fork" permission have prescribed meanings in an
    /// SELinux system, so the respective [`Class`] and [`Permission`] are indexed by this
    /// constructor. This operation fails if any well-known names are not found in `parsed_policy`.
    pub fn new(parsed_policy: ParsedPolicy<PS>) -> Result<Self, anyhow::Error> {
        let policy_classes = parsed_policy.classes();
        let common_symbols = parsed_policy.common_symbols();

        // Accumulate classes indexed by `selinux_common::ObjectClass`. If a class cannot be found
        // by name, add it to `missed_classes` for thorough error reporting.
        let mut classes = HashMap::new();
        let mut missed_classes = vec![];
        for known_class in sc::ObjectClass::all_variants().into_iter() {
            match get_class_index_by_name(policy_classes, known_class.name()) {
                Some(class_index) => {
                    classes.insert(known_class, class_index);
                }
                None => {
                    missed_classes.push(known_class);
                }
            }
        }
        if missed_classes.len() > 0 {
            return Err(anyhow::anyhow!(
                "failed to locate well-known SELinux object classes {:?} in SELinux binary policy",
                missed_classes.iter().map(sc::ObjectClass::name).collect::<Vec<_>>()
            ));
        }

        // Accumulate permissions indexed by `selinux_common::Permission`. If a permission cannot be
        // found by name, add it to `missed_permissions` for thorough error reporting.
        let mut permissions = HashMap::new();
        let mut missed_permissions = vec![];
        for known_permission in sc::Permission::all_variants().into_iter() {
            let object_class = known_permission.class();
            if let Some(class_index) = classes.get(&object_class) {
                let class = &policy_classes[*class_index];
                if let Some(permission_index) =
                    get_permission_index_by_name(common_symbols, class, known_permission.name())
                {
                    permissions.insert(known_permission, permission_index);
                } else {
                    missed_permissions.push(known_permission);
                }
            } else {
                missed_permissions.push(known_permission);
            }
        }
        if missed_permissions.len() > 0 {
            return Err(anyhow::anyhow!(
                "failed to locate well-known SELinux object permissions {:?} in SELinux binary policy",
                missed_permissions
                    .iter()
                    .map(|permission| {
                        let object_class = permission.class();
                        (object_class.name(), permission.name())
                    })
                    .collect::<Vec<_>>()
            ));
        }

        // Locate the "object_r" role.
        let cached_object_r_role = parsed_policy
            .role_by_name("object_r")
            .ok_or(anyhow::anyhow!(
                "failed to locate well-known 'object_r' role in SELinux binary policy"
            ))?
            .id();

        let index = Self { classes, permissions, parsed_policy, cached_object_r_role };

        // Verify that the initial Security Contexts are all defined, and valid.
        // TODO(b/335397745): Look up initial Contexts by name, and cache that index here.
        for id in sc::InitialSid::all_variants() {
            let _ = index.initial_context(id).map_err(anyhow::Error::from)?;
        }

        Ok(index)
    }

    pub fn class<'a>(&'a self, object_class: &sc::ObjectClass) -> &'a Class<PS> {
        let class_offset =
            *self.classes.get(object_class).expect("policy class index is exhaustive");
        &self.parsed_policy.classes()[class_offset]
    }

    pub fn permission<'a>(&'a self, permission: &sc::Permission) -> &'a Permission<PS> {
        let target_class = self.class(&permission.class());
        match *self.permissions.get(permission).expect("policy permission index is exhaustive") {
            PermissionIndex::Class { permission_index } => {
                &target_class.permissions()[permission_index]
            }
            PermissionIndex::Common { common_symbol_index, permission_index } => {
                let common_symbol = &self.parsed_policy().common_symbols()[common_symbol_index];
                &common_symbol.permissions()[permission_index]
            }
        }
    }

    pub fn new_file_security_context(
        &self,
        source: &SecurityContext,
        target: &SecurityContext,
        class: &sc::FileClass,
    ) -> Result<SecurityContext, NewSecurityContextError> {
        let object_class = sc::ObjectClass::from(class.clone());
        self.new_security_context(
            source,
            target,
            &object_class,
            // The SELinux notebook states the role component defaults to the object_r role.
            self.cached_object_r_role,
            // The SELinux notebook states the type component defaults to the type of the parent
            // directory.
            target.type_(),
            // The SELinux notebook states the range/level component defaults to the low/current
            // level of the creating process.
            source.low_level(),
            None,
        )
    }

    /// Calculates a new security context, as follows:
    ///
    /// - user: the `source` user, unless the policy contains a default_user statement for `class`.
    /// - role:
    ///     - if the policy contains a role_transition from the `source` role to the `target` type,
    ///       use the transition role
    ///     - otherwise, if the policy contains a default_role for `class`, use that default role
    ///     - lastly, if the policy does not contain either, use `default_role`.
    /// - type:
    ///     - if the policy contains a type_transition from the `source` type to the `target` type,
    ///       use the transition type
    ///     - otherwise, if the policy contains a default_type for `class`, use that default type
    ///     - lastly, if the policy does not contain either, use `default_type`.
    /// - range
    ///     - if the policy contains a range_transition from the `source` type to the `target` type,
    ///       use the transition range
    ///     - otherwise, if the policy contains a default_range for `class`, use that default range
    ///     - lastly, if the policy does not contain either, use the `default_low_level` -
    ///       `default_high_level` range.
    pub fn new_security_context(
        &self,
        source: &SecurityContext,
        target: &SecurityContext,
        class: &sc::ObjectClass,
        default_role: RoleId,
        default_type: TypeId,
        default_low_level: &SecurityLevel,
        default_high_level: Option<&SecurityLevel>,
    ) -> Result<SecurityContext, NewSecurityContextError> {
        let policy_class = self.class(&class);
        let class_defaults = policy_class.defaults();

        let user = match class_defaults.user() {
            ClassDefault::Source => source.user(),
            ClassDefault::Target => target.user(),
            _ => source.user(),
        };

        let role = match self.role_transition_new_role(source.role(), target.type_(), policy_class)
        {
            Some(new_role) => new_role,
            None => match class_defaults.role() {
                ClassDefault::Source => source.role(),
                ClassDefault::Target => target.role(),
                _ => default_role,
            },
        };

        let type_ =
            match self.type_transition_new_type(source.type_(), target.type_(), policy_class) {
                Some(new_type) => new_type,
                None => match class_defaults.type_() {
                    ClassDefault::Source => source.type_(),
                    ClassDefault::Target => target.type_(),
                    _ => default_type,
                },
            };

        let (low_level, high_level) =
            match self.range_transition_new_range(source.type_(), target.type_(), policy_class) {
                Some((low_level, high_level)) => (low_level, high_level),
                None => match class_defaults.range() {
                    ClassDefaultRange::SourceLow => (source.low_level().clone(), None),
                    ClassDefaultRange::SourceHigh => {
                        (source.high_level().unwrap_or(source.low_level()).clone(), None)
                    }
                    ClassDefaultRange::SourceLowHigh => {
                        (source.low_level().clone(), source.high_level().map(Clone::clone))
                    }
                    ClassDefaultRange::TargetLow => (target.low_level().clone(), None),
                    ClassDefaultRange::TargetHigh => {
                        (target.high_level().unwrap_or(target.low_level()).clone(), None)
                    }
                    ClassDefaultRange::TargetLowHigh => {
                        (target.low_level().clone(), target.high_level().map(Clone::clone))
                    }
                    _ => (default_low_level.clone(), default_high_level.map(Clone::clone)),
                },
            };

        // TODO(b/319232900): Ensure that the generated Context has e.g. valid security range.
        SecurityContext::new(self, user, role, type_, low_level.clone(), high_level.clone())
            .map_err(|err| NewSecurityContextError::MalformedComputedSecurityContext {
                source_security_context: source.clone(),
                target_security_context: target.clone(),
                computed_user: user,
                computed_role: role,
                computed_type: type_,
                computed_low_level: low_level,
                computed_high_level: high_level,
                error: err,
            })
    }

    /// Returns the Id of the "object_r" role within the `parsed_policy`, for use when validating
    /// Security Context fields.
    pub(crate) fn object_role(&self) -> RoleId {
        self.cached_object_r_role
    }

    pub(crate) fn parsed_policy(&self) -> &ParsedPolicy<PS> {
        &self.parsed_policy
    }

    /// Returns the [`SecurityContext`] defined by this policy for the specified
    /// well-known (or "initial") Id. Failure indicates that the parsed policy is not
    /// internally consistent.
    pub(super) fn initial_context(
        &self,
        id: sc::InitialSid,
    ) -> Result<security_context::SecurityContext, security_context::SecurityContextError> {
        let id = le::U32::from(id as u32);

        // Policy validation is assumed to have ensured that all `InitialSid` values exist
        // and have valid & consistent content.
        let context = self.parsed_policy().initial_context(id).unwrap();
        let low_level = self.security_level(context.low_level());
        let high_level = context.high_level().as_ref().map(|x| self.security_level(x));

        security_context::SecurityContext::new(
            &self,
            context.user_id(),
            context.role_id(),
            context.type_id(),
            low_level,
            high_level,
        )
    }

    /// Helper used by `initial_context()` to create a [`sc::SecurityLevel`] instance from
    /// the policy fields.
    fn security_level(&self, level: &MlsLevel<PS>) -> security_context::SecurityLevel {
        security_context::SecurityLevel::new(
            level.sensitivity(),
            level.categories().spans().map(|span| self.security_context_category(span)).collect(),
        )
    }

    /// Helper used by `security_level()` to create a `Category` instance from policy fields.
    fn security_context_category(&self, span: ExtensibleBitmapSpan) -> security_context::Category {
        // Spans describe zero-based bit indexes, corresponding to 1-based category Ids.
        if span.low == span.high {
            security_context::Category::Single(CategoryId(NonZeroU32::new(span.low + 1).unwrap()))
        } else {
            security_context::Category::Range {
                low: CategoryId(NonZeroU32::new(span.low + 1).unwrap()),
                high: CategoryId(NonZeroU32::new(span.high + 1).unwrap()),
            }
        }
    }

    fn role_transition_new_role(
        &self,
        current_role: RoleId,
        type_: TypeId,
        class: &Class<PS>,
    ) -> Option<RoleId> {
        self.parsed_policy
            .role_transitions()
            .iter()
            .find(|role_transition| {
                role_transition.current_role() == current_role
                    && role_transition.type_() == type_
                    && role_transition.class() == class.id()
            })
            .map(|x| x.new_role())
    }

    #[allow(dead_code)]
    // TODO(http://b/334968228): fn to be used again when checking role allow rules separately from
    // SID calculation.
    fn role_transition_is_explicitly_allowed(&self, source_role: RoleId, new_role: RoleId) -> bool {
        self.parsed_policy
            .role_allowlist()
            .iter()
            .find(|role_allow| {
                role_allow.source_role() == source_role && role_allow.new_role() == new_role
            })
            .is_some()
    }

    fn type_transition_new_type(
        &self,
        source_type: TypeId,
        target_type: TypeId,
        class: &Class<PS>,
    ) -> Option<TypeId> {
        // Return first match. The `checkpolicy` tool will not compile a policy that has
        // multiple matches, so behavior on multiple matches is undefined.
        self.parsed_policy
            .access_vectors()
            .iter()
            .find(|access_vector| {
                access_vector.is_type_transition()
                    && access_vector.source_type() == source_type
                    && access_vector.target_type() == target_type
                    && access_vector.target_class() == class.id()
            })
            .map(|x| x.new_type().unwrap())
    }

    fn range_transition_new_range(
        &self,
        source_type: TypeId,
        target_type: TypeId,
        class: &Class<PS>,
    ) -> Option<(security_context::SecurityLevel, Option<security_context::SecurityLevel>)> {
        for range_transition in self.parsed_policy.range_transitions() {
            if range_transition.source_type() == source_type
                && range_transition.target_type() == target_type
                && range_transition.target_class() == class.id()
            {
                let mls_range = range_transition.mls_range();
                let low_level = self.security_level(mls_range.low());
                let high_level =
                    mls_range.high().as_ref().map(|high_level| self.security_level(high_level));
                return Some((low_level, high_level));
            }
        }

        None
    }
}

/// Permissions may be stored in their associated [`Class`], or on the class's associated
/// [`CommonSymbol`]. This is a consequence of a limited form of inheritance supported for SELinux
/// policy classes. Classes may inherit from zero or one `common`. For example:
///
/// ```config
/// common file { ioctl read write create [...] }
/// class file inherits file { execute_no_trans entrypoint }
/// ```
///
/// In the above example, the "ioctl" permission for the "file" `class` is stored as a permission
/// on the "file" `common`, whereas the permission "execute_no_trans" is stored as a permission on
/// the "file" `class`.
#[derive(Debug)]
enum PermissionIndex {
    /// Permission is located at `Class::permissions()[permission_index]`.
    Class { permission_index: usize },
    /// Permission is located at
    /// `ParsedPolicy::common_symbols()[common_symbol_index].permissions()[permission_index]`.
    Common { common_symbol_index: usize, permission_index: usize },
}

fn get_class_index_by_name<'a, PS: ParseStrategy>(
    classes: &'a Classes<PS>,
    name: &str,
) -> Option<usize> {
    let name_bytes = name.as_bytes();
    for i in 0..classes.len() {
        if classes[i].name_bytes() == name_bytes {
            return Some(i);
        }
    }

    None
}

fn get_common_symbol_index_by_name_bytes<'a, PS: ParseStrategy>(
    common_symbols: &'a CommonSymbols<PS>,
    name_bytes: &[u8],
) -> Option<usize> {
    for i in 0..common_symbols.len() {
        if common_symbols[i].name_bytes() == name_bytes {
            return Some(i);
        }
    }

    None
}

fn get_permission_index_by_name<'a, PS: ParseStrategy>(
    common_symbols: &'a CommonSymbols<PS>,
    class: &'a Class<PS>,
    name: &str,
) -> Option<PermissionIndex> {
    if let Some(permission_index) = get_class_permission_index_by_name(class, name) {
        Some(PermissionIndex::Class { permission_index })
    } else if let Some(common_symbol_index) =
        get_common_symbol_index_by_name_bytes(common_symbols, class.common_name_bytes())
    {
        let common_symbol = &common_symbols[common_symbol_index];
        if let Some(permission_index) = get_common_permission_index_by_name(common_symbol, name) {
            Some(PermissionIndex::Common { common_symbol_index, permission_index })
        } else {
            None
        }
    } else {
        None
    }
}

fn get_class_permission_index_by_name<'a, PS: ParseStrategy>(
    class: &'a Class<PS>,
    name: &str,
) -> Option<usize> {
    let name_bytes = name.as_bytes();
    let permissions = class.permissions();
    for i in 0..permissions.len() {
        if permissions[i].name_bytes() == name_bytes {
            return Some(i);
        }
    }

    None
}

fn get_common_permission_index_by_name<'a, PS: ParseStrategy>(
    common_symbol: &'a CommonSymbol<PS>,
    name: &str,
) -> Option<usize> {
    let name_bytes = name.as_bytes();
    let permissions = common_symbol.permissions();
    for i in 0..permissions.len() {
        if permissions[i].name_bytes() == name_bytes {
            return Some(i);
        }
    }

    None
}
