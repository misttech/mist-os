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
use std::collections::HashMap;

struct LtpTestDefinition {
    name: String,
    command: String,
}

fn parse_test_definition(test_def: &str) -> LtpTestDefinition {
    let test_def = test_def.trim();
    match test_def.split_once(' ') {
        Some((name, command)) => {
            LtpTestDefinition { name: name.to_string(), command: command.trim().to_string() }
        }
        None => LtpTestDefinition { name: test_def.to_string(), command: test_def.to_string() },
    }
}

async fn read_tests_list(
    start_info: &mut frunner::ComponentStartInfo,
) -> Result<Vec<LtpTestDefinition>, Error> {
    let program = start_info.program.as_ref().unwrap();
    let tests_list_file = get_str_value_from_dict(program, "tests_list")?;
    Ok(read_file_from_component_ns(start_info, &tests_list_file)
        .await
        .with_context(|| format!("Failed to read {}", tests_list_file))?
        .split('\n')
        .filter(|s| !s.is_empty())
        .map(parse_test_definition)
        .collect())
}

pub async fn get_cases_list_for_ltp(
    mut start_info: frunner::ComponentStartInfo,
) -> Result<Vec<ftest::Case>, Error> {
    let tests_list = read_tests_list(&mut start_info)
        .await?
        .iter()
        .map(|t| t.name.clone())
        .collect::<Vec<String>>();

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

    let mut test_commands = HashMap::<String, String>::new();
    read_tests_list(&mut start_info).await?.drain(..).for_each(|t| {
        test_commands.insert(t.name, t.command);
    });

    let file_content =
        read_file_from_component_ns(&mut start_info, "data/test_results.json").await?;
    let expected_test_results: HashMap<String, String> = serde_json::from_str(&file_content)?;

    for test in tests {
        let test_name = test.name.as_ref().expect("No test name");
        let test_command: &String = test_commands
            .get(test_name)
            .ok_or_else(|| anyhow!("Invalid test name: {}", test_name))?;
        let command = vec!["/bin/sh", "-c", test_command];

        let (component_controller, std_handles) =
            start_command(&mut start_info, component_runner, &base_path, &command)?;

        let (case_listener_proxy, case_listener) = create_proxy::<ftest::CaseListenerMarker>()?;
        run_listener_proxy.on_test_case_started(&test, std_handles, case_listener)?;

        let allow_skipped = expected_test_results[test_name] == "IGNORED";
        let result =
            read_ltp_test_result(component_controller.take_event_stream(), allow_skipped).await;
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

// Starts a component that runs the specified `binary` with the specified `args`.
fn start_command(
    base_start_info: &mut frunner::ComponentStartInfo,
    component_runner: &frunner::ComponentRunnerProxy,
    cwd: &str,
    command: &[&str],
) -> Result<(frunner::ComponentControllerProxy, ftest::StdHandles), Error> {
    let mut program_entries = vec![
        fdata::DictionaryEntry {
            key: "cwd".to_string(),
            value: Some(Box::new(fdata::DictionaryValue::Str(cwd.to_string()))),
        },
        fdata::DictionaryEntry {
            key: "binary".to_string(),
            value: Some(Box::new(fdata::DictionaryValue::Str(command[0].to_string()))),
        },
        fdata::DictionaryEntry {
            key: "args".to_string(),
            value: Some(Box::new(fdata::DictionaryValue::StrVec(
                command.iter().skip(1).map(|s| s.to_string()).collect::<Vec<String>>(),
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

async fn read_ltp_test_result(
    event_stream: frunner::ComponentControllerEventStream,
    allow_skipped: bool,
) -> ftest::Result_ {
    // Base value used by the ComponentRunner implementation in Starnix to
    // return non-zero error codes (see src/starnix/kernel/execution/component_runner.rs ).
    //
    // TODO(https://fxbug.dev/42081234): Cleanup this once we have a proper mechanism to
    // get Linux exit code from component runner.
    const COMPONENT_EXIT_CODE_BASE: i32 = 1024;

    // Status code used by LTP tests for skipped tests. See
    // https://github.com/linux-test-project/ltp/blob/master/include/tst_res_flags.h .
    const LTP_RESULT_TCONF: i32 = COMPONENT_EXIT_CODE_BASE + 32;

    match read_component_epitaph(event_stream).await {
        zx::Status::OK => {
            ftest::Result_ { status: Some(ftest::Status::Passed), ..Default::default() }
        }
        status if status.into_raw() == LTP_RESULT_TCONF && allow_skipped => {
            ftest::Result_ { status: Some(ftest::Status::Skipped), ..Default::default() }
        }
        _ => ftest::Result_ { status: Some(ftest::Status::Failed), ..Default::default() },
    }
}
