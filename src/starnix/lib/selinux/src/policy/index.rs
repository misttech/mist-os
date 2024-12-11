// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::arrays::{Context, FsContext, FsUseType};
use super::extensible_bitmap::ExtensibleBitmapSpan;
use super::metadata::HandleUnknown;
use super::parser::ParseStrategy;
use super::security_context::{CategorySpan, SecurityContext, SecurityLevel};
use super::symbols::{
    Class, ClassDefault, ClassDefaultRange, Classes, CommonSymbol, CommonSymbols, MlsLevel,
    Permission,
};
use super::{CategoryId, ClassId, ParsedPolicy, RoleId, TypeId};

use crate::{ClassPermission as _, NullessByteStr};
use std::collections::HashMap;
use std::num::NonZeroU32;

/// The [`SecurityContext`] and [`FsUseType`] derived from some `fs_use_*` line of the policy.
pub struct FsUseLabelAndType {
    pub context: SecurityContext,
    pub use_type: FsUseType,
}

/// An index for facilitating fast lookup of common abstractions inside parsed binary policy data
/// structures. Typically, data is indexed by an enum that describes a well-known value and the
/// index stores the offset of the data in the binary policy to avoid scanning a collection to find
/// an element that contains a matching string. For example, the policy contains a collection of
/// classes that are identified by string names included in each collection entry. However,
/// `policy_index.classes(ObjectClass::Process).unwrap()` yields the offset in the policy's
/// collection of classes where the "process" class resides.
#[derive(Debug)]
pub(super) struct PolicyIndex<PS: ParseStrategy> {
    /// Map from well-known classes to their offsets in the associate policy's
    /// [`crate::symbols::Classes`] collection.
    classes: HashMap<crate::ObjectClass, usize>,
    /// Map from well-known permissions to their class's associated [`crate::symbols::Permissions`]
    /// collection.
    permissions: HashMap<crate::Permission, PermissionIndex>,
    /// The parsed binary policy.
    parsed_policy: ParsedPolicy<PS>,
    /// The "object_r" role used as a fallback for new file context transitions.
    cached_object_r_role: RoleId,
}

impl<PS: ParseStrategy> PolicyIndex<PS> {
    /// Constructs a [`PolicyIndex`] that indexes over well-known policy elements.
    ///
    /// [`Class`]es and [`Permission`]s used by the kernel are amongst the indexed elements.
    /// The policy's `handle_unknown()` configuration determines whether the policy can be loaded even
    /// if it omits classes or permissions expected by the kernel, and whether to allow or deny those
    /// permissions if so.
    pub fn new(parsed_policy: ParsedPolicy<PS>) -> Result<Self, anyhow::Error> {
        let policy_classes = parsed_policy.classes();
        let common_symbols = parsed_policy.common_symbols();

        // Accumulate classes indexed by `crate::ObjectClass`. If the policy defines that unknown
        // classes should cause rejection then return an error describing the missing element.
        let mut classes = HashMap::new();
        for known_class in crate::ObjectClass::all_variants().into_iter() {
            match get_class_index_by_name(policy_classes, known_class.name()) {
                Some(class_index) => {
                    classes.insert(known_class, class_index);
                }
                None => {
                    if parsed_policy.handle_unknown() == HandleUnknown::Reject {
                        return Err(anyhow::anyhow!("missing object class {:?}", known_class,));
                    }
                }
            }
        }

        // Accumulate permissions indexed by `crate::Permission`. If the policy defines that unknown
        // classes should cause rejection then return an error describing the missing element.
        let mut permissions = HashMap::new();
        for known_permission in crate::Permission::all_variants().into_iter() {
            let object_class = known_permission.class();
            if let Some(class_index) = classes.get(&object_class) {
                let class = &policy_classes[*class_index];
                if let Some(permission_index) =
                    get_permission_index_by_name(common_symbols, class, known_permission.name())
                {
                    permissions.insert(known_permission, permission_index);
                } else if parsed_policy.handle_unknown() == HandleUnknown::Reject {
                    return Err(anyhow::anyhow!(
                        "missing permission {:?}:{:?}",
                        object_class.name(),
                        known_permission.name(),
                    ));
                }
            }
        }

        // Locate the "object_r" role.
        let cached_object_r_role = parsed_policy
            .role_by_name("object_r")
            .ok_or_else(|| anyhow::anyhow!("missing 'object_r' role"))?
            .id();

        let index = Self { classes, permissions, parsed_policy, cached_object_r_role };

        // Verify that the initial Security Contexts are all defined, and valid.
        for id in crate::InitialSid::all_variants() {
            index.resolve_initial_context(id);
        }

        // Validate the contexts used in fs_use statements.
        for fs_use in index.parsed_policy.fs_uses() {
            index.security_context_from_policy_context(fs_use.context());
        }

        Ok(index)
    }

    pub fn class<'a>(&'a self, object_class: &crate::ObjectClass) -> Option<&'a Class<PS>> {
        self.classes.get(object_class).map(|offset| &self.parsed_policy.classes()[*offset])
    }

    pub fn permission<'a>(&'a self, permission: &crate::Permission) -> Option<&'a Permission<PS>> {
        let target_class = self.class(&permission.class())?;
        self.permissions.get(permission).map(|p| match p {
            PermissionIndex::Class { permission_index } => {
                &target_class.permissions()[*permission_index]
            }
            PermissionIndex::Common { common_symbol_index, permission_index } => {
                let common_symbol = &self.parsed_policy().common_symbols()[*common_symbol_index];
                &common_symbol.permissions()[*permission_index]
            }
        })
    }

    pub fn new_file_security_context(
        &self,
        source: &SecurityContext,
        target: &SecurityContext,
        class: &crate::FileClass,
    ) -> SecurityContext {
        let object_class = crate::ObjectClass::from(class.clone());
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
        class: &crate::ObjectClass,
        default_role: RoleId,
        default_type: TypeId,
        default_low_level: &SecurityLevel,
        default_high_level: Option<&SecurityLevel>,
    ) -> SecurityContext {
        let (user, role, type_, low_level, high_level) = if let Some(policy_class) =
            self.class(&class)
        {
            let class_defaults = policy_class.defaults();

            let user = match class_defaults.user() {
                ClassDefault::Source => source.user(),
                ClassDefault::Target => target.user(),
                _ => source.user(),
            };

            let role =
                match self.role_transition_new_role(source.role(), target.type_(), policy_class) {
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
                match self.range_transition_new_range(source.type_(), target.type_(), policy_class)
                {
                    Some((low_level, high_level)) => (low_level, high_level),
                    None => match class_defaults.range() {
                        ClassDefaultRange::SourceLow => (source.low_level().clone(), None),
                        ClassDefaultRange::SourceHigh => (
                            source.high_level().unwrap_or_else(|| source.low_level()).clone(),
                            None,
                        ),
                        ClassDefaultRange::SourceLowHigh => {
                            (source.low_level().clone(), source.high_level().map(Clone::clone))
                        }
                        ClassDefaultRange::TargetLow => (target.low_level().clone(), None),
                        ClassDefaultRange::TargetHigh => (
                            target.high_level().unwrap_or_else(|| target.low_level()).clone(),
                            None,
                        ),
                        ClassDefaultRange::TargetLowHigh => {
                            (target.low_level().clone(), target.high_level().map(Clone::clone))
                        }
                        _ => (default_low_level.clone(), default_high_level.map(Clone::clone)),
                    },
                };

            (user, role, type_, low_level, high_level)
        } else {
            // If the class is not defined in the policy then there can be no transitions, nor class-defined choice of
            // defaults, so the caller-supplied defaults (effectively "unspecified") should be used.
            (
                source.user(),
                default_role,
                default_type,
                default_low_level.clone(),
                default_high_level.map(Clone::clone),
            )
        };

        // `new()` may fail if the resulting combination of user, role etc is not permitted by the policy.
        SecurityContext::new(user, role, type_, low_level, high_level)

        // TODO(http://b/334968228): Validate domain & role transitions are allowed?
    }

    /// Returns the Id of the "object_r" role within the `parsed_policy`, for use when validating
    /// Security Context fields.
    pub(super) fn object_role(&self) -> RoleId {
        self.cached_object_r_role
    }

    pub(super) fn parsed_policy(&self) -> &ParsedPolicy<PS> {
        &self.parsed_policy
    }

    /// Returns the [`SecurityContext`] defined by this policy for the specified
    /// well-known (or "initial") Id.
    pub(super) fn initial_context(&self, id: crate::InitialSid) -> SecurityContext {
        // All [`InitialSid`] have already been verified as resolvable, by `new()`.
        self.resolve_initial_context(id)
    }

    /// If there is an fs_use statement for the given filesystem type, returns the associated
    /// [`SecurityContext`] and [`FsUseType`].
    pub(super) fn fs_use_label_and_type(
        &self,
        fs_type: NullessByteStr<'_>,
    ) -> Option<FsUseLabelAndType> {
        self.parsed_policy
            .fs_uses()
            .iter()
            .find(|fs_use| fs_use.fs_type() == fs_type.as_bytes())
            .map(|fs_use| FsUseLabelAndType {
                context: self.security_context_from_policy_context(fs_use.context()),
                use_type: fs_use.behavior(),
            })
    }

    /// If there is a genfscon statement for the given filesystem type, returns the associated
    /// [`SecurityContext`], taking the `node_path` into account. `class_id` defines the type
    /// of the file in the given `node_path`. It can only be omitted when looking up the filesystem
    /// label.
    pub(super) fn genfscon_label_for_fs_and_path(
        &self,
        fs_type: NullessByteStr<'_>,
        node_path: NullessByteStr<'_>,
        class_id: Option<ClassId>,
    ) -> Option<SecurityContext> {
        // All contexts listed in the policy for the file system type.
        let fs_contexts = self
            .parsed_policy
            .generic_fs_contexts()
            .iter()
            .find(|genfscon| genfscon.fs_type() == fs_type.as_bytes())?
            .contexts();

        // The correct match is the closest parent among the ones given in the policy file.
        // E.g. if in the policy we have
        //     genfscon foofs "/" label1
        //     genfscon foofs "/abc/" label2
        //     genfscon foofs "/abc/def" label3
        //
        // The correct label for a file "/abc/def/g/h/i" is label3, as "/abc/def" is the closest parent
        // among those defined.
        //
        // Partial paths are prefix-matched, so that "/abc/default" would also be assigned label3.
        //
        // TODO(372212126): Optimize the algorithm.
        let mut result: Option<&FsContext<PS>> = None;
        for fs_context in fs_contexts {
            if node_path.0.starts_with(fs_context.partial_path()) {
                if result.is_none()
                    || result.unwrap().partial_path().len() < fs_context.partial_path().len()
                {
                    if class_id.is_none()
                        || fs_context
                            .class()
                            .map(|other| other == class_id.unwrap())
                            .unwrap_or(true)
                    {
                        result = Some(fs_context);
                    }
                }
            }
        }

        // The returned SecurityContext must be valid with respect to the policy, since otherwise
        // we'd have rejected the policy load.
        result.and_then(|fs_context| {
            Some(self.security_context_from_policy_context(fs_context.context()))
        })
    }

    /// Helper used to construct and validate well-known [`SecurityContext`] values.
    fn resolve_initial_context(&self, id: crate::InitialSid) -> SecurityContext {
        self.security_context_from_policy_context(self.parsed_policy().initial_context(id))
    }

    /// Returns a [`SecurityContext`] based on the supplied policy-defined `context`.
    fn security_context_from_policy_context(&self, context: &Context<PS>) -> SecurityContext {
        let low_level = self.security_level(context.low_level());
        let high_level = context.high_level().as_ref().map(|x| self.security_level(x));

        // Creation of the new [`SecurityContext`] will fail if the fields are inconsistent
        // with the policy-defined constraints (e.g. on user roles, etc).
        SecurityContext::new(
            context.user_id(),
            context.role_id(),
            context.type_id(),
            low_level,
            high_level,
        )
    }

    /// Helper used by `initial_context()` to create a [`crate::SecurityLevel`] instance from
    /// the policy fields.
    fn security_level(&self, level: &MlsLevel<PS>) -> SecurityLevel {
        SecurityLevel::new(
            level.sensitivity(),
            level.categories().spans().map(|span| self.security_context_category(span)).collect(),
        )
    }

    /// Helper used by `security_level()` to create a `CategorySpan` instance from policy fields.
    fn security_context_category(&self, span: ExtensibleBitmapSpan) -> CategorySpan {
        // Spans describe zero-based bit indexes, corresponding to 1-based category Ids.
        CategorySpan::new(
            CategoryId(NonZeroU32::new(span.low + 1).unwrap()),
            CategoryId(NonZeroU32::new(span.high + 1).unwrap()),
        )
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
    ) -> Option<(SecurityLevel, Option<SecurityLevel>)> {
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
