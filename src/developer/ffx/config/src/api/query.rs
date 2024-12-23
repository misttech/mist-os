// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::api::ConfigResult;
use crate::mapping::env_var::env_var_strict;
use crate::nested::RecursiveMap;
use crate::{
    validate_type, ConfigError, ConfigLevel, Environment, EnvironmentContext, ValueStrategy,
};
use anyhow::{anyhow, bail, Context, Result};
use serde_json::Value;
use std::default::Default;
use tracing::debug;

use super::value::TryConvert;
use super::ConfigValue;

#[derive(Debug, PartialEq, Copy, Clone)]
pub enum SelectMode {
    First,
    All,
}

impl Default for SelectMode {
    fn default() -> Self {
        SelectMode::First
    }
}

#[derive(Debug, Default, Clone)]
pub struct ConfigQuery<'a> {
    pub name: Option<&'a str>,
    pub level: Option<ConfigLevel>,
    pub select: SelectMode,
    pub ctx: Option<&'a EnvironmentContext>,
}

impl<'a> ConfigQuery<'a> {
    pub fn new(
        name: Option<&'a str>,
        level: Option<ConfigLevel>,
        select: SelectMode,
        ctx: Option<&'a EnvironmentContext>,
    ) -> Self {
        Self { ctx, name, level, select }
    }

    /// Adds the given name to the query and returns a new composed query.
    pub fn name(self, name: Option<&'a str>) -> Self {
        Self { name, ..self }
    }
    /// Adds the given level to the query and returns a new composed query.
    pub fn level(self, level: Option<ConfigLevel>) -> Self {
        Self { level, ..self }
    }
    /// Adds the given select mode to the query and returns a new composed query.
    pub fn select(self, select: SelectMode) -> Self {
        Self { select, ..self }
    }
    /// Use the given environment context instead of the global one and returns
    /// a new composed query.
    pub fn context(self, ctx: Option<&'a EnvironmentContext>) -> Self {
        Self { ctx, ..self }
    }

    fn get_env_context(&self) -> Result<EnvironmentContext> {
        match self.ctx {
            Some(ctx) => Ok(ctx.clone()),
            None => crate::global_env_context().context("No configured global environment"),
        }
    }

    async fn get_env(&self) -> Result<Environment> {
        match self.ctx {
            Some(ctx) => ctx.load(),
            None => crate::global_env().context("No configured global environment"),
        }
    }

    fn get_config(&self, env: Environment) -> ConfigResult {
        debug!("{self}");
        let config = env.config_from_cache()?;
        let read_guard = config.read().map_err(|_| anyhow!("config read guard"))?;
        let result = match self {
            Self { name: Some(name), level: None, select, .. } => read_guard.get(*name, *select),
            Self { name: Some(name), level: Some(level), .. } => {
                read_guard.get_in_level(*name, *level)
            }
            Self { name: None, level: Some(level), .. } => {
                read_guard.get_level(*level).cloned().map(Value::Object)
            }
            _ => bail!("Invalid query: {self:?}"),
        };
        Ok(result.into())
    }

    /// Get a value with as little processing as possible
    pub fn get_raw<T>(&self) -> Result<T, ConfigError>
    where
        T: TryConvert + ValueStrategy,
    {
        let ctx = self.get_env_context().map_err(|e| ConfigError::new(e))?;
        T::validate_query(self)?;
        let cv = self
            .get_config(ctx.load().map_err(|e| ConfigError::new(e))?)
            .map_err(|e| ConfigError::new(e))?
            .recursive_map(&validate_type::<T>);
        T::try_convert(cv)
    }

    /// Get an optional value, ignoring "BadKey" errors, which are only generated when in strict
    /// mode. Used to let callers choose not to report errors due to bad mappings.
    pub fn get_optional<T>(&self) -> Result<T, ConfigError>
    where
        T: TryConvert + ValueStrategy,
    {
        self.get().or_else(|e| {
            if matches!(e, ConfigError::BadValue { .. }) {
                T::try_convert(ConfigValue(None))
            } else {
                Err(e)
            }
        })
    }

    /// Get a value with the normal processing of substitution strings
    pub fn get<T>(&self) -> Result<T, ConfigError>
    where
        T: TryConvert + ValueStrategy,
    {
        use crate::mapping::*;

        let ctx = self.get_env_context().map_err(|e| ConfigError::new(e))?;
        T::validate_query(self)?;

        // The use of `is_strict()` here is not ideal, because we'd like to have strict-specific
        // library inside the subtool boundary. But when we change to read-only config, this code
        // will all change: we'll build a single ConfigMap before invoking the subtool, rather than
        // doing substitutions and layers at query time.
        if ctx.is_strict() {
            // If we are going to fail to a reference to an env var, it's important that we
            // know which one. Threading the failure through the ConfigValue apparatus is quite
            // difficult, so for now, let's have an explicit check. Unfortunately, we need to
            // do all the other mappings first, since they _all_ look like env vars ("$BUILD_DIR", etc)
            let cv = self
                .get_config(ctx.load().map_err(|e| ConfigError::new(e))?)
                .map_err(|e| ConfigError::new(e))?
                .recursive_map(&|val| build(&ctx, val))
                .recursive_map(&|val| workspace(&ctx, val));
            let cv = if let Some(ref v) = cv.0 {
                // We want recursive mapping here, so that arrays that contain
                // env variables get handled correctly.
                let ev_res = cv.clone().recursive_map(&|val| env_var_strict(val));
                if ev_res.0.is_none() {
                    // Conveniently, this message will make sense for config
                    // mappings that we are ignoring because they are based on
                    // home: $CACHE, etc. Since they all look like environment
                    // variables, they will cause the env_var_strict() check
                    // to fail
                    return Err(ConfigError::BadValue { value: v.clone(), reason: format!(
                        "The value for {} contains a variable mapping, which is ignored in strict mode",
                        self.name.unwrap(),
                    )});
                }
                ev_res
            } else {
                cv
            };
            // The problem is not with an env variable; keep going
            let cv = cv.recursive_map(&T::handle_arrays);
            let cv = cv.recursive_map(&validate_type::<T>);
            T::try_convert(cv)
        } else {
            let cv = self
                .get_config(ctx.load().map_err(|e| ConfigError::new(e))?)
                .map_err(|e| ConfigError::new(e))?
                .recursive_map(&|val| runtime(&ctx, val))
                .recursive_map(&|val| cache(&ctx, val))
                .recursive_map(&|val| data(&ctx, val))
                .recursive_map(&|val| shared_data(&ctx, val))
                .recursive_map(&|val| config(&ctx, val))
                .recursive_map(&|val| home(&ctx, val))
                .recursive_map(&|val| build(&ctx, val))
                .recursive_map(&|val| workspace(&ctx, val))
                .recursive_map(&|val| env_var(&ctx, val))
                .recursive_map(&T::handle_arrays)
                .recursive_map(&validate_type::<T>);
            T::try_convert(cv)
        }
    }

    /// Get a value with normal processing, but verifying that it's a file that exists.
    pub async fn get_file<T>(&self) -> Result<T, ConfigError>
    where
        T: TryConvert + ValueStrategy,
    {
        use crate::mapping::*;

        let ctx = self.get_env_context().map_err(|e| ConfigError::new(e))?;
        T::validate_query(self)?;
        // See comments re strict checking in get() above
        if ctx.is_strict() {
            let cv = self
                .get_config(ctx.load().map_err(|e| ConfigError::new(e))?)
                .map_err(|e| ConfigError::new(e))?
                .recursive_map(&|val| build(&ctx, val))
                .recursive_map(&|val| workspace(&ctx, val));
            let cv = if let Some(ref v) = cv.0 {
                let ev_res = cv.clone().recursive_map(&|val| env_var_strict(val));
                if ev_res.0.is_none() {
                    return Err(ConfigError::BadValue{ value: v.clone(), reason: format!(
                        "The value for {} contains a variable mapping, which is ignored in strict mode",
                        self.name.unwrap(),
                    )});
                }
                ev_res
            } else {
                cv
            };
            // The problem is not with an env variable; keep going
            let cv = cv.recursive_map(&T::handle_arrays).recursive_map(&file_check);
            T::try_convert(cv)
        } else {
            let cv = self
                .get_config(ctx.load().map_err(|e| ConfigError::new(e))?)
                .map_err(|e| ConfigError::new(e))?
                .recursive_map(&|val| runtime(&ctx, val))
                .recursive_map(&|val| cache(&ctx, val))
                .recursive_map(&|val| data(&ctx, val))
                .recursive_map(&|val| config(&ctx, val))
                .recursive_map(&|val| home(&ctx, val))
                .recursive_map(&|val| build(&ctx, val))
                .recursive_map(&|val| workspace(&ctx, val))
                .recursive_map(&|val| env_var(&ctx, val))
                .recursive_map(&T::handle_arrays)
                .recursive_map(&file_check);
            T::try_convert(cv)
        }
    }

    fn validate_write_query(&self) -> Result<(&str, ConfigLevel)> {
        match self {
            ConfigQuery { name: None, .. } => {
                bail!("Name of configuration is required to write to a value")
            }
            ConfigQuery { level: None, .. } => {
                bail!("Level of configuration is required to write to a value")
            }
            ConfigQuery { level: Some(level), .. } if level == &ConfigLevel::Default => {
                bail!("Cannot override defaults")
            }
            ConfigQuery { name: Some(key), level: Some(level), .. } => Ok((*key, *level)),
        }
    }

    /// Set the queried location to the given Value.
    pub async fn set(&self, value: Value) -> Result<()> {
        tracing::debug!("Setting config value");
        let (key, level) = self.validate_write_query()?;
        let mut env = self.get_env().await?;
        tracing::debug!("Config set got environment");
        env.populate_defaults(&level).await?;
        tracing::debug!("Config set defaults populated");
        let config = env.config_from_cache()?;
        tracing::debug!("Config set got value from cache");
        let mut write_guard = config.write().map_err(|_| anyhow!("config write guard"))?;
        tracing::debug!("Config set got write guard");
        write_guard.set(key, level, value)?;
        tracing::debug!("Config set performed");
        write_guard.save().await?;
        tracing::debug!("Config set saved");
        Ok(())
    }

    /// Remove the value at the queried location.
    pub async fn remove(&self) -> Result<()> {
        let (key, level) = self.validate_write_query()?;
        let env = self.get_env().await?;
        let config = env.config_from_cache()?;
        let mut write_guard = config.write().map_err(|_| anyhow!("config write guard"))?;
        write_guard.remove(key, level)?;
        write_guard.save().await
    }

    /// Add this value at the queried location as an array item, converting the location to an array
    /// if necessary.
    pub async fn add(&self, value: Value) -> Result<()> {
        let (key, level) = self.validate_write_query()?;
        let mut env = self.get_env().await?;
        env.populate_defaults(&level).await?;
        let config = env.config_from_cache()?;
        let mut write_guard = config.write().map_err(|_| anyhow!("config write guard"))?;
        if let Some(mut current) = write_guard.get_in_level(key, level) {
            if current.is_object() {
                bail!("cannot add a value to a subtree");
            } else {
                match current.as_array_mut() {
                    Some(v) => {
                        v.push(value);
                        write_guard.set(key, level, Value::Array(v.to_vec()))?
                    }
                    None => write_guard.set(key, level, Value::Array(vec![current, value]))?,
                }
            }
        } else {
            write_guard.set(key, level, value)?
        };

        write_guard.save().await
    }
}

impl<'a> std::fmt::Display for ConfigQuery<'a> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let Self { name, level, select, .. } = self;
        let mut sep = "";
        if let Some(name) = name {
            write!(f, "{sep}key='{name}'")?;
            sep = ", ";
        }
        if let Some(level) = level {
            write!(f, "{sep}level={level:?}")?;
            sep = ", ";
        }
        write!(f, "{sep}select={select:?}")
    }
}

impl<'a> From<&'a str> for ConfigQuery<'a> {
    fn from(value: &'a str) -> Self {
        let name = Some(value);
        ConfigQuery { name, ..Default::default() }
    }
}

impl<'a> From<&'a String> for ConfigQuery<'a> {
    fn from(value: &'a String) -> Self {
        let name = Some(value.as_str());
        ConfigQuery { name, ..Default::default() }
    }
}

impl<'a> From<ConfigLevel> for ConfigQuery<'a> {
    fn from(value: ConfigLevel) -> Self {
        let level = Some(value);
        ConfigQuery { level, ..Default::default() }
    }
}
