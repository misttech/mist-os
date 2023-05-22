// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Deserialize, Serialize)]
pub struct ICUMap(std::collections::HashMap<Revision, String>);

impl ICUMap {
    /// Gets the commit ID corresponding to the given `revision`.
    pub fn get(&self, revision: &Revision) -> Option<&str> {
        self.0.get(revision).map(|k| k.as_str())
    }

    /// Maps back from `commit_id` to a `Revision` if possible.
    pub fn revision_for(&self, commit_id: &str) -> Option<&Revision> {
        // Small map, and a quick but easy way to map back from value to key.
        self.0.iter().filter(|(_, v)| &v[..] == commit_id).map(|(k, _)| k).next()
    }
}

// See `rustenv` in //src/lib/assembly/config_schema:config_schema.
pub static ICU_CONFIG_INFO: Lazy<ICUMap> = Lazy::new(|| {
    serde_json::from_value(
        serde_json::from_str(include_str!(env!("ICU_GIT_INFO_JSON_FILE"))).unwrap(),
    )
    .unwrap()
});

#[derive(Debug, Default, Deserialize, Serialize, PartialEq, Hash, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Revision {
    /// Whatever revision is currently 'default'.
    #[default]
    Default,
    /// Whatever revision is currently 'stable'.
    Stable,
    /// Whatever revision is currently 'latest'.
    Latest,
    /// If none of the above work, then you can specify a git commit ID.
    ///
    /// Use:
    ///
    /// ```ignore
    /// {
    ///   "revision": { "commit_id": "f005...ba11" }
    /// }
    /// ```
    ///
    /// Where the value of `commit_id` is a fully specified git
    /// commit hash.
    CommitId(String),
}

/// System assembly configuration for the ICU subsystem.
#[derive(Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct ICUConfig {
    /// The revision (corresponding to either one of the labels, or a git commit ID) of the ICU
    /// library to use in system assembly. This revision is constrained to the commit IDs available
    /// in the repos at `//third_party/icu/{default,stable,latest}`,
    pub revision: Revision,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_from_json_tag() {
        struct TestCase {
            input: &'static str,
            expected: ICUConfig,
        }
        let tests = vec![
            TestCase {
                input: r#"{ "revision": { "commit_id": "deadbeef" } }"#,
                expected: ICUConfig { revision: Revision::CommitId("deadbeef".into()) },
            },
            TestCase {
                input: r#"{ "revision": "stable" }"#,
                expected: ICUConfig { revision: Revision::Stable },
            },
        ];
        for test in tests {
            let json = serde_json::from_str(test.input).unwrap();
            let parsed: ICUConfig = serde_json::from_value(json).unwrap();
            assert_eq!(parsed, test.expected);
        }
    }

    #[test]
    fn check_icu_map() {
        assert!(ICU_CONFIG_INFO.get(&Revision::Stable).is_some());
        assert!(ICU_CONFIG_INFO.get(&Revision::Latest).is_some());
        assert!(ICU_CONFIG_INFO.get(&Revision::Default).is_some());
    }
}
