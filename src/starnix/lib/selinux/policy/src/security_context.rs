// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{index::PolicyIndex, CategoryId, ParseStrategy, RoleId, SensitivityId, TypeId, UserId};

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
    pub(crate) fn new<PS: ParseStrategy>(
        _policy: &PolicyIndex<PS>,
        user: UserId,
        role: RoleId,
        type_: TypeId,
        low_level: SecurityLevel,
        high_level: Option<SecurityLevel>,
    ) -> Result<Self, SecurityContextError> {
        Ok(Self { user, role, type_, low_level, high_level })
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
    pub(crate) fn parse<PS: ParseStrategy>(
        policy_index: &PolicyIndex<PS>,
        security_context: &[u8],
    ) -> Result<Self, SecurityContextError> {
        let as_str = std::str::from_utf8(security_context)
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
            .ok_or_else(|| SecurityContextError::UnknownUser { name: user.to_owned() })?
            .id();
        let role = policy_index
            .parsed_policy()
            .role_by_name(role)
            .ok_or_else(|| SecurityContextError::UnknownRole { name: role.to_owned() })?
            .id();
        let type_ = policy_index
            .parsed_policy()
            .type_by_name(type_)
            .ok_or_else(|| SecurityContextError::UnknownType { name: type_.to_owned() })?
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
    pub(crate) fn serialize<PS: ParseStrategy>(&self, policy_index: &PolicyIndex<PS>) -> Vec<u8> {
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
}

/// Describes a security level, consisting of a sensitivity, and an optional set
/// of associated categories.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SecurityLevel {
    sensitivity: SensitivityId,
    categories: Vec<Category>,
}

impl SecurityLevel {
    pub(crate) fn new(sensitivity: SensitivityId, categories: Vec<Category>) -> Self {
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
            .ok_or_else(|| SecurityContextError::UnknownSensitivity {
                name: sensitivity.to_owned(),
            })?
            .id();
        let mut categories = Vec::new();
        if let Some(categories_str) = categories_item {
            for entry in categories_str.split(",") {
                let category = if let Some((low, high)) = entry.split_once(".") {
                    Category::Range {
                        low: Self::category_id_by_name(policy_index, low)?,
                        high: Self::category_id_by_name(policy_index, high)?,
                    }
                } else {
                    Category::Single(Self::category_id_by_name(policy_index, entry)?)
                };
                categories.push(category);
            }
        }

        Ok(Self { sensitivity, categories })
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
            .ok_or_else(|| SecurityContextError::UnknownCategory { name: name.to_owned() })?
            .id())
    }
}

/// Describes an entry in a category specification, which may be an
/// individual category, or a range.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Category {
    Single(CategoryId),
    Range { low: CategoryId, high: CategoryId },
}

impl Category {
    /// Returns a byte string describing the category, or category range.
    fn serialize<PS: ParseStrategy>(&self, policy_index: &PolicyIndex<PS>) -> Vec<u8> {
        match self {
            Self::Single(category) => {
                policy_index.parsed_policy().category(*category).name_bytes().into()
            }
            Self::Range { low, high } => [
                policy_index.parsed_policy().category(*low).name_bytes(),
                policy_index.parsed_policy().category(*high).name_bytes(),
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
    UnknownSensitivity { name: String },
    #[error("category {name:?} not defined by policy")]
    UnknownCategory { name: String },
    #[error("user {name:?} not defined by policy")]
    UnknownUser { name: String },
    #[error("role {name:?} not defined by policy")]
    UnknownRole { name: String },
    #[error("type {name:?} not defined by policy")]
    UnknownType { name: String },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{parse_policy_by_reference, ByRef, Policy};

    type TestPolicy = Policy<ByRef<&'static [u8]>>;
    fn test_policy() -> TestPolicy {
        const TEST_POLICY: &[u8] =
            include_bytes!("../../testdata/micro_policies/security_context_tests_policy.pp");
        parse_policy_by_reference(TEST_POLICY).unwrap().validate().unwrap()
    }

    #[derive(Debug, Eq, PartialEq)]
    enum CategoryItem<'a> {
        Single(&'a str),
        Range { low: &'a str, high: &'a str },
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

    fn category_item<'a>(policy: &'a TestPolicy, category: &Category) -> CategoryItem<'a> {
        match category {
            Category::Single(id) => CategoryItem::Single(category_name(policy, *id)),
            Category::Range { low, high } => CategoryItem::Range {
                low: category_name(policy, *low),
                high: category_name(policy, *high),
            },
        }
    }

    fn category_items<'a>(
        policy: &'a TestPolicy,
        categories: &Vec<Category>,
    ) -> Vec<CategoryItem<'a>> {
        categories.iter().map(|x| category_item(policy, x)).collect()
    }

    #[test]
    fn parse_security_context_single_sensitivity() {
        let policy = test_policy();
        let security_context = policy
            .parse_security_context(b"user0:object_r:type0:s0")
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
            .parse_security_context(b"user0:object_r:type0:s0-s1")
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
            .parse_security_context(b"user0:object_r:type0:s1:c0.c4")
            .expect("creating security context should succeed");
        assert_eq!(user_name(&policy, security_context.user), "user0");
        assert_eq!(role_name(&policy, security_context.role), "object_r");
        assert_eq!(type_name(&policy, security_context.type_), "type0");
        assert_eq!(sensitivity_name(&policy, security_context.low_level.sensitivity), "s1");
        assert_eq!(
            category_items(&policy, &security_context.low_level.categories),
            [CategoryItem::Range { low: "c0", high: "c4" }]
        );
        assert_eq!(security_context.high_level, None);
    }

    #[test]
    fn parse_security_context_with_sensitivity_range_and_category_interval() {
        let policy = test_policy();
        let security_context = policy
            .parse_security_context(b"user0:object_r:type0:s0-s1:c0.c4")
            .expect("creating security context should succeed");
        assert_eq!(user_name(&policy, security_context.user), "user0");
        assert_eq!(role_name(&policy, security_context.role), "object_r");
        assert_eq!(type_name(&policy, security_context.type_), "type0");
        assert_eq!(sensitivity_name(&policy, security_context.low_level.sensitivity), "s0");
        assert_eq!(security_context.low_level.categories, Vec::new());
        let high_level = security_context.high_level.as_ref().unwrap();
        assert_eq!(sensitivity_name(&policy, high_level.sensitivity), "s1");
        assert_eq!(
            category_items(&policy, &high_level.categories),
            [CategoryItem::Range { low: "c0", high: "c4" }]
        );
    }

    #[test]
    fn parse_security_context_with_sensitivity_range_with_categories() {
        let policy = test_policy();
        let security_context = policy
            .parse_security_context(b"user0:object_r:type0:s0:c0-s1:c0.c4")
            .expect("creating security context should succeed");
        assert_eq!(user_name(&policy, security_context.user), "user0");
        assert_eq!(role_name(&policy, security_context.role), "object_r");
        assert_eq!(type_name(&policy, security_context.type_), "type0");
        assert_eq!(sensitivity_name(&policy, security_context.low_level.sensitivity), "s0");
        assert_eq!(
            category_items(&policy, &security_context.low_level.categories),
            [CategoryItem::Single("c0")]
        );
        let high_level = security_context.high_level.as_ref().unwrap();
        assert_eq!(sensitivity_name(&policy, high_level.sensitivity), "s1");
        assert_eq!(
            category_items(&policy, &high_level.categories),
            [CategoryItem::Range { low: "c0", high: "c4" }]
        );
    }

    #[test]
    fn parse_security_context_with_single_sensitivity_and_category_list() {
        let policy = test_policy();
        let security_context = policy
            .parse_security_context(b"user0:object_r:type0:s1:c0,c4")
            .expect("creating security context should succeed");
        assert_eq!(user_name(&policy, security_context.user), "user0");
        assert_eq!(role_name(&policy, security_context.role), "object_r");
        assert_eq!(type_name(&policy, security_context.type_), "type0");
        assert_eq!(sensitivity_name(&policy, security_context.low_level.sensitivity), "s1");
        assert_eq!(
            category_items(&policy, &security_context.low_level.categories),
            [CategoryItem::Single("c0"), CategoryItem::Single("c4")]
        );
        assert_eq!(security_context.high_level, None);
    }

    #[test]
    fn parse_security_context_with_single_sensitivity_and_category_list_and_range() {
        let policy = test_policy();
        let security_context = policy
            .parse_security_context(b"user0:object_r:type0:s1:c0,c3.c4")
            .expect("creating security context should succeed");
        assert_eq!(user_name(&policy, security_context.user), "user0");
        assert_eq!(role_name(&policy, security_context.role), "object_r");
        assert_eq!(type_name(&policy, security_context.type_), "type0");
        assert_eq!(sensitivity_name(&policy, security_context.low_level.sensitivity), "s1");
        assert_eq!(
            category_items(&policy, &security_context.low_level.categories),
            [CategoryItem::Single("c0"), CategoryItem::Range { low: "c3", high: "c4" }]
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
        ] {
            assert_eq!(
                policy.parse_security_context(invalid_label.as_bytes()),
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
                policy.parse_security_context(invalid_label.as_bytes()),
                Err(SecurityContextError::UnknownSensitivity { name: "s_invalid".to_string() }),
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
                policy.parse_security_context(invalid_label.as_bytes()),
                Err(SecurityContextError::UnknownCategory { name: "c_invalid".to_string() }),
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
        assert!(policy.parse_security_context(b"user0:object_r:type0:s1:c0,c3.c4-s1").is_ok());

        // TODO(b/319232900): Should fail validation because the sensitivity is not
        // valid for the user.

        assert!(policy.parse_security_context(b"user0:object_r:type0:s1").is_ok());

        // TODO(b/319232900): Should fail validation because the role is not valid for
        // user.
        assert!(policy.parse_security_context(b"user0:subject_r:type0:s0").is_ok());
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
                policy.parse_security_context(label.as_bytes()).expect("should succeed");
            assert_eq!(policy.serialize_security_context(&security_context), label.as_bytes());
        }
    }
}
