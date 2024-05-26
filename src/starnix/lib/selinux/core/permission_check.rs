// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::{
    access_vector_cache::{Fixed, Locked, Query, QueryMut, DEFAULT_SHARED_SIZE},
    security_server::SecurityServer,
    SecurityId,
};

use selinux_common::{AbstractObjectClass, ClassPermission, Permission};
use selinux_policy::{AccessVector, AccessVectorComputer};
use std::sync::{Arc, Weak};

/// Private module for sealed traits with tightly controlled implementations.
mod private {
    /// Public super-trait to seal [`super::PermissionCheck`].
    pub trait PermissionCheck {}

    /// Public super-trait to seal [`super::PermissionCheckMut`].
    pub trait PermissionCheckMut {}
}

/// Extension of [`Query`] that integrates sealed `has_permission()` trait method.
pub trait PermissionCheck: AccessVectorComputer + Query + private::PermissionCheck {
    /// Returns true if and only if all `permissions` are granted to `source_sid` acting on
    /// `target_sid` as a `target_class`.
    ///
    /// # Singleton trait implementation
    ///
    /// *Do not provide alternative implementations of this trait.* There must be one consistent
    /// way of computing `has_permission()` in terms of `Query::query()`.
    fn has_permissions<P: ClassPermission + Into<Permission> + Clone + 'static>(
        &self,
        source_sid: SecurityId,
        target_sid: SecurityId,
        permissions: &[P],
    ) -> bool {
        let target_class = match permissions.first() {
            Some(permission) => permission.class(),
            None => return true,
        };
        let permissions_access_vector = self.access_vector_from_permissions(permissions);
        let permitted_access_vector = self.query(source_sid, target_sid, target_class.into());
        permissions_access_vector & permitted_access_vector == permissions_access_vector
    }
}

/// Every [`AccessVectorComputer`] + [`Query`] implements [`private::PermissionCheck`].
impl<Q: AccessVectorComputer + Query> private::PermissionCheck for Q {}

/// Every [`AccessVectorComputer`] + [`Query`] implements [`PermissionCheck`] *without overriding
/// associated functions*.
impl<Q: AccessVectorComputer + Query> PermissionCheck for Q {}

/// Extension of [`QueryMut`] that integrates sealed `has_permission()` trait method.
pub trait PermissionCheckMut:
    AccessVectorComputer + QueryMut + private::PermissionCheckMut
{
    /// Returns true if and only if all `permissions` are granted to `source_sid` acting on
    /// `target_sid` as a `target_class`.
    ///
    /// # Singleton trait implementation
    ///
    /// *Do not provide alternative implementations of this trait.* There must be one consistent
    /// way of computing `has_permission()` in terms of `QueryMut::query()`.
    fn has_permissions<P: ClassPermission + Into<Permission> + Clone + 'static>(
        &mut self,
        source_sid: SecurityId,
        target_sid: SecurityId,
        permissions: &[P],
    ) -> bool {
        let target_class = match permissions.first() {
            Some(permission) => permission.class(),
            None => return true,
        };
        let permissions_access_vector = self.access_vector_from_permissions(permissions);
        let permitted_access_vector = self.query(source_sid, target_sid, target_class.into());
        permissions_access_vector & permitted_access_vector == permissions_access_vector
    }
}

/// Every [`QueryMut`] implements [`private::PermissionCheckMut`].
impl<QM: AccessVectorComputer + QueryMut> private::PermissionCheckMut for QM {}

/// Every [`QueryMut`] implements [`PermissionCheckMut`] *without overriding associated functions*.
impl<QM: AccessVectorComputer + QueryMut> PermissionCheckMut for QM {}

pub struct PermissionCheckImpl<'a> {
    security_server: &'a Arc<SecurityServer>,
    access_vector_cache: &'a Locked<Fixed<Weak<SecurityServer>, DEFAULT_SHARED_SIZE>>,
}

impl<'a> PermissionCheckImpl<'a> {
    pub(crate) fn new(
        security_server: &'a Arc<SecurityServer>,
        access_vector_cache: &'a Locked<Fixed<Weak<SecurityServer>, DEFAULT_SHARED_SIZE>>,
    ) -> Self {
        Self { security_server, access_vector_cache }
    }
}

impl<'a> AccessVectorComputer for PermissionCheckImpl<'a> {
    fn access_vector_from_permissions<P: ClassPermission + Into<Permission> + Clone + 'static>(
        &self,
        permissions: &[P],
    ) -> AccessVector {
        self.security_server.access_vector_from_permissions(permissions)
    }
}

impl<'a> Query for PermissionCheckImpl<'a> {
    fn query(
        &self,
        source_sid: SecurityId,
        target_sid: SecurityId,
        target_class: AbstractObjectClass,
    ) -> AccessVector {
        self.access_vector_cache.query(source_sid, target_sid, target_class)
    }
}

#[cfg(test)]
mod tests {
    use super::{super::access_vector_cache::DenyAll, *};

    use once_cell::sync::Lazy;
    use selinux_common::ProcessPermission;
    use selinux_policy::testing::{ACCESS_VECTOR_0001, ACCESS_VECTOR_0010};
    use std::{
        any::Any,
        num::NonZeroU32,
        sync::atomic::{AtomicU32, Ordering},
    };

    /// SID to use where any value will do.
    static A_TEST_SID: Lazy<SecurityId> = Lazy::new(unique_sid);

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
        ) -> AccessVector {
            self.0.query(source_sid, target_sid, target_class)
        }
    }

    impl AccessVectorComputer for DenyAllPermissions {
        fn access_vector_from_permissions<
            P: ClassPermission + Into<Permission> + Clone + 'static,
        >(
            &self,
            permissions: &[P],
        ) -> AccessVector {
            access_vector_from_permissions(permissions)
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
        ) -> AccessVector {
            AccessVector::ALL
        }
    }

    impl AccessVectorComputer for AllowAllPermissions {
        fn access_vector_from_permissions<
            P: ClassPermission + Into<Permission> + Clone + 'static,
        >(
            &self,
            permissions: &[P],
        ) -> AccessVector {
            access_vector_from_permissions(permissions)
        }
    }

    #[fuchsia::test]
    fn has_permission_both() {
        let mut deny_all: DenyAllPermissions = Default::default();
        let mut allow_all: AllowAllPermissions = Default::default();

        // Use permissions that are mapped to access vector bits in
        // `access_vector_from_permission`.
        let permissions = [ProcessPermission::Fork, ProcessPermission::Transition];
        for permission in &permissions {
            // DenyAllPermissions denies.
            assert_eq!(
                false,
                PermissionCheck::has_permissions(
                    &deny_all,
                    *A_TEST_SID,
                    *A_TEST_SID,
                    &[permission.clone()]
                )
            );
            assert_eq!(
                false,
                PermissionCheckMut::has_permissions(
                    &mut deny_all,
                    *A_TEST_SID,
                    *A_TEST_SID,
                    &[permission.clone()]
                )
            );
            // AllowAllPermissions allows.
            assert_eq!(
                true,
                PermissionCheck::has_permissions(
                    &allow_all,
                    *A_TEST_SID,
                    *A_TEST_SID,
                    &[permission.clone()]
                )
            );
            assert_eq!(
                true,
                PermissionCheckMut::has_permissions(
                    &mut allow_all,
                    *A_TEST_SID,
                    *A_TEST_SID,
                    &[permission.clone()]
                )
            );
        }

        // DenyAllPermissions denies.
        assert_eq!(
            false,
            PermissionCheck::has_permissions(&deny_all, *A_TEST_SID, *A_TEST_SID, &permissions)
        );
        assert_eq!(
            false,
            PermissionCheckMut::has_permissions(
                &mut deny_all,
                *A_TEST_SID,
                *A_TEST_SID,
                &permissions
            )
        );

        // AllowAllPermissions allows.
        assert_eq!(
            true,
            PermissionCheck::has_permissions(&allow_all, *A_TEST_SID, *A_TEST_SID, &permissions)
        );
        assert_eq!(
            true,
            PermissionCheckMut::has_permissions(
                &mut allow_all,
                *A_TEST_SID,
                *A_TEST_SID,
                &permissions
            )
        );

        // DenyAllPermissions and AllowAllPermissions vacuously accept empty permissions collection.
        let empty_permissions = [] as [ProcessPermission; 0];
        assert_eq!(
            true,
            PermissionCheck::has_permissions(
                &deny_all,
                *A_TEST_SID,
                *A_TEST_SID,
                &empty_permissions
            )
        );
        assert_eq!(
            true,
            PermissionCheckMut::has_permissions(
                &mut deny_all,
                *A_TEST_SID,
                *A_TEST_SID,
                &empty_permissions
            )
        );
        assert_eq!(
            true,
            PermissionCheck::has_permissions(
                &allow_all,
                *A_TEST_SID,
                *A_TEST_SID,
                &empty_permissions
            )
        );
        assert_eq!(
            true,
            PermissionCheckMut::has_permissions(
                &mut allow_all,
                *A_TEST_SID,
                *A_TEST_SID,
                &empty_permissions
            )
        );
    }
}
