// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::policy::index::PolicyIndex;
use crate::policy::{CategoryId, ParseStrategy, RoleId, SensitivityId, TypeId, UserId};

use crate::NullessByteStr;
use bstr::BString;
use std::cmp::Ordering;
use thiserror::Error;

/// The security context, a variable-length string associated with each SELinux object in the
/// system. The security context contains mandatory `user:role:type` components and an optional
/// [:range] component.
///
/// Security contexts are configured by userspace atop Starnix, and mapped to
/// [`SecurityId`]s for internal use in Starnix.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SecurityContext {
    /// The user component of the security context.
    user: UserId,
    /// The role component of the security context.
    role: RoleId,
    /// The type component of the security context.
    type_: TypeId,
    /// The [lowest] security level of the context.
    low_level: SecurityLevel,
    /// The highest security level, if it allows a range.
    high_level: Option<SecurityLevel>,
}

impl SecurityContext {
    /// Returns a new instance with the specified field values.
    // TODO(b/319232900): Validate that the specified fields are consistent
    // in the context of the supplied policy.
    pub(super) fn new<PS: ParseStrategy>(
        policy: &PolicyIndex<PS>,
        user: UserId,
        role: RoleId,
        type_: TypeId,
        low_level: SecurityLevel,
        high_level: Option<SecurityLevel>,
    ) -> Result<Self, SecurityContextError> {
        let context = Self { user, role, type_, low_level, high_level };
        context.validate(policy)?;
        Ok(context)
    }

    /// Returns the user component of the security context.
    pub fn user(&self) -> UserId {
        self.user
    }

    /// Returns the role component of the security context.
    pub fn role(&self) -> RoleId {
        self.role
    }

    /// Returns the type component of the security context.
    pub fn type_(&self) -> TypeId {
        self.type_
    }

    /// Returns the [lowest] security level of the context.
    pub fn low_level(&self) -> &SecurityLevel {
        &self.low_level
    }

    /// Returns the highest security level, if it allows a range.
    pub fn high_level(&self) -> Option<&SecurityLevel> {
        self.high_level.as_ref()
    }

    /// Returns a `SecurityContext` parsed from `security_context`, against the supplied
    /// `policy`.  The returned structure is guaranteed to be valid for this `policy`.
    ///
    /// Security Contexts in Multi-Level Security (MLS) and Multi-Category Security (MCS)
    /// policies take the form:
    ///   context := <user>:<role>:<type>:<levels>
    /// such that they always include user, role, type, and a range of
    /// security levels.
    ///
    /// The security levels part consists of a "low" value and optional "high"
    /// value, defining the range.  In MCS policies each level may optionally be
    /// associated with a set of categories:
    /// categories:
    ///   levels := <level>[-<level>]
    ///   level := <sensitivity>[:<category_spec>[,<category_spec>]*]
    ///
    /// Entries in the optional list of categories may specify individual
    /// categories, or ranges (from low to high):
    ///   category_spec := <category>[.<category>]
    ///
    /// e.g. "u:r:t:s0" has a single (low) sensitivity.
    /// e.g. "u:r:t:s0-s1" has a sensitivity range.
    /// e.g. "u:r:t:s0:c1,c2,c3" has a single sensitivity, with three categories.
    /// e.g. "u:r:t:s0:c1-s1:c1,c2,c3" has a sensitivity range, with categories
    ///      associated with both low and high ends.
    ///
    /// Returns an error if the [`security_context`] is not a syntactically valid
    /// Security Context string, or the fields are not valid under the current policy.
    pub(super) fn parse<PS: ParseStrategy>(
        policy_index: &PolicyIndex<PS>,
        security_context: NullessByteStr<'_>,
    ) -> Result<Self, SecurityContextError> {
        let as_str = std::str::from_utf8(security_context.as_bytes())
            .map_err(|_| SecurityContextError::InvalidSyntax)?;

        // Parse the user, role, type and security level parts, to validate syntax.
        let mut items = as_str.splitn(4, ":");
        let user = items.next().ok_or(SecurityContextError::InvalidSyntax)?;
        let role = items.next().ok_or(SecurityContextError::InvalidSyntax)?;
        let type_ = items.next().ok_or(SecurityContextError::InvalidSyntax)?;

        // `next()` holds the remainder of the string, if any.
        let mut levels = items.next().ok_or(SecurityContextError::InvalidSyntax)?.split("-");
        let low_level = levels.next().ok_or(SecurityContextError::InvalidSyntax)?;
        if low_level.is_empty() {
            return Err(SecurityContextError::InvalidSyntax);
        }
        let high_level = levels.next();
        if let Some(high_level) = high_level {
            if high_level.is_empty() {
                return Err(SecurityContextError::InvalidSyntax);
            }
        }
        if levels.next() != None {
            return Err(SecurityContextError::InvalidSyntax);
        }

        // Resolve the user, role, type and security levels to identifiers.
        let user = policy_index
            .parsed_policy()
            .user_by_name(user)
            .ok_or_else(|| SecurityContextError::UnknownUser { name: user.into() })?
            .id();
        let role = policy_index
            .parsed_policy()
            .role_by_name(role)
            .ok_or_else(|| SecurityContextError::UnknownRole { name: role.into() })?
            .id();
        let type_ = policy_index
            .parsed_policy()
            .type_by_name(type_)
            .ok_or_else(|| SecurityContextError::UnknownType { name: type_.into() })?
            .id();

        Self::new(
            policy_index,
            user,
            role,
            type_,
            SecurityLevel::parse(policy_index, low_level)?,
            high_level.map(|x| SecurityLevel::parse(policy_index, x)).transpose()?,
        )
    }

    /// Returns this Security Context serialized to a byte string.
    pub(super) fn serialize<PS: ParseStrategy>(&self, policy_index: &PolicyIndex<PS>) -> Vec<u8> {
        let mut levels = self.low_level.serialize(policy_index);
        if let Some(high_level) = &self.high_level {
            levels.push(b'-');
            levels.extend(high_level.serialize(policy_index));
        }
        let parts: [&[u8]; 4] = [
            policy_index.parsed_policy().user(self.user).name_bytes(),
            policy_index.parsed_policy().role(self.role).name_bytes(),
            policy_index.parsed_policy().type_(self.type_).name_bytes(),
            levels.as_slice(),
        ];
        parts.join(b":".as_ref())
    }

    fn validate<PS: ParseStrategy>(
        &self,
        policy_index: &PolicyIndex<PS>,
    ) -> Result<(), SecurityContextError> {
        let user = policy_index.parsed_policy().user(self.user);

        // Validate that the selected role is valid for this user.
        //
        // Roles have meaning for processes, with resources (e.g. files) normally having the
        // well-known "object_r" role.  Validation therefore implicitly allows the "object_r"
        // role, in addition to those defined for the user.
        //
        // TODO(b/335399404): Identifiers are 1-based, while the roles bitmap is 0-based.
        if self.role != policy_index.object_role() && !user.roles().is_set(self.role.0.get() - 1) {
            return Err(SecurityContextError::InvalidRoleForUser {
                role: policy_index.parsed_policy().role(self.role).name_bytes().into(),
                user: user.name_bytes().into(),
            });
        }

        // Validate that the MLS range fits within that defined for the user.
        let valid_low = user.mls_range().low();
        let valid_high = user.mls_range().high().as_ref().unwrap_or(valid_low);

        // 1. Validate that the low sensitivity is within the permitted range.
        if self.low_level.sensitivity < valid_low.sensitivity()
            || self.low_level.sensitivity > valid_high.sensitivity()
        {
            return Err(SecurityContextError::InvalidSensitivityForUser {
                sensitivity: Self::sensitivity_name(policy_index, self.low_level.sensitivity),
                user: user.name_bytes().into(),
            });
        }
        if let Some(high_level) = &self.high_level {
            // 2. Validate that the high sensitivity is within the permitted range.
            if high_level.sensitivity < valid_low.sensitivity()
                || high_level.sensitivity > valid_high.sensitivity()
            {
                return Err(SecurityContextError::InvalidSensitivityForUser {
                    sensitivity: Self::sensitivity_name(policy_index, high_level.sensitivity),
                    user: user.name_bytes().into(),
                });
            }

            // 3. Validate that the high level is not less-than the low level.
            if *high_level < self.low_level {
                return Err(SecurityContextError::InvalidSecurityRange {
                    low: self.low_level.serialize(policy_index).into(),
                    high: high_level.serialize(policy_index).into(),
                });
            }
        }

        // TODO(b/319232900): Determine whether additional validations should be performed here, e.g:
        // - That the categories are valid for the sensitivity levels.

        Ok(())
    }

    fn sensitivity_name<PS: ParseStrategy>(
        policy_index: &PolicyIndex<PS>,
        sensitivity: SensitivityId,
    ) -> BString {
        policy_index.parsed_policy().sensitivity(sensitivity).name_bytes().into()
    }
}

/// Describes a security level, consisting of a sensitivity, and an optional set
/// of associated categories.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SecurityLevel {
    sensitivity: SensitivityId,
    categories: Vec<CategorySpan>,
}

impl SecurityLevel {
    pub(super) fn new(sensitivity: SensitivityId, categories: Vec<CategorySpan>) -> Self {
        Self { sensitivity, categories }
    }

    /// Returns a new instance parsed from the supplied string slice.
    fn parse<PS: ParseStrategy>(
        policy_index: &PolicyIndex<PS>,
        level: &str,
    ) -> Result<Self, SecurityContextError> {
        if level.is_empty() {
            return Err(SecurityContextError::InvalidSyntax);
        }

        // Parse the parts before looking up values, to catch invalid syntax.
        let mut items = level.split(":");
        let sensitivity = items.next().ok_or(SecurityContextError::InvalidSyntax)?;
        let categories_item = items.next();
        if items.next() != None {
            return Err(SecurityContextError::InvalidSyntax);
        }

        // Lookup the sensitivity, and associated categories/ranges, if any.
        let sensitivity = policy_index
            .parsed_policy()
            .sensitivity_by_name(sensitivity)
            .ok_or_else(|| SecurityContextError::UnknownSensitivity { name: sensitivity.into() })?
            .id();
        let mut categories = Vec::new();
        if let Some(categories_str) = categories_item {
            for entry in categories_str.split(",") {
                let category = if let Some((low, high)) = entry.split_once(".") {
                    let low = Self::category_id_by_name(policy_index, low)?;
                    let high = Self::category_id_by_name(policy_index, high)?;
                    if high <= low {
                        return Err(SecurityContextError::InvalidSyntax);
                    }
                    CategorySpan::new(low, high)
                } else {
                    let id = Self::category_id_by_name(policy_index, entry)?;
                    CategorySpan::new(id, id)
                };
                categories.push(category);
            }
        }
        if categories.is_empty() {
            return Ok(Self { sensitivity, categories });
        }
        // Represent the set of category IDs in the following normalized form:
        // - Consecutive IDs are coalesced into spans.
        // - The list of spans is sorted by ID.
        //
        // 1. Sort by lower bound, then upper bound.
        categories.sort_by(|x, y| (x.low, x.high).cmp(&(y.low, y.high)));
        // 2. Merge overlapping and adjacent ranges.
        let categories = categories.into_iter();
        let normalized =
            categories.fold(vec![], |mut normalized: Vec<CategorySpan>, current: CategorySpan| {
                if let Some(last) = normalized.last_mut() {
                    if current.low <= last.high
                        || (u32::from(current.low.0) - u32::from(last.high.0) == 1)
                    {
                        *last = CategorySpan::new(last.low, current.high)
                    } else {
                        normalized.push(current);
                    }
                    return normalized;
                }
                normalized.push(current);
                normalized
            });

        Ok(Self { sensitivity, categories: normalized })
    }

    /// Returns a byte string describing the security level sensitivity and
    /// categories.
    fn serialize<PS: ParseStrategy>(&self, policy_index: &PolicyIndex<PS>) -> Vec<u8> {
        let sensitivity = policy_index.parsed_policy().sensitivity(self.sensitivity).name_bytes();
        let categories = self
            .categories
            .iter()
            .map(|x| x.serialize(policy_index))
            .collect::<Vec<Vec<u8>>>()
            .join(b",".as_ref());

        if categories.is_empty() {
            sensitivity.to_vec()
        } else {
            [sensitivity, categories.as_slice()].join(b":".as_ref())
        }
    }

    fn category_id_by_name<PS: ParseStrategy>(
        policy_index: &PolicyIndex<PS>,
        name: &str,
    ) -> Result<CategoryId, SecurityContextError> {
        Ok(policy_index
            .parsed_policy()
            .category_by_name(name)
            .ok_or_else(|| SecurityContextError::UnknownCategory { name: name.into() })?
            .id())
    }
}

impl PartialOrd for SecurityLevel {
    fn partial_cmp(&self, other: &SecurityLevel) -> Option<Ordering> {
        // TODO(b/319232900): Take category-set ordering into account!
        self.sensitivity.partial_cmp(&other.sensitivity)
    }
}

/// Describes an entry in a category specification, which may be a single category
/// (in which case `low` = `high`) or a span of consecutive categories. The bounds
/// are included in the span.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct CategorySpan {
    low: CategoryId,
    high: CategoryId,
}

impl CategorySpan {
    pub(super) fn new(low: CategoryId, high: CategoryId) -> Self {
        Self { low, high }
    }

    /// Returns a byte string describing the category, or category range.
    fn serialize<PS: ParseStrategy>(&self, policy_index: &PolicyIndex<PS>) -> Vec<u8> {
        match self.low == self.high {
            true => policy_index.parsed_policy().category(self.low).name_bytes().into(),
            false => [
                policy_index.parsed_policy().category(self.low).name_bytes(),
                policy_index.parsed_policy().category(self.high).name_bytes(),
            ]
            .join(b".".as_ref()),
        }
    }
}

/// Errors that may be returned when attempting to parse or validate a security context.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum SecurityContextError {
    #[error("security context syntax is invalid")]
    InvalidSyntax,
    #[error("sensitivity {name:?} not defined by policy")]
    UnknownSensitivity { name: BString },
    #[error("category {name:?} not defined by policy")]
    UnknownCategory { name: BString },
    #[error("user {name:?} not defined by policy")]
    UnknownUser { name: BString },
    #[error("role {name:?} not defined by policy")]
    UnknownRole { name: BString },
    #[error("type {name:?} not defined by policy")]
    UnknownType { name: BString },
    #[error("role {role:?} not valid for {user:?}")]
    InvalidRoleForUser { role: BString, user: BString },
    #[error("sensitivity {sensitivity:?} not valid for {user:?}")]
    InvalidSensitivityForUser { sensitivity: BString, user: BString },
    #[error("high security level {high:?} lower than low level {low:?}")]
    InvalidSecurityRange { low: BString, high: BString },
}

#[cfg(test)]
mod tests {
    use super::super::{parse_policy_by_reference, ByRef, Policy};
    use super::*;

    type TestPolicy = Policy<ByRef<&'static [u8]>>;
    fn test_policy() -> TestPolicy {
        const TEST_POLICY: &[u8] =
            include_bytes!("../../testdata/micro_policies/security_context_tests_policy.pp");
        parse_policy_by_reference(TEST_POLICY).unwrap().validate().unwrap()
    }

    // Represents a `CategorySpan`.
    #[derive(Debug, Eq, PartialEq)]
    struct CategoryItem<'a> {
        low: &'a str,
        high: &'a str,
    }

    fn user_name(policy: &TestPolicy, id: UserId) -> &str {
        std::str::from_utf8(policy.0.parsed_policy().user(id).name_bytes()).unwrap()
    }

    fn role_name(policy: &TestPolicy, id: RoleId) -> &str {
        std::str::from_utf8(policy.0.parsed_policy().role(id).name_bytes()).unwrap()
    }

    fn type_name(policy: &TestPolicy, id: TypeId) -> &str {
        std::str::from_utf8(policy.0.parsed_policy().type_(id).name_bytes()).unwrap()
    }

    fn sensitivity_name(policy: &TestPolicy, id: SensitivityId) -> &str {
        std::str::from_utf8(policy.0.parsed_policy().sensitivity(id).name_bytes()).unwrap()
    }

    fn category_name(policy: &TestPolicy, id: CategoryId) -> &str {
        std::str::from_utf8(policy.0.parsed_policy().category(id).name_bytes()).unwrap()
    }

    fn category_span<'a>(policy: &'a TestPolicy, category: &CategorySpan) -> CategoryItem<'a> {
        CategoryItem {
            low: category_name(policy, category.low),
            high: category_name(policy, category.high),
        }
    }

    fn category_spans<'a>(
        policy: &'a TestPolicy,
        categories: &Vec<CategorySpan>,
    ) -> Vec<CategoryItem<'a>> {
        categories.iter().map(|x| category_span(policy, x)).collect()
    }

    #[test]
    fn parse_security_context_single_sensitivity() {
        let policy = test_policy();
        let security_context = policy
            .parse_security_context(b"user0:object_r:type0:s0".into())
            .expect("creating security context should succeed");
        assert_eq!(user_name(&policy, security_context.user), "user0");
        assert_eq!(role_name(&policy, security_context.role), "object_r");
        assert_eq!(type_name(&policy, security_context.type_), "type0");
        assert_eq!(sensitivity_name(&policy, security_context.low_level.sensitivity), "s0");
        assert_eq!(security_context.low_level.categories, Vec::new());
        assert_eq!(security_context.high_level, None);
    }

    #[test]
    fn parse_security_context_with_sensitivity_range() {
        let policy = test_policy();
        let security_context = policy
            .parse_security_context(b"user0:object_r:type0:s0-s1".into())
            .expect("creating security context should succeed");
        assert_eq!(user_name(&policy, security_context.user), "user0");
        assert_eq!(role_name(&policy, security_context.role), "object_r");
        assert_eq!(type_name(&policy, security_context.type_), "type0");
        assert_eq!(sensitivity_name(&policy, security_context.low_level.sensitivity), "s0");
        assert_eq!(security_context.low_level.categories, Vec::new());
        let high_level = security_context.high_level.as_ref().unwrap();
        assert_eq!(sensitivity_name(&policy, high_level.sensitivity), "s1");
        assert_eq!(high_level.categories, Vec::new());
    }

    #[test]
    fn parse_security_context_with_single_sensitivity_and_categories_interval() {
        let policy = test_policy();
        let security_context = policy
            .parse_security_context(b"user0:object_r:type0:s1:c0.c4".into())
            .expect("creating security context should succeed");
        assert_eq!(user_name(&policy, security_context.user), "user0");
        assert_eq!(role_name(&policy, security_context.role), "object_r");
        assert_eq!(type_name(&policy, security_context.type_), "type0");
        assert_eq!(sensitivity_name(&policy, security_context.low_level.sensitivity), "s1");
        assert_eq!(
            category_spans(&policy, &security_context.low_level.categories),
            [CategoryItem { low: "c0", high: "c4" }]
        );
        assert_eq!(security_context.high_level, None);
    }

    #[test]
    fn parse_security_context_and_normalize_categories() {
        let policy = &test_policy();
        let normalize = {
            |security_context: &str| -> String {
                String::from_utf8(
                    policy.serialize_security_context(
                        &policy
                            .parse_security_context(security_context.into())
                            .expect("creating security context should succeed"),
                    ),
                )
                .unwrap()
            }
        };
        // Overlapping category ranges are merged.
        assert_eq!(normalize("user0:object_r:type0:s1:c0.c1,c1"), "user0:object_r:type0:s1:c0.c1");
        assert_eq!(
            normalize("user0:object_r:type0:s1:c0.c2,c1.c2"),
            "user0:object_r:type0:s1:c0.c2"
        );
        assert_eq!(
            normalize("user0:object_r:type0:s1:c0.c2,c1.c3"),
            "user0:object_r:type0:s1:c0.c3"
        );
        // Adjacent category ranges are merged.
        assert_eq!(normalize("user0:object_r:type0:s1:c0.c1,c2"), "user0:object_r:type0:s1:c0.c2");
        // Category ranges are ordered by first element.
        assert_eq!(
            normalize("user0:object_r:type0:s1:c2.c3,c0"),
            "user0:object_r:type0:s1:c0,c2.c3"
        );
    }

    #[test]
    fn parse_security_context_with_sensitivity_range_and_category_interval() {
        let policy = test_policy();
        let security_context = policy
            .parse_security_context(b"user0:object_r:type0:s0-s1:c0.c4".into())
            .expect("creating security context should succeed");
        assert_eq!(user_name(&policy, security_context.user), "user0");
        assert_eq!(role_name(&policy, security_context.role), "object_r");
        assert_eq!(type_name(&policy, security_context.type_), "type0");
        assert_eq!(sensitivity_name(&policy, security_context.low_level.sensitivity), "s0");
        assert_eq!(security_context.low_level.categories, Vec::new());
        let high_level = security_context.high_level.as_ref().unwrap();
        assert_eq!(sensitivity_name(&policy, high_level.sensitivity), "s1");
        assert_eq!(
            category_spans(&policy, &high_level.categories),
            [CategoryItem { low: "c0", high: "c4" }]
        );
    }

    #[test]
    fn parse_security_context_with_sensitivity_range_with_categories() {
        let policy = test_policy();
        let security_context = policy
            .parse_security_context(b"user0:object_r:type0:s0:c0-s1:c0.c4".into())
            .expect("creating security context should succeed");
        assert_eq!(user_name(&policy, security_context.user), "user0");
        assert_eq!(role_name(&policy, security_context.role), "object_r");
        assert_eq!(type_name(&policy, security_context.type_), "type0");
        assert_eq!(sensitivity_name(&policy, security_context.low_level.sensitivity), "s0");
        assert_eq!(
            category_spans(&policy, &security_context.low_level.categories),
            [CategoryItem { low: "c0", high: "c0" }]
        );

        let high_level = security_context.high_level.as_ref().unwrap();
        assert_eq!(sensitivity_name(&policy, high_level.sensitivity), "s1");
        assert_eq!(
            category_spans(&policy, &high_level.categories),
            [CategoryItem { low: "c0", high: "c4" }]
        );
    }

    #[test]
    fn parse_security_context_with_single_sensitivity_and_category_list() {
        let policy = test_policy();
        let security_context = policy
            .parse_security_context(b"user0:object_r:type0:s1:c0,c4".into())
            .expect("creating security context should succeed");
        assert_eq!(user_name(&policy, security_context.user), "user0");
        assert_eq!(role_name(&policy, security_context.role), "object_r");
        assert_eq!(type_name(&policy, security_context.type_), "type0");
        assert_eq!(sensitivity_name(&policy, security_context.low_level.sensitivity), "s1");
        assert_eq!(
            category_spans(&policy, &security_context.low_level.categories),
            [CategoryItem { low: "c0", high: "c0" }, CategoryItem { low: "c4", high: "c4" }]
        );
        assert_eq!(security_context.high_level, None);
    }

    #[test]
    fn parse_security_context_with_single_sensitivity_and_category_list_and_range() {
        let policy = test_policy();
        let security_context = policy
            .parse_security_context(b"user0:object_r:type0:s1:c0,c3.c4".into())
            .expect("creating security context should succeed");
        assert_eq!(user_name(&policy, security_context.user), "user0");
        assert_eq!(role_name(&policy, security_context.role), "object_r");
        assert_eq!(type_name(&policy, security_context.type_), "type0");
        assert_eq!(sensitivity_name(&policy, security_context.low_level.sensitivity), "s1");
        assert_eq!(
            category_spans(&policy, &security_context.low_level.categories),
            [CategoryItem { low: "c0", high: "c0" }, CategoryItem { low: "c3", high: "c4" }]
        );
        assert_eq!(security_context.high_level, None);
    }

    #[test]
    fn parse_invalid_syntax() {
        let policy = test_policy();
        for invalid_label in [
            "user0",
            "user0:object_r",
            "user0:object_r:type0",
            "user0:object_r:type0:s0-",
            "user0:object_r:type0:s0:s0:s0",
            "user0:object_r:type0:s0:c0.c0", // Category upper bound is equal to lower bound.
            "user0:object_r:type0:s0:c1.c0", // Category upper bound is less than lower bound.
        ] {
            assert_eq!(
                policy.parse_security_context(invalid_label.as_bytes().into()),
                Err(SecurityContextError::InvalidSyntax),
                "validating {:?}",
                invalid_label
            );
        }
    }

    #[test]
    fn parse_invalid_sensitivity() {
        let policy = test_policy();
        for invalid_label in ["user0:object_r:type0:s_invalid", "user0:object_r:type0:s0-s_invalid"]
        {
            assert_eq!(
                policy.parse_security_context(invalid_label.as_bytes().into()),
                Err(SecurityContextError::UnknownSensitivity { name: "s_invalid".into() }),
                "validating {:?}",
                invalid_label
            );
        }
    }

    #[test]
    fn parse_invalid_category() {
        let policy = test_policy();
        for invalid_label in
            ["user0:object_r:type0:s1:c_invalid", "user0:object_r:type0:s1:c0.c_invalid"]
        {
            assert_eq!(
                policy.parse_security_context(invalid_label.as_bytes().into()),
                Err(SecurityContextError::UnknownCategory { name: "c_invalid".into() }),
                "validating {:?}",
                invalid_label
            );
        }
    }

    #[test]
    fn invalid_security_context_fields() {
        let policy = test_policy();

        // TODO(b/319232900): Should fail validation because the low security level has
        // categories that the high level does not.
        assert!(policy
            .parse_security_context(b"user0:object_r:type0:s1:c0,c3.c4-s1".into())
            .is_ok());

        // Fails validation because the sensitivity is not valid for the user.
        assert!(policy.parse_security_context(b"user1:object_r:type0:s0".into()).is_err());

        // Fails validation because the role is not valid for the user.
        assert!(policy.parse_security_context(b"user0:subject_r:type0:s0".into()).is_err());

        // Passes validation even though the role is not explicitly allowed for the user,
        // because it is the special "object_r" role, used when labelling resources.
        assert!(policy.parse_security_context(b"user1:object_r:type0:s1".into()).is_ok());
    }

    #[test]
    fn format_security_contexts() {
        let policy = test_policy();
        for label in [
            "user0:object_r:type0:s0",
            "user0:object_r:type0:s0-s1",
            "user0:object_r:type0:s1:c0.c4",
            "user0:object_r:type0:s0-s1:c0.c4",
            "user0:object_r:type0:s1:c0,c3",
            "user0:object_r:type0:s0-s1:c0,c2,c4",
            "user0:object_r:type0:s1:c0,c3.c4-s1:c0,c2.c4",
        ] {
            let security_context =
                policy.parse_security_context(label.as_bytes().into()).expect("should succeed");
            assert_eq!(policy.serialize_security_context(&security_context), label.as_bytes());
        }
    }
}
