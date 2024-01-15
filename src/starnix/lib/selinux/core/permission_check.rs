// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::{
    access_vector_cache::{Query, QueryMut},
    AccessVector, ObjectClass, SecurityId,
};

/// Private module for sealed traits with tightly controlled implementations.
mod private {
    /// Public super-trait to seal [`super::PermissionCheck`].
    pub trait PermissionCheck {}

    /// Public super-trait to seal [`super::PermissionCheckMut`].
    pub trait PermissionCheckMut {}
}

/// Extension of [`Query`] that integrates sealed `has_permission()` trait method.
pub trait PermissionCheck: Query + private::PermissionCheck {
    /// Returns true if and only if all `permissions` are granted to `source_sid` acting on
    /// `target_sid` as a `target_class`.
    ///
    /// # Singleton trait implementation
    ///
    /// *Do not provide alternative implementations of this trait.* There must be one consistent
    /// way of computing `has_permission()` in terms of `Query::query()`.
    fn has_permission(
        &self,
        source_sid: SecurityId,
        target_sid: SecurityId,
        target_class: ObjectClass,
        permissions: AccessVector,
    ) -> bool {
        let access_vector = self.query(source_sid, target_sid, target_class);
        permissions & access_vector == permissions
    }
}

/// Every [`Query`] implements [`private::PermissionCheck`].
impl<Q: Query> private::PermissionCheck for Q {}

/// Every [`Query`] implements [`PermissionCheck`] *without overriding `has_permission()`*.
impl<Q: Query> PermissionCheck for Q {}

/// Extension of [`QueryMut`] that integrates sealed `has_permission()` trait method.
pub trait PermissionCheckMut: QueryMut + private::PermissionCheckMut {
    /// Returns true if and only if all `permissions` are granted to `source_sid` acting on
    /// `target_sid` as a `target_class`.
    ///
    /// # Singleton trait implementation
    ///
    /// *Do not provide alternative implementations of this trait.* There must be one consistent
    /// way of computing `has_permission()` in terms of `QueryMut::query()`.
    fn has_permission(
        &mut self,
        source_sid: SecurityId,
        target_sid: SecurityId,
        target_class: ObjectClass,
        permissions: AccessVector,
    ) -> bool {
        let access_vector = self.query(source_sid, target_sid, target_class);
        permissions & access_vector == permissions
    }
}

/// Every [`QueryMut`] implements [`private::PermissionCheckMut`].
impl<QM: QueryMut> private::PermissionCheckMut for QM {}

/// Every [`QueryMut`] implements [`PermissionCheckMut`] *without overriding `has_permission()`*.
impl<QM: QueryMut> PermissionCheckMut for QM {}

#[cfg(test)]
mod tests {
    use super::{super::access_vector_cache::DenyAll, *};

    /// A [`Query`] that permits all [`AccessVector`].
    #[derive(Default)]
    pub struct AllowAll;

    impl Query for AllowAll {
        fn query(
            &self,
            _source_sid: SecurityId,
            _target_sid: SecurityId,
            _target_class: ObjectClass,
        ) -> AccessVector {
            !AccessVector::NONE
        }
    }

    #[fuchsia::test]
    fn has_permission_both() {
        let mut deny_all: DenyAll = Default::default();
        let mut allow_all: AllowAll = Default::default();

        for permissions in
            [AccessVector::READ, AccessVector::WRITE, AccessVector::READ | AccessVector::WRITE]
        {
            // DenyAll denies.
            assert_eq!(
                false,
                (&deny_all).has_permission(
                    0.into(),
                    0.into(),
                    ObjectClass::Process,
                    permissions.clone(),
                )
            );
            assert_eq!(
                false,
                (&mut deny_all).has_permission(
                    0.into(),
                    0.into(),
                    ObjectClass::Process,
                    permissions.clone(),
                )
            );

            // AcceptAll accepts.
            assert_eq!(
                true,
                (&allow_all).has_permission(
                    0.into(),
                    0.into(),
                    ObjectClass::Process,
                    permissions.clone(),
                )
            );
            assert_eq!(
                true,
                (&mut allow_all).has_permission(
                    0.into(),
                    0.into(),
                    ObjectClass::Process,
                    permissions,
                )
            );
        }

        // DenyAll accepts on the empty access vector.
        assert_eq!(
            true,
            (&deny_all).has_permission(
                0.into(),
                0.into(),
                ObjectClass::Process,
                AccessVector::NONE
            )
        );
        assert_eq!(
            true,
            (&allow_all).has_permission(
                0.into(),
                0.into(),
                ObjectClass::Process,
                AccessVector::NONE,
            )
        );

        // AcceptAll accepts on the empty access vector.
        assert_eq!(
            true,
            (&mut deny_all).has_permission(
                0.into(),
                0.into(),
                ObjectClass::Process,
                AccessVector::NONE,
            )
        );
        assert_eq!(
            true,
            (&mut allow_all).has_permission(
                0.into(),
                0.into(),
                ObjectClass::Process,
                AccessVector::NONE,
            )
        );
    }
}
