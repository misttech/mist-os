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
    /// The MLS range component of the security context.
    range: Option<MlsRange>,
}

/// The MLS range, containing a mandatory sensitivity component and an optional category component.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MlsRange {
    sensitivity: Sensitivity,
    category: Option<Category>,
}

/// The value that can be used as components in an MLS range.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RangeValue(String);

impl From<&str> for RangeValue {
    fn from(value: &str) -> Self {
        Self(value.to_string())
    }
}

/// The sensitivity component of an MLS range, with a mandatory low value and an optional high value.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Sensitivity {
    low: RangeValue,
    high: Option<RangeValue>,
}

/// The MLS range category, that can either be a single value, an interval or a list of values.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Category {
    Single(RangeValue),
    Interval { low: RangeValue, high: RangeValue },
    List(Vec<RangeValue>),
}

/// Errors that may be returned when attempting to parse a security context.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum SecurityContextParseError {
    Invalid,
}

impl TryFrom<&str> for SecurityContext {
    type Error = SecurityContextParseError;

    // Parses a security context from a `str`.
    //
    // A valid security context contains mandatory `user:role:type` components, and an optional
    // `[:range]` component.
    // The MLS range contains a mandatory `sensitivity` component and an optional `[:category]`
    // component.
    // The MLS sensitity contains a mandatory `low` component and an optional `[-high]` component.
    // E.g. `s0`, `s0 - s2`, `s0-s5`.
    // The MLS category contains either a single `category` value, a `low.high` interval, or a
    // list of comma separated values (e.g. `c0,c1,c2`).
    //
    // Returns an error if the string is not a valid security context.
    fn try_from(security_context: &str) -> Result<Self, Self::Error> {
        let mut items = security_context.split(":");
        let user = items.next().ok_or(Self::Error::Invalid)?.to_string();
        let role = items.next().ok_or(Self::Error::Invalid)?.to_string();
        let type_ = items.next().ok_or(Self::Error::Invalid)?.to_string();
        let range = items.next().map_or(Ok(None), |s| {
            // Sensitivities can either be a single value or a range separated by '-'.
            let mut sensitivities = s.split("-").map(str::trim);
            let low = RangeValue::from(sensitivities.next().ok_or(Self::Error::Invalid)?);
            let high = sensitivities.next().map(RangeValue::from);
            let sensitivity = Sensitivity { low, high };
            if sensitivities.next().is_some() {
                return Err(Self::Error::Invalid);
            }

            let category = items.next().map_or(Ok(None), |s| {
                // Categories can either be a single value, a range separated by '.' or a list separated by ','.
                if s.contains(".") {
                    let mut categories = s.split(".");
                    let low = RangeValue::from(categories.next().ok_or(Self::Error::Invalid)?);
                    let high = RangeValue::from(categories.next().ok_or(Self::Error::Invalid)?);
                    if categories.next().is_some() {
                        return Err(Self::Error::Invalid);
                    }
                    Ok(Some(Category::Interval { low, high }))
                } else if s.contains(",") {
                    let categories = s.split(",").map(RangeValue::from).collect();
                    Ok(Some(Category::List(categories)))
                } else {
                    Ok(Some(Category::Single(s.into())))
                }
            })?;
            Ok(Some(MlsRange { sensitivity, category }))
        })?;

        Ok(Self { user, role, type_, range })
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
        let range_fmt =
            self.range.as_ref().map_or("".to_owned(), |r| format!(":{}", r.to_string()));
        write!(f, "{}:{}:{}{}", self.user, self.role, self.type_, range_fmt)
    }
}

impl fmt::Display for MlsRange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let category_fmt =
            self.category.as_ref().map_or("".to_owned(), |c| format!(":{}", c.to_string()));
        write!(f, "{}{}", self.sensitivity, category_fmt)
    }
}

impl fmt::Display for Sensitivity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let high_fmt =
            self.high.as_ref().map_or("".to_owned(), |rv| format!("-{}", rv.to_string()));
        write!(f, "{}{}", self.low, high_fmt)
    }
}

impl fmt::Display for Category {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Category::Single(category) => {
                write!(f, "{}", category.to_string())
            }
            Category::Interval { low, high } => {
                write!(f, "{}.{}", low.to_string(), high.to_string())
            }
            Category::List(list) => {
                let category = list.into_iter().map(|c| c.0.as_str()).collect::<Vec<_>>().join(",");
                write!(f, "{}", category)
            }
        }
    }
}

impl fmt::Display for RangeValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[fuchsia::test]
    fn parse_security_context() {
        let security_context = SecurityContext::try_from("u:unconfined_r:unconfined_t")
            .expect("creating security context should succeed");
        assert_eq!(security_context.user, "u");
        assert_eq!(security_context.role, "unconfined_r");
        assert_eq!(security_context.type_, "unconfined_t");
        assert_eq!(security_context.range, None);
    }

    #[fuchsia::test]
    fn parse_security_context_with_sensitivity_value() {
        let security_context = SecurityContext::try_from("u:unconfined_r:unconfined_t:s0")
            .expect("creating security context should succeed");
        assert_eq!(security_context.user, "u");
        assert_eq!(security_context.role, "unconfined_r");
        assert_eq!(security_context.type_, "unconfined_t");
        assert_eq!(
            security_context.range,
            Some(MlsRange {
                sensitivity: Sensitivity { low: RangeValue("s0".to_string()), high: None },
                category: None
            })
        );
    }

    #[fuchsia::test]
    fn parse_security_context_with_sensitivity_range() {
        for label in ["u:unconfined_r:unconfined_t:s0 - s0", "u:unconfined_r:unconfined_t:s0-s0"] {
            let security_context =
                SecurityContext::try_from(label).expect("creating security context should succeed");
            assert_eq!(security_context.user, "u");
            assert_eq!(security_context.role, "unconfined_r");
            assert_eq!(security_context.type_, "unconfined_t");
            assert_eq!(
                security_context.range,
                Some(MlsRange {
                    sensitivity: Sensitivity {
                        low: RangeValue("s0".to_string()),
                        high: Some(RangeValue("s0".to_string()))
                    },
                    category: None
                })
            );
        }
    }

    #[fuchsia::test]
    fn parse_security_context_with_sensitivity_value_and_category_interval() {
        let security_context = SecurityContext::try_from("u:unconfined_r:unconfined_t:s0:c0.c255")
            .expect("creating security context should succeed");
        assert_eq!(security_context.user, "u");
        assert_eq!(security_context.role, "unconfined_r");
        assert_eq!(security_context.type_, "unconfined_t");
        assert_eq!(
            security_context.range,
            Some(MlsRange {
                sensitivity: Sensitivity { low: RangeValue("s0".to_string()), high: None },
                category: Some(Category::Interval {
                    low: RangeValue("c0".to_string()),
                    high: RangeValue("c255".to_string())
                })
            })
        );
    }

    #[fuchsia::test]
    fn parse_security_context_with_sensitivity_range_and_category_interval() {
        let security_context =
            SecurityContext::try_from("u:unconfined_r:unconfined_t:s0 - s0:c0.c1023")
                .expect("creating security context should succeed");
        assert_eq!(security_context.user, "u");
        assert_eq!(security_context.role, "unconfined_r");
        assert_eq!(security_context.type_, "unconfined_t");
        assert_eq!(
            security_context.range,
            Some(MlsRange {
                sensitivity: Sensitivity {
                    low: RangeValue("s0".to_string()),
                    high: Some(RangeValue("s0".to_string()))
                },
                category: Some(Category::Interval {
                    low: RangeValue("c0".to_string()),
                    high: RangeValue("c1023".to_string())
                })
            })
        );
    }

    #[fuchsia::test]
    fn parse_security_context_with_sensitivity_value_and_category_list() {
        let security_context = SecurityContext::try_from("u:unconfined_r:unconfined_t:s0:c0,c255")
            .expect("creating security context should succeed");
        assert_eq!(security_context.user, "u");
        assert_eq!(security_context.role, "unconfined_r");
        assert_eq!(security_context.type_, "unconfined_t");
        assert_eq!(
            security_context.range,
            Some(MlsRange {
                sensitivity: Sensitivity { low: RangeValue("s0".to_string()), high: None },
                category: Some(Category::List(vec![
                    RangeValue("c0".to_string()),
                    RangeValue("c255".to_string())
                ]))
            })
        );
    }

    #[fuchsia::test]
    fn parse_security_context_with_sensitivity_range_and_category_list() {
        let security_context =
            SecurityContext::try_from("u:unconfined_r:unconfined_t:s0-s0:c0,c3,c7")
                .expect("creating security context should succeed");
        assert_eq!(security_context.user, "u");
        assert_eq!(security_context.role, "unconfined_r");
        assert_eq!(security_context.type_, "unconfined_t");
        assert_eq!(
            security_context.range,
            Some(MlsRange {
                sensitivity: Sensitivity {
                    low: RangeValue("s0".to_string()),
                    high: Some(RangeValue("s0".to_string()))
                },
                category: Some(Category::List(vec![
                    RangeValue("c0".to_string()),
                    RangeValue("c3".to_string()),
                    RangeValue("c7".to_string())
                ]))
            })
        );
    }

    #[fuchsia::test]
    fn parse_invalid_security_contexts() {
        for invalid_label in ["u", "u:unconfined_r"] {
            assert_eq!(
                SecurityContext::try_from(invalid_label),
                Err(SecurityContextParseError::Invalid)
            );
        }
    }

    #[fuchsia::test]
    fn format_security_contexts() {
        for label in [
            "u:unconfined_r:unconfined_t:s0",
            "u:unconfined_r:unconfined_t:s0-s0",
            "u:unconfined_r:unconfined_t:s0:c0.c255",
            "u:unconfined_r:unconfined_t:s0-s0:c0.c1023",
            "u:unconfined_r:unconfined_t:s0:c0,c255",
            "u:unconfined_r:unconfined_t:s0-s0:c0,c3,c7",
        ] {
            let security_context = SecurityContext::try_from(label).expect("should succeed");
            assert_eq!(security_context.to_string(), label);
        }
    }
}
