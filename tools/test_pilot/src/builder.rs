// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// DESIGN
//
// TestConfigBuilder combines input from the command line, environment variables, and JSON files
// to create a single, aggregate test configuration as a generic serde_json::Value tree. The
// resulting Value tree is parsed into a specific type using serde_json::from_value.
//
// TestConfigBuilder relies heavily on the test configuration schema to validate the parameter
// assignments it processes. Final validation includes validating the final Value against the
// schema.
//
// Processing of the command line and environment variables employs the parsers module from
// this crate. The parsers handle the particulars of those use cases (e.g. arrays don't have
// brackets around them, naked integers qualify as strings, etc). In those cases, schema type
// information is used to decide what parser to use. Various attempts were made to use serde_json
// and valico::dsl for this parsing, but those proved inadequate or conterproductive for various
// reasons.
//
// JSON files are not checked for type mismatches until final validation against the schema.
// However, parameters assigned values in JSON files are checked against the schema to see if
// the parameter name is allowed as a property of the top-level object. This is done simply
// because more informative errors are generated this way.

use crate::env::EnvLike;
use crate::errors::{BuildError, UsageError};
use crate::logger::Logger;
use crate::name::Name;
use crate::parsers::parser_for_parameter;
use crate::schema::{PropertyType, Schema};
use serde::Deserialize;
use serde_json::Value;
use serde_json5;
use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::fs::File;
use std::io::BufReader;
use std::path::PathBuf;
use valico::common::error::ValicoError;
use valico::json_schema;
use valico::json_schema::validators::ValidationState;

const NO_STRICT_OPTION: &str = "no_strict";
const NO_OPTION_PREFIX: &str = "no_";

/// Builds a configuration from an `EnvLike`, an abstraction of `std::env`, and returns it.
pub fn build_from_env_like<T: for<'a> Deserialize<'a>, E: EnvLike, L: Logger>(
    env_like: &E,
    schema: Schema,
    logger: &mut L,
) -> Result<T, BuildError> {
    let param_values_by_name =
        TestConfigBuilder::from_env_like(env_like, schema, logger)?.param_values_by_name;
    let param_values_by_string =
        param_values_by_name.into_iter().map(|(k, v)| (k.to_string(), v)).collect();
    Ok(serde_json::from_value(Value::Object(param_values_by_string))?)
}

/// Builds a test configuration based on command line arguments, environment variables, and JSON
/// files.
#[derive(Debug, PartialEq, Default)]
struct TestConfigBuilder {
    /// The validated test configuration schema in `Value` form.
    schema: Schema,

    /// Whether the builder is currently in strict mode.
    strict: bool,

    /// Test parameter values by name. The configuration is built here and later parsed into the
    /// target struct.
    param_values_by_name: HashMap<Name, Value>,

    /// Names of parameters in `param_values_by_name` that were assigned in non-strict mode and
    /// have not yet been assigned in strict mode. This collection is used to allow non-strict
    /// assignments to override a single strict assignment.
    overrides: HashSet<Name>,

    /// Indicates which environment variables should be merged into `param_values_by_name`.
    env_arg: EnvArg,

    /// JSON files that have been included.
    include: Vec<PathBuf>,

    /// JSON files that should be included but which have not been processed. Every member of this
    /// collection also appears in `include`.
    unprocessed_includes: VecDeque<PathBuf>,

    /// Parameters that appear in the schema and are required for this test configuration. This
    /// mechanism is distinct from the 'required' section of the schema in that the parameters
    /// listed here are only required for this particular test configuration, whereas the schema
    /// required section lists parameters that are required in all test configurations.
    require: Vec<Name>,

    /// Parameters that appear in the schema and are prohibited for this test configuration.
    prohibit: Vec<Name>,
}

/// Used for deserializing an include file.
#[derive(Debug, Deserialize)]
struct IncludeFile {
    #[serde(flatten)]
    values_by_name: HashMap<Name, Value>,
}

/// Builds a test configuration based on the command line, referenced environment variables, and
/// referenced JSON files.
impl TestConfigBuilder {
    /// Creates a new `TestConfigBuilder` from an `EnvLike`, an abstraction of `std::env`.
    fn from_env_like<E: EnvLike, L: Logger>(
        env_like: &E,
        schema: Schema,
        logger: &mut L,
    ) -> Result<Self, BuildError> {
        let mut to_return = Self::from_arg_iter(env_like.args(), schema, logger)?;
        to_return.process_vars(env_like, logger)?;
        while let Some(include) = to_return.unprocessed_includes.pop_front() {
            to_return.process_include(include, logger)?;
        }
        to_return.validate()?;

        Ok(to_return)
    }

    /// Create a new `TestConfigBuilder` from an iterator over argument strings.
    fn from_arg_iter<L: Logger>(
        args: impl Iterator<Item = String>,
        schema: Schema,
        logger: &mut L,
    ) -> Result<Self, UsageError> {
        let mut to_return = TestConfigBuilder { schema, ..Default::default() };
        logger.start_command_line();
        for arg in args {
            if let Some(stripped_arg) = arg.strip_prefix("--") {
                if let Some((arg_name, value)) = stripped_arg.split_once('=') {
                    // --name=value
                    let name = Name::from_arg_name(arg_name);
                    match &name {
                        Name::Schema => {
                            // Processed in `from_env`.
                            logger.schema_option(value);
                        }
                        Name::Debug => {
                            return Err(UsageError::UnexpectedOptionValue {
                                option: name,
                                got: Value::from(value),
                            });
                        }
                        Name::Env => {
                            logger.add_some_to_env(&value);
                            to_return.env_arg = to_return.env_arg.add(value.split(','));
                        }
                        Name::Strict
                        | Name::Include
                        | Name::Require
                        | Name::Prohibit
                        | Name::Parameter(_) => {
                            to_return.add_name_and_text_value(name, value, logger)?;
                        }
                    }
                } else {
                    // --name or --no-name
                    let name = Name::from_arg_name(stripped_arg);
                    match &name {
                        Name::Schema => {
                            return Err(UsageError::MissingValue(name));
                        }
                        Name::Debug => {
                            logger.debug_option();
                        }
                        Name::Env => {
                            logger.add_all_to_env();
                            to_return.env_arg = EnvArg::All;
                        }
                        Name::Strict
                        | Name::Include
                        | Name::Require
                        | Name::Prohibit
                        | Name::Parameter(_) => {
                            to_return.add_name_no_value(name, logger)?;
                        }
                    }
                }
            } else {
                return Err(UsageError::UnexpectedPositionalArgument(arg));
            }
        }

        Ok(to_return)
    }

    /// Processes environment variables. This function sets `self.env_arg` to `EnvArg::None`.
    fn process_vars<E: EnvLike, L: Logger>(
        &mut self,
        env_like: &E,
        logger: &mut L,
    ) -> Result<(), UsageError> {
        logger.start_environment_variables();

        let mut to_add = vec![];
        let mut env_arg = EnvArg::None;
        std::mem::swap(&mut self.env_arg, &mut env_arg);

        match env_arg {
            EnvArg::None => {}
            EnvArg::All => {
                for (var_name, value) in env_like.vars() {
                    match Name::from_var_name(var_name.as_ref()) {
                        Some(name) => {
                            if name.can_be_added(&self.schema) {
                                to_add.push((name, value.clone()));
                            }
                        }
                        None => {
                            // Ignore variables that are not viable test parameters.
                        }
                    }
                }
            }
            EnvArg::Some(names) => {
                for name in names {
                    if !self.schema.properties.contains_key(&name) {
                        return Err(UsageError::UnknownEnvParameter(name));
                    }
                    match env_like.var(&name.to_var_name()) {
                        Ok(value) => {
                            to_add.push((name, value.clone()));
                        }
                        Err(_) => {
                            // The value is not present, and we do nothing.
                        }
                    }
                }
            }
        }

        // Add the name/value pairs in a separate step to satisfy the borrow checker.
        for (name, value) in to_add {
            self.add_name_and_text_value(name, value.as_str(), logger)?;
        }

        Ok(())
    }

    /// Process a single JSON file. If new includes are encountered, they will get pushed to the
    /// back of `unprocessed_includes` and processed later.
    fn process_include<L: Logger>(
        &mut self,
        path: PathBuf,
        logger: &mut L,
    ) -> Result<(), BuildError> {
        logger.start_include(&path);

        let file = File::open(&path)
            .map_err(|e| BuildError::FailedToOpenInclude { path: path.clone(), source: e })?;
        let mut reader = BufReader::new(file);
        let include_file: IncludeFile = serde_json5::from_reader(&mut reader)
            .map_err(|e| BuildError::FailedToParseInclude { path: path.clone(), source: e })?;

        for (name, value) in include_file.values_by_name {
            // Strictly speaking, we could leave this check to validation by schema, but the
            // errors produced by the validator are less useful than we get this way.
            if name.can_be_added(&self.schema) {
                self.add_name_and_value(name, value, logger)?;
            } else {
                return Err(UsageError::UnrecognizedParameter(name).into());
            }
        }

        Ok(())
    }

    /// Adds a name/value pair to `param_values_by_name`. The name must be in lower_snake_case. The value
    /// is parsed as appropriate. This function is used for parameters given on the command
    /// line with a value (e.g. --foo=bar) and parameters given as environment variables.
    fn add_name_and_text_value<L: Logger>(
        &mut self,
        name: Name,
        value_str: &str,
        logger: &mut L,
    ) -> Result<(), UsageError> {
        let value = parser_for_parameter(&name, &self.schema)?.parse(&name, value_str)?;
        self.add_name_and_value(name, value, logger)
    }

    /// Adds a name/value pair as expressed by a command line option with no value.
    fn add_name_no_value<L: Logger>(
        &mut self,
        name: Name,
        logger: &mut L,
    ) -> Result<(), UsageError> {
        match name {
            Name::Parameter(p) if p == NO_STRICT_OPTION => {
                // Strict may not be turned off.
                return Err(UsageError::InvalidStrictValue(String::from("false")));
            }
            Name::Strict => {
                // Strict may be turned on this way.
                return self.add_name_and_value(name, Value::Bool(true), logger);
            }
            Name::Include | Name::Require | Name::Prohibit => {
                // These options require values.
                return Err(UsageError::MissingValue(name));
            }
            Name::Debug | Name::Env | Name::Schema | Name::Parameter(_) => {}
        }

        if let Some(scheme) = self.schema.properties.get(&name) {
            // The name is a valid parameter in the schema. Set it to true if its type
            // is boolean, otherwise, complain that the value was not provided.
            if scheme.property_type == PropertyType::Boolean {
                return self.add_name_and_value(name, Value::Bool(true), logger);
            } else {
                return Err(UsageError::MissingValue(name));
            }
        } else if name.starts_with(NO_OPTION_PREFIX) {
            // The name starts with 'no_'. If the rest of the name is a valid boolean
            // parameter in the schema, set it to false.
            let name = name.strip_prefix(NO_OPTION_PREFIX).unwrap();
            if let Some(scheme) = self.schema.properties.get(&name) {
                if scheme.property_type == PropertyType::Boolean {
                    return self.add_name_and_value(name, Value::Bool(false), logger);
                }
            }
        } else {
            // The name does not start with 'no_'. If the name with 'no_' prepended is
            // a valid boolean parameter in the schema, set it to false.
            let mut no_name = String::from(NO_OPTION_PREFIX);
            no_name.push_str(name.as_str());
            let name = Name::from(no_name);
            if let Some(scheme) = self.schema.properties.get(&name) {
                if scheme.property_type == PropertyType::Boolean {
                    return self.add_name_and_value(name, Value::Bool(false), logger);
                }
            }
        }

        Err(UsageError::UnrecognizedParameter(name))
    }

    /// Adds a name/value pair to `param_values_by_name`. The name must be in lower_snake_case.
    fn add_name_and_value<L: Logger>(
        &mut self,
        name: Name,
        mut value: Value,
        logger: &mut L,
    ) -> Result<(), UsageError> {
        match name {
            Name::Strict => match value {
                Value::Bool(true) => {
                    logger.strict();
                    self.strict = true;
                }
                v => {
                    return Err(UsageError::InvalidStrictValue(v.to_string()));
                }
            },
            Name::Include => {
                for item_string in strings_in_option_array_value(Name::Include, value)? {
                    let path = parse_include_path(item_string.as_str())?;
                    if !self.include.contains(&path) {
                        logger.add_include(&path);
                        self.include.push(path.clone());
                        self.unprocessed_includes.push_back(path);
                    } else {
                        logger.include_already_added(&path);
                    }
                }
            }
            Name::Require => {
                for item_string in strings_in_option_array_value(Name::Require, value)? {
                    let item_name = Name::from_arg_name(item_string.as_str());
                    if !item_name.is_viable_parameter_name() {
                        return Err(UsageError::InvalidParameterName {
                            option: Name::Require,
                            got: item_name,
                        });
                    }
                    if !self.require.contains(&item_name) {
                        logger.add_require(&item_name);
                        self.require.push(item_name);
                    } else {
                        logger.require_already_added(&item_name);
                    }
                }
            }
            Name::Prohibit => {
                for item_string in strings_in_option_array_value(Name::Prohibit, value)? {
                    let item_name = Name::from_str(item_string.as_str());
                    if !item_name.is_viable_parameter_name() {
                        return Err(UsageError::InvalidParameterName {
                            option: Name::Prohibit,
                            got: item_name,
                        });
                    }
                    if !self.prohibit.contains(&item_name) {
                        logger.add_prohibit(&item_name);
                        self.prohibit.push(item_name);
                    } else {
                        logger.prohibit_already_added(&item_name);
                    }
                }
            }
            _ => {
                if let Some(mut existing) = self.param_values_by_name.get_mut(&name) {
                    // Parameter already assigned a value.
                    if let Value::Array(vector) = &mut existing {
                        // Array parameter already assigned a value. Append the new value.
                        logger.add_to_array(&name, &value);
                        vector.append(
                            value.as_array_mut().expect("value merged into array is array"),
                        );
                    } else {
                        // Non-array parameter already assigned a value.
                        if self.strict {
                            // Strict mode.
                            if !self.overrides.remove(&name) {
                                // The name has been assigned previously in strict mode, so we fail.
                                return Err(UsageError::ParamAlreadyStrictlyAssigned(name));
                            } else {
                                // The name was assigned in non-strict mode and has not been
                                // assigned previously in strict mode. We already removed the
                                // name from `overrides` so any subsequent assignments in strict
                                // mode will fail.
                                logger.overridden_add_parameter_strict_ignored(&name, &value);
                            }
                        } else {
                            // Non-strict mode. We make the assignment, overriding the previous
                            // assignment, which was also made in non-strict mode. We know the
                            // name is already in `overrides`, because that happened when it
                            // was initially assigned in non-strict mode.
                            logger.add_parameter_non_strict(&name, &value);
                            let _ = self.param_values_by_name.insert(name, value);
                        }
                    }
                } else {
                    // Parameter not already assigned a value.
                    if value.is_array() {
                        logger.add_to_array(&name, &value);
                    } else if self.strict {
                        logger.add_parameter_strict(&name, &value);
                    } else {
                        logger.add_parameter_non_strict(&name, &value);
                        self.overrides.insert(name.clone());
                    }
                    let _ = self.param_values_by_name.insert(name, value);
                }
            }
        }

        Ok(())
    }

    /// Validates `self`.
    fn validate(&self) -> Result<(), BuildError> {
        assert!(
            self.unprocessed_includes.is_empty(),
            "TestConfigBuilder contains unprocessed includes."
        );

        let mut errors = vec![];

        // Ensure that all the parameters that have been required are in the schema and that
        // those parameters have been supplied.
        for required in &self.require {
            if !self.schema.properties.contains_key(required) {
                errors.push(UsageError::UnknownRequiredParameter(required.clone()).into());
            } else if !self.param_values_by_name.contains_key(required) {
                errors.push(UsageError::MissingRequiredParameter(required.clone()).into());
            }
        }

        // Ensure that all the parameters that have been prohibited are in the schema and that
        // those parameters have not been supplied.
        for prohibited in &self.prohibit {
            if !self.schema.properties.contains_key(prohibited) {
                errors.push(UsageError::UnknownProhibitedParameter(prohibited.clone()).into());
            } else if self.param_values_by_name.contains_key(prohibited) {
                errors.push(UsageError::DefinedProhibitedParameter(prohibited.clone()).into());
            }
        }

        let mut scope = json_schema::Scope::new();
        let validator = scope
            .compile_and_return(self.schema.as_value.clone(), /*ban_unknown=*/ true)
            .expect("Schema to compile");
        let param_values_by_string =
            self.param_values_by_name.iter().map(|(k, v)| (k.to_string(), v.clone())).collect();
        let validation_state = validator.validate(&Value::Object(param_values_by_string));

        if !validation_state.is_strictly_valid() {
            match validation_state_to_build_error(validation_state) {
                BuildError::ValidationMultiple(mut schema_errors) => {
                    errors.append(&mut schema_errors);
                }
                schema_error => {
                    errors.push(schema_error);
                }
            }
        }

        if errors.is_empty() {
            Ok(())
        } else if errors.len() == 1 {
            Err(errors.remove(0))
        } else {
            Err(BuildError::ValidationMultiple(errors))
        }
    }
}

/// Represents a set of environment variables as expressed in --env arguments.
#[derive(Default, Debug, PartialEq)]
enum EnvArg {
    /// Indicates an empty set.
    #[default]
    None,

    /// Indicates all 'valid' environment variables in std::env. An environment variable name
    /// is considered valid if it has a 'FUCHSIA_' prefix and is in SHOUTY_SNAKE_CASE.
    All,

    /// Specific environment variables identified by name in json format (snake_case without
    /// the 'FUCHSIA_' prefix). Always sorted in ascending alphabetical order.
    Some(Vec<Name>),
}

impl EnvArg {
    /// Returns a `EnvArg::Some` with the specified variable names. `names` may contain
    /// duplicates and may be in any order.
    fn some(mut names: Vec<Name>) -> Self {
        names.sort();
        names.dedup();
        EnvArg::Some(names)
    }

    /// Returns an `EnvArg` that merges self and the names in `to_add`. `to_add` may contain
    /// duplicates and may be in any order.
    fn add<'a>(self, to_add: impl IntoIterator<Item = &'a str>) -> Self {
        match self {
            EnvArg::None => Self::some(to_add.into_iter().map(|s| Name::from_str(s)).collect()),
            EnvArg::All => self,
            EnvArg::Some(mut self_args) => {
                for a in to_add {
                    self_args.push(Name::from_str(a));
                }
                Self::some(self_args)
            }
        }
    }
}

/// Returns an iterator for the strings in a `Value::Array` of `Value::String`. Returns an error
/// if anything else is found.
///
/// This is used for options 'include', 'require' and 'prohibit' only. Note that errors are only
/// generated for JSON assignments, because command line and environment variable values were
/// parsed with our parsers to be of the correct type.
fn strings_in_option_array_value(
    option_name: Name,
    value: Value,
) -> Result<impl Iterator<Item = String>, UsageError> {
    let item_values = match value {
        Value::Array(vector) => vector,
        v => {
            return Err(UsageError::UnexpectedOptionValue { option: option_name, got: v });
        }
    };

    for item_value in &item_values {
        if !item_value.is_string() {
            return Err(UsageError::UnexpectedOptionValue {
                option: option_name,
                got: item_value.clone(),
            });
        }
    }

    Ok(item_values.into_iter().map(|item_value| String::from(item_value.as_str().unwrap())))
}

/// Parse an include file path, checking that the file exists and is a file.
fn parse_include_path(path: &str) -> Result<PathBuf, UsageError> {
    let path = PathBuf::from(path);
    if !path.exists() {
        return Err(UsageError::IncludedPathDoesNotExist(path));
    }

    if let Ok(metadata) = fs::metadata(&path) {
        if !metadata.is_file() {
            return Err(UsageError::IncludedPathIsNotAFile(path));
        }
    } else {
        return Err(UsageError::IncludedPathUnreadable(path));
    }

    Ok(path)
}

/// Creates a `UsageError` from a failed `ValidationState`.
fn validation_state_to_build_error(validation_state: ValidationState) -> BuildError {
    let mut errors = vec![];

    for missing in &validation_state.missing {
        errors.push(UsageError::MissingParameterRequiredBySchema(missing.to_string()).into());
    }

    for e in &validation_state.errors {
        errors.push(validation_error_to_build_error(&e));
    }

    if errors.is_empty() {
        BuildError::UnclassifiedSchemaState(Box::new(validation_state))
    } else if errors.len() == 1 {
        errors.pop().unwrap()
    } else {
        BuildError::ValidationMultiple(errors)
    }
}

/// Creates a `UsageError` from a failed `ValicoError`.
fn validation_error_to_build_error(validation_error: &Box<dyn ValicoError>) -> BuildError {
    match validation_error.get_code() {
        "required" => UsageError::MissingParameterRequiredBySchema(validation_error_simple_path(
            &validation_error,
        ))
        .into(),
        "wrong_type" => UsageError::SchemaTypeMismatch {
            parameter: validation_error_simple_path(&validation_error),
            detail: String::from(
                validation_error.get_detail().expect("WrongType error has detail"),
            ),
        }
        .into(),
        _ => BuildError::UnclassifiedSchemaError(format!("{:?}", validation_error)),
    }
}

fn validation_error_simple_path(validation_error: &Box<dyn ValicoError>) -> String {
    String::from(validation_error.get_path().split('/').next_back().unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::env::testutils::FakeEnv;
    use crate::logger::NullLogger;
    use crate::schema::tests::fake_schema;
    use assert_matches::assert_matches;
    use serde_json::{json, Number};
    use std::fs;
    use std::str::FromStr;
    use tempfile::{tempdir, NamedTempFile};

    fn option_name() -> Name {
        Name::from_str("test_option_name")
    }

    /// Asserts that a `Result<TestConfigBuilder, BuilderError>` wraps `usage_error`
    #[track_caller]
    fn assert_usage_error(result: Result<TestConfigBuilder, BuildError>, usage_error: UsageError) {
        assert_matches!(result, Err(BuildError::IncorrectUsage(e)) if e == usage_error)
    }

    #[test]
    fn test_strings_in_option_array_value() {
        assert_eq!(
            Err(UsageError::UnexpectedOptionValue {
                option: option_name(),
                got: Value::from_str("{}").unwrap(),
            }),
            strings_in_option_array_value(option_name(), json!({})).map(|_| ())
        );
        assert_eq!(
            Err(UsageError::UnexpectedOptionValue {
                option: option_name(),
                got: Value::from_str("4").unwrap(),
            }),
            strings_in_option_array_value(option_name(), json!(4)).map(|_| ())
        );
        assert_eq!(
            Err(UsageError::UnexpectedOptionValue {
                option: option_name(),
                got: Value::from_str("\"squint\"").unwrap(),
            }),
            strings_in_option_array_value(option_name(), json!("squint")).map(|_| ())
        );
        assert_eq!(
            Err(UsageError::UnexpectedOptionValue {
                option: option_name(),
                got: Value::from_str("{}").unwrap(),
            }),
            strings_in_option_array_value(option_name(), json!([{}])).map(|_| ())
        );
        assert_eq!(
            Err(UsageError::UnexpectedOptionValue {
                option: option_name(),
                got: Value::from_str("4").unwrap(),
            }),
            strings_in_option_array_value(option_name(), json!([4])).map(|_| ())
        );
        assert_eq!(
            Err(UsageError::UnexpectedOptionValue {
                option: option_name(),
                got: Value::from_str("4").unwrap(),
            }),
            strings_in_option_array_value(option_name(), json!(["squint", 4])).map(|_| ())
        );
        assert_eq!(
            vec![String::from("squint")],
            strings_in_option_array_value(option_name(), json!(["squint"]))
                .unwrap()
                .collect::<Vec<String>>()
        );
        assert_eq!(
            vec![String::from("squint"), String::from("frown")],
            strings_in_option_array_value(option_name(), json!(["squint", "frown"]))
                .unwrap()
                .collect::<Vec<String>>()
        );
    }

    #[test]
    fn test_parse_include_path() {
        // Existent file.
        let temp_file = NamedTempFile::new().expect("to create temporary file");
        let temp_file_path = temp_file.path().to_str().unwrap().to_string();
        assert!(parse_include_path(temp_file_path.as_str()).is_ok());
        temp_file.close().expect("to close temporary file");

        // Non-existent file.
        let result = parse_include_path("/non_existent_file");
        assert_eq!(
            result,
            Err(UsageError::IncludedPathDoesNotExist(PathBuf::from("/non_existent_file")))
        );

        // Existent directory.
        let temp_dir = tempdir().expect("to create temporary directory");
        let temp_dir_path = temp_dir.path();
        let result = parse_include_path(temp_dir_path.to_str().unwrap());
        assert_eq!(
            result,
            Err(UsageError::IncludedPathIsNotAFile(PathBuf::from(temp_dir_path.to_str().unwrap())))
        );
        temp_dir.close().expect("to close temporary directory");
    }

    #[test]
    fn test_is_viable_parameter_name() {
        assert!(!Name::Schema.is_viable_parameter_name());
        assert!(!Name::Debug.is_viable_parameter_name());
        assert!(!Name::Env.is_viable_parameter_name());
        assert!(!Name::Strict.is_viable_parameter_name());
        assert!(!Name::Include.is_viable_parameter_name());
        assert!(!Name::Require.is_viable_parameter_name());
        assert!(!Name::Prohibit.is_viable_parameter_name());
        assert!(!Name::from_str("ahoy!").is_viable_parameter_name());
        assert!(!Name::from_str("1_thing").is_viable_parameter_name());
        assert!(Name::from_str("_one_thing").is_viable_parameter_name());
        assert!(Name::from_str("snorkel").is_viable_parameter_name());
    }

    #[test]
    fn test_env_arg() {
        let env_arg =
            EnvArg::some(vec![Name::from_str("e"), Name::from_str("a"), Name::from_str("c")]);
        assert_eq!(
            env_arg,
            EnvArg::Some(vec![Name::from_str("a"), Name::from_str("c"), Name::from_str("e")])
        );

        let env_arg =
            EnvArg::some(vec![Name::from_str("e"), Name::from_str("a"), Name::from_str("c")]);
        let env_arg = env_arg.add(vec!["d", "b"]);
        assert_eq!(
            env_arg,
            EnvArg::Some(vec![
                Name::from_str("a"),
                Name::from_str("b"),
                Name::from_str("c"),
                Name::from_str("d"),
                Name::from_str("e")
            ])
        );

        let env_arg = EnvArg::None;
        let env_arg = env_arg.add(vec!["d", "b"]);
        assert_eq!(env_arg, EnvArg::Some(vec![Name::from_str("b"), Name::from_str("d")]));

        let env_arg = EnvArg::All;
        let env_arg = env_arg.add(vec!["d", "b"]);
        assert_eq!(env_arg, EnvArg::All);
    }

    #[test]
    // Tests the case in which no arguments are supplied.
    fn test_no_args() {
        let fake_env = FakeEnv::new("", "");

        let under_test =
            TestConfigBuilder::from_env_like(&fake_env, fake_schema(), &mut NullLogger)
                .expect("Ok result");
        assert!(under_test.param_values_by_name.is_empty());
    }

    #[test]
    // Tests the case in which an unexpected positional argument is provided.
    fn test_unexpected_positional_arg() {
        let fake_env = FakeEnv::new("foo", "");

        let under_test =
            TestConfigBuilder::from_env_like(&fake_env, fake_schema(), &mut NullLogger);
        assert_usage_error(
            under_test,
            UsageError::UnexpectedPositionalArgument(String::from("foo")),
        );
    }

    #[test]
    // Tests processing of all environment variables.
    fn test_env_all() {
        let fake_env = FakeEnv::new("--env", "FUCHSIA_A=b FUCHSIA_C=d FUCHSIA_E=f");

        let under_test =
            TestConfigBuilder::from_env_like(&fake_env, fake_schema(), &mut NullLogger)
                .expect("Ok result");
        assert_eq!(
            under_test.param_values_by_name,
            HashMap::from_iter(
                [
                    (Name::from_str("a"), Value::String(String::from("b"))),
                    (Name::from_str("c"), Value::String(String::from("d"))),
                    (Name::from_str("e"), Value::String(String::from("f"))),
                ]
                .into_iter()
            )
        );
    }

    #[test]
    // Tests processing of specified environment variables.
    fn test_env_some() {
        let fake_env = FakeEnv::new("--env=a,d,e", "FUCHSIA_A=b FUCHSIA_C=d FUCHSIA_E=f");

        let under_test =
            TestConfigBuilder::from_env_like(&fake_env, fake_schema(), &mut NullLogger)
                .expect("Ok result");
        assert_eq!(
            under_test.param_values_by_name,
            HashMap::from_iter(
                [
                    (Name::from_str("a"), Value::String(String::from("b"))),
                    (Name::from_str("e"), Value::String(String::from("f"))),
                ]
                .into_iter()
            )
        );
    }

    #[test]
    // Tests cases in which values are missing for known parameters that require them.
    fn test_missing_values() {
        let fake_env = FakeEnv::new("--include", "");
        let under_test =
            TestConfigBuilder::from_env_like(&fake_env, fake_schema(), &mut NullLogger);
        assert_usage_error(under_test, UsageError::MissingValue(Name::Include));

        let fake_env = FakeEnv::new("--require", "");
        let under_test =
            TestConfigBuilder::from_env_like(&fake_env, fake_schema(), &mut NullLogger);
        assert_usage_error(under_test, UsageError::MissingValue(Name::Require));

        let fake_env = FakeEnv::new("--prohibit", "");
        let under_test =
            TestConfigBuilder::from_env_like(&fake_env, fake_schema(), &mut NullLogger);
        assert_usage_error(under_test, UsageError::MissingValue(Name::Prohibit));

        let fake_env = FakeEnv::new("--host-test-binary", "");
        let under_test =
            TestConfigBuilder::from_env_like(&fake_env, fake_schema(), &mut NullLogger);
        assert_usage_error(
            under_test,
            UsageError::MissingValue(Name::from_str("host_test_binary")),
        );

        let fake_env = FakeEnv::new("--host-test-args", "");
        let under_test =
            TestConfigBuilder::from_env_like(&fake_env, fake_schema(), &mut NullLogger);
        assert_usage_error(under_test, UsageError::MissingValue(Name::from_str("host_test_args")));

        let fake_env = FakeEnv::new("--output-directory", "");
        let under_test =
            TestConfigBuilder::from_env_like(&fake_env, fake_schema(), &mut NullLogger);
        assert_usage_error(
            under_test,
            UsageError::MissingValue(Name::from_str("output_directory")),
        );
    }

    #[test]
    // Tests cases in which disallowed values are supplied for known parameters.
    fn test_values_not_allowed() {
        let fake_env = FakeEnv::new("--require=include", "");
        let under_test =
            TestConfigBuilder::from_env_like(&fake_env, fake_schema(), &mut NullLogger);
        assert_usage_error(
            under_test,
            UsageError::InvalidParameterName { option: Name::Require, got: Name::Include },
        );

        let fake_env = FakeEnv::new("--require=require", "");
        let under_test =
            TestConfigBuilder::from_env_like(&fake_env, fake_schema(), &mut NullLogger);
        assert_usage_error(
            under_test,
            UsageError::InvalidParameterName { option: Name::Require, got: Name::Require },
        );

        let fake_env = FakeEnv::new("--require=prohibit", "");
        let under_test =
            TestConfigBuilder::from_env_like(&fake_env, fake_schema(), &mut NullLogger);
        assert_usage_error(
            under_test,
            UsageError::InvalidParameterName { option: Name::Require, got: Name::Prohibit },
        );

        let fake_env = FakeEnv::new("--prohibit=include", "");
        let under_test =
            TestConfigBuilder::from_env_like(&fake_env, fake_schema(), &mut NullLogger);
        assert_usage_error(
            under_test,
            UsageError::InvalidParameterName { option: Name::Prohibit, got: Name::Include },
        );

        let fake_env = FakeEnv::new("--prohibit=require", "");
        let under_test =
            TestConfigBuilder::from_env_like(&fake_env, fake_schema(), &mut NullLogger);
        assert_usage_error(
            under_test,
            UsageError::InvalidParameterName { option: Name::Prohibit, got: Name::Require },
        );

        let fake_env = FakeEnv::new("--prohibit=prohibit", "");
        let under_test =
            TestConfigBuilder::from_env_like(&fake_env, fake_schema(), &mut NullLogger);
        assert_usage_error(
            under_test,
            UsageError::InvalidParameterName { option: Name::Prohibit, got: Name::Prohibit },
        );
    }

    #[test]
    // Tests cases in which values of the wrong type are supplied for known parameters.
    fn test_values_wrong_type() {
        let fake_env = FakeEnv::new("--include=1", "");
        let under_test =
            TestConfigBuilder::from_env_like(&fake_env, fake_schema(), &mut NullLogger);
        assert_usage_error(under_test, UsageError::IncludedPathDoesNotExist(PathBuf::from("1")));

        let fake_env = FakeEnv::new("--include=1,something", "");
        let under_test =
            TestConfigBuilder::from_env_like(&fake_env, fake_schema(), &mut NullLogger);
        assert_usage_error(under_test, UsageError::IncludedPathDoesNotExist(PathBuf::from("1")));

        let fake_env = FakeEnv::new("--require=1", "");
        let under_test =
            TestConfigBuilder::from_env_like(&fake_env, fake_schema(), &mut NullLogger);
        assert_usage_error(
            under_test,
            UsageError::InvalidParameterName { option: Name::Require, got: Name::from_str("1") },
        );

        let fake_env = FakeEnv::new("--require=1,something", "");
        let under_test =
            TestConfigBuilder::from_env_like(&fake_env, fake_schema(), &mut NullLogger);
        assert_usage_error(
            under_test,
            UsageError::InvalidParameterName { option: Name::Require, got: Name::from_str("1") },
        );

        let fake_env = FakeEnv::new("--prohibit=1", "");
        let under_test =
            TestConfigBuilder::from_env_like(&fake_env, fake_schema(), &mut NullLogger);
        assert_usage_error(
            under_test,
            UsageError::InvalidParameterName { option: Name::Prohibit, got: Name::from_str("1") },
        );

        let fake_env = FakeEnv::new("--prohibit=1,something", "");
        let under_test =
            TestConfigBuilder::from_env_like(&fake_env, fake_schema(), &mut NullLogger);
        assert_usage_error(
            under_test,
            UsageError::InvalidParameterName { option: Name::Prohibit, got: Name::from_str("1") },
        );

        let fake_env = FakeEnv::new("--output-directory=1,true", "");
        let under_test =
            TestConfigBuilder::from_env_like(&fake_env, fake_schema(), &mut NullLogger);
        assert_usage_error(
            under_test,
            UsageError::CommasNotAllowed {
                parameter: Name::from_str("output_directory"),
                got: String::from("1,true"),
            },
        );

        let fake_env = FakeEnv::new("--output-directory=true,1", "");
        let under_test =
            TestConfigBuilder::from_env_like(&fake_env, fake_schema(), &mut NullLogger);
        assert_usage_error(
            under_test,
            UsageError::CommasNotAllowed {
                parameter: Name::from_str("output_directory"),
                got: String::from("true,1"),
            },
        );
    }

    #[test]
    // Tests the processing of parameters as args.
    fn test_args() {
        let fake_env = FakeEnv::new(
            "--true=true --false=false --true-simple --no-false-simple --negative-bool --zero=0 \
            --string=foo --array_of_number=1,2,3,4",
            "",
        );
        let under_test =
            TestConfigBuilder::from_env_like(&fake_env, fake_schema(), &mut NullLogger)
                .expect("Ok result");
        assert_eq!(
            under_test.param_values_by_name,
            HashMap::from_iter(
                vec![
                    (Name::from_str("true"), Value::Bool(true)),
                    (Name::from_str("false"), Value::Bool(false)),
                    (Name::from_str("true_simple"), Value::Bool(true)),
                    (Name::from_str("false_simple"), Value::Bool(false)),
                    (Name::from_str("no_negative_bool"), Value::Bool(false)),
                    (Name::from_str("zero"), Value::Number(Number::from(0))),
                    (Name::from_str("string"), Value::String(String::from("foo"))),
                    (
                        Name::from_str("array_of_number"),
                        Value::Array(vec![
                            Value::Number(Number::from(1)),
                            Value::Number(Number::from(2)),
                            Value::Number(Number::from(3)),
                            Value::Number(Number::from(4))
                        ])
                    ),
                ]
                .into_iter()
            )
        );

        let fake_env = FakeEnv::new("--zero-point-one=0.1 --negative-zero-point-one=-0.1", "");
        let under_test =
            TestConfigBuilder::from_env_like(&fake_env, fake_schema(), &mut NullLogger)
                .expect("Ok result");
        assert_eq!(
            under_test
                .param_values_by_name
                .get(&Name::from_str("zero_point_one"))
                .unwrap()
                .as_f64()
                .unwrap(),
            0.1
        );
        assert_eq!(
            under_test
                .param_values_by_name
                .get(&Name::from_str("negative_zero_point_one"))
                .unwrap()
                .as_f64()
                .unwrap(),
            -0.1
        );

        let fake_env = FakeEnv::new("--squeak", "");
        assert_usage_error(
            TestConfigBuilder::from_env_like(&fake_env, fake_schema(), &mut NullLogger),
            UsageError::UnrecognizedParameter(Name::from_str("squeak")),
        );

        let fake_env = FakeEnv::new("--zero", "");
        assert_usage_error(
            TestConfigBuilder::from_env_like(&fake_env, fake_schema(), &mut NullLogger),
            UsageError::MissingValue(Name::from_str("zero")),
        );
    }

    #[test]
    // Tests the processing of unknown parameters as vars.
    fn test_unknown_vars() {
        let fake_env = FakeEnv::new(
            "--env",
            "FUCHSIA_TRUE=true FUCHSIA_FALSE=false FUCHSIA_ZERO=0 \
                 FUCHSIA_STRING=foo FUCHSIA_ARRAY_OF_NUMBER=1,2,3,4",
        );
        let under_test =
            TestConfigBuilder::from_env_like(&fake_env, fake_schema(), &mut NullLogger)
                .expect("Ok result");
        assert_eq!(
            under_test.param_values_by_name,
            HashMap::from_iter(vec![
                (Name::from_str("true"), Value::Bool(true)),
                (Name::from_str("false"), Value::Bool(false)),
                (Name::from_str("zero"), Value::Number(Number::from(0))),
                (Name::from_str("string"), Value::String(String::from("foo"))),
                (
                    Name::from_str("array_of_number"),
                    Value::Array(vec![
                        Value::Number(Number::from(1)),
                        Value::Number(Number::from(2)),
                        Value::Number(Number::from(3)),
                        Value::Number(Number::from(4))
                    ])
                ),
            ]),
        );

        let fake_env = FakeEnv::new(
            "--env",
            "FUCHSIA_ZERO_POINT_ONE=0.1 FUCHSIA_NEGATIVE_ZERO_POINT_ONE=-0.1",
        );
        let under_test =
            TestConfigBuilder::from_env_like(&fake_env, fake_schema(), &mut NullLogger)
                .expect("Ok result");
        assert_eq!(
            under_test
                .param_values_by_name
                .get(&Name::from_str("zero_point_one"))
                .unwrap()
                .as_f64()
                .unwrap(),
            0.1
        );
        assert_eq!(
            under_test
                .param_values_by_name
                .get(&Name::from_str("negative_zero_point_one"))
                .unwrap()
                .as_f64()
                .unwrap(),
            -0.1
        );
    }

    #[test]
    // Tests the processing of include parameter in args and vars.
    fn test_include() {
        // Successful reference from a command line.
        let temp_file = NamedTempFile::new().expect("Failed to create temporary file");
        let temp_file_path = temp_file.path().to_str().unwrap();

        fs::write(temp_file_path, "{}").expect("Failed to write to temporary file");

        let fake_env = FakeEnv::new(format!("--include={}", temp_file_path).as_str(), "");
        let under_test =
            TestConfigBuilder::from_env_like(&fake_env, fake_schema(), &mut NullLogger)
                .expect("Ok result");
        assert_eq!(under_test.include, vec![PathBuf::from_str(temp_file_path).unwrap(),]);

        // Successful reference from an environment variable.
        let fake_env =
            FakeEnv::new("--env", format!("FUCHSIA_INCLUDE={}", temp_file_path).as_str());
        let under_test =
            TestConfigBuilder::from_env_like(&fake_env, fake_schema(), &mut NullLogger)
                .expect("Ok result");
        assert_eq!(under_test.include, vec![PathBuf::from_str(temp_file_path).unwrap()],);

        temp_file.close().expect("Failed to close temporary file");

        // Non-existent file.
        let fake_env = FakeEnv::new("--include=/non_existent_file", "");
        assert_usage_error(
            TestConfigBuilder::from_env_like(&fake_env, fake_schema(), &mut NullLogger),
            UsageError::IncludedPathDoesNotExist(PathBuf::from("/non_existent_file")),
        );

        // File containing non-JSON.
        let temp_file = NamedTempFile::new().expect("Able to create temporary file");
        let temp_file_path = temp_file.path().to_str().unwrap();

        fs::write(temp_file_path, "spagga!").expect("Able to write to temporary file");

        let fake_env = FakeEnv::new(format!("--include={}", temp_file_path).as_str(), "");
        assert_matches!(
            TestConfigBuilder::from_env_like(&fake_env, fake_schema(), &mut NullLogger),
            Err(BuildError::FailedToParseInclude { path: p, source: _ })
                if p == PathBuf::from_str(temp_file_path).unwrap()
        );

        temp_file.close().expect("Failed to close temporary file");
    }

    #[test]
    // Tests the processing of require parameter in args and vars.
    fn test_require() {
        let fake_env = FakeEnv::new(
            "--env --require=a,b,c --a=a --b=b --c=c --d=d --e=e",
            "FUCHSIA_REQUIRE=c,d,e",
        );
        let under_test =
            TestConfigBuilder::from_env_like(&fake_env, fake_schema(), &mut NullLogger)
                .expect("Ok result");
        let mut sorted = under_test.require.clone();
        sorted.sort();
        assert_eq!(
            sorted,
            vec![
                Name::from_str("a"),
                Name::from_str("b"),
                Name::from_str("c"),
                Name::from_str("d"),
                Name::from_str("e"),
            ]
        );
    }

    #[test]
    // Tests the processing of prohibit parameter in args and vars.
    fn test_prohibit() {
        let fake_env = FakeEnv::new("--env --prohibit=a,b,c", "FUCHSIA_PROHIBIT=c,d,e");
        let under_test =
            TestConfigBuilder::from_env_like(&fake_env, fake_schema(), &mut NullLogger)
                .expect("Ok result");
        let mut sorted = under_test.prohibit.clone();
        sorted.sort();
        assert_eq!(
            sorted,
            vec![
                Name::from_str("a"),
                Name::from_str("b"),
                Name::from_str("c"),
                Name::from_str("d"),
                Name::from_str("e"),
            ]
        );
    }

    #[test]
    // Tests the processing of an include.
    fn test_include_params() {
        let temp_file = NamedTempFile::new().expect("Failed to create temporary file");
        let temp_file_path = temp_file.path().to_str().unwrap();

        fs::write(
            temp_file_path,
            r#"{// JSON5 allows comments and trailing commas
                     "true":true, "false":false, zero:0, "string":"foo",
                     "array_of_number":[1,2,3,4,],}"#,
        )
        .expect("Failed to write to temporary file");

        let fake_env = FakeEnv::new(format!("--include={}", temp_file_path).as_str(), "");
        let under_test =
            TestConfigBuilder::from_env_like(&fake_env, fake_schema(), &mut NullLogger)
                .expect("Ok result");
        assert_eq!(under_test.include, vec![PathBuf::from_str(temp_file_path).unwrap()]);
        assert_eq!(
            under_test.param_values_by_name,
            HashMap::from_iter(vec![
                (Name::from_str("true"), Value::Bool(true)),
                (Name::from_str("false"), Value::Bool(false)),
                (Name::from_str("zero"), Value::Number(Number::from(0))),
                (Name::from_str("string"), Value::String(String::from("foo"))),
                (
                    Name::from_str("array_of_number"),
                    Value::Array(vec![
                        Value::Number(Number::from(1)),
                        Value::Number(Number::from(2)),
                        Value::Number(Number::from(3)),
                        Value::Number(Number::from(4))
                    ])
                ),
            ])
        );

        temp_file.close().expect("Failed to close temporary file");
    }

    #[test]
    // Tests the processing of nested includes.
    fn test_nested_includes() {
        let temp_file_outer = NamedTempFile::new().expect("Failed to create temporary file");
        let temp_file_outer_path = temp_file_outer.path().to_str().unwrap();
        let temp_file_inner = NamedTempFile::new().expect("Failed to create temporary file");
        let temp_file_inner_path = temp_file_inner.path().to_str().unwrap();

        fs::write(temp_file_outer_path, format!("{{\"include\":[\"{}\"]}}", temp_file_inner_path))
            .expect("Failed to write to temporary file");
        fs::write(temp_file_inner_path, "{}").expect("Failed to write to temporary file");

        let fake_env = FakeEnv::new(format!("--include={}", temp_file_outer_path).as_str(), "");
        let under_test =
            TestConfigBuilder::from_env_like(&fake_env, fake_schema(), &mut NullLogger)
                .expect("Ok result");
        assert_eq!(
            under_test.include,
            vec![
                PathBuf::from_str(temp_file_outer_path).unwrap(),
                PathBuf::from_str(temp_file_inner_path).unwrap()
            ]
        );

        temp_file_inner.close().expect("Failed to close temporary file");
        temp_file_outer.close().expect("Failed to close temporary file");
    }

    #[test]
    // Tests the processing of a JSON object in an include file.
    fn test_object() {
        let temp_file = NamedTempFile::new().expect("Failed to create temporary file");
        let temp_file_path = temp_file.path().to_str().unwrap();

        fs::write(temp_file_path, r#"{"object":{"foo":1,"bar":2}}"#)
            .expect("Failed to write to temporary file");

        let fake_env = FakeEnv::new(format!("--include={}", temp_file_path).as_str(), "");
        let under_test =
            TestConfigBuilder::from_env_like(&fake_env, fake_schema(), &mut NullLogger)
                .expect("Ok result");
        let mut object_map = serde_json::Map::new();
        object_map.insert(String::from("foo"), Value::Number(Number::from(1)));
        object_map.insert(String::from("bar"), Value::Number(Number::from(2)));
        assert_eq!(under_test.include, vec![PathBuf::from_str(temp_file_path).unwrap()]);
        assert_eq!(
            under_test.param_values_by_name,
            HashMap::from_iter(vec![(Name::from_str("object"), Value::Object(object_map)),])
        );

        temp_file.close().expect("Failed to close temporary file");
    }

    #[test]
    // Tests strictness rules implementation.
    fn test_strict() {
        // Multiple non-strict assignments yields the last value assigned.
        let fake_env = FakeEnv::new("---foo=a --foo=b", "");
        let under_test =
            TestConfigBuilder::from_env_like(&fake_env, fake_schema(), &mut NullLogger)
                .expect("Ok result");
        assert_eq!(
            String::from("b"),
            under_test.param_values_by_name.get(&Name::from_str("foo")).unwrap().as_str().unwrap(),
        );

        // One strict assignment after an override returns the override value.
        let fake_env = FakeEnv::new("---foo=a --strict --foo=b", "");
        let under_test =
            TestConfigBuilder::from_env_like(&fake_env, fake_schema(), &mut NullLogger)
                .expect("Ok result");
        assert_eq!(
            String::from("a"),
            under_test.param_values_by_name.get(&Name::from_str("foo")).unwrap().as_str().unwrap(),
        );

        // Multiple strict assignments are not allowed.
        let fake_env = FakeEnv::new("--strict --foo=a --foo=b", "");
        assert_usage_error(
            TestConfigBuilder::from_env_like(&fake_env, fake_schema(), &mut NullLogger),
            UsageError::ParamAlreadyStrictlyAssigned(Name::from_str("foo")),
        );

        // Multiple strict assignments are not allowed, even for overridden parameters.
        let fake_env = FakeEnv::new("--foo=a --strict --foo=b --foo=c", "");
        assert_usage_error(
            TestConfigBuilder::from_env_like(&fake_env, fake_schema(), &mut NullLogger),
            UsageError::ParamAlreadyStrictlyAssigned(Name::from_str("foo")),
        );

        // Multiple stricts are ok.
        let fake_env = FakeEnv::new("--strict --strict", "");
        let _ = TestConfigBuilder::from_env_like(&fake_env, fake_schema(), &mut NullLogger)
            .expect("Ok result");

        // Can't set strict to false.
        let fake_env = FakeEnv::new("--no-strict", "");
        assert_usage_error(
            TestConfigBuilder::from_env_like(&fake_env, fake_schema(), &mut NullLogger),
            UsageError::InvalidStrictValue(String::from("false")),
        );
    }

    #[test]
    // Tests that an unknown parameter assigned in JSON causes an error.
    fn test_include_unknown() {
        let temp_file = NamedTempFile::new().expect("Failed to create temporary file");
        let temp_file_path = temp_file.path().to_str().unwrap();

        fs::write(temp_file_path, r#"{tunnels: 4}"#).expect("Failed to write to temporary file");

        let fake_env = FakeEnv::new(format!("--include={}", temp_file_path).as_str(), "");
        assert_usage_error(
            TestConfigBuilder::from_env_like(&fake_env, fake_schema(), &mut NullLogger),
            UsageError::UnrecognizedParameter(Name::from_str("tunnels")),
        );

        temp_file.close().expect("Failed to close temporary file");
    }

    #[test]
    // Tests that array assignments are aggregated.
    fn test_array_aggregation() {
        let fake_env = FakeEnv::new("--strict --array_of_number=1 --array_of_number=2,3", "");
        let under_test =
            TestConfigBuilder::from_env_like(&fake_env, fake_schema(), &mut NullLogger)
                .expect("Ok result");
        assert_eq!(
            under_test.param_values_by_name,
            HashMap::from_iter(vec![(
                Name::from_str("array_of_number"),
                Value::Array(vec![
                    Value::Number(Number::from(1)),
                    Value::Number(Number::from(2)),
                    Value::Number(Number::from(3))
                ])
            )])
        );
    }

    #[test]
    // Tests final builder validation.
    fn test_final_validation() {
        let fake_env = FakeEnv::new("--require=chunky", "");
        assert_usage_error(
            TestConfigBuilder::from_env_like(&fake_env, fake_schema(), &mut NullLogger),
            UsageError::UnknownRequiredParameter(Name::from_str("chunky")),
        );

        let fake_env = FakeEnv::new("--require=foo", "");
        assert_usage_error(
            TestConfigBuilder::from_env_like(&fake_env, fake_schema(), &mut NullLogger),
            UsageError::MissingRequiredParameter(Name::from_str("foo")),
        );

        let fake_env = FakeEnv::new("--prohibit=chunky", "");
        assert_usage_error(
            TestConfigBuilder::from_env_like(&fake_env, fake_schema(), &mut NullLogger),
            UsageError::UnknownProhibitedParameter(Name::from_str("chunky")),
        );

        let fake_env = FakeEnv::new("--prohibit=foo --foo=bar", "");
        assert_usage_error(
            TestConfigBuilder::from_env_like(&fake_env, fake_schema(), &mut NullLogger),
            UsageError::DefinedProhibitedParameter(Name::from_str("foo")),
        );

        // In order to test final validation against the schema, we need to assign the wrong
        // type of value to a parameter in a JSON file. Unknown parameter names are caught
        // earlier regardless of the source, and type mismatches are caught early when the
        // parsers are used (command line and environment variables).
        let temp_file = NamedTempFile::new().expect("Failed to create temporary file");
        let temp_file_path = temp_file.path().to_str().unwrap();

        fs::write(temp_file_path, r#"{true: "horse"}"#).expect("Failed to write to temporary file");

        let fake_env = FakeEnv::new(format!("--include={}", temp_file_path).as_str(), "");
        assert_usage_error(
            TestConfigBuilder::from_env_like(&fake_env, fake_schema(), &mut NullLogger),
            UsageError::SchemaTypeMismatch {
                parameter: String::from("true"),
                detail: String::from("The value must be boolean"),
            },
        );

        temp_file.close().expect("Failed to close temporary file");
    }
}
