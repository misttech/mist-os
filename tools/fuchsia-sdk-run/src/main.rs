// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::collections::HashMap;
use std::fmt::{Debug, Display, Write};
use std::os::unix::process::CommandExt;
use std::process::{Command, Output, Stdio};

use anyhow::Context;
use argh::FromArgs;
use ffx_command::{Ffx, FFX_WRAPPER_INVOKE};
use ffx_config::environment::ExecutableKind::MainFfx;
use ffx_config::{EnvironmentContext, SdkRoot};
use ffx_config_domain::ConfigDomain;

use camino::Utf8Path;

const NOT_FOUND_CODE: u8 = 127;
const SCRIPT_FAILED: u8 = 100;
const BOOTSTRAP_INVALID_STATE: u8 = 101;
const SDK_NOT_FOUND: u8 = 110;
const SDK_TOOL_NOT_FOUND: u8 = 111;

trait DisplayError: Display + Debug {}
impl<T> DisplayError for T where T: Display + Debug {}

#[derive(Debug)]
struct Exit {
    message: Box<dyn DisplayError>,
    code: u8,
}

impl From<Exit> for u8 {
    fn from(value: Exit) -> Self {
        value.code
    }
}

impl From<anyhow::Error> for Exit {
    fn from(value: anyhow::Error) -> Self {
        let message = Box::new(value);
        let code = NOT_FOUND_CODE;
        Self { message, code }
    }
}

impl From<ffx_command::Error> for Exit {
    fn from(value: ffx_command::Error) -> Self {
        let message = Box::new(value);
        let code = NOT_FOUND_CODE;
        Self { message, code }
    }
}

impl Exit {
    fn from_early_exit(mut error: argh::EarlyExit, cmd: &str) -> Self {
        let code;
        match error.status {
            Ok(()) => {
                // an ok early exit from argh means it was help output from the ffx
                // parse, to which we'll want to add a note about how to see the
                // full list of commands
                Ffx::more_commands_help(&mut error.output, cmd).unwrap();
                code = 0;
            }
            Err(()) => {
                // an error early exit from argh means it was an error parsing
                // arguments and we should add a note saying that this was run
                // through a helper script and it's possible that it doesn't
                // know about a new ffx argument.
                write!(
                    &mut error.output,
                    "\nNote: This command was run through the `fuchsia-sdk-run` binary,\n\
                    which may not recognize ffx command line arguments added after it\n\
                    was built. You may need to update your copy of `fuchsia-sdk-run\n\
                    if the given command line should have parsed correctly."
                )
                .unwrap();
                code = 1;
            }
        }
        let message = Box::new(error.output);
        Self { message, code }
    }

    fn from_failed_exec(sdk: &ffx_config::Sdk, exe_path: &str, err: std::io::Error) -> Self {
        let message = Box::new(format!(
            "fuchsia-sdk-run: Failed to execute tool binary from the active sdk:\n\
            SDK Path: {}\n\
            Tool Path: {exe_path}\n\
            \n\
            Error: {err}",
            sdk.get_path_prefix().to_string_lossy(),
        ));
        let code = NOT_FOUND_CODE;
        Self { message, code }
    }
}

impl Display for Exit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        Display::fmt(&self.message, f)
    }
}

struct SdkToolRunner {
    ffx: Ffx,
    arg0: String,
    cmd: String,
    args: Vec<String>,
}

impl SdkToolRunner {
    fn from_args(args: impl IntoIterator<Item = String>) -> Result<Self, Exit> {
        let mut args = args.into_iter();
        let arg0 =
            args.next().context("No arguments provided to fuchsia-sdk-run, nothing to run.")?;
        let cmd = Utf8Path::new(&arg0)
            .file_name()
            .with_context(|| format!("fuchsia-sdk-run: '{arg0}' is not a valid host tool name"))?;

        let args = Vec::from_iter(args);
        let ffx = match cmd {
            "fuchsia-sdk-run" => return Self::from_args(args),
            "ffx" => parse_ffx_args(cmd, &args)?,
            _ => Ffx::default(),
        };
        Ok(Self { ffx, arg0: arg0.to_string(), cmd: cmd.to_string(), args })
    }
}

fn parse_ffx_args(cmd: &str, args: &[impl AsRef<str>]) -> Result<Ffx, Exit> {
    let ffx_args = Vec::from_iter(args.iter().map(AsRef::as_ref));
    Ffx::from_args(&[&cmd], &ffx_args).map_err(|err| Exit::from_early_exit(err, cmd))
}

/// Runs the command, redirecting stderr to the caller and capturing the output
/// and exit code to make decisions on. Returns the output if the update command
/// ran successfully, and `Exit` if it didn't.
/// Does not check the post-conditions of the run.
fn run_update_command(mut command: Command) -> Result<Output, Exit> {
    let program = command.get_program().to_owned();
    let output = command.stderr(Stdio::inherit()).output().map_err(|err| {
        log::warn!("Command could not be run: {command:?}");
        log::warn!("Error from failed command: {err:?}");
        Exit {
            message: Box::new(format!("Failed to run check script {program:?}:\n{err}")),
            code: SCRIPT_FAILED,
        }
    })?;

    if output.status.success() {
        log::trace!("Command succeeded: {command:?}");
        Ok(output)
    } else {
        log::debug!("Command failed: {command:?}");
        log::debug!("Failed command output: {output:?}");
        Err(Exit {
            message: Box::new(format!(
                "Check script {program:?} did not succeed. Output:\n\n{}",
                String::from_utf8_lossy(&output.stdout)
            )),
            code: output.status.code().map_or(SCRIPT_FAILED, |n| n as u8),
        })
    }
}

fn sdk_load_err(err: anyhow::Error) -> Exit {
    Exit { message: Box::new(err), code: SDK_NOT_FOUND }
}

fn sdk_tool_not_found_err(err: anyhow::Error) -> Exit {
    Exit { message: Box::new(err), code: SDK_TOOL_NOT_FOUND }
}

fn ensure_config(domain: &mut ConfigDomain) -> Result<(), Exit> {
    if let Some(cmd) = domain.needs_config_bootstrap() {
        // run the command
        log::trace!("Running bootstrap command: {cmd:?}");
        let output = run_update_command(cmd)?;
        // double check we don't still need bootstrapping, and return an
        // error if we do.
        if let Some(_) = domain.needs_config_bootstrap() {
            return Err(Exit {
                message: Box::new(format!("Configuration bootstrap command succeeded, but configuration was not established. Output of bootstrap command:\n\n{}", String::from_utf8_lossy(&output.stdout))),
                code: BOOTSTRAP_INVALID_STATE,
            });
        }
    }
    Ok(())
}

async fn load_domain_sdk_root(
    env: &EnvironmentContext,
    domain: &ConfigDomain,
) -> Result<SdkRoot, Exit> {
    // load details we need for the sdk updating process and check if we
    // need to and how
    let sdk_root = env.get_sdk_root().await.map_err(sdk_load_err)?;
    let Some(mut known_states) = domain.load_sdk_check_manifest() else {
        // no configured manifest, so no way to check
        return Ok(sdk_root);
    };
    if let Some(cmd) = domain.needs_sdk_update(&sdk_root, &mut known_states) {
        // run the command
        log::trace!("Running sdk update command: {cmd:?}");
        let output = run_update_command(cmd)?;
        // double check we have an sdk in place now.
        if !sdk_root.manifest_exists() {
            return Err(Exit {
                message: Box::new(format!("SDK check command succeeded, but no SDK found with '{sdk_root:?}'. Output of check command:\n\n{}", String::from_utf8_lossy(&output.stdout))),
                code: 100,
            });
        }
        // and write out the new manifest if we do.
        if let Err(err) = domain.save_sdk_check_manifest(&known_states) {
            eprintln!("Warning: fuchsia-sdk-run failed to write SDK version check manifest, is the directory read-only? Error:\n{err}");
        }
    }
    Ok(sdk_root)
}

async fn run(
    args: impl IntoIterator<Item = String>,
    env: impl IntoIterator<Item = (String, String)>,
) -> Result<(), Exit> {
    let runner = SdkToolRunner::from_args(args)?;
    let mut env = runner.ffx.load_context_with_env(MainFfx, HashMap::from_iter(env))?;
    // first ensure config exists with a mutable borrow so it can be updated
    // after the check script.
    if let Some(domain) = env.get_config_domain_mut() {
        ensure_config(domain)?;
    }
    // then re-borrow immutably to verify the SDK, or just get the sdk root
    // normally if we're not in a config domain.
    let sdk_root = if let Some(domain) = env.get_config_domain() {
        load_domain_sdk_root(&env, domain).await?
    } else {
        env.get_sdk_root().await.map_err(sdk_load_err)?
    };
    // and finally load the sdk for good.
    let sdk = sdk_root.get_sdk().map_err(sdk_load_err)?;

    let mut command = sdk.get_host_tool_command(&runner.cmd).map_err(sdk_tool_not_found_err)?;
    command.args(&runner.args);
    command.env(FFX_WRAPPER_INVOKE, runner.arg0);
    let exe_path = command.get_program().to_string_lossy().into_owned();
    // [`CommandExt::exec`] doesn't return if successful, the current process is
    // replaced, so this just always returns an error if anything at all.
    Err(Exit::from_failed_exec(&sdk, &exe_path, command.exec()))
}

#[fuchsia::main(logging_minimum_severity = "warn")]
async fn main() {
    let exit_code = match run(std::env::args(), std::env::vars()).await {
        Ok(()) => {
            eprintln!("ERROR: fuchsia-sdk-run exited without error or running a host tool!");
            NOT_FOUND_CODE
        }
        Err(err) => {
            eprintln!("{err}");
            err.code
        }
    };
    std::process::exit(exit_code.into())
}

#[cfg(test)]
mod test {
    use super::*;
    use ffx_config::{ConfigLevel, TestEnv};
    use ffx_config_domain::{ConfigPath, FuchsiaEnv};

    use camino::Utf8PathBuf;

    #[test]
    fn test_run_commands() {
        let cmd = Command::new("true");
        let res = run_update_command(cmd).expect("to run the command");
        assert!(res.status.success());

        let cmd = Command::new("false");
        let res = run_update_command(cmd).expect_err("to fail to run the command");
        assert_eq!(res.code, 1, "should exit with the exit code of the command run");

        let cmd = Command::new("this-command-definitely-does-not-exist");
        let res = run_update_command(cmd).expect_err("to fail to run the command");
        assert_eq!(res.code, SCRIPT_FAILED, "script failure exit code");
    }

    async fn test_bootstrap_with(
        test_env: &TestEnv,
        fuchsia_env: &FuchsiaEnv,
    ) -> Result<SdkRoot, Exit> {
        let root = Utf8Path::from_path(test_env.isolate_root.path()).expect("utf8 path");
        let mut domain =
            ConfigDomain::load_from_contents(root.join("fuchsia_env.toml"), fuchsia_env.clone())
                .expect("to load domain");

        ensure_config(&mut domain)?;
        load_domain_sdk_root(&test_env.context, &domain).await
    }

    async fn set_sdk_root_config(test_env: &TestEnv) {
        // set the sdk root in the environment context to the exported sdk
        // from the build
        let sdk_root =
            Utf8PathBuf::from("sdk/exported/core").canonicalize_utf8().expect("Exported SDK");
        test_env
            .context
            .query("sdk.root")
            .level(Some(ConfigLevel::User))
            .set(sdk_root.as_str().into())
            .await
            .unwrap();
    }

    fn shell_cmd(cmd: &str) -> Vec<String> {
        Vec::from_iter(["sh", "-c", cmd].into_iter().map(str::to_owned))
    }

    #[fuchsia::test]
    async fn test_bootstrapping() {
        let test_env = ffx_config::test_init().await.unwrap();
        let mut fuchsia_env = FuchsiaEnv::default();
        set_sdk_root_config(&test_env).await;

        println!("test root: {:?}", test_env.isolate_root.path());

        // empty config domain should be able to find the sdk root with
        // no bootstrapping necessary
        test_bootstrap_with(&test_env, &fuchsia_env)
            .await
            .expect("empty config domain should find the sdk");

        // if the config file doesn't exist and there's no bootstrap command set it should
        // not require bootstrapping
        fuchsia_env.fuchsia.project.build_config_path = Some(ConfigPath::relative("config.json"));
        test_bootstrap_with(&test_env, &fuchsia_env)
            .await
            .expect("no bootstrap command should still find the sdk");

        // if the bootstrap command is set, it should require running the command
        // and will fail if it returns false
        fuchsia_env.fuchsia.project.bootstrap_command = Some(shell_cmd("false"));
        test_bootstrap_with(&test_env, &fuchsia_env)
            .await
            .expect_err("bootstrap command that exits unsuccessfully should fail to load the sdk");

        // if the bootstrap command is set, and it returns true, it should still
        // error if the config file still isn't there
        fuchsia_env.fuchsia.project.bootstrap_command = Some(shell_cmd("true"));
        test_bootstrap_with(&test_env, &fuchsia_env).await.expect_err(
            "bootstrap command that doesn't set up the config file should fail to load the sdk",
        );

        // if the file now exists, it should be happy again!
        fuchsia_env.fuchsia.project.bootstrap_command = Some(shell_cmd("touch config.json"));
        test_bootstrap_with(&test_env, &fuchsia_env)
            .await
            .expect("bootstrap command that succeeds and lays down the file should find the sdk");

        // but if we switch it to looking for a relative path, it should be unhappy again
        fuchsia_env.fuchsia.project.build_config_path =
            Some(ConfigPath::path_ref("config.json.file"));
        test_bootstrap_with(&test_env, &fuchsia_env).await.expect_err(
            "bootstrap command that doesn't set up the path ref file should fail to load the sdk",
        );

        // and then if we make that point at the right file, it should be good again.
        fuchsia_env.fuchsia.project.bootstrap_command =
            Some(shell_cmd("echo config.json > config.json.file"));
        test_bootstrap_with(&test_env, &fuchsia_env).await.expect(
            "bootstrap command that succeeds and lays down the file ref file should find the sdk",
        );

        // now with it set to a configuration file that's in the build root that doesn't exist, it should fail
        fuchsia_env.fuchsia.project.build_config_path =
            Some(ConfigPath::out_dir_ref("config.json"));
        test_bootstrap_with(&test_env, &fuchsia_env)
            .await
            .expect_err("bootstrap command that doesn't set up the output directory should fail");

        // now set up an output directory and it should fail until the config file exists there
        fuchsia_env.fuchsia.project.build_out_dir = Some(ConfigPath::relative("out"));
        test_bootstrap_with(&test_env, &fuchsia_env)
            .await
            .expect_err("bootstrap command that doesn't set up the output directory should fail");

        // now set up an output directory and it should fail until the config file exists there
        fuchsia_env.fuchsia.project.bootstrap_command =
            Some(shell_cmd("mkdir -p out && touch out/config.json"));
        test_bootstrap_with(&test_env, &fuchsia_env)
            .await
            .expect("bootstrap command that does set up the output directory should succeed");

        // but not if it's a path_ref that doesn't exist
        fuchsia_env.fuchsia.project.build_out_dir = Some(ConfigPath::path_ref("out.path"));
        test_bootstrap_with(&test_env, &fuchsia_env)
            .await
            .expect_err("bootstrap command that doesn't set up the output directory should fail");

        // now set up an output directory and it should fail until the config file exists there
        fuchsia_env.fuchsia.project.bootstrap_command = Some(shell_cmd("echo out > out.path"));
        test_bootstrap_with(&test_env, &fuchsia_env).await.expect(
            "bootstrap command that does set up the output directory path_ref should succeed",
        );
    }

    #[fuchsia::test]
    async fn test_sdk_update() {
        let test_env = ffx_config::test_init().await.unwrap();
        let mut fuchsia_env = FuchsiaEnv::default();
        set_sdk_root_config(&test_env).await;

        println!("test root: {:?}", test_env.isolate_root.path());

        // start with the test file not existing and a command that returns false
        fuchsia_env.fuchsia.sdk.version_check_files = Some(vec!["test_file_1".into()]);
        fuchsia_env.fuchsia.sdk.version_check_command = Some(shell_cmd("false"));
        test_bootstrap_with(&test_env, &fuchsia_env)
            .await
            .expect_err("sdk check script should fail if it exits unsuccessfully");

        // then switch to a command that returns true
        fuchsia_env.fuchsia.sdk.version_check_files = Some(vec!["test_file_1".into()]);
        fuchsia_env.fuchsia.sdk.version_check_command = Some(shell_cmd("true"));
        test_bootstrap_with(&test_env, &fuchsia_env)
            .await
            .expect("sdk check script should succeed if the test command succeeds");

        // and now switch back to to a command that returns false to check
        // that it isn't run if the state of test_file_1 hasn't changed
        // note that the file not existing isn't a "problem", all that matters is that
        // its state hasn't changed.
        fuchsia_env.fuchsia.sdk.version_check_command = Some(shell_cmd("false"));
        test_bootstrap_with(&test_env, &fuchsia_env)
            .await
            .expect("sdk check script should succeed if the state hasn't changed");

        // now make that file exist and so its state has changed
        std::fs::File::create(test_env.isolate_root.path().join("test_file_1")).unwrap();
        test_bootstrap_with(&test_env, &fuchsia_env)
            .await
            .expect_err("sdk check script should fail if it exits unsuccessfully");

        // put the command back to one that succeeds so that we can 'bootstrap'
        // properly
        fuchsia_env.fuchsia.sdk.version_check_command = Some(shell_cmd("true"));
        test_bootstrap_with(&test_env, &fuchsia_env)
            .await
            .expect("sdk check script should succeed if the test command succeeds");

        // and again switch back to the 'false' command to make sure that it
        // doesn't re-run if the file doesn't change
        fuchsia_env.fuchsia.sdk.version_check_command = Some(shell_cmd("false"));
        test_bootstrap_with(&test_env, &fuchsia_env)
            .await
            .expect("sdk check script should succeed if the state hasn't changed");
    }
}
