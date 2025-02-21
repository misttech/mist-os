// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Result;
use argh::FromArgs;
use ffx_command::CliArgsInfo;
use ffx_config::environment::ExecutableKind;
use ffx_config::{EnvironmentContext, SdkRoot};
use ffx_isolate::{Isolate, SearchContext};
use serde::Serialize;
use std::fs;
use std::io::Write;
use std::path::PathBuf;

#[derive(FromArgs)]
/// CLI tool for generating golden JSON files for
/// ffx commands.
struct Args {
    #[argh(switch)]
    /// only generate the metadata, do not write golden files.
    pub describe_only: bool,
    #[argh(switch)]
    /// write out command list file and exit.
    pub commandlist_only: bool,
    #[argh(option)]
    /// the output directory
    pub out_dir: Option<PathBuf>,
    #[argh(option)]
    /// base dir of the golden files in source. This is used to
    /// create dependencies for the goldens
    pub base_golden_src_dir: Option<String>,
    #[argh(option)]
    /// generate the comparisons file for the goldens
    pub gen_comparisons: Option<PathBuf>,
    #[argh(option)]
    /// path to SDK root
    pub sdk_root: PathBuf,
    #[argh(option)]
    /// path to write out the list of golden files.
    pub golden_file_list: Option<PathBuf>,
    #[argh(option)]
    /// path to write out the list of top level commands.
    pub command_list: Option<PathBuf>,
    #[argh(option)]
    /// top level command to use to filter output
    pub filter_command: Option<String>,
}

#[derive(Eq, PartialEq, Serialize, Ord, PartialOrd)]
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
pub(crate) async fn new_ffx_isolate(name: &str, sdk_root_dir: PathBuf) -> Result<Isolate> {
    let context =
        EnvironmentContext::no_context(ExecutableKind::Test, Default::default(), None, false);

    let subtool_search_paths = Vec::<PathBuf>::new();
    let ffx_path = sdk_root_dir.join(get_tools_relpath()).join("ffx");

    let ffx_isolate = Isolate::new_with_search(
        name,
        SearchContext::Runtime {
            ffx_path: ffx_path,
            sdk_root: Some(SdkRoot::Full(sdk_root_dir)),
            subtool_search_paths,
        },
        PathBuf::from("."),
        &context,
    )
    .await?;

    Ok(ffx_isolate)
}

// Note: setting logging to "info" will print superfluous information to stdout.
#[fuchsia::main(logging_minimum_severity = "warn")]
async fn main() -> Result<()> {
    let args = argh::from_env::<Args>();

    let ffx_isolate = new_ffx_isolate("cli-goldens", args.sdk_root).await?;

    let output = ffx_isolate.ffx(&["--machine", "json-pretty", "--help"]).await?;

    let value: CliArgsInfo = serde_json::from_str(&output.stdout)
        .or_else(|e| -> Result<CliArgsInfo> {
            panic!(
                "{e}\n<start>{}\n<end>\n<starterr>\n{}\n<enderr>\n",
                output.stdout, output.stderr
            )
        })
        .expect("json");

    if args.commandlist_only {
        let mut out = fs::File::create(args.command_list.expect("command list path"))?;
        let mut commands: Vec<String> = value.commands.iter().map(|c| c.name.clone()).collect();
        commands.sort();
        writeln!(&mut out, "{}", commands.join("\n"))?;
        return Ok(());
    }

    let cmd_name = "ffx";

    if let Some(golden_root) = args.out_dir {
        let golden_files = generate_goldens(
            &value,
            cmd_name,
            &golden_root,
            args.filter_command,
            !args.describe_only,
        )?;

        if let Some(golden_src_dir) = &args.base_golden_src_dir {
            let mut comparisons: Vec<Comparison> = golden_files
                .iter()
                .map(|p| {
                    let golden = PathBuf::from(golden_src_dir)
                        .join(p.strip_prefix(&golden_root).expect("relative golden path"));
                    Comparison {
                        golden: golden.to_string_lossy().into(),
                        candidate: p.to_string_lossy().into(),
                    }
                })
                .collect();

            comparisons.sort();

            if let Some(comparision_file) = &args.gen_comparisons {
                fs::write(comparision_file, serde_json::to_string_pretty(&comparisons)?)?;
            }
            if let Some(golden_file_list) = &args.golden_file_list {
                let mut out = fs::File::create(golden_file_list)?;
                let prefix = format!("{golden_src_dir}/");
                for c in comparisons {
                    let f = c.golden.strip_prefix(&prefix).unwrap_or(&c.golden);
                    writeln!(&mut out, "{f}")?;
                }
            }
        }
    }

    Ok(())
}

fn generate_goldens(
    value: &CliArgsInfo,
    cmd: &str,
    out_dir: &PathBuf,
    filter_command: Option<String>,
    save_goldens: bool,
) -> Result<Vec<PathBuf>> {
    let mut files_written: Vec<PathBuf> = vec![];
    let file_name = out_dir.join(format!("{cmd}.golden"));

    // write out everything except the sub commands, which are broken out into separate files.
    let golden_data = CliArgsInfo {
        name: value.name.clone(),
        description: value.description.clone(),
        examples: value.examples.clone(),
        flags: value.flags.clone(),
        notes: value.notes.clone(),
        commands: vec![],
        positionals: value.positionals.clone(),
        error_codes: value.error_codes.clone(),
    };

    if filter_command.is_none() || Some(cmd) == filter_command.as_deref() {
        files_written.push(file_name.clone());
        if save_goldens {
            std::fs::create_dir_all(&out_dir)?;
            fs::write(&file_name, serde_json::to_string_pretty(&golden_data)?)?;
        }
    }

    if value.commands.len() > 0 {
        let subcmd_path = out_dir.join(cmd);
        if let Some(subcmd) = value.commands.iter().find(|c| Some(c.name.clone()) == filter_command)
        {
            if save_goldens {
                std::fs::create_dir_all(&subcmd_path)?;
            }
            files_written.extend(generate_goldens(
                &subcmd.command,
                &subcmd.name,
                &subcmd_path,
                None,
                save_goldens,
            )?);
        } else if filter_command.is_none() {
            if save_goldens {
                std::fs::create_dir_all(&subcmd_path)?;
            }

            // Now recurse on subcommands
            for subcmd in &value.commands {
                files_written.extend(generate_goldens(
                    &subcmd.command,
                    &subcmd.name,
                    &subcmd_path,
                    None,
                    save_goldens,
                )?);
            }
        }
    }

    Ok(files_written)
}
