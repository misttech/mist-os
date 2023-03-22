// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    api::{value::ValueStrategy, ConfigError, ConfigValue},
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
    io::{BufReader, Read, Write},
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};
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

impl Default for EnvironmentKind {
    fn default() -> Self {
        Self::NoContext
    }
}
/// Contextual information about where this instance of ffx is running
#[derive(Clone, Default, Debug, PartialEq)]
pub struct EnvironmentContext {
    kind: EnvironmentKind,
    env_vars: Option<EnvVars>,
    runtime_args: ConfigMap,
    env_file_path: Option<PathBuf>,
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
        env_vars: Option<EnvVars>,
        runtime_args: ConfigMap,
        env_file_path: Option<PathBuf>,
    ) -> Self {
        Self { kind, env_vars, runtime_args, env_file_path }
    }

    /// Initialize an environment type for an in tree context, rooted at `tree_root` and if
    /// a build directory is currently set at `build_dir`.
    pub fn in_tree(
        tree_root: PathBuf,
        build_dir: Option<PathBuf>,
        runtime_args: ConfigMap,
        env_file_path: Option<PathBuf>,
    ) -> Self {
        Self::new(
            EnvironmentKind::InTree { tree_root, build_dir },
            None,
            runtime_args,
            env_file_path,
        )
    }

    /// Initialize an environment with an isolated root under which things should be stored/used/run.
    pub fn isolated(
        isolate_root: PathBuf,
        env_vars: EnvVars,
        runtime_args: ConfigMap,
        env_file_path: Option<PathBuf>,
    ) -> Self {
        Self::new(
            EnvironmentKind::Isolated { isolate_root },
            Some(env_vars),
            runtime_args,
            env_file_path,
        )
    }

    /// Initialize an environment type that has no meaningful context, using only global and
    /// user level configuration.
    pub fn no_context(runtime_args: ConfigMap, env_file_path: Option<PathBuf>) -> Self {
        Self::new(EnvironmentKind::NoContext, None, runtime_args, env_file_path)
    }

    /// Detects what kind of environment we're in, based on the provided arguments,
    /// and returns the context found. If None is given for `env_file_path`, the default for
    /// the kind of environment will be used. Note that this will never automatically detect
    /// an isolated environment, that has to be chosen explicitly.
    pub fn detect(
        runtime_args: ConfigMap,
        current_dir: &Path,
        env_file_path: Option<PathBuf>,
    ) -> Result<Self, EnvironmentDetectError> {
        // strong signals that we're running...
        // - in-tree: we found a jiri root, and...
        if let Some(tree_root) = Self::find_jiri_root(current_dir)? {
            // look for a .fx-build-dir file and use that instead.
            let build_dir = Self::load_fx_build_dir(&tree_root)?;

            Ok(Self::in_tree(tree_root, build_dir, runtime_args, env_file_path))
        } else {
            // - no particular context: any other situation
            Ok(Self::no_context(runtime_args, env_file_path))
        }
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
        Environment::load(self.clone()).await
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
    /// - This will first check the environment variable in [`Self::FFX_BIN_ENV`], which should be set by a
    /// top level ffx invocation if we were run by one.
    /// - If that isn't set, it will look at the 'name' of the currently running executable, and
    /// if that doesn't appear to be a subtool (ie. has no hyphens in the filename), it will use that.
    /// - If neither of those are found, and an sdk is configured, search the sdk manifest for the
    /// ffx host-tool entry and use that.
    pub async fn rerun_bin(&self) -> Result<PathBuf, anyhow::Error> {
        if let Some(bin_from_env) = self.env_var(Self::FFX_BIN_ENV).ok() {
            return Ok(bin_from_env.into());
        }

        let current_exe = std::env::current_exe()?;
        let current_exe_name = current_exe.file_name().and_then(|name| name.to_str());
        if current_exe_name.map_or(false, |name| !name.contains("-")) {
            return Ok(current_exe);
        }

        let sdk = self.get_sdk_root().await?.get_sdk()?;
        sdk.get_host_tool("ffx")
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
    pub(crate) async fn new_empty(context: EnvironmentContext) -> Result<Self> {
        let _lock = Self::lock_env(&context.env_file_path()?).await?;

        let files = EnvironmentFiles::default();
        Ok(Self { context, files })
    }

    async fn load(context: EnvironmentContext) -> Result<Self> {
        let path = context.env_file_path()?;

        // Grab the lock because we're reading from the environment file.
        let lockfile = Self::lock_env(&path).await?;
        Self::load_with_lock(lockfile, path, context)
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

        let (lock_path, env) = match Self::lock_env(&path).await {
            Ok(lockfile) => (
                lockfile.path().to_owned(),
                Self::load_with_lock(lockfile, path.clone(), context.clone())?,
            ),
            Err(e) => return Ok(vec![(path, Err(e))]),
        };

        let mut checked = vec![(path, Ok(lock_path))];

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
        let _lock = Self::lock_env(&path).await?;

        Self::save_with_lock(_lock, path, &self.files)?;

        crate::cache::invalidate().await;

        Ok(())
    }

    fn load_with_lock(_lock: Lockfile, path: PathBuf, context: EnvironmentContext) -> Result<Self> {
        let file = File::open(&path).context("opening file for read")?;

        let files = serde_json::from_reader(BufReader::new(file))
            .context("reading environment from disk")?;

        Ok(Self { files, context })
    }

    fn save_with_lock(_lock: Lockfile, path: PathBuf, files: &EnvironmentFiles) -> Result<()> {
        // First save the config to a temp file in the same location as the file, then atomically
        // rename the file to the final location to avoid partially written files.
        let parent = path.parent().unwrap_or_else(|| Path::new("."));
        let mut tmp = tempfile::NamedTempFile::new_in(parent)?;

        serde_json::to_writer_pretty(&mut tmp, files).context("writing environment to disk")?;

        tmp.flush().context("flushing environment")?;

        let _ = tmp.persist(path)?;

        Ok(())
    }

    async fn lock_env(path: &Path) -> Result<Lockfile, LockfileCreateError> {
        Lockfile::lock_for(path, Duration::from_secs(2)).await.map_err(|e| {
            error!("Failed to create a lockfile for environment file {path}. Check that {lockpath} doesn't exist and can be written to. Ownership information: {owner:#?}", path=path.display(), lockpath=e.lock_path.display(), owner=e.owner);
            e
        })
    }

    pub fn get_user(&self) -> Option<&Path> {
        self.files.user.as_deref()
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

    pub fn get_build(&self) -> Option<&Path> {
        self.build_dir()
            .as_deref()
            .and_then(|dir| self.files.build.as_ref().and_then(|dirs| dirs.get(dir)))
            .map(PathBuf::as_ref)
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
        self.files
            .user
            .as_ref()
            .map_or_else(|| format!(" User: none\n"), |u| format!(" User: {}\n", u.display()))
    }

    fn display_build(&self) -> String {
        let mut res = format!(" Build:");
        match self.files.build.as_ref() {
            Some(m) => {
                if m.is_empty() {
                    res.push_str(&format!("  none\n"));
                }
                res.push_str(&format!("\n"));
                for (key, val) in m.iter() {
                    res.push_str(&format!("  {} => {}\n", key.display(), val.display()));
                }
            }
            None => {
                res.push_str(&format!("  none\n"));
            }
        }
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

    pub async fn init_env_file(path: &Path) -> Result<()> {
        let _e = Self::lock_env(path).await?;
        let mut f = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(path)
            .map_err(|e| {
                ffx_error!(
                    "Could not create envinronment file from given path \"{}\": {}",
                    path.display(),
                    e
                )
            })?;
        f.write_all(b"{}")?;
        f.sync_all()?;
        Ok(())
    }

    /// Checks the config files at the requested level to make sure they exist and are configured
    /// properly.
    pub async fn populate_defaults(&mut self, level: &ConfigLevel) -> Result<()> {
        match level {
            ConfigLevel::User => {
                if let None = self.files.user {
                    let default_path = self.context.get_default_user_file_path()?;
                    // This will override the config file if it exists.  This would happen anyway
                    // because of the cache.
                    let mut file = File::create(&default_path).context("opening write buffer")?;
                    file.write_all(b"{}").context("writing default user configuration file")?;
                    file.sync_all()
                        .context("syncing default user configuration file to filesystem")?;

                    self.files.user = Some(default_path);
                    self.save().await?;
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
                        let mut b_name =
                            b_dir.file_name().context("build dir had no filename")?.to_owned();
                        b_name.push(".json");
                        let config = b_dir.with_file_name(&b_name);
                        if !config.is_file() {
                            info!("Build configuration file for '{b_dir}' does not exist yet, will create it by default at '{config}' if a value is set", b_dir=b_dir.display(), config=config.display());
                        }
                        build_dirs.insert(b_dir, config);
                        self.save().await?;
                    }
                }
                None => bail!("Cannot set a build configuration without a build directory."),
            },
            _ => bail!("This config level is not writable."),
        }
        Ok(())
    }
}

impl fmt::Display for Environment {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "{}", self.display(&None))
    }
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
            temp_dir.clone(),
            Some(build_dir_path.clone()),
            ConfigMap::default(),
            Some(env_file_path.clone()),
        );
        assert!(!env_file_path.is_file(), "Environment file shouldn't exist yet");
        Environment::init_env_file(&env_file_path)
            .await
            .expect("Should be able to initialize the environment file");
        let mut env = context.load().await.expect("Should be able to load the environment");

        env.populate_defaults(&ConfigLevel::Build)
            .await
            .expect("Setting build level environment to automatic path should work");
        drop(env);
        if let Some(build_configs) = context
            .load()
            .await
            .expect("should be able to load the test-configured env-file.")
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
    async fn build_config_manual_configure() {
        let temp = tempfile::tempdir().expect("temporary build directory");
        let temp_dir = std::fs::canonicalize(temp.path()).expect("canonical temp path");
        let build_dir_path = temp_dir.join("build");
        let build_dir_config = temp_dir.join("build-manual.json");
        let env_file_path = temp_dir.join("env.json");
        let context = EnvironmentContext::in_tree(
            temp_dir.clone(),
            Some(build_dir_path.clone()),
            ConfigMap::default(),
            Some(env_file_path.clone()),
        );

        assert!(!env_file_path.is_file(), "Environment file shouldn't exist yet");
        let mut env = Environment::new_empty(context.clone())
            .await
            .expect("Creating new empty environment file");
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
