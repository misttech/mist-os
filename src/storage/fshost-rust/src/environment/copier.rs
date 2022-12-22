// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Error;

/// Recursively read all the data out of [`path`] in the namespace, preserving the directory
/// structure and data.
pub async fn read_from(path: &str) -> Result<CopiedData, Error> {
    Ok(CopiedData { entries: read_entries_from(path)? })
}

fn read_entries_from(path: &str) -> Result<Vec<Entry>, Error> {
    let mut entries = Vec::new();
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        // unwrap is okay because fuchsia paths are utf-8.
        let name = entry.file_name().to_str().unwrap().to_string();
        if entry.file_type()?.is_dir() {
            let subdir = format!("{}/{}", path, name);
            entries.push(Entry::Directory { name, entries: read_entries_from(&subdir)? })
        } else {
            entries.push(Entry::File { name, contents: std::fs::read(entry.path())? });
        }
    }
    Ok(entries)
}

pub struct CopiedData {
    entries: Vec<Entry>,
}

impl CopiedData {
    /// Recursively write the stored data and directory structure to a filesystem at bound at
    /// [`path`] in the namespace.
    pub async fn write_to(&self, path: &str) -> Result<(), Error> {
        write_entries_to(path, &self.entries)
    }
}

fn write_entries_to(path: &str, entries: &Vec<Entry>) -> Result<(), Error> {
    for entry in entries {
        match entry {
            Entry::File { name, contents } => {
                let file_path = format!("{}/{}", path, name);
                std::fs::write(&file_path, contents)?;
            }
            Entry::Directory { name, entries } => {
                let subdir = format!("{}/{}", path, name);
                std::fs::create_dir(&subdir)?;
                write_entries_to(&subdir, entries)?;
            }
        }
    }

    Ok(())
}

enum Entry {
    File { name: String, contents: Vec<u8> },
    Directory { name: String, entries: Vec<Entry> },
}
