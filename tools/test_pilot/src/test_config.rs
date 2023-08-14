// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use serde::{Deserialize, Serialize};
use serde_json::from_str;
use std::convert::TryFrom;
use std::fs;
use std::path::Path;

#[derive(Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TestConfigV1 {
    /// Arbitrary tags to identify and categorize the tests.
    #[serde(default)]
    pub tags: Vec<TestTag>,

    /// Specifies requested features for the test.
    #[serde(default)]
    pub requested_features: RequestedFeatures,

    /// Defines the configuration that is directly passed to the Host test binary.
    #[serde(default)]
    pub execution: serde_json::Value,
}

/// Configuration for a test to be executed.
#[derive(Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "version")]
pub enum TestConfiguration {
    #[serde(rename = "1")]
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
pub struct RequestedFeatures {
    /// True if this test requires a path to SDK tools.
    #[serde(default)]
    pub sdk_tools_path: bool,

    /// True if this test requires a Fuchsia system.
    #[serde(default)]
    pub requires_target: bool,

    /// True if the tests should be executed over serial.
    #[serde(default)]
    pub requires_serial: bool,
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
            "version": "1",
            "tags": [
                { "key": "tag1", "value": "value1" },
                { "key": "tag2", "value": "value2" }
            ],
            "requested_features": {
                "sdk_tools_path": true,
                "requires_target": false,
                "requires_serial": true
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
                requested_features: RequestedFeatures {
                    sdk_tools_path: true,
                    requires_target: false,
                    requires_serial: true,
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
            "version": "1",
        });

        temp_file.write_all(test_data.to_string().as_bytes()).unwrap();
        let file_path = temp_file.path();
        let test_config = TestConfiguration::try_from(file_path).expect("invalid json");

        assert_eq!(
            test_config,
            TestConfigV1 {
                tags: vec![],
                requested_features: RequestedFeatures {
                    sdk_tools_path: false,
                    requires_target: false,
                    requires_serial: false,
                },
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
            "requested_features": {
                "sdk_tools_path": false,
                "requires_target": true,
                "requires_serial": false
            },
            "execution": {}
        });

        temp_file.write_all(test_data.to_string().as_bytes()).unwrap();
        let file_path = temp_file.path();
        let err = TestConfiguration::try_from(file_path).expect_err("parsing should error out");
        let err_str = format!("{}", err);
        assert!(err_str.contains("unknown variant `1.0`, expected `1`"), "{}", err_str);
        temp_file.close().expect("Failed to close temporary file");
    }
}
