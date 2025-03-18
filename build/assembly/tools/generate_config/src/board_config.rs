// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{BoardArgs, HybridBoardArgs};

use anyhow::Result;
use assembly_config_schema::{BoardInformation, BoardInputBundleSet};
use assembly_container::{AssemblyContainer, DirectoryPathBuf};
use std::collections::BTreeMap;

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
    let mut config = config.add_bib_names()?;

    // Map of BIB repository to the BIB set.
    let bib_sets: BTreeMap<String, BoardInputBundleSet> = args
        .board_input_bundle_sets
        .iter()
        .map(|path| {
            let bib_set = BoardInputBundleSet::from_dir(&path)?;
            let set_name = bib_set.name.clone();
            Ok((set_name, bib_set))
        })
        .collect::<Result<BTreeMap<String, BoardInputBundleSet>>>()?;

    // Add all the BIBs from the BIB sets.
    for (set_name, set) in bib_sets {
        for (bib_name, bib_entry) in set.board_input_bundles {
            let bib_ref = BibReference::FromBibSet { set: set_name.clone(), name: bib_name };
            config.input_bundles.insert(bib_ref.to_string(), bib_entry.path);
        }
    }

    config.write_to_dir(&args.output, args.depfile.as_ref())?;
    Ok(())
}

pub fn hybrid(args: &HybridBoardArgs) -> Result<()> {
    let mut config = BoardInformation::from_dir(&args.config)?;

    // First, replace the bibs found in `replace_bibs_from_board`.
    if let Some(replace_bibs_from_board) = &args.replace_bibs_from_board {
        let replace_config = BoardInformation::from_dir(replace_bibs_from_board)?;
        for (name, replacement) in replace_config.input_bundles.into_iter() {
            config.input_bundles.entry(name).and_modify(|bib| *bib = replacement);
        }
    }

    // Second, replace the bibs found in `replace_bib_sets`.
    let replace_bib_sets: BTreeMap<String, BoardInputBundleSet> = args
        .replace_bib_sets
        .iter()
        .map(|path| {
            let bib_set = BoardInputBundleSet::from_dir(&path)?;
            let set_name = bib_set.name.clone();
            Ok((set_name, bib_set))
        })
        .collect::<Result<BTreeMap<String, BoardInputBundleSet>>>()?;
    for (full_bib_name, bib_path) in &mut config.input_bundles {
        let bib_ref = BibReference::from(full_bib_name);

        // Replace BIBs that are part of a BIB set.
        if let BibReference::FromBibSet { set, name } = bib_ref {
            if let Some(replace_bib_set) = replace_bib_sets.get(&set) {
                if let Some(replace_bib_entry) = replace_bib_set.board_input_bundles.get(&name) {
                    *bib_path = replace_bib_entry.path.clone();
                }
            }
        }
    }

    config.write_to_dir(&args.output, args.depfile.as_ref())?;
    Ok(())
}

/// A reference of a BIB found in a board, which can either have been from a
/// BIB set or added independently not through a set.
enum BibReference {
    /// A BIB that was added via a BIB set.
    /// We keep track of the set name, so that we can easily replace the entire
    /// set of BIBs wholesale.
    FromBibSet { set: String, name: String },

    /// A BIB that was added independent of a BIB set.
    Independent { name: String },
}

impl From<&String> for BibReference {
    fn from(s: &String) -> Self {
        let mut parts: Vec<&str> = s.split("::").collect();
        let bib_name = parts.pop();
        let set_name = parts.pop();
        match (set_name, bib_name) {
            (Some(set), Some(name)) => {
                Self::FromBibSet { set: set.to_string(), name: name.to_string() }
            }
            _ => Self::Independent { name: s.to_string() },
        }
    }
}

impl ToString for BibReference {
    fn to_string(&self) -> String {
        match self {
            Self::FromBibSet { set, name } => format!("{}::{}", set, name),
            Self::Independent { name } => name.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use assembly_config_schema::{BoardInputBundle, BoardInputBundleEntry};
    use camino::Utf8PathBuf;
    use std::collections::BTreeSet;
    use tempfile::{tempdir, NamedTempFile};

    #[test]
    fn test_new_board() {
        let config_file = NamedTempFile::new().unwrap();
        let config_path = Utf8PathBuf::from_path_buf(config_file.path().to_path_buf()).unwrap();
        let config_value = serde_json::json!({
            "name": "my_board",
        });
        serde_json::to_writer(&config_file, &config_value).unwrap();

        let tmp_dir = tempdir().unwrap();
        let tmp_path = Utf8PathBuf::from_path_buf(tmp_dir.path().to_path_buf()).unwrap();

        // Create a BIB.
        let bib_path = tmp_path.join("my_bib");
        let bib = BoardInputBundle {
            name: "my_bib".to_string(),
            kernel_boot_args: ["arg".to_string()].into(),
            ..Default::default()
        };
        bib.write_to_dir(&bib_path, None::<Utf8PathBuf>).unwrap();

        // Add the BIB to a set.
        let bib_set_path = tmp_path.join("my_bib_set");
        let bib_set = BoardInputBundleSet {
            name: "my_bib_set".to_string(),
            board_input_bundles: [(
                "my_bib".to_string(),
                BoardInputBundleEntry { path: DirectoryPathBuf(bib_path) },
            )]
            .into(),
        };
        bib_set.write_to_dir(&bib_set_path, None::<Utf8PathBuf>).unwrap();

        // Create a board.
        let board_path = tmp_path.join("my_board");
        let args = BoardArgs {
            config: config_path,
            board_input_bundles: vec![],
            board_input_bundle_sets: vec![bib_set_path],
            output: board_path.clone(),
            depfile: None,
        };
        new(&args).unwrap();

        // Ensure the BIB in the board contains the correct kernel_boot_args.
        let board = BoardInformation::from_dir(board_path).unwrap();
        let expected = vec!["my_bib_set::my_bib".to_string()];
        itertools::assert_equal(expected.iter(), board.input_bundles.keys());
        let bib_path = board.input_bundles.get("my_bib_set::my_bib").unwrap();
        let bib = BoardInputBundle::from_dir(bib_path).unwrap();
        let expected = BTreeSet::<String>::from(["arg".to_string()]);
        assert_eq!(expected, bib.kernel_boot_args);
    }

    #[test]
    fn test_hybrid_board() {
        let tmp_dir = tempdir().unwrap();
        let tmp_path = Utf8PathBuf::from_path_buf(tmp_dir.path().to_path_buf()).unwrap();

        // Create a BIB.
        let bib_path = tmp_path.join("my_bib");
        let bib = BoardInputBundle {
            name: "my_bib".to_string(),
            kernel_boot_args: ["before".to_string()].into(),
            ..Default::default()
        };
        bib.write_to_dir(&bib_path, None::<Utf8PathBuf>).unwrap();

        // Create a board with the BIB already added.
        let board_path = tmp_path.join("my_board");
        let board = BoardInformation {
            name: "my_board".to_string(),
            hardware_info: Default::default(),
            provided_features: Default::default(),
            devicetree: Default::default(),
            devicetree_overlay: Default::default(),
            filesystems: Default::default(),
            input_bundles: [("my_bib_set::my_bib".to_string(), DirectoryPathBuf(bib_path))].into(),
            configuration: Default::default(),
            kernel: Default::default(),
            platform: Default::default(),
            tee_trusted_app_guids: Default::default(),
        };
        board.write_to_dir(&board_path, None::<Utf8PathBuf>).unwrap();

        // Create a new BIB with the same name, but different kernel_boot_args.
        let new_bib_path = tmp_path.join("new_my_bib");
        let bib = BoardInputBundle {
            name: "my_bib".to_string(),
            kernel_boot_args: ["after".to_string()].into(),
            ..Default::default()
        };
        bib.write_to_dir(&new_bib_path, None::<Utf8PathBuf>).unwrap();

        // Add the BIB to a set.
        let bib_set_path = tmp_path.join("my_bib_set");
        let bib_set = BoardInputBundleSet {
            name: "my_bib_set".to_string(),
            board_input_bundles: [(
                "my_bib".to_string(),
                BoardInputBundleEntry { path: DirectoryPathBuf(new_bib_path) },
            )]
            .into(),
        };
        bib_set.write_to_dir(&bib_set_path, None::<Utf8PathBuf>).unwrap();

        // Create a hybrid board and replace the BIB using the set.
        let hybrid_board_path = tmp_path.join("my_hybrid_board");
        let args = HybridBoardArgs {
            config: board_path,
            output: hybrid_board_path.clone(),
            replace_bibs_from_board: None,
            replace_bib_sets: vec![bib_set_path],
            depfile: None,
        };
        hybrid(&args).unwrap();

        // Ensure the BIB in the board contains the correct kernel_boot_args.
        let board = BoardInformation::from_dir(hybrid_board_path).unwrap();
        let expected = vec!["my_bib_set::my_bib".to_string()];
        itertools::assert_equal(expected.iter(), board.input_bundles.keys());
        let bib_path = board.input_bundles.get("my_bib_set::my_bib").unwrap();
        let bib = BoardInputBundle::from_dir(bib_path).unwrap();
        let expected = BTreeSet::<String>::from(["after".to_string()]);
        assert_eq!(expected, bib.kernel_boot_args);
    }
}
