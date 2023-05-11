// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use serde::{
    de::{Error, Unexpected},
    Deserialize, Deserializer, Serialize, Serializer,
};

use std::collections::BTreeMap;

const VERSION_HISTORY_BYTES: &[u8] = include_bytes!(env!("SDK_VERSION_HISTORY"));
const VERSION_HISTORY_SCHEMA_ID: &str = "https://fuchsia.dev/schema/version_history-22rnd667.json";
const VERSION_HISTORY_NAME: &str = "Platform version map";
const VERSION_HISTORY_TYPE: &str = "version_history";

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Clone)]
pub struct AbiRevision {
    pub value: u64,
}

impl AbiRevision {
    pub fn new(u: u64) -> AbiRevision {
        AbiRevision { value: u }
    }
}

impl Serialize for AbiRevision {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        format!("{:#X}", self.value).serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for AbiRevision {
    fn deserialize<D>(deserializer: D) -> Result<AbiRevision, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        if let Some(s) = s.strip_prefix("0x") {
            u64::from_str_radix(&s, 16)
        } else {
            u64::from_str_radix(&s, 10)
        }
        .map_err(|_| D::Error::invalid_value(Unexpected::Str(&s), &"an unsigned integer"))
        .map(|v| AbiRevision { value: v })
    }
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Clone, Serialize, Deserialize)]
struct ApiLevel {
    pub abi_revision: AbiRevision,
    pub status: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Version {
    pub api_level: u64,
    pub abi_revision: AbiRevision,
    pub status: Status,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, PartialOrd, Ord, Hash)]
pub enum Status {
    #[serde(rename = "in-development")]
    InDevelopment,
    #[serde(rename = "supported")]
    Supported,
    #[serde(rename = "unsupported")]
    Unsupported,
}

fn status_from_str(s: &str) -> Result<Status, String> {
    match s {
        "in-development" => Ok(Status::InDevelopment),
        "supported" => Ok(Status::Supported),
        "unsupported" => Ok(Status::Unsupported),
        _ => Err(format!("Invalid status value: {}", s)),
    }
}

#[derive(Serialize, Deserialize)]
struct VersionHistoryData {
    name: String,
    #[serde(rename = "type")]
    element_type: String,
    api_levels: BTreeMap<String, ApiLevel>,
}

#[derive(Serialize, Deserialize)]
struct VersionHistory {
    schema_id: String,
    data: VersionHistoryData,
}

pub fn version_history() -> Result<Vec<Version>, serde_json::Error> {
    parse_version_history(VERSION_HISTORY_BYTES)
}

fn parse_version_history(bytes: &[u8]) -> Result<Vec<Version>, serde_json::Error> {
    let v: VersionHistory = serde_json::from_slice(bytes)?;
    if v.schema_id != VERSION_HISTORY_SCHEMA_ID {
        return Err(serde_json::Error::invalid_value(
            Unexpected::Str(&v.schema_id),
            &VERSION_HISTORY_SCHEMA_ID,
        ));
    }
    if v.data.name != VERSION_HISTORY_NAME {
        return Err(serde_json::Error::invalid_value(
            Unexpected::Str(&v.data.name),
            &VERSION_HISTORY_NAME,
        ));
    }
    if v.data.element_type != VERSION_HISTORY_TYPE {
        return Err(serde_json::Error::invalid_value(
            Unexpected::Str(&v.data.element_type),
            &VERSION_HISTORY_TYPE,
        ));
    }

    let mut versions = Vec::new();

    for (key, value) in v.data.api_levels {
        let api_level = key
            .parse()
            .map_err(|_| serde::de::Error::invalid_value(Unexpected::Str(&key), &"an integer"))?;

        let version_status = status_from_str(&value.status)
            .map_err(|err| serde_json::Error::custom(format!("Error: {}", err)))?;

        versions.push(Version {
            api_level,
            abi_revision: value.abi_revision,
            status: version_status,
        });
    }

    versions.sort_by_key(|s| s.api_level);

    Ok(versions)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_version_history_works() {
        let versions = version_history().unwrap();
        assert_eq!(
            versions[0],
            Version {
                api_level: 4,
                abi_revision: AbiRevision::new(0x601665C5B1A89C7F),
                status: Status::Unsupported,
            }
        )
    }

    #[test]
    fn test_parse_history_works() {
        let expected_bytes = br#"{
            "data": {
                "name": "Platform version map",
                "type": "version_history",
                "api_levels": {
                    "1":{
                        "abi_revision":"10",
                        "status":"supported"
                    },
                    "2":{
                        "abi_revision":"0x20",
                        "status":"in-development"
                    }
                }
            },
            "schema_id": "https://fuchsia.dev/schema/version_history-22rnd667.json"
        }"#;

        assert_eq!(
            parse_version_history(&expected_bytes[..]).unwrap(),
            vec![
                Version {
                    api_level: 1,
                    abi_revision: AbiRevision::new(10),
                    status: Status::Supported
                },
                Version {
                    api_level: 2,
                    abi_revision: AbiRevision::new(0x20),
                    status: Status::InDevelopment
                },
            ],
        );
    }

    #[test]
    fn test_parse_history_rejects_invalid_schema() {
        let expected_bytes = br#"{
            "data": {
                "name": "Platform version map",
                "type": "version_history",
                "api_levels": {}
            },
            "schema_id": "some-schema"
        }"#;

        assert_eq!(
            &parse_version_history(&expected_bytes[..]).unwrap_err().to_string(),
            "invalid value: string \"some-schema\", expected https://fuchsia.dev/schema/version_history-22rnd667.json"
        );
    }

    #[test]
    fn test_parse_history_rejects_invalid_name() {
        let expected_bytes = br#"{
            "data": {
                "name": "some-name",
                "type": "version_history",
                "api_levels": {}
            },
            "schema_id": "https://fuchsia.dev/schema/version_history-22rnd667.json"
        }"#;

        assert_eq!(
            &parse_version_history(&expected_bytes[..]).unwrap_err().to_string(),
            "invalid value: string \"some-name\", expected Platform version map"
        );
    }

    #[test]
    fn test_parse_history_rejects_invalid_type() {
        let expected_bytes = br#"{
            "data": {
                "name": "Platform version map",
                "type": "some-type",
                "api_levels": {}
            },
            "schema_id": "https://fuchsia.dev/schema/version_history-22rnd667.json"
        }"#;

        assert_eq!(
            &parse_version_history(&expected_bytes[..]).unwrap_err().to_string(),
            "invalid value: string \"some-type\", expected version_history"
        );
    }

    #[test]
    fn test_parse_history_rejects_invalid_versions() {
        for (api_level, abi_revision, err) in [
            (
                "some-version",
                "1",
                "invalid value: string \"some-version\", expected an integer",
            ),
            (
                "-1",
                "1",
                 "invalid value: string \"-1\", expected an integer",
            ),
            (
                "1",
                "some-revision",
                "invalid value: string \"some-revision\", expected an unsigned integer at line 1 column 58",
            ),
            (
                "1",
                "-1",
                "invalid value: string \"-1\", expected an unsigned integer at line 1 column 47",
            ),
        ] {
            let expected_bytes = serde_json::to_vec(&serde_json::json!({
                "data": {
                    "name": VERSION_HISTORY_NAME,
                    "type": VERSION_HISTORY_TYPE,
                    "api_levels": {
                        api_level:{
                            "abi_revision": abi_revision,
                            "status": Status::InDevelopment,
                        }
                    },
                },
                "schema_id": VERSION_HISTORY_SCHEMA_ID,
            }))
            .unwrap();

            assert_eq!(parse_version_history(&expected_bytes[..]).unwrap_err().to_string(), err);
        }
    }
}
