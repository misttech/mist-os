// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::api::query::SelectMode;
use crate::api::value::merge_map;
use crate::api::ConfigError;
use crate::environment::Environment;
use crate::nested::{nested_get, nested_remove, nested_set};
use crate::{ConfigLevel, EnvironmentContext};
use anyhow::{bail, Context, Result};
use config_macros::include_default;
use fuchsia_lockfile::Lockfile;
use futures::stream::FuturesUnordered;
use futures::StreamExt;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};
use std::fmt;
use std::fs::OpenOptions;
use std::io::{BufReader, BufWriter, ErrorKind, Read, Write};
use std::path::{Path, PathBuf};
use tracing::error;

fn format_env_variables_error(preamble: &Option<String>, values: &Vec<ConfigValue>) -> String {
    let error_list_string = values
        .iter()
        .map(|cv| {
            format!(
                "    -\"{}\" points to \"{}\", which expands to {}",
                cv.path, cv.value, cv.expansion,
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let flags_string = values
        .iter()
        .map(|cv| format!("--config {}=\"{}\"", cv.path, cv.value))
        .collect::<Vec<_>>()
        .join(" ");
    let mut error_title = concat!(
        "One or more configuration values includes an env variable. ",
        "Please expand these in the command line."
    )
    .to_string();
    if let Some(p) = preamble {
        error_title = format!("{error_title}\n{p}");
    }
    let mut msg = format!("{error_title}\n{error_list_string}\n\n");
    msg.push_str("These values can be overridden with the following flags:\n");
    msg.push_str("    ");
    msg.push_str(flags_string.as_str());
    msg
}

#[derive(Debug)]
pub struct ConfigValue {
    /// Dot-delimited path to the config value.
    pub path: String,
    /// The string value from the config.
    pub value: String,
    /// The string value from the config after env expansion.
    pub expansion: String,
}

#[derive(thiserror::Error, Debug)]
pub enum AssertNoEnvError {
    #[error("{}", format_env_variables_error(.0, .1))]
    EnvVariablesFound(Option<String>, Vec<ConfigValue>),
    #[error("critical unexpected error during no-env assert: {:?}", .0)]
    Unexpected(#[source] anyhow::Error),
}

pub trait AssertNoEnv {
    /// Looks through the entirety of the config map to find if there are any definitions that are
    /// intended to be substituted with environment variables.
    fn assert_no_env(
        &self,
        preamble: Option<String>,
        ctx: &EnvironmentContext,
    ) -> Result<(), AssertNoEnvError>;
}

/// The type of a configuration level's mapping.
pub type ConfigMap = Map<String, Value>;

impl AssertNoEnv for ConfigMap {
    fn assert_no_env(
        &self,
        preamble: Option<String>,
        ctx: &EnvironmentContext,
    ) -> Result<(), AssertNoEnvError> {
        struct KeyValue<'a> {
            key: String,
            value: &'a serde_json::Value,
        }

        let mut values =
            self.iter().map(|(k, value)| KeyValue { key: k.into(), value }).collect::<Vec<_>>();
        let mut errors = Vec::<ConfigValue>::new();
        loop {
            let Some(kv) = values.pop() else { break };
            match &kv.value {
                Value::Object(ref map) => {
                    for (k, v) in map.iter() {
                        let full_path = format!("{}.{}", kv.key, k);
                        values.push(KeyValue { key: full_path, value: v });
                    }
                }
                Value::Null | Value::Bool(_) | Value::Number(_) => {}
                val @ Value::String(ref s) => {
                    match crate::mapping::env_var::env_var_check(&ctx, val) {
                        Some(expansion) => {
                            errors.push(ConfigValue { path: kv.key, value: s.clone(), expansion })
                        }
                        None => {}
                    }
                }
                Value::Array(ref arr) => {
                    for elmnt in arr.iter() {
                        values.push(KeyValue { key: kv.key.clone(), value: elmnt })
                    }
                }
            }
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(AssertNoEnvError::EnvVariablesFound(preamble, errors))
        }
    }
}

impl AssertNoEnv for Config {
    fn assert_no_env(
        &self,
        preamble: Option<String>,
        ctx: &EnvironmentContext,
    ) -> Result<(), AssertNoEnvError> {
        self.default.assert_no_env(preamble, ctx)
    }
}

/// An individually loaded configuration file, including the path it came from
/// if it was loaded from disk.
#[derive(Debug, Clone, PartialEq)]
pub struct ConfigFile {
    path: Option<PathBuf>,
    contents: ConfigMap,
    dirty: bool,
    flush: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub(crate) default: ConfigMap,
    global: Option<ConfigFile>,
    user: Option<ConfigFile>,
    build: Option<ConfigFile>,
    runtime: ConfigMap,
}

pub(crate) struct PriorityIterator<'a> {
    curr: Option<ConfigLevel>,
    config: &'a Config,
}

impl<'a> Iterator for PriorityIterator<'a> {
    type Item = Option<&'a ConfigMap>;

    fn next(&mut self) -> Option<Self::Item> {
        use ConfigLevel::*;
        self.curr = ConfigLevel::next(self.curr);
        match self.curr {
            Some(Runtime) => Some(Some(&self.config.runtime)),
            Some(Build) => Some(self.config.build.as_ref().map(|file| &file.contents)),
            Some(User) => Some(self.config.user.as_ref().map(|file| &file.contents)),
            Some(Global) => Some(self.config.global.as_ref().map(|file| &file.contents)),
            Some(Default) => Some(Some(&self.config.default)),
            None => None,
        }
    }
}

/// Reads a JSON formatted reader permissively, returning None if for whatever reason
/// the file couldn't be read.
///
/// If the JSON is malformed, it will just get overwritten if set is ever used.
/// (TODO: Validate above assumptions)
fn read_json<T: DeserializeOwned>(file: impl Read) -> Option<T> {
    serde_json::from_reader(file).ok()
}

fn write_json<W: Write>(file: Option<W>, value: Option<&Value>) -> Result<()> {
    match (value, file) {
        (Some(v), Some(mut f)) => {
            serde_json::to_writer_pretty(&mut f, v).context("writing config file")?;
            f.flush().map_err(Into::into)
        }
        (_, _) => {
            // If either value or file are None, then return Ok(()). File being none will
            // presume the user doesn't want to save at this level.
            Ok(())
        }
    }
}

struct MaybeFlushWriter<T> {
    flush: bool,
    writer: T,
}

impl<T: Write> Write for MaybeFlushWriter<T> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.writer.write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        if self.flush {
            tracing::debug!("Flushing writer");
            let ret = self.writer.flush();
            tracing::debug!("Flushed writer");
            ret
        } else {
            tracing::debug!("Skipped flushing writer (isolate detected)");
            Ok(())
        }
    }
}

/// Atomically write to the file by creating a temporary file and passing it
/// to the closure, and atomically rename it to the destination file.
async fn with_writer<F>(path: Option<&Path>, f: F, flush: bool) -> Result<()>
where
    F: FnOnce(Option<BufWriter<&mut MaybeFlushWriter<tempfile::NamedTempFile>>>) -> Result<()>,
{
    if let Some(path) = path {
        let path = Path::new(path);
        let _lockfile = Lockfile::lock_for(path, std::time::Duration::from_secs(2)).await.map_err(|e| {
            error!("Failed to create a lockfile for {path}. Check that {lockpath} doesn't exist and can be written to. Ownership information: {owner:#?}", path=path.display(), lockpath=e.lock_path.display(), owner=e.owner);
            e
        })?;
        let parent = path.parent().unwrap_or_else(|| Path::new("."));
        let tmp = tempfile::NamedTempFile::new_in(parent)?;
        let mut writer = MaybeFlushWriter { flush, writer: tmp };
        tracing::debug!("Calling writer callback");
        f(Some(BufWriter::new(&mut writer)))?;
        tracing::debug!("Calling persist");
        writer.writer.persist(path)?;
        tracing::debug!("Persisted");

        Ok(())
    } else {
        tracing::debug!("Calling writer callback with no persist");
        let ret = f(None);
        tracing::debug!("Called writer callback");
        ret
    }
}

impl ConfigFile {
    #[cfg(test)]
    fn from_map(path: Option<PathBuf>, contents: ConfigMap) -> Self {
        Self { path, contents, dirty: false, flush: true }
    }

    fn from_buf(path: Option<PathBuf>, buffer: impl Read, flush: bool) -> Self {
        let contents = read_json(buffer)
            .as_ref()
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_else(Map::default);
        Self { path, contents, dirty: false, flush }
    }

    fn from_file(path: &Path) -> Result<Self> {
        let file = OpenOptions::new().read(true).open(path);

        match file {
            Ok(buf) => Ok(Self::from_buf(Some(path.to_owned()), BufReader::new(buf), true)),
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(Self {
                path: Some(path.to_owned()),
                contents: ConfigMap::default(),
                dirty: false,
                flush: true,
            }),
            Err(e) => Err(e.into()),
        }
    }

    fn from_nonflushing_file(path: &Path) -> Result<Self> {
        let file = OpenOptions::new().read(true).open(path);

        match file {
            Ok(buf) => Ok(Self::from_buf(Some(path.to_owned()), BufReader::new(buf), false)),
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(Self {
                path: Some(path.to_owned()),
                contents: ConfigMap::default(),
                dirty: false,
                flush: false,
            }),
            Err(e) => Err(e.into()),
        }
    }

    fn is_dirty(&self) -> bool {
        self.dirty
    }

    fn set(&mut self, key: &str, value: Value) -> Result<bool> {
        let key_vec: Vec<&str> = key.split('.').collect();
        let key = *key_vec.get(0).context("Can't set empty key")?;
        let changed = nested_set(&mut self.contents, key, &key_vec[1..], value);
        self.dirty = self.dirty || changed;
        Ok(changed)
    }

    pub fn remove(&mut self, key: &str) -> Result<()> {
        let key_vec: Vec<&str> = key.split('.').collect();
        let key = *key_vec.get(0).context(ConfigError::KeyNotFound)?;
        self.dirty = true;
        nested_remove(&mut self.contents, key, &key_vec[1..])
    }

    async fn save(&mut self) -> Result<()> {
        tracing::debug!("Saving path {:?}", self.path);

        // FIXME(81502): There is a race between the ffx CLI and the daemon service
        // in updating the config. We can lose changes if both try to change the
        // config at the same time. We can reduce the rate of races by only writing
        // to the config if the value actually changed.
        let ret = if self.is_dirty() {
            self.dirty = false;
            with_writer(
                self.path.as_deref(),
                |writer| write_json(writer, Some(&Value::Object(self.contents.clone()))),
                self.flush,
            )
            .await
        } else {
            Ok(())
        };
        tracing::debug!("Saved path {:?}", self.path);
        ret
    }
}

#[cfg(test)]
impl Default for ConfigFile {
    fn default() -> Self {
        Self::from_map(None, Map::default())
    }
}

impl Default for Config {
    fn default() -> Self {
        Self::new(None, None, None, ConfigMap::new(), ConfigMap::new())
    }
}

impl Config {
    pub(crate) fn new(
        global: Option<ConfigFile>,
        build: Option<ConfigFile>,
        user: Option<ConfigFile>,
        runtime: ConfigMap,
        default_override: ConfigMap,
    ) -> Self {
        let mut default = match include_default!() {
            Value::Object(obj) => obj,
            _ => panic!("Statically build default configuration was not an object"),
        };
        merge_map(&mut default, &default_override);

        Self { user, build, global, runtime, default }
    }

    pub(crate) fn from_env(env: &Environment) -> Result<Self> {
        let user_conf: Option<PathBuf> = env.get_user();
        let build_conf: Option<PathBuf> = env.get_build();
        let global_conf: Option<PathBuf> = env.get_global();
        let is_isolated = env.context().env_kind().is_isolated();
        if !is_isolated {
            tracing::debug!("Non isolated context {:?}", env.context().env_kind());
        }
        let from_file =
            if is_isolated { ConfigFile::from_nonflushing_file } else { ConfigFile::from_file };
        let user = user_conf.as_deref().map(from_file).transpose()?;
        let build = build_conf.as_deref().map(from_file).transpose()?;
        let global = global_conf.as_deref().map(from_file).transpose()?;

        Ok(Self::new(
            global,
            build,
            user,
            env.get_runtime_args().clone(),
            env.context().get_default_overrides(),
        ))
    }

    #[cfg(test)]
    fn write<W: Write>(&self, global: Option<W>, build: Option<W>, user: Option<W>) -> Result<()> {
        write_json(
            user,
            self.user.as_ref().map(|file| Value::Object(file.contents.clone())).as_ref(),
        )?;
        write_json(
            build,
            self.build.as_ref().map(|file| Value::Object(file.contents.clone())).as_ref(),
        )?;
        write_json(
            global,
            self.global.as_ref().map(|file| Value::Object(file.contents.clone())).as_ref(),
        )?;
        Ok(())
    }

    pub(crate) async fn save(&mut self) -> Result<()> {
        let files = [&mut self.global, &mut self.build, &mut self.user];
        // Try to save all files and only fail out if any of them fail afterwards (with the first error). This hopefully mitigates
        // any weird partial-save issues, though there's no way to eliminate them altogether (short of filesystem
        // transactions)
        FuturesUnordered::from_iter(
            files.into_iter().filter_map(|file| file.as_mut()).map(ConfigFile::save),
        )
        .fold(Ok(()), |res, i| async { res.and(i) })
        .await
    }

    pub fn get_level(&self, level: ConfigLevel) -> Option<&ConfigMap> {
        match level {
            ConfigLevel::Runtime => Some(&self.runtime),
            ConfigLevel::User => self.user.as_ref().map(|file| &file.contents),
            ConfigLevel::Build => self.build.as_ref().map(|file| &file.contents),
            ConfigLevel::Global => self.global.as_ref().map(|file| &file.contents),
            ConfigLevel::Default => Some(&self.default),
        }
    }

    pub fn get_in_level(&self, key: &str, level: ConfigLevel) -> Option<Value> {
        let key_vec: Vec<&str> = key.split('.').collect();
        nested_get(self.get_level(level), key_vec.get(0)?, &key_vec[1..]).cloned()
    }

    fn merge_object(&self, mut omap: Map<String, Value>, key: &str) -> Value {
        let key_vec: Vec<&str> = key.split('.').collect();

        for c in self.iter() {
            if let Some(Value::Object(map)) = nested_get(c, key_vec[0], &key_vec[1..]) {
                for (k, v) in map {
                    if let serde_json::map::Entry::Vacant(e) = omap.entry(k) {
                        e.insert(v.clone());
                    }
                }
            }
        }

        Value::Object(omap)
    }

    pub fn get(&self, key: &str, select: SelectMode) -> Option<Value> {
        let key_vec: Vec<&str> = key.split('.').collect();
        match select {
            SelectMode::First => {
                let res = self
                    .iter()
                    .find_map(|c| nested_get(c, *key_vec.get(0)?, &key_vec[1..]))
                    .cloned();
                if let Some(Value::Object(omap)) = res {
                    // When we are querying an object, we want the semantics to
                    // match that of querying fields within an object. I.e. if
                    // we get back an object with the same fields as if queries
                    // those individual fields. This means we need to query the
                    // all the config levels, merging on all the fields that are
                    // not shadowed by a higher-level config. Note: this merging
                    // only makes sense for objects, not for arrays. Objects
                    // are treated specially in config, by virtue of the "key"
                    // syntax: "a.b.c"; there is no equivalent array syntax
                    // ("a.b[0]"), so the semantics can stay simple.
                    Some(self.merge_object(omap, key))
                } else {
                    res
                }
            }
            SelectMode::All => {
                let result: Vec<Value> = self
                    .iter()
                    .filter_map(|c| nested_get(c, *key_vec.get(0)?, &key_vec[1..]))
                    .cloned()
                    .collect();
                if result.len() > 0 {
                    Some(Value::Array(result))
                } else {
                    None
                }
            }
        }
    }

    pub fn set(&mut self, key: &str, level: ConfigLevel, value: Value) -> Result<bool> {
        let file = self.get_level_mut(level)?;
        file.set(key, value)
    }

    pub fn remove(&mut self, key: &str, level: ConfigLevel) -> Result<()> {
        let file = self.get_level_mut(level)?;
        file.remove(key)
    }

    pub(crate) fn iter(&self) -> PriorityIterator<'_> {
        PriorityIterator { curr: None, config: self }
    }

    fn get_level_mut(&mut self, level: ConfigLevel) -> Result<&mut ConfigFile> {
        match level {
            ConfigLevel::Runtime => bail!("No mutable access to runtime level configuration"),
            ConfigLevel::User => self
                .user
                .as_mut()
                .context("Tried to write to unconfigured user level configuration"),
            ConfigLevel::Build => self
                .build
                .as_mut()
                .context("Tried to write to unconfigured build level configuration"),
            ConfigLevel::Global => self
                .global
                .as_mut()
                .context("Tried to write to unconfigured global level configuration"),
            ConfigLevel::Default => bail!("No mutable access to default level configuration"),
        }
    }
}

impl fmt::Display for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            f,
            "FFX configuration can come from several places and has an inherent priority assigned\n\
            to the different ways the configuration is gathered. A configuration key can be set\n\
            in multiple locations but the first value found is returned. The following output\n\
            shows the locations checked in descending priority order.\n"
        )?;
        let mut iterator = self.iter();
        while let Some(next) = iterator.next() {
            if let Some(level) = iterator.curr {
                match level {
                    ConfigLevel::Runtime => {
                        write!(f, "Runtime Configuration")?;
                    }
                    ConfigLevel::User => {
                        write!(f, "User Configuration")?;
                    }
                    ConfigLevel::Build => {
                        write!(f, "Build Configuration")?;
                    }
                    ConfigLevel::Global => {
                        write!(f, "Global Configuration")?;
                    }
                    ConfigLevel::Default => {
                        write!(f, "Default Configuration")?;
                    }
                };
            }
            if let Some(value) = next {
                writeln!(f, "")?;
                writeln!(f, "{}", serde_json::to_string_pretty(&value).unwrap())?;
            } else {
                writeln!(f, ": {}", "none")?;
            }
            writeln!(f, "")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::nested::RecursiveMap;
    use regex::Regex;
    use serde_json::json;
    use std::collections::HashMap;
    use tempfile::tempdir;

    const ERROR: &'static [u8] = b"0";

    const USER: &'static [u8] = br#"
        {
            "name": "User"
        }"#;

    const BUILD: &'static [u8] = br#"
        {
            "name": "Build"
        }"#;

    const GLOBAL: &'static [u8] = br#"
        {
            "name": "Global"
        }"#;

    const DEFAULT: &'static [u8] = br#"
        {
            "name": "Default"
        }"#;

    const RUNTIME: &'static [u8] = br#"
        {
            "name": "Runtime"
        }"#;

    const MAPPED: &'static [u8] = br#"
        {
            "name": "TEST_MAP"
        }"#;

    const NESTED: &'static [u8] = br#"
        {
            "name": {
               "nested": "Nested"
            }
        }"#;

    const SHALLOW: &'static [u8] = br#"
        {
            "name": {
               "nested": {
                    "shallow": "SHALLOW"
               }
            }
        }"#;

    const DEEP: &'static [u8] = br#"
        {
            "name": {
               "nested": {
                    "deep": {
                        "name": "TEST_MAP"
                    }
               }
            }
        }"#;

    const LITERAL: &'static [u8] = b"[]";

    #[test]
    fn test_persistent_build() -> Result<()> {
        let persistent_config = Config::new(
            Some(ConfigFile::from_buf(None, BufReader::new(GLOBAL), true)),
            Some(ConfigFile::from_buf(None, BufReader::new(BUILD), true)),
            Some(ConfigFile::from_buf(None, BufReader::new(USER), true)),
            Map::default(),
            Map::default(),
        );

        let value = persistent_config.get("name", SelectMode::First);
        assert!(value.is_some());
        assert_eq!(value.unwrap(), Value::String(String::from("User")));

        let mut user_file_out = String::new();
        let mut build_file_out = String::new();
        let mut global_file_out = String::new();

        unsafe {
            persistent_config.write(
                Some(BufWriter::new(global_file_out.as_mut_vec())),
                Some(BufWriter::new(build_file_out.as_mut_vec())),
                Some(BufWriter::new(user_file_out.as_mut_vec())),
            )?;
        }

        // Remove whitespace
        let mut user_file = String::from_utf8_lossy(USER).to_string();
        let mut build_file = String::from_utf8_lossy(BUILD).to_string();
        let mut global_file = String::from_utf8_lossy(GLOBAL).to_string();
        user_file.retain(|c| !c.is_whitespace());
        build_file.retain(|c| !c.is_whitespace());
        global_file.retain(|c| !c.is_whitespace());
        user_file_out.retain(|c| !c.is_whitespace());
        build_file_out.retain(|c| !c.is_whitespace());
        global_file_out.retain(|c| !c.is_whitespace());

        assert_eq!(user_file, user_file_out);
        assert_eq!(build_file, build_file_out);
        assert_eq!(global_file, global_file_out);

        Ok(())
    }

    #[test]
    fn test_priority_iterator() -> Result<()> {
        let test = Config {
            user: Some(ConfigFile::from_buf(None, BufReader::new(USER), true)),
            build: Some(ConfigFile::from_buf(None, BufReader::new(BUILD), true)),
            global: Some(ConfigFile::from_buf(None, BufReader::new(GLOBAL), true)),
            default: serde_json::from_slice(DEFAULT)?,
            runtime: serde_json::from_slice(RUNTIME)?,
        };

        let mut test_iter = test.iter();
        assert_eq!(test_iter.next(), Some(Some(&test.runtime)));
        assert_eq!(test_iter.next(), Some(test.user.as_ref().map(|file| &file.contents)));
        assert_eq!(test_iter.next(), Some(test.build.as_ref().map(|file| &file.contents)));
        assert_eq!(test_iter.next(), Some(test.global.as_ref().map(|file| &file.contents)));
        assert_eq!(test_iter.next(), Some(Some(&test.default)));
        assert_eq!(test_iter.next(), None);
        Ok(())
    }

    #[test]
    fn test_priority_iterator_with_nones() -> Result<()> {
        let test = Config {
            user: Some(ConfigFile::from_buf(None, BufReader::new(USER), true)),
            build: None,
            global: None,
            default: serde_json::from_slice(DEFAULT)?,
            runtime: ConfigMap::default(),
        };

        let mut test_iter = test.iter();
        assert_eq!(test_iter.next(), Some(Some(&test.runtime)));
        assert_eq!(test_iter.next(), Some(test.user.as_ref().map(|file| &file.contents)));
        assert_eq!(test_iter.next(), Some(test.build.as_ref().map(|file| &file.contents)));
        assert_eq!(test_iter.next(), Some(test.global.as_ref().map(|file| &file.contents)));
        assert_eq!(test_iter.next(), Some(Some(&test.default)));
        assert_eq!(test_iter.next(), None);
        Ok(())
    }

    #[test]
    fn test_get() -> Result<()> {
        let test = Config {
            user: Some(ConfigFile::from_buf(None, BufReader::new(USER), true)),
            build: Some(ConfigFile::from_buf(None, BufReader::new(BUILD), true)),
            global: Some(ConfigFile::from_buf(None, BufReader::new(GLOBAL), true)),
            default: serde_json::from_slice(DEFAULT)?,
            runtime: ConfigMap::default(),
        };

        let value = test.get("name", SelectMode::First);
        assert!(value.is_some());
        assert_eq!(value.unwrap(), Value::String(String::from("User")));

        let test_build = Config {
            user: Some(ConfigFile::from_buf(None, BufReader::new(USER), true)),
            build: None,
            global: Some(ConfigFile::from_buf(None, BufReader::new(GLOBAL), true)),
            default: serde_json::from_slice(DEFAULT)?,
            runtime: ConfigMap::default(),
        };

        let value_build = test_build.get("name", SelectMode::First);
        assert!(value_build.is_some());
        assert_eq!(value_build.unwrap(), Value::String(String::from("User")));

        let test_global = Config {
            user: None,
            build: None,
            global: Some(ConfigFile::from_buf(None, BufReader::new(GLOBAL), true)),
            default: serde_json::from_slice(DEFAULT)?,
            runtime: ConfigMap::default(),
        };

        let value_global = test_global.get("name", SelectMode::First);
        assert!(value_global.is_some());
        assert_eq!(value_global.unwrap(), Value::String(String::from("Global")));

        let test_default = Config {
            user: None,
            build: None,
            global: None,
            default: serde_json::from_slice(DEFAULT)?,
            runtime: ConfigMap::default(),
        };

        let value_default = test_default.get("name", SelectMode::First);
        assert!(value_default.is_some());
        assert_eq!(value_default.unwrap(), Value::String(String::from("Default")));

        let test_none = Config {
            user: None,
            build: None,
            global: None,
            default: ConfigMap::default(),
            runtime: ConfigMap::default(),
        };

        let value_none = test_none.get("name", SelectMode::First);
        assert!(value_none.is_none());
        Ok(())
    }

    #[test]
    fn test_set_non_map_value() -> Result<()> {
        let mut test = Config {
            user: Some(ConfigFile::from_buf(None, BufReader::new(ERROR), true)),
            build: None,
            global: None,
            default: ConfigMap::default(),
            runtime: ConfigMap::default(),
        };
        test.set("name", ConfigLevel::User, Value::String(String::from("whatever")))?;
        let value = test.get("name", SelectMode::First);
        assert_eq!(value, Some(Value::String(String::from("whatever"))));
        Ok(())
    }

    #[test]
    fn test_get_nonexistent_config() -> Result<()> {
        let test = Config {
            user: Some(ConfigFile::from_buf(None, BufReader::new(USER), true)),
            build: Some(ConfigFile::from_buf(None, BufReader::new(BUILD), true)),
            global: Some(ConfigFile::from_buf(None, BufReader::new(GLOBAL), true)),
            default: serde_json::from_slice(DEFAULT)?,
            runtime: ConfigMap::default(),
        };
        let value = test.get("field that does not exist", SelectMode::First);
        assert!(value.is_none());
        Ok(())
    }

    #[test]
    fn test_set() -> Result<()> {
        let mut test = Config {
            user: Some(ConfigFile::from_buf(None, BufReader::new(USER), true)),
            build: Some(ConfigFile::from_buf(None, BufReader::new(BUILD), true)),
            global: Some(ConfigFile::from_buf(None, BufReader::new(GLOBAL), true)),
            default: serde_json::from_slice(DEFAULT)?,
            runtime: ConfigMap::default(),
        };
        test.set("name", ConfigLevel::User, Value::String(String::from("build-test")))?;
        let value = test.get("name", SelectMode::First);
        assert!(value.is_some());
        assert_eq!(value.unwrap(), Value::String(String::from("build-test")));
        Ok(())
    }

    #[test]
    fn test_set_twice_does_not_change_config() -> Result<()> {
        let mut test = Config {
            user: Some(ConfigFile::from_buf(None, BufReader::new(USER), true)),
            build: Some(ConfigFile::from_buf(None, BufReader::new(BUILD), true)),
            global: Some(ConfigFile::from_buf(None, BufReader::new(GLOBAL), true)),
            default: serde_json::from_slice(DEFAULT)?,
            runtime: ConfigMap::default(),
        };
        assert!(test.set(
            "name1",
            ConfigLevel::Build,
            Value::String(String::from("build-test1"))
        )?);
        assert_eq!(
            test.get("name1", SelectMode::First).unwrap(),
            Value::String(String::from("build-test1"))
        );

        assert!(!test.set(
            "name1",
            ConfigLevel::Build,
            Value::String(String::from("build-test1"))
        )?);
        assert_eq!(
            test.get("name1", SelectMode::First).unwrap(),
            Value::String(String::from("build-test1"))
        );

        assert!(test.set(
            "name1",
            ConfigLevel::Build,
            Value::String(String::from("build-test2"))
        )?);
        assert_eq!(
            test.get("name1", SelectMode::First).unwrap(),
            Value::String(String::from("build-test2"))
        );

        Ok(())
    }

    #[test]
    fn test_set_build_from_none() -> Result<()> {
        let mut test = Config {
            user: Some(ConfigFile::default()),
            build: Some(ConfigFile::default()),
            global: Some(ConfigFile::default()),
            default: ConfigMap::default(),
            runtime: ConfigMap::default(),
        };
        let value_none = test.get("name", SelectMode::First);
        assert!(value_none.is_none());
        let error_set =
            test.set("name", ConfigLevel::Default, Value::String(String::from("default")));
        assert!(error_set.is_err(), "Should not be able to set default values at runtime");
        let value_default = test.get("name", SelectMode::First);
        assert!(
            value_default.is_none(),
            "Default value should be unset after failed attempt to set it"
        );
        test.set("name", ConfigLevel::Global, Value::String(String::from("global")))?;
        let value_global = test.get("name", SelectMode::First);
        assert!(value_global.is_some());
        assert_eq!(value_global.unwrap(), Value::String(String::from("global")));

        test.set("name", ConfigLevel::Build, Value::String(String::from("build")))?;
        let value_build = test.get("name", SelectMode::First);
        assert!(value_build.is_some());
        assert_eq!(value_build.unwrap(), Value::String(String::from("build")));

        test.set("name", ConfigLevel::User, Value::String(String::from("user")))?;
        let value_user = test.get("name", SelectMode::First);
        assert!(value_user.is_some());
        assert_eq!(value_user.unwrap(), Value::String(String::from("user")));
        Ok(())
    }

    #[test]
    fn test_remove() -> Result<()> {
        let mut test = Config {
            user: Some(ConfigFile::from_buf(None, BufReader::new(USER), true)),
            build: Some(ConfigFile::from_buf(None, BufReader::new(BUILD), true)),
            global: Some(ConfigFile::from_buf(None, BufReader::new(GLOBAL), true)),
            default: serde_json::from_slice(DEFAULT)?,
            runtime: ConfigMap::default(),
        };
        test.remove("name", ConfigLevel::User)?;
        let user_value = test.get("name", SelectMode::First);
        assert!(user_value.is_some());
        assert_eq!(user_value.unwrap(), Value::String(String::from("Build")));
        test.remove("name", ConfigLevel::Build)?;
        let global_value = test.get("name", SelectMode::First);
        assert!(global_value.is_some());
        assert_eq!(global_value.unwrap(), Value::String(String::from("Global")));
        test.remove("name", ConfigLevel::Global)?;
        let default_value = test.get("name", SelectMode::First);
        assert!(default_value.is_some());
        assert_eq!(default_value.unwrap(), Value::String(String::from("Default")));
        let error_removed = test.remove("name", ConfigLevel::Default);
        assert!(error_removed.is_err(), "Should not be able to remove a default value");
        let default_value = test.get("name", SelectMode::First);
        assert_eq!(
            default_value,
            Some(Value::String(String::from("Default"))),
            "value should still be default after trying to remove it (was {:?})",
            default_value
        );
        Ok(())
    }

    #[test]
    fn test_default() {
        let test = Config::new(None, None, None, Map::default(), Map::default());
        let default_value = test.get("log.enabled", SelectMode::First);
        assert_eq!(
            default_value.unwrap(),
            Value::Array(vec![Value::String("$FFX_LOG_ENABLED".to_string()), Value::Bool(true)])
        );
    }

    #[test]
    fn test_display() -> Result<()> {
        let test = Config {
            user: Some(ConfigFile::from_buf(None, BufReader::new(USER), true)),
            build: Some(ConfigFile::from_buf(None, BufReader::new(BUILD), true)),
            global: Some(ConfigFile::from_buf(None, BufReader::new(GLOBAL), true)),
            default: serde_json::from_slice(DEFAULT)?,
            runtime: ConfigMap::default(),
        };
        let output = format!("{}", test);
        assert!(output.len() > 0);
        let user_reg = Regex::new("\"name\": \"User\"").expect("test regex");
        assert_eq!(1, user_reg.find_iter(&output).count());
        let build_reg = Regex::new("\"name\": \"Build\"").expect("test regex");
        assert_eq!(1, build_reg.find_iter(&output).count());
        let global_reg = Regex::new("\"name\": \"Global\"").expect("test regex");
        assert_eq!(1, global_reg.find_iter(&output).count());
        let default_reg = Regex::new("\"name\": \"Default\"").expect("test regex");
        assert_eq!(1, default_reg.find_iter(&output).count());
        Ok(())
    }

    fn test_map(value: Value) -> Option<Value> {
        value
            .as_str()
            .map(|s| match s {
                "TEST_MAP" => Value::String("passed".to_string()),
                _ => Value::String("failed".to_string()),
            })
            .or(Some(value))
    }

    #[test]
    fn test_mapping() -> Result<()> {
        let test = Config {
            user: Some(ConfigFile::from_buf(None, BufReader::new(MAPPED), true)),
            build: None,
            global: None,
            default: ConfigMap::default(),
            runtime: ConfigMap::default(),
        };
        let test_mapping = "TEST_MAP".to_string();
        let test_passed = "passed".to_string();
        let mapped_value = test.get("name", SelectMode::First).as_ref().recursive_map(&test_map);
        assert_eq!(mapped_value, Some(Value::String(test_passed)));
        let identity_value = test.get("name", SelectMode::First);
        assert_eq!(identity_value, Some(Value::String(test_mapping)));
        Ok(())
    }

    #[test]
    fn test_nested_get() -> Result<()> {
        let test = Config {
            user: None,
            build: None,
            global: None,
            default: ConfigMap::default(),
            runtime: serde_json::from_slice(NESTED)?,
        };
        let value = test.get("name.nested", SelectMode::First);
        assert_eq!(value, Some(Value::String("Nested".to_string())));
        Ok(())
    }

    #[test]
    fn test_nested_get_should_return_sub_tree() -> Result<()> {
        let test = Config {
            user: None,
            build: None,
            global: None,
            default: serde_json::from_slice(DEFAULT)?,
            runtime: serde_json::from_slice(NESTED)?,
        };
        let value = test.get("name", SelectMode::First);
        assert_eq!(value, Some(serde_json::from_str("{\"nested\": \"Nested\"}")?));
        Ok(())
    }

    #[test]
    fn test_nested_get_should_return_full_match() -> Result<()> {
        let test = Config {
            user: None,
            build: None,
            global: None,
            default: serde_json::from_slice(NESTED)?,
            runtime: serde_json::from_slice(RUNTIME)?,
        };
        let value = test.get("name.nested", SelectMode::First);
        assert_eq!(value, Some(Value::String("Nested".to_string())));
        Ok(())
    }

    #[test]
    fn test_nested_get_should_map_values_in_sub_tree() -> Result<()> {
        let test = Config {
            user: None,
            build: None,
            global: None,
            default: serde_json::from_slice(NESTED)?,
            runtime: serde_json::from_slice(DEEP)?,
        };
        let value = test.get("name.nested", SelectMode::First).as_ref().recursive_map(&test_map);
        assert_eq!(value, Some(serde_json::from_str("{\"deep\": {\"name\": \"passed\"}}")?));
        Ok(())
    }

    #[test]
    fn test_get_should_merge_values_in_sub_tree() -> Result<()> {
        let test = Config {
            user: None,
            build: None,
            global: None,
            default: serde_json::from_slice(SHALLOW)?,
            runtime: serde_json::from_slice(DEEP)?,
        };
        let value: Option<Value> = test.get("name.nested", SelectMode::First);
        assert_eq!(
            value,
            Some(serde_json::from_str(r#"{"deep": {"name": "TEST_MAP"}, "shallow": "SHALLOW"}"#)?)
        );
        Ok(())
    }

    #[test]
    fn test_get_should_merge_overlapping_values_in_sub_tree() -> Result<()> {
        const SHALLOW2: &'static [u8] = br#"
            {
                "name": {
                   "nested": {
                        "shallow": "SHALLOW2"
                   }
                }
            }"#;

        let test = Config {
            user: None,
            build: None,
            global: None,
            default: serde_json::from_slice(SHALLOW2)?,
            runtime: serde_json::from_slice(SHALLOW)?,
        };
        let value: Option<Value> = test.get("name.nested", SelectMode::First);
        assert_eq!(value, Some(serde_json::from_str(r#"{"shallow": "SHALLOW"}"#)?));
        Ok(())
    }

    #[test]
    fn test_get_should_merge_objects_in_sub_tree() -> Result<()> {
        const OBJ1: &'static [u8] = br#"
            {
                "top": {
                   "list": ["a"]
                }
            }"#;

        const OBJ2: &'static [u8] = br#"
            {
                "top": {
                   "str": "b"
                }
            }"#;

        let test = Config {
            user: None,
            build: None,
            global: None,
            default: serde_json::from_slice(OBJ1)?,
            runtime: serde_json::from_slice(OBJ2)?,
        };
        let value: Option<Value> = test.get("top", SelectMode::First);
        assert_eq!(value, Some(serde_json::from_str(r#"{"list": ["a"], "str": "b"}"#)?));
        Ok(())
    }

    #[test]
    fn test_nested_set_from_none() -> Result<()> {
        let mut test = Config {
            user: Some(ConfigFile::default()),
            build: None,
            global: None,
            default: ConfigMap::default(),
            runtime: ConfigMap::default(),
        };
        test.set("name.nested", ConfigLevel::User, Value::Bool(false))?;
        let nested_value = test.get("name", SelectMode::First);
        assert_eq!(nested_value, Some(serde_json::from_str("{\"nested\": false}")?));
        Ok(())
    }

    #[test]
    fn test_nested_set_from_already_populated_tree() -> Result<()> {
        let mut test = Config {
            user: Some(ConfigFile::from_buf(None, BufReader::new(NESTED), true)),
            build: None,
            global: None,
            default: ConfigMap::default(),
            runtime: ConfigMap::default(),
        };
        test.set("name.updated", ConfigLevel::User, Value::Bool(true))?;
        let expected = json!({
           "nested": "Nested",
           "updated": true
        });
        let nested_value = test.get("name", SelectMode::First);
        assert_eq!(nested_value, Some(expected));
        Ok(())
    }

    #[test]
    fn test_nested_set_override_literals() -> Result<()> {
        let mut test = Config {
            user: Some(ConfigFile::from_buf(None, BufReader::new(LITERAL), true)),
            build: None,
            global: None,
            default: ConfigMap::default(),
            runtime: ConfigMap::default(),
        };
        test.set("name.updated", ConfigLevel::User, Value::Bool(true))?;
        let expected = json!({
           "updated": true
        });
        let nested_value = test.get("name", SelectMode::First);
        assert_eq!(nested_value, Some(expected));
        test.set("name.updated", ConfigLevel::User, serde_json::from_slice(NESTED)?)?;
        let nested_value = test.get("name.updated.name.nested", SelectMode::First);
        assert_eq!(nested_value, Some(Value::String(String::from("Nested"))));
        Ok(())
    }

    #[test]
    fn test_nested_remove_from_none() -> Result<()> {
        let mut test = Config {
            user: None,
            build: None,
            global: None,
            default: ConfigMap::default(),
            runtime: ConfigMap::default(),
        };
        let result = test.remove("name.nested", ConfigLevel::User);
        assert!(result.is_err());
        Ok(())
    }

    #[test]
    fn test_nested_remove_throws_error_if_key_not_found() -> Result<()> {
        let mut test = Config {
            user: Some(ConfigFile::from_buf(None, BufReader::new(NESTED), true)),
            build: None,
            global: None,
            default: ConfigMap::default(),
            runtime: ConfigMap::default(),
        };
        let result = test.remove("name.unknown", ConfigLevel::User);
        assert!(result.is_err());
        Ok(())
    }

    #[test]
    fn test_nested_remove_deletes_literals() -> Result<()> {
        let mut test = Config {
            user: Some(ConfigFile::from_buf(None, BufReader::new(DEEP), true)),
            build: None,
            global: None,
            default: ConfigMap::default(),
            runtime: ConfigMap::default(),
        };
        test.remove("name.nested.deep.name", ConfigLevel::User)?;
        let value = test.get("name", SelectMode::First);
        assert_eq!(value, None);
        Ok(())
    }

    #[test]
    fn test_nested_remove_deletes_subtrees() -> Result<()> {
        let mut test = Config {
            user: Some(ConfigFile::from_buf(None, BufReader::new(DEEP), true)),
            build: None,
            global: None,
            default: ConfigMap::default(),
            runtime: ConfigMap::default(),
        };
        test.remove("name.nested", ConfigLevel::User)?;
        let value = test.get("name", SelectMode::First);
        assert_eq!(value, None);
        Ok(())
    }

    #[test]
    fn test_additive_mode() -> Result<()> {
        let test = Config {
            user: Some(ConfigFile::from_buf(None, BufReader::new(USER), true)),
            build: Some(ConfigFile::from_buf(None, BufReader::new(BUILD), true)),
            global: Some(ConfigFile::from_buf(None, BufReader::new(GLOBAL), true)),
            default: serde_json::from_slice(DEFAULT)?,
            runtime: serde_json::from_slice(RUNTIME)?,
        };
        let value = test.get("name", SelectMode::All);
        match value {
            Some(Value::Array(v)) => {
                assert_eq!(v.len(), 5);
                let mut v = v.into_iter();
                assert_eq!(v.next(), Some(Value::String("Runtime".to_string())));
                assert_eq!(v.next(), Some(Value::String("User".to_string())));
                assert_eq!(v.next(), Some(Value::String("Build".to_string())));
                assert_eq!(v.next(), Some(Value::String("Global".to_string())));
                assert_eq!(v.next(), Some(Value::String("Default".to_string())));
            }
            _ => anyhow::bail!("additive mode should return a Value::Array full of all values."),
        }
        Ok(())
    }

    #[test]
    fn test_config_error_report() {
        // Build the following:
        //
        // {
        //   "foo": "$TEST_ENV_VAR_OF_SOME_KIND",  // expands to "whatever"
        //   "inner_map": {
        //     "bar": true,
        //     "baz": "$TEST_ENV_VAR_OF_SOME_KIND",  // expands to "whatever"
        //     "leaf": {
        //       "last": "$TEST_ENV_VAR_TWOOO",     // expands to "whomever"
        //       "last_other": ["$NONEXISTENT_VAR", "blah"],
        //     }
        //   }
        // }
        static TEST_ENV_VAR1: &'static str = "TEST_ENV_VAR_OF_SOME_KIND";
        static TEST_ENV_VAR1_VALUE: &'static str = "whatever";
        static TEST_ENV_VAR2: &'static str = "TEST_ENV_VAR_TWOOO";
        static TEST_ENV_VAR2_VALUE: &'static str = "whomever";
        let mut config_map = ConfigMap::new();
        config_map.insert("foo".to_owned(), Value::String(format!("${TEST_ENV_VAR1}")));
        let mut inner_map = ConfigMap::new();
        inner_map.insert("bar".to_owned(), Value::Bool(true));
        // Escaped sequence here.
        inner_map.insert("baz".to_owned(), Value::String(format!("${TEST_ENV_VAR1}")));
        let mut map_leaf = ConfigMap::new();
        map_leaf.insert("last".to_owned(), Value::String(format!("${TEST_ENV_VAR2}")));
        map_leaf.insert(
            "last_other".to_owned(),
            Value::Array(vec![
                Value::String("$NONEXISTENT_VAR".to_owned()),
                Value::String("blah".to_owned()),
            ]),
        );
        inner_map.insert("leaf".to_owned(), Value::Object(map_leaf));
        config_map.insert("inner_map".to_owned(), Value::Object(inner_map));

        // Now that we have the map set up, let's do the actual testing.
        let isolate_dir = tempdir().expect("tempdir");
        let mut env_vars = HashMap::new();
        env_vars.insert(TEST_ENV_VAR1.to_string(), TEST_ENV_VAR1_VALUE.to_string());
        env_vars.insert(TEST_ENV_VAR2.to_string(), TEST_ENV_VAR2_VALUE.to_string());
        let context = EnvironmentContext::isolated(
            crate::environment::ExecutableKind::Test,
            isolate_dir.path().to_owned(),
            env_vars,
            Default::default(),
            None,
            None,
            true,
        )
        .expect("env context creation");
        let result = config_map.assert_no_env(None, &context);
        assert!(result.is_err());
        let err = result.unwrap_err();
        let AssertNoEnvError::EnvVariablesFound(None, config_values) = err else {
            panic!("wrong error type: {err:?}");
        };
        assert!(
            config_values.iter().any(|cv| cv.path.as_str() == "inner_map.leaf.last"
                && cv.value == format!("${TEST_ENV_VAR2}")
                && cv.expansion.replace("\"", "") == TEST_ENV_VAR2_VALUE.to_owned()),
            "config error not found in {config_values:?}"
        );
        assert!(
            config_values.iter().any(|cv| cv.path.as_str() == "inner_map.baz"
                && cv.value == format!("${TEST_ENV_VAR1}")
                && cv.expansion.replace("\"", "") == TEST_ENV_VAR1_VALUE.to_owned()),
            "config error not found in {config_values:?}"
        );
        assert!(
            config_values.iter().any(|cv| cv.path.as_str() == "foo"
                && cv.value == format!("${TEST_ENV_VAR1}")
                && cv.expansion.replace("\"", "") == TEST_ENV_VAR1_VALUE.to_owned()),
            "config error not found in {config_values:?}"
        );
    }
}
