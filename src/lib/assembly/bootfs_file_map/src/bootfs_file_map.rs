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

/// A paired source and optional merkle for a bootfs file.
#[derive(Debug, Serialize)]
pub struct SourceMerklePair {
    pub source: Utf8PathBuf,
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

/// A named set of file entries, keyed by file destination.
#[derive(Debug, Serialize)]
pub struct BootfsFileMap {
    map: NamedMap<SourceMerklePair>,
}

impl BootfsFileMap {
    /// Construct a BootfsFileMap.
    pub fn new() -> Self {
        BootfsFileMap { map: NamedMap::new("bootfs_files") }
    }

    /// Add a single FileEntry to the map, if the 'destination' path is a
    /// duplicate, return an error, otherwise add the entry.
    pub fn add_entry(&mut self, entry: FileEntry) -> Result<()> {
        self.map
            .try_insert_unique(entry.destination.clone(), entry.into())
            .with_context(|| format!("Adding entry to set: {}", self.map.name))
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
                    r#"Bootfs file has the same destination but a different merkle
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

    /// Return the contents of the BootfsFileMap as a Vec<FileEntry>.
    pub fn into_file_entries(self) -> Vec<FileEntry> {
        self.map
            .entries
            .into_iter()
            .map(|(destination, SourceMerklePair { source, .. })| FileEntry { destination, source })
            .collect()
    }
}

impl std::ops::Deref for BootfsFileMap {
    type Target = NamedMap<SourceMerklePair>;

    fn deref(&self) -> &Self::Target {
        &self.map
    }
}

impl std::ops::DerefMut for BootfsFileMap {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.map
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entries_diff_src_diff_dest() {
        let mut map = BootfsFileMap::new();
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
        let mut map = BootfsFileMap::new();
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
        let mut map = BootfsFileMap::new();
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
        let mut map = BootfsFileMap::new();
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
        let mut map = BootfsFileMap::new();
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
        let mut map = BootfsFileMap::new();
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
        let mut map = BootfsFileMap::new();
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
        let mut map = BootfsFileMap::new();
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
        let mut map = BootfsFileMap::new();
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
        let mut map = BootfsFileMap::new();
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
