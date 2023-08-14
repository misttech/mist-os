// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::test_config;
use anyhow::{ensure, format_err, Error};
use std::fs;
use std::num::ParseIntError;
use std::path::PathBuf;
use structopt::StructOpt;

#[allow(dead_code)]
#[derive(Debug, StructOpt)]
#[structopt(name = "Test Pilot")]
pub struct CommandLineArgs {
    /// Path to test configuration.
    #[structopt(
        long = "test_config",
        value_name = "FILEPATH",
        required = true,
        parse(try_from_str = "file_parse_path")
    )]
    pub test_config: PathBuf,

    /// Path to the host test binary to execute.
    #[structopt(
        long = "test_bin_path",
        value_name = "FILEPATH",
        required = true,
        parse(try_from_str = "file_parse_path")
    )]
    pub test_bin_path: PathBuf,

    ///  Fuchsia targets. User can pass in multiple targets using multiple --target flags.
    #[structopt(long = "target", multiple = true)]
    pub targets: Vec<String>,

    /// Timeout for the test in seconds. This would be passed to host binary if supported.
    #[structopt(
        long = "timeout_seconds",
        value_name = "SECONDS",
        parse(try_from_str = "parse_u32")
    )]
    pub timeout_seconds: Option<u32>,

    /// Override grace timeout period. This timeout gives the Host test binary a certain period to
    /// exit gracefully after `timeout_seconds`.
    #[structopt(long = "grace_timeout", value_name = "SECONDS", parse(try_from_str = "parse_u32"))]
    pub grace_timeout: Option<u32>,

    /// Path to the SDK tools directory.
    #[structopt(
        long = "sdk_tools_path",
        value_name = "DIR_PATH",
        parse(try_from_str = "dir_parse_path")
    )]
    pub sdk_tools_path: Option<PathBuf>,

    /// Path to the resources directory.
    #[structopt(
        long = "resource_path",
        value_name = "DIR_PATH",
        parse(try_from_str = "dir_parse_path")
    )]
    pub resource_path: Option<PathBuf>,

    /// Comma-separated glob pattern for test cases to run.
    #[structopt(long = "test_filter")]
    pub test_filter: Option<String>,

    ///  Extra arguments to pass to the host test binary as is.
    #[structopt(long = "extra_test_args")]
    pub extra_test_args: Option<String>,
}

impl CommandLineArgs {
    pub fn validate(&self, config: &test_config::TestConfiguration) -> Result<(), Error> {
        match config {
            test_config::TestConfiguration::V1 { config } => {
                if config.requested_features.sdk_tools_path && self.sdk_tools_path.is_none() {
                    return Err(format_err!("--sdk_tools_path is required for this test"));
                }

                if config.requested_features.requires_target && self.targets.is_empty() {
                    return Err(format_err!("--target is required for this test"));
                }
            }
        };
        Ok(())
    }
}

fn parse_u32(s: &str) -> Result<u32, ParseIntError> {
    Ok(s.parse::<u32>()?)
}

fn file_parse_path(path: &str) -> Result<PathBuf, Error> {
    let path = PathBuf::from(path);
    ensure!(path.exists(), "{:?} does not exist", path);
    let metadata = fs::metadata(&path)?;
    ensure!(metadata.is_file(), "{:?} should be a file", path);
    Ok(path)
}

fn dir_parse_path(path: &str) -> Result<PathBuf, Error> {
    let path = PathBuf::from(path);
    ensure!(path.exists(), "{:?} does not exist", path);
    let metadata = fs::metadata(&path)?;
    ensure!(metadata.is_dir(), "{:?} should be a directory", path);
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    use tempfile::NamedTempFile;
    use test_config::*;

    #[test]
    fn test_parse_u32() {
        assert_eq!(parse_u32("42"), Ok(42));
        assert_eq!(parse_u32("0"), Ok(0));
        assert_eq!(parse_u32("12345"), Ok(12345));
        assert_eq!(parse_u32("100000"), Ok(100000));

        // Invalid inputs
        assert!(parse_u32("abc").is_err());
        assert!(parse_u32("-10").is_err());
        assert!(parse_u32("4294967296").is_err()); // Exceeds u32::MAX
    }

    #[test]
    fn test_dir_parse_path() {
        // Create a temporary directory
        let temp_dir = tempdir().expect("Failed to create temporary directory");
        let temp_dir_path = temp_dir.path();

        // Valid directory path
        assert!(dir_parse_path(temp_dir_path.to_str().unwrap()).is_ok());
        assert!(dir_parse_path(".").is_ok());

        // Clean up temporary directory after the test
        temp_dir.close().expect("Failed to close temporary directory");

        // Invalid directory path
        assert!(dir_parse_path("/non_existent_path").is_err());
        // Create a temporary file
        let temp_file = NamedTempFile::new().expect("Failed to create temporary file in the test");
        let temp_file_path = temp_file.path().to_str().unwrap().to_string();

        assert!(dir_parse_path(temp_file_path.as_str()).is_err()); // File path
    }

    #[test]
    fn test_file_parse_path() {
        // Create a temporary file
        let temp_file = NamedTempFile::new().expect("Failed to create temporary file in the test");

        // Get the path of the temporary file
        let temp_file_path = temp_file.path().to_str().unwrap().to_string();

        // Valid file path
        assert!(file_parse_path(temp_file_path.as_str()).is_ok());

        // Clean up temporary file after the test
        temp_file.close().expect("Failed to close temporary file");

        // Invalid file path
        assert!(file_parse_path("/non_existent_file").is_err());
        assert!(file_parse_path("/tmp").is_err()); // Directory path
    }

    #[test]
    fn test_args() {
        let temp_dir = tempdir().expect("Failed to create temporary directory");
        let temp_dir_path = temp_dir.path();

        let temp_file = NamedTempFile::new().expect("Failed to create temporary file in the test");
        let temp_file_path = temp_file.path();
        let temp_config_file =
            NamedTempFile::new().expect("Failed to create temporary file in the test");
        let temp_config_file_path = temp_config_file.path();

        let args = vec![
            "test_pilot",
            "--target",
            "arm64",
            "--timeout_seconds",
            "10",
            "--sdk_tools_path",
            temp_dir_path.to_str().unwrap(),
            "--resource_path",
            temp_dir_path.to_str().unwrap(),
            "--grace_timeout",
            "5",
            "--test_config",
            temp_config_file_path.to_str().unwrap(),
            "--test_bin_path",
            temp_file_path.to_str().unwrap(),
            "--test_filter",
            "test_filter",
            "--extra_test_args",
            "extra_args",
        ];

        // Convert the arguments to OsString and pass them to the test
        let args_os: Vec<std::ffi::OsString> =
            args.iter().map(|arg| std::ffi::OsString::from(*arg)).collect();
        let args_os_slice: &[std::ffi::OsString] = &args_os;

        // Parse command-line arguments
        let parsed_args = CommandLineArgs::from_iter_safe(args_os_slice)
            .expect("command line args should not fail");

        // Check the individual arguments
        assert_eq!(parsed_args.targets, vec!["arm64".to_string()]);
        assert_eq!(parsed_args.timeout_seconds, Some(10));
        assert_eq!(parsed_args.sdk_tools_path, Some(temp_dir_path.to_path_buf()));
        assert_eq!(parsed_args.resource_path, Some(temp_dir_path.to_path_buf()));
        assert_eq!(parsed_args.grace_timeout, Some(5));
        assert_eq!(parsed_args.test_bin_path, temp_file_path.to_path_buf());
        assert_eq!(parsed_args.test_config, temp_config_file_path.to_path_buf());
        assert_eq!(parsed_args.test_filter, Some("test_filter".to_string()));
        assert_eq!(parsed_args.extra_test_args, Some("extra_args".to_string()));

        // Clean up temporary resources after the test
        temp_dir.close().expect("Failed to close temporary directory");
        temp_file.close().expect("Failed to close temporary file");
    }

    #[test]
    fn test_args_missing_required_param() {
        // Simulate command-line arguments without '--test_bin_path'
        let args = vec![
            "test_pilot",
            "--target",
            "arm64",
            "--timeout_seconds",
            "10",
            "--sdk_tools_path",
            "/path/to/sdk_tools",
            "--resource_path",
            "/path/to/resources",
            "--grace_timeout",
            "5",
            "--test_filter",
            "test_filter",
            "--extra_test_args",
            "extra_args",
        ];

        // Convert the arguments to OsString and pass them to the test
        let args_os: Vec<std::ffi::OsString> =
            args.iter().map(|arg| std::ffi::OsString::from(*arg)).collect();
        let args_os_slice: &[std::ffi::OsString] = &args_os;

        // Parse command-line arguments
        let parsed_args = CommandLineArgs::from_iter_safe(args_os_slice);

        let err = parsed_args.unwrap_err();
        assert!(
            err.message.contains(
                "The following required arguments were not provided:\n    --test_bin_path"
            ),
            "{:?}",
            err
        );
    }

    #[test]
    fn test_args_only_required_params() {
        let temp_file = NamedTempFile::new().expect("Failed to create temporary file in the test");
        let temp_file_path = temp_file.path();

        // Simulate command-line arguments with only '--test_bin_path'
        let args = vec![
            "test_pilot",
            "--test_bin_path",
            temp_file_path.to_str().unwrap(),
            "--test_config",
            temp_file_path.to_str().unwrap(),
        ];

        // Convert the arguments to OsString and pass them to the test
        let args_os: Vec<std::ffi::OsString> =
            args.iter().map(|arg| std::ffi::OsString::from(*arg)).collect();
        let args_os_slice: &[std::ffi::OsString] = &args_os;

        // Parse command-line arguments
        let parsed_args = CommandLineArgs::from_iter_safe(args_os_slice).unwrap();

        // Check the individual arguments
        assert_eq!(parsed_args.targets, Vec::<String>::new());
        assert_eq!(parsed_args.timeout_seconds, None);
        assert_eq!(parsed_args.sdk_tools_path, None);
        assert_eq!(parsed_args.resource_path, None);
        assert_eq!(parsed_args.grace_timeout, None);
        assert_eq!(parsed_args.test_filter, None);
        assert_eq!(parsed_args.extra_test_args, None);
        assert_eq!(parsed_args.test_bin_path, temp_file_path.to_path_buf());
        assert_eq!(parsed_args.test_config, temp_file_path.to_path_buf());
    }

    // Test for invalid value for '--timeout_seconds'
    #[test]
    fn test_args_invalid_timeout_seconds() {
        let temp_file = NamedTempFile::new().expect("Failed to create temporary file in the test");
        let temp_file_path = temp_file.path();

        // Simulate command-line arguments with invalid '--timeout_seconds'
        let args = vec![
            "test_pilot",
            "--test_bin_path",
            temp_file_path.to_str().unwrap(),
            "--test_config",
            temp_file_path.to_str().unwrap(),
            "--timeout_seconds",
            "abc", // Invalid value for u32
        ];

        // Convert the arguments to OsString and pass them to the test
        let args_os: Vec<std::ffi::OsString> =
            args.iter().map(|arg| std::ffi::OsString::from(*arg)).collect();
        let args_os_slice: &[std::ffi::OsString] = &args_os;

        // Parse command-line arguments
        let parsed_args = CommandLineArgs::from_iter_safe(args_os_slice);

        let err = parsed_args.unwrap_err();
        assert!(err.message.contains("invalid digit found in string"), "{:?}", err);
    }

    // Helper function to create a simple TestConfiguration for testing
    fn create_test_config_v1() -> TestConfigV1 {
        TestConfigV1 {
            tags: Vec::new(),
            requested_features: RequestedFeatures {
                sdk_tools_path: false,
                requires_target: false,
                requires_serial: false,
            },
            execution: serde_json::json!({}),
        }
    }

    #[test]
    fn test_validate_success() {
        let args = CommandLineArgs {
            test_config: PathBuf::from("test_config.json"),
            test_bin_path: PathBuf::from("test_bin"),
            targets: vec!["target1".to_string()],
            timeout_seconds: Some(30),
            grace_timeout: Some(5),
            sdk_tools_path: Some(PathBuf::from("sdk_tools")),
            resource_path: Some(PathBuf::from("resources")),
            test_filter: Some("test_filter".to_string()),
            extra_test_args: Some("extra_args".to_string()),
        };

        let mut test_config = create_test_config_v1();
        test_config.requested_features.sdk_tools_path = true;
        test_config.requested_features.requires_target = true;
        let test_config = test_config.into();
        let result = args.validate(&test_config);
        assert!(result.is_ok());

        let args = CommandLineArgs {
            test_config: PathBuf::from("test_config.json"),
            test_bin_path: PathBuf::from("test_bin"),
            targets: vec![],
            timeout_seconds: Some(30),
            grace_timeout: Some(5),
            sdk_tools_path: None,
            resource_path: None,
            test_filter: Some("test_filter".to_string()),
            extra_test_args: Some("extra_args".to_string()),
        };

        let test_config = create_test_config_v1().into();
        let result = args.validate(&test_config);
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_missing_sdk_tools_path() {
        let args = CommandLineArgs {
            test_config: PathBuf::from("test_config.json"),
            test_bin_path: PathBuf::from("test_bin"),
            targets: vec!["target1".to_string()],
            timeout_seconds: Some(30),
            grace_timeout: Some(5),
            sdk_tools_path: None, // Missing sdk_tools_path
            resource_path: Some(PathBuf::from("resources")),
            test_filter: Some("test_filter".to_string()),
            extra_test_args: Some("extra_args".to_string()),
        };

        let mut test_config = create_test_config_v1();
        test_config.requested_features.sdk_tools_path = true;
        let test_config = test_config.into();
        let result = args.validate(&test_config);
        assert_eq!(result.unwrap_err().to_string(), "--sdk_tools_path is required for this test");
    }

    #[test]
    fn test_validate_missing_targets() {
        let args = CommandLineArgs {
            test_config: PathBuf::from("test_config.json"),
            test_bin_path: PathBuf::from("test_bin"),
            targets: Vec::new(), // Missing targets
            timeout_seconds: Some(30),
            grace_timeout: Some(5),
            sdk_tools_path: Some(PathBuf::from("sdk_tools")),
            resource_path: Some(PathBuf::from("resources")),
            test_filter: Some("test_filter".to_string()),
            extra_test_args: Some("extra_args".to_string()),
        };

        let mut test_config = create_test_config_v1();
        test_config.requested_features.requires_target = true;
        let test_config = test_config.into();
        let result = args.validate(&test_config);
        assert_eq!(result.unwrap_err().to_string(), "--target is required for this test");
    }
}
