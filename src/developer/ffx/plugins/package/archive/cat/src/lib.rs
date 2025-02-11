// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use ffx_package_archive_cat_args::CatCommand;
use ffx_package_archive_utils::{read_file_entries, FarArchiveReader, FarListReader};
use ffx_writer::SimpleWriter;
use fho::{bug, return_user_error, FfxMain, FfxTool, Result};

#[derive(FfxTool)]
pub struct ArchiveCatTool {
    #[command]
    pub cmd: CatCommand,
}

fho::embedded_plugin!(ArchiveCatTool);

#[async_trait::async_trait(?Send)]
impl FfxMain for ArchiveCatTool {
    type Writer = SimpleWriter;
    async fn main(self, mut writer: <Self as fho::FfxMain>::Writer) -> fho::Result<()> {
        let mut archive_reader = FarArchiveReader::new(&self.cmd.archive)?;

        cat_implementation(self.cmd, &mut writer, &mut archive_reader)
    }
}

fn cat_implementation<W: std::io::Write>(
    cmd: CatCommand,
    writer: &mut W,
    reader: &mut dyn FarListReader,
) -> Result<()> {
    let file_name = cmd.far_path.to_string_lossy();

    let entries = read_file_entries(reader)?;
    if let Some(entry) = entries.iter().find(|x| x.name == file_name) {
        let data = reader.read_entry(entry)?;
        writer.write_all(&data).map_err(|e| bug!(e))?;
    } else {
        return_user_error!("file {} not found in {}", file_name, cmd.archive.to_string_lossy());
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ffx_package_archive_utils::test_utils::{
        create_mockreader, test_contents, LIB_RUN_SO_BLOB, LIB_RUN_SO_PATH,
    };
    use std::path::PathBuf;

    #[test]
    fn test_cat_filename() -> Result<()> {
        let cmd = CatCommand {
            archive: PathBuf::from("some.far"),
            far_path: PathBuf::from(LIB_RUN_SO_PATH),
        };

        let mut output: Vec<u8> = vec![];

        let expected = test_contents(LIB_RUN_SO_BLOB);

        cat_implementation(cmd, &mut output, &mut create_mockreader())?;
        assert_eq!(expected, output);

        Ok(())
    }
}
