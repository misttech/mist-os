// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{anyhow, Context as _, Result};
use ffx_config::{EnvironmentContext, SdkRoot};
use fuchsia_async;
use sdk::FfxSdkConfig;
use serde::Serialize;
use std::{
    borrow::Cow,
    collections::HashMap,
    path::{Path, PathBuf},
    process::{Child, ExitStatus, Stdio},
    time::SystemTime,
};
use tempfile::TempDir;

/// Where to search for ffx and subtools, based on either being part of an
/// ffx command (like `ffx self-test`) or being part of the build (using the
/// build root to find things in either the host tool or test data targets.
#[derive(Debug, Clone)]
pub enum SearchContext {
    Runtime { ffx_path: PathBuf, sdk_root: Option<SdkRoot> },
    Build { build_root: PathBuf },
}

fn env_search_paths(search: &SearchContext) -> Vec<Cow<'_, Path>> {
    use SearchContext::*;
    match search {
        Runtime { ffx_path, .. } => {
            if let Some(parent) = ffx_path.parent() {
                return vec![Cow::from(parent)];
            }
        }
        Build { build_root } => {
            // The build passes these search paths in so that when this is run from
            // a unit test we can find the path that ffx subtools exist at from
            // the build root.
            return vec![
                Cow::Owned(build_root.join(std::env!("SUBTOOL_SEARCH_TEST_DATA"))),
                Cow::Owned(build_root.join(std::env!("SUBTOOL_SEARCH_HOST_TOOLS"))),
            ];
        }
    }
    vec![]
}

fn find_ffx(search: &SearchContext, search_paths: &[Cow<'_, Path>]) -> Result<PathBuf> {
    use SearchContext::*;
    match search {
        Runtime { ffx_path, .. } => return Ok(ffx_path.to_owned()),
        Build { build_root } => {
            for path in search_paths {
                let path = build_root.join(path).join("ffx");
                if path.exists() {
                    return Ok(path);
                }
            }
        }
    }
    Err(anyhow!("ffx not found in search paths for isolation"))
}

#[derive(Debug)]
pub struct CommandOutput {
    pub status: ExitStatus,
    pub stdout: String,
    pub stderr: String,
}

/// Isolate provides an abstraction around an isolated configuration environment for `ffx`.
pub struct Isolate {
    tmpdir: TempDir,
    env_ctx: ffx_config::EnvironmentContext,
}

impl Isolate {
    /// Creates a new isolated environment for ffx to run in, including a
    /// user level configuration that isolates the ascendd socket into a temporary
    /// directory. If $FUCHSIA_TEST_OUTDIR is set, then it is used as the log
    /// directory. The isolated environment is torn down when the Isolate is
    /// dropped, which will attempt to terminate any running daemon and then
    /// remove all isolate files.
    ///
    /// Most of the time you'll want to use the appropriate convenience wrapper,
    /// [`Isolate::new_with_sdk`] or [`Isolate::new_in_test`].
    pub async fn new_with_search(
        name: &str,
        search: SearchContext,
        ssh_key: PathBuf,
    ) -> Result<Isolate> {
        let tmpdir = tempfile::Builder::new().prefix(name).tempdir()?;
        let search_paths = env_search_paths(&search);

        let ffx_path = find_ffx(&search, &search_paths)?;

        let sdk_config = match &search {
            SearchContext::Runtime { sdk_root: Some(sdk_root), .. } => Some(sdk_root.to_config()),
            _ => None,
        };

        let log_dir = if let Ok(d) = std::env::var("FUCHSIA_TEST_OUTDIR") {
            PathBuf::from(d)
        } else {
            tmpdir.path().join("log")
        };

        std::fs::create_dir_all(&log_dir)?;
        let metrics_path = tmpdir.path().join("metrics_home/.fuchsia/metrics");
        std::fs::create_dir_all(&metrics_path)?;

        // TODO(114011): See if we should get isolate-dir itself to deal with metrics isolation.

        // Mark that analytics are disabled
        std::fs::write(metrics_path.join("analytics-status"), "0")?;
        // Mark that the notice has been given
        std::fs::write(metrics_path.join("ffx"), "1")?;

        let mut mdns_discovery = true;
        let mut target_addr = None;
        if let Ok(addr) = std::env::var("FUCHSIA_DEVICE_ADDR") {
            // When run in infra, disable mdns discovery.
            // TODO(fxbug.dev/44710): Remove when we have proper network isolation.
            target_addr = Option::Some(Cow::Owned(addr.to_string() + &":0".to_string()));
            mdns_discovery = false;
        }
        let user_config = UserConfig::for_test(
            log_dir.to_string_lossy(),
            target_addr,
            mdns_discovery,
            search_paths,
            sdk_config,
        );
        std::fs::write(
            tmpdir.path().join(".ffx_user_config.json"),
            serde_json::to_string(&user_config)?,
        )?;

        std::fs::write(
            tmpdir.path().join(".ffx_env"),
            serde_json::to_string(&FfxEnvConfig::for_test(
                tmpdir.path().join(".ffx_user_config.json").to_string_lossy(),
            ))?,
        )?;

        let mut env_vars = HashMap::new();

        // Pass along all temp related variables, so as to avoid anything
        // falling back to writing into /tmp. In our CI environment /tmp is
        // extremely limited, whereas invocations of tests are provided
        // dedicated temporary areas.
        for (var, val) in std::env::vars() {
            if var.contains("TEMP") || var.contains("TMP") {
                let _ = env_vars.insert(var, val);
            }
        }

        let _ = env_vars.insert(
            "HOME".to_owned(),
            tmpdir.path().join("metrics_home").to_string_lossy().to_string(),
        );

        let _ = env_vars.insert(
            ffx_config::EnvironmentContext::FFX_BIN_ENV.to_owned(),
            ffx_path.to_string_lossy().to_string(),
        );

        // On developer systems, FUCHSIA_SSH_KEY is normally not set, and so ffx
        // looks up an ssh key via a $HOME heuristic, however that is broken by
        // isolation. ffx also however respects the FUCHSIA_SSH_KEY environment
        // variable natively, so, fetching the ssh key path from the config, and
        // then passing that expanded path along explicitly is sufficient to
        // ensure that the isolate has a viable key path.
        let _ =
            env_vars.insert("FUCHSIA_SSH_KEY".to_owned(), ssh_key.to_string_lossy().to_string());

        let env_ctx = ffx_config::EnvironmentContext::isolated(
            tmpdir.path().to_owned(),
            env_vars,
            ffx_config::ConfigMap::new(),
            Some(tmpdir.path().join(".ffx_env").to_owned()),
        );

        Ok(Isolate { tmpdir, env_ctx })
    }

    /// Simple wrapper around [`Isolate::new_with_search`] for situations where all you
    /// have is the path to ffx. You should prefer to use [`Isolate::new_with_sdk`] or
    /// [`Isolate::new_in_test`] if you can.
    pub async fn new(name: &str, ffx_path: PathBuf, ssh_key: PathBuf) -> Result<Self> {
        let search = SearchContext::Runtime { ffx_path, sdk_root: None };
        Self::new_with_search(name, search, ssh_key).await
    }

    /// Use this when building an isolation environment from within an ffx subtool
    /// or other situation where there's an sdk involved.
    pub async fn new_with_sdk(
        name: &str,
        ssh_key: PathBuf,
        context: &EnvironmentContext,
    ) -> Result<Self> {
        let ffx_path = context.rerun_bin().await?;
        let ffx_path =
            std::fs::canonicalize(ffx_path).context("could not canonicalize own path")?;

        let sdk_root = context.get_sdk_root().await.ok();

        Self::new_with_search(name, SearchContext::Runtime { ffx_path, sdk_root }, ssh_key).await
    }

    /// Use this when building an isolation environment from within a unit test
    /// in the fuchsia tree. This will make the isolated ffx look for subtools
    /// in the appropriate places in the build tree.
    pub async fn new_in_test(name: &str, build_root: PathBuf, ssh_key: PathBuf) -> Result<Self> {
        Self::new_with_search(name, SearchContext::Build { build_root }, ssh_key).await
    }

    pub fn ascendd_path(&self) -> PathBuf {
        self.tmpdir.path().join("daemon.sock")
    }

    pub async fn ffx_cmd(&self, args: &[&str]) -> Result<std::process::Command> {
        let mut cmd = self.env_ctx.rerun_prefix().await?;
        cmd.args(args);
        Ok(cmd)
    }

    pub async fn ffx_spawn(&self, args: &[&str]) -> Result<Child> {
        let mut cmd = self.ffx_cmd(args).await?;
        let child = cmd.stdout(Stdio::null()).stderr(Stdio::null()).spawn()?;
        Ok(child)
    }

    pub async fn ffx(&self, args: &[&str]) -> Result<CommandOutput> {
        let mut cmd = self.ffx_cmd(args).await?;

        fuchsia_async::unblock(move || {
            let out = cmd.output().context("failed to execute")?;
            let stdout = String::from_utf8(out.stdout).context("convert from utf8")?;
            let stderr = String::from_utf8(out.stderr).context("convert from utf8")?;
            Ok::<_, anyhow::Error>(CommandOutput { status: out.status, stdout, stderr })
        })
        .await
    }
}

#[derive(Serialize, Debug)]
struct UserConfig<'a> {
    log: UserConfigLog<'a>,
    test: UserConfigTest,
    targets: UserConfigTargets<'a>,
    discovery: UserConfigDiscovery,
    ffx: UserConfigFfx<'a>,
    sdk: Option<FfxSdkConfig>,
}

#[derive(Serialize, Debug)]
struct UserConfigLog<'a> {
    enabled: bool,
    dir: Cow<'a, str>,
}

#[derive(Serialize, Debug)]
struct UserConfigFfx<'a> {
    #[serde(rename = "subtool-search-paths")]
    subtool_search_paths: Vec<Cow<'a, Path>>,
}

#[derive(Serialize, Debug)]
struct UserConfigTest {
    #[serde(rename(serialize = "is-isolated"))]
    is_isolated: bool,
}

#[derive(Serialize, Debug)]
struct UserConfigTargets<'a> {
    manual: HashMap<Cow<'a, str>, Option<SystemTime>>,
}

#[derive(Serialize, Debug)]
struct UserConfigDiscovery {
    mdns: UserConfigMdns,
}

#[derive(Serialize, Debug)]
struct UserConfigMdns {
    enabled: bool,
}

impl<'a> UserConfig<'a> {
    fn for_test(
        dir: Cow<'a, str>,
        target: Option<Cow<'a, str>>,
        discovery: bool,
        subtool_search_paths: Vec<Cow<'a, Path>>,
        sdk: Option<FfxSdkConfig>,
    ) -> Self {
        let mut manual_targets = HashMap::new();
        if !target.is_none() {
            manual_targets.insert(target.unwrap(), None);
        }
        Self {
            log: UserConfigLog { enabled: true, dir },
            test: UserConfigTest { is_isolated: true },
            targets: UserConfigTargets { manual: manual_targets },
            discovery: UserConfigDiscovery { mdns: UserConfigMdns { enabled: discovery } },
            ffx: UserConfigFfx { subtool_search_paths },
            sdk,
        }
    }
}

#[derive(Serialize, Debug)]
struct FfxEnvConfig<'a> {
    user: Cow<'a, str>,
    build: Option<&'static str>,
    global: Option<&'static str>,
}

impl<'a> FfxEnvConfig<'a> {
    fn for_test(user: Cow<'a, str>) -> Self {
        Self { user, build: None, global: None }
    }
}
