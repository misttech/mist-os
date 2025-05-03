// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use ffx_e2e_emu::IsolatedEmulator;
use std::io::Write;
use std::path::PathBuf;
use tempfile::NamedTempFile;
use tracing::info;

#[fuchsia::test(logging_minimum_severity = "TRACE")]
async fn profiler_symbolize_data() {
    info!("starting emulator...");
    let emu = IsolatedEmulator::start("test-ffx-symbolize-lib").await.unwrap();
    info!("running print_fn_ptr component...");
    let outputs = run_print_symbolize_data(&emu).await;
    let mut symbolize_input = NamedTempFile::new().expect("Failed to create temp file");
    writeln!(symbolize_input, "{}", outputs).expect("Failed to write to temp file");
    symbolize_input.flush().expect("Failed to flush");
    let profiler_record_path: PathBuf = symbolize_input.path().to_path_buf();
    //set up output path
    let symbolized_output = NamedTempFile::new().expect("Failed to create temp file");
    let symbolized_output_path: PathBuf = symbolized_output.path().to_path_buf();

    //call the symbolize function
    let _res = ffx_profiler::symbolize::symbolize_with_context(
        &profiler_record_path,
        &symbolized_output_path,
        false,
        emu.env_context(),
    )
    .unwrap();
    let read_data_string = std::fs::read_to_string(&symbolized_output_path).unwrap();
    println!("the symbolized data: {}", read_data_string);
    assert!(read_data_string.contains(
        "function: \"print_symbolize_data_bin::get_function_addr::to_be_symbolized_1()\","
    ));
    assert!(read_data_string.contains(
        "function: \"print_symbolize_data_bin::get_function_addr::to_be_symbolized_2()\","
    ));
    assert!(read_data_string.contains(
        "function: \"print_symbolize_data_bin::get_function_addr::to_be_symbolized_3()\","
    ));
    assert!(read_data_string.contains(
        "function: \"print_symbolize_data_bin::get_function_addr::to_be_symbolized_4()\","
    ));
    assert!(read_data_string.contains(
        "function: \"print_symbolize_data_bin::get_function_addr::to_be_symbolized_5()\","
    ));
    assert!(read_data_string.contains("zx_channel_create\","));
}

async fn run_print_symbolize_data(emu: &IsolatedEmulator) -> String {
    let stdout = emu
        .ffx_output(&[
            // JSON output prevents the command from printing its status messages to stdout.
            "--machine",
            "json",
            "component",
            "run",
            "/core/ffx-laboratory:print_symbolize_data",
            "fuchsia-pkg://fuchsia.com/print_symbolize_data#meta/print_symbolize_data.cm",
            // Ensure we get the component's stdout to the ffx command.
            "--connect-stdio",
        ])
        .await
        .unwrap();
    serde_json::from_str(&stdout).unwrap()
}
