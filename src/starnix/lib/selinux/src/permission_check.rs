// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::access_vector_cache::{Fixed, Locked, Query, DEFAULT_SHARED_SIZE};
use crate::policy::{AccessVectorComputer, SELINUX_AVD_FLAGS_PERMISSIVE};
use crate::security_server::SecurityServer;
use crate::{ClassPermission, Permission, SecurityId};

#[cfg(target_os = "fuchsia")]
use fuchsia_inspect_contrib::profile_duration;

use std::sync::Weak;

/// Describes the result of a permission lookup between two Security Contexts.
#[derive(Clone, Debug, PartialEq)]
pub struct PermissionCheckResult {
    /// True if the specified permissions should be permitted.
    pub permit: bool,

    /// True if details of the check should be audit logged. Audit logs are by default only output
    /// when the policy defines that the permissions should be denied (whether or not the check is
    /// "permissive"), but may be suppressed for some denials ("dontaudit"), or for some allowed
    /// permissions ("auditallow").
    pub audit: bool,
}

/// Implements the `has_permission()` API, based on supplied `Query` and `AccessVectorComputer`
/// implementations.
// TODO: https://fxbug.dev/362699811 - Revise the traits to avoid direct dependencies on `SecurityServer`.
pub struct PermissionCheck<'a> {
    security_server: &'a SecurityServer,
    access_vector_cache: &'a Locked<Fixed<Weak<SecurityServer>, DEFAULT_SHARED_SIZE>>,
}

impl<'a> PermissionCheck<'a> {
    pub(crate) fn new(
        security_server: &'a SecurityServer,
        access_vector_cache: &'a Locked<Fixed<Weak<SecurityServer>, DEFAULT_SHARED_SIZE>>,
    ) -> Self {
        Self { security_server, access_vector_cache }
    }

    /// Returns whether the `source_sid` has the specified `permission` on `target_sid`.
    /// The result indicates both whether `permission` is `permit`ted, and whether the caller
    /// should `audit` log the query.
    pub fn has_permission<P: ClassPermission + Into<Permission> + Clone + 'static>(
        &self,
        source_sid: SecurityId,
        target_sid: SecurityId,
        permission: P,
    ) -> PermissionCheckResult {
        has_permission(
            self.security_server.is_enforcing(),
            self.access_vector_cache,
            self.security_server,
            source_sid,
            target_sid,
            permission,
        )
    }

    // TODO: https://fxbug.dev/362699811 - Remove this once `SecurityServer` APIs such as `sid_to_security_context()`
    // are exposed via a trait rather than directly by that implementation.
    pub fn security_server(&self) -> &SecurityServer {
        self.security_server
    }
}

/// Internal implementation of the `has_permission()` API, in terms of the `Query` and `AccessVectorComputer` traits.
fn has_permission<P: ClassPermission + Into<Permission> + Clone + 'static>(
    is_enforcing: bool,
    query: &impl Query,
    access_vector_computer: &impl AccessVectorComputer,
    source_sid: SecurityId,
    target_sid: SecurityId,
    permission: P,
) -> PermissionCheckResult {
    #[cfg(target_os = "fuchsia")]
    profile_duration!("libselinux.check_permission");
    let target_class = permission.class();

    let decision = query.query(source_sid, target_sid, target_class.into());

    let mut result = if let Some(permission_access_vector) =
        access_vector_computer.access_vector_from_permissions(&[permission])
    {
        let permit = permission_access_vector & decision.allow == permission_access_vector;
        let audit = if permit {
            permission_access_vector & decision.auditallow == permission_access_vector
        } else {
            permission_access_vector & decision.auditdeny == permission_access_vector
        };
        PermissionCheckResult { permit, audit }
    } else {
        PermissionCheckResult { permit: false, audit: true }
    };

    if !result.permit {
        if !is_enforcing {
            // If the security server is not currently enforcing then permit all access.
            result.permit = true;
        } else if decision.flags & SELINUX_AVD_FLAGS_PERMISSIVE != 0 {
            // If the access decision indicates that the source domain is permissive then permit
            // all access.
            result.permit = true;

            // TODO: https://fxbug.dev/379153786 - Don't audit permissive types, to reduce log
            // overload in tests, until "dontaudit" is available.
            result.audit = false;
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::access_vector_cache::DenyAll;
    use crate::policy::testing::{ACCESS_VECTOR_0001, ACCESS_VECTOR_0010};
    use crate::policy::{AccessDecision, AccessVector};
    use crate::{AbstractObjectClass, ProcessPermission};

    use std::any::Any;
    use std::num::NonZeroU32;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::LazyLock;

    /// SID to use where any value will do.
    static A_TEST_SID: LazyLock<SecurityId> = LazyLock::new(unique_sid);

    /// Returns a new `SecurityId` with unique id.
    fn unique_sid() -> SecurityId {
        static NEXT_ID: AtomicU32 = AtomicU32::new(1000);
        SecurityId(NonZeroU32::new(NEXT_ID.fetch_add(1, Ordering::AcqRel)).unwrap())
    }

    fn access_vector_from_permission<P: ClassPermission + Into<Permission> + 'static>(
        permission: P,
    ) -> AccessVector {
        let any = &permission as &dyn Any;
        let permission_ref = match any.downcast_ref::<ProcessPermission>() {
            Some(permission_ref) => permission_ref,
            None => return AccessVector::NONE,
        };

        match permission_ref {
            ProcessPermission::Fork => ACCESS_VECTOR_0001,
            ProcessPermission::Transition => ACCESS_VECTOR_0010,
            _ => AccessVector::NONE,
        }
    }

    fn access_vector_from_permissions<
        'a,
        P: ClassPermission + Into<Permission> + Clone + 'static,
    >(
        permissions: &[P],
    ) -> AccessVector {
        let mut access_vector = AccessVector::NONE;
        for permission in permissions {
            access_vector |= access_vector_from_permission(permission.clone());
        }
        access_vector
    }

    #[derive(Default)]
    pub struct DenyAllPermissions(DenyAll);

    impl Query for DenyAllPermissions {
        fn query(
            &self,
            source_sid: SecurityId,
            target_sid: SecurityId,
            target_class: AbstractObjectClass,
        ) -> AccessDecision {
            self.0.query(source_sid, target_sid, target_class)
        }
    }

    impl AccessVectorComputer for DenyAllPermissions {
        fn access_vector_from_permissions<
            P: ClassPermission + Into<Permission> + Clone + 'static,
        >(
            &self,
            permissions: &[P],
        ) -> Option<AccessVector> {
            Some(access_vector_from_permissions(permissions))
        }
    }

    /// A [`Query`] that permits all [`AccessVector`].
    #[derive(Default)]
    struct AllowAllPermissions;

    impl Query for AllowAllPermissions {
        fn query(
            &self,
            _source_sid: SecurityId,
            _target_sid: SecurityId,
            _target_class: AbstractObjectClass,
        ) -> AccessDecision {
            AccessDecision::allow(AccessVector::ALL)
        }
    }

    impl AccessVectorComputer for AllowAllPermissions {
        fn access_vector_from_permissions<
            P: ClassPermission + Into<Permission> + Clone + 'static,
        >(
            &self,
            permissions: &[P],
        ) -> Option<AccessVector> {
            Some(access_vector_from_permissions(permissions))
        }
    }

    #[test]
    fn has_permission_both() {
        let deny_all: DenyAllPermissions = Default::default();
        let allow_all: AllowAllPermissions = Default::default();

        // Use permissions that are mapped to access vector bits in
        // `access_vector_from_permission`.
        let permissions = [ProcessPermission::Fork, ProcessPermission::Transition];
        for permission in &permissions {
            // DenyAllPermissions denies.
            assert_eq!(
                PermissionCheckResult { permit: false, audit: true },
                has_permission(
                    /*is_enforcing=*/ true,
                    &deny_all,
                    &deny_all,
                    *A_TEST_SID,
                    *A_TEST_SID,
                    permission.clone()
                )
            );
            // AllowAllPermissions allows.
            assert_eq!(
                PermissionCheckResult { permit: true, audit: false },
                has_permission(
                    /*is_enforcing=*/ true,
                    &allow_all,
                    &allow_all,
                    *A_TEST_SID,
                    *A_TEST_SID,
                    permission.clone()
                )
            );
        }
    }
}
