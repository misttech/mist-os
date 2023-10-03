// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Result;
use argh::FromArgs;
use ffx_command::CLIArgsInfo;
use ffx_config::{SdkRoot, TestEnv};
use ffx_isolate::{Isolate, SearchContext};
use serde::Serialize;
use std::{fs, path::PathBuf};
use tempfile::TempDir;

#[derive(FromArgs)]
/// CLI tool for generating golden JSON files for
/// ffx commands.
struct Args {
    #[argh(option)]
    /// the output directory
    pub out_dir: String,
    #[argh(option)]
    /// base dir of the golden files in source. This is used to
    /// create dependencies for the goldens
    pub base_golden_src_dir: String,
    #[argh(option)]
    /// generate the comparisons file for the goldens
    pub gen_comparisons: String,
    #[argh(option)]
    /// path to SDK root
    pub sdk_root: PathBuf,
}

#[derive(Serialize)]
struct Comparison {
    pub candidate: String,
    pub golden: String,
}

#[cfg(target_arch = "x86_64")]
fn get_tools_relpath() -> String {
    String::from("tools/x64")
}

#[cfg(target_arch = "aarch64")]
fn get_tools_relpath() -> String {
    String::from("tools/arm64")
}

/// Creates a new Isolate for ffx and a temp directory for holding
/// test artifacts, such as ssh keys.
/// These objects need to exist for the duration of the test,
/// since the underlying data is cleaned up when they are dropped.
pub(crate) async fn new_ffx_isolate(
    name: &str,
    sdk_root_dir: PathBuf,
) -> Result<(Isolate, TempDir, TestEnv)> {
    let temp_dir = tempfile::TempDir::new()?;
    assert!(temp_dir.path().exists());
    let test_env = ffx_config::test_init().await?;

    let subtool_search_paths = Vec::<PathBuf>::new();
    //     ffx_config::query("ffx.subtool-search-paths").get().await.unwrap_or_default();
    let ffx_path = sdk_root_dir.join(get_tools_relpath()).join("ffx");
    println!("Using ffx command from {ffx_path:?}");

    let ffx_isolate = Isolate::new_with_search(
        name,
        SearchContext::Runtime {
            ffx_path: ffx_path,
            sdk_root: Some(SdkRoot::Full(sdk_root_dir)),
            subtool_search_paths,
        },
        PathBuf::from("."),
        &test_env.context,
    )
    .await?;

    Ok((ffx_isolate, temp_dir, test_env))
}

#[fuchsia::main]
async fn main() -> Result<()> {
    let args = argh::from_env::<Args>();

    let (ffx_isolate, _temp_dir, _test_env) = new_ffx_isolate("cli-goldens", args.sdk_root).await?;

    let output = ffx_isolate.ffx(&["--help-json"]).await?;

    let value: CLIArgsInfo = serde_json::from_str(&output.stdout)
        .or_else(|e| -> Result<CLIArgsInfo> { panic!("{e} {}", output.stdout) })
        .expect("json");

    let cmd_name = "ffx";

    let golden_root = PathBuf::from(args.out_dir);
    let golden_files = write_goldens(&value, cmd_name, &golden_root)?;

    let comparisons: Vec<Comparison> = golden_files
        .iter()
        .map(|p| {
            let golden = PathBuf::from(&args.base_golden_src_dir)
                .join(p.strip_prefix(&golden_root).expect("relative golden path"));
            Comparison {
                golden: golden.to_string_lossy().into(),
                candidate: p.to_string_lossy().into(),
            }
        })
        .collect();

    fs::write(&args.gen_comparisons, serde_json::to_string_pretty(&comparisons)?)?;

    Ok(())
}

fn write_goldens(value: &CLIArgsInfo, cmd: &str, out_dir: &PathBuf) -> Result<Vec<PathBuf>> {
    let mut files_written: Vec<PathBuf> = vec![];
    let file_name = out_dir.join(format!("{cmd}.golden"));
    files_written.push(file_name.clone());
    // write out everything except the sub commands, which are broken out into separate files.
    let golden_data = CLIArgsInfo {
        name: value.name.clone(),
        description: value.description.clone(),
        examples: value.examples.clone(),
        flags: value.flags.clone(),
        notes: value.notes.clone(),
        commands: vec![],
        positionals: value.positionals.clone(),
        error_codes: value.error_codes.clone(),
    };

    fs::write(&file_name, serde_json::to_string_pretty(&golden_data)?)?;

    if value.commands.len() > 0 {
        let subcmd_path = out_dir.join(cmd);
        std::fs::create_dir_all(&subcmd_path)?;
        // Now recurse on subcommands
        for subcmd in &value.commands {
            files_written.extend(write_goldens(&subcmd.command, &subcmd.name, &subcmd_path)?);
        }
    }
    Ok(files_written)
}
