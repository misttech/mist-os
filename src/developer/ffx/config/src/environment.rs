// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    api::{value::ValueStrategy, ConfigError, ConfigValue},
    storage::Config,
    BuildOverride, ConfigLevel, ConfigMap, ConfigQuery,
};
use anyhow::{bail, Context, Result};
use errors::ffx_error;
use fuchsia_lockfile::{Lockfile, LockfileCreateError};
use sdk::{Sdk, SdkRoot};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fmt,
    fs::{File, OpenOptions},
    io::{BufReader, ErrorKind, Read, Write},
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
    time::Duration,
};
use tempfile::{NamedTempFile, TempDir};
use thiserror::Error;
use tracing::{error, info};

const SDK_NOT_FOUND_HELP: &str = "\
SDK directory could not be found. Please set with
`ffx sdk set root <PATH_TO_SDK_DIR>`\n
If you are developing in the fuchsia tree, ensure \
that you are running the `ffx` command (in $FUCHSIA_DIR/.jiri_root) or `fx ffx`, not a built binary.
Running the binary directly is not supported in the fuchsia tree.\n\n";

/// A name for the type used as an environment variable mapping for isolation override
type EnvVars = HashMap<String, String>;

#[derive(Clone, Debug, Default, PartialEq, Deserialize, Serialize)]
struct EnvironmentFiles {
    user: Option<PathBuf>,
    build: Option<HashMap<PathBuf, PathBuf>>,
    global: Option<PathBuf>,
}

/// The type of environment we're running in, along with relevant information about
/// that environment.
#[derive(Clone, Debug, PartialEq)]
pub enum EnvironmentKind {
    /// In a fuchsia.git build tree with a jiri root and possibly a build directory.
    InTree { tree_root: PathBuf, build_dir: Option<PathBuf> },
    /// Isolated within a particular directory for testing or consistency purposes
    Isolated { isolate_root: PathBuf },
    /// Any other context with no specific information, using the user directory for
    /// all (non-global/default) configuration.
    NoContext,
}

impl std::fmt::Display for EnvironmentKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use EnvironmentKind::*;
        match self {
            InTree { tree_root, build_dir: Some(build_dir) } => write!(
                f,
                "Fuchsia.git In-Tree Rooted at {root}, with default build directory of {build}",
                root = tree_root.display(),
                build = build_dir.display()
            ),
            InTree { tree_root, build_dir: None } => write!(
                f,
                "Fuchsia.git In-Tree Root at {root} with no default build directory",
                root = tree_root.display()
            ),
            Isolated { isolate_root } => write!(
                f,
                "Isolated environment with an isolated root of {root}",
                root = isolate_root.display()
            ),
            NoContext => write!(f, "Global user context"),
        }
    }
}

/// What kind of executable target this environment was created in
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ExecutableKind {
    /// A main "launcher" ffx that can be used to run other ffx commands.
    MainFfx,
    /// A subtool ffx that only knows its own command.
    Subtool,
    /// In a unit or integration test
    Test,
}

/// Contextual information about where this instance of ffx is running
#[derive(Clone, Debug)]
pub struct EnvironmentContext {
    kind: EnvironmentKind,
    exe_kind: ExecutableKind,
    env_vars: Option<EnvVars>,
    runtime_args: ConfigMap,
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
        // - in-tree: we found a jiri root, and...
        if let Some(tree_root) = Self::find_jiri_root(current_dir)? {
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
            EnvironmentKind::InTree { .. } | EnvironmentKind::NoContext => {}
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

#[derive(Debug)]
pub struct Environment {
    files: EnvironmentFiles,
    context: EnvironmentContext,
}

impl Environment {
    /// Creates a new empty env that will be saved to a specific path, but is initialized
    /// with no settings. For internal use only, when loading the global environment fails.
    pub(crate) fn new_empty(context: EnvironmentContext) -> Self {
        let files = EnvironmentFiles::default();
        Self { context, files }
    }

    async fn load(context: &EnvironmentContext) -> Result<Self> {
        let path = context.env_file_path()?;

        Self::load_env_file(&path, context)
    }

    pub fn context(&self) -> &EnvironmentContext {
        &self.context
    }

    /// Checks if we can manage to open the given environment file's lockfile,
    /// as well as each configuration file referenced by it, and returns the lockfile
    /// owner if we can't. Will return a normal error via result if any non-lockfile
    /// error is encountered while processing the files.
    ///
    /// Used to implement diagnostics for `ffx doctor`.
    pub async fn check_locks(
        context: &EnvironmentContext,
    ) -> Result<Vec<(PathBuf, Result<PathBuf, LockfileCreateError>)>> {
        let path = context.env_file_path()?.clone();
        let env = Self::load_env_file(&path, context)?;

        let mut checked = vec![];

        if let Some(user) = env.files.user {
            let res = Lockfile::lock_for(&user, Duration::from_secs(1)).await;
            checked.push((user, res.map(|lock| lock.path().to_owned())));
        }
        if let Some(global) = env.files.global {
            let res = Lockfile::lock_for(&global, Duration::from_secs(1)).await;
            checked.push((global, res.map(|lock| lock.path().to_owned())));
        }
        for (_, build) in env.files.build.unwrap_or_default() {
            let res = Lockfile::lock_for(&build, Duration::from_secs(1)).await;
            checked.push((build, res.map(|lock| lock.path().to_owned())));
        }

        Ok(checked)
    }

    pub async fn save(&self) -> Result<()> {
        let path = self.context.env_file_path()?;

        Self::save_env_file(path, &self.files)?;

        crate::cache::invalidate(&self.context.cache).await;

        Ok(())
    }

    pub(crate) async fn config_from_cache(
        self,
        build_override: Option<BuildOverride<'_>>,
    ) -> Result<Arc<async_lock::RwLock<Config>>> {
        let build_dir = self.override_build_dir(build_override);
        crate::cache::load_config(&self.context.cache, build_dir, || Config::from_env(&self)).await
    }

    fn load_env_file(path: &Path, context: &EnvironmentContext) -> Result<Self> {
        let files = match File::open(path) {
            Ok(file) => serde_json::from_reader(BufReader::new(file))
                .context("reading environment from disk"),
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(EnvironmentFiles::default()),
            Err(e) => Err(e).context("loading host log directories from ffx config"),
        }?;

        Ok(Self { files, context: context.clone() })
    }

    fn save_env_file(path: PathBuf, files: &EnvironmentFiles) -> Result<()> {
        // First save the config to a temp file in the same location as the file, then atomically
        // rename the file to the final location to avoid partially written files.
        let parent = path.parent().unwrap_or_else(|| Path::new("."));
        let mut tmp = tempfile::NamedTempFile::new_in(parent)?;

        serde_json::to_writer_pretty(&mut tmp, files).context("writing environment to disk")?;

        tmp.flush().context("flushing environment")?;

        let _ = tmp.persist(path)?;

        Ok(())
    }

    pub fn get_user(&self) -> Option<PathBuf> {
        self.files
            .user
            .as_ref()
            .map(PathBuf::clone)
            .or_else(|| self.context.get_default_user_file_path().ok())
    }
    pub fn set_user(&mut self, to: Option<&Path>) {
        self.files.user = to.map(Path::to_owned);
    }

    pub fn get_global(&self) -> Option<&Path> {
        self.files.global.as_deref()
    }
    pub fn set_global(&mut self, to: Option<&Path>) {
        self.files.global = to.map(Path::to_owned);
    }

    pub fn get_runtime_args(&self) -> &ConfigMap {
        &self.context.runtime_args
    }

    pub fn build_dir(&self) -> Option<&Path> {
        self.context.build_dir()
    }

    /// returns either the directory indicated by the override or the one configured in this
    /// environment.
    pub fn override_build_dir<'a>(
        &'a self,
        build_override: Option<BuildOverride<'a>>,
    ) -> Option<&'a Path> {
        match (build_override, self.build_dir()) {
            (Some(BuildOverride::Path(path)), _) => Some(path),
            (Some(BuildOverride::NoBuild), _) => None,
            (_, maybe_path) => maybe_path,
        }
    }

    pub fn get_build(&self) -> Option<PathBuf> {
        let dir = self.build_dir()?;
        self.files
            .build
            .as_ref()
            .and_then(|dirs| dirs.get(dir).cloned())
            .or_else(|| self.context.get_default_build_dir_config_path(dir).ok())
    }

    pub fn set_build(
        &mut self,
        to: &Path,
        build_override: Option<BuildOverride<'_>>,
    ) -> Result<()> {
        let build_dir = self
            .override_build_dir(build_override)
            .context("Tried to set unknown build directory")?
            .to_owned();
        let build_dirs = match &mut self.files.build {
            Some(build_dirs) => build_dirs,
            None => self.files.build.get_or_insert_with(Default::default),
        };
        build_dirs.insert(build_dir, to.to_owned());
        Ok(())
    }

    fn display_user(&self) -> String {
        match self.files.user.as_ref() {
            Some(file) => format!(" User: {}\n", file.display()),
            None => match self.context.get_default_user_file_path() {
                Ok(default) => format!(" User: {} (default)\n", default.display()),
                Err(_) => format!(" User: none"),
            },
        }
    }

    /// Returns the default build config file for the build dir if there is one,
    /// None otherwise.
    fn display_build_default(&self, current_build_dir: Option<&Path>) -> Option<String> {
        let current = current_build_dir?;
        let default = self.context.get_default_build_dir_config_path(current).ok()?;
        Some(format!("  {} => {} (default)\n", current.display(), default.display()))
    }

    fn display_build(&self) -> String {
        let mut current_build_dir = self.build_dir();
        let mut res = format!(" Build:");
        if let Some(m) = self.files.build.as_ref() {
            if !m.is_empty() {
                res.push_str(&format!("\n"));
                for (key, val) in m.iter() {
                    // if we have an explicitly set path for the current build
                    // dir then prevent displaying the default later by removing
                    // it from the option.
                    if Some(key.as_ref()) == current_build_dir {
                        current_build_dir.take();
                    }
                    res.push_str(&format!("  {} => {}\n", key.display(), val.display()));
                }
            }
        }
        res.push_str(
            self.display_build_default(current_build_dir).as_deref().unwrap_or("  none\n"),
        );
        res
    }

    fn display_global(&self) -> String {
        self.files
            .global
            .as_ref()
            .map_or_else(|| format!(" Global: none\n"), |g| format!(" Global: {}\n", g.display()))
    }

    pub fn display(&self, level: &Option<ConfigLevel>) -> String {
        level.map_or_else(
            || {
                let mut res = format!("\nEnvironment:\n");
                res.push_str(&self.display_user());
                res.push_str(&self.display_build());
                res.push_str(&self.display_global());
                res
            },
            |l| match l {
                ConfigLevel::User => self.display_user(),
                ConfigLevel::Build => self.display_build(),
                ConfigLevel::Global => self.display_global(),
                _ => format!(" This level is not saved in the environment file."),
            },
        )
    }

    /// Checks the config files at the requested level to make sure they exist and are configured
    /// properly.
    pub async fn populate_defaults(&mut self, level: &ConfigLevel) -> Result<()> {
        match level {
            ConfigLevel::User => {
                if let None = self.files.user {
                    let default_path = self.context.get_default_user_file_path()?;

                    match create_new_file(&default_path) {
                        Ok(mut file) => {
                            file.write_all(b"{}")
                                .context("writing default user configuration file")?;
                            file.sync_all()
                                .context("syncing default user configuration file to filesystem")?;
                        }
                        Err(e) if e.kind() == ErrorKind::AlreadyExists => {}
                        other => {
                            other.context("creating default user configuration file").map(|_| ())?
                        }
                    }
                    self.files.user = Some(default_path);
                }
            }
            ConfigLevel::Global => {
                if let None = self.files.global {
                    bail!(
                        "Global configuration not set. Use 'ffx config env set' command \
                         to setup the environment."
                    );
                }
            }
            ConfigLevel::Build => match self.build_dir().map(Path::to_owned) {
                Some(b_dir) => {
                    let build_dirs = match &mut self.files.build {
                        Some(build_dirs) => build_dirs,
                        None => self.files.build.get_or_insert_with(Default::default),
                    };
                    if !build_dirs.contains_key(&b_dir) {
                        let config = self.context.get_default_build_dir_config_path(&b_dir)?;
                        if !config.is_file() {
                            info!("Build configuration file for '{b_dir}' does not exist yet, will create it by default at '{config}' if a value is set", b_dir=b_dir.display(), config=config.display());
                        }
                        build_dirs.insert(b_dir, config);
                    }
                }
                None => bail!("Cannot set a build configuration without a build directory."),
            },
            _ => bail!("This config level is not writable."),
        }
        Ok(())
    }
}

/// polyfill for File::create_new, which is currently nightly-only.
fn create_new_file(path: &Path) -> std::io::Result<File> {
    OpenOptions::new().read(true).write(true).create_new(true).open(path)
}

impl fmt::Display for Environment {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "{}", self.display(&None))
    }
}

/// A structure that holds information about the test config environment for the duration
/// of a test. This object must continue to exist for the duration of the test, or the test
/// may fail.
#[must_use = "This object must be held for the duration of a test (ie. `let _env = ffx_config::test_init()`) for it to operate correctly."]
pub struct TestEnv {
    pub env_file: NamedTempFile,
    pub context: EnvironmentContext,
    pub isolate_root: TempDir,
    pub user_file: NamedTempFile,
    _guard: async_lock::MutexGuardArc<()>,
}

impl TestEnv {
    async fn new(_guard: async_lock::MutexGuardArc<()>) -> Result<Self> {
        let env_file = NamedTempFile::new().context("tmp access failed")?;
        let user_file = NamedTempFile::new().context("tmp access failed")?;
        let isolate_root = tempfile::tempdir()?;

        // Point the user config at a temporary file.
        let user_file_path = user_file.path().to_owned();
        let context = EnvironmentContext::isolated(
            ExecutableKind::Test,
            isolate_root.path().to_owned(),
            HashMap::from_iter(std::env::vars()),
            ConfigMap::default(),
            Some(env_file.path().to_owned()),
        );
        let test_env = TestEnv { env_file, context, user_file, isolate_root, _guard };

        let mut env = Environment::new_empty(test_env.context.clone());
        env.set_user(Some(&user_file_path));
        env.save().await.context("saving env file")?;

        Ok(test_env)
    }

    pub async fn load(&self) -> Environment {
        self.context.load().await.expect("opening test env file")
    }
}

impl Drop for TestEnv {
    fn drop(&mut self) {
        // after the test, wipe out all the test configuration we set up. Explode if things aren't as we
        // expect them.
        let mut env = crate::ENV.lock().expect("Poisoned lock");
        let env_prev = env.clone();
        *env = None;
        drop(env);

        if let Some(env_prev) = env_prev {
            assert_eq!(
                env_prev,
                self.context,
                "environment context changed from isolated environment to {other:?} during test run somehow.",
                other = env_prev
            );
        }

        // since we're not running in async context during drop, we can't clear the cache unfortunately.
    }
}

/// When running tests we usually just want to initialize a blank slate configuration, so
/// use this for tests. You must hold the returned object object for the duration of the test, not doing so
/// will result in strange behaviour.
pub async fn test_init() -> Result<TestEnv> {
    lazy_static::lazy_static! {
        static ref TEST_LOCK: Arc<async_lock::Mutex<()>> = Arc::default();
    }
    let env = TestEnv::new(TEST_LOCK.lock_arc().await).await?;

    // force an overwrite of the configuration setup
    crate::init(&env.context).await?;

    Ok(env)
}

////////////////////////////////////////////////////////////////////////////////
// tests

#[cfg(test)]
mod test {
    use super::*;
    use crate::test_init;
    use std::fs;
    use tempfile::tempdir;

    const ENVIRONMENT: &'static str = r#"
        {
            "user": "/tmp/user.json",
            "build": {
                "/tmp/build/1": "/tmp/build/1/build.json"
            },
            "global": "/tmp/global.json"
        }"#;

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
    async fn test_loading_and_saving_environment() {
        let mut test_env = test_init().await.expect("initializing test environment");
        let env: EnvironmentFiles = serde_json::from_str(ENVIRONMENT).unwrap();

        // Write out the initial test environment.
        let tmp_path = test_env.env_file.path().to_owned();
        let mut env_file = File::create(&tmp_path).unwrap();
        serde_json::to_writer(&mut env_file, &env).unwrap();
        env_file.flush().unwrap();

        // Load the environment back in, and make sure it's correct.
        let env_load = test_env.load().await;
        assert_eq!(env, env_load.files);

        // Remove the file to prevent a spurious success
        std::fs::remove_file(&tmp_path).expect("Temporary env file wasn't available to remove");

        // Save the environment, then read the saved file and make sure it's correct.
        env_load.save().await.unwrap();
        test_env.env_file.flush().unwrap();

        let env_file = fs::read(&tmp_path).unwrap();
        let env_save: EnvironmentFiles = serde_json::from_slice(&env_file).unwrap();

        assert_eq!(env, env_save);
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn build_config_autoconfigure() {
        let temp = tempfile::tempdir().expect("temporary build directory");
        let temp_dir = std::fs::canonicalize(temp.path()).expect("canonical temp path");
        let build_dir_path = temp_dir.join("build");
        let build_dir_config = temp_dir.join("build.json");
        let env_file_path = temp_dir.join("env.json");
        let context = EnvironmentContext::in_tree(
            ExecutableKind::Test,
            temp_dir.clone(),
            Some(build_dir_path.clone()),
            ConfigMap::default(),
            Some(env_file_path.clone()),
        );
        assert!(!env_file_path.is_file(), "Environment file shouldn't exist yet");
        let mut env = context.load().await.expect("Should be able to load the environment");

        env.populate_defaults(&ConfigLevel::Build)
            .await
            .expect("Setting build level environment to automatic path should work");
        drop(env);
        let config = context
            .load()
            .await
            .expect("should be able to load the test-configured env-file.")
            .get_build()
            .expect("should be a default build configuration");

        assert_eq!(config, build_dir_config, "build config for {build_dir_path:?}");
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn build_config_manual_configure() {
        let temp = tempfile::tempdir().expect("temporary build directory");
        let temp_dir = std::fs::canonicalize(temp.path()).expect("canonical temp path");
        let build_dir_path = temp_dir.join("build");
        let build_dir_config = temp_dir.join("build-manual.json");
        let env_file_path = temp_dir.join("env.json");
        let context = EnvironmentContext::in_tree(
            ExecutableKind::Test,
            temp_dir.clone(),
            Some(build_dir_path.clone()),
            ConfigMap::default(),
            Some(env_file_path.clone()),
        );

        assert!(!env_file_path.is_file(), "Environment file shouldn't exist yet");
        let mut env = Environment::new_empty(context.clone());
        let mut config_map = std::collections::HashMap::new();
        config_map.insert(build_dir_path.clone(), build_dir_config.clone());
        env.files.build = Some(config_map);
        env.save().await.expect("Should be able to save the configured environment");

        env.populate_defaults(&ConfigLevel::Build)
            .await
            .expect("Setting build level environment to automatic path should work");
        drop(env);

        if let Some(build_configs) = context
            .load()
            .await
            .expect("should be able to load the manually configured env-file.")
            .files
            .build
        {
            match build_configs.get(&build_dir_path) {
                Some(config) if config == &build_dir_config => (),
                Some(config) => panic!("Build directory config file was wrong. Expected: {build_dir_config}, got: {config})", build_dir_config=build_dir_config.display(), config=config.display()),
                None => panic!("No build directory config was set"),
            }
        } else {
            panic!("No build configurations set after setting a configuration value");
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
