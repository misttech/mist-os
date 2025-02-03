// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// TODO(https://fxbug.dev/378521591) Remove once implementations below are used.
#![allow(dead_code)]

const DEBUG_ARG: &str = "--debug";

// Wrapper to allow faking of std::env.
pub trait EnvLike {
    /// Returns an iterator over the program's arguments. Unlike `std::env::args``, this method
    /// filters non-unicode arguments rather than panicking. It also omits the first item returned
    /// by `std::env::args`` (the program name).
    fn args(&self) -> impl Iterator<Item = String>;

    /// Returns an iterator over the process's environment variables and their respective values.
    /// Unlike `std::env::vars``, this method filters non-unicode names and values rather than
    /// panicking.
    fn vars(&self) -> impl Iterator<Item = (String, String)>;

    /// Returns the value of the environment variable identified by `key`. This is a wrapper for
    /// `std::env::var` and has the same behavior.
    fn var(&self, key: &str) -> Result<String, std::env::VarError>;

    // Determines if this `EnvLike` contains a '--debug' command line argument.
    fn contains_debug_arg(&self) -> bool {
        self.args().any(|a| a == DEBUG_ARG)
    }
}

/// Implements `EnvLike` using `std::env`.
pub struct ActualEnv;

impl EnvLike for ActualEnv {
    fn args(&self) -> impl Iterator<Item = String> {
        std::env::args_os().skip(1).filter_map(|os_string| os_string.into_string().ok())
    }

    fn vars(&self) -> impl Iterator<Item = (String, String)> {
        std::env::vars_os().filter_map(|(os_name, os_value)| {
            let name = os_name.into_string().ok()?;
            let value = os_value.into_string().ok()?;
            Some((name, value))
        })
    }

    fn var(&self, key: &str) -> Result<String, std::env::VarError> {
        std::env::var(key)
    }
}

#[cfg(test)]
pub mod testutils {
    use super::*;
    use std::collections::HashMap;

    /// Fake `EnvLike` for tests.
    pub struct FakeEnv {
        args: Vec<String>,
        vars: HashMap<String, String>,
    }

    impl FakeEnv {
        /// Creates a `FakeEnv` from a space-separated string of arguments and a space-separated
        /// string of variable assignments.
        pub fn new(args: &str, vars: &str) -> Self {
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
                let (key, rest) = assignment.split_once("=").unwrap();
                (String::from(key), String::from(rest))
            })
            .collect::<HashMap<String, String>>()
    }
}
