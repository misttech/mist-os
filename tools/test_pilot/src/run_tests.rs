// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::errors::TestRunError;
use crate::opts::{
    TestPilotArgs, ENV_CUSTOM_TEST_ARGS, ENV_OUT_DIR, ENV_PATH, ENV_RESOURCE_PATH,
    ENV_SDK_TOOL_PATH, ENV_TARGETS, ENV_TEST_FILTER,
};
use crate::test_config::{TestConfigV1, TestConfiguration};
use std::io::{self, Write};
use std::process::{ExitStatus, Stdio};
use tokio::io::AsyncReadExt;
use tokio::process::{self, Command};

const ENV_TAGS: &str = "FUCHSIA_TAGS";
const ENV_EXECUTION_JSON: &str = "FUCHSIA_EXECUTION_JSON";
const ENV_DEBUG: &str = "DEBUG";
const BUFFER_SIZE: usize = 2048;

fn create_test_launch_command_v1(args: &TestPilotArgs, config: &TestConfigV1) -> Command {
    let mut cmd = Command::new(&args.test_bin_path);
    cmd.env_clear();
    cmd.env(ENV_PATH, args.path.clone());
    if let Some(path) = &args.sdk_tools_path {
        cmd.env(ENV_SDK_TOOL_PATH, path);
    }
    if !args.targets.is_empty() {
        cmd.env(ENV_TARGETS, args.targets.join(", "));
    }
    if let Some(path) = &args.resource_path {
        cmd.env(ENV_RESOURCE_PATH, path);
    }
    match &config.execution {
        serde_json::Value::Null => {}
        execution => {
            let exec_json = serde_json::to_string(&execution).unwrap();
            cmd.env(ENV_EXECUTION_JSON, exec_json);
        }
    };
    if let Some(test_filter) = &args.test_filter {
        cmd.env(ENV_TEST_FILTER, test_filter);
    }
    if let Some(custom_test_args) = &args.custom_test_args {
        cmd.env(ENV_CUSTOM_TEST_ARGS, custom_test_args);
    }
    if let Some(out_dir) = &args.out_dir {
        cmd.env(ENV_OUT_DIR, out_dir);
    }
    if args.debug {
        cmd.env(ENV_DEBUG, "1");
    }

    if config.tags.len() > 0 {
        let tags_str = config
            .tags
            .iter()
            .map(|tag| format!("{}={}", tag.key, tag.value))
            .collect::<Vec<_>>()
            .join(";");

        cmd.env(ENV_TAGS, tags_str);
    }

    if !args.strict_mode {
        for (key, value) in &args.extra_env_vars {
            cmd.env(key, value);
        }
    } else {
        for (key, value) in &args.extra_env_vars {
            if config.requested_vars.extra_vars.contains(key) {
                cmd.env(key, value);
            }
        }
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

async fn run_test_and_stream_output_v1<W1: Write + Send, W2: Write + Send>(
    command: &mut Command,
    mut stdout_writer: W1,
    mut stderr_writer: W2,
) -> Result<ExitStatus, TestRunError> {
    let mut child: ChildProcess = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| TestRunError::Spawn(command.as_std().get_program().into(), e))?
        .into();

    let mut stdout = child.inner.stdout.take().unwrap();
    let mut stderr = child.inner.stderr.take().unwrap();

    let stdout_writer_handle = async move {
        let mut buf = vec![0; BUFFER_SIZE];
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
        let mut buf = vec![0; BUFFER_SIZE];
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

pub async fn run_test(
    args: &TestPilotArgs,
    config: &TestConfiguration,
) -> Result<ExitStatus, TestRunError> {
    match config {
        TestConfiguration::V1 { config } => {
            let mut cmd = create_test_launch_command_v1(args, config);
            run_test_and_stream_output_v1(&mut cmd, &mut io::stdout(), &mut io::stderr()).await
        }
    }
}

#[cfg(test)]
mod tests {
    use self::test_config::RequestedVars;

    use super::*;
    use crate::*;
    use assert_matches::assert_matches;
    use rand::distributions::Alphanumeric;
    use rand::Rng;
    use std::collections::HashMap;
    use std::ffi::OsStr;
    use std::io::Cursor;

    fn default_args() -> TestPilotArgs {
        TestPilotArgs {
            test_bin_path: "/path/to/test_bin".into(),
            sdk_tools_path: None,
            path: std::env::var("PATH").unwrap(),
            targets: Vec::new(),
            resource_path: None,
            out_dir: None,
            test_filter: None,
            custom_test_args: None,
            test_config: "/path/to/test_config".into(),
            timeout_seconds: None,
            strict_mode: true,
            extra_env_vars: vec![],
            debug: false,
        }
    }

    fn default_config_v1() -> TestConfigV1 {
        TestConfigV1 {
            execution: serde_json::Value::Null,
            tags: Vec::new(),
            requested_vars: RequestedVars::default(),
        }
    }

    #[test]
    fn test_default_command() {
        let args = default_args();
        let config = default_config_v1();

        let cmd = create_test_launch_command_v1(&args, &config);

        assert_eq!(cmd.as_std().get_program(), "/path/to/test_bin");
        assert_eq!(cmd.as_std().get_args().len(), 0);
        let env = cmd.as_std().get_envs().collect::<HashMap<_, _>>();
        assert_eq!(env.get(OsStr::new(ENV_PATH)).unwrap().unwrap(), args.path.as_str());
        assert_eq!(env.get(OsStr::new(ENV_SDK_TOOL_PATH)), None);
        assert_eq!(env.get(OsStr::new(ENV_TARGETS)), None);
        assert_eq!(env.get(OsStr::new(ENV_RESOURCE_PATH)), None);
        assert_eq!(env.get(OsStr::new(ENV_EXECUTION_JSON)), None);
        assert_eq!(env.get(OsStr::new(ENV_TEST_FILTER)), None);
        assert_eq!(env.get(OsStr::new(ENV_CUSTOM_TEST_ARGS)), None);
        assert_eq!(env.get(OsStr::new(ENV_TAGS)), None);

        // make sure there are no inherited env variables
        assert_eq!(env.len(), 1);
    }

    #[test]
    fn test_strict_mode() {
        let mut args = default_args();
        let mut config = default_config_v1();

        args.extra_env_vars = vec![("key1".into(), "val1".into()), ("key2".into(), "val2".into())];

        let cmd = create_test_launch_command_v1(&args, &config);

        assert_eq!(cmd.as_std().get_program(), "/path/to/test_bin");
        assert_eq!(cmd.as_std().get_args().len(), 0);
        let env = cmd.as_std().get_envs().collect::<HashMap<_, _>>();
        assert_eq!(env.get(OsStr::new("key1")), None);
        assert_eq!(env.get(OsStr::new("key2")), None);

        // make sure there are no inherited env variables
        assert_eq!(env.len(), 1);

        args.strict_mode = false;
        let cmd = create_test_launch_command_v1(&args, &config);
        assert_eq!(cmd.as_std().get_program(), "/path/to/test_bin");
        assert_eq!(cmd.as_std().get_args().len(), 0);
        let env = cmd.as_std().get_envs().collect::<HashMap<_, _>>();

        // make sure we inherit extra env variables.
        assert_eq!(env.get(OsStr::new("key1")).unwrap().unwrap(), "val1");
        assert_eq!(env.get(OsStr::new("key2")).unwrap().unwrap(), "val2");

        args.strict_mode = true;
        config.requested_vars.extra_vars = vec!["key3".into(), "key2".into()];
        args.extra_env_vars.push(("key3".into(), "val3".into()));
        let cmd = create_test_launch_command_v1(&args, &config);
        assert_eq!(cmd.as_std().get_program(), "/path/to/test_bin");
        assert_eq!(cmd.as_std().get_args().len(), 0);
        let env = cmd.as_std().get_envs().collect::<HashMap<_, _>>();

        // make sure we inherit allowed extra env variables.
        assert_eq!(env.get(OsStr::new("key3")).unwrap().unwrap(), "val3");
        assert_eq!(env.get(OsStr::new("key2")).unwrap().unwrap(), "val2");
    }

    #[test]
    fn test_command_with_non_default_test_config() {
        let mut args = default_args();
        args.sdk_tools_path = Some("/path/to/sdk_tools".into());
        args.targets = vec!["target1".to_string(), "target2".to_string()];

        let config = TestConfigV1 {
            execution: serde_json::json!({ "key": "value" }),
            tags: vec![
                test_config::TestTag {
                    key: "tag_key1".to_string(),
                    value: "tag_value1".to_string(),
                },
                test_config::TestTag {
                    key: "tag_key2".to_string(),
                    value: "tag_value2".to_string(),
                },
            ],
            requested_vars: RequestedVars::default(),
        };

        let cmd = create_test_launch_command_v1(&args, &config);
        let env = cmd.as_std().get_envs().collect::<HashMap<_, _>>();
        assert_eq!(cmd.as_std().get_program(), "/path/to/test_bin");
        assert_eq!(cmd.as_std().get_args().len(), 0);
        assert_eq!(env.get(OsStr::new(ENV_PATH)).unwrap().unwrap(), args.path.as_str());
        assert_eq!(env.get(OsStr::new(ENV_SDK_TOOL_PATH)).unwrap().unwrap(), "/path/to/sdk_tools");
        assert_eq!(env.get(OsStr::new(ENV_TARGETS)).unwrap().unwrap(), "target1, target2");
        assert_eq!(env.get(OsStr::new(ENV_RESOURCE_PATH)), None);
        assert_eq!(
            env.get(OsStr::new(ENV_EXECUTION_JSON)).unwrap().unwrap(),
            "{\"key\":\"value\"}"
        );
        assert_eq!(env.get(OsStr::new(ENV_TEST_FILTER)), None);
        assert_eq!(env.get(OsStr::new(ENV_CUSTOM_TEST_ARGS)), None);
        assert_eq!(
            env.get(OsStr::new(ENV_TAGS)).unwrap().unwrap(),
            "tag_key1=tag_value1;tag_key2=tag_value2"
        );
    }

    #[test]
    fn test_command_with_non_default_args() {
        let mut args = default_args();
        let config = default_config_v1();
        args.resource_path = Some("/path/to/resource_path".into());
        args.test_filter = Some("test*filter".into());
        args.custom_test_args = Some("--arg1 --arg2".into());
        args.strict_mode = false;
        args.extra_env_vars = vec![("key1".into(), "val1".into()), ("key2".into(), "val2".into())];

        let cmd = create_test_launch_command_v1(&args, &config);
        let env = cmd.as_std().get_envs().collect::<HashMap<_, _>>();
        assert_eq!(cmd.as_std().get_program(), "/path/to/test_bin");
        assert_eq!(cmd.as_std().get_args().len(), 0);
        assert_eq!(env.get(OsStr::new(ENV_PATH)).unwrap().unwrap(), args.path.as_str());
        assert_eq!(env.get(OsStr::new(ENV_SDK_TOOL_PATH)), None);
        assert_eq!(env.get(OsStr::new(ENV_TARGETS)), None);
        assert_eq!(
            env.get(OsStr::new(ENV_RESOURCE_PATH)).unwrap().unwrap(),
            "/path/to/resource_path"
        );
        assert_eq!(env.get(OsStr::new(ENV_EXECUTION_JSON)), None);
        assert_eq!(env.get(OsStr::new(ENV_CUSTOM_TEST_ARGS)).unwrap().unwrap(), "--arg1 --arg2");
        assert_eq!(env.get(OsStr::new(ENV_TEST_FILTER)).unwrap().unwrap(), "test*filter");
        assert_eq!(env.get(OsStr::new(ENV_TAGS)), None);
        assert_eq!(env.get(OsStr::new("key1")).unwrap().unwrap(), "val1");
        assert_eq!(env.get(OsStr::new("key2")).unwrap().unwrap(), "val2");
    }

    #[fuchsia::test]
    async fn run_print_env() {
        let mut stdout_buf = Cursor::new(Vec::new());
        let mut stderr_buf = Cursor::new(Vec::new());
        let mut args = default_args();
        let config = default_config_v1();
        args.test_bin_path = "printenv".into();

        let mut cmd = create_test_launch_command_v1(&args, &config);
        let status = run_test_and_stream_output_v1(&mut cmd, &mut stdout_buf, &mut stderr_buf)
            .await
            .unwrap();

        let stdout_output = String::from_utf8(stdout_buf.into_inner()).unwrap();
        let stderr_output = String::from_utf8(stderr_buf.into_inner()).unwrap();

        assert_eq!(stdout_output, format!("{}={}\n", ENV_PATH, args.path.as_str()));
        assert_eq!(stderr_output, "");
        assert!(status.success(), "status: {}", status);
    }

    #[fuchsia::test]
    async fn test_exit_code() {
        let mut stdout_buf = Cursor::new(Vec::new());
        let mut stderr_buf = Cursor::new(Vec::new());
        let mut cmd = Command::new("false");

        let status = run_test_and_stream_output_v1(&mut cmd, &mut stdout_buf, &mut stderr_buf)
            .await
            .unwrap();

        let stdout_output = String::from_utf8(stdout_buf.into_inner()).unwrap();
        let stderr_output = String::from_utf8(stderr_buf.into_inner()).unwrap();

        assert_eq!(stdout_output, format!(""));
        assert_eq!(stderr_output, "");
        assert_eq!(status.code(), Some(1));
    }

    #[fuchsia::test]
    async fn run_and_test_stderr() {
        let mut stdout_buf = Cursor::new(Vec::new());
        let mut stderr_buf = Cursor::new(Vec::new());
        let mut cmd = Command::new("ls");
        cmd.arg("non-existent-file");

        let status = run_test_and_stream_output_v1(&mut cmd, &mut stdout_buf, &mut stderr_buf)
            .await
            .unwrap();

        let stdout_output = String::from_utf8(stdout_buf.into_inner()).unwrap();
        let stderr_output = String::from_utf8(stderr_buf.into_inner()).unwrap();

        assert_eq!(stdout_output, format!(""));
        assert!(stderr_output.contains("non-existent-file"), "{}", stderr_output);
        assert!(!status.success(), "status: {}", status);
    }

    #[fuchsia::test]
    async fn run_and_test_large_stdout() {
        let mut stdout_buf = Cursor::new(Vec::new());
        let mut stderr_buf = Cursor::new(Vec::new());
        let mut cmd = Command::new("echo");
        let s: String = rand::thread_rng()
            .sample_iter(&Alphanumeric)
            .take(BUFFER_SIZE * 10 - 10)
            .map(char::from)
            .collect();
        cmd.arg(s.clone());

        let status = run_test_and_stream_output_v1(&mut cmd, &mut stdout_buf, &mut stderr_buf)
            .await
            .unwrap();

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
    async fn run_and_test_large_stderr() {
        let mut stdout_buf = Cursor::new(Vec::new());
        let mut stderr_buf = Cursor::new(Vec::new());
        let s: String = rand::thread_rng()
            .sample_iter(&Alphanumeric)
            .take(BUFFER_SIZE * 10 - 10)
            .map(char::from)
            .collect();
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(format!("echo '{}' >&2", s));

        let status = run_test_and_stream_output_v1(&mut cmd, &mut stdout_buf, &mut stderr_buf)
            .await
            .unwrap();

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
    async fn run_invalid_command() {
        let mut stdout_buf = Cursor::new(Vec::new());
        let mut stderr_buf = Cursor::new(Vec::new());
        let mut cmd = Command::new("invalid-cmd");

        let err = run_test_and_stream_output_v1(&mut cmd, &mut stdout_buf, &mut stderr_buf)
            .await
            .expect_err("should have failed");

        assert_matches!(err, TestRunError::Spawn(_, _));
    }
}
