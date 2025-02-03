// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Error;

mod env;
mod errors;
mod opts;
mod params;
mod run_tests;
mod schema;
mod test_config;

const INVALID_ARGS_CONFIG_EXIT_CODE: i32 = 222;

// We need multiple threads to stream stdout/err in parallel until we can switch this to use tokio.
#[fuchsia::main(threads = 2)]
async fn main() {
    let args = match opts::TestPilotArgs::from_env_and_cmd() {
        Ok(v) => v,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(INVALID_ARGS_CONFIG_EXIT_CODE);
        }
    };
    let test_config = parse_test_config(&args)
        .map_err(|e| {
            eprintln!("Invalid config: {}", e);
            std::process::exit(INVALID_ARGS_CONFIG_EXIT_CODE);
        })
        .unwrap();
    let _ = args.validate_config(&test_config).map_err(|e| {
        eprintln!("Invalid args: {}", e);
        std::process::exit(INVALID_ARGS_CONFIG_EXIT_CODE);
    });

    // TODO(b/294567715): Map error to output format.
    match run_tests::run_test(&args, &test_config.into()).await {
        Ok(exit_code) => std::process::exit(exit_code.code().unwrap()),
        Err(e) => {
            eprint!("Error running test: {}", e);
            std::process::exit(INVALID_ARGS_CONFIG_EXIT_CODE);
        }
    }
}

pub fn parse_test_config(
    args: &opts::TestPilotArgs,
) -> Result<test_config::TestConfiguration, Error> {
    test_config::TestConfiguration::try_from(args.test_config.as_path())
}
