// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    api::{value::ValueStrategy, ConfigError, ConfigValue},
    storage::Config,
    BuildOverride, ConfigMap, ConfigQuery, Environment,
};
use anyhow::{Context, Result};
use errors::ffx_error;
use ffx_config_domain::ConfigDomain;
use sdk::{Sdk, SdkRoot};
use std::{
    collections::HashMap,
    fs::File,
    io::Read,
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
};
use thiserror::Error;
use tracing::error;

use super::{EnvironmentKind, ExecutableKind};

/// A name for the type used as an environment variable mapping for isolation override
type EnvVars = HashMap<String, String>;

const SDK_NOT_FOUND_HELP: &str = "\
SDK directory could not be found. Please set with
`ffx sdk set root <PATH_TO_SDK_DIR>`\n
If you are developing in the fuchsia tree, ensure \
that you are running the `ffx` command (in $FUCHSIA_DIR/.jiri_root) or `fx ffx`, not a built binary.
Running the binary directly is not supported in the fuchsia tree.\n\n";

/// Contextual information about where this instance of ffx is running
#[derive(Clone, Debug)]
pub struct EnvironmentContext {
    kind: EnvironmentKind,
    exe_kind: ExecutableKind,
    env_vars: Option<EnvVars>,
    pub(crate) runtime_args: ConfigMap,
    env_file_path: Option<PathBuf>,
    pub(crate) cache: Arc<crate::cache::Cache<Config>>,
}

impl std::cmp::PartialEq for EnvironmentContext {
    fn eq(&self, other: &Self) -> bool {
        self.kind == other.kind
            && self.exe_kind == other.exe_kind
            && self.env_vars == other.env_vars
            && self.runtime_args == other.runtime_args
            && self.env_file_path == other.env_file_path
    }
}

#[derive(Error, Debug)]
pub enum EnvironmentDetectError {
    #[error("Error reading metadata or data from the filesystem")]
    FileSystem(#[from] std::io::Error),
    #[error("Invalid path, not utf8-safe")]
    Path(#[from] camino::FromPathError),
    #[error("Error in config domain environment file")]
    ConfigDomain(#[from] ffx_config_domain::FileError),
}

impl EnvironmentContext {
    /// Initializes a new environment type with the given kind and runtime arguments.
    pub(crate) fn new(
        kind: EnvironmentKind,
        exe_kind: ExecutableKind,
        env_vars: Option<EnvVars>,
        runtime_args: ConfigMap,
        env_file_path: Option<PathBuf>,
    ) -> Self {
        let cache = Arc::default();
        Self { kind, exe_kind, env_vars, runtime_args, env_file_path, cache }
    }

    /// Initialize an environment type for a config domain context, with a
    /// `fuchsia_env` file at its root.
    pub fn config_domain(
        exe_kind: ExecutableKind,
        env_file: ConfigDomain,
        runtime_args: ConfigMap,
    ) -> Self {
        Self::new(EnvironmentKind::ConfigDomain(env_file), exe_kind, None, runtime_args, None)
    }

    /// Initialize an environment type for an in tree context, rooted at `tree_root` and if
    /// a build directory is currently set at `build_dir`.
    pub fn in_tree(
        exe_kind: ExecutableKind,
        tree_root: PathBuf,
        build_dir: Option<PathBuf>,
        runtime_args: ConfigMap,
        env_file_path: Option<PathBuf>,
    ) -> Self {
        Self::new(
            EnvironmentKind::InTree { tree_root, build_dir },
            exe_kind,
            None,
            runtime_args,
            env_file_path,
        )
    }

    /// Initialize an environment with an isolated root under which things should be stored/used/run.
    pub fn isolated(
        exe_kind: ExecutableKind,
        isolate_root: PathBuf,
        env_vars: EnvVars,
        runtime_args: ConfigMap,
        env_file_path: Option<PathBuf>,
    ) -> Self {
        Self::new(
            EnvironmentKind::Isolated { isolate_root },
            exe_kind,
            Some(env_vars),
            runtime_args,
            env_file_path,
        )
    }

    /// Initialize an environment type that has no meaningful context, using only global and
    /// user level configuration.
    pub fn no_context(
        exe_kind: ExecutableKind,
        runtime_args: ConfigMap,
        env_file_path: Option<PathBuf>,
    ) -> Self {
        Self::new(EnvironmentKind::NoContext, exe_kind, None, runtime_args, env_file_path)
    }

    /// Detects what kind of environment we're in, based on the provided arguments,
    /// and returns the context found. If None is given for `env_file_path`, the default for
    /// the kind of environment will be used. Note that this will never automatically detect
    /// an isolated environment, that has to be chosen explicitly.
    pub fn detect(
        exe_kind: ExecutableKind,
        runtime_args: ConfigMap,
        current_dir: &Path,
        env_file_path: Option<PathBuf>,
    ) -> Result<Self, EnvironmentDetectError> {
        // strong signals that we're running...
        if let Some(env_file_path) = ConfigDomain::find_root(current_dir.try_into()?) {
            // - a config-domain: we found a fuchsia-env file
            let env_file = ConfigDomain::load_from(&env_file_path)?;
            Ok(Self::config_domain(exe_kind, env_file, runtime_args))
        } else if let Some(tree_root) = Self::find_jiri_root(current_dir)? {
            // - in-tree: we found a jiri root, and...
            // look for a .fx-build-dir file and use that instead.
            let build_dir = Self::load_fx_build_dir(&tree_root)?;

            Ok(Self::in_tree(exe_kind, tree_root, build_dir, runtime_args, env_file_path))
        } else {
            // - no particular context: any other situation
            Ok(Self::no_context(exe_kind, runtime_args, env_file_path))
        }
    }

    pub fn exe_kind(&self) -> ExecutableKind {
        self.exe_kind
    }

    pub async fn analytics_enabled(&self) -> bool {
        use EnvironmentKind::*;
        if let Isolated { .. } = self.kind {
            false
        } else {
            // note: double negative to turn this into an affirmative
            !self.get("ffx.analytics.disabled").await.unwrap_or(false)
        }
    }

    pub fn env_file_path(&self) -> Result<PathBuf> {
        match &self.env_file_path {
            Some(path) => Ok(path.clone()),
            None => Ok(self.get_default_env_path()?),
        }
    }

    /// Returns the path to the currently active build output directory
    pub fn build_dir(&self) -> Option<&Path> {
        match &self.kind {
            EnvironmentKind::InTree { build_dir, .. } => build_dir.as_deref(),
            EnvironmentKind::ConfigDomain(domain) => Some(domain.get_build_dir()?.as_std_path()),
            _ => None,
        }
    }

    /// Returns version info about the running ffx binary
    pub fn build_info(&self) -> ffx_build_version::VersionInfo {
        ffx_build_version::build_info()
    }

    /// Returns a unique identifier denoting the version of the daemon binary.
    pub fn daemon_version_string(&self) -> Result<String> {
        buildid::get_build_id().map_err(Into::into)
    }

    pub fn env_kind(&self) -> &EnvironmentKind {
        &self.kind
    }

    pub async fn load(&self) -> Result<Environment> {
        Environment::load(self).await
    }

    /// Gets an environment variable, either from the system environment or from the isolation-configured
    /// environment.
    pub fn env_var(&self, name: &str) -> Result<String, std::env::VarError> {
        match &self.env_vars {
            Some(env_vars) => env_vars.get(name).cloned().ok_or(std::env::VarError::NotPresent),
            _ => std::env::var(name),
        }
    }

    /// Creates a [`ConfigQuery`] against the global config cache and
    /// this environment.
    ///
    /// Example:
    ///
    /// ```no_run
    /// use ffx_config::ConfigLevel;
    /// use ffx_config::BuildSelect;
    /// use ffx_config::SelectMode;
    ///
    /// let ctx = EnvironmentContext::default();
    /// let query = ctx.build()
    ///     .name("testing")
    ///     .level(Some(ConfigLevel::Build))
    ///     .build(Some(BuildSelect::Path("/tmp/build.json")))
    ///     .select(SelectMode::All);
    /// let value = query.get().await?;
    /// ```
    pub fn build<'a>(&'a self) -> ConfigQuery<'a> {
        ConfigQuery::default().context(Some(self))
    }

    /// Creates a [`ConfigQuery`] against the global config cache and this
    /// environment, using the provided value converted in to a base query.
    ///
    /// Example:
    ///
    /// ```no_run
    /// let ctx = EnvironmentContext::default();
    /// ctx.query("a_key").get();
    /// ctx.query(ffx_config::ConfigLevel::User).get();
    /// ```
    pub fn query<'a>(&'a self, with: impl Into<ConfigQuery<'a>>) -> ConfigQuery<'a> {
        with.into().context(Some(self))
    }

    /// A shorthand for the very common case of querying a value from the global config
    /// cache and this environment, using the provided value converted into a query.
    pub async fn get<'a, T, U>(&'a self, with: U) -> std::result::Result<T, T::Error>
    where
        T: TryFrom<ConfigValue> + ValueStrategy,
        <T as std::convert::TryFrom<ConfigValue>>::Error: std::convert::From<ConfigError>,
        U: Into<ConfigQuery<'a>>,
    {
        self.query(with).get().await
    }

    /// Find the appropriate sdk root for this invocation of ffx, looking at configuration
    /// values and the current environment context to determine the correct place to find it.
    pub async fn get_sdk_root(&self) -> Result<SdkRoot> {
        // some in-tree tooling directly overrides sdk.root. But if that's not done, the 'root' is just the
        // build directory.
        // Out of tree, we will always want to pull the config from the normal config path, which
        // we can defer to the SdkRoot's mechanisms for.
        let runtime_root: Option<PathBuf> =
            self.query("sdk.root").build(Some(BuildOverride::NoBuild)).get().await.ok();

        match (&self.kind, runtime_root) {
            (EnvironmentKind::ConfigDomain(domain), None) => {
                self.sdk_from_config(domain.get_explicit_sdk_root().map(|p| p.as_std_path())).await
            }
            (EnvironmentKind::InTree { build_dir: Some(build_dir), .. }, None) => {
                let manifest = build_dir.clone();
                let module = self.query("sdk.module").get().await.ok();
                match module {
                    Some(module) => Ok(SdkRoot::Modular { manifest, module }),
                    None => Ok(SdkRoot::Full(manifest)),
                }
            }
            (_, runtime_root) => self.sdk_from_config(runtime_root.as_deref()).await,
        }
    }

    /// Load the sdk configured for this environment context
    pub async fn get_sdk(&self) -> Result<Sdk> {
        self.get_sdk_root().await?.get_sdk()
    }

    /// The environment variable we search for
    pub const FFX_BIN_ENV: &str = "FFX_BIN";
    /// Gets the path to the top level binary for use when re-running ffx.
    ///
    /// - This will first check the environment variable in [`Self::FFX_BIN_ENV`],
    /// which should be set by a top level ffx invocation if we were run by one.
    /// - If that isn't set, it will use the current executable if this
    /// context's `ExecutableType` is MainFfx.
    /// - If neither of those are found, and an sdk is configured, search the
    /// sdk manifest for the ffx host-tool entry and use that.
    pub async fn rerun_bin(&self) -> Result<PathBuf, anyhow::Error> {
        if let Some(bin_from_env) = self.env_var(Self::FFX_BIN_ENV).ok() {
            return Ok(bin_from_env.into());
        }

        if let ExecutableKind::MainFfx = self.exe_kind {
            return Ok(std::env::current_exe()?);
        }

        let sdk = self.get_sdk().await.with_context(|| {
            ffx_error!("Unable to load SDK while searching for the 'main' ffx binary")
        })?;
        sdk.get_host_tool("ffx")
            .with_context(|| ffx_error!("Unable to find the 'main' ffx binary in the loaded SDK"))
    }

    /// Creates a command builder that starts with everything necessary to re-run ffx within the same context,
    /// without any subcommands.
    pub async fn rerun_prefix(&self) -> Result<Command, anyhow::Error> {
        // we may have been run by a wrapper script, so we want to make sure we're using the 'real' executable.
        let mut ffx_path = self.rerun_bin().await?;
        // if we daemonize, our path will change to /, so get the canonical path before that occurs.
        ffx_path = std::fs::canonicalize(ffx_path)?;

        let mut cmd = Command::new(&ffx_path);
        match &self.kind {
            EnvironmentKind::Isolated { isolate_root } => {
                cmd.arg("--isolate-dir").arg(isolate_root);

                // for isolation we're always going to clear the environment,
                // because it's better to fail than poison the isolation with something
                // external.
                // But an isolated context without an env var hash shouldn't be
                // constructable anyways.
                cmd.env_clear();
                if let Some(env_vars) = &self.env_vars {
                    for (k, v) in env_vars {
                        cmd.env(k, v);
                    }
                }
            }
            _ => {}
        }
        cmd.env(Self::FFX_BIN_ENV, &ffx_path);
        cmd.arg("--config").arg(serde_json::to_string(&self.runtime_args)?);
        if let Some(e) = self.env_file_path.as_ref() {
            cmd.arg("--env").arg(e);
        }
        Ok(cmd)
    }

    /// Searches for the .jiri_root that should be at the top of the tree. Returns
    /// Ok(Some(parent_of_jiri_root)) if one is found.
    fn find_jiri_root(from: &Path) -> Result<Option<PathBuf>, EnvironmentDetectError> {
        let mut from = Some(std::fs::canonicalize(from)?);
        while let Some(next) = from {
            let jiri_path = next.join(".jiri_root");
            if jiri_path.is_dir() {
                return Ok(Some(next));
            } else {
                from = next.parent().map(Path::to_owned);
            }
        }
        Ok(None)
    }

    /// Looks for an fx-configured .fx-build-dir file in the tree root and returns the path
    /// presented there if the directory exists.
    fn load_fx_build_dir(from: &Path) -> Result<Option<PathBuf>, EnvironmentDetectError> {
        let build_dir_file = from.join(".fx-build-dir");
        if build_dir_file.is_file() {
            let mut dir = String::default();
            File::open(build_dir_file)?.read_to_string(&mut dir)?;
            Ok(from.join(dir.trim()).canonicalize().ok())
        } else {
            Ok(None)
        }
    }

    /// Gets the basic information about the sdk as configured, without diving deeper into the sdk's own configuration.
    async fn sdk_from_config(&self, sdk_root: Option<&Path>) -> Result<SdkRoot> {
        // All gets in this function should declare that they don't want the build directory searched, because
        // if there is a build directory it *is* generally the sdk.
        let manifest = match sdk_root {
            Some(root) => root.to_owned(),
            _ => {
                let path = std::env::current_exe().map_err(|e| {
                    errors::ffx_error!(
                        "{}Error was: failed to get current ffx exe path for SDK root: {:?}",
                        SDK_NOT_FOUND_HELP,
                        e
                    )
                })?;

                match find_sdk_root(&path) {
                    Ok(Some(root)) => root,
                    Ok(None) => {
                        errors::ffx_bail!(
                            "{}Could not find an SDK manifest in any parent of ffx's directory.",
                            SDK_NOT_FOUND_HELP,
                        );
                    }
                    Err(e) => {
                        errors::ffx_bail!("{}Error was: {:?}", SDK_NOT_FOUND_HELP, e);
                    }
                }
            }
        };
        let module = self.query("sdk.module").build(Some(BuildOverride::NoBuild)).get().await.ok();
        match module {
            Some(module) => Ok(SdkRoot::Modular { manifest, module }),
            _ => Ok(SdkRoot::Full(manifest)),
        }
    }
}

fn find_sdk_root(start_path: &Path) -> Result<Option<PathBuf>> {
    let mut path = std::fs::canonicalize(start_path)
        .context(format!("canonicalizing ffx path {:?}", start_path))?;

    loop {
        path = if let Some(parent) = path.parent() {
            parent.to_path_buf()
        } else {
            return Ok(None);
        };

        if SdkRoot::is_sdk_root(&path) {
            return Ok(Some(path));
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;

    use tempfile::tempdir;

    impl Default for EnvironmentContext {
        fn default() -> Self {
            Self {
                kind: EnvironmentKind::NoContext,
                exe_kind: ExecutableKind::Test,
                env_vars: Default::default(),
                runtime_args: Default::default(),
                env_file_path: Default::default(),
                cache: Default::default(),
            }
        }
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_find_sdk_root_finds_root() {
        let temp = tempdir().unwrap();
        let temp_path = std::fs::canonicalize(temp.path()).expect("canonical temp path");

        let start_path = temp_path.join("test1").join("test2");
        std::fs::create_dir_all(start_path.clone()).unwrap();

        let meta_path = temp_path.join("meta");
        std::fs::create_dir(meta_path.clone()).unwrap();

        std::fs::write(meta_path.join("manifest.json"), "").unwrap();

        assert_eq!(find_sdk_root(&start_path).unwrap().unwrap(), temp_path);
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_find_sdk_root_no_manifest() {
        let temp = tempdir().unwrap();

        let start_path = temp.path().to_path_buf().join("test1").join("test2");
        std::fs::create_dir_all(start_path.clone()).unwrap();

        let meta_path = temp.path().to_path_buf().join("meta");
        std::fs::create_dir(meta_path).unwrap();

        assert!(find_sdk_root(&start_path).unwrap().is_none());
    }
}
