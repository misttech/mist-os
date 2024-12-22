// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::api::value::{TryConvert, ValueStrategy};
use crate::api::ConfigError;
use crate::storage::{AssertNoEnv, Config};
use crate::{is_analytics_disabled, ConfigMap, ConfigQuery, Environment};
use anyhow::{Context, Result};
use camino::{Utf8Path, Utf8PathBuf};
use errors::ffx_error;
use ffx_config_domain::ConfigDomain;
use sdk::{Sdk, SdkRoot};
use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use thiserror::Error;
use tracing::{debug, error, trace};

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
    self_path: PathBuf,
    /// if true, do not read or write any environment files.
    pub(crate) no_environment: bool,
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
        no_environment: bool,
    ) -> Self {
        let cache = Arc::default();
        Self {
            kind,
            exe_kind,
            env_vars,
            runtime_args,
            env_file_path,
            cache,
            self_path: std::env::current_exe().unwrap(),
            no_environment,
        }
    }

    /// Initializes an environment type that is just the bare minimum, containing no ambient configuration, only
    /// the runtime args.
    pub fn strict(exe_kind: ExecutableKind, runtime_args: ConfigMap) -> Result<Self> {
        let cache = Arc::default();
        let res = Self {
            kind: EnvironmentKind::StrictContext,
            exe_kind: exe_kind.clone(),
            // For simplicity, the runtime_args will be kept empty.
            runtime_args: ConfigMap::new(),
            env_vars: None,
            env_file_path: None,
            no_environment: true,
            cache,
            self_path: std::env::current_exe().unwrap(),
        };

        // This just takes the whole config and makes it a flattened map of
        // "default" plus "runtime". Since environment variables won't be
        // expanded even when specified at run-time, we verify that there are no
        // environment variables specified on the command line.  Values in the
        // default config that refer to environment variables will be ignored,
        // so such values will look like unspecified config values, and will
        // need to be specified on the command-line. For example:
        // `ffx ... --config ssh.priv=path/to/ssh-key --strict target echo`
        runtime_args.assert_no_env(None, &res)?;
        res.cache.overwrite_default(&runtime_args)?;
        // TODO(b/368058956): Print a sanitized config into the logs so we can use it for
        // debugging.
        Ok(res)
    }

    /// Initialize an environment type for a config domain context, with a
    /// `fuchsia_env` file at its root.
    pub fn config_domain(
        exe_kind: ExecutableKind,
        domain: ConfigDomain,
        runtime_args: ConfigMap,
        isolate_root: Option<PathBuf>,
        no_environment: bool,
    ) -> Self {
        Self::new(
            EnvironmentKind::ConfigDomain { domain, isolate_root },
            exe_kind,
            None,
            runtime_args,
            None,
            no_environment,
        )
    }

    /// Initialize an environment type for a config domain context, looking for
    /// a fuchsia_env file at the given path.
    pub fn config_domain_root(
        exe_kind: ExecutableKind,
        domain_root: Utf8PathBuf,
        runtime_args: ConfigMap,
        isolate_root: Option<PathBuf>,
        no_environment: bool,
    ) -> Result<Self> {
        let domain_config = ConfigDomain::find_root(&domain_root).with_context(|| {
            ffx_error!("Could not find config domain root from '{domain_root}'")
        })?;
        let domain = ConfigDomain::load_from(&domain_config).with_context(|| {
            ffx_error!("Could not load config domain file at '{domain_config}'")
        })?;
        Ok(Self::config_domain(exe_kind, domain, runtime_args, isolate_root, no_environment))
    }

    /// Initialize an environment type for an in tree context, rooted at `tree_root` and if
    /// a build directory is currently set at `build_dir`.
    pub fn in_tree(
        exe_kind: ExecutableKind,
        tree_root: PathBuf,
        build_dir: Option<PathBuf>,
        runtime_args: ConfigMap,
        env_file_path: Option<PathBuf>,
        no_environment: bool,
    ) -> Self {
        Self::new(
            EnvironmentKind::InTree { tree_root, build_dir },
            exe_kind,
            None,
            runtime_args,
            env_file_path,
            no_environment,
        )
    }

    /// Initialize an environment with an isolated root under which things should be stored/used/run.
    pub fn isolated(
        exe_kind: ExecutableKind,
        isolate_root: PathBuf,
        env_vars: EnvVars,
        runtime_args: ConfigMap,
        env_file_path: Option<PathBuf>,
        current_dir: Option<&Utf8Path>,
        no_environment: bool,
    ) -> Result<Self> {
        if let Some(domain_path) = current_dir.and_then(ConfigDomain::find_root) {
            let domain = ConfigDomain::load_from(&domain_path)?;
            Ok(Self::config_domain(
                exe_kind,
                domain,
                runtime_args,
                Some(isolate_root),
                no_environment,
            ))
        } else {
            // Isolate dirs should be absolute paths
            let isolate_root = std::path::absolute(&isolate_root)?;
            Ok(Self::new(
                EnvironmentKind::Isolated { isolate_root },
                exe_kind,
                Some(env_vars),
                runtime_args,
                env_file_path,
                no_environment,
            ))
        }
    }

    pub fn is_strict(&self) -> bool {
        match self.kind {
            EnvironmentKind::StrictContext => true,
            _ => false,
        }
    }

    pub fn is_isolated(&self) -> bool {
        match self.kind {
            EnvironmentKind::ConfigDomain { isolate_root: Some(..), .. }
            | EnvironmentKind::Isolated { .. } => true,
            _ => false,
        }
    }

    pub fn has_no_environment(&self) -> bool {
        self.no_environment
    }

    /// Initialize an environment type that has no meaningful context, using only global and
    /// user level configuration.
    pub fn no_context(
        exe_kind: ExecutableKind,
        runtime_args: ConfigMap,
        env_file_path: Option<PathBuf>,
        no_environment: bool,
    ) -> Self {
        Self::new(
            EnvironmentKind::NoContext,
            exe_kind,
            None,
            runtime_args,
            env_file_path,
            no_environment,
        )
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
        no_environment: bool,
    ) -> Result<Self, EnvironmentDetectError> {
        // strong signals that we're running...
        if let Some(domain_path) = ConfigDomain::find_root(current_dir.try_into()?) {
            // - a config-domain: we found a fuchsia-env file
            let domain = ConfigDomain::load_from(&domain_path)?;
            Ok(Self::config_domain(exe_kind, domain, runtime_args, None, no_environment))
        } else if let Some(tree_root) = Self::find_jiri_root(current_dir)? {
            // - in-tree: we found a jiri root, and...
            // look for a .fx-build-dir file and use that instead.
            let build_dir = Self::load_fx_build_dir(&tree_root)?;

            Ok(Self::in_tree(
                exe_kind,
                tree_root,
                build_dir,
                runtime_args,
                env_file_path,
                no_environment,
            ))
        } else {
            // - no particular context: any other situation
            Ok(Self::no_context(exe_kind, runtime_args, env_file_path, no_environment))
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
            !is_analytics_disabled(self).await
        }
    }

    pub fn env_file_path(&self) -> Result<PathBuf> {
        match &self.env_file_path {
            Some(path) => Ok(path.clone()),
            None => Ok(self.get_default_env_path()?),
        }
    }

    /// Returns the context's project root, if it makes sense for its
    /// [`EnvironmentKind`].
    pub fn project_root(&self) -> Option<&Path> {
        match &self.kind {
            EnvironmentKind::InTree { tree_root, .. } => Some(&tree_root),
            EnvironmentKind::ConfigDomain { domain, .. } => Some(domain.root().as_std_path()),
            _ => None,
        }
    }

    /// Returns the path to the currently active build output directory
    pub fn build_dir(&self) -> Option<&Path> {
        match &self.kind {
            EnvironmentKind::InTree { build_dir, .. } => build_dir.as_deref(),
            EnvironmentKind::ConfigDomain { domain, .. } => {
                Some(domain.get_build_dir()?.as_std_path())
            }
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

    pub fn load(&self) -> Result<Environment> {
        Environment::load(self)
    }

    /// Gets an environment variable, either from the system environment or from the isolation-configured
    /// environment.
    pub fn env_var(&self, name: &str) -> Result<String, std::env::VarError> {
        match &self.env_vars {
            Some(env_vars) => env_vars.get(name).cloned().ok_or(std::env::VarError::NotPresent),
            _ => std::env::var(name),
        }
    }

    // Some tests need to clear out the env
    pub fn remove_var(&mut self, name: &str) {
        if let Some(env_vars) = &mut self.env_vars {
            env_vars.remove(name);
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
    pub fn get<'a, T, U>(&'a self, with: U) -> std::result::Result<T, ConfigError>
    where
        T: TryConvert + ValueStrategy,
        U: Into<ConfigQuery<'a>>,
    {
        self.query(with).get()
    }

    /// A shorthand for the very common case of querying a value from the global config
    /// cache and this environment, using the provided value converted into a query.
    pub fn get_optional<'a, T, U>(&'a self, with: U) -> std::result::Result<T, ConfigError>
    where
        T: TryConvert + ValueStrategy,
        U: Into<ConfigQuery<'a>>,
    {
        self.query(with).get_optional()
    }

    /// Find the appropriate sdk root for this invocation of ffx, looking at configuration
    /// values and the current environment context to determine the correct place to find it.
    pub async fn get_sdk_root(&self) -> Result<SdkRoot> {
        // some in-tree tooling directly overrides sdk.root. But if that's not done, the 'root' is just the
        // build directory.
        // Out of tree, we will always want to pull the config from the normal config path, which
        // we can defer to the SdkRoot's mechanisms for.
        let runtime_root: Option<PathBuf> = self.query("sdk.root").get().ok();

        match (&self.kind, runtime_root) {
            (EnvironmentKind::InTree { build_dir: Some(build_dir), .. }, None) => {
                let manifest = build_dir.clone();
                let module = self.query("sdk.module").get().ok();
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
    pub const FFX_BIN_ENV: &'static str = "FFX_BIN";

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
            return Ok(self.self_path.clone());
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

    pub fn get_default_overrides(&self) -> ConfigMap {
        use EnvironmentKind::*;
        let mut cm = match &self.kind {
            ConfigDomain { domain, .. } => domain.get_config_defaults().clone(),
            _ => ConfigMap::default(),
        };
        if self.is_isolated() {
            crate::aliases::add_isolation_default(&mut cm);
        }
        cm
    }

    /// Gets the basic information about the sdk as configured, without diving deeper into the sdk's own configuration.
    async fn sdk_from_config(&self, sdk_root: Option<&Path>) -> Result<SdkRoot> {
        // All gets in this function should declare that they don't want the build directory searched, because
        // if there is a build directory it *is* generally the sdk.
        let manifest = match sdk_root {
            Some(root) => root.to_owned(),
            _ => {
                let exe_path = find_exe_path()?;

                match find_sdk_root(&Path::new(&exe_path)) {
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
        let module = self.query("sdk.module").get().ok();
        match module {
            Some(module) => {
                debug!("Found modular Fuchsia SDK at {manifest:?} with module {module}");
                Ok(SdkRoot::Modular { manifest, module })
            }
            _ => {
                debug!("Found full Fuchsia SDK at {manifest:?}");
                Ok(SdkRoot::Full(manifest))
            }
        }
    }

    /// Returns the configuration domain for the current invocation, if there
    /// is one.
    pub fn get_config_domain(&self) -> Option<&ConfigDomain> {
        match &self.kind {
            EnvironmentKind::ConfigDomain { domain, .. } => Some(domain),
            _ => None,
        }
    }

    /// Returns a mutable reference to the configuration domain for the current
    /// invocation, if there is one. This can be used in bootstrapping to
    /// refresh the project-local configuration.
    pub fn get_config_domain_mut(&mut self) -> Option<&mut ConfigDomain> {
        match &mut self.kind {
            EnvironmentKind::ConfigDomain { domain, .. } => Some(domain),
            _ => None,
        }
    }
}

/// Finds the executable path of the ffx binary being run, attempting to
/// get the path the user believes it to be at, even if it's symlinked from
/// somewhere else, by using `argv[0]` and [`std::env::current_exe`].
///
/// We do this because sometimes ffx is invoked through an SDK that is symlinked
/// into place from a content addressable store, and we want to make a best
/// effort to search for the sdk in the right place.
fn find_exe_path() -> Result<PathBuf> {
    // get the 'real' binary path, which may have symlinks resolved, as well
    // as the command this was run as and the cwd
    let cwd = std::env::current_dir().context("FFX was run from an invalid working directory")?;
    let binary_path = std::env::current_exe()
        .and_then(|p| p.canonicalize())
        .context("FFX Binary doesn't exist in the file system")?;
    let args_path = match std::env::args_os().next() {
        Some(arg) => PathBuf::from(&arg),
        None => {
            trace!("FFX was run without an argv[0] somehow");
            return Ok(binary_path);
        }
    };

    // canonicalize the path from argv0 to try to figure out where it 'really'
    // is to make sure it's actually the right binary through potential
    // symlinks.
    let canonical_args_path = match args_path.canonicalize() {
        Ok(path) => path,
        Err(e) => {
            trace!(
                "Could not canonicalize the path ffx was run with, \
                which might mean the working directory has changed or the file \
                doesn't exist anymore: {e:?}"
            );
            return Ok(binary_path);
        }
    };

    // check that it's the same file in the end
    if binary_path == canonical_args_path {
        // but return the path it was actually run through instead of the canonical
        // path, but [`Path::join`]-ed to the cwd to make it more or less
        // absolute.
        Ok(cwd.join(args_path))
    } else {
        trace!(
            "FFX's argv[0] ({args_path:?}) resolved to {canonical_args_path:?} \
            instead of the binary's path {binary_path:?}, falling back to the \
            binary path."
        );
        Ok(binary_path)
    }
}

fn find_sdk_root(start_path: &Path) -> Result<Option<PathBuf>> {
    let cwd = std::env::current_dir()
        .context("Could not resolve working directory while searching for the Fuchsia SDK")?;
    let mut path = cwd.join(start_path);
    debug!("Attempting to find the sdk root from {path:?}");

    loop {
        path = if let Some(parent) = path.parent() {
            parent.to_path_buf()
        } else {
            return Ok(None);
        };

        if SdkRoot::is_sdk_root(&path) {
            debug!("Found sdk root through recursive search in {path:?}");
            return Ok(Some(path));
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;

    use assert_matches::assert_matches;
    use tempfile::tempdir;

    const DOMAINS_TEST_DATA_PATH: &str = env!("DOMAINS_TEST_DATA_PATH");

    impl Default for EnvironmentContext {
        fn default() -> Self {
            Self {
                kind: EnvironmentKind::NoContext,
                exe_kind: ExecutableKind::Test,
                env_vars: Default::default(),
                runtime_args: Default::default(),
                env_file_path: Default::default(),
                cache: Default::default(),
                self_path: std::env::current_exe().unwrap(),
                no_environment: false,
            }
        }
    }

    #[fuchsia::test]
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

    #[fuchsia::test]
    async fn test_find_sdk_root_no_manifest() {
        let temp = tempdir().unwrap();

        let start_path = temp.path().to_path_buf().join("test1").join("test2");
        std::fs::create_dir_all(start_path.clone()).unwrap();

        let meta_path = temp.path().to_path_buf().join("meta");
        std::fs::create_dir(meta_path).unwrap();

        assert!(find_sdk_root(&start_path).unwrap().is_none());
    }

    fn domains_test_data_path() -> &'static Utf8Path {
        Utf8Path::new(DOMAINS_TEST_DATA_PATH)
    }

    #[fuchsia::test]
    async fn test_config_domain_context() {
        let domain_root = domains_test_data_path().join("basic_example");
        let context = EnvironmentContext::config_domain_root(
            ExecutableKind::Test,
            domain_root.clone(),
            Default::default(),
            None,
            false,
        )
        .expect("config domain context");

        check_config_domain_paths(&context, &domain_root).await;
        assert!(!context.is_isolated());
    }

    #[fuchsia::test]
    async fn test_strict_context() {
        // For the time being, these are all the values that default (built into the binary,
        // specifically) to being env variables. These must be overwritten to allow for allocating
        // of a strict env context.
        let mut config_map = ConfigMap::new();
        config_map.insert(
            "target".to_string(),
            serde_json::json!({
                "default": "127.0.0.1"
            }),
        );
        config_map.insert(
            "ssh".to_string(),
            serde_json::json!({
                "pub": "/tmp/whatever",
                "priv": "/tmp/whatever2"
            }),
        );
        config_map.insert(
            "log".to_string(),
            serde_json::json!({
                "dir": "/tmp/loggodoggo"
            }),
        );
        config_map.insert(
            "fastboot".to_string(),
            serde_json::json!({
                "devices_file": {
                    "path": "/tmp/fastboot_thing_I_guess"
                }
            }),
        );
        let context =
            EnvironmentContext::strict(ExecutableKind::Test, config_map).expect("strict context");
        assert!(context.is_strict());
    }

    #[fuchsia::test]
    async fn test_config_domain_context_isolated() {
        let isolate_dir = tempdir().expect("tempdir");
        let domain_root = domains_test_data_path().join("basic_example");
        println!("check with explicit config domain path");
        let context = EnvironmentContext::config_domain_root(
            ExecutableKind::Test,
            domain_root.clone(),
            Default::default(),
            Some(isolate_dir.path().to_owned()),
            false,
        )
        .expect("isolated config domain context");

        check_config_domain_paths(&context, &domain_root).await;
        check_isolated_paths(&context, &isolate_dir.path());

        println!("check with implied config domain path");
        let context = EnvironmentContext::isolated(
            ExecutableKind::Test,
            isolate_dir.path().to_owned(),
            Default::default(),
            Default::default(),
            None,
            Some(&domain_root),
            false,
        )
        .expect("Isolated context");

        check_config_domain_paths(&context, &domain_root).await;
        check_isolated_paths(&context, &isolate_dir.path());
    }

    #[test]
    fn test_config_isolated_context() {
        let isolate_dir = tempdir().expect("tempdir");
        let context = EnvironmentContext::isolated(
            ExecutableKind::Test,
            isolate_dir.path().to_owned(),
            Default::default(),
            Default::default(),
            None,
            None,
            false,
        )
        .expect("Isolated context");

        check_isolated_paths(&context, &isolate_dir.path());
    }

    async fn check_config_domain_paths(context: &EnvironmentContext, domain_root: &Utf8Path) {
        let domain_root = domain_root.canonicalize().expect("canonicalized domain root");
        assert_eq!(context.build_dir().unwrap(), domain_root.join("bazel-out"));
        assert_eq!(
            context.get_build_config_file().unwrap(),
            domain_root.join(".fuchsia-build-config.json")
        );
        assert_matches!(context.get_sdk_root().await.unwrap(), SdkRoot::Full(path) if path == domain_root.join("bazel-out/external/fuchsia_sdk"));
    }

    fn check_isolated_paths(context: &EnvironmentContext, isolate_dir: &Path) {
        assert_eq!(
            context.get_default_user_file_path().unwrap(),
            isolate_dir.join(crate::paths::USER_FILE)
        );
        assert_eq!(
            context.get_default_env_path().unwrap(),
            isolate_dir.join(crate::paths::ENV_FILE)
        );
        assert_eq!(context.get_default_ascendd_path().unwrap(), isolate_dir.join("daemon.sock"));
        assert_eq!(context.get_runtime_path().unwrap(), isolate_dir.join("runtime"));
        assert_eq!(context.get_cache_path().unwrap(), isolate_dir.join("cache"));
        assert_eq!(context.get_config_path().unwrap(), isolate_dir.join("config"));
        assert_eq!(context.get_data_path().unwrap(), isolate_dir.join("data"));
        assert!(context.is_isolated());
    }
}
