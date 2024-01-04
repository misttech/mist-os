// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::output::{
    directory::{DirectoryReporter, SchemaVersion},
    mux::{MultiplexedDirectoryWriter, MultiplexedWriter},
    shell::ShellReporter,
    ArtifactType, DirectoryArtifactType, DynArtifact, DynDirectoryArtifact, EntityId, EntityInfo,
    ReportedOutcome, Reporter, SuiteId, Timestamp,
};
use fuchsia_sync::Mutex;
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufWriter, Error};
use std::path::PathBuf;

/// A reporter implementation that saves output to the structured directory output format, and also
/// saves human-readable reports to the directory. The reports are generated per-suite, and are
/// generated using |ShellReporter|.
// In the future, we may want to make the type of stdout output configurable.
pub struct DirectoryWithStdoutReporter {
    directory_reporter: DirectoryReporter,
    /// Set of shell reporters, one per suite. Each |ShellReporter| is routed events for a single
    /// suite, and produces a report as if there is a run containing a single suite.
    shell_reporters: Mutex<HashMap<SuiteId, ShellReporter<BufWriter<File>>>>,
}

impl DirectoryWithStdoutReporter {
    pub fn new(root: PathBuf, version: SchemaVersion) -> Result<Self, Error> {
        Ok(Self {
            directory_reporter: DirectoryReporter::new(root, version)?,
            shell_reporters: Mutex::new(HashMap::new()),
        })
    }

    fn get_locked_shell_reporter(
        &self,
        suite: &SuiteId,
    ) -> impl '_ + std::ops::Deref<Target = ShellReporter<BufWriter<File>>> {
        fuchsia_sync::MutexGuard::map(self.shell_reporters.lock(), |reporters| {
            reporters.get_mut(suite).unwrap()
        })
    }
}

impl Reporter for DirectoryWithStdoutReporter {
    fn new_entity(&self, entity: &EntityId, name: &str) -> Result<(), Error> {
        self.directory_reporter.new_entity(entity, name)?;

        match entity {
            EntityId::Suite(suite_id) => {
                let human_readable_artifact = self.directory_reporter.add_report(entity)?;
                let shell_reporter = ShellReporter::new(human_readable_artifact);
                shell_reporter.new_entity(entity, name)?;
                self.shell_reporters.lock().insert(*suite_id, shell_reporter);
                Ok(())
            }
            EntityId::Case { suite, .. } => {
                self.shell_reporters.lock().get(suite).unwrap().new_entity(entity, name)
            }
            EntityId::TestRun => Ok(()),
        }
    }

    fn set_entity_info(&self, entity: &EntityId, info: &EntityInfo) {
        self.directory_reporter.set_entity_info(entity, info);

        let suite_id = match entity {
            EntityId::Suite(suite) => Some(suite),
            EntityId::Case { suite, .. } => Some(suite),
            _ => None,
        };
        if let Some(suite) = suite_id {
            self.get_locked_shell_reporter(suite).set_entity_info(entity, info);
        }
    }

    fn entity_started(&self, entity: &EntityId, timestamp: Timestamp) -> Result<(), Error> {
        self.directory_reporter.entity_started(entity, timestamp)?;

        match entity {
            EntityId::Suite(suite) => {
                let reporter = self.get_locked_shell_reporter(suite);
                // Since we create one reporter per suite, we should start the run at the
                // same time as the suite.
                reporter.entity_started(&EntityId::TestRun, timestamp)?;
                reporter.entity_started(entity, timestamp)
            }
            EntityId::Case { suite, .. } => {
                self.get_locked_shell_reporter(suite).entity_started(entity, timestamp)
            }
            EntityId::TestRun => Ok(()),
        }
    }

    fn entity_stopped(
        &self,
        entity: &EntityId,
        outcome: &ReportedOutcome,
        timestamp: Timestamp,
    ) -> Result<(), Error> {
        self.directory_reporter.entity_stopped(entity, outcome, timestamp)?;

        match entity {
            EntityId::Suite(suite) => {
                let reporter = self.get_locked_shell_reporter(suite);
                // Since we create one reporter per suite, we should stop the run at the
                // same time as the suite.
                reporter.entity_stopped(entity, outcome, timestamp)?;
                reporter.entity_stopped(&EntityId::TestRun, outcome, timestamp)
            }
            EntityId::Case { suite, .. } => {
                self.get_locked_shell_reporter(suite).entity_stopped(entity, outcome, timestamp)
            }
            EntityId::TestRun => Ok(()),
        }
    }

    fn entity_finished(&self, entity: &EntityId) -> Result<(), Error> {
        self.directory_reporter.entity_finished(entity)?;

        match entity {
            EntityId::Suite(suite) => {
                let reporter = self.get_locked_shell_reporter(suite);
                // Since we create one reporter per suite, we should finish the run at the
                // same time as the suite.
                reporter.entity_finished(entity)?;
                reporter.entity_finished(&EntityId::TestRun)
            }
            EntityId::Case { suite, .. } => {
                self.get_locked_shell_reporter(suite).entity_finished(entity)
            }
            EntityId::TestRun => Ok(()),
        }
    }

    fn new_artifact(
        &self,
        entity: &EntityId,
        artifact_type: &ArtifactType,
    ) -> Result<Box<DynArtifact>, Error> {
        let shell_reporter_artifact = match entity {
            EntityId::Suite(suite) | EntityId::Case { suite, .. } => {
                Some(self.get_locked_shell_reporter(suite).new_artifact(entity, artifact_type)?)
            }
            EntityId::TestRun => None,
        };
        let directory_reporter_artifact =
            self.directory_reporter.new_artifact(entity, artifact_type)?;
        match shell_reporter_artifact {
            Some(artifact) => {
                Ok(Box::new(MultiplexedWriter::new(artifact, directory_reporter_artifact)))
            }
            None => Ok(directory_reporter_artifact),
        }
    }

    fn new_directory_artifact(
        &self,
        entity: &EntityId,
        artifact_type: &DirectoryArtifactType,
        component_moniker: Option<String>,
    ) -> Result<Box<DynDirectoryArtifact>, Error> {
        let component_moniker_clone = component_moniker.clone();
        let shell_reporter_artifact = match entity {
            EntityId::Suite(suite) | EntityId::Case { suite, .. } => {
                Some(self.get_locked_shell_reporter(suite).new_directory_artifact(
                    entity,
                    artifact_type,
                    component_moniker_clone,
                )?)
            }
            EntityId::TestRun => None,
        };
        let directory_reporter_artifact = self.directory_reporter.new_directory_artifact(
            entity,
            artifact_type,
            component_moniker,
        )?;
        match shell_reporter_artifact {
            Some(artifact) => {
                Ok(Box::new(MultiplexedDirectoryWriter::new(artifact, directory_reporter_artifact)))
            }
            None => Ok(directory_reporter_artifact),
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::output::{CaseId, RunReporter};
    use tempfile::tempdir;
    use test_output_directory as directory;
    use test_output_directory::testing::{
        assert_run_result, ExpectedSuite, ExpectedTestCase, ExpectedTestRun,
    };

    // these tests are intended to verify that events are routed correctly. The actual contents
    // are verified more thoroughly in tests for DirectoryReporter and ShellReporter.
    // TODO(satsukiu): consider adding a reporter that outputs something more structured to stdout
    // so that we can write more thorough tests.

    #[fuchsia::test]
    async fn directory_with_stdout() {
        let dir = tempdir().expect("create temp directory");
        let run_reporter = RunReporter::new(
            DirectoryWithStdoutReporter::new(dir.path().to_path_buf(), SchemaVersion::V1).unwrap(),
        );

        for suite_no in 0..3 {
            let suite_reporter = run_reporter
                .new_suite(&format!("test-suite-{}", suite_no), &SuiteId(suite_no))
                .expect("create suite");
            suite_reporter.started(Timestamp::Unknown).expect("start suite");
            let case_reporter =
                suite_reporter.new_case("test-case", &CaseId(0)).expect("create test case");
            case_reporter.started(Timestamp::Unknown).expect("start case");
            let mut case_stdout =
                case_reporter.new_artifact(&ArtifactType::Stdout).expect("create stdout");
            writeln!(case_stdout, "Stdout for test case").expect("write to stdout");
            case_stdout.flush().expect("flush stdout");
            case_reporter.stopped(&ReportedOutcome::Passed, Timestamp::Unknown).expect("stop case");
            case_reporter.finished().expect("finish case");
            suite_reporter
                .stopped(&ReportedOutcome::Passed, Timestamp::Unknown)
                .expect("stop suite");
            suite_reporter.finished().expect("finish suite");
        }
        run_reporter.stopped(&ReportedOutcome::Passed, Timestamp::Unknown).expect("stop run");
        run_reporter.finished().expect("finish run");

        let mut expected_test = ExpectedTestRun::new(directory::Outcome::Passed);
        for suite_no in 0..3 {
            let expected_report = format!(
                "Running test 'test-suite-{:?}'\n\
            [RUNNING]\ttest-case\n\
            [stdout - test-case]\n\
            Stdout for test case\n\
            [PASSED]\ttest-case\n\
            \n\
            1 out of 1 tests passed...\n\
            test-suite-{:?} completed with result: PASSED\n",
                suite_no, suite_no
            );
            let suite = ExpectedSuite::new(
                format!("test-suite-{:?}", suite_no),
                directory::Outcome::Passed,
            )
            .with_artifact(directory::ArtifactType::Report, "report.txt".into(), &expected_report)
            .with_case(
                ExpectedTestCase::new("test-case", directory::Outcome::Passed).with_artifact(
                    directory::ArtifactType::Stdout,
                    "stdout.txt".into(),
                    "Stdout for test case\n",
                ),
            );
            expected_test = expected_test.with_suite(suite);
        }

        assert_run_result(dir.path(), &expected_test);
    }
}
