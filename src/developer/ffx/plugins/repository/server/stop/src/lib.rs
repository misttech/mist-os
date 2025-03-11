// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::time::Duration;

use async_trait::async_trait;
use ffx_config::api::ConfigError;
use ffx_config::EnvironmentContext;
use ffx_repository_server_stop_args::StopCommand;
use ffx_writer::VerifiedMachineWriter;
use fho::{bug, return_bug, return_user_error, Error, FfxMain, FfxTool, Result};
use pkg::{PkgServerInfo, PkgServerInstanceInfo as _, PkgServerInstances};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, Serialize, JsonSchema, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum CommandStatus {
    /// Successful execution with an optional informational string.
    Ok { message: String },
    /// Unexpected error with string.
    UnexpectedError { message: String },
    /// A known kind of error that can be reported usefully to the user
    UserError { message: String },
}
#[derive(FfxTool)]
pub struct RepoStopTool {
    #[command]
    cmd: StopCommand,
    context: EnvironmentContext,
}

fho::embedded_plugin!(RepoStopTool);

#[async_trait(?Send)]
impl FfxMain for RepoStopTool {
    type Writer = VerifiedMachineWriter<CommandStatus>;
    async fn main(self, mut writer: Self::Writer) -> Result<()> {
        match self.stop().await {
            Ok(info) => {
                let message = info.unwrap_or_else(|| "Stopped the repository server".into());
                writer.machine_or(&CommandStatus::Ok { message: message.clone() }, message)?;
                Ok(())
            }
            Err(e @ Error::User(_)) => {
                writer.machine(&CommandStatus::UserError { message: e.to_string() })?;
                Err(e)
            }
            Err(e) => {
                writer.machine(&CommandStatus::UnexpectedError { message: e.to_string() })?;
                Err(e)
            }
        }
    }
}

impl RepoStopTool {
    pub async fn stop(self) -> Result<Option<String>> {
        let instance_root =
            self.context.get("repository.process_dir").map_err(|e: ConfigError| bug!(e))?;
        let mgr = PkgServerInstances::new(instance_root);
        let instances: Vec<PkgServerInfo> = mgr.list_instances()?;
        if instances.is_empty() {
            return Ok(Some("no running servers".into()));
        }
        let repo_port = self.cmd.port;

        if self.cmd.all {
            for instance in instances {
                Self::stop_instance(&instance).await?;
            }
            return Ok(None);
        } else if let Some(repo_name) = &self.cmd.name {
            if let Some(instance) = instances.iter().find(|s| {
                &s.name == repo_name && (repo_port.is_none() || repo_port.unwrap() == s.port())
            }) {
                return Self::stop_instance(instance).await;
            } else {
                return_user_error!("no running server named {repo_name} is found.");
            }
        } else if let Some(product_bundle) = &self.cmd.product_bundle {
            if let Some(instance) = instances.iter().find(|s| {
                s.repo_path_display() == *product_bundle
                    && (repo_port.is_none() || repo_port.unwrap() == s.port())
            }) {
                return Self::stop_instance(instance).await;
            } else {
                return_user_error!(
                    "no running server serving a product bundle {product_bundle} is found."
                );
            }
        } else {
            match instances.len() {
            0 => return Ok(Some("no running servers".into())),
            1 => return {
                let instance = instances.get(0).unwrap();
                Self::stop_instance(instance).await
            },
            _ => return_user_error!("more than 1 server running. Use --all or specify the name and port (if needed) of the server to stop.")
        }
        }
    }

    async fn stop_instance(instance: &PkgServerInfo) -> Result<Option<String>> {
        match instance.terminate(Duration::from_secs(3)).await {
            Ok(_) => Ok(None),
            Err(e) => return_bug!("Could not terminate server: {e}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use camino::Utf8PathBuf;
    use ffx_config::{ConfigLevel, TestEnv};
    use fidl_fuchsia_developer_ffx_ext::RepositorySpec;
    use fidl_fuchsia_pkg_ext::{
        RepositoryConfigBuilder, RepositoryRegistrationAliasConflictMode, RepositoryStorageType,
    };
    use pkg::ServerMode;
    use std::collections::BTreeSet;
    use std::fs;
    use std::net::Ipv4Addr;
    use std::os::unix::fs::PermissionsExt as _;
    use std::process::{Child, Command};

    const FAKE_SERVER_CONTENTS: &str = r#"#!/bin/bash
       while sleep 1s
       do
         echo "."
       done
    "#;

    fn make_standalone_instance(
        name: String,
        product_bundle_path: Option<Utf8PathBuf>,
        context: &EnvironmentContext,
        test_env: &TestEnv,
    ) -> Result<(PkgServerInstances, Child)> {
        let fake_server = test_env.isolate_root.path().join(format!("{name}_fake_server.sh"));
        // write out the shell script
        fs::write(&fake_server, FAKE_SERVER_CONTENTS).expect("writing fake server");
        let mut perm =
            fs::metadata(&fake_server).expect("Failed to get test server metadata").permissions();

        perm.set_mode(0o755);
        fs::set_permissions(&fake_server, perm).expect("Failed to set permissions on test runner");

        let child = Command::new(fake_server).spawn().expect("child process");

        let instance_root = context.get("repository.process_dir").expect("instance dir");
        let mgr = PkgServerInstances::new(instance_root);

        let repo_config =
            RepositoryConfigBuilder::new(format!("fuchsia-pkg://{name}").parse().unwrap()).build();

        let address = (Ipv4Addr::LOCALHOST, 1234).into();

        let repo_path: Utf8PathBuf =
            if let Some(pb) = product_bundle_path { pb } else { Utf8PathBuf::from("/somewhere") };

        mgr.write_instance(&PkgServerInfo {
            name,
            address,
            repo_spec: RepositorySpec::Pm { path: repo_path, aliases: BTreeSet::new() }.into(),
            registration_storage_type: RepositoryStorageType::Ephemeral,
            registration_alias_conflict_mode: RepositoryRegistrationAliasConflictMode::ErrorOut,
            server_mode: ServerMode::Foreground,
            pid: child.id(),
            repo_config,
        })
        .expect("writing instance");
        Ok((mgr, child))
    }

    #[fuchsia::test]
    async fn test_standalone_stop() {
        let env = ffx_config::test_init().await.unwrap();
        env.context
            .query("repository.process_dir")
            .level(Some(ConfigLevel::User))
            .set(env.isolate_root.path().join("repo_servers").to_string_lossy().into())
            .await
            .expect("setting isolated process dir");

        let (_mgr, _server_proc) =
            make_standalone_instance("default".into(), None, &env.context, &env)
                .expect("test daemon instance");

        let tool = RepoStopTool {
            context: env.context.clone(),
            cmd: StopCommand { all: true, name: None, port: None, product_bundle: None },
        };
        let buffers = ffx_writer::TestBuffers::default();
        let writer = <RepoStopTool as FfxMain>::Writer::new_test(None, &buffers);
        let res = tool.main(writer).await;

        let (stdout, stderr) = buffers.into_strings();
        assert_eq!(stdout, "Stopped the repository server\n");
        assert_eq!(stderr, "");
        assert!(res.is_ok());
    }

    #[fuchsia::test]
    async fn test_product_bundle_stop() {
        let env = ffx_config::test_init().await.unwrap();
        env.context
            .query("repository.process_dir")
            .level(Some(ConfigLevel::User))
            .set(env.isolate_root.path().join("repo_servers").to_string_lossy().into())
            .await
            .expect("setting isolated process dir");

        let product_bundle_path =
            Utf8PathBuf::from_path_buf(env.isolate_root.path().join("pb")).expect("utf8 path");

        let (_mgr, mut server_proc) = make_standalone_instance(
            "some-pb.com".into(),
            Some(product_bundle_path.clone()),
            &env.context,
            &env,
        )
        .expect("test daemon instance");

        let tool = RepoStopTool {
            context: env.context.clone(),
            cmd: StopCommand {
                all: false,
                name: None,
                port: None,
                product_bundle: Some(product_bundle_path),
            },
        };
        let buffers = ffx_writer::TestBuffers::default();
        let writer = <RepoStopTool as FfxMain>::Writer::new_test(None, &buffers);
        let res = tool.main(writer).await;
        let (stdout, stderr) = buffers.into_strings();

        // clean up the server process, if still present
        let _ = server_proc.kill();
        assert!(res.is_ok(), "Expected ok, got {res:?} {stdout} {stderr}");
        assert_eq!(stdout, "Stopped the repository server\n", "stderr: {stderr}");
        assert_eq!(stderr, "");
        assert!(res.is_ok());
    }
}
