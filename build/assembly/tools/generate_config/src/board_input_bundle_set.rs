// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::BoardInputBundleSetArgs;
use anyhow::Result;
use assembly_config_schema::{BoardInputBundle, BoardInputBundleEntry, BoardInputBundleSet};
use assembly_container::{AssemblyContainer, DirectoryPathBuf};
use std::collections::BTreeMap;

pub fn new(args: &BoardInputBundleSetArgs) -> Result<()> {
    let name = args.name.clone();
    let board_input_bundles: BTreeMap<String, BoardInputBundleEntry> = args
        .board_input_bundles
        .iter()
        .map(|path| {
            let bib = BoardInputBundle::from_dir(&path)?;
            let directory = DirectoryPathBuf(path.clone());
            let entry = BoardInputBundleEntry { path: directory };
            Ok((bib.name, entry))
        })
        .collect::<Result<BTreeMap<String, BoardInputBundleEntry>>>()?;
    let set = BoardInputBundleSet { name, board_input_bundles };
    set.write_to_dir(&args.output, args.depfile.as_ref())?;
    Ok(())
}
