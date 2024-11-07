// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{ensure, Error};
use heck::{ShoutySnakeCase, SnakeCase};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, VecDeque};
use std::fs;
use std::fs::File;
use std::io::BufReader;
use std::path::PathBuf;
use thiserror::Error as ThisError;

pub const DEBUG_ARG: &str = "--debug";
pub const DEBUG_OPTION: &str = "debug";
pub const ENV_OPTION: &str = "env";
pub const INCLUDE_OPTION: &str = "include";
pub const REQUIRE_OPTION: &str = "require";
pub const PROHIBIT_OPTION: &str = "prohibit";
pub const HOST_TEST_BINARY_OPTION: &str = "host_test_binary";
pub const OUTPUT_DIRECTORY_OPTION: &str = "output_directory";
pub const VAR_PREFIX: &str = "FUCHSIA_";
pub const NO_OPTION_PREFIX: &str = "no_";

/// Command-line arguments, including both command-line-only arguments and TestParams, which
/// integrates command-line arguments, environment variables, and JSON streams.
#[derive(Default, Debug, PartialEq)]
pub struct CommandLineArgs {
    /// Indicates which environment variables should be merged into test_params.
    pub env: EnvArg,

    /// Parameters describing a test to be run.
    pub test_params: TestParams,
}

/// Models the program's command line arguments and the test arguments they imply. Referenced
/// environment variables and JSON includes are integrated.
impl CommandLineArgs {
    /// Creates a new `CommandLineArgs` from `std::env`.
    pub fn from_env() -> Result<Self, UsageError> {
        let env_like = ActualEnv {};
        if env_like.args().any(|a| a == DEBUG_ARG) {
            Self::from_env_like(&env_like, &StderrLogger {})
        } else {
            Self::from_env_like(&env_like, &NullLogger {})
        }
    }

    /// Validates `self`.
    pub fn validate(&self) -> Result<(), UsageError> {
        self.test_params.validate()
    }

    /// Creates a new `CommandLineArgs` from an `EnvLike`.
    fn from_env_like<E: EnvLike, L: Logger>(env_like: &E, logger: &L) -> Result<Self, UsageError> {
        logger.prologue();
        logger.start_command_line();
        let mut to_return = Self::from_arg_iter(env_like.args(), &*logger)?;
        logger.start_environment_variables();
        to_return.process_vars(env_like, &*logger)?;
        to_return.test_params.process_includes(&*logger)?;
        logger.epilogue();
        Ok(to_return)
    }

    /// Create a new `CommandLineArgs` from an iterator over argument strings.
    fn from_arg_iter<L: Logger>(
        args: impl Iterator<Item = String>,
        logger: &L,
    ) -> Result<Self, UsageError> {
        let mut to_return: CommandLineArgs = Default::default();
        for arg in args {
            if let Some(stripped_arg) = arg.strip_prefix("--") {
                if let Some((name, value)) = stripped_arg.split_once('=') {
                    let name = arg_name_to_json_name(name);
                    match name.as_str() {
                        DEBUG_OPTION => {
                            logger.unexpected_value(&name, &value);
                            return Err(UsageError::UnexpectedValue(
                                String::from(name),
                                String::from(value),
                            ));
                        }
                        ENV_OPTION => {
                            logger.add_some_to_env(&value);
                            to_return.env = to_return.env.add(value.split(','));
                        }
                        _ => {
                            to_return.test_params.add_name_value(
                                &name,
                                string_to_value(value),
                                logger,
                            )?;
                        }
                    }
                } else {
                    let name = arg_name_to_json_name(stripped_arg);
                    match name.as_str() {
                        DEBUG_OPTION => {
                            logger.debug_option();
                        }
                        ENV_OPTION => {
                            logger.add_all_to_env();
                            to_return.env = EnvArg::All;
                        }
                        _ => {
                            to_return.test_params.add_simple_arg(&name, logger)?;
                        }
                    }
                }
            } else {
                return Err(UsageError::UnexpectedPositionalArgument(arg));
            }
        }

        Ok(to_return)
    }

    /// Updates a `CommandLineArgs` based on environment variables access via an `EnvLike`.
    fn process_vars<E: EnvLike, L: Logger>(
        &mut self,
        env_like: &E,
        logger: &L,
    ) -> Result<(), UsageError> {
        match &self.env {
            EnvArg::None => {}
            EnvArg::All => {
                for (name, value) in env_like.vars() {
                    if is_valid_var_name(name.as_ref()) {
                        self.test_params.add_name_value(
                            var_name_to_json_name(name.as_ref()).as_ref(),
                            string_to_value(value.as_ref()),
                            logger,
                        )?;
                    }
                }
            }
            EnvArg::Some(names) => {
                for name in names {
                    match env_like.var(json_name_to_var_name(name).as_ref()) {
                        Ok(value) => {
                            self.test_params.add_name_value(
                                name,
                                string_to_value(value.as_ref()),
                                logger,
                            )?;
                        }
                        Err(_) => {
                            // The value is not present, and we do nothing.
                        }
                    }
                }
            }
        }

        Ok(())
    }
}

/// Parameters describing a test to be run.
#[derive(Serialize, Deserialize, Default, PartialEq, Debug)]
pub struct TestParams {
    /// JSON files that should be included.
    #[serde(skip)]
    pub include: Vec<PathBuf>,

    /// Parameters that must be defined in order for the test to run.
    #[serde(skip)]
    pub require: Vec<String>,

    /// Parameters that must NOT be defined.
    #[serde(skip)]
    pub prohibit: Vec<String>,

    /// Path for the executable that this program should invoke to run the test.
    pub host_test_binary: PathBuf,

    /// Path for test output.
    pub output_directory: PathBuf,

    /// Pass-through test parameters that are not known to this tool.
    #[serde(flatten)]
    pub unknown: HashMap<String, Value>,

    /// JSON files that should be included but which have not been processed. Every member of this
    /// collection also appears in `include`.
    #[serde(skip)]
    pub unprocessed_includes: VecDeque<PathBuf>,
}

/// Models test parameters, including parameters known to this program and unknown parameters.
impl TestParams {
    /// Validates `self`.
    fn validate(&self) -> Result<(), UsageError> {
        assert!(self.unprocessed_includes.is_empty(), "TestParams contains unprocessed includes.");

        if self.host_test_binary == PathBuf::new() {
            return Err(UsageError::MissingRequiredParameter(String::from(
                HOST_TEST_BINARY_OPTION,
            )));
        }

        if self.output_directory == PathBuf::new() {
            return Err(UsageError::MissingRequiredParameter(String::from(
                OUTPUT_DIRECTORY_OPTION,
            )));
        }

        for required in &self.require {
            if !self.unknown.contains_key(required) {
                return Err(UsageError::MissingRequiredParameter(required.clone()));
            }
        }

        for prohibited in &self.prohibit {
            if self.unknown.contains_key(prohibited) {
                return Err(UsageError::DefinedProhibitedParameter(prohibited.clone()));
            }
        }

        Ok(())
    }

    /// Adds a name with no explicit value as occurs in command line arguments (--foo). The name
    /// must be in lower_snake_case.
    fn add_simple_arg<L: Logger>(&mut self, name: &str, logger: &L) -> Result<(), UsageError> {
        assert_eq!(name, name.to_snake_case());
        match name {
            ENV_OPTION | DEBUG_OPTION => {}

            INCLUDE_OPTION
            | REQUIRE_OPTION
            | PROHIBIT_OPTION
            | HOST_TEST_BINARY_OPTION
            | OUTPUT_DIRECTORY_OPTION => {
                logger.missing_value(&name);
                return Err(UsageError::MissingValue(String::from(name)));
            }

            _ => {
                if name.starts_with(NO_OPTION_PREFIX) {
                    let _ = self.add_unknown(
                        name.strip_prefix(NO_OPTION_PREFIX).unwrap(),
                        Value::Bool(false),
                        logger,
                    );
                } else {
                    let _ = self.add_unknown(name, Value::Bool(true), logger);
                }
            }
        };
        Ok(())
    }

    /// Adds a name with an explicit value. The name must be in lower_snake_case.
    fn add_name_value<L: Logger>(
        &mut self,
        name: &str,
        value: Value,
        logger: &L,
    ) -> Result<(), UsageError> {
        assert_eq!(name, name.to_snake_case());
        match name {
            ENV_OPTION => Ok(()),

            INCLUDE_OPTION => Self::add_array_or_string(
                INCLUDE_OPTION,
                value,
                |s| {
                    self.add_include(
                        parse_existent_file_path(s.as_str())
                            .map_err(|e| UsageError::InvalidPath(e.to_string()))?,
                        logger,
                    );
                    Ok(())
                },
                logger,
            ),

            REQUIRE_OPTION => Self::add_array_or_string(
                REQUIRE_OPTION,
                value,
                |s| self.add_require(s, logger),
                logger,
            ),

            PROHIBIT_OPTION => Self::add_array_or_string(
                PROHIBIT_OPTION,
                value,
                |s| self.add_prohibit(s, logger),
                logger,
            ),

            HOST_TEST_BINARY_OPTION => {
                if let Value::String(string) = value {
                    self.host_test_binary =
                        parse_existent_file_path(string.as_str()).map_err(|e| {
                            logger.invalid_path(&name, &string);
                            UsageError::InvalidPath(e.to_string())
                        })?;
                    logger.set_host_test_binary(&string);
                    Ok(())
                } else {
                    logger.expected_string(&name, &value);
                    Err(UsageError::ExpectedString(String::from(HOST_TEST_BINARY_OPTION), value))
                }
            }

            OUTPUT_DIRECTORY_OPTION => {
                if let Value::String(string) = value {
                    self.output_directory = parse_dir_path(string.as_str()).map_err(|e| {
                        logger.invalid_path(&name, &string);
                        UsageError::InvalidPath(e.to_string())
                    })?;
                    logger.set_output_directory(&string);
                    Ok(())
                } else {
                    logger.expected_string(&name, &value);
                    Err(UsageError::ExpectedString(String::from(OUTPUT_DIRECTORY_OPTION), value))
                }
            }

            _ => self.add_unknown(name, value, logger),
        }
    }

    /// Adds an array of strings or a single string from a `Value::Array` or `Value::String` by
    /// calling a supplied function.
    fn add_array_or_string<F, L: Logger>(
        name: &str,
        value: Value,
        mut add: F,
        logger: &L,
    ) -> Result<(), UsageError>
    where
        F: FnMut(String) -> Result<(), UsageError>,
    {
        match value {
            Value::Array(values) => {
                for value in values {
                    if let Value::String(string) = value {
                        add(string)?;
                    } else {
                        logger.expected_string(&name, &value);
                        return Err(UsageError::ExpectedString(String::from(name), value));
                    }
                }
            }
            Value::String(string) => {
                add(string)?;
            }
            _ => {
                logger.expected_array_or_string(&name, &value);
                return Err(UsageError::ExpectedArrayOrString(String::from(name), value));
            }
        };

        Ok(())
    }

    /// Add an unknown parameter.
    fn add_unknown<L: Logger>(
        &mut self,
        name: &str,
        value: Value,
        logger: &L,
    ) -> Result<(), UsageError> {
        if !self.unknown.contains_key(name) {
            logger.add_unknown(&name, &value);
            let _ = self.unknown.insert(String::from(name), value);
        } else {
            logger.unknown_already_added(&name, &value);
        }
        Ok(())
    }

    /// Process JSON files referenced by arguments and environment variables.
    fn process_includes<L: Logger>(&mut self, logger: &L) -> Result<(), UsageError> {
        while let Some(include) = self.unprocessed_includes.pop_front() {
            logger.start_include(&include);
            self.process_include(&include, logger)?;
        }
        Ok(())
    }

    /// Process a single JSON file. If new includes are encountered, they will get pushed to the
    /// back of `unprocessed_includes` and processed later.
    fn process_include<L: Logger>(&mut self, path: &PathBuf, logger: &L) -> Result<(), UsageError> {
        let file = File::open(path).map_err(|e| {
            logger.failed_to_open(&path);
            UsageError::FailedToOpenInclude(path.clone(), e.to_string())
        })?;
        let reader = BufReader::new(file);
        let value: Value = serde_json::from_reader(reader).map_err(|e| {
            logger.failed_to_parse_include(&path);
            UsageError::FailedToParseInclude(path.clone(), e.to_string())
        })?;

        let top_map = match value {
            Value::Object(obj) => obj,
            _ => {
                logger.include_is_not_object(&path);
                return Err(UsageError::IncludeIsNotObject(path.clone()));
            }
        };

        for (name, value) in top_map {
            let _ = self.add_name_value(name.as_str(), value, logger)?;
        }

        Ok(())
    }

    /// Adds an include path if it wasn't added previously.
    fn add_include<L: Logger>(&mut self, include: PathBuf, logger: &L) {
        if !self.include.contains(&include) {
            logger.add_include(&include);
            self.include.push(include.clone());
            self.unprocessed_includes.push_back(include);
        } else {
            logger.include_already_added(&include);
        }
    }

    /// Adds a required parameter if it wasn't added previously.
    fn add_require<L: Logger>(&mut self, require: String, logger: &L) -> Result<(), UsageError> {
        let require = require.to_snake_case();
        match require.as_str() {
            INCLUDE_OPTION
            | REQUIRE_OPTION
            | PROHIBIT_OPTION
            | HOST_TEST_BINARY_OPTION
            | OUTPUT_DIRECTORY_OPTION => {
                logger.require_value_not_allowed(&require);
                return Err(UsageError::ValueNotAllowed(require, String::from(REQUIRE_OPTION)));
            }
            _ => {}
        }

        if !self.require.contains(&require) {
            logger.add_require(&require);
            self.require.push(require);
        } else {
            logger.require_already_added(&require);
        }

        Ok(())
    }

    /// Adds a required parameter if it wasn't added previously.
    fn add_prohibit<L: Logger>(&mut self, prohibit: String, logger: &L) -> Result<(), UsageError> {
        let prohibit = prohibit.to_snake_case();
        match prohibit.as_str() {
            INCLUDE_OPTION
            | REQUIRE_OPTION
            | PROHIBIT_OPTION
            | HOST_TEST_BINARY_OPTION
            | OUTPUT_DIRECTORY_OPTION => {
                logger.prohibit_value_not_allowed(&prohibit);
                return Err(UsageError::ValueNotAllowed(prohibit, String::from(PROHIBIT_OPTION)));
            }
            _ => {}
        }

        if !self.prohibit.contains(&prohibit) {
            logger.add_prohibit(&prohibit);
            self.prohibit.push(prohibit);
        } else {
            logger.prohibit_already_added(&prohibit);
        }

        Ok(())
    }
}

/// Represents a set of environment variables as expressed in --env arguments.
#[derive(Default, Debug, PartialEq)]
pub enum EnvArg {
    /// Indicates an empty set.
    #[default]
    None,

    /// Indicates all 'valid' environment variables in std::env. An environment variable name
    /// is considered valid if it has a 'FUCHSIA_' prefix and is in SHOUTY_SNAKE_CASE.
    All,

    /// Specific environment variables identified by name in command line argument format
    /// (snake_case without the 'FUCHSIA_' prefix). Always sorted in ascending alphabetical order.
    Some(Vec<String>),
}

impl EnvArg {
    /// Returns a `EnvArg::Some` with the specified variable names. `names` may contain
    /// duplicates and may be in any order.
    pub fn some(mut names: Vec<String>) -> Self {
        names.sort();
        names.dedup();
        EnvArg::Some(names)
    }

    /// Returns an `EnvArg` that merges self and the names in `to_add`. `to_add` may contain
    /// duplicates and may be in any order.
    pub fn add<'a>(self, to_add: impl IntoIterator<Item = &'a str>) -> Self {
        match self {
            EnvArg::None => Self::some(to_add.into_iter().map(|s| String::from(s)).collect()),
            EnvArg::All => self,
            EnvArg::Some(mut self_args) => {
                for a in to_add {
                    self_args.push(String::from(a));
                }
                Self::some(self_args)
            }
        }
    }
}

// Wrapper to allow faking of std::env.
trait EnvLike {
    /// Returns an iterator over the program's arguments. Unlike `std::env::args``, this method
    /// filters non-unicode arguments rather than panicking. It also omits the first item returned
    /// by `std::env::args`` (the program name).
    fn args(&self) -> impl Iterator<Item = String>;

    /// Returns an iterator over the process's environment variables and their respective values.
    /// Unlike `std::env::vars``, this method filters non-unicode names and values rather than
    /// panicking.
    fn vars(&self) -> impl Iterator<Item = (String, String)>;

    /// Returns the value of the environment variable identified by `key`. This is a wrapper for
    /// `std::env::var` and has the same behvior.
    fn var(&self, key: &str) -> Result<String, std::env::VarError>;
}

/// Implements `EnvLike` using `std::env`.
struct ActualEnv;

impl EnvLike for ActualEnv {
    fn args(&self) -> impl Iterator<Item = String> {
        std::env::args_os().skip(1).filter_map(|os_string| os_string.into_string().ok())
    }

    fn vars(&self) -> impl Iterator<Item = (String, String)> {
        std::env::vars_os().filter_map(|(os_name, os_value)| {
            if let Ok(name) = os_name.into_string() {
                if let Ok(value) = os_value.into_string() {
                    Some((name, value))
                } else {
                    None
                }
            } else {
                None
            }
        })
    }

    fn var(&self, key: &str) -> Result<String, std::env::VarError> {
        std::env::var(key)
    }
}

trait Logger {
    fn prologue(&self);

    fn epilogue(&self);

    fn start_command_line(&self);

    fn start_environment_variables(&self);

    fn start_include(&self, path: &PathBuf);

    fn debug_option(&self);

    fn add_some_to_env(&self, value: &str);

    fn add_all_to_env(&self);

    fn missing_value(&self, name: &str);

    fn unexpected_value(&self, name: &str, value: &str);

    fn add_require(&self, value: &str);

    fn require_already_added(&self, value: &str);

    fn require_value_not_allowed(&self, value: &str);

    fn add_prohibit(&self, value: &str);

    fn prohibit_already_added(&self, value: &str);

    fn prohibit_value_not_allowed(&self, value: &str);

    fn set_host_test_binary(&self, value: &str);

    fn set_output_directory(&self, value: &str);

    fn invalid_path(&self, name: &str, value: &str);

    fn expected_string(&self, name: &str, value: &Value);

    fn expected_array_or_string(&self, name: &str, value: &Value);

    fn add_unknown(&self, name: &str, value: &Value);

    fn unknown_already_added(&self, name: &str, value: &Value);

    fn add_include(&self, path: &PathBuf);

    fn failed_to_open(&self, path: &PathBuf);

    fn failed_to_parse_include(&self, path: &PathBuf);

    fn include_is_not_object(&self, path: &PathBuf);

    fn include_already_added(&self, path: &PathBuf);
}

struct NullLogger {}

impl Logger for NullLogger {
    fn prologue(&self) {}

    fn epilogue(&self) {}

    fn start_command_line(&self) {}

    fn start_environment_variables(&self) {}

    fn start_include(&self, _path: &PathBuf) {}

    fn debug_option(&self) {}

    fn add_some_to_env(&self, _value: &str) {}

    fn add_all_to_env(&self) {}

    fn missing_value(&self, _name: &str) {}

    fn unexpected_value(&self, _name: &str, _value: &str) {}

    fn add_require(&self, _value: &str) {}

    fn require_already_added(&self, _value: &str) {}

    fn require_value_not_allowed(&self, _value: &str) {}

    fn add_prohibit(&self, _value: &str) {}

    fn prohibit_already_added(&self, _value: &str) {}

    fn prohibit_value_not_allowed(&self, _value: &str) {}

    fn set_host_test_binary(&self, _value: &str) {}

    fn set_output_directory(&self, _value: &str) {}

    fn invalid_path(&self, _name: &str, _value: &str) {}

    fn expected_string(&self, _name: &str, _value: &Value) {}

    fn expected_array_or_string(&self, _name: &str, _value: &Value) {}

    fn add_unknown(&self, _name: &str, _value: &Value) {}

    fn unknown_already_added(&self, _name: &str, _value: &Value) {}

    fn add_include(&self, _path: &PathBuf) {}

    fn failed_to_open(&self, _path: &PathBuf) {}

    fn failed_to_parse_include(&self, _path: &PathBuf) {}

    fn include_is_not_object(&self, _path: &PathBuf) {}

    fn include_already_added(&self, _path: &PathBuf) {}
}
struct StderrLogger {}

impl StderrLogger {}

impl Logger for StderrLogger {
    fn prologue(&self) {
        eprintln!("\nBEGIN PARAMETER DEBUG FROM test_pilot");
    }

    fn epilogue(&self) {
        eprintln!("END PARAMETER DEBUG FROM test_pilot\n");
    }

    fn start_command_line(&self) {
        eprintln!("from command line:");
    }

    fn start_environment_variables(&self) {
        eprintln!("from environment variables:");
    }

    fn start_include(&self, path: &PathBuf) {
        eprintln!("from {}", path.to_str().unwrap());
    }

    fn debug_option(&self) {
        eprintln!("    debug");
    }

    fn add_some_to_env(&self, value: &str) {
        eprintln!("    env = {}", value);
    }

    fn add_all_to_env(&self) {
        eprintln!("    env (process all qualified environment variables)");
    }

    fn missing_value(&self, name: &str) {
        eprintln!("    ERROR: value missing from parameter: {}", name);
    }

    fn unexpected_value(&self, name: &str, value: &str) {
        eprintln!("    ERROR: parameter {} has unexpected value {}", name, value);
    }

    fn add_require(&self, value: &str) {
        eprintln!("    require {}", value);
    }

    fn require_already_added(&self, value: &str) {
        eprintln!("    ignoring redundant require {}", value);
    }

    fn require_value_not_allowed(&self, value: &str) {
        eprintln!("    ERROR: invalid value for 'require': {}", value);
    }

    fn add_prohibit(&self, value: &str) {
        eprintln!("    require {}", value);
    }

    fn prohibit_already_added(&self, value: &str) {
        eprintln!("    ERROR: prohibited parameter already added, ignoring: {}", value);
    }

    fn prohibit_value_not_allowed(&self, value: &str) {
        eprintln!("    ERROR: invalid value for 'prohibit': {}", value);
    }

    fn set_host_test_binary(&self, value: &str) {
        eprintln!("    {} = {}", HOST_TEST_BINARY_OPTION, value);
    }

    fn set_output_directory(&self, value: &str) {
        eprintln!("    {} = {}", OUTPUT_DIRECTORY_OPTION, value);
    }

    fn invalid_path(&self, name: &str, value: &str) {
        eprintln!("    ERROR: invalid {} path: {}", name, value);
    }

    fn expected_string(&self, name: &str, value: &Value) {
        eprintln!("    ERROR: string value expected for {}, got: {:?}", name, value);
    }

    fn expected_array_or_string(&self, name: &str, value: &Value) {
        eprintln!("    ERROR: string or array value expected for {}, got: {:?}", name, value);
    }

    fn add_unknown(&self, name: &str, value: &Value) {
        eprintln!("    unkonwn {} = {:?}", name, value);
    }

    fn unknown_already_added(&self, name: &str, value: &Value) {
        eprintln!("    ignoring already defined {} = {:?}", name, value);
    }

    fn add_include(&self, path: &PathBuf) {
        eprintln!("    include {}", path.to_str().unwrap());
    }

    fn failed_to_open(&self, path: &PathBuf) {
        eprintln!("ERROR: failed to open include: {}", path.to_str().unwrap());
    }

    fn failed_to_parse_include(&self, path: &PathBuf) {
        eprintln!("ERROR: failed to parse include: {}", path.to_str().unwrap());
    }

    fn include_is_not_object(&self, path: &PathBuf) {
        eprintln!(
            "ERROR: include file {} does not contain a top-level JSON object",
            path.to_str().unwrap()
        );
    }

    fn include_already_added(&self, path: &PathBuf) {
        eprintln!("    ignoring (already added) include {}", path.to_str().unwrap());
    }
}

/// Parse a directory path, checking that the path does not reference an existing file.
/// Returns an Ok value whether the directory exists or not.
fn parse_dir_path(path: &str) -> Result<PathBuf, Error> {
    let path = PathBuf::from(path);
    if path.exists() {
        let metadata = fs::metadata(&path)?;
        ensure!(metadata.is_dir(), "{:?} exists but is not a directory.", path);
    }
    Ok(path)
}

/// Parse a file path, checking that the file exists and is a file.
fn parse_existent_file_path(path: &str) -> Result<PathBuf, Error> {
    let path = PathBuf::from(path);
    ensure!(path.exists(), "{:?} does not exist.", path);
    let metadata = fs::metadata(&path)?;
    ensure!(metadata.is_file(), "{:?} exists but is not a file.", path);
    Ok(path)
}

/// Interprets a value from command line arguments or environment variables as a serde_json `Value`.
/// If `string` contains no commas, this function just defers to `string_to_value_no_commas`.
/// Otherwise, it returns a `Value::Array` that maps each comma-separated part of `string` using
/// `string_to_value_no_commas`.
fn string_to_value(string: &str) -> Value {
    let mut strings = string.split(',');
    let first_string = strings.next().unwrap();
    if let Some(second_string) = strings.next() {
        Value::Array(
            std::iter::once(first_string)
                .chain(std::iter::once(second_string))
                .chain(strings)
                .map(|s| string_to_value_no_commas(s))
                .collect(),
        )
    } else {
        string_to_value_no_commas(first_string)
    }
}

/// Interprets a value from command line arguments or environment variables as a serde_json `Value`
/// assuming it contains no commas. Conversion rules are as follows:
/// 1. "true" and "false" yield 'Value::Bool(true)' and 'Value::Bool(false)', respectively.
/// 2. Numeric strings yield `Value::Number(...)`. Floats are allowed.
/// 3. Other strings yield `Value::String(...)`.
fn string_to_value_no_commas(string: &str) -> Value {
    match string {
        "true" => Value::Bool(true),
        "false" => Value::Bool(false),
        _ => {
            if let Ok(number) = std::str::FromStr::from_str(string) {
                Value::Number(number)
            } else {
                Value::String(String::from(string))
            }
        }
    }
}

/// Determines whether `name` is a 'valid' environment variable name. 'Valid' variable names
/// start with 'FUCHSIA_' and are in SHOUTY_SNAKE_CASE.
fn is_valid_var_name(name: &str) -> bool {
    name.starts_with(VAR_PREFIX) && (name.to_shouty_snake_case() == name)
}

/// Converts a command line argument name to the equivalent JSON property name. Specifically,
/// the result is converted to lower-snake-case.
fn arg_name_to_json_name(arg_name: &str) -> String {
    arg_name.to_snake_case()
}

/// Converts a valid environment variable name to the equivalent JSON property name. Specifically,
/// the 'FUCHSIA_' prefix is stripped, and the result is converted to lower-snake-case.
fn var_name_to_json_name(var_name: &str) -> String {
    assert!(is_valid_var_name(var_name));
    var_name.strip_prefix(VAR_PREFIX).unwrap().to_snake_case()
}

/// Converts a valid JSON property name into the equivalent environment variable name.
/// Specifically, it converts the name to SHOUTY_SNAKE_CASE and adds a 'FUCHSIA_' prefix.
fn json_name_to_var_name(arg_name: &str) -> String {
    let mut result = String::from(VAR_PREFIX);
    result.push_str(arg_name.to_shouty_snake_case().as_ref());
    result
}

/// Error encountered validating config
#[derive(Debug, ThisError, Eq, PartialEq)]
pub enum UsageError {
    #[error("Unexpected positional argument {0}")]
    UnexpectedPositionalArgument(String),

    #[error("Argument {0} missing required value")]
    MissingValue(String),

    #[error("Argument {0} has unexpected value {1}")]
    UnexpectedValue(String, String),

    #[error("Invalid path: {0}")]
    InvalidPath(String),

    #[error("Include file {0} could not be opened for reading: {1}")]
    FailedToOpenInclude(PathBuf, String),

    #[error("Failure attemptying to parse include {0}: {1}")]
    FailedToParseInclude(PathBuf, String),

    #[error("Parsed include {0} is not a JSON object at top level")]
    IncludeIsNotObject(PathBuf),

    #[error("Expected string in {0}, got {1}")]
    ExpectedString(String, Value),

    #[error("Expected array or string in {0}, got {1}")]
    ExpectedArrayOrString(String, Value),

    #[error("Parameter {0} is required, but was not defined")]
    MissingRequiredParameter(String),

    #[error("Parameter {0} is prohibited, but was defined")]
    DefinedProhibitedParameter(String),

    #[error("Value {0} is not allowed for parameter {1}")]
    ValueNotAllowed(String, String),
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::*;
    use serde_json::Number;
    use tempfile::{tempdir, NamedTempFile};

    struct FakeEnv {
        args: Vec<String>,
        vars: HashMap<String, String>,
    }

    impl FakeEnv {
        /// Creates a `FakeEnv` from a space-separated string of arguments and a space-separated
        /// string of variable assignments.
        pub fn from(args: &str, vars: &str) -> Self {
            Self {
                args: args.split_ascii_whitespace().map(|str| String::from(str)).collect(),
                vars: make_hashmap(vars),
            }
        }
    }

    impl EnvLike for FakeEnv {
        fn args(&self) -> impl Iterator<Item = String> {
            self.args.clone().into_iter()
        }

        fn vars(&self) -> impl Iterator<Item = (String, String)> {
            self.vars.clone().into_iter()
        }

        fn var(&self, key: &str) -> Result<String, std::env::VarError> {
            self.vars.get(key).ok_or(std::env::VarError::NotPresent).cloned()
        }
    }

    // Makes a hashmap of strings from a space-separated string of variable assignments.
    fn make_hashmap(from: &str) -> HashMap<String, String> {
        from.split_ascii_whitespace()
            .map(|assignment| {
                let strs = assignment.split_once("=").unwrap();
                (String::from(strs.0), String::from(strs.1))
            })
            .collect::<HashMap<String, String>>()
    }

    #[test]
    fn test_parse_dir_path() {
        // Existent directory.
        let temp_dir = tempdir().expect("Failed to create temporary directory");
        let temp_dir_path = temp_dir.path();
        assert!(parse_dir_path(temp_dir_path.to_str().unwrap()).is_ok());
        temp_dir.close().expect("Failed to close temporary directory");

        // Non-existent directory.
        assert!(parse_dir_path("/non_existent_dir").is_ok());

        // Existent file.
        let temp_file = NamedTempFile::new().expect("Failed to create temporary file");
        let temp_file_path = temp_file.path().to_str().unwrap().to_string();
        let result = parse_dir_path(temp_file_path.as_str());
        assert!(result.is_err());
        assert_eq!(
            result.unwrap_err().to_string(),
            format!("\"{}\" exists but is not a directory.", temp_file_path.as_str())
        );
        temp_file.close().expect("Failed to close temporary file");
    }

    #[test]
    fn test_parse_existent_file_path() {
        // Existent file.
        let temp_file = NamedTempFile::new().expect("Failed to create temporary file");
        let temp_file_path = temp_file.path().to_str().unwrap().to_string();
        assert!(parse_existent_file_path(temp_file_path.as_str()).is_ok());
        temp_file.close().expect("Failed to close temporary file");

        // Non-existent file.
        let result = parse_existent_file_path("/non_existent_file");
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().to_string(), "\"/non_existent_file\" does not exist.");

        // Existent directory.
        let temp_dir = tempdir().expect("Failed to create temporary directory");
        let temp_dir_path = temp_dir.path();
        let result = parse_existent_file_path(temp_dir_path.to_str().unwrap());
        assert!(result.is_err());
        assert_eq!(
            result.unwrap_err().to_string(),
            format!("\"{}\" exists but is not a file.", temp_dir_path.to_str().unwrap())
        );
        temp_dir.close().expect("Failed to close temporary directory");
    }

    #[test]
    fn test_string_to_value() {
        assert_eq!(string_to_value("true"), Value::Bool(true));
        assert_eq!(string_to_value("false"), Value::Bool(false));
        assert_eq!(string_to_value("0").as_number().unwrap().as_i64(), Some(0));
        assert_eq!(string_to_value("-1").as_number().unwrap().as_i64(), Some(-1));
        assert_eq!(string_to_value("1").as_number().unwrap().as_i64(), Some(1));
        assert_eq!(string_to_value("0.1").as_number().unwrap().as_f64(), Some(0.1));
        assert_eq!(string_to_value("-0.1").as_number().unwrap().as_f64(), Some(-0.1));
        assert_eq!(string_to_value("snaggle").as_str(), Some("snaggle"));
        assert_eq!(
            string_to_value("1,2,3,4").as_array(),
            Some(vec![
                Value::Number(Number::from(1)),
                Value::Number(Number::from(2)),
                Value::Number(Number::from(3)),
                Value::Number(Number::from(4))
            ])
            .as_ref()
        );
    }

    #[test]
    fn test_env_arg() {
        let env_arg = EnvArg::some(vec![String::from("e"), String::from("a"), String::from("c")]);
        assert_eq!(
            env_arg,
            EnvArg::Some(vec![String::from("a"), String::from("c"), String::from("e")])
        );

        let env_arg = EnvArg::some(vec![String::from("e"), String::from("a"), String::from("c")]);
        let env_arg = env_arg.add(vec!["d", "b"]);
        assert_eq!(
            env_arg,
            EnvArg::Some(vec![
                String::from("a"),
                String::from("b"),
                String::from("c"),
                String::from("d"),
                String::from("e")
            ])
        );

        let env_arg = EnvArg::None;
        let env_arg = env_arg.add(vec!["d", "b"]);
        assert_eq!(env_arg, EnvArg::Some(vec![String::from("b"), String::from("d")]));

        let env_arg = EnvArg::All;
        let env_arg = env_arg.add(vec!["d", "b"]);
        assert_eq!(env_arg, EnvArg::All);
    }

    #[test]
    // Tests the case in which no arguments are supplied.
    fn test_command_line_args_no_args() {
        let fake_env = FakeEnv::from("", "");

        let under_test = CommandLineArgs::from_env_like(&fake_env, &NullLogger {});
        assert!(under_test.is_ok());
        assert_eq!(
            under_test.unwrap(),
            CommandLineArgs { env: EnvArg::None, test_params: TestParams { ..Default::default() } }
        );
    }

    #[test]
    // Tests the case in which an unexpected positional argument is provided.
    fn test_command_line_args_unexpected_positional_arg() {
        let fake_env = FakeEnv::from("foo", "");

        let under_test = CommandLineArgs::from_env_like(&fake_env, &NullLogger {});
        assert_eq!(under_test, Err(UsageError::UnexpectedPositionalArgument(String::from("foo"))));
    }

    #[test]
    // Tests processing of all environment variables.
    fn test_command_line_args_env_all() {
        let fake_env = FakeEnv::from("--env", "FUCHSIA_A=b FUCHSIA_C=d FUCHSIA_E=f");

        let under_test = CommandLineArgs::from_env_like(&fake_env, &NullLogger {});
        assert_eq!(
            under_test,
            Ok(CommandLineArgs {
                env: EnvArg::All,
                test_params: TestParams {
                    unknown: HashMap::from([
                        (String::from("a"), Value::String(String::from("b"))),
                        (String::from("c"), Value::String(String::from("d"))),
                        (String::from("e"), Value::String(String::from("f"))),
                    ]),
                    ..Default::default()
                }
            })
        );
    }

    #[test]
    // Tests processing of specified environment variables.
    fn test_command_line_args_env_some() {
        let fake_env = FakeEnv::from("--env=a,d,e", "FUCHSIA_A=b FUCHSIA_C=d FUCHSIA_E=f");

        let under_test = CommandLineArgs::from_env_like(&fake_env, &NullLogger {});
        assert_eq!(
            under_test,
            Ok(CommandLineArgs {
                env: EnvArg::Some(vec![String::from("a"), String::from("d"), String::from("e")]),
                test_params: TestParams {
                    unknown: HashMap::from([
                        (String::from("a"), Value::String(String::from("b"))),
                        (String::from("e"), Value::String(String::from("f"))),
                    ]),
                    ..Default::default()
                }
            })
        );
    }

    #[test]
    // Tests cases in which values are missing for known parameters that require them.
    fn test_command_line_args_missing_values() {
        let fake_env = FakeEnv::from("--include", "");
        let under_test = CommandLineArgs::from_env_like(&fake_env, &NullLogger {});
        assert_eq!(under_test, Err(UsageError::MissingValue(String::from("include"))));

        let fake_env = FakeEnv::from("--require", "");
        let under_test = CommandLineArgs::from_env_like(&fake_env, &NullLogger {});
        assert_eq!(under_test, Err(UsageError::MissingValue(String::from("require"))));

        let fake_env = FakeEnv::from("--prohibit", "");
        let under_test = CommandLineArgs::from_env_like(&fake_env, &NullLogger {});
        assert_eq!(under_test, Err(UsageError::MissingValue(String::from("prohibit"))));

        let fake_env = FakeEnv::from("--host-test-binary", "");
        let under_test = CommandLineArgs::from_env_like(&fake_env, &NullLogger {});
        assert_eq!(under_test, Err(UsageError::MissingValue(String::from("host_test_binary"))));

        let fake_env = FakeEnv::from("--output-directory", "");
        let under_test = CommandLineArgs::from_env_like(&fake_env, &NullLogger {});
        assert_eq!(under_test, Err(UsageError::MissingValue(String::from("output_directory"))));
    }

    #[test]
    // Tests cases in which disallowed values are supplied for known parameters.
    fn test_command_line_args_values_not_allowed() {
        let fake_env = FakeEnv::from("--require=include", "");
        let under_test = CommandLineArgs::from_env_like(&fake_env, &NullLogger {});
        assert_eq!(
            under_test,
            Err(UsageError::ValueNotAllowed(String::from("include"), String::from(REQUIRE_OPTION)))
        );

        let fake_env = FakeEnv::from("--require=require", "");
        let under_test = CommandLineArgs::from_env_like(&fake_env, &NullLogger {});
        assert_eq!(
            under_test,
            Err(UsageError::ValueNotAllowed(String::from("require"), String::from(REQUIRE_OPTION)))
        );

        let fake_env = FakeEnv::from("--require=prohibit", "");
        let under_test = CommandLineArgs::from_env_like(&fake_env, &NullLogger {});
        assert_eq!(
            under_test,
            Err(UsageError::ValueNotAllowed(
                String::from("prohibit"),
                String::from(REQUIRE_OPTION)
            ))
        );

        let fake_env = FakeEnv::from("--require=host-test-binary", "");
        let under_test = CommandLineArgs::from_env_like(&fake_env, &NullLogger {});
        assert_eq!(
            under_test,
            Err(UsageError::ValueNotAllowed(
                String::from("host_test_binary"),
                String::from(REQUIRE_OPTION)
            ))
        );

        let fake_env = FakeEnv::from("--require=output-directory", "");
        let under_test = CommandLineArgs::from_env_like(&fake_env, &NullLogger {});
        assert_eq!(
            under_test,
            Err(UsageError::ValueNotAllowed(
                String::from("output_directory"),
                String::from(REQUIRE_OPTION)
            ))
        );

        let fake_env = FakeEnv::from("--prohibit=include", "");
        let under_test = CommandLineArgs::from_env_like(&fake_env, &NullLogger {});
        assert_eq!(
            under_test,
            Err(UsageError::ValueNotAllowed(
                String::from("include"),
                String::from(PROHIBIT_OPTION)
            ))
        );

        let fake_env = FakeEnv::from("--prohibit=require", "");
        let under_test = CommandLineArgs::from_env_like(&fake_env, &NullLogger {});
        assert_eq!(
            under_test,
            Err(UsageError::ValueNotAllowed(
                String::from("require"),
                String::from(PROHIBIT_OPTION)
            ))
        );

        let fake_env = FakeEnv::from("--prohibit=prohibit", "");
        let under_test = CommandLineArgs::from_env_like(&fake_env, &NullLogger {});
        assert_eq!(
            under_test,
            Err(UsageError::ValueNotAllowed(
                String::from("prohibit"),
                String::from(PROHIBIT_OPTION)
            ))
        );

        let fake_env = FakeEnv::from("--prohibit=host-test-binary", "");
        let under_test = CommandLineArgs::from_env_like(&fake_env, &NullLogger {});
        assert_eq!(
            under_test,
            Err(UsageError::ValueNotAllowed(
                String::from("host_test_binary"),
                String::from(PROHIBIT_OPTION)
            ))
        );

        let fake_env = FakeEnv::from("--prohibit=output-directory", "");
        let under_test = CommandLineArgs::from_env_like(&fake_env, &NullLogger {});
        assert_eq!(
            under_test,
            Err(UsageError::ValueNotAllowed(
                String::from("output_directory"),
                String::from(PROHIBIT_OPTION)
            ))
        );
    }

    #[test]
    // Tests cases in which values of the wrong type are supplied for known parameters.
    fn test_command_line_args_values_wrong_type() {
        let fake_env = FakeEnv::from("--include=1", "");
        let under_test = CommandLineArgs::from_env_like(&fake_env, &NullLogger {});
        assert_eq!(
            under_test,
            Err(UsageError::ExpectedArrayOrString(
                String::from(INCLUDE_OPTION),
                Value::Number(Number::from(1))
            ))
        );

        let fake_env = FakeEnv::from("--include=1,something", "");
        let under_test = CommandLineArgs::from_env_like(&fake_env, &NullLogger {});
        assert_eq!(
            under_test,
            Err(UsageError::ExpectedString(
                String::from(INCLUDE_OPTION),
                Value::Number(Number::from(1))
            ))
        );

        let fake_env = FakeEnv::from("--include=true", "");
        let under_test = CommandLineArgs::from_env_like(&fake_env, &NullLogger {});
        assert_eq!(
            under_test,
            Err(UsageError::ExpectedArrayOrString(String::from(INCLUDE_OPTION), Value::Bool(true)))
        );

        let fake_env = FakeEnv::from("--require=1", "");
        let under_test = CommandLineArgs::from_env_like(&fake_env, &NullLogger {});
        assert_eq!(
            under_test,
            Err(UsageError::ExpectedArrayOrString(
                String::from(REQUIRE_OPTION),
                Value::Number(Number::from(1))
            ))
        );

        let fake_env = FakeEnv::from("--require=1,something", "");
        let under_test = CommandLineArgs::from_env_like(&fake_env, &NullLogger {});
        assert_eq!(
            under_test,
            Err(UsageError::ExpectedString(
                String::from(REQUIRE_OPTION),
                Value::Number(Number::from(1))
            ))
        );

        let fake_env = FakeEnv::from("--require=true", "");
        let under_test = CommandLineArgs::from_env_like(&fake_env, &NullLogger {});
        assert_eq!(
            under_test,
            Err(UsageError::ExpectedArrayOrString(String::from(REQUIRE_OPTION), Value::Bool(true)))
        );

        let fake_env = FakeEnv::from("--prohibit=1", "");
        let under_test = CommandLineArgs::from_env_like(&fake_env, &NullLogger {});
        assert_eq!(
            under_test,
            Err(UsageError::ExpectedArrayOrString(
                String::from(PROHIBIT_OPTION),
                Value::Number(Number::from(1))
            ))
        );

        let fake_env = FakeEnv::from("--prohibit=1,something", "");
        let under_test = CommandLineArgs::from_env_like(&fake_env, &NullLogger {});
        assert_eq!(
            under_test,
            Err(UsageError::ExpectedString(
                String::from(PROHIBIT_OPTION),
                Value::Number(Number::from(1))
            ))
        );

        let fake_env = FakeEnv::from("--prohibit=true", "");
        let under_test = CommandLineArgs::from_env_like(&fake_env, &NullLogger {});
        assert_eq!(
            under_test,
            Err(UsageError::ExpectedArrayOrString(
                String::from(PROHIBIT_OPTION),
                Value::Bool(true)
            ))
        );

        let fake_env = FakeEnv::from("--host-test-binary=1", "");
        let under_test = CommandLineArgs::from_env_like(&fake_env, &NullLogger {});
        assert_eq!(
            under_test,
            Err(UsageError::ExpectedString(
                String::from(HOST_TEST_BINARY_OPTION),
                Value::Number(Number::from(1))
            ))
        );

        let fake_env = FakeEnv::from("--host-test-binary=true", "");
        let under_test = CommandLineArgs::from_env_like(&fake_env, &NullLogger {});
        assert_eq!(
            under_test,
            Err(UsageError::ExpectedString(
                String::from(HOST_TEST_BINARY_OPTION),
                Value::Bool(true)
            ))
        );

        let fake_env = FakeEnv::from("--output-directory=1", "");
        let under_test = CommandLineArgs::from_env_like(&fake_env, &NullLogger {});
        assert_eq!(
            under_test,
            Err(UsageError::ExpectedString(
                String::from(OUTPUT_DIRECTORY_OPTION),
                Value::Number(Number::from(1))
            ))
        );

        let fake_env = FakeEnv::from("--output-directory=true", "");
        let under_test = CommandLineArgs::from_env_like(&fake_env, &NullLogger {});
        assert_eq!(
            under_test,
            Err(UsageError::ExpectedString(
                String::from(OUTPUT_DIRECTORY_OPTION),
                Value::Bool(true)
            ))
        );
    }

    #[test]
    // Tests the processing of unknown parameters as args.
    fn test_command_line_args_unknown_args() {
        let fake_env = FakeEnv::from(
            "--true=true --false=false --true-simple --no-false-simple --zero=0 --string=foo --array=1,2,3,4",
            "",
        );
        let under_test = CommandLineArgs::from_env_like(&fake_env, &NullLogger {});
        assert_eq!(
            under_test,
            Ok(CommandLineArgs {
                env: EnvArg::None,
                test_params: TestParams {
                    unknown: HashMap::from([
                        (String::from("true"), Value::Bool(true)),
                        (String::from("false"), Value::Bool(false)),
                        (String::from("true_simple"), Value::Bool(true)),
                        (String::from("false_simple"), Value::Bool(false)),
                        (String::from("zero"), Value::Number(Number::from(0))),
                        (String::from("string"), Value::String(String::from("foo"))),
                        (
                            String::from("array"),
                            Value::Array(vec![
                                Value::Number(Number::from(1)),
                                Value::Number(Number::from(2)),
                                Value::Number(Number::from(3)),
                                Value::Number(Number::from(4))
                            ])
                        ),
                    ]),
                    ..Default::default()
                }
            })
        );

        let fake_env = FakeEnv::from("--zero-point-one=0.1 --negative-zero-point-one=-0.1", "");
        let under_test = CommandLineArgs::from_env_like(&fake_env, &NullLogger {});
        assert!(under_test.is_ok());
        assert_eq!(
            under_test
                .as_ref()
                .unwrap()
                .test_params
                .unknown
                .get("zero_point_one")
                .unwrap()
                .as_f64()
                .unwrap(),
            0.1
        );
        assert_eq!(
            under_test
                .as_ref()
                .unwrap()
                .test_params
                .unknown
                .get("negative_zero_point_one")
                .unwrap()
                .as_f64()
                .unwrap(),
            -0.1
        );
    }

    #[test]
    // Tests the processing of unknown parameters as vars.
    fn test_command_line_args_unknown_vars() {
        let fake_env =
            FakeEnv::from("--env", "FUCHSIA_TRUE=true FUCHSIA_FALSE=false FUCHSIA_ZERO=0 FUCHSIA_STRING=foo FUCHSIA_ARRAY=1,2,3,4");
        let under_test = CommandLineArgs::from_env_like(&fake_env, &NullLogger {});
        assert_eq!(
            under_test,
            Ok(CommandLineArgs {
                env: EnvArg::All,
                test_params: TestParams {
                    unknown: HashMap::from([
                        (String::from("true"), Value::Bool(true)),
                        (String::from("false"), Value::Bool(false)),
                        (String::from("zero"), Value::Number(Number::from(0))),
                        (String::from("string"), Value::String(String::from("foo"))),
                        (
                            String::from("array"),
                            Value::Array(vec![
                                Value::Number(Number::from(1)),
                                Value::Number(Number::from(2)),
                                Value::Number(Number::from(3)),
                                Value::Number(Number::from(4))
                            ])
                        ),
                    ]),
                    ..Default::default()
                }
            })
        );

        let fake_env = FakeEnv::from(
            "--env",
            "FUCHSIA_ZERO_POINT_ONE=0.1 FUCHSIA_NEGATIVE_ZERO_POINT_ONE=-0.1",
        );
        let under_test = CommandLineArgs::from_env_like(&fake_env, &NullLogger {});
        assert!(under_test.is_ok());
        assert_eq!(
            under_test
                .as_ref()
                .unwrap()
                .test_params
                .unknown
                .get("zero_point_one")
                .unwrap()
                .as_f64()
                .unwrap(),
            0.1
        );
        assert_eq!(
            under_test
                .as_ref()
                .unwrap()
                .test_params
                .unknown
                .get("negative_zero_point_one")
                .unwrap()
                .as_f64()
                .unwrap(),
            -0.1
        );
    }

    #[test]
    // Tests the processing of host-test-binary parameter in args and vars.
    fn test_command_line_args_host_test_binary() {
        let temp_file = NamedTempFile::new().expect("Failed to create temporary file");
        let temp_file_path = temp_file.path().to_str().unwrap();

        let fake_env = FakeEnv::from(format!("--host-test-binary={}", temp_file_path).as_str(), "");
        let under_test = CommandLineArgs::from_env_like(&fake_env, &NullLogger {});
        assert_eq!(
            under_test,
            Ok(CommandLineArgs {
                env: EnvArg::None,
                test_params: TestParams {
                    host_test_binary: PathBuf::from_str(temp_file_path).unwrap(),
                    ..Default::default()
                }
            })
        );

        let fake_env =
            FakeEnv::from("--env", format!("FUCHSIA_HOST_TEST_BINARY={}", temp_file_path).as_str());
        let under_test = CommandLineArgs::from_env_like(&fake_env, &NullLogger {});
        assert_eq!(
            under_test,
            Ok(CommandLineArgs {
                env: EnvArg::All,
                test_params: TestParams {
                    host_test_binary: PathBuf::from_str(temp_file_path).unwrap(),
                    ..Default::default()
                }
            })
        );

        temp_file.close().expect("Failed to close temporary file");
    }

    #[test]
    // Tests the processing of include parameter in args and vars.
    fn test_command_line_args_include() {
        let temp_file = NamedTempFile::new().expect("Failed to create temporary file");
        let temp_file_path = temp_file.path().to_str().unwrap();

        fs::write(temp_file_path, "{}").expect("Failed to write to temporary file");

        let fake_env = FakeEnv::from(format!("--include={}", temp_file_path).as_str(), "");
        let under_test = CommandLineArgs::from_env_like(&fake_env, &NullLogger {});
        assert_eq!(
            under_test,
            Ok(CommandLineArgs {
                env: EnvArg::None,
                test_params: TestParams {
                    include: vec![PathBuf::from_str(temp_file_path).unwrap()],
                    ..Default::default()
                }
            })
        );

        let fake_env =
            FakeEnv::from("--env", format!("FUCHSIA_INCLUDE={}", temp_file_path).as_str());
        let under_test = CommandLineArgs::from_env_like(&fake_env, &NullLogger {});
        assert_eq!(
            under_test,
            Ok(CommandLineArgs {
                env: EnvArg::All,
                test_params: TestParams {
                    include: vec![PathBuf::from_str(temp_file_path).unwrap()],
                    ..Default::default()
                }
            })
        );

        temp_file.close().expect("Failed to close temporary file");
    }

    #[test]
    // Tests the processing of require parameter in args and vars.
    fn test_command_line_args_require() {
        let fake_env = FakeEnv::from("--env --require=a,b,c", "FUCHSIA_REQUIRE=c,d,e");
        let under_test = CommandLineArgs::from_env_like(&fake_env, &NullLogger {});
        assert_eq!(
            under_test,
            Ok(CommandLineArgs {
                env: EnvArg::All,
                test_params: TestParams {
                    require: vec![
                        String::from("a"),
                        String::from("b"),
                        String::from("c"),
                        String::from("d"),
                        String::from("e")
                    ],
                    ..Default::default()
                },
                ..Default::default()
            })
        );
    }

    #[test]
    // Tests the processing of prohibit parameter in args and vars.
    fn test_command_line_args_prohibit() {
        let fake_env = FakeEnv::from("--env --prohibit=a,b,c", "FUCHSIA_PROHIBIT=c,d,e");
        let under_test = CommandLineArgs::from_env_like(&fake_env, &NullLogger {});
        assert_eq!(
            under_test,
            Ok(CommandLineArgs {
                env: EnvArg::All,
                test_params: TestParams {
                    prohibit: vec![
                        String::from("a"),
                        String::from("b"),
                        String::from("c"),
                        String::from("d"),
                        String::from("e")
                    ],
                    ..Default::default()
                },
                ..Default::default()
            })
        );
    }

    #[test]
    // Tests the processing of an include.
    fn test_command_line_args_include_params() {
        let temp_file = NamedTempFile::new().expect("Failed to create temporary file");
        let temp_file_path = temp_file.path().to_str().unwrap();

        fs::write(
            temp_file_path,
            r#"{"true":true, "false":false, "zero":0, "string":"foo", "array":[1,2,3,4]}"#,
        )
        .expect("Failed to write to temporary file");

        let fake_env = FakeEnv::from(format!("--include={}", temp_file_path).as_str(), "");
        let under_test = CommandLineArgs::from_env_like(&fake_env, &NullLogger {});
        assert_eq!(
            under_test,
            Ok(CommandLineArgs {
                env: EnvArg::None,
                test_params: TestParams {
                    include: vec![PathBuf::from_str(temp_file_path).unwrap()],
                    unknown: HashMap::from([
                        (String::from("true"), Value::Bool(true)),
                        (String::from("false"), Value::Bool(false)),
                        (String::from("zero"), Value::Number(Number::from(0))),
                        (String::from("string"), Value::String(String::from("foo"))),
                        (
                            String::from("array"),
                            Value::Array(vec![
                                Value::Number(Number::from(1)),
                                Value::Number(Number::from(2)),
                                Value::Number(Number::from(3)),
                                Value::Number(Number::from(4))
                            ])
                        ),
                    ]),
                    ..Default::default()
                }
            })
        );

        temp_file.close().expect("Failed to close temporary file");
    }

    #[test]
    // Tests the processing of nested includes.
    fn test_command_line_args_nested_includes() {
        let temp_file_outer = NamedTempFile::new().expect("Failed to create temporary file");
        let temp_file_outer_path = temp_file_outer.path().to_str().unwrap();
        let temp_file_inner = NamedTempFile::new().expect("Failed to create temporary file");
        let temp_file_inner_path = temp_file_inner.path().to_str().unwrap();

        fs::write(temp_file_outer_path, format!("{{\"include\":\"{}\"}}", temp_file_inner_path))
            .expect("Failed to write to temporary file");
        fs::write(temp_file_inner_path, "{}").expect("Failed to write to temporary file");

        let fake_env = FakeEnv::from(format!("--include={}", temp_file_outer_path).as_str(), "");
        let under_test = CommandLineArgs::from_env_like(&fake_env, &NullLogger {});
        assert_eq!(
            under_test,
            Ok(CommandLineArgs {
                env: EnvArg::None,
                test_params: TestParams {
                    include: vec![
                        PathBuf::from_str(temp_file_outer_path).unwrap(),
                        PathBuf::from_str(temp_file_inner_path).unwrap()
                    ],
                    ..Default::default()
                }
            })
        );

        temp_file_inner.close().expect("Failed to close temporary file");
        temp_file_outer.close().expect("Failed to close temporary file");
    }

    #[test]
    // Tests the processing of a JSON object in an include file.
    fn test_command_line_args_object() {
        let temp_file = NamedTempFile::new().expect("Failed to create temporary file");
        let temp_file_path = temp_file.path().to_str().unwrap();

        fs::write(temp_file_path, r#"{"object":{"foo":1,"bar":2}}"#)
            .expect("Failed to write to temporary file");

        let fake_env = FakeEnv::from(format!("--include={}", temp_file_path).as_str(), "");
        let under_test = CommandLineArgs::from_env_like(&fake_env, &NullLogger {});
        let mut object_map = serde_json::Map::new();
        object_map.insert(String::from("foo"), Value::Number(Number::from(1)));
        object_map.insert(String::from("bar"), Value::Number(Number::from(2)));
        assert_eq!(
            under_test,
            Ok(CommandLineArgs {
                env: EnvArg::None,
                test_params: TestParams {
                    include: vec![PathBuf::from_str(temp_file_path).unwrap()],
                    unknown: HashMap::from([(String::from("object"), Value::Object(object_map))]),
                    ..Default::default()
                }
            })
        );

        temp_file.close().expect("Failed to close temporary file");
    }

    #[test]
    // Tests recedence of parameter settings in args, vars, and includes.
    fn test_command_line_args_precedence() {
        let temp_file = NamedTempFile::new().expect("Failed to create temporary file");
        let temp_file_path = temp_file.path().to_str().unwrap();

        fs::write(temp_file_path, r#"{"foo":"include", "bar":"include", "baz":"include"}"#)
            .expect("Failed to write to temporary file");

        let fake_env = FakeEnv::from(
            format!("--include={} --env --foo=args", temp_file_path).as_str(),
            "FUCHSIA_FOO=vars FUCHSIA_BAR=vars",
        );
        let under_test = CommandLineArgs::from_env_like(&fake_env, &NullLogger {});

        // Args take precedence over vars, and both take precedence over includes.
        assert_eq!(
            under_test,
            Ok(CommandLineArgs {
                env: EnvArg::All,
                test_params: TestParams {
                    include: vec![PathBuf::from_str(temp_file_path).unwrap()],
                    unknown: HashMap::from([
                        (String::from("foo"), Value::String(String::from("args"))),
                        (String::from("bar"), Value::String(String::from("vars"))),
                        (String::from("baz"), Value::String(String::from("include"))),
                    ]),
                    ..Default::default()
                }
            })
        );

        temp_file.close().expect("Failed to close temporary file");
    }

    #[test]
    // Tests validation.
    fn test_command_line_args_validate() {
        let fake_env = FakeEnv::from("", "");
        let under_test = CommandLineArgs::from_env_like(&fake_env, &NullLogger {}).unwrap();
        assert_eq!(
            under_test.validate(),
            Err(UsageError::MissingRequiredParameter(String::from(HOST_TEST_BINARY_OPTION)))
        );

        let temp_file = NamedTempFile::new().expect("Failed to create temporary file");
        let temp_file_path = temp_file.path().to_str().unwrap();

        let fake_env = FakeEnv::from(format!("--host-test-binary={}", temp_file_path).as_str(), "");
        let under_test = CommandLineArgs::from_env_like(&fake_env, &NullLogger {}).unwrap();
        assert_eq!(
            under_test.validate(),
            Err(UsageError::MissingRequiredParameter(String::from(OUTPUT_DIRECTORY_OPTION)))
        );

        let fake_env = FakeEnv::from(
            format!("--host-test-binary={} --output-directory=/nonexistent", temp_file_path)
                .as_str(),
            "",
        );
        let under_test = CommandLineArgs::from_env_like(&fake_env, &NullLogger {}).unwrap();
        assert_eq!(under_test.validate(), Ok(()));

        let fake_env = FakeEnv::from(
            format!(
                "--host-test-binary={} --output-directory=/nonexistent --require=foo",
                temp_file_path
            )
            .as_str(),
            "",
        );
        let under_test = CommandLineArgs::from_env_like(&fake_env, &NullLogger {}).unwrap();
        assert_eq!(
            under_test.validate(),
            Err(UsageError::MissingRequiredParameter(String::from("foo")))
        );

        let fake_env = FakeEnv::from(
            format!(
                "--host-test-binary={} --output-directory=/nonexistent --prohibit=foo --foo=bar",
                temp_file_path
            )
            .as_str(),
            "",
        );
        let under_test = CommandLineArgs::from_env_like(&fake_env, &NullLogger {}).unwrap();
        assert_eq!(
            under_test.validate(),
            Err(UsageError::DefinedProhibitedParameter(String::from("foo")))
        );

        temp_file.close().expect("Failed to close temporary file");
    }
}
