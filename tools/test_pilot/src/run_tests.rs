// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::errors::TestRunError;
use crate::invocation_log::CommandInvocationLog;
use crate::std_writer::StdWriter;
use crate::test_config::{OutputProcessor, TestConfig};
use crate::test_output::{
    is_fail_exit_status, outcome_from_exit_status, OutputDirectory, Summary, SummaryArtifact,
};
use std::borrow::Cow;
use std::path::PathBuf;
use std::process::{ExitStatus, Stdio};
use std::time::Instant;
use std::{fs, io};
use tokio::io::AsyncReadExt;
use tokio::process::{self, Command};

const STDIO_BUFFER_SIZE: usize = 2 * 1024;
const ENV_PATH: &str = "PATH";

const ARTIFACT_TYPE_STDOUT: &str = "stdout";
const ARTIFACT_TYPE_STDERR: &str = "stderr";

pub async fn run_test(
    test_config: &TestConfig,
    outdir: &OutputDirectory<'_>,
    path_env_var_value: &str,
) -> Result<ExitStatus, TestRunError> {
    let mut log = CommandInvocationLog::new();

    let exit_status = run_test_inner(test_config, &outdir, path_env_var_value, &mut log).await?;

    let _ = fs::write(outdir.execution_log(), serde_json::to_string_pretty(&log).unwrap());

    Ok(exit_status)
}

async fn run_test_inner(
    test_config: &TestConfig,
    outdir: &OutputDirectory<'_>,
    path_env_var_value: &str,
    log: &mut CommandInvocationLog,
) -> Result<ExitStatus, TestRunError> {
    let mut main_summary = Summary::default();
    let mut cmd =
        create_test_launch_command(test_config, &outdir.test_config(), path_env_var_value);

    let exit_status = {
        let mut stdout_writer = StdWriter::new(outdir.test_stdout(), Box::new(io::stdout()))?;
        let mut stderr_writer = StdWriter::new(outdir.test_stderr(), Box::new(io::stderr()))?;
        let start_time = Instant::now();
        let exit_status = run_command(&mut cmd, &mut stdout_writer, &mut stderr_writer).await?;
        log.record_test(
            &cmd,
            exit_status,
            stdout_writer.path_if_wrote().and_then(|p| outdir.make_relative(&p)),
            stderr_writer.path_if_wrote().and_then(|p| outdir.make_relative(&p)),
        );

        // Populate the main summary based on test run results.
        main_summary.common.duration = start_time.elapsed().as_millis() as i64;
        main_summary.common.outcome = String::from(outcome_from_exit_status(exit_status));
        if let Some(stdout) = stdout_writer.path_if_wrote() {
            main_summary.common.artifacts.insert(
                outdir.make_relative(&stdout).expect("stdout file is in output directory"),
                SummaryArtifact { artifact_type: String::from(ARTIFACT_TYPE_STDOUT) },
            );
        }
        if let Some(stderr) = stderr_writer.path_if_wrote() {
            main_summary.common.artifacts.insert(
                outdir.make_relative(&stderr).expect("stdout file is in output directory"),
                SummaryArtifact { artifact_type: String::from(ARTIFACT_TYPE_STDERR) },
            );
        }

        if !exit_status.success() && is_fail_exit_status(exit_status) {
            eprintln!(
                "host test binary {0} returned bad exit status {1}",
                test_config.host_test_binary.display(),
                exit_status
            );
            eprintln!("see {0} for execution details", outdir.execution_log().display());

            return Ok(exit_status);
        }

        // stdout_writer and stderr_writer are dropped here, closing and maybe deleting their
        // respective output files.
        exit_status
    };

    if main_summary.maybe_merge_file(&outdir.subdir_summary("target"))? {
        main_summary.write(&outdir.main_summary())?;
    }

    for output_processor in &test_config.output_processors {
        if (exit_status.success() && !output_processor.use_on_success)
            || (!exit_status.success() && !output_processor.use_on_failure)
        {
            continue;
        }

        let mut cmd = create_output_processor_command(
            test_config,
            &output_processor,
            &outdir.test_config(),
            path_env_var_value,
        );

        {
            let mut stdout_writer = StdWriter::new(
                outdir.postprocessor_stdout(&output_processor.binary),
                Box::new(io::stdout()),
            )?;
            let mut stderr_writer = StdWriter::new(
                outdir.postprocessor_stderr(&output_processor.binary),
                Box::new(io::stderr()),
            )?;
            let exit_status = run_command(&mut cmd, &mut stdout_writer, &mut stderr_writer).await?;
            log.record_postprocessor(
                &cmd,
                exit_status,
                stdout_writer.path_if_wrote().and_then(|p| outdir.make_relative(&p)),
                stderr_writer.path_if_wrote().and_then(|p| outdir.make_relative(&p)),
            );

            if main_summary
                .maybe_merge_file(&outdir.postprocessor_summary(&output_processor.binary))?
            {
                main_summary.write(&outdir.main_summary())?;
            }

            if !exit_status.success() && is_fail_exit_status(exit_status) {
                eprintln!(
                    "output processor {0:?} returned bad exit status {1}",
                    output_processor.binary.display(),
                    exit_status
                );
                eprintln!("see {0:?} for execution details", outdir.execution_log());

                return Ok(exit_status);
            }

            // stdout_writer and stderr_writer are dropped here, closing and maybe deleting their
            // respective output files.
        }
    }

    Ok(exit_status)
}

/// Creates a `Command` that runs the test as specified in `test_config`.
fn create_test_launch_command(
    test_config: &TestConfig,
    test_config_path: &PathBuf,
    path_env_var_value: &str,
) -> Command {
    create_command(
        &test_config.host_test_binary,
        test_config.resolved_host_test_args(test_config_path),
        path_env_var_value,
    )
}

/// Creates a `Command` that runs an output processor as specified in `output_processor`.
fn create_output_processor_command(
    test_config: &TestConfig,
    output_processor: &OutputProcessor,
    test_config_path: &PathBuf,
    path_env_var_value: &str,
) -> Command {
    create_command(
        &output_processor.binary,
        output_processor.resolved_args(test_config, test_config_path),
        path_env_var_value,
    )
}

fn create_command<'a>(
    binary: &PathBuf,
    args: Box<dyn Iterator<Item = Cow<'a, str>> + 'a>,
    path_env_var_value: &str,
) -> Command {
    let mut cmd = Command::new(binary);

    for arg in args {
        cmd.arg(arg.into_owned());
    }

    cmd.env_clear();
    cmd.env(ENV_PATH, path_env_var_value);

    if let Ok(current_dir) = std::env::current_dir() {
        cmd.current_dir(current_dir);
    }

    cmd
}

/// Makes sure that the child process is killed and waited to remove zombie process on drop.
struct ChildProcess {
    inner: process::Child,
}

impl Drop for ChildProcess {
    fn drop(&mut self) {
        let _ = self.inner.kill();
        let _ = self.inner.wait();
    }
}

impl From<process::Child> for ChildProcess {
    fn from(inner: process::Child) -> Self {
        ChildProcess { inner }
    }
}

async fn run_command<W1: io::Write + Send, W2: io::Write + Send>(
    command: &mut Command,
    mut stdout_writer: W1,
    mut stderr_writer: W2,
) -> Result<ExitStatus, TestRunError> {
    let mut child: ChildProcess = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| TestRunError::Spawn {
            path: command.as_std().get_program().into(),
            source: e,
        })?
        .into();

    let mut stdout = child.inner.stdout.take().unwrap();
    let mut stderr = child.inner.stderr.take().unwrap();

    let stdout_writer_handle = async move {
        let mut buf = [0; STDIO_BUFFER_SIZE];
        loop {
            let n = stdout.read(&mut buf).await.map_err(TestRunError::StdoutRead)?;
            if n > 0 {
                stdout_writer.write_all(&buf[..n]).map_err(TestRunError::StdoutWrite)?;
            } else {
                break;
            }
        }
        Ok::<(), TestRunError>(())
    };

    let stderr_writer_handle = async move {
        let mut buf = [0; STDIO_BUFFER_SIZE];
        loop {
            let n: usize = stderr.read(&mut buf).await.map_err(TestRunError::StderrRead)?;
            if n > 0 {
                stderr_writer.write_all(&buf[..n]).map_err(TestRunError::StderrWrite)?;
            } else {
                break;
            }
        }
        Ok::<(), TestRunError>(())
    };

    // TODO(b/294567408) : Support timeout.
    // The futures might block depending on underlying primitives, so we need to run this with more
    // than 1 thread to stream stdout and stderr in parallel. We will replace it with tokio when
    // available,
    let (stdout_status, stderr_status) = futures::join!(stdout_writer_handle, stderr_writer_handle);

    stdout_status?;
    stderr_status?;
    Ok(child.inner.wait().await.expect("Command wasn't running"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_matches::assert_matches;
    use rand::distributions::Alphanumeric;
    use rand::Rng;
    use serde_json::{from_reader, json, Value};
    use std::collections::HashMap;
    use std::io::Cursor;
    use std::os::unix::process::ExitStatusExt;
    use tempfile::tempdir;

    const CURRENT_DIR: &str = "current_dir";

    #[fuchsia::test]
    async fn test_run_command_exit_code() {
        let mut stdout_buf = Cursor::new(Vec::new());
        let mut stderr_buf = Cursor::new(Vec::new());
        let mut cmd = Command::new("false");

        let status = run_command(&mut cmd, &mut stdout_buf, &mut stderr_buf).await.unwrap();

        let stdout_output = String::from_utf8(stdout_buf.into_inner()).unwrap();
        let stderr_output = String::from_utf8(stderr_buf.into_inner()).unwrap();

        assert_eq!(stdout_output, format!(""));
        assert_eq!(stderr_output, "");
        assert_eq!(status.code(), Some(1));
    }

    #[fuchsia::test]
    async fn test_run_command_stderr() {
        let mut stdout_buf = Cursor::new(Vec::new());
        let mut stderr_buf = Cursor::new(Vec::new());
        let mut cmd = Command::new("ls");
        cmd.arg("non-existent-file");

        let status = run_command(&mut cmd, &mut stdout_buf, &mut stderr_buf).await.unwrap();

        let stdout_output = String::from_utf8(stdout_buf.into_inner()).unwrap();
        let stderr_output = String::from_utf8(stderr_buf.into_inner()).unwrap();

        assert_eq!(stdout_output, format!(""));
        assert!(stderr_output.contains("non-existent-file"), "{}", stderr_output);
        assert!(!status.success(), "status: {}", status);
    }

    #[fuchsia::test]
    async fn test_run_command_large_stdout() {
        let mut stdout_buf = Cursor::new(Vec::new());
        let mut stderr_buf = Cursor::new(Vec::new());
        let mut cmd = Command::new("echo");
        let s: String = rand::thread_rng()
            .sample_iter(&Alphanumeric)
            .take(STDIO_BUFFER_SIZE * 10 - 10)
            .map(char::from)
            .collect();
        cmd.arg(s.clone());

        let status = run_command(&mut cmd, &mut stdout_buf, &mut stderr_buf).await.unwrap();

        let stdout_output = String::from_utf8(stdout_buf.into_inner()).unwrap();
        let stderr_output = String::from_utf8(stderr_buf.into_inner()).unwrap();

        let len = stdout_output.len();
        // echo writes a new line at the end
        assert_eq!(stdout_output.as_bytes()[len - 1], 10);
        assert_eq!(stdout_output[..len - 1], s);
        assert_eq!(stderr_output, "");
        assert!(status.success(), "status: {}", status);
    }

    #[fuchsia::test]
    async fn test_run_command_large_stderr() {
        let mut stdout_buf = Cursor::new(Vec::new());
        let mut stderr_buf = Cursor::new(Vec::new());
        let s: String = rand::thread_rng()
            .sample_iter(&Alphanumeric)
            .take(STDIO_BUFFER_SIZE * 10 - 10)
            .map(char::from)
            .collect();
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(format!("echo '{}' >&2", s));

        let status = run_command(&mut cmd, &mut stdout_buf, &mut stderr_buf).await.unwrap();

        let stdout_output = String::from_utf8(stdout_buf.into_inner()).unwrap();
        let stderr_output = String::from_utf8(stderr_buf.into_inner()).unwrap();

        let len = stderr_output.len();
        // echo writes a new line at the end
        assert_eq!(stderr_output.as_bytes()[len - 1], 10);
        assert_eq!(stderr_output[..len - 1], s);
        assert_eq!(stdout_output, "");
        assert!(status.success(), "status: {}", status);
    }

    #[fuchsia::test]
    async fn test_run_command_invalid_command() {
        let mut stdout_buf = Cursor::new(Vec::new());
        let mut stderr_buf = Cursor::new(Vec::new());
        let mut cmd = Command::new("invalid-cmd");

        let err = run_command(&mut cmd, &mut stdout_buf, &mut stderr_buf)
            .await
            .expect_err("should have failed");

        assert_matches!(err, TestRunError::Spawn { path: _, source: _ });
    }

    #[fuchsia::test]
    async fn test_run_test() {
        let temp_dir = tempdir().expect("to create temporary directory");

        let config = TestConfig {
            host_test_binary: PathBuf::from("echo"),
            host_test_args: vec![String::from("should_end_up_in_test_stdout")],
            output_directory: temp_dir.path().to_path_buf(),
            output_processors: vec![OutputProcessor {
                binary: PathBuf::from("echo"),
                args: vec![String::from("should_end_up_in_proc_stdout")],
                use_on_success: true,
                use_on_failure: false,
            }],
            unknown: HashMap::new(),
        };

        let output_directory = OutputDirectory::new(&config.output_directory);
        let exit_status = run_test(&config, &output_directory, "/usr/bin")
            .await
            .expect("run_test should succeed");

        assert_eq!(ExitStatus::from_raw(0), exit_status);
        assert_eq!(
            b"should_end_up_in_test_stdout\n".to_vec(),
            fs::read(output_directory.test_stdout()).expect("read from test stdout succeeds")
        );
        assert_eq!(
            b"should_end_up_in_proc_stdout\n".to_vec(),
            fs::read(output_directory.postprocessor_stdout(&PathBuf::from("echo")))
                .expect("read from post-processor stdout succeeds")
        );

        let mut log_reader = std::io::BufReader::new(
            fs::File::open(&output_directory.execution_log()).expect("can open invocation log"),
        );
        let mut log: Value = from_reader(&mut log_reader).expect("can parse invocation log");
        normalize_current_dir(&mut log);

        assert_eq!(
            json!({
                "postprocessors": {
                    "echo": {
                        "args": ["should_end_up_in_proc_stdout"],
                        "current_dir": "current_dir",
                        "envs": {"PATH": "/usr/bin"},
                        "exit_status": 0,
                        "program_path": "echo",
                        "stdout_artifact": "echo_stdout.txt"
                    }
                },
                "test": {
                    "args": ["should_end_up_in_test_stdout"],
                    "current_dir": "current_dir",
                    "envs": {"PATH": "/usr/bin"},
                    "exit_status": 0,
                    "program_path": "echo",
                    "stdout_artifact": "host_binary_stdout.txt"
                }
            }),
            log
        );

        temp_dir.close().expect("temp directory successfully closed");
    }

    /// Replaces all "current_dir" property values with "current_dir" to normalize a log that
    /// would otherwise contain absolute paths.
    fn normalize_current_dir(value: &mut Value) {
        if !value.is_object() {
            return;
        }

        let map = value.as_object_mut().unwrap();

        if map.contains_key(CURRENT_DIR) {
            map.insert(String::from(CURRENT_DIR), Value::from(CURRENT_DIR));
            return;
        }

        for (_, value) in map {
            normalize_current_dir(value);
        }
    }
}
