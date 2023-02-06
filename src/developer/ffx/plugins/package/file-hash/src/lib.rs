// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{Context, Result};
use ffx_core::ffx_plugin;
use ffx_package_file_hash_args::FileHashCommand;
use ffx_writer::Writer;
use fuchsia_merkle::MerkleTree;
use rayon::prelude::*;
use serde::Serialize;
use std::fs::File;
use std::io::prelude::*;
use std::path::PathBuf;

#[ffx_plugin("ffx_package")]
pub async fn cmd_file_hash(
    cmd: FileHashCommand,
    #[ffx(machine = Vec<FileHashEntry>)] mut writer: Writer,
) -> Result<()> {
    if cmd.paths.is_empty() {
        if writer.is_machine() {
            writer.machine::<Vec<FileHashEntry>>(&vec![])?;
        } else {
            writer.error("Missing file path")?;
        }
        return Ok(());
    }

    let entries = cmd.paths.into_par_iter().map(FileHashEntry::new);

    if writer.is_machine() {
        // Fails if any of the files don't exist.
        let entries: Vec<_> = entries.collect::<Result<_>>()?;
        writer.machine(&entries)?;
    } else {
        // Only fails if writing to stdout/stderr fails.
        for entry in entries.collect::<Vec<_>>() {
            match entry {
                Ok(FileHashEntry { path, hash }) => {
                    writeln!(writer, "{}  {}", hash, path.display())?;
                }
                Err(e) => {
                    // Display the error and continue.
                    writer.error(e)?;
                }
            }
        }
    }

    Ok(())
}

#[derive(Serialize)]
pub struct FileHashEntry {
    path: PathBuf,
    hash: String,
}

impl FileHashEntry {
    /// Compute the merkle root hash of a file.
    fn new(path: PathBuf) -> Result<Self> {
        let file = File::open(&path)
            .with_context(|| format!("failed to open file: {}", path.display()))?;

        let tree = MerkleTree::from_reader(file)
            .with_context(|| format!("failed to read file: {}", path.display()))?;

        Ok(Self { path, hash: tree.root().to_string() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ffx_writer::Format;
    use tempfile::TempDir;

    fn create_test_files(name_content_pairs: &[(&str, &str)]) -> Result<TempDir> {
        let dir = TempDir::new()?;

        for (name, content) in name_content_pairs {
            let path = dir.path().join(name);
            let mut file = File::create(path)?;
            write!(file, "{content}")?;
        }

        Ok(dir)
    }

    /// * Create a few temp files.
    /// * Pass those paths to the file-hash command.
    /// * Verify that the output is correct.
    #[fuchsia::test]
    async fn validate_output() -> Result<()> {
        let test_data = [
            ("first_file", ""),
            ("second_file", "Hello world!"),
            ("third_file", &"0123456789".repeat(1024)),
        ];

        let dir = create_test_files(&test_data)?;
        let paths = test_data.iter().map(|(name, _)| dir.path().join(name)).collect();

        let cmd = FileHashCommand { paths };
        let writer = Writer::new_test(None);
        cmd_file_hash(cmd, writer.clone()).await?;

        let expected_output = format!(
            "\
15ec7bf0b50732b49f8228e07d24365338f9e3ab994b00af08e5a3bffe55fd8b  {0}/first_file
e6a73dbd2d88e51ccdaa648cbf49b3939daac8a3e370169bc85e0324a41adbc2  {0}/second_file
f5a0dff4578d0150d3dace71b08733d5cd8cbe63a322633445c9ff0d9041b9c4  {0}/third_file
",
            dir.path().display(),
        );

        assert_eq!(writer.test_output()?, expected_output);
        assert_eq!(writer.test_error()?, "");

        // Clean up temp files.
        drop(dir);

        Ok(())
    }

    /// * Create a few temp files.
    /// * Pass those paths to the file-hash command,
    ///   producing **machine-readable** output.
    /// * Verify that the output is correct.
    #[fuchsia::test]
    async fn validate_output_machine_mode() -> Result<()> {
        let test_data = [
            ("first_file", ""),
            ("second_file", "Hello world!"),
            ("third_file", &"0123456789".repeat(1024)),
        ];

        let dir = create_test_files(&test_data)?;
        let paths = test_data.iter().map(|(name, _)| dir.path().join(name)).collect();

        let cmd = FileHashCommand { paths };
        let writer = Writer::new_test(Some(Format::Json));
        cmd_file_hash(cmd, writer.clone()).await?;

        let expected_output = format!(
            concat!(
                "[",
                r#"{{"path":"{0}/first_file","hash":"15ec7bf0b50732b49f8228e07d24365338f9e3ab994b00af08e5a3bffe55fd8b"}},"#,
                r#"{{"path":"{0}/second_file","hash":"e6a73dbd2d88e51ccdaa648cbf49b3939daac8a3e370169bc85e0324a41adbc2"}},"#,
                r#"{{"path":"{0}/third_file","hash":"f5a0dff4578d0150d3dace71b08733d5cd8cbe63a322633445c9ff0d9041b9c4"}}"#,
                "]\n",
            ),
            dir.path().display(),
        );

        assert_eq!(writer.test_output()?, expected_output);
        assert_eq!(writer.test_error()?, "");

        // Clean up temp files.
        drop(dir);

        Ok(())
    }

    /// * Run the command on a file that doesn't exist.
    /// * Check for a specific error message.
    #[fuchsia::test]
    async fn file_not_found() -> Result<()> {
        const NAME: &str = "filename_that_does_not_exist";

        let cmd = FileHashCommand { paths: vec![NAME.into()] };
        let writer = Writer::new_test(None);
        cmd_file_hash(cmd, writer.clone()).await?;

        assert_eq!(writer.test_output()?, "");
        assert_eq!(writer.test_error()?, format!("failed to open file: {NAME}\n"));

        Ok(())
    }

    /// * Run the command **in machine mode** on a file that doesn't exist.
    /// * It should return an error, **instead of** printing to stderr.
    #[fuchsia::test]
    async fn file_not_found_machine_mode() -> Result<()> {
        const NAME: &str = "filename_that_does_not_exist";

        let cmd = FileHashCommand { paths: vec![NAME.into()] };
        let writer = Writer::new_test(Some(Format::Json));
        let result = cmd_file_hash(cmd, writer.clone()).await;

        let msg = result.unwrap_err().to_string();
        assert_eq!(msg, format!("failed to open file: {NAME}"));

        assert_eq!(writer.test_output()?, "");
        assert_eq!(writer.test_error()?, "");

        Ok(())
    }
}
