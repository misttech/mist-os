// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::{Error, Result},
    serde::{Deserialize, Serialize},
    std::collections::HashMap,
    thiserror::Error,
};

#[derive(Clone, Debug, Deserialize, Serialize, Error)]
#[serde(rename_all = "snake_case")]
pub enum ValidationError {
    #[error("Invalid validation policy configuration: {error}")]
    InvalidPolicyConfiguration { error: String },
    #[error("Validation failure: additional_boot_args MUST contain {expected_key}={expected_value}, but is missing key {expected_key}")]
    AdditionalBootArgsMustContainsKeyMissing { expected_key: String, expected_value: String },
    #[error("Validation failure: additional_boot_args MUST contain {expected_key}={expected_value}, but the value does not match. Expected: {expected_value}, Actual: {found_value}")]
    AdditionalBootArgsMustContainsValueIncorrect {
        expected_key: String,
        expected_value: String,
        found_value: String,
    },
    #[error("Validation failure: additional_boot_args MUST NOT contain {expected_key}={expected_value}, but does.")]
    AdditionalBootArgsMustNotContainsHasKeyValue { expected_key: String, expected_value: String },
}

/// The type of content to expect when performing ContentChecks.
#[derive(Deserialize, Serialize)]
pub enum ContentType {
    KeyValuePair(String, String),
    String(String),
}

#[derive(Deserialize, Serialize)]
pub struct BuildCheckSpec {
    /// Checks requiring presence or absence of specific key-value pairs in additional boot args.
    pub additional_boot_args_checks: Option<ContentCheckSpec>,
}

/// Defines a set of validations for content that must or must not be part of some input content.
/// There is no enforcement on mutual exclusion between must_contain and must_not_contain. If the same
/// value appears in both sets, validation will simply fail at check-time.
#[derive(Deserialize, Serialize)]
pub struct ContentCheckSpec {
    /// Set of items that must be present in the target content.
    pub must_contain: Option<Vec<ContentType>>,
    /// Set of items that must not be present in the target content.
    pub must_not_contain: Option<Vec<ContentType>>,
}

pub fn validate_build_checks(
    validation_policy: BuildCheckSpec,
    boot_args_data: HashMap<String, Vec<String>>,
) -> Result<Vec<ValidationError>, Error> {
    let mut errors_found = Vec::new();

    // If the policy specifies additional_boot_args checks, run them.
    if let Some(additional_boot_args_checks) = validation_policy.additional_boot_args_checks {
        for error in validate_additional_boot_args(additional_boot_args_checks, boot_args_data) {
            errors_found.push(error);
        }
    }
    Ok(errors_found)
}

fn validate_additional_boot_args(
    checks: ContentCheckSpec,
    boot_args_data: HashMap<String, Vec<String>>,
) -> Vec<ValidationError> {
    let mut errors = Vec::new();

    if let Some(must_contain_checks) = checks.must_contain {
        for check in must_contain_checks {
            match check {
                ContentType::KeyValuePair(key, value) => {
                    if !boot_args_data.contains_key(&key) {
                        errors.push(ValidationError::AdditionalBootArgsMustContainsKeyMissing {
                            expected_key: key,
                            expected_value: value,
                        });
                        continue;
                    }

                    if let Some(values_vec) = boot_args_data.get(&key) {
                        // AdditionalBootArgsCollector splits its values by the `+` delimiter.
                        // This check will only operate on the first value found in cases where
                        // multiple values are present.
                        let found_value = values_vec[0].clone();
                        if !(found_value == value) {
                            errors.push(
                                ValidationError::AdditionalBootArgsMustContainsValueIncorrect {
                                    expected_key: key,
                                    expected_value: value,
                                    found_value,
                                },
                            );
                        }
                    }
                }
                _ => {
                    errors.push(ValidationError::InvalidPolicyConfiguration {
                        error:
                            "Unexpected content type check for boot args, supports key value only."
                                .to_string(),
                    });
                }
            }
        }
    }

    if let Some(must_not_contain_checks) = checks.must_not_contain {
        for check in must_not_contain_checks {
            match check {
                ContentType::KeyValuePair(key, value) => {
                    if boot_args_data.contains_key(&key) {
                        if let Some(values_vec) = boot_args_data.get(&key) {
                            // AdditionalBootArgsCollector supports multiple `+` delimited values. This expects only 1 value for now.
                            let found_value = values_vec[0].clone();
                            if found_value == value {
                                errors.push(
                                    ValidationError::AdditionalBootArgsMustNotContainsHasKeyValue {
                                        expected_key: key,
                                        expected_value: value,
                                    },
                                );
                            }
                        }
                    }
                }
                _ => {
                    errors.push(ValidationError::InvalidPolicyConfiguration {
                        error:
                            "Unexpected content type check for boot args, supports key value only."
                                .to_string(),
                    });
                }
            }
        }
    }

    errors
}

#[cfg(test)]
mod tests {
    use {super::*, maplit::hashmap};

    // Test against a basic policy which has all of the elements included.
    #[test]
    fn test_validate_build_checks_success() {
        let expected_key = "test_key";
        let expected_value = "test_value";
        let policy = BuildCheckSpec {
            additional_boot_args_checks: Some(ContentCheckSpec {
                must_contain: Some(vec![ContentType::KeyValuePair(
                    expected_key.to_string(),
                    expected_value.to_string(),
                )]),
                must_not_contain: Some(vec![ContentType::KeyValuePair(
                    "some_other_key".to_string(),
                    "and_value".to_string(),
                )]),
            }),
        };
        let boot_args_data = hashmap! {
            expected_key.to_string() => vec![expected_value.to_string()]
        };

        let errors = validate_build_checks(policy, boot_args_data).unwrap();

        assert_eq!(errors.len(), 0);
    }

    #[test]
    fn test_validate_build_checks_tolerates_absent_boot_args_policy() {
        let expected_key = "test_key";
        let expected_value = "test_value";
        let policy = BuildCheckSpec { additional_boot_args_checks: None };
        let boot_args_data = hashmap! {
            expected_key.to_string() => vec![expected_value.to_string()]
        };

        let errors = validate_build_checks(policy, boot_args_data).unwrap();

        assert_eq!(errors.len(), 0);
    }

    #[test]
    fn test_boot_args_must_contain_success() {
        let expected_key = "test_key";
        let expected_value = "test_value";
        let policy = ContentCheckSpec {
            must_contain: Some(vec![ContentType::KeyValuePair(
                expected_key.to_string(),
                expected_value.to_string(),
            )]),
            must_not_contain: None,
        };
        let input_data = hashmap! {
            expected_key.to_string() => vec![expected_value.to_string()]
        };

        let validation_errors = validate_additional_boot_args(policy, input_data);

        assert_eq!(validation_errors.len(), 0);
    }

    #[test]
    fn test_boot_args_must_contain_failure() {
        let expected_key = "test_key";
        let expected_value = "test_value";
        let policy = ContentCheckSpec {
            must_contain: Some(vec![ContentType::KeyValuePair(
                expected_key.to_string(),
                expected_value.to_string(),
            )]),
            must_not_contain: None,
        };
        let input_data = hashmap! {
            "some_other_key".to_string() => vec!["some_other_value".to_string()]
        };

        let validation_errors = validate_additional_boot_args(policy, input_data);

        assert!(validation_errors.len() == 1);
        match &validation_errors[0] {
            // Check that we report the value we were looking for, but did not find.
            ValidationError::AdditionalBootArgsMustContainsKeyMissing {
                expected_key,
                expected_value,
            } => {
                assert_eq!(*expected_key, "test_key".to_string());
                assert_eq!(*expected_value, "test_value".to_string());
            }
            _ => assert!(false),
        }
    }

    #[test]
    fn test_boot_args_must_contain_invalid_policy_configuration() {
        // Boot args checks only accept KeyValue pair as the content type.
        let policy = ContentCheckSpec {
            must_contain: Some(vec![ContentType::String("test".to_string())]),
            must_not_contain: None,
        };
        let input_data = HashMap::new();

        let validation_errors = validate_additional_boot_args(policy, input_data);

        assert_eq!(validation_errors.len(), 1);
        // Check error type.
        match &validation_errors[0] {
            ValidationError::InvalidPolicyConfiguration { error: _ } => {}
            _ => assert!(false, "Unexpected error type from validate_additional_boot_args test"),
        }
    }

    #[test]
    fn test_boot_args_must_not_contain_success() {
        let expected_key = "test_key";
        let expected_value = "test_value";
        // The policy sets the expectations.
        let policy = ContentCheckSpec {
            must_contain: None,
            must_not_contain: Some(vec![ContentType::KeyValuePair(
                expected_key.to_string(),
                expected_value.to_string(),
            )]),
        };
        // The input data here conforms to the policy.
        let input_data = hashmap! {
            "some_other_key".to_string() => vec!["some_other_value".to_string()]
        };

        let validation_errors = validate_additional_boot_args(policy, input_data);

        assert_eq!(validation_errors.len(), 0);
    }

    #[test]
    fn test_boot_args_must_not_contain_failure() {
        let expected_key = "test_key";
        let expected_value = "test_value";
        // The policy sets the expectations.
        let policy = ContentCheckSpec {
            must_contain: None,
            must_not_contain: Some(vec![ContentType::KeyValuePair(
                expected_key.to_string(),
                expected_value.to_string(),
            )]),
        };
        // The input data here does not conform to the policy.
        let input_data = hashmap! {
            expected_key.to_string() => vec![expected_value.to_string()]
        };

        let validation_errors = validate_additional_boot_args(policy, input_data);

        assert_eq!(validation_errors.len(), 1);
        match &validation_errors[0] {
            // Check that we report the value we were expecting to be absent, but was present.
            ValidationError::AdditionalBootArgsMustNotContainsHasKeyValue {
                expected_key,
                expected_value,
            } => {
                assert_eq!(*expected_key, "test_key");
                assert_eq!(*expected_value, "test_value");
            }
            _ => assert!(false, "Unexpected error type from validate_additional_boot_args test"),
        }
    }

    #[test]
    fn test_boot_args_must_not_contain_invalid_policy_configuration() {
        // Boot args checks only accept KeyValue pair as the content type.
        let policy = ContentCheckSpec {
            must_contain: None,
            must_not_contain: Some(vec![ContentType::String("test".to_string())]),
        };
        let input_data = HashMap::new();

        let validation_errors = validate_additional_boot_args(policy, input_data);

        assert_eq!(validation_errors.len(), 1);
        // Check error type.
        match &validation_errors[0] {
            ValidationError::InvalidPolicyConfiguration { error: _ } => {}
            _ => assert!(false, "Unexpected error type from validate_additional_boot_args test"),
        }
    }
}
