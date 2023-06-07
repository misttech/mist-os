// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::helpers::*;
use anyhow::{anyhow, Context, Error};
use fidl::endpoints::{create_proxy, Proxy};
use fidl_fuchsia_component_runner as frunner;
use fidl_fuchsia_data as fdata;
use fidl_fuchsia_io as fio;
use fidl_fuchsia_test as ftest;
use fuchsia_zircon::sys::ZX_CHANNEL_MAX_MSG_BYTES;
use futures::{AsyncReadExt, TryStreamExt};
use rust_measure_tape_for_case::Measurable as _;
use test_runners_lib::elf::SuiteServerError;

pub async fn handle_case_iterator_for_ltp(
    mut start_info: frunner::ComponentStartInfo,
    component_runner: &frunner::ComponentRunnerProxy,
    mut stream: ftest::CaseIteratorRequestStream,
) -> Result<(), Error> {
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
                "/system/bin/ls",
                &[base_path.as_str()],
            )?;

            read_lines_from_socket(std_handles.out.unwrap())
                .await
                .with_context(|| format!("Failed to enumerate tests in {}", base_path))?
        }
    };

    let cases: Vec<ftest::Case> = tests_list
        .iter()
        .map(|name| ftest::Case { name: Some(name.clone()), ..Default::default() })
        .collect();
    let mut remaining_cases = &cases[..];

    while let Some(event) = stream.try_next().await? {
        match event {
            ftest::CaseIteratorRequest::GetNext { responder } => {
                // Paginate cases
                // Page overhead of message header + vector
                let mut bytes_used: usize = 32;
                let mut case_count = 0;
                for case in remaining_cases {
                    bytes_used += case.measure().num_bytes;
                    if bytes_used > ZX_CHANNEL_MAX_MSG_BYTES as usize {
                        break;
                    }
                    case_count += 1;
                }
                responder
                    .send(&remaining_cases[..case_count])
                    .map_err(SuiteServerError::Response)?;
                remaining_cases = &remaining_cases[case_count..];
            }
        }
    }
    Ok(())
}

pub async fn run_ltp_cases(
    tests: Vec<ftest::Invocation>,
    mut start_info: frunner::ComponentStartInfo,
    run_listener_proxy: &ftest::RunListenerProxy,
    component_runner: &frunner::ComponentRunnerProxy,
) -> Result<(), Error> {
    let program = start_info.program.as_ref().unwrap();
    let base_path = get_str_value_from_dict(program, "tests_dir")?;
    for test in tests {
        let test_path = format!("{}/{}", base_path, test.name.as_ref().expect("No test name"));
        let (component_controller, std_handles) =
            start_command(&mut start_info, component_runner, test_path.as_str(), &[])?;
        let (case_listener_proxy, case_listener) = create_proxy::<ftest::CaseListenerMarker>()?;
        run_listener_proxy.on_test_case_started(&test, std_handles, case_listener)?;

        let result = read_result(component_controller.take_event_stream()).await;
        case_listener_proxy.finished(&result)?;
    }

    Ok(())
}

fn get_opt_str_value_from_dict(
    dict: &fdata::Dictionary,
    name: &str,
) -> Result<Option<String>, Error> {
    match runner::get_value(dict, name) {
        Some(fdata::DictionaryValue::Str(value)) => Ok(Some(value.clone())),
        Some(_) => Err(anyhow!("{} must a string", name)),
        _ => Ok(None),
    }
}

fn get_str_value_from_dict(dict: &fdata::Dictionary, name: &str) -> Result<String, Error> {
    match get_opt_str_value_from_dict(dict, name)? {
        Some(s) => Ok(s),
        None => Err(anyhow!("{} is not specified", name)),
    }
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
    binary: &str,
    args: &[&str],
) -> Result<(frunner::ComponentControllerProxy, ftest::StdHandles), Error> {
    let mut program_entries = vec![
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
