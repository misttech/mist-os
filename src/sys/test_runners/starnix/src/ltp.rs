// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::helpers::*;
use anyhow::{anyhow, Context as _, Error};
use fidl::endpoints::{create_proxy, Proxy};
use fidl_fuchsia_component_runner as frunner;
use fidl_fuchsia_data as fdata;
use fidl_fuchsia_io as fio;
use fidl_fuchsia_test as ftest;
use fuchsia_zircon as zx;
use futures::{AsyncReadExt, StreamExt};
use socket_parsing::NewlineChunker;
use std::collections::HashMap;

/// `Results` represent the results of an LTP test run as reported by the test
/// binary's `stderr`.
#[derive(Default)]
struct Results {
    passed: Option<usize>,
    failed: Option<usize>,
    broken: Option<usize>,
    skipped: Option<usize>,
    warnings: Option<usize>,
}

impl Results {
    /// Returns whether or not these results should be reported as a "pass" to the test
    /// framework.
    ///
    /// Returns `None` if the `Results` do not contain enough information to determine whether or
    /// not the test passed.
    fn passed(&self, expected_result: &str) -> Option<bool> {
        match (expected_result, self) {
            (
                "PASSED",
                Results {
                    passed: Some(_),
                    failed: Some(0),
                    broken: Some(0),
                    warnings: Some(0),
                    skipped: Some(0),
                },
            )
            | (
                "IGNORED",
                Results {
                    passed: Some(_),
                    failed: Some(0),
                    broken: Some(0),
                    skipped: Some(_),
                    warnings: Some(0),
                },
            ) => {
                // Test case results were parsed successfully, and the results matched
                // expectations.
                Some(true)
            }
            (
                _,
                Results { passed: None, failed: None, broken: None, skipped: None, warnings: None },
            ) => {
                // Results were not parsed successfully, so can't determine whether or not the test
                // passed.
                None
            }
            // Results were parsed successfully, but don't match expectations.
            _ => Some(false),
        }
    }

    /// Parses results from `input` and adds the to the provided `Results` instance.
    fn count_results(&mut self, input: &[u8]) {
        match input {
            input if input.starts_with(b"passed") => {
                self.passed = Results::parse_number(&input);
            }
            input if input.starts_with(b"failed") => {
                self.failed = Results::parse_number(&input);
            }
            input if input.starts_with(b"broken") => {
                self.broken = Results::parse_number(&input);
            }
            input if input.starts_with(b"skipped") => {
                self.skipped = Results::parse_number(&input);
            }
            input if input.starts_with(b"warnings") => {
                self.warnings = Results::parse_number(&input);
            }
            _ => {}
        };
    }

    fn parse_number(input: &[u8]) -> Option<usize> {
        let input = String::from_utf8_lossy(input);
        input
            .split_whitespace()
            .flat_map(|s| s.parse::<usize>())
            .collect::<Vec<usize>>()
            .first()
            .copied()
    }
}

pub async fn get_cases_list_for_ltp(
    mut start_info: frunner::ComponentStartInfo,
    component_runner: &frunner::ComponentRunnerProxy,
) -> Result<Vec<ftest::Case>, Error> {
    let program = start_info.program.as_ref().unwrap();
    let base_path = get_str_value_from_dict(program, "tests_dir")?;

    let tests_list = match get_opt_str_value_from_dict(program, "tests_list")? {
        Some(list_file) => read_file_from_component_ns(&mut start_info, &list_file)
            .await
            .with_context(|| format!("Failed to read {}", list_file))?
            .split('\n')
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .collect::<Vec<String>>(),
        None => {
            // If tests list file is not specified then run `/system/bin/ls <base_path>` to get list
            // of all files in the target directory.
            let (_ls_component_controller, std_handles) = start_command(
                &mut start_info,
                component_runner,
                "/",
                "/system/bin/ls",
                &[base_path.as_str()],
            )?;

            read_lines_from_socket(std_handles.out.unwrap())
                .await
                .with_context(|| format!("Failed to enumerate tests in {}", base_path))?
        }
    };

    Ok(tests_list
        .iter()
        .map(|name| ftest::Case { name: Some(name.clone()), ..Default::default() })
        .collect())
}

pub async fn run_ltp_cases(
    tests: Vec<ftest::Invocation>,
    mut start_info: frunner::ComponentStartInfo,
    run_listener_proxy: &ftest::RunListenerProxy,
    component_runner: &frunner::ComponentRunnerProxy,
) -> Result<(), Error> {
    let program = start_info.program.as_ref().unwrap();
    let base_path = get_str_value_from_dict(program, "tests_dir")?;

    // TODO(b/287506763): Parse the expected test results, if available. This is a temporary
    // measure, eventually the LTP test package will always contain this file.
    let expected_test_results: Option<HashMap<String, String>> = {
        match read_file_from_component_ns(&mut start_info, "data/test_results.json").await {
            Ok(results_string) => serde_json::from_str(&results_string).ok(),
            _ => None,
        }
    };

    for test in tests {
        let test_name = test.name.as_ref().expect("No test name");
        let test_path = format!("{}/{}", base_path, test_name);
        let (component_controller, component_std_handles) =
            start_command(&mut start_info, component_runner, &base_path, &test_path, &[])?;

        // Create the proxy socket endpoints to hand off to the run listener.
        let (sender_stderr, listener_stderr) = zx::Socket::create_stream();
        let parsed_std_handles = ftest::StdHandles {
            out: component_std_handles.out,
            err: Some(listener_stderr),
            ..Default::default()
        };
        let (case_listener_proxy, case_listener) = create_proxy::<ftest::CaseListenerMarker>()?;
        run_listener_proxy.on_test_case_started(&test, parsed_std_handles, case_listener)?;

        /// Max size for message when draining input stream socket. This number is
        /// slightly smaller than size allowed by Archivist (LogSink service implementation).
        const MAX_MESSAGE_SIZE: usize = 30720;

        // Read stderr from the test component and:
        //   - forward the logs to the run listener
        //   - parse the logs for test results
        let mut stderr_lines =
            NewlineChunker::new_with_newlines(component_std_handles.err.unwrap(), MAX_MESSAGE_SIZE)
                .unwrap();

        let mut parsed_results = Results::default();
        while let Some(Ok(line)) = stderr_lines.next().await {
            let _ = sender_stderr.write(&line);
            parsed_results.count_results(&line);
        }

        let exit_code_result = read_result(component_controller.take_event_stream()).await;

        let result = if let Some(expected_test_results) = &expected_test_results {
            // If a result file was present, we check the parsed results to determine whether or not
            // the test passed. If the result file is not present, always default to just the exit
            // code.
            //
            // Eventually the exit code result will only be used if the test result parsing failed,
            // since the test expectations file will always be present.
            if let Some(success) = parsed_results.passed(&expected_test_results[test_name]) {
                if success {
                    ftest::Result_ { status: Some(ftest::Status::Passed), ..Default::default() }
                } else {
                    ftest::Result_ { status: Some(ftest::Status::Failed), ..Default::default() }
                }
            } else {
                exit_code_result
            }
        } else {
            exit_code_result
        };

        case_listener_proxy.finished(&result)?;
    }

    Ok(())
}

async fn read_file_from_dir(dir: &fio::DirectoryProxy, path: &str) -> Result<String, Error> {
    let file_proxy = fuchsia_fs::directory::open_file_no_describe(
        &dir,
        path,
        fuchsia_fs::OpenFlags::RIGHT_READABLE,
    )?;
    fuchsia_fs::file::read_to_string(&file_proxy).await.map_err(Into::into)
}

async fn read_file_from_component_ns(
    start_info: &mut frunner::ComponentStartInfo,
    path: &str,
) -> Result<String, Error> {
    for entry in start_info.ns.as_mut().ok_or(anyhow!("Component NS is not set"))?.iter_mut() {
        if entry.path == Some("/pkg".to_string()) {
            let dir = entry.directory.take().ok_or(anyhow!("NS entry directory is not set"))?;
            let dir_proxy = dir.into_proxy()?;

            let result = read_file_from_dir(&dir_proxy, path).await;

            // Return the directory back to the `start_info`.
            entry.directory = Some(fidl::endpoints::ClientEnd::new(
                dir_proxy.into_channel().unwrap().into_zx_channel(),
            ));

            return result;
        }
    }

    Err(anyhow!("/pkg is not in the namespace"))
}

async fn read_lines_from_socket(socket: fidl::Socket) -> Result<Vec<String>, Error> {
    let mut async_socket = fidl::AsyncSocket::from_socket(socket)?;
    let mut buffer = vec![];
    async_socket.read_to_end(&mut buffer).await?;
    Ok(String::from_utf8(buffer)?
        .split('\n')
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect())
}

// Starts a component that runs the specified `binary` with the specified `args`.
fn start_command(
    base_start_info: &mut frunner::ComponentStartInfo,
    component_runner: &frunner::ComponentRunnerProxy,
    cwd: &str,
    binary: &str,
    args: &[&str],
) -> Result<(frunner::ComponentControllerProxy, ftest::StdHandles), Error> {
    let mut program_entries = vec![
        fdata::DictionaryEntry {
            key: "cwd".to_string(),
            value: Some(Box::new(fdata::DictionaryValue::Str(cwd.to_string()))),
        },
        fdata::DictionaryEntry {
            key: "binary".to_string(),
            value: Some(Box::new(fdata::DictionaryValue::Str(binary.to_string()))),
        },
        fdata::DictionaryEntry {
            key: "args".to_string(),
            value: Some(Box::new(fdata::DictionaryValue::StrVec(
                args.iter().map(|s| s.to_string()).collect::<Vec<String>>(),
            ))),
        },
    ];

    // Copy "environ" and "uid" from `base_start_info`.
    if let Some(fidl_fuchsia_data::Dictionary { entries: Some(entries), .. }) =
        base_start_info.program.as_ref()
    {
        for entry in entries {
            match entry.key.as_str() {
                "environ" | "uid" => {
                    program_entries.push(entry.clone());
                }
                _ => (),
            }
        }
    }

    let (numbered_handles, std_handles) = create_numbered_handles();
    let start_info = frunner::ComponentStartInfo {
        program: Some(fidl_fuchsia_data::Dictionary {
            entries: Some(program_entries),
            ..Default::default()
        }),
        numbered_handles,
        ..clone_start_info(base_start_info)?
    };

    Ok((start_test_component(start_info, component_runner)?, std_handles))
}
