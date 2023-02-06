// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{Context, Result};
use ffx_core::ffx_plugin;
use ffx_package_archive_hash_args::HashCommand;
use ffx_package_archive_utils::get_merkleroot;
use ffx_writer::Writer;
use std::fs::File;

#[ffx_plugin("ffx_package")]
pub fn cmd_hash(cmd: HashCommand, mut writer: Writer) -> Result<()> {
    merkleroot_hash_implementation(cmd, &mut writer)
}

fn merkleroot_hash_implementation(cmd: HashCommand, writer: &mut Writer) -> Result<()> {
    let mut archive_file = File::open(cmd.archive)?;
    let merkleroot_string = get_merkleroot(&mut archive_file)?.root().to_string();
    if writer.is_machine() {
        let merkle_vec = vec![merkleroot_string];
        writer.machine(&merkle_vec).context("machine merkleroot")?;
    } else {
        writer.line(merkleroot_string)?;
    }
    Ok(())
}

#[cfg(test)]
mod test {
    use super::*;
    use fuchsia_merkle::MerkleTreeBuilder;
    use std::io::Write;
    use tempfile;

    #[test]
    fn test_merkleroot() -> Result<()> {
        let far_contents = b"hello world";
        let tempdir = tempfile::tempdir().unwrap();
        let file_path = tempdir.path().join("test.far");
        let mut far_buffer = File::create(file_path)?;
        far_buffer.write_all(far_contents)?;

        let mut builder = MerkleTreeBuilder::new();
        builder.write(&far_contents[..]);
        let expected = builder.finish();

        let mut writer = Writer::new_test(None);
        let far_path = tempdir.path().join("test.far");
        let cmd = HashCommand { archive: far_path };
        merkleroot_hash_implementation(cmd, &mut writer)?;
        let mut ffx_output = writer.test_output()?;
        ffx_output.pop();
        assert_eq!(ffx_output, expected.root().to_string());

        Ok(())
    }
}
