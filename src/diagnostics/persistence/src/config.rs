// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use core::ops::Deref;
use std::{borrow::Borrow, fmt::Display};

use {
    anyhow::{bail, Error},
    glob::glob,
    lazy_static::lazy_static,
    regex::Regex,
    serde_derive::Deserialize,
    std::collections::HashMap,
};

/// The outer map is service_name; the inner is tag.
pub(crate) type Config = HashMap<ServiceName, HashMap<Tag, TagConfig>>;

/// Schema for config-file entries. Each config file is a JSON array of these.
#[derive(Deserialize, Default, Debug, PartialEq)]
#[cfg_attr(test, derive(Clone))]
#[serde(deny_unknown_fields)]
pub struct TaggedPersist {
    /// The Inspect data defined here will be published under this tag.
    /// Tags must not be duplicated within a service, even between files.
    /// Tags must conform to /[a-z][a-z-]*/.
    pub tag: String,
    /// Each tag will only be requestable via a named service. Multiple tags can use the
    /// same service name, which will be published and routed as DataPersistence_{service_name}.
    /// Service names must conform to /[a-z][a-z-]*/.
    pub service_name: String,
    /// These selectors will be fetched and stored for publication on the next boot.
    pub selectors: Vec<String>,
    /// This is the max size of the file saved, which is the JSON-serialized version
    /// of the selectors' data.
    pub max_bytes: usize,
    /// Persistence requests will be throttled to this. Requests received early will be delayed.
    pub min_seconds_between_fetch: i64,
}

/// Configuration for a single tag for a single service.
///
/// See [`TaggedPersist`] for the meaning of corresponding fields.
#[derive(Debug, Eq, PartialEq)]
pub struct TagConfig {
    pub selectors: Vec<String>,
    pub max_bytes: usize,
    pub min_seconds_between_fetch: i64,
}

/// Wrapper class for a valid tag name.
///
/// This is a witness class that can only be constructed from a `String` that
/// matches [`NAME_PATTERN`].
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct Tag(String);

/// Wrapper class for a valid service name.
///
/// This is a witness class that can only be constructed from a `String` that
/// matches [`NAME_PATTERN`].
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct ServiceName(String);

/// A regular expression corresponding to a valid tag or service name.
pub const NAME_PATTERN: &'static str = r"^[a-z][a-z-]*$";

lazy_static! {
    static ref NAME_VALIDATOR: Regex = Regex::new(NAME_PATTERN).unwrap();
}

impl Tag {
    pub fn new(tag: String) -> Result<Self, Error> {
        if !NAME_VALIDATOR.is_match(&tag) {
            bail!("Invalid tag {} must match [a-z][a-z-]*", tag);
        }
        Ok(Self(tag))
    }
}

impl ServiceName {
    pub fn new(name: String) -> Result<Self, Error> {
        if !NAME_VALIDATOR.is_match(&name) {
            bail!("Invalid service name {} must match [a-z][a-z-]*", name);
        }
        Ok(Self(name))
    }
}

/// Allow `Tag` to be treated like a `&str` for display, etc.
impl Deref for Tag {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        let Self(tag) = self;
        tag
    }
}

/// Allow `ServiceName` to be treated like a `&str` for display, etc.
impl Deref for ServiceName {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        let Self(tag) = self;
        tag
    }
}

/// Allow treating `Tag` as a `&str` for, e.g., HashMap indexing operations.
impl Borrow<str> for Tag {
    fn borrow(&self) -> &str {
        &*self
    }
}

/// Allow treating `ServiceName` as a `&str` for, e.g., HashMap indexing
/// operations.
impl Borrow<str> for ServiceName {
    fn borrow(&self) -> &str {
        &*self
    }
}

impl Display for Tag {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let Self(name) = self;
        name.fmt(f)
    }
}

impl Display for ServiceName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let Self(name) = self;
        name.fmt(f)
    }
}

const CONFIG_GLOB: &str = "/config/data/*.persist";

pub fn try_insert_items(config: &mut Config, config_text: &str) -> Result<(), Error> {
    let items = serde_json5::from_str::<Vec<TaggedPersist>>(config_text)?;
    for item in items.into_iter() {
        let TaggedPersist { tag, service_name, selectors, max_bytes, min_seconds_between_fetch } =
            item;
        let tag = Tag::new(tag)?;
        let name = ServiceName::new(service_name)?;
        if let Some(existing) = config
            .entry(name.clone())
            .or_insert_with(|| HashMap::new())
            .insert(tag, TagConfig { selectors, max_bytes, min_seconds_between_fetch })
        {
            bail!("Duplicate TagConfig found: {:?}", existing);
        }
    }
    Ok(())
}

pub fn load_configuration_files() -> Result<Config, Error> {
    let mut config = HashMap::new();
    for file_path in glob(CONFIG_GLOB)? {
        try_insert_items(&mut config, &std::fs::read_to_string(file_path?)?)?;
    }
    Ok(config)
}

#[cfg(test)]
mod test {
    use super::*;

    impl From<TaggedPersist> for TagConfig {
        fn from(
            TaggedPersist {
                tag: _,
                service_name: _,
                selectors,
                max_bytes,
                min_seconds_between_fetch,
            }: TaggedPersist,
        ) -> Self {
            Self { selectors, max_bytes, min_seconds_between_fetch }
        }
    }

    #[fuchsia::test]
    fn verify_insert_logic() {
        let mut config = HashMap::new();
        let taga_servab = "[{tag: 'tag-a', service_name: 'serv-a', max_bytes: 10, \
                           min_seconds_between_fetch: 31, selectors: ['foo', 'bar']}, \
                           {tag: 'tag-a', service_name: 'serv-b', max_bytes: 20, \
                           min_seconds_between_fetch: 32, selectors: ['baz']}, ]";
        let tagb_servb = "[{tag: 'tag-b', service_name: 'serv-b', max_bytes: 30, \
                          min_seconds_between_fetch: 33, selectors: ['quux']}]";
        // Numbers not allowed in names
        let bad_tag = "[{tag: 'tag-b1', service_name: 'serv-b', max_bytes: 30, \
                       min_seconds_between_fetch: 33, selectors: ['quux']}]";
        // Underscores not allowed in names
        let bad_serv = "[{tag: 'tag-b', service_name: 'serv_b', max_bytes: 30, \
                        min_seconds_between_fetch: 33, selectors: ['quux']}]";
        let persist_aa = TaggedPersist {
            tag: "tag-a".to_string(),
            service_name: "serv-a".to_string(),
            max_bytes: 10,
            min_seconds_between_fetch: 31,
            selectors: vec!["foo".to_string(), "bar".to_string()],
        };
        let persist_ba = TaggedPersist {
            tag: "tag-a".to_string(),
            service_name: "serv-b".to_string(),
            max_bytes: 20,
            min_seconds_between_fetch: 32,
            selectors: vec!["baz".to_string()],
        };
        let persist_bb = TaggedPersist {
            tag: "tag-b".to_string(),
            service_name: "serv-b".to_string(),
            max_bytes: 30,
            min_seconds_between_fetch: 33,
            selectors: vec!["quux".to_string()],
        };

        assert!(try_insert_items(&mut config, taga_servab).is_ok());
        assert!(try_insert_items(&mut config, tagb_servb).is_ok());
        assert_eq!(config.len(), 2);
        let service_a = config.get("serv-a").unwrap();
        assert_eq!(service_a.len(), 1);
        assert_eq!(service_a.get("tag-a"), Some(&persist_aa.clone().into()));
        let service_b = config.get("serv-b").unwrap();
        assert_eq!(service_b.len(), 2);
        assert_eq!(service_b.get("tag-a"), Some(&persist_ba.clone().into()));
        assert_eq!(service_b.get("tag-b"), Some(&persist_bb.clone().into()));

        assert!(try_insert_items(&mut config, bad_tag).is_err());
        assert!(try_insert_items(&mut config, bad_serv).is_err());
        // Can't duplicate tags in the same service
        assert!(try_insert_items(&mut config, tagb_servb).is_err());
    }
}
