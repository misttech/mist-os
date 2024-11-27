// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::helpers::*;
use anyhow::Error;
use fidl::endpoints::create_proxy;
use std::collections::HashMap;
use {
    fidl_fuchsia_component_runner as frunner, fidl_fuchsia_data as fdata,
    fidl_fuchsia_test as ftest,
};

pub async fn run_selinux_cases(
    tests: Vec<ftest::Invocation>,
    mut start_info: frunner::ComponentStartInfo,
    run_listener_proxy: &ftest::RunListenerProxy,
    component_runner: &frunner::ComponentRunnerProxy,
) -> Result<(), Error> {
    let mut test_commands = HashMap::<String, String>::new();
    read_tests_list(&mut start_info).await?.drain(..).for_each(|t| {
        test_commands.insert(t.name, t.command);
    });

    for test in tests {
        let test_name = test.name.as_ref().expect("No test name");

        let (component_controller, std_handles) =
            start_selinux(&mut start_info, component_runner, test_name)?;

        let (case_listener_proxy, case_listener) = create_proxy::<ftest::CaseListenerMarker>();
        run_listener_proxy.on_test_case_started(&test, std_handles, case_listener)?;

        let result = read_selinux_test_result(component_controller.take_event_stream()).await;
        case_listener_proxy.finished(&result)?;
    }

    Ok(())
}

fn start_selinux(
    base_start_info: &mut frunner::ComponentStartInfo,
    component_runner: &frunner::ComponentRunnerProxy,
    test_name: &str,
) -> Result<(frunner::ComponentControllerProxy, ftest::StdHandles), Error> {
    let mut program_entries = vec![fdata::DictionaryEntry {
        key: "environ".to_string(),
        value: Some(Box::new(fdata::DictionaryValue::StrVec(vec![
            format!("SUBDIRS={}", test_name.to_owned()),
            "PATH=/usr/bin:/bin:/usr/sbin:/sbin:/tmp/selinux-testsuite/tests".to_string(),
        ]))),
    }];

    if let Some(fidl_fuchsia_data::Dictionary { entries: Some(entries), .. }) =
        base_start_info.program.as_ref()
    {
        for entry in entries {
            match entry.key.as_str() {
                "binary" | "uid" | "seclabel" | "fsseclabel" => {
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

async fn read_selinux_test_result(
    event_stream: frunner::ComponentControllerEventStream,
) -> ftest::Result_ {
    match read_component_epitaph(event_stream).await {
        zx::Status::OK => {
            ftest::Result_ { status: Some(ftest::Status::Passed), ..Default::default() }
        }
        _ => ftest::Result_ { status: Some(ftest::Status::Failed), ..Default::default() },
    }
}
