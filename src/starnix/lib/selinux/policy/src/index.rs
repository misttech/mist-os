// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::{
    parser::ParseStrategy,
    symbols::{Class, Classes, CommonSymbol, CommonSymbols, Permission},
    ParsedPolicy,
};

use selinux_common::{self as sc, ClassPermission};
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

    pub(crate) fn parsed_policy(&self) -> &ParsedPolicy<PS> {
        &self.parsed_policy
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
