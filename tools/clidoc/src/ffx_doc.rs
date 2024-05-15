// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{md_path, HEADER};
use anyhow::{bail, Context, Result};
use ffx_command::{CliArgsInfo, ErrorCodeInfo, FlagInfo, SubCommandInfo};
use std::{
    fs::File,
    io::{BufWriter, Write},
    path::PathBuf,
    process::Command,
};
use tracing::debug;

pub(crate) fn write_formatted_output_for_ffx(
    cmd_path: &PathBuf,
    output_path: &PathBuf,
    sdk_root_path: &Option<PathBuf>,
    sdk_manifest_path: &Option<PathBuf>,
    isolate_dir_path: &Option<PathBuf>,
) -> Result<()> {
    // Get name of command from full path to the command executable.
    let cmd_name = cmd_path.file_name().expect("Could not get file name for command");
    let output_md_path = md_path(&cmd_name, &output_path);
    debug!("Generating docs for {:?} to {:?}", cmd_path, output_md_path);

    let mut cmd = Command::new(&cmd_path);
    // ffx can't really run standalone in a hermetic environment, so we need to impute some
    // configuration.
    if let Some(sdk_root) = sdk_root_path {
        cmd.args(["--config", &format!("sdk.root={}", sdk_root.to_string_lossy())]);
    }
    if let Some(sdk_manifest) = sdk_manifest_path {
        cmd.args([
            "--config",
            &format!("sdk.module={}", sdk_manifest.file_name().unwrap().to_string_lossy()),
        ]);
    }
    if let Some(isolate_dir) = isolate_dir_path {
        cmd.args(["--isolate-dir", &isolate_dir.to_string_lossy()]);
    }
    let output = cmd
        .args(["--machine", "json-pretty", "--help"])
        .output()
        .context(format!("Command failed for {cmd_path:?}"))
        .expect("get output");

    if !output.status.success() {
        bail!(
            "{cmd_path:?} failed {}: {}",
            output.status.to_string(),
            String::from_utf8(output.stderr)?
        );
    }
    let value: CliArgsInfo = serde_json::from_slice(&output.stdout)
        .or_else(|e| -> Result<CliArgsInfo> {
            panic!(
                "{e}\n<start>{}\n<end>\n<starterr>\n{}\n<enderr>\n",
                String::from_utf8(output.stdout)?,
                String::from_utf8(output.stderr)?
            )
        })
        .expect("json");

    // Create a buffer writer to format and write consecutive lines to a file.
    let file = File::create(&output_md_path).context(format!("create {:?}", output_md_path))?;
    let output_writer = &mut BufWriter::new(file);

    writeln!(output_writer, "{}", HEADER)?;
    write_command(output_writer, 1, "", &value)?;
    Ok(())
}

fn write_command(
    output_writer: &mut BufWriter<File>,
    level: usize,
    parent_cmd: &str,
    command: &CliArgsInfo,
) -> Result<()> {
    let code_block_start =
        r#"```none {: style="white-space: break-spaces;" .devsite-disable-click-to-copy}"#;
    let mut sorted_commands = command.commands.clone();
    sorted_commands.sort_by(|a, b| a.name.cmp(&b.name));
    let heading_level = "#".repeat(level);
    writeln!(output_writer, "{heading_level} {}\n", command.name.to_lowercase())?;
    writeln!(output_writer, "{}\n", escape_text(&command.description))?;
    writeln!(output_writer, "{code_block_start}\n")?;
    writeln!(output_writer, "Usage: {}\n", build_usage_string(parent_cmd, &command)?)?;
    writeln!(output_writer, "```\n")?;

    write_flags(output_writer, &command.flags)?;
    write_subcommand_list(output_writer, &sorted_commands)?;
    write_examples(output_writer, &command.examples)?;
    write_notes(output_writer, &command.notes)?;
    write_errors(output_writer, &command.error_codes)?;

    for cmd in &sorted_commands {
        write_command(
            output_writer,
            level + 1,
            &format!("{parent_cmd} {}", command.name.to_lowercase()),
            &cmd.command,
        )?;
    }
    Ok(())
}

fn build_usage_string(parent_cmd: &str, value: &CliArgsInfo) -> Result<String> {
    let mut buf = Vec::<u8>::new();
    if !parent_cmd.is_empty() {
        write!(buf, "{parent_cmd} ")?;
    }
    write!(buf, "{}", value.name.to_lowercase())?;
    for flag in &value.flags {
        if flag.hidden || flag.long == "--help" {
            continue;
        }
        let flag_name = if let Some(short_flag) = flag.short {
            format!("-{short_flag}")
        } else {
            format!("{}", flag.long)
        };
        match &flag.kind {
            ffx_command::FlagKind::Option { arg_name } => match flag.optionality {
                ffx_command::Optionality::Greedy | ffx_command::Optionality::Required => {
                    write!(buf, " {flag_name} <{arg_name}>")?
                }
                ffx_command::Optionality::Optional => write!(buf, " [{flag_name} <{arg_name}>]")?,
                ffx_command::Optionality::Repeating => {
                    write!(buf, " [{flag_name} <{arg_name}...>]")?
                }
            },
            ffx_command::FlagKind::Switch => match flag.optionality {
                ffx_command::Optionality::Greedy | ffx_command::Optionality::Required => {
                    write!(buf, " {flag_name}")?
                }
                ffx_command::Optionality::Optional => write!(buf, " [{flag_name}]")?,
                ffx_command::Optionality::Repeating => write!(buf, " [{flag_name}...]")?,
            },
        }
    }
    if value.commands.is_empty() {
        for pos in &value.positionals {
            if pos.hidden {
                continue;
            }
            match pos.optionality {
                ffx_command::Optionality::Greedy | ffx_command::Optionality::Required => {
                    write!(buf, " {}", pos.name)?
                }
                ffx_command::Optionality::Optional => write!(buf, " [{}]", pos.name)?,
                ffx_command::Optionality::Repeating => write!(buf, " [{}...]", pos.name)?,
            }
        }
    } else {
        write!(buf, " [subcommand...]")?;
    }

    Ok(String::from_utf8(buf)?)
}

fn write_flags<W: Write>(output_writer: &mut BufWriter<W>, flags: &Vec<FlagInfo>) -> Result<()> {
    if flags.is_empty() {
        writeln!(output_writer, "\n")?;
        return Ok(());
    }
    writeln!(output_writer, "__Options__ | &nbsp;")?;
    writeln!(output_writer, "----------- | ------")?;

    for flag in flags {
        if flag.hidden {
            continue;
        }
        if let Some(s) = flag.short {
            writeln!(
                output_writer,
                "| <nobr>-{s}, {}</nobr> | {}",
                flag.long,
                escape_text(&flag.description)
            )?;
        } else {
            writeln!(
                output_writer,
                "| <nobr>{}</nobr> | {}",
                flag.long,
                escape_text(&flag.description)
            )?;
        }
    }
    writeln!(output_writer, "\n")?;
    Ok(())
}

fn write_examples(output_writer: &mut BufWriter<File>, examples: &Vec<String>) -> Result<()> {
    if examples.is_empty() {
        writeln!(output_writer, "\n")?;
        return Ok(());
    }
    writeln!(output_writer, "__Examples__\n")?;
    for ex in examples {
        writeln!(output_writer, "\n<pre>{}</pre>\n", escape_text(ex))?;
    }
    writeln!(output_writer, "\n")?;
    Ok(())
}

fn write_notes(output_writer: &mut BufWriter<File>, notes: &Vec<String>) -> Result<()> {
    if notes.is_empty() {
        writeln!(output_writer, "\n")?;
        return Ok(());
    }
    writeln!(output_writer, "__Notes__\n")?;
    for note in notes {
        writeln!(output_writer, "* <pre>{}</pre>", escape_text(note))?;
    }
    writeln!(output_writer, "\n")?;
    Ok(())
}

fn write_errors(output_writer: &mut BufWriter<File>, errors: &Vec<ErrorCodeInfo>) -> Result<()> {
    if errors.is_empty() {
        writeln!(output_writer, "\n")?;
        return Ok(());
    }
    writeln!(output_writer, "__Errors__ | &nbsp;")?;
    writeln!(output_writer, "----------- | ------")?;

    for e in errors {
        writeln!(output_writer, "| {} | {}", e.code, escape_text(&e.description))?;
    }
    writeln!(output_writer, "\n")?;
    Ok(())
}

fn write_subcommand_list(
    output_writer: &mut BufWriter<File>,
    commands: &Vec<SubCommandInfo>,
) -> Result<()> {
    if commands.is_empty() {
        writeln!(output_writer, "\n")?;
        return Ok(());
    }
    writeln!(output_writer, "__Subcommands__ | &nbsp;")?;
    writeln!(output_writer, "----------- | ------")?;

    for cmd in commands {
        writeln!(
            output_writer,
            "| [{}](#{}) | {}",
            cmd.name,
            cmd.name,
            escape_text(&cmd.command.description)
        )?;
    }
    writeln!(output_writer, "\n")?;
    Ok(())
}

fn escape_text(raw: &str) -> String {
    raw.replace("{", "&#123;").replace("}", "&#125;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use ffx_command::{Optionality, PositionalInfo};

    #[test]
    fn test_write_subcommand_list() {}
    #[test]
    fn test_write_errors() {}

    #[test]
    fn test_write_notes() {}

    #[test]
    fn test_write_examples() {}

    #[test]
    fn test_write_flags() {
        let test_data = [
            (vec![], "\n\n"),
            (
                vec![
                    FlagInfo {
                        kind: ffx_command::FlagKind::Switch,
                        optionality: Optionality::Optional,
                        long: "--help".into(),
                        short: None,
                        description: "help".into(),
                        hidden: false,
                    },
                    FlagInfo {
                        kind: ffx_command::FlagKind::Switch,
                        optionality: Optionality::Optional,
                        long: "--hidden".into(),
                        short: None,
                        description: "hidden".into(),
                        hidden: true,
                    },
                ],
                r#"__Options__ | &nbsp;
----------- | ------
| <nobr>--help</nobr> | help


"#,
            ),
            (
                vec![
                    FlagInfo {
                        kind: ffx_command::FlagKind::Switch,
                        optionality: Optionality::Optional,
                        long: "--something".into(),
                        short: None,
                        description: "something".into(),
                        hidden: false,
                    },
                    FlagInfo {
                        kind: ffx_command::FlagKind::Switch,
                        optionality: Optionality::Repeating,
                        long: "--something-really-long".into(),
                        short: Some('s'),
                        description: "something".into(),
                        hidden: false,
                    },
                ],
                r#"__Options__ | &nbsp;
----------- | ------
| <nobr>--something</nobr> | something
| <nobr>-s, --something-really-long</nobr> | something


"#,
            ),
        ];

        for (flags, expected) in test_data {
            let buf = Vec::<u8>::new();
            let mut writer = BufWriter::new(buf);
            write_flags(&mut writer, &flags).expect("write_flags");
            writer.flush().expect("flush buffer");
            let actual = String::from_utf8(writer.get_ref().to_vec()).expect("string from utf8");
            assert_eq!(actual, expected);
        }
    }

    #[test]
    fn test_build_usage_string_top() {
        let test_data = [
            (
                "",                     //parent_cmd
                CliArgsInfo::default(), //value
                String::from(""),       //expected
            ),
            (
                "", //parent_cmd
                CliArgsInfo {
                    name: "cmd1".into(),
                    description: "the cmd, 1".into(),
                    flags: vec![],
                    commands: vec![],
                    positionals: vec![],
                    ..Default::default()
                },
                String::from("cmd1"), //expected
            ),
            (
                "", //parent_cmd
                CliArgsInfo {
                    name: "cmd1".into(),
                    description: "the cmd, 1".into(),
                    flags: vec![
                        FlagInfo {
                            kind: ffx_command::FlagKind::Switch,
                            optionality: Optionality::Optional,
                            long: "--help".into(),
                            short: None,
                            description: "help".into(),
                            hidden: false,
                        },
                        FlagInfo {
                            kind: ffx_command::FlagKind::Switch,
                            optionality: Optionality::Optional,
                            long: "--hidden".into(),
                            short: None,
                            description: "hidden".into(),
                            hidden: true,
                        },
                    ],
                    commands: vec![],
                    positionals: vec![],
                    ..Default::default()
                },
                String::from("cmd1"), //expected
            ),
            (
                "parent", //parent_cmd
                CliArgsInfo {
                    name: "cmd2".into(),
                    description: "the cmd, 2".into(),
                    flags: vec![
                        FlagInfo {
                            kind: ffx_command::FlagKind::Switch,
                            optionality: Optionality::Optional,
                            long: "--help".into(),
                            short: None,
                            description: "help".into(),
                            hidden: false,
                        },
                        FlagInfo {
                            kind: ffx_command::FlagKind::Switch,
                            optionality: Optionality::Optional,
                            long: "--hidden".into(),
                            short: None,
                            description: "hidden".into(),
                            hidden: true,
                        },
                        FlagInfo {
                            kind: ffx_command::FlagKind::Switch,
                            optionality: Optionality::Optional,
                            long: "--something".into(),
                            short: None,
                            description: "something".into(),
                            hidden: false,
                        },
                        FlagInfo {
                            kind: ffx_command::FlagKind::Switch,
                            optionality: Optionality::Repeating,
                            long: "--something-really-long".into(),
                            short: Some('s'),
                            description: "something".into(),
                            hidden: false,
                        },
                    ],
                    commands: vec![],
                    positionals: vec![],
                    ..Default::default()
                },
                String::from("parent cmd2 [--something] [-s...]"), //expected
            ),
            (
                "parent", //parent_cmd
                CliArgsInfo {
                    name: "cmd3".into(),
                    description: "the cmd, 2".into(),
                    flags: vec![
                        FlagInfo {
                            kind: ffx_command::FlagKind::Switch,
                            optionality: Optionality::Required,
                            long: "--path".into(),
                            short: Some('p'),
                            description: "path".into(),
                            hidden: false,
                        },
                        FlagInfo {
                            kind: ffx_command::FlagKind::Switch,
                            optionality: Optionality::Optional,
                            long: "--hidden".into(),
                            short: None,
                            description: "hidden".into(),
                            hidden: true,
                        },
                        FlagInfo {
                            kind: ffx_command::FlagKind::Option { arg_name: "the_arg".into() },
                            optionality: Optionality::Optional,
                            long: "--something".into(),
                            short: None,
                            description: "something".into(),
                            hidden: false,
                        },
                        FlagInfo {
                            kind: ffx_command::FlagKind::Option { arg_name: "s_val".into() },
                            optionality: Optionality::Repeating,
                            long: "--something-really-long".into(),
                            short: Some('s'),
                            description: "something".into(),
                            hidden: false,
                        },
                    ],
                    commands: vec![],
                    positionals: vec![],
                    ..Default::default()
                },
                String::from("parent cmd3 -p [--something <the_arg>] [-s <s_val...>]"), //expected
            ),
            (
                "parent", //parent_cmd
                CliArgsInfo {
                    name: "cmd3".into(),
                    description: "the cmd, 2".into(),
                    flags: vec![
                        FlagInfo {
                            kind: ffx_command::FlagKind::Switch,
                            optionality: Optionality::Required,
                            long: "--path".into(),
                            short: Some('p'),
                            description: "path".into(),
                            hidden: false,
                        },
                        FlagInfo {
                            kind: ffx_command::FlagKind::Switch,
                            optionality: Optionality::Optional,
                            long: "--hidden".into(),
                            short: None,
                            description: "hidden".into(),
                            hidden: true,
                        },
                        FlagInfo {
                            kind: ffx_command::FlagKind::Option { arg_name: "the_arg".into() },
                            optionality: Optionality::Optional,
                            long: "--something".into(),
                            short: None,
                            description: "something".into(),
                            hidden: false,
                        },
                        FlagInfo {
                            kind: ffx_command::FlagKind::Option { arg_name: "s_val".into() },
                            optionality: Optionality::Repeating,
                            long: "--something-really-long".into(),
                            short: Some('s'),
                            description: "something".into(),
                            hidden: false,
                        },
                    ],
                    commands: vec![],
                    positionals: vec![PositionalInfo {
                        name: "pos1".into(),
                        description: "1".into(),
                        optionality: Optionality::Required,
                        hidden: false,
                    }],
                    ..Default::default()
                },
                String::from("parent cmd3 -p [--something <the_arg>] [-s <s_val...>] pos1"), //expected
            ),
            (
                "parent", //parent_cmd
                CliArgsInfo {
                    name: "cmd4".into(),
                    description: "the cmd, 4".into(),
                    flags: vec![
                        FlagInfo {
                            kind: ffx_command::FlagKind::Switch,
                            optionality: Optionality::Required,
                            long: "--path".into(),
                            short: Some('p'),
                            description: "path".into(),
                            hidden: false,
                        },
                        FlagInfo {
                            kind: ffx_command::FlagKind::Switch,
                            optionality: Optionality::Optional,
                            long: "--hidden".into(),
                            short: None,
                            description: "hidden".into(),
                            hidden: true,
                        },
                        FlagInfo {
                            kind: ffx_command::FlagKind::Option { arg_name: "the_arg".into() },
                            optionality: Optionality::Optional,
                            long: "--something".into(),
                            short: None,
                            description: "something".into(),
                            hidden: false,
                        },
                        FlagInfo {
                            kind: ffx_command::FlagKind::Option { arg_name: "s_val".into() },
                            optionality: Optionality::Repeating,
                            long: "--something-really-long".into(),
                            short: Some('s'),
                            description: "something".into(),
                            hidden: false,
                        },
                    ],
                    commands: vec![],
                    positionals: vec![
                        PositionalInfo {
                            name: "pos1".into(),
                            description: "1".into(),
                            optionality: Optionality::Required,
                            hidden: false,
                        },
                        PositionalInfo {
                            name: "other_pos".into(),
                            description: "others".into(),
                            optionality: Optionality::Repeating,
                            hidden: false,
                        },
                    ],
                    ..Default::default()
                },
                String::from(
                    "parent cmd4 -p [--something <the_arg>] [-s <s_val...>] pos1 [other_pos...]",
                ), //expected
            ),
            (
                "parent", //parent_cmd
                CliArgsInfo {
                    name: "cmd5".into(),
                    description: "the cmd, 5".into(),
                    flags: vec![
                        FlagInfo {
                            kind: ffx_command::FlagKind::Switch,
                            optionality: Optionality::Required,
                            long: "--path".into(),
                            short: Some('p'),
                            description: "path".into(),
                            hidden: false,
                        },
                        FlagInfo {
                            kind: ffx_command::FlagKind::Switch,
                            optionality: Optionality::Optional,
                            long: "--hidden".into(),
                            short: None,
                            description: "hidden".into(),
                            hidden: true,
                        },
                        FlagInfo {
                            kind: ffx_command::FlagKind::Option { arg_name: "the_arg".into() },
                            optionality: Optionality::Optional,
                            long: "--something".into(),
                            short: None,
                            description: "something".into(),
                            hidden: false,
                        },
                        FlagInfo {
                            kind: ffx_command::FlagKind::Option { arg_name: "s_val".into() },
                            optionality: Optionality::Repeating,
                            long: "--something-really-long".into(),
                            short: Some('s'),
                            description: "something".into(),
                            hidden: false,
                        },
                    ],
                    commands: vec![SubCommandInfo {
                        name: "another_cmd".into(),
                        command: CliArgsInfo { name: "another_cmd".into(), ..Default::default() },
                    }],
                    positionals: vec![
                        PositionalInfo {
                            name: "pos1".into(),
                            description: "1".into(),
                            optionality: Optionality::Required,
                            hidden: false,
                        },
                        PositionalInfo {
                            name: "other_pos".into(),
                            description: "others".into(),
                            optionality: Optionality::Repeating,
                            hidden: false,
                        },
                    ],
                    ..Default::default()
                },
                String::from(
                    "parent cmd5 -p [--something <the_arg>] [-s <s_val...>] [subcommand...]",
                ), //expected
            ),
        ];

        for (parent_cmd, value, expected) in test_data {
            let actual = build_usage_string(parent_cmd, &value).expect("usage string");
            assert_eq!(actual, expected);
        }
    }
}
