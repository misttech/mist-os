// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::test::*;
use anyhow::{ensure, Context, Result};
use std::{
    fs::{create_dir_all, File},
    io::Write,
    path::Path,
    process::Stdio,
};

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
            let manifest_file = Path::new("sdk/manifest/core");
            if !manifest_file.exists() {
                create_dir_all(manifest_file.parent().unwrap())?;
                File::create(manifest_file)?.write_all(br#"{
                    "atoms": [{
                        "category": "partner",
                        "deps": [],
                        "files": [
                        {
                            "destination": "tools/x64/zxdb",
                            "source": "host_x64/zxdb"
                        },
                        {
                            "destination": "tools/x64/zxdb-meta.json",
                            "source": "host_x64/gen/src/developer/debug/zxdb/zxdb_sdk.meta.json"
                        }
                        ],
                        "gn-label": "//src/developer/debug/zxdb:zxdb_sdk(//build/toolchain:host_x64)",
                        "id": "sdk://tools/x64/zxdb",
                        "meta": "tools/x64/zxdb-meta.json",
                        "plasa": [],
                        "type": "host_tool"
                    }],
                    "ids": []
                }"#)?;
            }
        }

        let target = get_target_nodename().await?;
        let sdk = ffx_config::global_env_context()
            .context("loading global environment context")?
            .get_sdk()
            .await?;
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
            .ffx_cmd(&["--target", &target, "--config", &config, "log"])
            .await?
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
