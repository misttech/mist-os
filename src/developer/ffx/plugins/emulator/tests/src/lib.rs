// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

/// This file defines some e2e tests for ffx emu related workflows.

#[cfg(test)]
mod tests {
    use anyhow::Result;
    use ffx_emulator_stop_command_output::CommandStatus;
    use ffx_executor::strict::{LogOutputLocation, StrictContext};
    use ffx_executor::test::{TestCommandLineInfo, TestingError};
    use std::path::PathBuf;
    use std::str::FromStr;
    use tempfile::TempDir;

    const FFX_PATH: &str = env!("FFX_PATH");

    fn ssh_key_path(temp_dir: &TempDir) -> PathBuf {
        temp_dir.path().join("ssh_private_key")
    }

    async fn new_strict_context() -> Result<(StrictContext, TempDir)> {
        let temp_dir = tempfile::TempDir::new()?;
        assert!(temp_dir.path().exists());
        let emu_dir = temp_dir.path().join("emulators");
        std::fs::create_dir_all(&emu_dir)?;
        let ssh_priv_key = ssh_key_path(&temp_dir);
        let ssh_pub_key = temp_dir.path().join("ssh_public_key");
        let ffx_binary = PathBuf::from_str(FFX_PATH).unwrap();
        let mut ffx_root_dir = ffx_binary.clone();
        ffx_root_dir.pop();
        let ffx_strict_context = StrictContext::new(
            ffx_binary,
            LogOutputLocation::Stderr,
            [
                ("log.level", "debug"),
                ("ssh.priv", &ssh_priv_key.to_string_lossy()),
                ("ssh.pub", &ssh_pub_key.to_string_lossy()),
                ("ffx.subtool-search-paths", &ffx_root_dir.to_string_lossy()),
                ("emu.instance_dir", &emu_dir.to_string_lossy()),
            ]
            .into_iter()
            .map(|(k, v)| (k.to_owned(), v.to_string()))
            .collect(),
        );
        Ok((ffx_strict_context, temp_dir))
    }

    #[fuchsia::test]
    async fn test_strict_stop_no_emulators() {
        let (ffx_strict_ctx, _temp_dir) =
            new_strict_context().await.expect("creating ffx_strict context");
        // Expect a failed exit code.
        let test_data =
            vec![TestCommandLineInfo::new(vec!["emu", "stop"], |mut command_output| {
                // TODO(b/395982857): The tool should not print out two error messages upon
                // failure. For now, that is why the split is happening. The fix for this is
                // going to be complicated.
                let mut s_arr = command_output.stdout.split("\n");
                command_output.stdout = s_arr.next().unwrap().to_owned();

                let err: CommandStatus =
                    command_output.machine_output().map_err(TestingError::ParsingError)?;
                let CommandStatus::UserError { ref message } = err else {
                    return Err(TestingError::MatchingError(format!(
                        "wrong error returned. Expected `UserError`, Got: `{err:?}`"
                    )));
                };
                let expected = "does not exist";
                if message.contains(expected) {
                    Ok(())
                } else {
                    Err(TestingError::MatchingError(format!(
                        "Expected: '{}', Got: '{}'",
                        expected.to_owned(),
                        message
                    )))
                }
            })];
        TestCommandLineInfo::run_command_lines(&ffx_strict_ctx, test_data)
            .await
            .expect("run commands");
    }
}
