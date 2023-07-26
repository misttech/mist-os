// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

/// This file defines some e2e tests for ffx emu related workflows.

#[cfg(test)]
mod tests {
    use anyhow::Result;
    use ffx_config::{SdkRoot, TestEnv};
    use ffx_isolate::{test::TestCommandLineInfo, Isolate, SearchContext};
    use serde_json::Value;
    use std::path::PathBuf;
    use tempfile::TempDir;

    const PRODUCT_BUNDLE_PATH: &str = env!("PRODUCT_BUNDLE_PATH_FROM_GN");

    #[cfg(target_arch = "x86_64")]
    fn get_tools_relpath() -> String {
        String::from("tools/x64")
    }

    #[cfg(target_arch = "aarch64")]
    fn get_tools_relpath() -> String {
        String::from("tools/arm64")
    }

    fn is_empty(s: &str) -> bool {
        s.is_empty()
    }

    /// Creates a new Isolate for ffx and a temp directory for holding
    /// test artifacts, such as ssh keys.
    /// These objects need to exist for the duration of the test,
    /// since the underlying data is cleaned up when they are dropped.
    async fn new_ffx_isolate(
        name: &str,
        sdk_root_dir: PathBuf,
        ssh_priv_key: Option<PathBuf>,
    ) -> Result<(Isolate, TempDir, TestEnv)> {
        let temp_dir = tempfile::TempDir::new()?;
        assert!(temp_dir.path().exists());
        let test_env = ffx_config::test_init().await?;

        // Create paths to the files to hold the ssh key pair.
        // The key is not generated here, since ffx will generate the
        // key if it is missing when starting an emulator or flashing a device.
        // If a  private key is supplied, it is used, but the public key path
        // is still in the temp dir.
        let ssh_priv_key = if let Some(existing_key) = ssh_priv_key {
            existing_key.clone()
        } else {
            temp_dir.path().join("ssh_private_key")
        };
        let ssh_pub_key = temp_dir.path().join("ssh_public_key");
        let subtool_search_paths =
            ffx_config::query("ffx.subtool-search-paths").get().await.unwrap_or_default();

        let ffx_isolate = Isolate::new_with_search(
            name,
            SearchContext::Runtime {
                ffx_path: sdk_root_dir.join(get_tools_relpath()).join("ffx"),
                sdk_root: Some(SdkRoot::Full(sdk_root_dir)),
                subtool_search_paths,
            },
            ssh_priv_key.clone(),
            &test_env.context,
        )
        .await?;

        // Set the path to the ssh keys
        ffx_isolate.ffx(&["config", "set", "ssh.priv", &ssh_priv_key.to_string_lossy()]).await?;
        ffx_isolate.ffx(&["config", "set", "ssh.pub", &ssh_pub_key.to_string_lossy()]).await?;

        Ok((ffx_isolate, temp_dir, test_env))
    }

    async fn build_emulator_log_path(ffx_isolate: &Isolate) -> Result<String> {
        let output = ffx_isolate.ffx(&["config", "get", "log.dir"]).await?;

        let base_log_dir: String =
            serde_json::from_str::<Value>(&output.stdout)?.as_str().expect("parse log.dir").into();
        Ok(format!("{base_log_dir}/emulator.log"))
    }

    #[fuchsia::test]
    async fn test_help() -> Result<()> {
        // When this object is dropped, the daemon is stopped.
        let (ffx_isolate, _temp_dir, _test_env) =
            new_ffx_isolate("test_help", "sdk/exported/core".into(), None).await?;

        let expected_stdout = |x: &str| x.starts_with("Usage: ffx [");
        let cmd = TestCommandLineInfo::new(vec!["--help"], expected_stdout, is_empty, 0);

        TestCommandLineInfo::run_command_lines(&ffx_isolate, vec![cmd])
            .await
            .expect("run command lines");
        Ok(())
    }

    #[fuchsia::test]
    async fn test_default_emu() {
        let (ffx_isolate, _temp_dir, _test_env) =
            new_ffx_isolate("test_emu", "sdk/exported/core".into(), None)
                .await
                .expect("new test isolate");
        let emulator_log = build_emulator_log_path(&ffx_isolate).await.expect("emulator path");
        let test_data = vec![
            TestCommandLineInfo::new(
                vec!["config", "set", "log.level", "debug"],
                is_empty,
                is_empty,
                0,
            ),
            TestCommandLineInfo::new(
                vec!["config", "set", "daemon.autostart", "true"],
                is_empty,
                is_empty,
                0,
            ),
            TestCommandLineInfo::new(
                vec!["daemon", "stop", "--no-wait"],
                |s| s == "No daemon was running.\n",
                is_empty,
                0,
            ),
            TestCommandLineInfo::new(vec!["emu", "list"], is_empty, is_empty, 0),
            TestCommandLineInfo::new(
                vec!["target", "list"],
                is_empty,
                |s| s == "No devices found.\n",
                0,
            ),
            TestCommandLineInfo::new(
                vec![
                    "emu",
                    "start",
                    "--headless",
                    "--net",
                    "user",
                    "--log",
                    &emulator_log,
                    PRODUCT_BUNDLE_PATH,
                ],
                |s| s.contains("\nEmulator is ready.\n"),
                |_| true,
                0,
            ),
            TestCommandLineInfo::new(
                vec!["emu", "list"],
                |s| s == "[running]       fuchsia-emulator\n",
                is_empty,
                0,
            ),
            TestCommandLineInfo::new(
                vec!["target", "list"],
                |s| s.contains("fuchsia-emulator") && s.contains("Y\n"),
                is_empty,
                0,
            ),
            TestCommandLineInfo::new(vec!["target", "show"], |_| true, is_empty, 0),
            TestCommandLineInfo::new(vec!["emu", "show"], |_| true, is_empty, 0),
            TestCommandLineInfo::new(vec!["emu", "stop"], |_| true, is_empty, 0),
            TestCommandLineInfo::new(
                vec!["daemon", "stop", "--no-wait"],
                |s| s == "Stopped daemon.\n",
                is_empty,
                0,
            ),
        ];

        TestCommandLineInfo::run_command_lines(&ffx_isolate, test_data).await.expect("run commands")
    }

    #[fuchsia::test]
    async fn test_large_device_emu() {
        let (ffx_isolate, _temp_dir, _test_env) =
            new_ffx_isolate("test_emu", "sdk/exported/core".into(), None)
                .await
                .expect("test isolate");
        let emulator_log = build_emulator_log_path(&ffx_isolate).await.expect("emulator log path");
        let test_data = vec![
            TestCommandLineInfo::new(
                vec!["config", "set", "log.level", "debug"],
                is_empty,
                is_empty,
                0,
            ),
            TestCommandLineInfo::new(
                vec!["config", "set", "daemon.autostart", "true"],
                is_empty,
                is_empty,
                0,
            ),
            TestCommandLineInfo::new(
                vec!["daemon", "stop", "--no-wait"],
                |s| s == "No daemon was running.\n",
                is_empty,
                0,
            ),
            TestCommandLineInfo::new(vec!["emu", "list"], is_empty, is_empty, 0),
            TestCommandLineInfo::new(
                vec!["emu", "start", "--device-list", PRODUCT_BUNDLE_PATH],
                |s| s.contains("virtual_device_large"),
                is_empty,
                0,
            ),
            TestCommandLineInfo::new(
                vec!["target", "list"],
                is_empty,
                |s| s == "No devices found.\n",
                0,
            ),
            TestCommandLineInfo::new(
                vec![
                    "emu",
                    "start",
                    "--headless",
                    "--net",
                    "user",
                    "--log",
                    &emulator_log,
                    "--device",
                    "virtual_device_large",
                    PRODUCT_BUNDLE_PATH,
                ],
                |s| s.contains("\nEmulator is ready.\n"),
                |_| true,
                0,
            ),
            TestCommandLineInfo::new(
                vec!["emu", "list"],
                |s| s == "[running]       fuchsia-emulator\n",
                is_empty,
                0,
            ),
            TestCommandLineInfo::new(
                vec!["target", "list"],
                |s| s.contains("fuchsia-emulator") && s.contains("Y\n"),
                is_empty,
                0,
            ),
            TestCommandLineInfo::new(vec!["target", "show"], |_| true, is_empty, 0),
            TestCommandLineInfo::new(vec!["emu", "show"], |_| true, is_empty, 0),
            TestCommandLineInfo::new(vec!["emu", "stop"], |_| true, is_empty, 0),
            TestCommandLineInfo::new(
                vec!["daemon", "stop", "--no-wait"],
                |s| s == "Stopped daemon.\n",
                is_empty,
                0,
            ),
        ];

        TestCommandLineInfo::run_command_lines(&ffx_isolate, test_data)
            .await
            .expect("test command line")
    }
}
