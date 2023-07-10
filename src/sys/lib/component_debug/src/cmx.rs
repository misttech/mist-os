// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{
        io::{Directory, RemoteDirectory},
        realm::{ExecutionInfo, Instance, InstanceType, ResolvedInfo, Runtime},
    },
    anyhow::{format_err, Context, Result},
    fidl::endpoints::{create_proxy, ServerEnd},
    fidl_fuchsia_io as fio, fidl_fuchsia_sys2 as fsys,
    futures::{
        future::{join, join_all, BoxFuture},
        FutureExt,
    },
    moniker::{AbsoluteMoniker, AbsoluteMonikerBase, ChildMoniker, ChildMonikerBase},
};

/// Reading from the v1/CMX hub is flaky while components are being added/removed.
/// Attempt to get CMX instances several times before calling it a failure.
const CMX_HUB_RETRY_ATTEMPTS: u64 = 10;

pub async fn get_runtime(hub_dir: &RemoteDirectory) -> Result<Runtime> {
    let (job_id, process_id) =
        futures::join!(hub_dir.read_file("job-id"), hub_dir.read_file("process-id"),);

    let job_id = job_id?.parse::<u64>().context("Job ID is not u64")?;

    let process_id = if hub_dir.exists("process-id").await? {
        Some(process_id?.parse::<u64>().context("Process ID is not u64")?)
    } else {
        None
    };

    Ok(Runtime::Elf {
        job_id,
        process_id,
        process_start_time: None,
        process_start_time_utc_estimate: None,
    })
}

pub async fn get_namespace_capabilities(hub_dir: &RemoteDirectory) -> Result<Vec<String>> {
    let in_dir = hub_dir.open_dir_readonly("in")?;
    get_capabilities(in_dir).await
}

pub async fn get_outgoing_capabilities(hub_dir: &RemoteDirectory) -> Result<Vec<String>> {
    let out_dir = hub_dir.open_dir_readonly("out")?;
    get_capabilities(out_dir).await
}

pub async fn get_merkle_root(hub_dir: &RemoteDirectory) -> Result<String> {
    let merkle_root = hub_dir.read_file("in/pkg/meta").await?;
    Ok(merkle_root)
}

// Get all entries in a capabilities directory. If there is a "svc" directory, traverse it and
// collect all protocol names as well.
async fn get_capabilities(capability_dir: RemoteDirectory) -> Result<Vec<String>, anyhow::Error> {
    let mut entries = capability_dir.entry_names().await?;

    for (index, name) in entries.iter().enumerate() {
        if name == "svc" {
            entries.remove(index);
            let svc_dir = capability_dir.open_dir_readonly("svc")?;
            let mut svc_entries = svc_dir.entry_names().await?;
            entries.append(&mut svc_entries);
            break;
        }
    }

    entries.sort_unstable();
    Ok(entries)
}

pub async fn get_all_instances(query: &fsys::RealmQueryProxy) -> Result<Vec<Instance>> {
    // Reading from the v1/CMX hub is flaky while components are being added/removed.
    // Attempt to get CMX instances several times before calling it a failure.
    let mut attempt = 1;
    loop {
        match get_all_instances_internal(query).await {
            Ok(instances) => break Ok(instances),
            Err(e) => {
                if attempt == CMX_HUB_RETRY_ATTEMPTS {
                    break Err(format_err!(
                        "Maximum attempts reached trying to parse CMX realm.\nLast Error: {}",
                        e
                    ));
                }
                attempt += 1;
            }
        }
    }
}

async fn get_all_instances_internal(
    query: &fsys::RealmQueryProxy,
) -> Result<Vec<Instance>, anyhow::Error> {
    let moniker = AbsoluteMoniker::parse_str("/core/appmgr")?;

    let (root_realm_dir, server_end) = create_proxy::<fio::DirectoryMarker>().unwrap();
    let root_realm_dir = RemoteDirectory::from_proxy(root_realm_dir);
    let server_end = ServerEnd::new(server_end.into_channel());

    match query
        .open(
            &moniker.to_string(),
            fsys::OpenDirType::OutgoingDir,
            fio::OpenFlags::RIGHT_READABLE,
            fio::ModeType::empty(),
            "hub",
            server_end,
        )
        .await?
    {
        Ok(()) => parse_cmx_realm(moniker, root_realm_dir).await,
        Err(fsys::OpenError::InstanceNotFound) | Err(fsys::OpenError::InstanceNotRunning) => {
            // appmgr doesn't exist or isn't running
            Ok(vec![])
        }
        Err(e) => {
            Err(format_err!("Component manager returned error opening appmgr out dir: {:?}", e))
        }
    }
}

fn parse_cmx_realm(
    moniker: AbsoluteMoniker,
    realm_dir: RemoteDirectory,
) -> BoxFuture<'static, Result<Vec<Instance>>> {
    async move {
        let children_dir = realm_dir.open_dir_readonly("c")?;
        let realms_dir = realm_dir.open_dir_readonly("r")?;

        let future_children = parse_cmx_components_in_c_dir(children_dir, moniker.clone());
        let future_realms = parse_cmx_realms_in_r_dir(realms_dir, moniker.clone());

        let (children, realms) = join(future_children, future_realms).await;
        let mut children = children?;
        let mut realms = realms?;

        children.append(&mut realms);

        Ok(children)
    }
    .boxed()
}

fn parse_cmx_component(
    moniker: AbsoluteMoniker,
    dir: RemoteDirectory,
) -> BoxFuture<'static, Result<Vec<Instance>>> {
    async move {
        // Runner CMX components may have child components.
        let url = dir.read_file("url").await?;

        let mut instances = if dir.exists("c").await? {
            let children_dir = dir.open_dir_readonly("c")?;
            parse_cmx_components_in_c_dir(children_dir, moniker.clone()).await?
        } else {
            vec![]
        };

        let execution_info = ExecutionInfo { start_reason: "Unknown start reason".to_string() };
        let resolved_info =
            ResolvedInfo { resolved_url: url.clone(), execution_info: Some(execution_info) };

        instances.push(Instance {
            moniker,
            url,
            environment: None,
            instance_id: None,
            resolved_info: Some(resolved_info),
            instance_type: InstanceType::Cmx(dir.clone()?),
        });

        Ok(instances)
    }
    .boxed()
}

async fn parse_cmx_components_in_c_dir(
    children_dir: RemoteDirectory,
    moniker: AbsoluteMoniker,
) -> Result<Vec<Instance>> {
    let child_component_names = children_dir.entry_names().await?;
    let mut future_children = vec![];
    for child_component_name in child_component_names {
        let child_moniker = ChildMoniker::parse(&child_component_name)?;
        let child_moniker = moniker.child(child_moniker);
        let job_ids_dir = children_dir.open_dir_readonly(&child_component_name)?;
        let child_dirs = open_all_job_ids(job_ids_dir).await?;
        for child_dir in child_dirs {
            let future_child = parse_cmx_component(child_moniker.clone(), child_dir);
            future_children.push(future_child);
        }
    }

    let instances: Vec<Result<Vec<Instance>>> = join_all(future_children).await;
    let instances: Result<Vec<Vec<Instance>>> = instances.into_iter().collect();
    let instances: Vec<Instance> = instances?.into_iter().flatten().collect();

    Ok(instances)
}

async fn parse_cmx_realms_in_r_dir(
    realms_dir: RemoteDirectory,
    moniker: AbsoluteMoniker,
) -> Result<Vec<Instance>> {
    let mut future_realms = vec![];
    for child_realm_name in realms_dir.entry_names().await? {
        let child_moniker = ChildMoniker::parse(&child_realm_name)?;
        let child_moniker = moniker.child(child_moniker);
        let job_ids_dir = realms_dir.open_dir_readonly(&child_realm_name)?;
        let child_realm_dirs = open_all_job_ids(job_ids_dir).await?;
        for child_realm_dir in child_realm_dirs {
            let future_realm = parse_cmx_realm(child_moniker.clone(), child_realm_dir);
            future_realms.push(future_realm);
        }
    }

    let instances: Vec<Result<Vec<Instance>>> = join_all(future_realms).await;
    let instances: Result<Vec<Vec<Instance>>> = instances.into_iter().collect();
    let instances: Vec<Instance> = instances?.into_iter().flatten().collect();

    Ok(instances)
}

async fn open_all_job_ids(job_ids_dir: RemoteDirectory) -> Result<Vec<RemoteDirectory>> {
    let dirs = job_ids_dir
        .entry_names()
        .await?
        .into_iter()
        .map(|job_id| job_ids_dir.open_dir_readonly(&job_id))
        .collect::<Result<Vec<RemoteDirectory>>>()?;
    Ok(dirs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::*;
    use std::collections::HashMap;
    use tempfile::TempDir;

    pub fn create_appmgr_out() -> TempDir {
        let temp_dir = TempDir::new_in("/tmp").unwrap();
        let root = temp_dir.path();
        std::fs::create_dir_all(root.join("hub/r")).unwrap();
        {
            let sshd = root.join("hub/c/sshd.cmx/9898");
            std::fs::create_dir_all(&sshd).unwrap();
            std::fs::create_dir_all(sshd.join("in/pkg")).unwrap();
            std::fs::create_dir_all(sshd.join("in/data")).unwrap();
            std::fs::create_dir_all(sshd.join("out/dev")).unwrap();
            std::fs::write(sshd.join("url"), "fuchsia-pkg://fuchsia.com/sshd#meta/sshd.cmx")
                .unwrap();
            std::fs::write(sshd.join("in/pkg/meta"), "1234").unwrap();
            std::fs::write(sshd.join("job-id"), "5454").unwrap();
            std::fs::write(sshd.join("process-id"), "9898").unwrap();
        }
        temp_dir
    }

    #[fuchsia::test]
    async fn test_cmx() {
        let appmgr_out_dir = create_appmgr_out();

        let query = serve_realm_query(
            vec![],
            HashMap::new(),
            HashMap::new(),
            HashMap::from([(
                ("./core/appmgr".to_string(), fsys::OpenDirType::OutgoingDir),
                appmgr_out_dir,
            )]),
        );

        let mut instances = get_all_instances(&query).await.unwrap();
        assert_eq!(instances.len(), 1);
        let instance = instances.remove(0);

        assert_eq!(instance.moniker, AbsoluteMoniker::parse_str("/core/appmgr/sshd.cmx").unwrap());
        assert_eq!(instance.url, "fuchsia-pkg://fuchsia.com/sshd#meta/sshd.cmx");

        let hub_dir = match instance.instance_type {
            InstanceType::Cmx(hub_dir) => hub_dir,
            i => panic!("unexpected instance type: {:?}", i),
        };

        assert!(instance.instance_id.is_none());
        let resolved = instance.resolved_info.unwrap();
        resolved.execution_info.unwrap();

        let namespace_capabilities = get_namespace_capabilities(&hub_dir).await.unwrap();
        assert_eq!(namespace_capabilities, vec!["data", "pkg"]);

        let outgoing_capabilities = get_outgoing_capabilities(&hub_dir).await.unwrap();
        assert_eq!(outgoing_capabilities, vec!["dev"]);

        let runtime = get_runtime(&hub_dir).await.unwrap();
        match runtime {
            Runtime::Elf {
                job_id,
                process_id,
                process_start_time,
                process_start_time_utc_estimate,
            } => {
                assert_eq!(job_id, 5454);
                assert_eq!(process_id, Some(9898));
                assert!(process_start_time.is_none());
                assert!(process_start_time_utc_estimate.is_none());
            }
            r => panic!("unexpected runtime: {:?}", r),
        }
    }
}
