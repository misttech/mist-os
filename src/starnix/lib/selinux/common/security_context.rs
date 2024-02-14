// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::{fmt, str};

/// The security context, a variable-length string associated with each SELinux object in the
/// system. The security context contains mandatory `user:role:type` components and an optional
/// [:range] component.
///
/// Security contexts are configured by userspace atop Starnix, and mapped to
/// [`SecurityId`]s for internal use in Starnix.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SecurityContext {
    /// The user component of the security context.
    user: String,
    /// The role component of the security context.
    role: String,
    /// The type component of the security context.
    type_: String,
    /// The [lowest] security level of the context.
    low_level: SecurityLevel,
    /// The highest security level, if it allows a range.
    high_level: Option<SecurityLevel>,
}

impl SecurityContext {
    /// Returns the user component of the security context.
    pub fn user(&self) -> &str {
        &self.user
    }

    /// Returns the role component of the security context.
    pub fn role(&self) -> &str {
        &self.role
    }

    /// Returns the type component of the security context.
    pub fn type_(&self) -> &str {
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
}

/// Describes a security level, consisting of a sensitivity, and an optional set
/// of associated categories.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SecurityLevel {
    sensitivity: Sensitivity,
    categories: Vec<Category>,
}

impl TryFrom<&str> for SecurityLevel {
    type Error = SecurityContextParseError;

    fn try_from(level: &str) -> Result<Self, Self::Error> {
        if level.is_empty() {
            return Err(Self::Error::Invalid);
        }
        let mut items = level.split(":");
        let sensitivity = Sensitivity(items.next().ok_or(Self::Error::Invalid)?.to_string());
        let categories = items.next().map_or_else(Vec::new, |s| {
            s.split(",")
                .map(|entry| {
                    if let Some((low, high)) = entry.split_once(".") {
                        Category::Range { low: low.to_string(), high: high.to_string() }
                    } else {
                        Category::Single(entry.to_string())
                    }
                })
                .collect()
        });
        // Level has at most two colon-separated parts, so nothing should remain.
        if items.next().is_some() {
            return Err(Self::Error::Invalid);
        }
        Ok(Self { sensitivity, categories })
    }
}

/// Typed container for a policy-defined sensitivity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Sensitivity(String);

/// Describes an entry in a category specification, which may be an
/// individual category, or a range.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Category {
    Single(String),
    Range { low: String, high: String },
}

/// Errors that may be returned when attempting to parse a security context.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum SecurityContextParseError {
    Invalid,
}

impl TryFrom<&str> for SecurityContext {
    type Error = SecurityContextParseError;

    /// Parses a security context from a `str`.
    ///
    /// Security Contexts take the form:
    ///   context := <user>:<role>:<type>:<levels>
    /// such that they always include user, role, type, and a range of
    /// security levels.
    ///
    /// The security levels part consists of a "low" value and optional "high"
    /// value, defining the range, each optionally associated with a set of
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
    /// [`SecurityContext`] values returned by this function should always be
    /// validated against the active policy, to ensure that they use policy-
    /// defined user, role, type, etc names, and that sensitivity and
    /// category ranges are well-ordered.
    ///
    /// Returns an error if the [`security_context`] is not a syntactically valid
    /// Security Context string.
    fn try_from(security_context: &str) -> Result<Self, Self::Error> {
        let mut items = security_context.splitn(4, ":");
        let user = items.next().ok_or(Self::Error::Invalid)?.to_string();
        let role = items.next().ok_or(Self::Error::Invalid)?.to_string();
        let type_ = items.next().ok_or(Self::Error::Invalid)?.to_string();

        // `next()` holds the remainder of the string, if any.
        let mut levels = items.next().ok_or(Self::Error::Invalid)?.splitn(2, "-");
        let low_level = SecurityLevel::try_from(levels.next().ok_or(Self::Error::Invalid)?)?;
        // `next()` holds the remainder, i.e. the high part, of the range, if any.
        let high_level = levels.next().map(SecurityLevel::try_from).transpose()?;

        Ok(Self { user, role, type_, low_level, high_level })
    }
}

impl TryFrom<Vec<u8>> for SecurityContext {
    type Error = SecurityContextParseError;

    // Parses a security context from a `Vec<u8>`.
    //
    // The supplied vector of octets is required to contain a valid UTF-8
    // encoded string, from which a valid Security Context string can be
    // parsed.
    fn try_from(security_context: Vec<u8>) -> Result<Self, Self::Error> {
        let as_string = String::from_utf8(security_context).map_err(|_| Self::Error::Invalid)?;
        Self::try_from(as_string.as_str())
    }
}

impl fmt::Display for SecurityContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let level_fmt = if let Some(high_level) = &self.high_level {
            format!("{}-{}", self.low_level, high_level)
        } else {
            self.low_level.to_string()
        };
        write!(f, "{}:{}:{}:{}", self.user, self.role, self.type_, level_fmt)
    }
}

impl fmt::Display for SecurityLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let categories_fmt = self.categories.iter().map(|c| c.to_string()).collect::<Vec<_>>();
        if categories_fmt.is_empty() {
            write!(f, "{}", self.sensitivity.0)
        } else {
            write!(f, "{}:{}", self.sensitivity.0, categories_fmt.join(","))
        }
    }
}

impl fmt::Display for Category {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Category::Single(category) => {
                write!(f, "{}", category.to_string())
            }
            Category::Range { low, high } => {
                write!(f, "{}.{}", low.to_string(), high.to_string())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_security_context_single_sensitivity() {
        let security_context = SecurityContext::try_from("u:unconfined_r:unconfined_t:s0")
            .expect("creating security context should succeed");
        assert_eq!(security_context.user, "u");
        assert_eq!(security_context.role, "unconfined_r");
        assert_eq!(security_context.type_, "unconfined_t");
        assert_eq!(security_context.low_level.sensitivity, Sensitivity("s0".to_string()));
        assert_eq!(security_context.high_level, None);
    }

    #[test]
    fn parse_security_context_with_sensitivity_range() {
        let security_context = SecurityContext::try_from("u:unconfined_r:unconfined_t:s0-s1")
            .expect("creating security context should succeed");
        assert_eq!(security_context.user, "u");
        assert_eq!(security_context.role, "unconfined_r");
        assert_eq!(security_context.type_, "unconfined_t");
        assert_eq!(security_context.low_level.sensitivity.0, "s0");
        assert!(security_context.low_level.categories.is_empty());
        let high_level = security_context.high_level.as_ref().unwrap();
        assert_eq!(high_level.sensitivity.0, "s1");
        assert!(high_level.categories.is_empty());
    }

    #[test]
    fn parse_security_context_with_single_sensitivity_and_categories_interval() {
        let security_context = SecurityContext::try_from("u:unconfined_r:unconfined_t:s0:c0.c255")
            .expect("creating security context should succeed");
        assert_eq!(security_context.user, "u");
        assert_eq!(security_context.role, "unconfined_r");
        assert_eq!(security_context.type_, "unconfined_t");
        assert_eq!(security_context.low_level.sensitivity.0, "s0");
        assert_eq!(
            security_context.low_level.categories,
            [Category::Range { low: "c0".to_string(), high: "c255".to_string() }]
        );
    }

    #[test]
    fn parse_security_context_with_sensitivity_range_and_category_interval() {
        let security_context =
            SecurityContext::try_from("u:unconfined_r:unconfined_t:s0-s0:c0.c1023")
                .expect("creating security context should succeed");
        assert_eq!(security_context.user, "u");
        assert_eq!(security_context.role, "unconfined_r");
        assert_eq!(security_context.type_, "unconfined_t");
        assert_eq!(security_context.low_level.sensitivity.0, "s0");
        assert!(security_context.low_level.categories.is_empty());
        let high_level = security_context.high_level.as_ref().unwrap();
        assert_eq!(high_level.sensitivity.0, "s0");
        assert_eq!(
            high_level.categories,
            [Category::Range { low: "c0".to_string(), high: "c1023".to_string() }]
        );
    }

    #[test]
    fn parse_security_context_with_sensitivity_range_with_categories() {
        let security_context =
            SecurityContext::try_from("u:unconfined_r:unconfined_t:s0:c0.c255-s0:c0.c1023")
                .expect("creating security context should succeed");
        assert_eq!(security_context.user, "u");
        assert_eq!(security_context.role, "unconfined_r");
        assert_eq!(security_context.type_, "unconfined_t");
        assert_eq!(security_context.low_level.sensitivity.0, "s0");
        assert_eq!(
            security_context.low_level.categories,
            [Category::Range { low: "c0".to_string(), high: "c255".to_string() }]
        );
        let high_level = security_context.high_level.as_ref().unwrap();
        assert_eq!(high_level.sensitivity.0, "s0");
        assert_eq!(
            high_level.categories,
            [Category::Range { low: "c0".to_string(), high: "c1023".to_string() }]
        );
    }

    #[test]
    fn parse_security_context_with_single_sensitivity_and_category_list() {
        let security_context = SecurityContext::try_from("u:unconfined_r:unconfined_t:s0:c0,c255")
            .expect("creating security context should succeed");
        assert_eq!(security_context.user, "u");
        assert_eq!(security_context.role, "unconfined_r");
        assert_eq!(security_context.type_, "unconfined_t");
        assert_eq!(security_context.low_level.sensitivity.0, "s0");
        assert_eq!(
            security_context.low_level.categories,
            [Category::Single("c0".to_string()), Category::Single("c255".to_string())]
        );
    }

    #[test]
    fn parse_security_context_with_single_sensitivity_and_category_list_and_range() {
        let security_context =
            SecurityContext::try_from("u:unconfined_r:unconfined_t:s0:c0,c200.c255")
                .expect("creating security context should succeed");
        assert_eq!(security_context.user, "u");
        assert_eq!(security_context.role, "unconfined_r");
        assert_eq!(security_context.type_, "unconfined_t");
        assert_eq!(security_context.low_level.sensitivity.0, "s0");
        assert_eq!(
            security_context.low_level.categories,
            [
                Category::Single("c0".to_string()),
                Category::Range { low: "c200".to_string(), high: "c255".to_string() }
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
                SecurityContext::try_from(invalid_label),
                Err(SecurityContextParseError::Invalid),
                "validating {}",
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
            // has categories not associated with the "low" level.
            "u:unconfined_r:unconfined_t:s0:c0,c3.c255-s0",
        ] {
            let security_context = SecurityContext::try_from(label).expect("should succeed");
            assert_eq!(security_context.to_string(), label);
        }
    }
}
