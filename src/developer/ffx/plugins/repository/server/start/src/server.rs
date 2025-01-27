// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::server_impl::{get_repo_base_name, serve_impl, REPO_BACKGROUND_FEATURE_FLAG};
use crate::ServerStartTool;
use ffx_command_error::{bug, return_user_error, user_error, FfxContext as _, Result};
use ffx_config::EnvironmentContext;
use ffx_repository_server_start_args::StartCommand;
use fho::{Deferred, FfxMain};
use fidl_fuchsia_developer_ffx as ffx;
use fidl_fuchsia_developer_remotecontrol::RemoteControlProxy;
use pkg::{PkgServerInstanceInfo, PkgServerInstances, ServerMode};
use std::time::Duration;
use target_connector::Connector;
use target_holders::TargetProxyHolder;

pub(crate) fn to_argv(cmd: &StartCommand) -> Vec<String> {
    let mut argv: Vec<String> = vec![];
    if let Some(addr) = cmd.address {
        argv.extend_from_slice(&["--address".into(), addr.to_string()]);
    }
    for a in &cmd.alias {
        argv.extend_from_slice(&["--alias".into(), a.to_string()]);
    }
    let mode = match cmd.alias_conflict_mode {
        ffx::RepositoryRegistrationAliasConflictMode::ErrorOut => "error-out".to_string(),
        ffx::RepositoryRegistrationAliasConflictMode::Replace => "replace".to_string(),
    };
    argv.extend_from_slice(&["--alias-conflict-mode".into(), mode]);

    if cmd.no_device {
        argv.push("--no-device".into());
    }

    if let Some(pb) = &cmd.product_bundle {
        argv.extend_from_slice(&["--product-bundle".into(), pb.as_str().to_string()]);
    }

    if let Some(p) = &cmd.repo_path {
        argv.extend_from_slice(&["--repo-path".into(), p.as_str().to_string()]);
    }
    if let Some(name) = &cmd.repository {
        argv.extend_from_slice(&["--repository".into(), name.to_string()]);
    }
    if let Some(s) = cmd.storage_type {
        let st = match s {
            ffx::RepositoryStorageType::Ephemeral => "ephemeral",
            ffx::RepositoryStorageType::Persistent => "Persistent",
        };
        argv.extend_from_slice(&["--storage-type".into(), st.to_string()]);
    }

    if let Some(p) = &cmd.trusted_root {
        argv.extend_from_slice(&["--trusted-root".into(), p.as_str().to_string()]);
    }
    if cmd.refresh_metadata {
        argv.push("--refresh-metadata".into());
    }
    if let Some(manifest) = &cmd.auto_publish {
        argv.extend_from_slice(&["--auto-publish".into(), manifest.as_str().to_string()]);
    }
    argv
}

pub async fn run_foreground_server(
    start_cmd: StartCommand,
    context: EnvironmentContext,
    target_proxy_connector: Connector<TargetProxyHolder>,
    rcs_proxy_connector: Connector<RemoteControlProxy>,
    repos: Deferred<ffx::RepositoryRegistryProxy>,
    w: <ServerStartTool as FfxMain>::Writer,
    mode: ServerMode,
) -> Result<()> {
    /* This check is specific to the `ffx repository serve` command and should be ignored
        if the entry point is `ffx repository server start`.
    */
    // TODO(b/389735589): Remove the daemon based repo server.
    let bg: bool =
        context.get(REPO_BACKGROUND_FEATURE_FLAG).bug_context("checking for daemon server flag")?;
    if bg {
        return_user_error!(
            r#"The ffx setting '{}' and the foreground server are mutually incompatible.
    Please disable background serving by running the following commands:
    $ ffx config remove repository.server.enabled
    $ ffx doctor --restart-daemon"#,
            REPO_BACKGROUND_FEATURE_FLAG,
        );
    }

    serve_impl(
        target_proxy_connector,
        rcs_proxy_connector,
        repos,
        start_cmd,
        context,
        w.simple_writer(),
        mode,
    )
    .await
}

pub(crate) async fn wait_for_start(
    context: EnvironmentContext,
    cmd: StartCommand,
    time_to_wait: Duration,
) -> Result<()> {
    let instance_root =
        context.get("repository.process_dir").map_err(|e: ffx_config::api::ConfigError| bug!(e))?;
    let mgr = PkgServerInstances::new(instance_root);

    let repo_base_name = get_repo_base_name(&cmd.repository, &context)?;
    tracing::debug!("waiting up to {time_to_wait:?} for {repo_base_name} to start.");
    timeout::timeout(time_to_wait, async move {
        loop {
            match mgr.list_instances() {
                Ok(running_instances) => {
                    if running_instances
                        .iter()
                        .any(|instance| instance.name.starts_with(&repo_base_name))
                    {
                        return Ok(());
                    }
                    tracing::debug!(
                        "waiting for {repo_base_name} to start. Got: {running_instances:?}"
                    );
                }
                Err(e) => {
                    tracing::debug!(
                        "list_instances returned {e} wait for {repo_base_name} to start."
                    );
                }
            }
            fuchsia_async::Timer::new(std::time::Duration::from_secs(1)).await;
        }
    })
    .await
    .map_err(|e| user_error!(e))?
}

#[cfg(test)]
mod test {
    use super::*;
    use camino::Utf8PathBuf;
    use ffx::{RepositoryRegistrationAliasConflictMode, RepositoryStorageType};
    use std::str::FromStr as _;

    #[fuchsia::test]
    fn test_to_argv() {
        let start_cmd = StartCommand {
            address: Some(([127, 0, 0, 1], 8787).into()),
            background: true,
            daemon: false,
            foreground: false,
            disconnected: false,
            repository: Some("repo-name".into()),
            trusted_root: Some(Utf8PathBuf::from_str("/trusted/root").expect("UTF8 path")),
            repo_path: Some(Utf8PathBuf::from_str("/repo/path/root").expect("UTF8 path")),
            product_bundle: Some(Utf8PathBuf::from_str("/product/bundle/path").expect("UTF8 path")),
            alias: vec!["alias1".into(), "alias2".into()],
            storage_type: Some(RepositoryStorageType::Ephemeral),
            alias_conflict_mode: RepositoryRegistrationAliasConflictMode::Replace,
            port_path: Some("/path/port/file".into()),
            no_device: false,
            refresh_metadata: true,
            auto_publish: Some(Utf8PathBuf::from_str("/auto/publish/list").expect("UTF8 path")),
        };
        let actual = to_argv(&start_cmd);
        let expected: Vec<String> = [
            "--address",
            "127.0.0.1:8787",
            "--alias",
            "alias1",
            "--alias",
            "alias2",
            "--alias-conflict-mode",
            "replace",
            "--product-bundle",
            "/product/bundle/path",
            "--repo-path",
            "/repo/path/root",
            "--repository",
            "repo-name",
            "--storage-type",
            "ephemeral",
            "--trusted-root",
            "/trusted/root",
            "--refresh-metadata",
            "--auto-publish",
            "/auto/publish/list",
        ]
        .iter()
        .map(ToString::to_string)
        .collect();

        assert_eq!(actual, expected);
    }
}
