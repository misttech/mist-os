// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{Context, Result};

use ffx_package_far_list_args::ListCommand;
use fho::{FfxMain, FfxTool, MachineWriter, ToolIO as _};
use fuchsia_archive as far;
use humansize::{file_size_opts, FileSize};
use prettytable::{cell, row, Table};
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::Write;

#[derive(Serialize, Deserialize)]
pub struct FarEntry {
    path: String,
    offset: u64,
    length: u64,
}

impl<'a> From<far::Entry<'a>> for FarEntry {
    fn from(entry: far::Entry<'a>) -> FarEntry {
        FarEntry {
            path: String::from_utf8_lossy(entry.path()).to_string(),
            offset: entry.offset(),
            length: entry.length(),
        }
    }
}

#[derive(FfxTool)]
pub struct FarListTool {
    #[command]
    pub cmd: ListCommand,
}

fho::embedded_plugin!(FarListTool);

#[async_trait::async_trait(?Send)]
impl FfxMain for FarListTool {
    type Writer = MachineWriter<Vec<FarEntry>>;
    async fn main(self, writer: <Self as fho::FfxMain>::Writer) -> fho::Result<()> {
        cmd_list(self.cmd, writer).await.map_err(Into::into)
    }
}

pub async fn cmd_list(
    cmd: ListCommand,
    mut writer: <FarListTool as FfxMain>::Writer,
) -> Result<()> {
    let file = File::open(&cmd.far_file)
        .with_context(|| format!("failed to open file: {}", cmd.far_file.display()))?;
    let reader = far::Reader::new(file)
        .with_context(|| format!("failed to parse FAR file: {}", cmd.far_file.display()))?;

    let mut entries: Vec<FarEntry> = reader.list().into_iter().map(|e| e.into()).collect();
    entries.sort_by(|e1, e2| e1.path.cmp(&e2.path));

    if writer.is_machine() {
        return writer.machine(&entries).map_err(Into::into);
    }

    if entries.is_empty() {
        writeln!(writer, "FAR file contains no entries.")?;
    } else {
        write!(writer, "{}", format_table(&entries, cmd.long_format))?;
    }

    Ok(())
}

fn format_table(entries: &[FarEntry], display_lengths: bool) -> Table {
    let mut table = Table::new();

    if display_lengths {
        table.set_titles(row!["path", "length"]);

        for entry in entries {
            let path = &entry.path;
            let length = entry
                .length
                .file_size(file_size_opts::CONVENTIONAL)
                .expect("length is non-negative");

            table.add_row(row![path, length]);
        }
    } else {
        table.set_titles(row!["path"]);

        for entry in entries {
            table.add_row(row![entry.path]);
        }
    }

    table
}

#[cfg(test)]
mod tests {
    use super::*;
    use fho::{Format, TestBuffers};
    use std::collections::BTreeMap;
    use std::io::Read;
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn create_test_far(tmp_dir: &TempDir, file_names: &[&str]) -> Result<PathBuf> {
        let mut path_content_map: BTreeMap<&str, (u64, Box<dyn Read>)> = BTreeMap::new();
        for file_name in file_names.iter() {
            path_content_map.insert(
                file_name,
                (file_name.len().try_into().unwrap(), Box::new((*file_name).as_bytes())),
            );
        }
        let mut far_contents = Vec::new();
        fuchsia_archive::write(&mut far_contents, path_content_map)?;
        let far_path = tmp_dir.path().join("test.far");
        let mut tmp_file = File::create(&far_path)?;
        tmp_file.write_all(&far_contents)?;
        Ok(far_path)
    }

    #[fuchsia::test]
    async fn normal_output() -> Result<()> {
        let tmp_dir = TempDir::new().unwrap();
        let far_path = create_test_far(&tmp_dir, &["foo", "bar", "baz"]).unwrap();
        let cmd = ListCommand { far_file: far_path, long_format: false };
        let buffers = TestBuffers::default();
        let writer = <FarListTool as FfxMain>::Writer::new_test(None, &buffers);
        cmd_list(cmd, writer).await?;
        let (stdout, stderr) = buffers.into_strings();
        assert_eq!(stdout, "+------+\n| path |\n+======+\n| bar  |\n+------+\n| baz  |\n+------+\n| foo  |\n+------+\n");
        assert_eq!(stderr, "");
        Ok(())
    }

    #[fuchsia::test]
    async fn long_output() -> Result<()> {
        let tmp_dir = TempDir::new().unwrap();
        let far_path = create_test_far(&tmp_dir, &["alpha", "beta", "gamma"]).unwrap();
        let cmd = ListCommand { far_file: far_path, long_format: true };
        let buffers = TestBuffers::default();
        let writer = <FarListTool as FfxMain>::Writer::new_test(None, &buffers);
        cmd_list(cmd, writer).await?;
        let (stdout, stderr) = buffers.into_strings();
        assert_eq!(stdout, "+-------+--------+\n| path  | length |\n+=======+========+\n| alpha | 5 B    |\n+-------+--------+\n| beta  | 4 B    |\n+-------+--------+\n| gamma | 5 B    |\n+-------+--------+\n");
        assert_eq!(stderr, "");
        Ok(())
    }

    #[fuchsia::test]
    async fn machine_output() -> Result<()> {
        let tmp_dir = TempDir::new().unwrap();
        let far_path = create_test_far(&tmp_dir, &["one", "two", "three"]).unwrap();
        let cmd = ListCommand { far_file: far_path, long_format: false };
        let buffers = TestBuffers::default();
        let writer = <FarListTool as FfxMain>::Writer::new_test(Some(Format::Json), &buffers);
        cmd_list(cmd, writer).await?;
        let (stdout, stderr) = buffers.into_strings();
        assert_eq!(stdout, "[{\"path\":\"one\",\"offset\":4096,\"length\":3},{\"path\":\"three\",\"offset\":8192,\"length\":5},{\"path\":\"two\",\"offset\":12288,\"length\":3}]\n");
        assert_eq!(stderr, "");
        Ok(())
    }
}
