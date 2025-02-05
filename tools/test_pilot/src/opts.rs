// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::test_config;
use anyhow::{ensure, Error};
use std::env::VarError;
use std::fs;
use std::num::ParseIntError;
use std::path::PathBuf;
use structopt::StructOpt;
use thiserror::Error as ThisError;

pub const ENV_TARGETS: &str = "FUCHSIA_TARGETS";
pub const ENV_TIMEOUT_SECONDS: &str = "FUCHSIA_TIMEOUT_SECONDS";
pub const ENV_SDK_TOOL_PATH: &str = "FUCHSIA_SDK_TOOL_PATH";
pub const ENV_RESOURCE_PATH: &str = "FUCHSIA_RESOURCE_PATH";
pub const ENV_CUSTOM_TEST_ARGS: &str = "FUCHSIA_CUSTOM_TEST_ARGS";
pub const ENV_TEST_FILTER: &str = "FUCHSIA_TEST_FILTER";
pub const ENV_OUT_DIR: &str = "FUCHSIA_TEST_OUTDIR";
pub const ENV_PATH: &str = "PATH";
pub const ENV_DEBUG: &str = "DEBUG";

const ENV_STRICT_MODE: &str = "FUCHSIA_TEST_PILOT_STRICT_MODE";

const ALL_ENV_VARS: [&str; 8] = [
    ENV_TARGETS,
    ENV_TIMEOUT_SECONDS,
    ENV_SDK_TOOL_PATH,
    ENV_RESOURCE_PATH,
    ENV_OUT_DIR,
    ENV_CUSTOM_TEST_ARGS,
    ENV_TEST_FILTER,
    ENV_STRICT_MODE,
];

#[derive(Debug, StructOpt, Clone)]
#[structopt(name = "Test Pilot", about = "Assist Test Execution.")]
struct CommandLineArgs {
    /// Path to the host test binary to execute.
    #[structopt(
        long,
        value_name = "FILEPATH",
        required = true,
        parse(try_from_str = "file_parse_path")
    )]
    fuchsia_test_bin_path: PathBuf,

    /// Path to test configuration.
    #[structopt(
        long,
        value_name = "FILEPATH",
        required = true,
        parse(try_from_str = "file_parse_path")
    )]
    fuchsia_test_configuration: PathBuf,
}

/// Error encountered running test manager
#[derive(Debug, PartialEq, Eq)]
pub struct TestPilotArgs {
    /// Path to test configuration.
    pub test_config: PathBuf,

    /// Path to the host test binary to execute.
    pub test_bin_path: PathBuf,

    ///  PATH environment variable.
    pub path: String,

    ///  Fuchsia targets. User can pass in multiple comma separated targets.
    pub targets: Vec<String>,

    /// Timeout for the test in seconds. This would be passed to host binary if supported.
    pub timeout_seconds: Option<u32>,

    /// Path to the SDK tools directory.
    pub sdk_tools_path: Option<PathBuf>,

    /// Path to the resources directory.
    pub resource_path: Option<PathBuf>,

    /// Comma-separated glob pattern for test cases to run.
    pub test_filter: Option<String>,

    /// Output directory.
    pub out_dir: Option<PathBuf>,

    ///  Extra arguments to pass to the host test binary as is.
    pub custom_test_args: Option<String>,

    /// Pass along extra_env_vars if strict mode is disabled.
    pub strict_mode: bool,

    /// Extra environment variables passed as it is to the test binary.
    pub extra_env_vars: Vec<(String, String)>,

    /// Pass on a DEBUG environment variable to the test binary.
    pub debug: bool,
}

// allows us to mock std::env for unit tests.
trait Environment {
    fn var(&self, key: &str) -> Result<String, VarError>;
    fn vars(&self) -> impl Iterator<Item = (String, String)>;
    fn remove_var(&mut self, key: &str);

    #[cfg(test)]
    fn set_var(&mut self, key: &str, value: &str);
}

struct StandardEnvironment;

impl Environment for StandardEnvironment {
    fn var(&self, key: &str) -> Result<String, VarError> {
        std::env::var(key)
    }

    fn vars(&self) -> impl Iterator<Item = (String, String)> {
        std::env::vars()
    }

    fn remove_var(&mut self, key: &str) {
        std::env::remove_var(key);
    }

    #[cfg(test)]
    fn set_var(&mut self, key: &str, value: &str) {
        std::env::set_var(key, value);
    }
}

/// Error encountered parsing Test Pilot arguments
#[derive(Debug, ThisError)]
pub enum TestPilotArgsError {
    #[error("Error with environment variable '{1}': {0:?}")]
    Var(VarError, &'static str),

    #[error("Error validating environment variable: '{1}': {0:?}")]
    Validation(anyhow::Error, &'static str),
}

/// Error encountered validating config
#[derive(Debug, ThisError, Eq, PartialEq)]
pub enum ConfigError {
    #[error("{0} is required for this test.")]
    Required(&'static str),
}

macro_rules! parse_optional_var {
    ($env:expr, $key:expr, $parser:expr) => {
        $env.var($key)
            .ok()
            .map(|v| $parser(&v).map_err(|e| TestPilotArgsError::Validation(e.into(), $key)))
            .transpose()
    };
}

macro_rules! required_config {
    (option: $option:expr, $config_var:ident) => {
        if $option.is_none() {
            return Err(ConfigError::Required($config_var));
        }
    };
    (vec: $vec:expr, $config_var:ident) => {
        if $vec.is_empty() {
            return Err(ConfigError::Required($config_var));
        }
    };
}

impl TestPilotArgs {
    pub fn validate_config(
        &self,
        test_config: &test_config::TestConfiguration,
    ) -> Result<(), ConfigError> {
        match test_config {
            test_config::TestConfiguration::V1 { config } => {
                for var in &config.requested_vars.known_vars {
                    match var.as_str() {
                        ENV_SDK_TOOL_PATH => {
                            required_config!(option: self.sdk_tools_path, ENV_SDK_TOOL_PATH)
                        }
                        ENV_TARGETS => required_config!(vec: self.targets, ENV_TARGETS),
                        ENV_RESOURCE_PATH => {
                            required_config!(option: self.resource_path, ENV_RESOURCE_PATH)
                        }
                        _ => {}
                    }
                }
            }
        };
        Ok(())
    }

    pub fn from_env_and_cmd() -> Result<Self, TestPilotArgsError> {
        let mut env = StandardEnvironment;
        let cmd = CommandLineArgs::from_args();
        Self::from_internal(&mut env, cmd)
    }

    fn from_internal<E: Environment>(
        env: &mut E,
        cmd: CommandLineArgs,
    ) -> Result<Self, TestPilotArgsError> {
        let mut s = Self {
            test_config: cmd.fuchsia_test_configuration,
            test_bin_path: cmd.fuchsia_test_bin_path,
            path: env.var(ENV_PATH).unwrap_or_else(|_| "".into()),
            timeout_seconds: parse_optional_var!(env, ENV_TIMEOUT_SECONDS, parse_u32)?,
            sdk_tools_path: parse_optional_var!(env, ENV_SDK_TOOL_PATH, dir_parse_path)?,
            resource_path: parse_optional_var!(env, ENV_RESOURCE_PATH, dir_parse_path)?,
            out_dir: parse_optional_var!(env, ENV_OUT_DIR, dir_parse_path)?,
            targets: env
                .var(ENV_TARGETS)
                .ok()
                .and_then(|v| Some(v.split(',').map(|s| s.trim().to_string()).collect()))
                .unwrap_or_default(),
            test_filter: env.var(ENV_TEST_FILTER).ok(),
            custom_test_args: env.var(ENV_CUSTOM_TEST_ARGS).ok(),
            strict_mode: env.var(ENV_STRICT_MODE).ok().map_or(true, |m| !m.eq("0")),
            extra_env_vars: vec![],
            debug: env.var(ENV_DEBUG).map(|_| true).unwrap_or(false),
        };

        // Remove known env variables so that we can store extra variables to pass them along.
        for var in ALL_ENV_VARS {
            env.remove_var(var);
        }

        for v in env.vars() {
            s.extra_env_vars.push(v)
        }

        Ok(s)
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
    use structopt::clap::ErrorKind::MissingRequiredArgument;
    use tempfile::{tempdir, NamedTempFile};
    use test_config::*;

    struct MockEnvironment {
        variables: std::collections::HashMap<String, String>,
    }

    impl MockEnvironment {
        fn new() -> Self {
            Self { variables: std::collections::HashMap::new() }
        }
    }

    impl Environment for MockEnvironment {
        fn var(&self, key: &str) -> Result<String, VarError> {
            self.variables.get(key).cloned().ok_or(VarError::NotPresent)
        }

        fn vars(&self) -> impl Iterator<Item = (String, String)> {
            self.variables.clone().into_iter()
        }

        fn set_var(&mut self, key: &str, value: &str) {
            self.variables.insert(key.to_string(), value.to_string());
        }

        fn remove_var(&mut self, key: &str) {
            self.variables.remove(key);
        }
    }

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

    // Validate that known environment variables are cleared
    macro_rules! validate_env_cleared {
        ($var:expr) => {
            for v in ALL_ENV_VARS {
                $var.var(v).expect_err(&format!("{} should not be present", v));
            }
        };
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

        let mut env = MockEnvironment::new();

        let cmd = CommandLineArgs {
            fuchsia_test_bin_path: temp_file_path.to_path_buf(),
            fuchsia_test_configuration: temp_config_file_path.to_path_buf(),
        };

        env.set_var(ENV_TARGETS, "target1, target2");
        env.set_var(ENV_TIMEOUT_SECONDS, "10");
        env.set_var(ENV_SDK_TOOL_PATH, temp_dir_path.to_str().unwrap());
        env.set_var(ENV_RESOURCE_PATH, temp_dir_path.to_str().unwrap());
        env.set_var(ENV_OUT_DIR, temp_dir_path.to_str().unwrap());
        env.set_var(ENV_TEST_FILTER, "test_filter");
        env.set_var(ENV_CUSTOM_TEST_ARGS, "custom_args");

        let parsed_args =
            TestPilotArgs::from_internal(&mut env, cmd).expect("env args should not fail");

        // Check the individual arguments
        assert_eq!(parsed_args.targets, vec!["target1".to_string(), "target2".to_string()]);
        assert_eq!(parsed_args.timeout_seconds, Some(10));
        assert_eq!(parsed_args.sdk_tools_path, Some(temp_dir_path.to_path_buf()));
        assert_eq!(parsed_args.resource_path, Some(temp_dir_path.to_path_buf()));
        assert_eq!(parsed_args.test_bin_path, temp_file_path.to_path_buf());
        assert_eq!(parsed_args.test_config, temp_config_file_path.to_path_buf());
        assert_eq!(parsed_args.test_filter, Some("test_filter".to_string()));
        assert_eq!(parsed_args.out_dir, Some(temp_dir_path.to_path_buf()));
        assert!(parsed_args.strict_mode);
        assert_eq!(parsed_args.custom_test_args, Some("custom_args".to_string()));
        assert_eq!(parsed_args.extra_env_vars, vec![]);
        validate_env_cleared!(env);
        // Clean up temporary resources after the test
        temp_dir.close().expect("Failed to close temporary directory");
        temp_file.close().expect("Failed to close temporary file");
    }

    #[test]
    fn test_cmd_args() {
        let temp_file = NamedTempFile::new().expect("Failed to create temporary file in the test");
        let temp_file_path = temp_file.path();
        let temp_config_file =
            NamedTempFile::new().expect("Failed to create temporary file in the test");
        let temp_config_file_path = temp_config_file.path();

        let args = vec![
            "test_pilot",
            "--fuchsia_test_configuration",
            temp_config_file_path.to_str().unwrap(),
            "--fuchsia_test_bin_path",
            temp_file_path.to_str().unwrap(),
        ];

        // Convert the arguments to OsString and pass them to the arg parser.
        let args_os: Vec<std::ffi::OsString> =
            args.iter().map(|arg| std::ffi::OsString::from(*arg)).collect();
        let args_os_slice: &[std::ffi::OsString] = &args_os;

        // Parse command-line arguments
        let parsed_args = CommandLineArgs::from_iter_safe(args_os_slice)
            .expect("command line args should not fail");

        // Check the individual arguments
        assert_eq!(parsed_args.fuchsia_test_bin_path, temp_file_path.to_path_buf());
        assert_eq!(parsed_args.fuchsia_test_configuration, temp_config_file_path.to_path_buf());

        // Clean up temporary resources after the test
        temp_file.close().expect("Failed to close temporary file");
        temp_config_file.close().expect("Failed to close temporary file");
    }

    #[test]
    fn test_cmd_args_missing_required_param() {
        // Simulate command-line arguments without '--test_bin_path'
        let args = vec!["test_pilot"];

        // Convert the arguments to OsString and pass them to the arg parser.
        let args_os: Vec<std::ffi::OsString> =
            args.iter().map(|arg| std::ffi::OsString::from(*arg)).collect();
        let args_os_slice: &[std::ffi::OsString] = &args_os;

        // Parse command-line arguments
        let parsed_args = CommandLineArgs::from_iter_safe(args_os_slice);

        let err = parsed_args.unwrap_err();
        assert_eq!(err.kind, MissingRequiredArgument);
        assert!(
            err.message.contains(
                "The following required arguments were not provided:\n    --fuchsia_test_bin_path <FILEPATH>\n    --fuchsia_test_configuration <FILEPATH>"
            ),
            "{:?}",
            err
        );
    }

    #[test]
    fn test_args_only_required_params() {
        let temp_file = NamedTempFile::new().expect("Failed to create temporary file in the test");
        let temp_file_path = temp_file.path();

        let mut env = MockEnvironment::new();
        let cmd = CommandLineArgs {
            fuchsia_test_bin_path: temp_file_path.to_path_buf(),
            fuchsia_test_configuration: temp_file_path.to_path_buf(),
        };

        let parsed_args = TestPilotArgs::from_internal(&mut env, cmd).unwrap();

        // Check the individual arguments
        assert_eq!(parsed_args.targets, Vec::<String>::new());
        assert_eq!(parsed_args.timeout_seconds, None);
        assert_eq!(parsed_args.sdk_tools_path, None);
        assert_eq!(parsed_args.resource_path, None);
        assert_eq!(parsed_args.test_filter, None);
        assert_eq!(parsed_args.out_dir, None);
        assert_eq!(parsed_args.custom_test_args, None);
        assert_eq!(parsed_args.test_bin_path, temp_file_path.to_path_buf());
        assert_eq!(parsed_args.test_config, temp_file_path.to_path_buf());
        assert_eq!(parsed_args.extra_env_vars, vec![]);

        validate_env_cleared!(env);
    }

    // Test that extra env variables are not cleared.
    #[test]
    fn test_extra_env_vars() {
        let temp_file = NamedTempFile::new().expect("Failed to create temporary file in the test");
        let temp_file_path = temp_file.path();

        let mut env = MockEnvironment::new();
        let cmd = CommandLineArgs {
            fuchsia_test_bin_path: temp_file_path.to_path_buf(),
            fuchsia_test_configuration: temp_file_path.to_path_buf(),
        };

        env.set_var("EXTRA_VAR1", "some_str1");
        env.set_var("EXTRA_VAR2", "some_str2");

        let mut parsed_args = TestPilotArgs::from_internal(&mut env, cmd).unwrap();

        validate_env_cleared!(env);
        parsed_args.extra_env_vars.sort();
        let mut expected = vec![
            ("EXTRA_VAR1".to_string(), "some_str1".to_string()),
            ("EXTRA_VAR2".to_string(), "some_str2".to_string()),
        ];
        expected.sort();
        assert_eq!(parsed_args.extra_env_vars, expected);
    }

    #[test]
    fn test_args_invalid_timeout_seconds() {
        let temp_file = NamedTempFile::new().expect("Failed to create temporary file in the test");
        let temp_file_path = temp_file.path();

        let mut env = MockEnvironment::new();
        let cmd = CommandLineArgs {
            fuchsia_test_bin_path: temp_file_path.to_path_buf(),
            fuchsia_test_configuration: temp_file_path.to_path_buf(),
        };
        env.set_var(ENV_TIMEOUT_SECONDS, "abc");

        let parsed_args = TestPilotArgs::from_internal(&mut env, cmd);

        match parsed_args.unwrap_err() {
            TestPilotArgsError::Validation(_e, var) => {
                assert_eq!(var, ENV_TIMEOUT_SECONDS);
            }
            err => panic!("unexpected error: {}", err),
        }
    }

    #[test]
    fn test_args_parse_strict_mode() {
        let temp_file = NamedTempFile::new().expect("Failed to create temporary file in the test");
        let temp_file_path = temp_file.path();

        let mut env = MockEnvironment::new();
        let cmd = CommandLineArgs {
            fuchsia_test_bin_path: temp_file_path.to_path_buf(),
            fuchsia_test_configuration: temp_file_path.to_path_buf(),
        };
        env.set_var(ENV_STRICT_MODE, "0");

        let parsed_args = TestPilotArgs::from_internal(&mut env, cmd.clone()).unwrap();
        assert_eq!(parsed_args.strict_mode, false);

        env.set_var(ENV_STRICT_MODE, "1");
        let parsed_args = TestPilotArgs::from_internal(&mut env, cmd.clone()).unwrap();
        assert_eq!(parsed_args.strict_mode, true);
    }

    // Helper function to create a simple TestConfiguration for testing
    fn create_test_config_v1() -> TestConfigV1 {
        TestConfigV1 {
            tags: Vec::new(),
            requested_vars: RequestedVars::default(),
            execution: serde_json::json!({}),
        }
    }

    #[test]
    fn test_validate_success() {
        let args = TestPilotArgs {
            test_config: PathBuf::from("test_config.json"),
            test_bin_path: PathBuf::from("test_bin"),
            path: "/some/path".into(),
            targets: vec!["target1".to_string()],
            timeout_seconds: Some(30),
            sdk_tools_path: Some(PathBuf::from("sdk_tools")),
            resource_path: Some(PathBuf::from("resources")),
            test_filter: Some("test_filter".to_string()),
            out_dir: Some(PathBuf::from("out_dir")),
            custom_test_args: Some("extra_args".to_string()),
            strict_mode: true,
            extra_env_vars: vec![],
            debug: false,
        };

        let mut test_config = create_test_config_v1();
        test_config.requested_vars.known_vars = vec![ENV_SDK_TOOL_PATH.into(), ENV_TARGETS.into()];
        let test_config = test_config.into();
        let result = args.validate_config(&test_config);
        assert!(result.is_ok());

        let args = TestPilotArgs {
            test_config: PathBuf::from("test_config.json"),
            test_bin_path: PathBuf::from("test_bin"),
            path: "/some/path".into(),
            targets: vec![],
            timeout_seconds: Some(30),
            sdk_tools_path: None,
            resource_path: None,
            out_dir: None,
            test_filter: Some("test_filter".to_string()),
            custom_test_args: Some("extra_args".to_string()),
            strict_mode: true,
            extra_env_vars: vec![],
            debug: false,
        };

        let test_config = create_test_config_v1().into();
        let result = args.validate_config(&test_config);
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_missing_sdk_tools_path() {
        let args = TestPilotArgs {
            test_config: PathBuf::from("test_config.json"),
            test_bin_path: PathBuf::from("test_bin"),
            path: "/some/path".into(),
            targets: vec!["target1".to_string()],
            timeout_seconds: Some(30),
            sdk_tools_path: None, // Missing sdk_tools_path
            resource_path: Some(PathBuf::from("resources")),
            out_dir: None,
            test_filter: Some("test_filter".to_string()),
            custom_test_args: Some("extra_args".to_string()),
            strict_mode: true,
            extra_env_vars: vec![],
            debug: false,
        };

        let mut test_config = create_test_config_v1();
        test_config.requested_vars.known_vars = vec![ENV_SDK_TOOL_PATH.into()];
        let test_config = test_config.into();
        let result = args.validate_config(&test_config);
        assert_eq!(result.unwrap_err(), ConfigError::Required(ENV_SDK_TOOL_PATH));
    }

    #[test]
    fn test_validate_missing_targets() {
        let args = TestPilotArgs {
            test_config: PathBuf::from("test_config.json"),
            test_bin_path: PathBuf::from("test_bin"),
            path: "/some/path".into(),
            targets: Vec::new(), // Missing targets
            timeout_seconds: Some(30),
            sdk_tools_path: Some(PathBuf::from("sdk_tools")),
            resource_path: Some(PathBuf::from("resources")),
            test_filter: Some("test_filter".to_string()),
            out_dir: None,
            custom_test_args: Some("extra_args".to_string()),
            strict_mode: true,
            extra_env_vars: vec![],
            debug: false,
        };

        let mut test_config = create_test_config_v1();
        test_config.requested_vars.known_vars = vec![ENV_TARGETS.into()];
        let test_config = test_config.into();
        let result = args.validate_config(&test_config);
        assert_eq!(result.unwrap_err(), ConfigError::Required(ENV_TARGETS));
    }

    #[test]
    fn test_validate_missing_resources() {
        let args = TestPilotArgs {
            test_config: PathBuf::from("test_config.json"),
            test_bin_path: PathBuf::from("test_bin"),
            path: "/some/path".into(),
            targets: Vec::new(), // Missing targets
            timeout_seconds: Some(30),
            sdk_tools_path: Some(PathBuf::from("sdk_tools")),
            resource_path: None,
            test_filter: Some("test_filter".to_string()),
            out_dir: None,
            custom_test_args: Some("extra_args".to_string()),
            strict_mode: true,
            extra_env_vars: vec![],
            debug: false,
        };

        let mut test_config = create_test_config_v1();
        test_config.requested_vars.known_vars = vec![ENV_RESOURCE_PATH.into()];
        let test_config = test_config.into();
        let result = args.validate_config(&test_config);
        assert_eq!(result.unwrap_err(), ConfigError::Required(ENV_RESOURCE_PATH));
    }
}
