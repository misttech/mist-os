// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#[cfg(test)]
use ffx_isolate::test::TestingError;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CLIArgsInfo {
    /// The name of the command.
    pub name: String,
    /// A short description of the command's functionality.
    pub description: String,
    /// Examples of usage
    pub examples: Vec<String>,
    /// Flags
    pub flags: Vec<FlagInfo>,
    /// Notes about usage
    pub notes: Vec<String>,
    /// The subcommands.
    pub commands: Vec<SubCommandInfo>,
    /// Positional args
    pub positionals: Vec<PositionalInfo>,
    /// Error code information
    pub error_codes: Vec<ErrorCodeInfo>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub enum FlagKind {
    Option { arg_name: String },
    Switch,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct FlagInfo {
    pub kind: FlagKind,
    /// The optionality of the flag.
    pub optionality: String,
    /// The long string of the flag.
    pub long: String,
    /// The single character short indicator
    /// for trhis flag.
    pub short: Option<char>,
    /// The description of the flag.
    pub description: String,
    /// Visibility in the help for this argument.
    /// `false` indicates this argument will not appear
    /// in the help message.
    pub hidden: bool,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PositionalInfo {
    /// Name of the argument.
    pub name: String,
    /// Description of the argument.
    pub description: String,
    /// Optionality of the argument.
    pub optionality: String,
    /// Visibility in the help for this argument.
    /// `false` indicates this argument will not appear
    /// in the help message.
    pub hidden: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SubCommandInfo {
    /// The subcommand name.
    pub name: String,
    /// The information about the subcommand.
    pub command: CLIArgsInfo,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ErrorCodeInfo {
    /// The code value.
    pub code: i32,
    /// Short description about what this code indicates.
    pub description: String,
}

impl PositionalInfo {
    #[cfg(test)]
    pub fn is_compatible(
        &self,
        other: &PositionalInfo,
    ) -> Result<(), ffx_isolate::test::TestingError> {
        // name, description, hidden do not matter for positional args.
        if self.optionality != other.optionality {
            return Err(TestingError::ExecutionError(anyhow::anyhow!(
                "{self:?} positional mismatch for {other:?}"
            )));
        }
        Ok(())
    }
}

impl FlagInfo {
    #[cfg(test)]
    pub fn is_compatible(&self, other: &FlagInfo) -> Result<(), ffx_isolate::test::TestingError> {
        if self.long != other.long {
            return Err(TestingError::ExecutionError(anyhow::anyhow!(
                "{self:?} flag mismatch for {other:?}"
            )));
        }
        if self.short != other.short {
            return Err(TestingError::ExecutionError(anyhow::anyhow!(
                "{self:?}  flag mismatch for {other:?}"
            )));
        }
        if self.kind != other.kind {
            return Err(TestingError::ExecutionError(anyhow::anyhow!(
                "{self:?}  flag mismatch for {other:?}"
            )));
        }
        if self.optionality != other.optionality {
            return Err(TestingError::ExecutionError(anyhow::anyhow!(
                "{self:?}  flag mismatch for {other:?}"
            )));
        }

        // hidden and description do not affect compatibility
        Ok(())
    }
}

impl CLIArgsInfo {
    pub(crate) fn get_golden_data(&self) -> CLIArgsInfo {
        CLIArgsInfo {
            name: self.name.clone(),
            description: self.description.clone(),
            examples: self.examples.clone(),
            flags: self.flags.clone(),
            notes: self.notes.clone(),
            commands: vec![],
            positionals: self.positionals.clone(),
            error_codes: self.error_codes.clone(),
        }
    }
}
