// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::access_vector_cache::{FifoQueryCache, Locked, Query};
use crate::policy::{AccessVector, AccessVectorComputer, SELINUX_AVD_FLAGS_PERMISSIVE};
use crate::security_server::SecurityServer;
use crate::{ClassPermission, FsNodeClass, NullessByteStr, Permission, SecurityId};

#[cfg(target_os = "fuchsia")]
use fuchsia_inspect_contrib::profile_duration;

use std::num::NonZeroU64;
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

    /// If the `AccessDecision` indicates that permission denials should not be enforced then `permit`
    /// will be true, and this field will hold the Id of the bug to reference in audit logging.
    pub todo_bug: Option<NonZeroU64>,
}

/// Implements the `has_permission()` API, based on supplied `Query` and `AccessVectorComputer`
/// implementations.
// TODO: https://fxbug.dev/362699811 - Revise the traits to avoid direct dependencies on `SecurityServer`.
pub struct PermissionCheck<'a> {
    security_server: &'a SecurityServer,
    access_vector_cache: &'a Locked<FifoQueryCache<Weak<SecurityServer>>>,
}

impl<'a> PermissionCheck<'a> {
    pub(crate) fn new(
        security_server: &'a SecurityServer,
        access_vector_cache: &'a Locked<FifoQueryCache<Weak<SecurityServer>>>,
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

    /// Returns whether the `source_sid` has both the `ioctl` permission and the specified extended
    /// permission on `target_sid`, and whether the decision should be audited.
    ///
    /// A request is allowed if the ioctl permission is `allow`ed and either the numeric ioctl
    /// extended permission is `allowxperm`, or ioctl extended permissions are not filtered for this
    /// domain.
    ///
    /// A granted request is audited if the ioctl permission is `auditallow` and the numeric ioctl
    /// extended permission is `auditallowxperm`.
    ///
    /// A denied request is audited if the ioctl permission is `dontaudit` or the numeric ioctl
    /// extended permission is `dontauditxperm`.
    pub fn has_ioctl_permission<P: ClassPermission + Into<Permission> + Clone + 'static>(
        &self,
        source_sid: SecurityId,
        target_sid: SecurityId,
        permission: P,
        ioctl: u16,
    ) -> PermissionCheckResult {
        has_ioctl_permission(
            self.security_server.is_enforcing(),
            self.access_vector_cache,
            self.security_server,
            source_sid,
            target_sid,
            permission,
            ioctl,
        )
    }

    // TODO: https://fxbug.dev/362699811 - Remove this once `SecurityServer` APIs such as `sid_to_security_context()`
    // are exposed via a trait rather than directly by that implementation.
    pub fn security_server(&self) -> &SecurityServer {
        self.security_server
    }

    /// Returns the SID with which to label a new `file_class` instance created by `subject_sid`, with `target_sid`
    /// as its parent, taking into account role & type transition rules, and filename-transition rules.
    /// If a filename-transition rule matches the `fs_node_name` then that will be used, otherwise the
    /// filename-independent computation will be applied.
    pub fn compute_new_fs_node_sid(
        &self,
        source_sid: SecurityId,
        target_sid: SecurityId,
        fs_node_class: FsNodeClass,
        fs_node_name: NullessByteStr<'_>,
    ) -> Result<SecurityId, anyhow::Error> {
        // TODO: https://fxbug.dev/385075470 - Stop skipping empty name lookups once by-name lookup is better optimized.
        if !fs_node_name.as_bytes().is_empty() {
            if let Some(sid) = self.access_vector_cache.compute_new_fs_node_sid_with_name(
                source_sid,
                target_sid,
                fs_node_class,
                fs_node_name,
            ) {
                return Ok(sid);
            }
        }
        self.access_vector_cache.compute_new_fs_node_sid(source_sid, target_sid, fs_node_class)
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

    let decision = query.compute_access_decision(source_sid, target_sid, target_class.into());

    let mut result = if let Some(permission_access_vector) =
        access_vector_computer.access_vector_from_permissions(&[permission])
    {
        let permit = permission_access_vector & decision.allow == permission_access_vector;
        let audit = if permit {
            permission_access_vector & decision.auditallow != AccessVector::NONE
        } else {
            permission_access_vector & decision.auditdeny != AccessVector::NONE
        };
        PermissionCheckResult { permit, audit, todo_bug: None }
    } else {
        PermissionCheckResult { permit: false, audit: true, todo_bug: None }
    };

    if !result.permit {
        if !is_enforcing {
            // If the security server is not currently enforcing then permit all access.
            result.permit = true;
        } else if decision.flags & SELINUX_AVD_FLAGS_PERMISSIVE != 0 {
            // If the access decision indicates that the source domain is permissive then permit
            // all access.
            result.permit = true;
        } else if decision.todo_bug.is_some() {
            // If the access decision includes a `todo_bug` then permit the access and return the
            // bug Id to the caller, for audit logging.
            result.permit = true;
            result.todo_bug = decision.todo_bug;
        }
    }

    result
}

/// Internal implementation of the `has_ioctl_permission()` API, in terms of the `Query` and
/// `AccessVectorComputer` traits.
fn has_ioctl_permission<P: ClassPermission + Into<Permission> + Clone + 'static>(
    is_enforcing: bool,
    query: &impl Query,
    access_vector_computer: &impl AccessVectorComputer,
    source_sid: SecurityId,
    target_sid: SecurityId,
    permission: P,
    ioctl: u16,
) -> PermissionCheckResult {
    let target_class = permission.class();

    let permission_decision =
        query.compute_access_decision(source_sid, target_sid, target_class.into());

    let [ioctl_postfix, ioctl_prefix] = ioctl.to_le_bytes();
    let xperm_decision = query.compute_ioctl_access_decision(
        source_sid,
        target_sid,
        target_class.into(),
        ioctl_prefix,
    );

    let mut result = if let Some(permission_access_vector) =
        access_vector_computer.access_vector_from_permissions(&[permission])
    {
        let permit = (permission_access_vector & permission_decision.allow
            == permission_access_vector)
            && xperm_decision.allow.contains(ioctl_postfix);
        let audit = if permit {
            (permission_access_vector & permission_decision.auditallow == permission_access_vector)
                && xperm_decision.auditallow.contains(ioctl_postfix)
        } else {
            (permission_access_vector & permission_decision.auditdeny == permission_access_vector)
                && xperm_decision.auditdeny.contains(ioctl_postfix)
        };
        PermissionCheckResult { permit, audit, todo_bug: None }
    } else {
        PermissionCheckResult { permit: false, audit: true, todo_bug: None }
    };

    if !result.permit {
        if !is_enforcing {
            result.permit = true;
        } else if permission_decision.flags & SELINUX_AVD_FLAGS_PERMISSIVE != 0 {
            result.permit = true;
        } else if permission_decision.todo_bug.is_some() {
            // Currently we can make an exception for the overall `ioctl` permission,
            // but not for specific ioctl xperms.
            result.permit = true;
            result.todo_bug = permission_decision.todo_bug;
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::access_vector_cache::DenyAll;
    use crate::policy::testing::{ACCESS_VECTOR_0001, ACCESS_VECTOR_0010};
    use crate::policy::{AccessDecision, AccessVector, IoctlAccessDecision};
    use crate::{
        AbstractObjectClass, CommonFilePermission, CommonFsNodePermission, FileClass,
        FilePermission, ProcessPermission,
    };

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
        match permission.into() {
            // Process class permissions
            Permission::Process(ProcessPermission::Fork) => ACCESS_VECTOR_0001,
            Permission::Process(ProcessPermission::Transition) => ACCESS_VECTOR_0010,
            // File class permissions
            Permission::File(FilePermission::Common(CommonFilePermission::Common(
                CommonFsNodePermission::Ioctl,
            ))) => ACCESS_VECTOR_0001,
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
        fn compute_access_decision(
            &self,
            source_sid: SecurityId,
            target_sid: SecurityId,
            target_class: AbstractObjectClass,
        ) -> AccessDecision {
            self.0.compute_access_decision(source_sid, target_sid, target_class)
        }

        fn compute_new_fs_node_sid(
            &self,
            _source_sid: SecurityId,
            _target_sid: SecurityId,
            _fs_node_class: FsNodeClass,
        ) -> Result<SecurityId, anyhow::Error> {
            unreachable!();
        }

        fn compute_new_fs_node_sid_with_name(
            &self,
            _source_sid: SecurityId,
            _target_sid: SecurityId,
            _fs_node_class: FsNodeClass,
            _fs_node_name: NullessByteStr<'_>,
        ) -> Option<SecurityId> {
            unreachable!();
        }

        fn compute_ioctl_access_decision(
            &self,
            source_sid: SecurityId,
            target_sid: SecurityId,
            target_class: AbstractObjectClass,
            ioctl_prefix: u8,
        ) -> IoctlAccessDecision {
            self.0.compute_ioctl_access_decision(source_sid, target_sid, target_class, ioctl_prefix)
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
        fn compute_access_decision(
            &self,
            _source_sid: SecurityId,
            _target_sid: SecurityId,
            _target_class: AbstractObjectClass,
        ) -> AccessDecision {
            AccessDecision::allow(AccessVector::ALL)
        }

        fn compute_new_fs_node_sid(
            &self,
            _source_sid: SecurityId,
            _target_sid: SecurityId,
            _fs_node_class: FsNodeClass,
        ) -> Result<SecurityId, anyhow::Error> {
            unreachable!();
        }

        fn compute_new_fs_node_sid_with_name(
            &self,
            _source_sid: SecurityId,
            _target_sid: SecurityId,
            _fs_node_class: FsNodeClass,
            _fs_node_name: NullessByteStr<'_>,
        ) -> Option<SecurityId> {
            unreachable!();
        }

        fn compute_ioctl_access_decision(
            &self,
            _source_sid: SecurityId,
            _target_sid: SecurityId,
            _target_class: AbstractObjectClass,
            _ioctl_prefix: u8,
        ) -> IoctlAccessDecision {
            IoctlAccessDecision::ALLOW_ALL
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

    /// A [`Query`] that denies all [`AccessVectors`] and allows all ioctl extended permissions.
    #[derive(Default)]
    struct DenyPermissionsAllowXperms;

    impl Query for DenyPermissionsAllowXperms {
        fn compute_access_decision(
            &self,
            _source_sid: SecurityId,
            _target_sid: SecurityId,
            _target_class: AbstractObjectClass,
        ) -> AccessDecision {
            AccessDecision::allow(AccessVector::NONE)
        }

        fn compute_new_fs_node_sid(
            &self,
            _source_sid: SecurityId,
            _target_sid: SecurityId,
            _fs_node_class: FsNodeClass,
        ) -> Result<SecurityId, anyhow::Error> {
            unreachable!();
        }

        fn compute_new_fs_node_sid_with_name(
            &self,
            _source_sid: SecurityId,
            _target_sid: SecurityId,
            _fs_node_class: FsNodeClass,
            _fs_node_name: NullessByteStr<'_>,
        ) -> Option<SecurityId> {
            unreachable!();
        }

        fn compute_ioctl_access_decision(
            &self,
            _source_sid: SecurityId,
            _target_sid: SecurityId,
            _target_class: AbstractObjectClass,
            _ioctl_prefix: u8,
        ) -> IoctlAccessDecision {
            IoctlAccessDecision::ALLOW_ALL
        }
    }

    impl AccessVectorComputer for DenyPermissionsAllowXperms {
        fn access_vector_from_permissions<
            P: ClassPermission + Into<Permission> + Clone + 'static,
        >(
            &self,
            permissions: &[P],
        ) -> Option<AccessVector> {
            Some(access_vector_from_permissions(permissions))
        }
    }

    /// A [`Query`] that allows all [`AccessVectors`] and denies all ioctl extended permissions.
    #[derive(Default)]
    struct AllowPermissionsDenyXperms;

    impl Query for AllowPermissionsDenyXperms {
        fn compute_access_decision(
            &self,
            _source_sid: SecurityId,
            _target_sid: SecurityId,
            _target_class: AbstractObjectClass,
        ) -> AccessDecision {
            AccessDecision::allow(AccessVector::ALL)
        }

        fn compute_new_fs_node_sid(
            &self,
            _source_sid: SecurityId,
            _target_sid: SecurityId,
            _fs_node_class: FsNodeClass,
        ) -> Result<SecurityId, anyhow::Error> {
            unreachable!();
        }

        fn compute_new_fs_node_sid_with_name(
            &self,
            _source_sid: SecurityId,
            _target_sid: SecurityId,
            _fs_node_class: FsNodeClass,
            _fs_node_name: NullessByteStr<'_>,
        ) -> Option<SecurityId> {
            unreachable!();
        }

        fn compute_ioctl_access_decision(
            &self,
            _source_sid: SecurityId,
            _target_sid: SecurityId,
            _target_class: AbstractObjectClass,
            _ioctl_prefix: u8,
        ) -> IoctlAccessDecision {
            IoctlAccessDecision::DENY_ALL
        }
    }

    impl AccessVectorComputer for AllowPermissionsDenyXperms {
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
                PermissionCheckResult { permit: false, audit: true, todo_bug: None },
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
                PermissionCheckResult { permit: true, audit: false, todo_bug: None },
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

    #[test]
    fn has_ioctl_permission_enforcing() {
        let deny_all: DenyAllPermissions = Default::default();
        let allow_all: AllowAllPermissions = Default::default();
        let deny_perms_allow_xperms: DenyPermissionsAllowXperms = Default::default();
        let allow_perms_deny_xperms: AllowPermissionsDenyXperms = Default::default();
        let permission = CommonFsNodePermission::Ioctl.for_class(FileClass::File);

        // DenyAllPermissions denies.
        assert_eq!(
            PermissionCheckResult { permit: false, audit: true, todo_bug: None },
            has_ioctl_permission(
                /*is_enforcing=*/ true,
                &deny_all,
                &deny_all,
                *A_TEST_SID,
                *A_TEST_SID,
                permission.clone(),
                0xabcd
            )
        );
        // AllowAllPermissions allows.
        assert_eq!(
            PermissionCheckResult { permit: true, audit: false, todo_bug: None },
            has_ioctl_permission(
                /*is_enforcing=*/ true,
                &allow_all,
                &allow_all,
                *A_TEST_SID,
                *A_TEST_SID,
                permission.clone(),
                0xabcd
            )
        );
        // DenyPermissionsAllowXperms denies.
        assert_eq!(
            PermissionCheckResult { permit: false, audit: true, todo_bug: None },
            has_ioctl_permission(
                /*is_enforcing=*/ true,
                &deny_perms_allow_xperms,
                &deny_perms_allow_xperms,
                *A_TEST_SID,
                *A_TEST_SID,
                permission.clone(),
                0xabcd
            )
        );
        // AllowPermissionsDenyXperms denies.
        assert_eq!(
            PermissionCheckResult { permit: false, audit: true, todo_bug: None },
            has_ioctl_permission(
                /*is_enforcing=*/ true,
                &allow_perms_deny_xperms,
                &allow_perms_deny_xperms,
                *A_TEST_SID,
                *A_TEST_SID,
                permission,
                0xabcd
            )
        );
    }

    #[test]
    fn has_ioctl_permission_not_enforcing() {
        let deny_all: DenyAllPermissions = Default::default();
        let permission = CommonFsNodePermission::Ioctl.for_class(FileClass::File);

        // DenyAllPermissions denies, but the permission is allowed when the security server
        // is not in enforcing mode. The decision should still be audited.
        assert_eq!(
            PermissionCheckResult { permit: true, audit: true, todo_bug: None },
            has_ioctl_permission(
                /*is_enforcing=*/ false,
                &deny_all,
                &deny_all,
                *A_TEST_SID,
                *A_TEST_SID,
                permission,
                0xabcd
            )
        );
    }
}
