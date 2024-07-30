// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Result;
use ffx_config::EnvironmentContext;
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::{fs, process};

use crate::{RegistrationConflictMode, RepoStorageType};

/// PathType is an enum encapulating filesystem and URL based paths.
#[derive(Debug, Deserialize, Serialize, PartialEq)]
pub enum PathType {
    File(PathBuf),
    Url(String),
}

impl From<&Path> for PathType {
    fn from(value: &Path) -> Self {
        PathType::File(value.into())
    }
}

/// ServerMode is the execution mode of the server process.
#[derive(Debug, Deserialize, Serialize, PartialEq)]
pub enum ServerMode {
    Background,
    Foreground,
    Daemon,
}

/// PkgServerInfo is serialized as a file that contains
/// the startup information for a running package server.
/// This includes the process id, which is intended for
/// use to troubleshoot and stop running instances.
#[derive(Debug, Deserialize, Serialize)]
pub struct PkgServerInfo {
    pub name: String,
    pub address: SocketAddr,
    pub repo_path: PathType,
    pub registration_aliases: Vec<String>,
    pub registration_storage_type: RepoStorageType,
    pub registration_alias_conflict_mode: RegistrationConflictMode,
    pub server_mode: ServerMode,
    pub pid: u32,
}

impl PkgServerInfo {
    /// Returns true if the process identified by the pid is running.
    fn is_running(&self) -> bool {
        if self.pid != 0 {
            // First do a no-hang wait to collect the process if it's defunct.
            let _ = nix::sys::wait::waitpid(
                nix::unistd::Pid::from_raw(self.pid.try_into().unwrap()),
                Some(nix::sys::wait::WaitPidFlag::WNOHANG),
            );
            // Check to see if it is running by sending signal 0. If there is no error,
            // the process is running.
            return nix::sys::signal::kill(
                nix::unistd::Pid::from_raw(self.pid.try_into().unwrap()),
                None,
            )
            .is_ok();
        }
        return false;
    }
}

/// PkgServerInstanceInfo is the trait implemented by
/// objects that manage package server instance info.
pub trait PkgServerInstanceInfo {
    /// Returns a list of running package servers.
    fn list_instances(&self) -> Result<Vec<PkgServerInfo>>;

    /// Returns the information for the package server with the
    /// provided name, or None if there is no running server with that
    /// name.
    fn get_instance(&self, name: String) -> Result<Option<PkgServerInfo>>;

    ///  Removes the instance information for the server with the given name.
    /// If the name does not exist, it is ignored.
    fn remove_instance(&self, name: String) -> Result<()>;

    /// Writes the instance information provided.
    fn write_instance(&self, instance: &PkgServerInfo) -> Result<()>;
}

pub struct PkgServerInstances {
    instance_root: PathBuf,
}

impl PkgServerInstances {
    pub fn new(instance_root: PathBuf) -> Self {
        PkgServerInstances { instance_root }
    }
}

impl PkgServerInstanceInfo for PkgServerInstances {
    fn list_instances(&self) -> Result<Vec<PkgServerInfo>> {
        let mut instances = Vec::<PkgServerInfo>::new();
        let root = self.instance_root.as_path();
        if root.is_dir() {
            for entry in root.read_dir()? {
                if let Ok(entry) = entry {
                    if entry.path().is_dir() {
                        continue;
                    }
                    if entry.path().extension().unwrap_or_default() == "json" {
                        // If there is a problem reading the file, return the error.
                        let data = fs::read(entry.path())?;

                        match serde_json::from_slice::<PkgServerInfo>(&data) {
                            Ok(info) => {
                                if info.is_running() {
                                    instances.push(info);
                                } else {
                                    fs::remove_file(entry.path())?;
                                }
                            }
                            Err(e) => {
                                tracing::warn!(
                                    "Error deserializing {:?} into PkgServerInfo: {e}",
                                    entry.path()
                                );
                                let mut bad_name = entry.path().clone();
                                bad_name.set_extension(".json.bad");
                                fs::rename(entry.path(), bad_name)?;
                            }
                        };
                    }
                }
            }
        }
        Ok(instances)
    }

    fn get_instance(&self, name: String) -> Result<Option<PkgServerInfo>> {
        let mut instance_file = self.instance_root.join(name);
        instance_file.set_extension("json");
        if !instance_file.exists() {
            return Ok(None);
        }
        let data = fs::read(&instance_file)?;

        match serde_json::from_slice::<PkgServerInfo>(&data) {
            Ok(info) => {
                if info.is_running() {
                    Ok(Some(info))
                } else {
                    fs::remove_file(&instance_file)?;
                    Ok(None)
                }
            }
            Err(e) => {
                tracing::warn!("Error deserializing {:?} into PkgServerInfo: {e}", instance_file);
                fs::remove_file(instance_file)?;
                Ok(None)
            }
        }
    }

    fn remove_instance(&self, name: String) -> Result<()> {
        let mut instance_file = self.instance_root.join(name);
        instance_file.set_extension("json");
        if instance_file.exists() {
            fs::remove_file(instance_file).map_err(Into::into)
        } else {
            Ok(())
        }
    }

    fn write_instance(&self, instance: &PkgServerInfo) -> Result<()> {
        if !self.instance_root.exists() {
            fs::create_dir_all(&self.instance_root)?;
        }
        let mut instance_file = self.instance_root.join(&instance.name);
        instance_file.set_extension("json");
        let contents = serde_json::to_string_pretty(&instance)?;
        fs::write(instance_file, &contents).map_err(Into::into)
    }
}

pub async fn write_instance_info(
    env_context: Option<EnvironmentContext>,
    server_mode: ServerMode,
    name: &str,
    address: &SocketAddr,
    repo_path: PathType,
    aliases: Vec<String>,
    storage_type: RepoStorageType,
    conflict_mode: RegistrationConflictMode,
) -> Result<()> {
    let instance_root = if let Some(context) = env_context {
        context.get("repository.process_dir")?
    } else {
        ffx_config::get("repository.process_dir").await?
    };
    let mgr = PkgServerInstances::new(instance_root);

    let info = PkgServerInfo {
        name: name.into(),
        address: address.clone(),
        repo_path: repo_path,
        registration_aliases: aliases.clone(),
        registration_storage_type: storage_type.into(),
        registration_alias_conflict_mode: conflict_mode.into(),
        server_mode,
        pid: process::id(),
    };
    if let Some(existing) = mgr.get_instance(name.into())? {
        let message = format!("WARNING: Overwriting server info for {name}: {existing:?}");
        tracing::error!("{message}");
    }
    mgr.write_instance(&info).map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv6Addr;
    use std::process;

    #[fuchsia::test]
    async fn test_write_instance_info_with_global_context() {
        let env = ffx_config::test_init().await.expect("test env");
        let addr = SocketAddr::new(std::net::IpAddr::V6(std::net::Ipv6Addr::UNSPECIFIED), 8000);
        write_instance_info(
            None,
            ServerMode::Foreground,
            "somename",
            &addr,
            PathBuf::new().as_path().into(),
            vec![],
            RepoStorageType::Ephemeral,
            RegistrationConflictMode::ErrorOut,
        )
        .await
        .expect("write_instance_info");

        let mgr = PkgServerInstances::new(
            env.context
                .get::<PathBuf, &str>("repository.process_dir")
                .expect("expected repo.process_dir"),
        );

        let instances = mgr.list_instances().expect("list_instances");
        assert_eq!(instances.len(), 1);
    }
    #[fuchsia::test]
    async fn test_write_instance_info_with_context() {
        let env = ffx_config::test_init().await.expect("test env");
        let addr = SocketAddr::new(std::net::IpAddr::V6(std::net::Ipv6Addr::UNSPECIFIED), 8000);
        write_instance_info(
            Some(env.context.clone()),
            ServerMode::Foreground,
            "somename",
            &addr,
            PathBuf::new().as_path().into(),
            vec![],
            RepoStorageType::Ephemeral,
            RegistrationConflictMode::ErrorOut,
        )
        .await
        .expect("write_instance_info");

        let mgr = PkgServerInstances::new(
            env.context
                .get::<PathBuf, &str>("repository.process_dir")
                .expect("expected repo.process_dir"),
        );

        let instances = mgr.list_instances().expect("list_instances");
        assert_eq!(instances.len(), 1);
    }
    #[test]
    fn test_is_running() {
        let info = PkgServerInfo {
            name: "somename".into(),
            address: (Ipv6Addr::UNSPECIFIED, 8000).into(),
            repo_path: Path::new("").into(),
            registration_alias_conflict_mode: RegistrationConflictMode::ErrorOut,
            server_mode: ServerMode::Foreground,
            pid: process::id(),
            registration_aliases: vec![],
            registration_storage_type: RepoStorageType::Ephemeral,
        };
        assert!(info.is_running());
    }

    #[test]
    fn test_list_not_exist() {
        let mgr = PkgServerInstances::new("/some/path".into());

        let instances = mgr.list_instances().expect("list_instances");
        assert!(instances.is_empty());
    }

    #[test]
    fn test_list_empty() {
        let tmp = tempfile::TempDir::new().expect("temp dir");
        let mgr = PkgServerInstances::new(tmp.path().into());

        let instances = mgr.list_instances().expect("list_instances");
        assert!(instances.is_empty());
    }

    #[test]
    fn test_write_not_exist() {
        let tmp = tempfile::TempDir::new().expect("temp dir");
        let mgr = PkgServerInstances::new(tmp.path().join("instances").into());
        let instance_name = "some_instance";
        let info = PkgServerInfo {
            name: instance_name.into(),
            address: (Ipv6Addr::UNSPECIFIED, 8000).into(),
            repo_path: Path::new("").into(),
            registration_alias_conflict_mode: RegistrationConflictMode::ErrorOut,
            server_mode: ServerMode::Foreground,
            pid: process::id(),
            registration_aliases: vec![],
            registration_storage_type: RepoStorageType::Ephemeral,
        };

        mgr.write_instance(&info).expect("written OK");
        mgr.get_instance(instance_name.into()).expect("get instance");
    }

    #[test]
    fn test_writer_overwrite() {
        let tmp = tempfile::TempDir::new().expect("temp dir");
        let mgr = PkgServerInstances::new(tmp.path().join("instances").into());
        let instance_name = "some_instance";
        let mut info = PkgServerInfo {
            name: instance_name.into(),
            address: (Ipv6Addr::UNSPECIFIED, 8000).into(),
            repo_path: PathBuf::from("path1").as_path().into(),
            registration_alias_conflict_mode: RegistrationConflictMode::ErrorOut,
            server_mode: ServerMode::Foreground,
            pid: process::id(),
            registration_aliases: vec![],
            registration_storage_type: RepoStorageType::Ephemeral,
        };

        mgr.write_instance(&info).expect("written OK");

        let got = mgr.get_instance(instance_name.into()).expect("get instance").unwrap();
        assert_eq!(got.repo_path, PathBuf::from("path1").as_path().into());

        info.repo_path = PathBuf::from("path2").as_path().into();

        mgr.write_instance(&info).expect("written OK");

        let mut got = mgr.get_instance(instance_name.into()).expect("get instance").unwrap();
        assert_eq!(got.repo_path, PathBuf::from("path2").as_path().into());

        let mut instances = mgr.list_instances().expect("list instances");
        assert_eq!(instances.len(), 1);
        got = instances.pop().unwrap();
        assert_eq!(got.name, instance_name);
    }

    #[test]
    fn test_list_1() {
        let tmp = tempfile::TempDir::new().expect("temp dir");
        let mgr = PkgServerInstances::new(tmp.path().join("instances").into());
        let instance_name = "some_instance";
        let info = PkgServerInfo {
            name: instance_name.into(),
            address: (Ipv6Addr::UNSPECIFIED, 8000).into(),
            repo_path: Path::new("").into(),
            registration_alias_conflict_mode: RegistrationConflictMode::ErrorOut,
            server_mode: ServerMode::Foreground,
            pid: process::id(),
            registration_aliases: vec![],
            registration_storage_type: RepoStorageType::Ephemeral,
        };

        mgr.write_instance(&info).expect("written OK");
        let mut instances = mgr.list_instances().expect("list instance");
        assert_eq!(instances.len(), 1);
        let got = instances.pop().unwrap();
        assert_eq!(got.name, instance_name);
        assert_eq!(got.pid, info.pid);
    }

    #[test]
    fn test_list_2() {
        let tmp = tempfile::TempDir::new().expect("temp dir");
        let mgr = PkgServerInstances::new(tmp.path().join("instances").into());
        let instance_name = "some_instance";
        let info_1 = PkgServerInfo {
            name: instance_name.into(),
            address: (Ipv6Addr::UNSPECIFIED, 8000).into(),
            repo_path: Path::new("").into(),
            registration_alias_conflict_mode: RegistrationConflictMode::ErrorOut,
            server_mode: ServerMode::Foreground,
            pid: process::id(),
            registration_aliases: vec![],
            registration_storage_type: RepoStorageType::Ephemeral,
        };

        let another_instance_name = "another_instance";
        let info_2 = PkgServerInfo {
            name: another_instance_name.into(),
            address: (Ipv6Addr::UNSPECIFIED, 8000).into(),
            repo_path: Path::new("").into(),
            registration_alias_conflict_mode: RegistrationConflictMode::ErrorOut,
            server_mode: ServerMode::Foreground,
            pid: process::id(),
            registration_aliases: vec![],
            registration_storage_type: RepoStorageType::Ephemeral,
        };
        mgr.write_instance(&info_1).expect("written OK");
        mgr.write_instance(&info_2).expect("written OK");
        let mut instances = mgr.list_instances().expect("list instance");
        assert_eq!(instances.len(), 2);
        let got = instances.pop().unwrap();
        let other = instances.pop().unwrap();
        // order is not defined
        if got.name == instance_name {
            assert_eq!(other.name, another_instance_name);
        } else if got.name == another_instance_name {
            assert_eq!(other.name, instance_name);
        }
    }

    #[test]
    fn test_list_1_not_running() {
        let tmp = tempfile::TempDir::new().expect("temp dir");
        let mgr = PkgServerInstances::new(tmp.path().join("instances").into());
        let instance_name = "some_instance";
        let info_1 = PkgServerInfo {
            name: instance_name.into(),
            address: (Ipv6Addr::UNSPECIFIED, 8000).into(),
            repo_path: Path::new("").into(),
            registration_alias_conflict_mode: RegistrationConflictMode::ErrorOut,
            server_mode: ServerMode::Foreground,
            pid: process::id(),
            registration_aliases: vec![],
            registration_storage_type: RepoStorageType::Ephemeral,
        };

        let another_instance_name = "another_instance";
        let info_2 = PkgServerInfo {
            name: another_instance_name.into(),
            address: (Ipv6Addr::UNSPECIFIED, 8000).into(),
            repo_path: Path::new("").into(),
            registration_alias_conflict_mode: RegistrationConflictMode::ErrorOut,
            server_mode: ServerMode::Foreground,
            pid: 0,
            registration_aliases: vec![],
            registration_storage_type: RepoStorageType::Ephemeral,
        };
        mgr.write_instance(&info_1).expect("written OK");
        mgr.write_instance(&info_2).expect("written OK");
        let mut instances = mgr.list_instances().expect("list instance");
        assert_eq!(instances.len(), 1);
        let got = instances.pop().unwrap();
        assert_eq!(got.name, instance_name);
        assert_eq!(got.pid, info_1.pid);
    }

    #[test]
    fn test_get_instance_dir_not_exist() {
        let tmp = tempfile::TempDir::new().expect("temp dir");
        let mgr = PkgServerInstances::new(tmp.path().join("instances").into());

        let got = mgr.get_instance("some_instance".into()).expect("get_instance");
        assert!(got.is_none());
    }

    #[test]
    fn test_get_instance_unknown() {
        let tmp = tempfile::TempDir::new().expect("temp dir");
        let mgr = PkgServerInstances::new(tmp.path().into());

        let got = mgr.get_instance("some_instance".into()).expect("get_instance");
        assert!(got.is_none());
    }

    #[test]
    fn test_get_instance_not_running() {
        let tmp = tempfile::TempDir::new().expect("temp dir");
        let mgr = PkgServerInstances::new(tmp.path().join("instances").into());
        let instance_name = "some_instance";
        let info = PkgServerInfo {
            name: instance_name.into(),
            address: (Ipv6Addr::UNSPECIFIED, 8000).into(),
            repo_path: Path::new("").into(),
            registration_alias_conflict_mode: RegistrationConflictMode::ErrorOut,
            server_mode: ServerMode::Foreground,
            pid: 0,
            registration_aliases: vec![],
            registration_storage_type: RepoStorageType::Ephemeral,
        };

        mgr.write_instance(&info).expect("written OK");
        let got = mgr.get_instance(instance_name.into()).expect("get instance");
        assert!(got.is_none());
    }

    #[test]
    fn test_remove_dir_not_exist() {
        let tmp = tempfile::TempDir::new().expect("temp dir");
        let mgr = PkgServerInstances::new(tmp.path().join("instances").into());
        let instance_name = "some_instance";

        mgr.remove_instance(instance_name.into()).expect("remove OK");
    }

    #[test]
    fn test_remove_running() {
        // This tests the case of the current process cleaning up itself. The process is still running, so the file
        // is still there and should be removed.
        let tmp = tempfile::TempDir::new().expect("temp dir");
        let mgr = PkgServerInstances::new(tmp.path().join("instances").into());

        let instance_name = "some_instance";
        let info = PkgServerInfo {
            name: instance_name.into(),
            address: (Ipv6Addr::UNSPECIFIED, 8000).into(),
            repo_path: Path::new("").into(),
            registration_alias_conflict_mode: RegistrationConflictMode::ErrorOut,
            server_mode: ServerMode::Foreground,
            pid: process::id(),
            registration_aliases: vec![],
            registration_storage_type: RepoStorageType::Ephemeral,
        };

        mgr.write_instance(&info).expect("written OK");

        mgr.remove_instance(instance_name.into()).expect("remove OK");

        assert!(mgr.get_instance(instance_name.into()).expect("get instance").is_none());
    }

    #[test]
    fn test_remove_not_running() {
        let tmp = tempfile::TempDir::new().expect("temp dir");
        let mgr = PkgServerInstances::new(tmp.path().join("instances").into());

        let instance_name = "some_instance";
        let info = PkgServerInfo {
            name: instance_name.into(),
            address: (Ipv6Addr::UNSPECIFIED, 8000).into(),
            repo_path: Path::new("").into(),
            registration_alias_conflict_mode: RegistrationConflictMode::ErrorOut,
            server_mode: ServerMode::Foreground,
            pid: 0,
            registration_aliases: vec![],
            registration_storage_type: RepoStorageType::Ephemeral,
        };

        mgr.write_instance(&info).expect("written OK");

        mgr.remove_instance(instance_name.into()).expect("remove OK");

        assert!(mgr.get_instance(instance_name.into()).expect("get instance").is_none());
    }

    #[test]
    fn test_remove_not_found() {
        let tmp = tempfile::TempDir::new().expect("temp dir");
        let mgr = PkgServerInstances::new(tmp.path().join("instances").into());

        let instance_name = "some_instance";
        let another_instance_name = "another_instance";
        let info = PkgServerInfo {
            name: instance_name.into(),
            address: (Ipv6Addr::UNSPECIFIED, 8000).into(),
            repo_path: Path::new("").into(),
            registration_alias_conflict_mode: RegistrationConflictMode::ErrorOut,
            server_mode: ServerMode::Foreground,
            pid: process::id(),
            registration_aliases: vec![],
            registration_storage_type: RepoStorageType::Ephemeral,
        };

        mgr.write_instance(&info).expect("written OK");

        mgr.remove_instance(another_instance_name.into()).expect("remove OK");

        assert!(mgr.get_instance(instance_name.into()).expect("get instance").is_some());
        assert!(mgr.get_instance(another_instance_name.into()).expect("get instance").is_none());
    }
}
