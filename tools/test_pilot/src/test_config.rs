// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use serde::{Deserialize, Serialize};
use serde_json::from_str;
use std::fs;
use std::path::Path;

#[derive(Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TestConfigV1 {
    /// Arbitrary tags to identify and categorize the tests.
    #[serde(default)]
    pub tags: Vec<TestTag>,

    /// Specifies requested features for the test.
    #[serde(default)]
    pub requested_vars: RequestedVars,

    /// Defines the configuration that is directly passed to the Host test binary.
    #[serde(default)]
    pub execution: serde_json::Value,
}

/// Configuration for a test to be executed.
#[derive(Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "version")]
pub enum TestConfiguration {
    #[serde(rename = "0.1")]
    V1 {
        #[serde(flatten)]
        config: TestConfigV1,
    },
}

impl From<TestConfigV1> for TestConfiguration {
    fn from(config: TestConfigV1) -> Self {
        Self::V1 { config }
    }
}

#[derive(Debug, Eq, PartialEq, Serialize, Deserialize, PartialOrd, Ord, Clone)]
pub struct TestTag {
    pub key: String,
    pub value: String,
}

/// Specifies requested features for the test.
#[derive(Debug, Eq, PartialEq, Serialize, Deserialize, Default)]
pub struct RequestedVars {
    /// List of known env variables which are required by the test.
    #[serde(default, alias = "KNOWN_VARS")]
    pub known_vars: Vec<String>,

    /// Extra variables required by the test.
    #[serde(default, alias = "EXTRA_VARS")]
    pub extra_vars: Vec<String>,
}

impl TryFrom<&Path> for TestConfiguration {
    type Error = anyhow::Error;

    fn try_from(file_path: &Path) -> Result<Self, Self::Error> {
        // Read JSON data from the file
        let json_str = fs::read_to_string(&file_path)?;

        // Deserialize JSON data into a TestConfiguration struct
        let test_config: TestConfiguration = from_str(&json_str)?;

        Ok(test_config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn test_success() {
        let mut temp_file = NamedTempFile::new().unwrap();
        let test_data = json!({
            "environment": {},
            "version": "0.1",
            "tags": [
                { "key": "tag1", "value": "value1" },
                { "key": "tag2", "value": "value2" }
            ],
            "requested_vars": {
                "KNOWN_VARS": ["FUCHSIA_SDK_TOOL_PATH"],
                "EXTRA_VARS": ["FOO", "BAR"]
            },
            "execution": {
                "some_object": { "some_key": "val" },
                "some_array": [1, 10],
            }
        });

        temp_file.write_all(test_data.to_string().as_bytes()).unwrap();
        let file_path = temp_file.path();
        let test_config = TestConfiguration::try_from(file_path).expect("invalid json");

        assert_eq!(
            test_config,
            TestConfigV1 {
                tags: vec![
                    TestTag { key: "tag1".to_string(), value: "value1".to_string() },
                    TestTag { key: "tag2".to_string(), value: "value2".to_string() },
                ],
                requested_vars: RequestedVars {
                    known_vars: vec!["FUCHSIA_SDK_TOOL_PATH".into()],
                    extra_vars: vec!["FOO".into(), "BAR".into()],
                },
                execution: json!({
                    "some_object": { "some_key": "val" },
                    "some_array": [1, 10]
                }),
            }
            .into()
        );
        temp_file.close().expect("Failed to close temporary file");
    }

    #[test]
    fn test_default() {
        let mut temp_file = NamedTempFile::new().unwrap();
        let test_data = json!({
            "environment": {}, // extra value in config file
            "version": "0.1",
        });

        temp_file.write_all(test_data.to_string().as_bytes()).unwrap();
        let file_path = temp_file.path();
        let test_config = TestConfiguration::try_from(file_path).expect("invalid json");

        assert_eq!(
            test_config,
            TestConfigV1 {
                tags: vec![],
                requested_vars: RequestedVars { known_vars: vec![], extra_vars: vec![] },
                execution: serde_json::Value::default(),
            }
            .into()
        );
        temp_file.close().expect("Failed to close temporary file");
    }

    #[test]
    fn test_invalid_json() {
        let mut temp_file = NamedTempFile::new().unwrap();
        temp_file.write_all(b"invalid_json_data").unwrap();

        let file_path = temp_file.path();

        let _err = TestConfiguration::try_from(file_path).expect_err("parsing should error out");

        temp_file.close().expect("Failed to close temporary file");
    }

    #[test]
    fn test_empty_config() {
        // Create a temporary JSON file with missing "version" field
        let mut temp_file = NamedTempFile::new().unwrap();
        let test_data = "{}";

        temp_file.write_all(test_data.to_string().as_bytes()).unwrap();
        let file_path = temp_file.path();

        let err = TestConfiguration::try_from(file_path).expect_err("parsing should error out");
        let err_str = format!("{}", err);
        assert!(err_str.contains("missing field `version`"), "{}", err_str);
        temp_file.close().expect("Failed to close temporary file");
    }

    #[test]
    fn test_invalid_version() {
        // Create a temporary JSON file with invalid "count" field
        let mut temp_file = NamedTempFile::new().unwrap();
        let test_data = json!({
            "tags": [],
            "version": "1.0",
            "execution": {}
        });

        temp_file.write_all(test_data.to_string().as_bytes()).unwrap();
        let file_path = temp_file.path();
        let err = TestConfiguration::try_from(file_path).expect_err("parsing should error out");
        let err_str = format!("{}", err);
        assert!(err_str.contains("unknown variant `1.0`, expected `0.1`"), "{}", err_str);
        temp_file.close().expect("Failed to close temporary file");
    }
}
