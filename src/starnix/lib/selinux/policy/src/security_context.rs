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
    /// No validation of the supplied values is performed. Contexts are
    /// typically validated against the loaded policy by the Security Server,
    /// e.g. when exchanging them for a Security Id.
    pub(crate) fn new(
        user: UserId,
        role: RoleId,
        type_: TypeId,
        low_level: SecurityLevel,
        high_level: Option<SecurityLevel>,
    ) -> Self {
        Self { user, role, type_, low_level, high_level }
    }

    /// Returns the user component of the security context.
    pub fn user(&self) -> &UserId {
        &self.user
    }

    /// Returns the role component of the security context.
    pub fn role(&self) -> &RoleId {
        &self.role
    }

    /// Returns the type component of the security context.
    pub fn type_(&self) -> &TypeId {
        &self.type_
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
    ) -> Result<Self, SecurityContextParseError> {
        let as_str = std::str::from_utf8(security_context)
            .map_err(|_| SecurityContextParseError::Invalid)?;

        // TODO(): Revise this to (1) parse (2) map names to Ids and (3) validate the combination of fields,
        // and sensitivity & category ordering.
        let mut items = as_str.splitn(4, ":");
        let user = UserId(items.next().ok_or(SecurityContextParseError::Invalid)?.to_string());
        let role = RoleId(items.next().ok_or(SecurityContextParseError::Invalid)?.to_string());
        let type_ = TypeId(items.next().ok_or(SecurityContextParseError::Invalid)?.to_string());

        // `next()` holds the remainder of the string, if any.
        let mut levels = items.next().ok_or(SecurityContextParseError::Invalid)?.splitn(2, "-");
        let low_level = SecurityLevel::parse(
            policy_index,
            levels.next().ok_or(SecurityContextParseError::Invalid)?,
        )?;

        // `next()` holds the remainder, i.e. the high part, of the range, if any.
        let high_level =
            levels.next().map(|x| SecurityLevel::parse(policy_index, x)).transpose()?;

        // TODO(): Validate fields against the policy.
        Ok(Self::new(user, role, type_, low_level, high_level))
    }

    /// Returns this Security Context serialized to a byte string.
    pub(crate) fn serialize<PS: ParseStrategy>(&self, policy_index: &PolicyIndex<PS>) -> Vec<u8> {
        let mut levels = self.low_level.serialize(policy_index);
        if let Some(high_level) = &self.high_level {
            levels.push(b'-');
            levels.extend(high_level.serialize(policy_index));
        }
        let parts: [&[u8]; 4] = [
            self.user.0.as_bytes(),
            self.role.0.as_bytes(),
            self.type_.0.as_bytes(),
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
        _policy_index: &PolicyIndex<PS>,
        level: &str,
    ) -> Result<Self, SecurityContextParseError> {
        if level.is_empty() {
            return Err(SecurityContextParseError::Invalid);
        }
        let mut items = level.split(":");
        let sensitivity =
            SensitivityId(items.next().ok_or(SecurityContextParseError::Invalid)?.to_string());
        let categories = items.next().map_or_else(Vec::new, |s| {
            s.split(",")
                .map(|entry| {
                    if let Some((low, high)) = entry.split_once(".") {
                        Category::Range {
                            low: CategoryId(low.to_string()),
                            high: CategoryId(high.to_string()),
                        }
                    } else {
                        Category::Single(CategoryId(entry.to_string()))
                    }
                })
                .collect()
        });
        // Level has at most two colon-separated parts, so nothing should remain.
        if items.next().is_some() {
            return Err(SecurityContextParseError::Invalid);
        }
        Ok(Self { sensitivity, categories })
    }

    /// Returns a byte string describing the security level sensitivity and
    /// categories.
    fn serialize<PS: ParseStrategy>(&self, policy_index: &PolicyIndex<PS>) -> Vec<u8> {
        let categories = self
            .categories
            .iter()
            .map(|x| x.serialize(policy_index))
            .collect::<Vec<Vec<u8>>>()
            .join(b",".as_ref());
        if categories.is_empty() {
            self.sensitivity.0.clone().into()
        } else {
            [self.sensitivity.0.as_bytes(), categories.as_slice()].join(b":".as_ref())
        }
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
    fn serialize<PS: ParseStrategy>(&self, _policy_index: &PolicyIndex<PS>) -> Vec<u8> {
        match self {
            Self::Single(category) => category.0.clone().into(),
            Self::Range { low, high } => [low.0.as_bytes(), high.0.as_bytes()].join(b".".as_ref()),
        }
    }
}

/// Errors that may be returned when attempting to parse a security context.
#[derive(Copy, Clone, Debug, Error, Eq, PartialEq)]
pub enum SecurityContextParseError {
    #[error("security context is invalid")]
    Invalid,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{parse_policy_by_reference, ByRef, Policy};

    fn test_policy() -> Policy<ByRef<&'static [u8]>> {
        const TEST_POLICY: &[u8] = include_bytes!("../../testdata/policies/emulator");
        parse_policy_by_reference(TEST_POLICY).unwrap().validate().unwrap()
    }

    #[test]
    fn parse_security_context_single_sensitivity() {
        let security_context = test_policy()
            .parse_security_context(b"u:unconfined_r:unconfined_t:s0")
            .expect("creating security context should succeed");
        assert_eq!(security_context.user.0, "u");
        assert_eq!(security_context.role.0, "unconfined_r");
        assert_eq!(security_context.type_.0, "unconfined_t");
        assert_eq!(security_context.low_level.sensitivity, SensitivityId("s0".to_string()));
        assert_eq!(security_context.high_level, None);
    }

    #[test]
    fn parse_security_context_with_sensitivity_range() {
        let security_context = test_policy()
            .parse_security_context(b"u:unconfined_r:unconfined_t:s0-s1")
            .expect("creating security context should succeed");
        assert_eq!(security_context.user.0, "u");
        assert_eq!(security_context.role.0, "unconfined_r");
        assert_eq!(security_context.type_.0, "unconfined_t");
        assert_eq!(security_context.low_level.sensitivity.0, "s0");
        assert!(security_context.low_level.categories.is_empty());
        let high_level = security_context.high_level.as_ref().unwrap();
        assert_eq!(high_level.sensitivity.0, "s1");
        assert!(high_level.categories.is_empty());
    }

    #[test]
    fn parse_security_context_with_single_sensitivity_and_categories_interval() {
        let security_context = test_policy()
            .parse_security_context(b"u:unconfined_r:unconfined_t:s0:c0.c255")
            .expect("creating security context should succeed");
        assert_eq!(security_context.user.0, "u");
        assert_eq!(security_context.role.0, "unconfined_r");
        assert_eq!(security_context.type_.0, "unconfined_t");
        assert_eq!(security_context.low_level.sensitivity.0, "s0");
        assert_eq!(
            security_context.low_level.categories,
            [Category::Range {
                low: CategoryId("c0".to_string()),
                high: CategoryId("c255".to_string())
            }]
        );
    }

    #[test]
    fn parse_security_context_with_sensitivity_range_and_category_interval() {
        let security_context = test_policy()
            .parse_security_context(b"u:unconfined_r:unconfined_t:s0-s0:c0.c1023")
            .expect("creating security context should succeed");
        assert_eq!(security_context.user.0, "u");
        assert_eq!(security_context.role.0, "unconfined_r");
        assert_eq!(security_context.type_.0, "unconfined_t");
        assert_eq!(security_context.low_level.sensitivity.0, "s0");
        assert!(security_context.low_level.categories.is_empty());
        let high_level = security_context.high_level.as_ref().unwrap();
        assert_eq!(high_level.sensitivity.0, "s0");
        assert_eq!(
            high_level.categories,
            [Category::Range {
                low: CategoryId("c0".to_string()),
                high: CategoryId("c1023".to_string())
            }]
        );
    }

    #[test]
    fn parse_security_context_with_sensitivity_range_with_categories() {
        let security_context = test_policy()
            .parse_security_context(b"u:unconfined_r:unconfined_t:s0:c0.c255-s0:c0.c1023")
            .expect("creating security context should succeed");
        assert_eq!(security_context.user.0, "u");
        assert_eq!(security_context.role.0, "unconfined_r");
        assert_eq!(security_context.type_.0, "unconfined_t");
        assert_eq!(security_context.low_level.sensitivity.0, "s0");
        assert_eq!(
            security_context.low_level.categories,
            [Category::Range {
                low: CategoryId("c0".to_string()),
                high: CategoryId("c255".to_string())
            }]
        );
        let high_level = security_context.high_level.as_ref().unwrap();
        assert_eq!(high_level.sensitivity.0, "s0");
        assert_eq!(
            high_level.categories,
            [Category::Range {
                low: CategoryId("c0".to_string()),
                high: CategoryId("c1023".to_string())
            }]
        );
    }

    #[test]
    fn parse_security_context_with_single_sensitivity_and_category_list() {
        let security_context = test_policy()
            .parse_security_context(b"u:unconfined_r:unconfined_t:s0:c0,c255")
            .expect("creating security context should succeed");
        assert_eq!(security_context.user.0, "u");
        assert_eq!(security_context.role.0, "unconfined_r");
        assert_eq!(security_context.type_.0, "unconfined_t");
        assert_eq!(security_context.low_level.sensitivity.0, "s0");
        assert_eq!(
            security_context.low_level.categories,
            [
                Category::Single(CategoryId("c0".to_string())),
                Category::Single(CategoryId("c255".to_string()))
            ]
        );
    }

    #[test]
    fn parse_security_context_with_single_sensitivity_and_category_list_and_range() {
        let security_context = test_policy()
            .parse_security_context(b"u:unconfined_r:unconfined_t:s0:c0,c200.c255")
            .expect("creating security context should succeed");
        assert_eq!(security_context.user.0, "u");
        assert_eq!(security_context.role.0, "unconfined_r");
        assert_eq!(security_context.type_.0, "unconfined_t");
        assert_eq!(security_context.low_level.sensitivity.0, "s0");
        assert_eq!(
            security_context.low_level.categories,
            [
                Category::Single(CategoryId("c0".to_string())),
                Category::Range {
                    low: CategoryId("c200".to_string()),
                    high: CategoryId("c255".to_string())
                }
            ]
        );
    }

    #[test]
    fn parse_invalid_security_contexts() {
        for invalid_label in [
            "u",
            "u:unconfined_r",
            "u:unconfined_r:unconfined_t",
            "u:unconfined_r:unconfined_t:s0-",
            "u:unconfined_r:unconfined_t:s0:s0:s0",
        ] {
            assert_eq!(
                test_policy().parse_security_context(invalid_label.as_bytes()),
                Err(SecurityContextParseError::Invalid),
                "validating {:?}",
                invalid_label
            );
        }
    }

    #[test]
    fn format_security_contexts() {
        for label in [
            "u:unconfined_r:unconfined_t:s0",
            "u:unconfined_r:unconfined_t:s0-s0",
            "u:unconfined_r:unconfined_t:s0:c0.c255",
            "u:unconfined_r:unconfined_t:s0-s0:c0.c1023",
            "u:unconfined_r:unconfined_t:s0:c0,c255",
            "u:unconfined_r:unconfined_t:s0-s0:c0,c3,c7",
            "u:unconfined_r:unconfined_t:s0:c0,c3.c255-s0:c0,c3.255,c1024",
            // The following is not a valid Security Context, since the "low" security level
            // has categories not associated with the "high" level.
            "u:unconfined_r:unconfined_t:s0:c0,c3.c255-s0",
        ] {
            let security_context =
                test_policy().parse_security_context(label.as_bytes()).expect("should succeed");
            assert_eq!(
                test_policy().serialize_security_context(&security_context),
                label.as_bytes()
            );
        }
    }
}
