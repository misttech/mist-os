// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{anyhow, Context, Result};
use assembly_config_schema::FileEntry;
use assembly_util::{BTreeMapDuplicateKeyError, InsertUniqueExt, MapEntry, NamedMap};
use camino::Utf8PathBuf;
use fuchsia_merkle::Hash;
use fuchsia_pkg::BlobInfo;
use serde::Serialize;
use utf8_path::path_relative_from_current_dir;

/// A paired source and optional merkle for a file.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct SourceMerklePair {
    /// The source path.
    pub source: Utf8PathBuf,
    /// The merkle of the file at the source path.
    pub merkle: Option<Hash>,
}

impl From<BlobInfo> for SourceMerklePair {
    fn from(blob: BlobInfo) -> Self {
        Self { source: blob.source_path.into(), merkle: Some(blob.merkle) }
    }
}

impl From<FileEntry> for SourceMerklePair {
    fn from(entry: FileEntry) -> Self {
        Self { source: entry.source, merkle: None }
    }
}

fn relativize_entry(entry: FileEntry) -> Result<FileEntry> {
    let source = path_relative_from_current_dir(&entry.source)
        .with_context(|| format!("relativizing path {}", entry.source))?;
    Ok(FileEntry { source, destination: entry.destination })
}

/// A named set of file entries, keyed by file destination.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct NamedFileMap {
    /// Map from destination path to source path and merkle.
    pub map: NamedMap<SourceMerklePair>,
}

impl NamedFileMap {
    /// Construct a NamedFileMap.
    pub fn new(name: &str) -> Self {
        NamedFileMap { map: NamedMap::new(name) }
    }

    /// Add a single FileEntry to the map, if the 'destination' path is a
    /// duplicate, return an error, otherwise add the entry.
    pub fn add_entry(&mut self, entry: FileEntry) -> Result<()> {
        let entry = relativize_entry(entry)?;
        self.map
            .try_insert_unique(entry.destination.clone(), entry.into())
            .with_context(|| format!("Adding entry to set: {}", self.map.name))
    }

    /// Add a single FileEntry to the map by first computing the merkle hash
    /// then calling add_blob. This is helpful because it allows FileEntries
    /// to be de-duplicated.
    pub fn add_entry_as_blob(&mut self, entry: FileEntry) -> Result<()> {
        let entry = relativize_entry(entry)?;
        let mut file = std::fs::File::open(&entry.source)
            .with_context(|| format!("Opening file {}", &entry.source))?;
        let merkle_tree = fuchsia_merkle::MerkleTree::from_reader(&mut file)
            .with_context(|| format!("Constructing merkle for file {}", &entry.source))?;
        let blob = BlobInfo {
            source_path: entry.source.to_string(),
            path: entry.destination,
            merkle: merkle_tree.root(),
            size: 0,
        };
        self.add_blob(blob)
    }

    /// Add a single blob to the map. If the 'destination' path is a
    /// duplicate but the merkle is the same, do nothing. If the
    /// `destination' path is a duplicate and the merkle is different,
    /// return an error. Otherwise add the entry.
    pub fn add_blob(&mut self, blob: BlobInfo) -> Result<()> {
        let blob_path = blob.path.clone();
        let result = self.map.entries.try_insert_unique(MapEntry(blob_path.clone(), blob.into()));
        match result {
            Ok(_) => Ok(()),
            Err(BTreeMapDuplicateKeyError { existing_entry, new_value }) => {
                let previous_value = existing_entry.get();
                if let (Some(previous_merkle), Some(new_merkle)) =
                    (previous_value.merkle, new_value.merkle)
                {
                    if previous_merkle == new_merkle {
                        return Ok(());
                    }
                }
                return Err(anyhow!(
                    r#"File has the same destination but a different merkle
            destination     = {}
            previous_merkle = {:?}
            merkle          = {:?}"#,
                    blob_path,
                    previous_value.merkle,
                    new_value.merkle
                ));
            }
        }
    }

    /// Return the contents of the NamedFileMap as a Vec<FileEntry>.
    pub fn into_file_entries(self) -> Vec<FileEntry> {
        self.map
            .entries
            .into_iter()
            .map(|(destination, SourceMerklePair { source, .. })| FileEntry { destination, source })
            .collect()
    }
}

impl std::ops::Deref for NamedFileMap {
    type Target = NamedMap<SourceMerklePair>;

    fn deref(&self) -> &Self::Target {
        &self.map
    }
}

impl std::ops::DerefMut for NamedFileMap {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.map
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entries_diff_src_diff_dest() {
        let mut map = NamedFileMap::new("test");
        map.add_entry(FileEntry { source: "src1".into(), destination: "dest1".into() }).unwrap();
        map.add_entry(FileEntry { source: "src2".into(), destination: "dest2".into() }).unwrap();

        assert_eq!(
            vec![
                FileEntry { source: "src1".into(), destination: "dest1".into() },
                FileEntry { source: "src2".into(), destination: "dest2".into() },
            ],
            map.into_file_entries()
        );
    }

    #[test]
    fn entries_same_src_diff_dest() {
        let mut map = NamedFileMap::new("test");
        map.add_entry(FileEntry { source: "src1".into(), destination: "dest1".into() }).unwrap();
        map.add_entry(FileEntry { source: "src1".into(), destination: "dest2".into() }).unwrap();

        assert_eq!(
            vec![
                FileEntry { source: "src1".into(), destination: "dest1".into() },
                FileEntry { source: "src1".into(), destination: "dest2".into() },
            ],
            map.into_file_entries()
        );
    }

    #[test]
    fn entries_same_src_same_dest() {
        let mut map = NamedFileMap::new("test");
        map.add_entry(FileEntry { source: "src1".into(), destination: "dest1".into() }).unwrap();
        let res = map.add_entry(FileEntry { source: "src1".into(), destination: "dest1".into() });
        assert!(res.is_err());

        assert_eq!(
            vec![FileEntry { source: "src1".into(), destination: "dest1".into() },],
            map.into_file_entries()
        );
    }

    #[test]
    fn blobs_diff_src_diff_dest() {
        let mut map = NamedFileMap::new("test");
        map.add_blob(BlobInfo {
            source_path: "src1".into(),
            path: "dest1".into(),
            merkle: "0000000000000000000000000000000000000000000000000000000000000000"
                .parse()
                .unwrap(),
            size: 0,
        })
        .unwrap();
        map.add_blob(BlobInfo {
            source_path: "src2".into(),
            path: "dest2".into(),
            merkle: "0000000000000000000000000000000000000000000000000000000000000000"
                .parse()
                .unwrap(),
            size: 0,
        })
        .unwrap();

        assert_eq!(
            vec![
                FileEntry { source: "src1".into(), destination: "dest1".into() },
                FileEntry { source: "src2".into(), destination: "dest2".into() },
            ],
            map.into_file_entries()
        );
    }

    #[test]
    fn blobs_same_src_diff_dest() {
        let mut map = NamedFileMap::new("test");
        map.add_blob(BlobInfo {
            source_path: "src1".into(),
            path: "dest1".into(),
            merkle: "0000000000000000000000000000000000000000000000000000000000000000"
                .parse()
                .unwrap(),
            size: 0,
        })
        .unwrap();
        map.add_blob(BlobInfo {
            source_path: "src1".into(),
            path: "dest2".into(),
            merkle: "0000000000000000000000000000000000000000000000000000000000000000"
                .parse()
                .unwrap(),
            size: 0,
        })
        .unwrap();

        assert_eq!(
            vec![
                FileEntry { source: "src1".into(), destination: "dest1".into() },
                FileEntry { source: "src1".into(), destination: "dest2".into() },
            ],
            map.into_file_entries()
        );
    }

    #[test]
    fn blobs_same_src_same_dest_same_merkle() {
        let mut map = NamedFileMap::new("test");
        map.add_blob(BlobInfo {
            source_path: "src1".into(),
            path: "dest1".into(),
            merkle: "0000000000000000000000000000000000000000000000000000000000000000"
                .parse()
                .unwrap(),
            size: 0,
        })
        .unwrap();
        map.add_blob(BlobInfo {
            source_path: "src1".into(),
            path: "dest1".into(),
            merkle: "0000000000000000000000000000000000000000000000000000000000000000"
                .parse()
                .unwrap(),
            size: 0,
        })
        .unwrap();

        assert_eq!(
            vec![FileEntry { source: "src1".into(), destination: "dest1".into() },],
            map.into_file_entries()
        );
    }

    #[test]
    fn blobs_same_src_same_dest_diff_merkle() {
        let mut map = NamedFileMap::new("test");
        map.add_blob(BlobInfo {
            source_path: "src1".into(),
            path: "dest1".into(),
            merkle: "0000000000000000000000000000000000000000000000000000000000000000"
                .parse()
                .unwrap(),
            size: 0,
        })
        .unwrap();
        let res = map.add_blob(BlobInfo {
            source_path: "src1".into(),
            path: "dest1".into(),
            merkle: "1111111111111111111111111111111111111111111111111111111111111111"
                .parse()
                .unwrap(),
            size: 0,
        });
        assert!(res.is_err());

        assert_eq!(
            vec![FileEntry { source: "src1".into(), destination: "dest1".into() },],
            map.into_file_entries()
        );
    }

    #[test]
    fn blob_and_entry_diff_src_diff_dest() {
        let mut map = NamedFileMap::new("test");
        map.add_blob(BlobInfo {
            source_path: "src1".into(),
            path: "dest1".into(),
            merkle: "0000000000000000000000000000000000000000000000000000000000000000"
                .parse()
                .unwrap(),
            size: 0,
        })
        .unwrap();
        map.add_entry(FileEntry { source: "src2".into(), destination: "dest2".into() }).unwrap();

        assert_eq!(
            vec![
                FileEntry { source: "src1".into(), destination: "dest1".into() },
                FileEntry { source: "src2".into(), destination: "dest2".into() },
            ],
            map.into_file_entries()
        );
    }

    #[test]
    fn blob_and_entry_same_src_diff_dest() {
        let mut map = NamedFileMap::new("test");
        map.add_blob(BlobInfo {
            source_path: "src1".into(),
            path: "dest1".into(),
            merkle: "0000000000000000000000000000000000000000000000000000000000000000"
                .parse()
                .unwrap(),
            size: 0,
        })
        .unwrap();
        map.add_entry(FileEntry { source: "src1".into(), destination: "dest2".into() }).unwrap();

        assert_eq!(
            vec![
                FileEntry { source: "src1".into(), destination: "dest1".into() },
                FileEntry { source: "src1".into(), destination: "dest2".into() },
            ],
            map.into_file_entries()
        );
    }

    #[test]
    fn blob_and_entry_same_src_same_dest() {
        let mut map = NamedFileMap::new("test");
        map.add_blob(BlobInfo {
            source_path: "src1".into(),
            path: "dest1".into(),
            merkle: "0000000000000000000000000000000000000000000000000000000000000000"
                .parse()
                .unwrap(),
            size: 0,
        })
        .unwrap();
        let res = map.add_entry(FileEntry { source: "src1".into(), destination: "dest1".into() });
        assert!(res.is_err());

        assert_eq!(
            vec![FileEntry { source: "src1".into(), destination: "dest1".into() },],
            map.into_file_entries()
        );
    }
}
