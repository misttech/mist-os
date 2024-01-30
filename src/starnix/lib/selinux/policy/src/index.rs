// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::{
    parser::ParseStrategy,
    symbols::{Class, Permission},
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
    permissions: HashMap<sc::Permission, usize>,
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
        let mut classes = HashMap::new();
        let mut known_classes = sc::ObjectClass::all_variants();
        for i in 0..policy_classes.len() {
            let policy_class = &policy_classes[i];
            let mut found = None;
            for j in 0..known_classes.len() {
                let known_class = &known_classes[j];
                if policy_class.name_bytes() == known_class.name().as_bytes() {
                    found = Some(j);
                    break;
                }
            }
            if let Some(j) = found {
                let known_class = known_classes.remove(j);
                classes.insert(known_class.clone(), i);
            }
        }
        if known_classes.len() > 0 {
            return Err(anyhow::anyhow!(
                "failed to locate well-known SELinux object classes {:?} in SELinux binary policy",
                known_classes.iter().map(sc::ObjectClass::name).collect::<Vec<_>>()
            ));
        }

        let mut permissions = HashMap::new();
        let mut known_permissions = sc::Permission::all_variants();
        let mut i = 0;
        while i < known_permissions.len() {
            let known_permission = &known_permissions[i];
            let object_class = known_permission.class();
            if let Some(class_idx) = classes.get(&object_class) {
                let class = &policy_classes[*class_idx];
                let mut found = None;
                let class_permissions = class.permissions();
                for j in 0..class_permissions.len() {
                    let permission = &class_permissions[j];
                    if permission.name_bytes() == known_permission.name().as_bytes() {
                        found = Some(j);
                        break;
                    }
                }
                if let Some(j) = found {
                    let known_permission = known_permissions.remove(i);
                    permissions.insert(known_permission, j);

                    // `i = i + 1 - 1 = i` on account of `known_permissions` removal.
                } else {
                    // Known permission not found. Skip it and report the full set of missing
                    // permissions after loop completes.
                    i += 1
                }
            } else {
                // Permission class not found. Skip this permission and report the full set of
                // missing permissions after loop completes.
                i += 1;
            }
        }
        if known_permissions.len() > 0 {
            return Err(anyhow::anyhow!(
                "failed to locate well-known SELinux object permissions {:?} in SELinux binary policy",
                known_permissions
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
        let permission_offset =
            *self.permissions.get(permission).expect("policy permission index is exhaustive");
        &target_class.permissions()[permission_offset]
    }

    pub(crate) fn parsed_policy(&self) -> &ParsedPolicy<PS> {
        &self.parsed_policy
    }
}
