// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use camino::{Utf8Path, Utf8PathBuf};
use serde::{Deserialize, Serialize};
use sha2::Digest;
use std::collections::{HashMap, HashSet};
use std::io::ErrorKind;
use std::ops::Deref;

#[derive(thiserror::Error, Debug)]
#[non_exhaustive]
pub enum FileStateError {
    #[error("Paths in state files must be relative, but '{0}' was an absolute path.")]
    AbsolutePath(Utf8PathBuf),
    #[error("Unexpected error trying to hash '{path}': {source}")]
    HashFileError {
        path: Utf8PathBuf,
        #[source]
        source: std::io::Error,
    },
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct FileStates {
    /// For future compatibility. For now this is always zero.
    version: u64,
    /// the path this state set was pulled from originally, so that if the
    /// directory itself moves it will force a re-check.
    root: Utf8PathBuf,
    /// the state of the given paths within the root, where `None` means
    /// the file didn't exist and if it's Some then it contains a content hash
    /// of the file at last observation.
    files: HashMap<Utf8PathBuf, Option<Vec<u8>>>,
}

impl FileStates {
    pub fn check_paths(
        root: Utf8PathBuf,
        paths: &[impl AsRef<Utf8Path>],
    ) -> Result<Self, FileStateError> {
        let mut files = HashMap::default();
        for path in paths {
            let path = path.as_ref().to_owned();
            if path.is_absolute() {
                return Err(FileStateError::AbsolutePath(path));
            }
            match hash_path(&root.join(&path)) {
                Ok(state) => files.insert(path, state),
                Err(source) => return Err(FileStateError::HashFileError { path, source }),
            };
        }
        Ok(FileStates { root, files, ..Default::default() })
    }

    pub fn files(&self) -> impl Iterator<Item = &Utf8Path> {
        self.files.keys().map(Deref::deref)
    }

    pub fn changed_files(&self, newer: &Self) -> Result<(), HashSet<&Utf8Path>> {
        if self != newer {
            let mut changed = HashSet::new();
            for (path, hash) in self.files.iter() {
                match newer.files.get(path) {
                    Some(new_hash) if hash != new_hash => {
                        changed.insert(path.deref());
                    }
                    None if hash.is_some() => {
                        changed.insert(path.deref());
                    }
                    _ => continue,
                }
            }

            Err(changed)
        } else {
            Ok(())
        }
    }
}

fn hash_path(full_path: &Utf8Path) -> Result<Option<Vec<u8>>, std::io::Error> {
    match std::fs::File::open(full_path) {
        Ok(mut file) => {
            let mut hasher = sha2::Sha256::new();
            std::io::copy(&mut file, &mut hasher)?;
            Ok(Some(hasher.finalize().to_vec()))
        }
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::*;

    fn basic_root_files(root: Utf8PathBuf) -> FileStates {
        FileStates {
            version: 0,
            root,
            files: HashMap::from_iter([
                (
                    "fuchsia_env.toml".into(),
                    Some(
                        [
                            242, 217, 205, 123, 87, 157, 133, 69, 196, 56, 125, 48, 130, 161, 2,
                            37, 41, 91, 133, 106, 215, 182, 116, 250, 24, 55, 250, 114, 154, 22,
                            119, 70,
                        ]
                        .into(),
                    ),
                ),
                (
                    "stuff/keep-me.txt".into(),
                    Some(
                        [
                            59, 142, 178, 241, 236, 111, 62, 254, 173, 184, 138, 160, 82, 47, 217,
                            134, 14, 192, 128, 38, 187, 17, 162, 33, 90, 148, 181, 2, 76, 94, 225,
                            71,
                        ]
                        .into(),
                    ),
                ),
                ("stuff/blah".into(), None),
                ("blorp".into(), None),
            ]),
        }
    }

    #[test]
    fn check_paths_no_absolute_paths() {
        let basic_root = test_data_path().join("basic_example").canonicalize_utf8().unwrap();
        FileStates::check_paths(basic_root, &["/tmp/blah"])
            .expect_err("Absolute paths should fail");
    }

    #[test]
    fn check_paths() {
        let basic_root = test_data_path().join("basic_example").canonicalize_utf8().unwrap();
        let basic_root_files = basic_root_files(basic_root.clone());
        let states =
            FileStates::check_paths(basic_root.clone(), &Vec::from_iter(basic_root_files.files()))
                .unwrap();
        assert_eq!(states, basic_root_files);
        assert_ne!(
            states,
            FileStates {
                version: 0,
                root: basic_root.clone(),
                files: HashMap::from_iter([
                    (
                        "fuchsia_env.toml".into(),
                        Some(
                            [
                                242, 217, 205, 123, 87, 157, 133, 69, 196, 56, 125, 48, 130, 161,
                                2, 37, 41, 91, 133, 106, 215, 182, 116, 250, 24, 55, 250, 114, 154,
                                22, 119, 70
                            ]
                            .into()
                        )
                    ),
                    (
                        "stuff/keep-me.txt".into(),
                        Some(
                            [
                                59, 142, 178, 241, 236, 111, 62, 254, 173, 184, 138, 160, 82, 47,
                                217, 134, 14, 192, 128, 38, 187, 17, 162, 33, 90, 148, 181, 2, 76,
                                94, 225, 71
                            ]
                            .into()
                        )
                    ),
                ]),
            },
            "Non-present files count for equality"
        );
    }

    #[test]
    fn changed_files() {
        let basic_root = test_data_path().join("basic_example").canonicalize_utf8().unwrap();
        let basic_root_files = basic_root_files(basic_root.clone());

        FileStates::default()
            .changed_files(&FileStates::default())
            .expect("No changes to empty sets");

        basic_root_files.changed_files(&basic_root_files).expect("No changes from the same set");

        assert_eq!(
            basic_root_files.changed_files(&FileStates::default()),
            Err(HashSet::from_iter(
                ["fuchsia_env.toml".into(), "stuff/keep-me.txt".into(),].into_iter()
            )),
            "empty new set should return changes"
        );

        let files_with_no_existence =
            HashMap::from_iter(basic_root_files.files().map(|f| (f.into(), None)));
        assert_eq!(
            basic_root_files.changed_files(&FileStates {
                files: files_with_no_existence,
                ..basic_root_files.clone()
            }),
            Err(HashSet::from_iter(
                ["fuchsia_env.toml".into(), "stuff/keep-me.txt".into(),].into_iter()
            )),
            "new set with all files not-present should return changes"
        );

        let files_with_fake_hashes =
            HashMap::from_iter(basic_root_files.files().map(|f| (f.into(), Some(vec![0]))));
        assert_eq!(
            basic_root_files.changed_files(&FileStates {
                files: files_with_fake_hashes,
                ..basic_root_files.clone()
            }),
            Err(HashSet::from_iter(
                [
                    "fuchsia_env.toml".into(),
                    "stuff/keep-me.txt".into(),
                    "stuff/blah".into(),
                    "blorp".into(),
                ]
                .into_iter()
            )),
            "new set with all files with fake hashes should return changes to all files"
        );
    }
}
