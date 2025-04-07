// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::test::*;
use anyhow::{ensure, Context, Result};
use ffx_executor::FfxExecutor;
use std::fs::{create_dir_all, File};
use std::io::Write;
use std::path::Path;
use std::process::Stdio;

pub mod include_log {
    use std::io::{BufRead, BufReader};

    use fuchsia_async::unblock;

    use super::*;
    pub(crate) async fn test_log_run_normal() -> Result<()> {
        // If the test is running on CI/CQ bots, it's isolated with only files listed as test_data
        // available. We have added zxdb and zxdb-meta.json in ffx-e2e-test-data but we have to
        // also provide an index file at host_x64/sdk/manifest/host_tools.modular.
        // Only when invoked from ffx-e2e-with-target.sh we could get sdk.root=.
        if ffx_config::get::<String, _>("sdk.root").await.unwrap_or_default() == "." {
            ensure!(cfg!(target_arch = "x86_64"), "The test only supports x86_64 for now.");
        }

        let target = get_target_addr().await?;
        let sdk = ffx_config::global_env_context()
            .context("loading global environment context")?
            .get_sdk()?;
        let isolate = new_isolate("log-run-normal").await?;
        isolate.start_daemon().await?;
        // Test with proactive logging enabled
        run_logging_e2e_test(&sdk, &isolate, &target, true).await?;
        // Test without proactive logging enabled.
        run_logging_e2e_test(&sdk, &isolate, &target, false).await?;
        Ok(())
    }

    async fn run_logging_e2e_test(
        sdk: &ffx_config::Sdk,
        isolate: &ffx_isolate::Isolate,
        target: &String,
        enable_proactive_logger: bool,
    ) -> Result<(), anyhow::Error> {
        let mut config = "sdk.root=".to_owned();
        config.push_str(sdk.get_path_prefix().to_str().unwrap());
        if sdk.get_version() == &sdk::SdkVersion::InTree {
            config.push_str(",sdk.type=in-tree");
        }
        config.push_str(&format!(",proactive_log.enabled={}", enable_proactive_logger));
        let mut child = isolate
            .make_ffx_cmd(&["--target", &target, "--config", &config, "log"])?
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let output = child.stdout.take().unwrap();
        let background_task = unblock(move || {
            let mut reader = BufReader::new(output);
            let mut output = String::new();
            loop {
                reader.read_line(&mut output).unwrap();
                if output.contains("welcome to Zircon") {
                    break;
                }
            }
        });
        background_task.await;
        Ok(())
    }
}
