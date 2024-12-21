// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{format_err, Error};
use diagnostics_data::ExtendedMoniker;
use glob::glob;
use log::{info, warn};
use persistence_config::{ServiceName, Tag};
use serde::ser::SerializeMap;
use serde::{Serialize, Serializer};
use serde_json::Value;
use std::collections::HashMap;
use std::fs;

const CURRENT_PATH: &str = "/cache/current";
const PREVIOUS_PATH: &str = "/cache/previous";

pub(crate) struct PersistSchema {
    pub timestamps: Timestamps,
    pub payload: PersistPayload,
}

pub(crate) enum PersistPayload {
    Data(PersistData),
    Error(String),
}

pub(crate) struct PersistData {
    pub data_length: usize,
    pub entries: HashMap<ExtendedMoniker, Value>,
}

#[derive(Clone, Serialize)]
pub(crate) struct Timestamps {
    pub before_monotonic: i64,
    pub before_utc: i64,
    pub after_monotonic: i64,
    pub after_utc: i64,
}

// Keys for JSON per-tag metadata to be persisted and published
const TIMESTAMPS_KEY: &str = "@timestamps";
const SIZE_KEY: &str = "@persist_size";
const ERROR_KEY: &str = ":error";
const ERROR_DESCRIPTION_KEY: &str = "description";

impl Serialize for PersistSchema {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match &self.payload {
            PersistPayload::Data(data) => {
                let mut s = serializer.serialize_map(Some(data.entries.len() + 2))?;
                s.serialize_entry(TIMESTAMPS_KEY, &self.timestamps)?;
                s.serialize_entry(SIZE_KEY, &data.data_length)?;
                for (k, v) in data.entries.iter() {
                    s.serialize_entry(&k.to_string(), v)?;
                }
                s.end()
            }
            PersistPayload::Error(error) => {
                let mut s = serializer.serialize_map(Some(2))?;
                s.serialize_entry(TIMESTAMPS_KEY, &self.timestamps)?;
                s.serialize_entry(ERROR_KEY, &ErrorHelper(error))?;
                s.end()
            }
        }
    }
}

impl PersistSchema {
    pub(crate) fn error(timestamps: Timestamps, description: String) -> Self {
        Self { timestamps, payload: PersistPayload::Error(description) }
    }
}

struct ErrorHelper<'a>(&'a str);

impl Serialize for ErrorHelper<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut s = serializer.serialize_map(Some(1))?;
        s.serialize_entry(ERROR_DESCRIPTION_KEY, self.0)?;
        s.end()
    }
}

// Throw away stuff from two boots ago. Move stuff in the "current"
// directory to the "previous" directory.
pub fn shuffle_at_boot() {
    // These may fail if /cache was wiped. This is WAI and should not signal an error.
    fs::remove_dir_all(PREVIOUS_PATH)
        .map_err(|e| info!("Could not delete {}: {:?}", PREVIOUS_PATH, e))
        .ok();
    fs::rename(CURRENT_PATH, PREVIOUS_PATH)
        .map_err(|e| info!("Could not move {} to {}: {:?}", CURRENT_PATH, PREVIOUS_PATH, e))
        .ok();
}

// Write a VMO's contents to the appropriate file.
pub(crate) fn write(service_name: &ServiceName, tag: &Tag, data: &PersistSchema) {
    // /cache/ may be deleted any time. It's OK to try to create CURRENT_PATH if it already exists.
    let path = format!("{}/{}", CURRENT_PATH, service_name);
    fs::create_dir_all(&path)
        .map_err(|e| warn!("Could not create directory {}: {:?}", path, e))
        .ok();
    let data = match serde_json::to_string(data) {
        Ok(data) => data,
        Err(e) => {
            warn!("Could not serialize data - unexpected error {e}");
            return;
        }
    };
    fs::write(format!("{}/{}", path, tag), data)
        .map_err(|e| warn!("Could not write file {}/{}: {:?}", path, tag, e))
        .ok();
}

pub(crate) struct ServiceEntry {
    pub name: String,
    pub data: Vec<TagEntry>,
}

pub(crate) struct TagEntry {
    pub name: String,
    pub data: String,
}

// All the names in the previous-boot directory.
// TODO(https://fxbug.dev/42150693): If this gets big, use Lazy Inspect.
pub(crate) fn remembered_data() -> Result<Vec<ServiceEntry>, Error> {
    // Counter for number of tags successfully retrieved. If no persisted tags were
    // retrieved, this method returns an error.
    let mut tags_retrieved = 0;

    let mut service_entries = Vec::new();
    // Create an iterator over all subdirectories of /cache/previous
    // which contains persisted data from the last boot.
    for service_path in glob(&format!("{}/*", PREVIOUS_PATH))
        .map_err(|e| format_err!("Failed to read previous-path glob pattern: {:?}", e))?
    {
        match service_path {
            Err(e) => {
                // If our glob pattern was valid, but we encountered glob errors while iterating, just warn
                // since there may still be some persisted metrics.
                warn!(
                    "Encountered GlobError; contents could not be read to determine if glob pattern was matched: {:?}",
                    e
                )
            }
            Ok(path) => {
                if let Some(name) = path.file_name() {
                    let service_name = name.to_string_lossy().to_string();
                    let mut tag_entries = Vec::new();
                    for tag_path in
                        glob(&format!("{}/{}/*", PREVIOUS_PATH, service_name)).map_err(|e| {
                            format_err!(
                                "Failed to read previous service persistence pattern: {:?}",
                                e
                            )
                        })?
                    {
                        match tag_path {
                            Ok(path) => {
                                if let Some(tag_name) = path.file_name() {
                                    let tag_name = tag_name.to_string_lossy().to_string();
                                    match fs::read(path.clone()) {
                                        Ok(text) => {
                                            // TODO(cphoenix): We want to encode failures at retrieving persisted
                                            // metrics in the inspect hierarchy so clients know why their data is
                                            // missing.
                                            match std::str::from_utf8(&text) {
                                                Ok(contents) => {
                                                    tags_retrieved += 1;

                                                    tag_entries.push(TagEntry {
                                                        name: tag_name,
                                                        data: contents.to_owned(),
                                                    });
                                                }
                                                Err(e) => {
                                                    warn!(
                                                        "Failed to parse persisted bytes at path: {:?} into text: {:?}",
                                                        path, e
                                                    );
                                                }
                                            }
                                        }
                                        Err(e) => {
                                            warn!(
                                            "Failed to retrieve text persisted at path: {:?}: {:?}",
                                            path, e
                                        );
                                        }
                                    }
                                }
                            }
                            Err(e) => {
                                // If our glob pattern was valid, but we encountered glob errors while iterating, just warn
                                // since there may still be some persisted metrics.
                                warn!(
                                        "Encountered GlobError; contents could not be read to determine if glob pattern was matched: {:?}",
                                        e
                                    )
                            }
                        }
                    }
                    service_entries.push(ServiceEntry { name: service_name, data: tag_entries });
                }
            }
        };
    }

    if tags_retrieved == 0 {
        info!("No persisted data was successfully retrieved.");
    }

    Ok(service_entries)
}
