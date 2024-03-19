// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::symbols::{find_role_by_id, Role};

use super::{
    error::NewSecurityContextError,
    parser::ParseStrategy,
    symbols::{
        Class, ClassDefault, ClassDefaultRange, Classes, CommonSymbol, CommonSymbols, Permission,
    },
    ParsedPolicy, RoleId, SecurityContext, TypeId,
};

use selinux_common::{self as sc, ClassPermission as _};
use std::collections::HashMap;

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

        Ok(Self { classes, permissions, parsed_policy })
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
        let policy_class = self.class(&object_class);
        let class_defaults = policy_class.defaults();

        // The SELinux notebook states:
        //
        // The user component is inherited from the creating process (policy version 27 allows a
        // default_user of source or target to be defined for each object class).
        let user = match class_defaults.user() {
            ClassDefault::Source => source.user(),
            ClassDefault::Target => target.user(),
            _ => source.user(),
        };

        // The SELinux notebook states:
        //
        // The role component generally defaults to the object_r role (policy version 26 allows a
        // role_transition and version 27 allows a default_role of source or target to be defined
        // for each object class).
        let source_role = self.parsed_policy.find_role(source.role());
        let computed_role =
            match self.role_transition_new_role(source_role, target.type_(), policy_class) {
                Some(new_role) => {
                    if !self.role_transition_is_explicitly_allowed(source_role, new_role) {
                        return Err(NewSecurityContextError::RoleTransitionNotAllowed {
                            source_security_context: source.clone(),
                            target_security_context: target.clone(),
                            source_role: std::str::from_utf8(source_role.name_bytes())
                                .expect("valid role name in policy")
                                .to_string(),
                            new_role: std::str::from_utf8(new_role.name_bytes())
                                .expect("valid role name in policy")
                                .to_string(),
                        });
                    }

                    new_role
                }
                None => {
                    match class_defaults.role() {
                        ClassDefault::Source => self.parsed_policy.find_role(source.role()),
                        ClassDefault::Target => self.parsed_policy.find_role(target.role()),
                        _ => {
                            // TODO: This special role should probably be cached somewhere.
                            let object_r = RoleId("object_r".to_string());
                            self.parsed_policy.find_role(&object_r)
                        }
                    }
                }
            };
        let role = RoleId(
            std::str::from_utf8(computed_role.name_bytes())
                .expect("valid role name in policy")
                .to_owned(),
        );

        // The SELinux notebook states:
        //
        // The type component defaults to the type of the parent directory if no matching
        // type_transition rule was specified in the policy (policy version 25 allows a filename
        // type_transition rule and version 28 allows a default_type of source or target to be
        // defined for each object class).
        //
        // TODO(b/322353836): Implement `type_transition` handling.
        let type_ = match class_defaults.type_() {
            ClassDefault::Source => source.type_(),
            ClassDefault::Target => target.type_(),
            // The "parent directory" in this context is the target. (The source is the process
            // creating the file-like object.)
            _ => target.type_(),
        };

        // The SELinux notebook states:
        //
        // The range/level component defaults to the low/current level of the creating process if no
        // matching range_transition rule was specified in the policy (policy version 27 allows a
        // default_range of source or target with the selected range being low, high or low-high to
        // be defined for each object class).
        //
        // TODO(b/322353836): Implement `range_transition` handling.
        let (low_level, high_level) = match class_defaults.range() {
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
            _ => (source.low_level().clone(), None),
        };

        // TODO(b/322353836): Implement transitions for `*_transition` rules based on initial
        // `user`, `role`, `type_`, `range` values.
        // TODO(b/319232900): Ensure that the generated Context has e.g. valid security range.
        Ok(SecurityContext::new(user.clone(), role, type_.clone(), low_level, high_level))
    }

    pub(crate) fn parsed_policy(&self) -> &ParsedPolicy<PS> {
        &self.parsed_policy
    }

    fn role_transition_new_role(
        &self,
        current_role: &Role<PS>,
        type_id: &TypeId,
        class: &Class<PS>,
    ) -> Option<&Role<PS>> {
        let type_ = self.parsed_policy.find_type_alias_or_attribute(type_id);
        let role_transitions = self.parsed_policy.role_transitions();

        for role_transition in role_transitions {
            if role_transition.current_role() == current_role.id()
                && role_transition.type_() == type_.id()
                && role_transition.class() == class.id()
            {
                let new_role =
                    find_role_by_id(self.parsed_policy.roles(), role_transition.new_role())?;
                return Some(new_role);
            }
        }

        None
    }

    fn role_transition_is_explicitly_allowed(
        &self,
        source_role: &Role<PS>,
        new_role: &Role<PS>,
    ) -> bool {
        for role_allow in self.parsed_policy.role_allowlist() {
            if role_allow.source_role() == source_role.id()
                && role_allow.new_role() == new_role.id()
            {
                return true;
            }
        }

        false
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
