// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

pub mod args;
pub mod error;

use crate::shell::args::args_to_json;
use crate::shell::error::ShellError;
use anyhow::{Error, Result};
use scrutiny::engine::dispatcher::{ControllerDispatcher, DispatcherError};
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::fmt::Write;
use std::sync::{Arc, RwLock};
use tracing::info;

/// A CommandResponse is an internal type to define whether a command was acted
/// on by a submodule or not.
#[derive(Eq, PartialEq)]
enum CommandResponse {
    Executed,
    NotFound,
}

pub struct Shell {
    dispatcher: Arc<RwLock<ControllerDispatcher>>,
    silent_mode: bool,
}

impl Shell {
    pub fn new(dispatcher: Arc<RwLock<ControllerDispatcher>>, silent_mode: bool) -> Self {
        Self { dispatcher, silent_mode }
    }

    /// Parses a command returning the namespace and arguments as a json value.
    /// This can fail if any of the command arguments are invalid. This function
    /// does not check whether the command exists just verifies the input is
    /// sanitized.
    fn parse_command(command: impl Into<String>) -> Result<(String, Value)> {
        let mut tokens: VecDeque<String> =
            command.into().split_whitespace().map(|s| String::from(s)).collect();
        if tokens.len() == 0 {
            return Err(Error::new(ShellError::empty_command()));
        }
        // Transform foo.bar to /foo/bar
        let head = tokens.pop_front().unwrap();
        let namespace = if head.starts_with("/") {
            head.to_string()
        } else {
            "/api/".to_owned() + &str::replace(&head, ".", "/")
        };

        // Parse the command arguments.
        let empty_command: HashMap<String, String> = HashMap::new();
        let mut query = json!(empty_command);
        if !tokens.is_empty() {
            if tokens.front().unwrap().starts_with("`") {
                let mut body = Vec::from(tokens).join(" ");
                if body.len() > 2 && body.ends_with("`") {
                    body.remove(0);
                    body.pop();
                    match serde_json::from_str(&body) {
                        Ok(expr) => {
                            query = expr;
                        }
                        Err(err) => {
                            return Err(Error::new(err));
                        }
                    }
                } else {
                    return Err(Error::new(ShellError::unescaped_json_string(body)));
                }
            } else if tokens.front().unwrap().starts_with("--") {
                query = args_to_json(&tokens);
            }
        }
        Ok((namespace, query))
    }

    /// Attempts to transform the command into a dispatcher command to see if
    /// any plugin services that url. Two syntaxes are supported /foo/bar or
    /// foo.bar both will be translated into /foo/bar before being sent to the
    /// dispatcher.
    fn plugin(
        &mut self,
        command: String,
        out_buffer: &mut impl std::fmt::Write,
    ) -> Result<CommandResponse> {
        let command_result = Self::parse_command(command);
        if let Err(err) = command_result {
            if let Some(shell_error) = err.downcast_ref::<ShellError>() {
                if let ShellError::EmptyCommand = shell_error {
                    return Ok(CommandResponse::Executed);
                }
            }
            writeln!(out_buffer, "Error: {:?}", err)?;
            return Ok(CommandResponse::Executed);
        }
        let (namespace, query) = command_result.unwrap();

        let result = self.dispatcher.read().unwrap().query(namespace, query);
        match result {
            Err(e) => {
                if let Some(dispatch_error) = e.downcast_ref::<DispatcherError>() {
                    match dispatch_error {
                        DispatcherError::NamespaceDoesNotExist(_) => {
                            return Ok(CommandResponse::NotFound);
                        }
                        _ => {}
                    }
                }
                writeln!(out_buffer, "Error: {:?}", e)?;
                Ok(CommandResponse::Executed)
            }
            Ok(result) => {
                writeln!(out_buffer, "{}", serde_json::to_string_pretty(&result).unwrap())?;
                Ok(CommandResponse::Executed)
            }
        }
    }

    /// Executes a single command.
    pub fn execute(&mut self, command: String) -> Result<String> {
        if command.is_empty() || command.starts_with("#") {
            return Ok(String::new());
        }
        info!(%command);

        let mut output = String::new();
        if self.plugin(command.clone(), &mut output)? == CommandResponse::Executed {
        } else {
            writeln!(output, "scrutiny: command not found: {}", command)?;
        }
        if !self.silent_mode {
            print!("{}", output);
        }
        Ok(output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_command_errors() {
        assert_eq!(Shell::parse_command("foo").is_ok(), true);
        assert_eq!(Shell::parse_command("foo `{}`").is_ok(), true);
        assert_eq!(Shell::parse_command("foo `{\"abc\": 1}`").is_ok(), true);
        assert_eq!(Shell::parse_command("foo `{aaa}`").is_ok(), false);
        assert_eq!(Shell::parse_command("foo `{").is_ok(), false);
        assert_eq!(Shell::parse_command("foo `").is_ok(), false);
    }
    #[test]
    fn test_parse_command_tokens() {
        assert_eq!(Shell::parse_command("foo").unwrap().0, "/api/foo");
        assert_eq!(Shell::parse_command("foo `{\"a\": 1}`").unwrap().1, json!({"a": 1}));
    }

    #[test]
    fn test_help_ok() {
        assert_eq!(Shell::parse_command("help").is_ok(), true);
        assert_eq!(Shell::parse_command("help zbi.bootfs").is_ok(), true);
    }
}
