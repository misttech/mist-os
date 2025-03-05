// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{BoardArgs, HybridBoardArgs};

use anyhow::Result;
use assembly_config_schema::BoardInformation;
use assembly_container::{AssemblyContainer, DirectoryPathBuf};

pub fn new(args: &BoardArgs) -> Result<()> {
    let mut config = BoardInformation::from_config_path(&args.config)?;
    for (i, board_input_bundle) in args.board_input_bundles.iter().enumerate() {
        let key = format!("tmp{}", i);
        let directory = DirectoryPathBuf(board_input_bundle.clone());
        config.input_bundles.insert(key, directory);
    }

    // Build systems do not know the name of the BIBs, so they serialize index
    // numbers in place of BIB names by default. We add the BIB names in now,
    // so all the rest of the rules can assume the config has proper BIB names.
    let config = config.add_bib_names()?;
    config.write_to_dir(&args.output, args.depfile.as_ref())?;
    Ok(())
}

pub fn hybrid(args: &HybridBoardArgs) -> Result<()> {
    let mut config = BoardInformation::from_dir(&args.config)?;
    let replace_config = BoardInformation::from_dir(&args.replace_bibs_from_board)?;
    for (name, replacement) in replace_config.input_bundles.into_iter() {
        config.input_bundles.entry(name).and_modify(|bib| *bib = replacement);
    }
    config.write_to_dir(&args.output, args.depfile.as_ref())?;
    Ok(())
}
