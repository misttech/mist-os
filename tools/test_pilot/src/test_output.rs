// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::errors::TestRunError;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::fs;
use std::io::BufReader;
use std::os::unix::process::ExitStatusExt;
use std::path::PathBuf;

/// Distinguished exit status used to indicate the test was run as requested but that the
/// test itself failed.
const FAIL_EXIT_STATUS: i32 = 86;

/// The body of the test output summary.
#[derive(Serialize, Deserialize, Debug, Default, PartialEq)]
pub struct Summary {
    /// Properties concerning the overall test run.
    #[serde(flatten)]
    pub common: SummaryCommonProperties,

    /// Cases by case name.
    #[serde(default)]
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    pub cases: HashMap<String, SummaryCase>,
}

impl Summary {
    /// Merges `Summary` read from a file into self. Return Ok(true) if the file was merge
    /// successfully, Ok(false) if the file did not exist.
    pub fn maybe_merge_file(&mut self, path: &PathBuf) -> Result<bool, TestRunError> {
        if let Ok(file) = fs::File::open(&path) {
            let mut reader = BufReader::new(file);
            let summary_from_file: Summary = serde_json::from_reader(&mut reader)
                .map_err(|e| TestRunError::OutputSummaryRead { path: path.clone(), source: e })?;
            self.merge(summary_from_file);
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Writes this summary to a file.
    pub fn write(&self, path: &PathBuf) -> Result<(), TestRunError> {
        fs::write(path, serde_json::to_string_pretty(self).unwrap())
            .map_err(|e| TestRunError::OutputSummaryWrite { path: path.clone(), source: e })?;

        Ok(())
    }

    /// Merge `Summary` into self.
    fn merge(&mut self, other: Summary) {
        self.common.merge(other.common);
        for (other_case_name, other_case) in other.cases {
            match &mut self.cases.get_mut(other_case_name.as_str()) {
                Some(case_properties) => {
                    case_properties.merge(other_case);
                }
                None => {
                    self.cases.insert(other_case_name, other_case);
                }
            }
        }
    }
}

/// Test output regarding a single case.
#[derive(Serialize, Deserialize, Debug, Default, Clone, PartialEq)]
pub struct SummaryCase {
    #[serde(flatten)]
    pub common: SummaryCommonProperties,
}

impl SummaryCase {
    fn merge(&mut self, other: SummaryCase) {
        self.common.merge(other.common);
    }
}

/// Properties shared by the overall summary and by cases.
#[derive(Serialize, Deserialize, Debug, Default, Clone, PartialEq)]
pub struct SummaryCommonProperties {
    #[serde(default)]
    #[serde(skip_serializing_if = "is_zero")]
    pub duration: i64,

    pub outcome: String,

    #[serde(default)]
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    pub artifacts: HashMap<PathBuf, SummaryArtifact>,

    #[serde(flatten)]
    pub extension: HashMap<String, Value>,
}

impl SummaryCommonProperties {
    fn merge(&mut self, other: SummaryCommonProperties) {
        if other.duration != 0 {
            self.duration = other.duration;
        }

        if !other.outcome.is_empty() {
            self.outcome = other.outcome;
        }

        for (other_artifact_name, other_artifact_properties) in other.artifacts {
            match &mut self.artifacts.get_mut(&other_artifact_name) {
                Some(artifact_properties) => {
                    artifact_properties.merge(other_artifact_properties);
                }
                None => {
                    self.artifacts.insert(other_artifact_name, other_artifact_properties);
                }
            }
        }

        for (other_extension_name, other_extension_properties) in other.extension {
            self.extension.insert(other_extension_name, other_extension_properties);
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Default, Clone, PartialEq)]
pub struct SummaryArtifact {
    #[serde(rename = "type")]
    pub artifact_type: String,
}

impl SummaryArtifact {
    fn merge(&mut self, other: SummaryArtifact) {
        if !other.artifact_type.is_empty() {
            self.artifact_type = other.artifact_type;
        }
    }
}

fn is_zero(to_test: &i64) -> bool {
    *to_test == 0
}

const EXECUTION_LOG_FILE_PATH: &str = "execution_log.json";
const TEST_CONFIG_FILE_PATH: &str = "test_config.json";
const TEST_OUTPUT_SUMMARY_FILE_PATH: &str = "test_output_summary.json";
const TEST_STDOUT_FILE_PATH: &str = "host_binary_stdout.txt";
const TEST_STDERR_FILE_PATH: &str = "host_binary_stderr.txt";

/// Creates path values based on output directory structure.
pub struct OutputDirectory<'a> {
    path: &'a PathBuf,
}

impl<'a> OutputDirectory<'a> {
    /// Creates a new `OutputDirectory` located at `Path`.
    pub fn new(path: &'a PathBuf) -> Self {
        Self { path }
    }

    /// Returns the full path of the main summary.
    pub fn main_summary(&self) -> PathBuf {
        self.path.join(TEST_OUTPUT_SUMMARY_FILE_PATH)
    }

    /// Returns the full path of the summary in a specified subdirectory.
    pub fn subdir_summary(&self, subdir: &str) -> PathBuf {
        self.path.join(subdir).join(TEST_OUTPUT_SUMMARY_FILE_PATH)
    }

    /// Returns the full path of the summary for a specified postprocessor.
    pub fn postprocessor_summary(&self, binary: &PathBuf) -> PathBuf {
        self.subdir_summary(
            binary
                .file_name()
                .expect("binary path has file name")
                .to_str()
                .expect("file name is ansi"),
        )
    }

    /// Returns the full path of the test configuration.
    pub fn test_config(&self) -> PathBuf {
        self.path.join(TEST_CONFIG_FILE_PATH)
    }

    /// Returns the full path of the test's stdout file.
    pub fn test_stdout(&self) -> PathBuf {
        self.path.join(TEST_STDOUT_FILE_PATH)
    }

    /// Returns the full path of the test's stderr file.
    pub fn test_stderr(&self) -> PathBuf {
        self.path.join(TEST_STDERR_FILE_PATH)
    }

    /// Returns the full path of a post-processor's stdout file.
    pub fn postprocessor_stdout(&self, binary: &PathBuf) -> PathBuf {
        self.path.join(format!(
            "{}_stdout.txt",
            binary
                .file_name()
                .expect("binary path has file name")
                .to_str()
                .expect("file name is ansi")
        ))
    }

    /// Returns the full path of a post-processor's stderr file.
    pub fn postprocessor_stderr(&self, binary: &PathBuf) -> PathBuf {
        self.path.join(format!(
            "{}_stderr.txt",
            binary
                .file_name()
                .expect("binary path has file name")
                .to_str()
                .expect("file name is ansi")
        ))
    }

    /// Returns the full path of the execution log.
    pub fn execution_log(&self) -> PathBuf {
        self.path.join(EXECUTION_LOG_FILE_PATH)
    }

    /// Converts a full path into a path relative to the output directory.
    pub fn make_relative(&self, path: &PathBuf) -> Option<PathBuf> {
        path.strip_prefix(self.path).ok().map(|p| p.to_path_buf())
    }
}

/// Determines whether `exit_status` indicates the test run ran correctly, but the test itself
/// failed.
pub fn is_fail_exit_status(exit_status: std::process::ExitStatus) -> bool {
    exit_status.into_raw() == FAIL_EXIT_STATUS
}

/// Returns an outcome string from an exit status.
pub fn outcome_from_exit_status(exit_status: std::process::ExitStatus) -> &'static str {
    match exit_status.into_raw() {
        0 => "PASSED",
        FAIL_EXIT_STATUS => "FAILED",
        _ => "INTERNAL_ERROR",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fs::File;
    use std::os::unix::process::ExitStatusExt;
    use std::process::ExitStatus;
    use tempfile::tempdir;

    fn artifact(artifact_type: &str) -> SummaryArtifact {
        SummaryArtifact { artifact_type: String::from(artifact_type) }
    }

    #[fuchsia::test]
    fn test_merge_common_properties() {
        let mut under_test = SummaryCommonProperties {
            duration: 1,
            outcome: String::from("test outcome a"),
            artifacts: [
                (PathBuf::from("a"), artifact("a_type")),
                (PathBuf::from("b"), artifact("b_type")),
            ]
            .into(),
            extension: [
                (String::from("x"), Value::from("x_value")),
                (String::from("y"), Value::from("y_value")),
            ]
            .into(),
        };

        // Non-trivial values from argument take precedent. New artifacts are inserted.
        // Existing artifacts with non-trivial values get updated.
        under_test.merge(SummaryCommonProperties {
            duration: 3,
            outcome: String::from("test outcome b"),
            artifacts: [
                (PathBuf::from("a"), artifact("new_a_type")),
                (PathBuf::from("c"), artifact("c_type")),
            ]
            .into(),
            extension: [
                (String::from("x"), Value::from("new_x_value")),
                (String::from("z"), Value::from("z_value")),
            ]
            .into(),
        });
        assert_eq!(
            under_test,
            SummaryCommonProperties {
                duration: 3,
                outcome: String::from("test outcome b"),
                artifacts: [
                    (PathBuf::from("a"), artifact("new_a_type")),
                    (PathBuf::from("b"), artifact("b_type")),
                    (PathBuf::from("c"), artifact("c_type")),
                ]
                .into(),
                extension: [
                    (String::from("x"), Value::from("new_x_value")),
                    (String::from("y"), Value::from("y_value")),
                    (String::from("z"), Value::from("z_value")),
                ]
                .into(),
            }
        );

        // Trivial values from argument are not copied. For existing artifacts, trivial
        // values from argument are not copied.
        under_test.merge(SummaryCommonProperties {
            duration: 0,
            outcome: String::from(""),
            artifacts: [(PathBuf::from("a"), artifact(""))].into(),
            extension: HashMap::new(),
        });
        assert_eq!(
            under_test,
            SummaryCommonProperties {
                duration: 3,
                outcome: String::from("test outcome b"),
                artifacts: [
                    (PathBuf::from("a"), artifact("new_a_type")),
                    (PathBuf::from("b"), artifact("b_type")),
                    (PathBuf::from("c"), artifact("c_type")),
                ]
                .into(),
                extension: [
                    (String::from("x"), Value::from("new_x_value")),
                    (String::from("y"), Value::from("y_value")),
                    (String::from("z"), Value::from("z_value")),
                ]
                .into(),
            }
        );
    }

    fn case(outcome: &str) -> SummaryCase {
        SummaryCase {
            common: SummaryCommonProperties {
                outcome: String::from(outcome),
                ..Default::default()
            },
        }
    }

    #[fuchsia::test]
    fn test_merge_cases() {
        let mut under_test = Summary {
            common: SummaryCommonProperties::default(),
            cases: [
                (String::from("case_a"), case("case_a_outcome")),
                (String::from("case_b"), case("case_b_outcome")),
            ]
            .into(),
        };

        // Non-trivial values from argument take precedent. New artifacts are inserted.
        // Existing artifacts with non-trivial values get updated.
        under_test.merge(Summary {
            common: SummaryCommonProperties::default(),
            cases: [
                (String::from("case_a"), case("case_a_new_outcome")),
                (String::from("case_c"), case("case_c_outcome")),
            ]
            .into(),
        });
        assert_eq!(
            under_test,
            Summary {
                common: SummaryCommonProperties::default(),
                cases: [
                    (String::from("case_a"), case("case_a_new_outcome")),
                    (String::from("case_b"), case("case_b_outcome")),
                    (String::from("case_c"), case("case_c_outcome")),
                ]
                .into()
            }
        );
    }

    #[fuchsia::test]
    fn test_outcome_from_exit_status() {
        assert_eq!(outcome_from_exit_status(ExitStatus::from_raw(0)), "PASSED");
        assert_eq!(outcome_from_exit_status(ExitStatus::from_raw(FAIL_EXIT_STATUS)), "FAILED");
        assert_eq!(outcome_from_exit_status(ExitStatus::from_raw(1)), "INTERNAL_ERROR");
    }

    #[fuchsia::test]
    fn test_summary_write() {
        let under_test = Summary {
            common: SummaryCommonProperties {
                duration: 3,
                outcome: String::from("test outcome b"),
                artifacts: [
                    (PathBuf::from("a"), artifact("a_type")),
                    (PathBuf::from("b"), artifact("b_type")),
                    (PathBuf::from("c"), artifact("c_type")),
                ]
                .into(),
                extension: [
                    (String::from("x"), Value::from("x_value")),
                    (String::from("y"), Value::from("y_value")),
                    (String::from("z"), Value::from("z_value")),
                ]
                .into(),
            },
            cases: [
                (String::from("case_a"), case("case_a_outcome")),
                (String::from("case_b"), case("case_b_outcome")),
            ]
            .into(),
        };

        let temp_dir = tempdir().expect("to create temporary directory");
        let file_path = temp_dir.path().join("test_output_summary.json");

        under_test.write(&file_path).expect("to write summary");

        let file = File::open(&file_path).expect("to open summary");
        let mut reader = BufReader::new(file);
        let file_contents: Value = serde_json5::from_reader(&mut reader).expect("to read summary");
        assert_eq!(
            file_contents,
            json!({
                "artifacts": {
                    "a": {"type": "a_type"},
                    "b": {"type": "b_type"},
                    "c": {"type": "c_type"}
                },
                "cases": {
                    "case_a": {"outcome": "case_a_outcome"},
                    "case_b": {"outcome": "case_b_outcome"}
                },
                "duration": 3,
                "outcome": "test outcome b",
                "x": "x_value",
                "y": "y_value",
                "z": "z_value"
            })
        );

        temp_dir.close().expect("to close temporary directory");
    }

    #[fuchsia::test]
    fn test_summary_maybe_merge_file() {
        let temp_dir = tempdir().expect("to create temporary directory");
        let mut under_test = Summary::default();

        assert_eq!(
            false,
            under_test
                .maybe_merge_file(&temp_dir.path().join("does_not_exist.json"))
                .expect("success"),
        );

        assert_eq!(Summary::default(), under_test);

        let file_path = temp_dir.path().join("test_output_summary.json");
        fs::write(
            &file_path,
            serde_json::to_string_pretty(&json!({
                "artifacts": {
                    "a": {"type": "a_type"},
                    "b": {"type": "b_type"},
                    "c": {"type": "c_type"}
                },
                "cases": {
                    "case_a": {"outcome": "case_a_outcome"},
                    "case_b": {"outcome": "case_b_outcome"}
                },
                "duration": 3,
                "outcome": "test outcome b",
                "x": "x_value",
                "y": "y_value",
                "z": "z_value"
            }))
            .unwrap(),
        )
        .expect("json write succeeds");

        assert_eq!(true, under_test.maybe_merge_file(&file_path).expect("success"),);

        assert_eq!(
            Summary {
                common: SummaryCommonProperties {
                    duration: 3,
                    outcome: String::from("test outcome b"),
                    artifacts: [
                        (PathBuf::from("a"), artifact("a_type")),
                        (PathBuf::from("b"), artifact("b_type")),
                        (PathBuf::from("c"), artifact("c_type")),
                    ]
                    .into(),
                    extension: [
                        (String::from("x"), Value::from("x_value")),
                        (String::from("y"), Value::from("y_value")),
                        (String::from("z"), Value::from("z_value")),
                    ]
                    .into(),
                },
                cases: [
                    (String::from("case_a"), case("case_a_outcome")),
                    (String::from("case_b"), case("case_b_outcome")),
                ]
                .into(),
            },
            under_test
        );

        temp_dir.close().expect("to close temporary directory");
    }
}
