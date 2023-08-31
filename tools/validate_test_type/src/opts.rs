// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{ensure, Error};
use camino::Utf8PathBuf;
use std::str::FromStr;
use structopt::StructOpt;

#[derive(Debug, PartialEq)]
pub enum ValidateType {
    Hermetic,
}

type ParseError = &'static str;

impl FromStr for ValidateType {
    type Err = ParseError;
    fn from_str(validation_type: &str) -> Result<Self, Self::Err> {
        match validation_type.to_ascii_lowercase().as_str() {
            "hermetic" => Ok(ValidateType::Hermetic),
            _ => Err("Could not parse --validate"),
        }
    }
}

impl std::fmt::Display for ValidateType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ValidateType::Hermetic => f.write_str("hermetic"),
        }
    }
}

#[derive(StructOpt, Debug)]
pub struct Opt {
    #[structopt(long = "test-group-name")]
    pub test_group_name: String,

    #[structopt(short = "i", long = "test_list")]
    /// Path to the test list file.
    pub test_list: Utf8PathBuf,

    #[structopt(short = "t", long = "test-components")]
    /// Path to the test components list file.
    pub test_components_list: Utf8PathBuf,

    /// Validation type.
    /// Possible values: [ hermetic ]
    #[structopt(short = "v", long = "validate", default_value = "hermetic")]
    pub validation_type: ValidateType,

    #[structopt(short = "b", long = "build-dir")]
    /// Path to the build directory.
    pub build_dir: Utf8PathBuf,

    #[structopt(short = "o", long = "output")]
    /// Path to an optional output file with the results.
    pub output: Option<Utf8PathBuf>,
}

impl Opt {
    pub fn validate(&self) -> Result<(), Error> {
        ensure!(self.test_list.exists(), "test_list {:?} does not exist", self.test_list);
        ensure!(
            self.test_components_list.exists(),
            "test_components_list {:?} does not exist",
            self.test_components_list
        );
        ensure!(self.build_dir.exists(), "build-dir {:?} does not exist", self.build_dir);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;
    use tempfile::{tempdir, NamedTempFile};

    #[test]
    fn test_validate_type_from_str() {
        assert_eq!(ValidateType::from_str("hermetic"), Ok(ValidateType::Hermetic));
        assert_eq!(ValidateType::from_str("unknown"), Err("Could not parse --validate"));
    }

    #[test]
    fn test_opt() {
        // Modify these paths to point to actual or non-existent paths for testing
        let test_list = NamedTempFile::new().expect("Failed to create temporary test_list");
        let test_components_list =
            NamedTempFile::new().expect("Failed to create temporary test_components_list");
        let build_dir = tempdir().expect("Failed to create temporary build_dir");
        let output = build_dir.path().join("output.txt");

        let opt = Opt {
            test_group_name: "test_group".into(),
            test_list: Utf8PathBuf::from_path_buf(test_list.path().into()).unwrap(),
            test_components_list: Utf8PathBuf::from_path_buf(test_components_list.path().into())
                .unwrap(),
            validation_type: ValidateType::Hermetic,
            build_dir: Utf8PathBuf::from_path_buf(build_dir.path().into()).unwrap(),
            output: Some(Utf8PathBuf::from_path_buf(output).unwrap()),
        };

        // Test valid paths
        assert!(opt.validate().is_ok());

        // Test missing paths
        let invalid_opt = Opt {
            test_group_name: "test_group".into(),
            test_list: Utf8PathBuf::from_str("/tmp/nonexistent").unwrap(),
            test_components_list: Utf8PathBuf::from_str("/tmp/nonexistent").unwrap(),
            validation_type: ValidateType::Hermetic,
            build_dir: Utf8PathBuf::from_str("/tmp/nonexistent").unwrap(),
            output: None,
        };
        assert!(invalid_opt.validate().is_err());
    }
}
