// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::PkgServerInfo;
use async_lock::RwLock;
use fidl_fuchsia_developer_ffx as ffx;
use fidl_fuchsia_developer_ffx_ext::{
    RepositoryError, RepositoryRegistrationAliasConflictMode, RepositoryTarget,
};
use fidl_fuchsia_pkg::RepositoryManagerProxy;
use fidl_fuchsia_pkg_ext::{MirrorConfigBuilder, RepositoryConfigBuilder};
use fidl_fuchsia_pkg_rewrite::EngineProxy;
use fidl_fuchsia_pkg_rewrite_ext::{do_transaction, Rule};
use fuchsia_hyper::HttpsClient;
use fuchsia_repo::repo_client::RepoClient;
use fuchsia_repo::repository::{
    self, FileSystemRepository, HttpRepository, PmRepository, RepoProvider, RepositorySpec,
};
use fuchsia_url::RepositoryUrl;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::Arc;
use url::Url;
use zx_status::Status;

pub fn repo_spec_to_backend(
    repo_spec: &RepositorySpec,
    https_client: HttpsClient,
) -> Result<Box<dyn RepoProvider>, RepositoryError> {
    match repo_spec {
        RepositorySpec::FileSystem { metadata_repo_path, blob_repo_path, aliases } => Ok(Box::new(
            FileSystemRepository::builder(metadata_repo_path.into(), blob_repo_path.into())
                .aliases(aliases.clone())
                .build(),
        )),
        RepositorySpec::Pm { path, aliases } => {
            Ok(Box::new(PmRepository::builder(path.into()).aliases(aliases.clone()).build()))
        }
        RepositorySpec::Http { metadata_repo_url, blob_repo_url, aliases } => {
            let metadata_repo_url = Url::parse(metadata_repo_url.as_str()).map_err(|err| {
                tracing::error!(
                    "Unable to parse metadata repo url {}: {:#}",
                    metadata_repo_url,
                    err
                );
                ffx::RepositoryError::InvalidUrl
            })?;

            let blob_repo_url = Url::parse(blob_repo_url.as_str()).map_err(|err| {
                tracing::error!("Unable to parse blob repo url {}: {:#}", blob_repo_url, err);
                ffx::RepositoryError::InvalidUrl
            })?;

            Ok(Box::new(HttpRepository::new(
                https_client,
                metadata_repo_url,
                blob_repo_url,
                aliases.clone(),
            )))
        }
        RepositorySpec::Gcs { .. } => {
            // FIXME(https://fxbug.dev/42181388): Implement support for daemon-side GCS repositories.
            tracing::error!("Trying to register a GCS repository, but that's not supported yet");
            Err(RepositoryError::UnknownRepositorySpec)
        }
    }
}

async fn update_repository(
    repo_name: &str,
    repo: &RwLock<RepoClient<Box<dyn RepoProvider>>>,
) -> Result<bool, ffx::RepositoryError> {
    repo.write().await.update().await.map_err(|err| {
        tracing::error!("Unable to update repository {}: {:#?}", repo_name, err);

        match err {
            repository::Error::Tuf(tuf::Error::ExpiredMetadata { .. }) => {
                ffx::RepositoryError::ExpiredRepositoryMetadata
            }
            _ => ffx::RepositoryError::IoError,
        }
    })
}

pub async fn register_target_with_repo_instance(
    repo_proxy: RepositoryManagerProxy,
    rewrite_engine_proxy: EngineProxy,
    repo_target_info: &RepositoryTarget,
    target: &ffx::TargetInfo,
    repo_server_listen_addr: SocketAddr,
    repo_instance: &PkgServerInfo,
    alias_conflict_mode: RepositoryRegistrationAliasConflictMode,
) -> Result<(), ffx::RepositoryError> {
    let repo_name: &str = &repo_target_info.repo_name;
    let target_ssh_host_address = target.ssh_host_address.clone();

    tracing::info!(
        "Registering repository {:?} for target {:?}",
        repo_name,
        repo_target_info.target_identifier
    );

    // Before we register the repository, we need to decide which address the
    // target device should use to reach the repository. If the server is
    // running on a loopback device, then a tunnel will be created for the
    // device to access the server.
    let (_, repo_host_addr) = create_repo_host(
        repo_server_listen_addr,
        target_ssh_host_address.ok_or_else(|| {
            tracing::error!(
                "target {:?} does not have a host address",
                repo_target_info.target_identifier
            );
            ffx::RepositoryError::TargetCommunicationFailure
        })?,
    );

    let aliases = {
        // Use the repository aliases if the registration doesn't have any.
        let aliases: BTreeSet<String> = if let Some(aliases) = &repo_target_info.aliases {
            aliases.clone()
        } else {
            repo_instance.repo_spec().aliases()
        };

        // If the registration conflict mode is ErrorOut, read the aliases from the device
        // and determine if any of the aliases are pointing to a different repository.
        if alias_conflict_mode == RepositoryRegistrationAliasConflictMode::ErrorOut {
            let alias_repos = read_alias_repos(rewrite_engine_proxy.clone()).await?;
            for alias in &aliases {
                if let Some(repos) = alias_repos.get(alias) {
                    if !repos.contains(&repo_name.to_string()) {
                        tracing::error!(
                            "Alias registration conflict for {alias} replaced by {} ",
                            repos.join(", ")
                        );
                        return Err(ffx::RepositoryError::ConflictingRegistration);
                    }
                }
            }
        }
        aliases
    };

    let mirrors = repo_instance.repo_config.mirrors();
    let mut subscribe = false;
    //remove all the mirrors, there should only be one placeholder
    for m in mirrors {
        subscribe = subscribe || m.subscribe();
    }
    // now add repo_host_addr as the mirror
    let mirror_url = format!("http://{repo_host_addr}/{}", repo_instance.name);
    let mirror_url: http::Uri = mirror_url.parse().map_err(|err| {
        tracing::error!("failed to parse mirror url {}: {:#}", mirror_url, err);
        ffx::RepositoryError::InvalidUrl
    })?;

    let mut mirror = MirrorConfigBuilder::new(mirror_url.clone()).map_err(|err| {
        tracing::error!("failed to parse mirror url {}: {:#}", mirror_url, err);
        ffx::RepositoryError::InvalidUrl
    })?;
    mirror = mirror.subscribe(subscribe);

    let mut config = RepositoryConfigBuilder::new(
        RepositoryUrl::parse_host(repo_name.to_string()).map_err(|err| {
            tracing::error!("failed to parse repo url {}: {:#}", repo_name, err);
            ffx::RepositoryError::InvalidUrl
        })?,
    )
    .add_mirror(mirror)
    .root_version(repo_instance.repo_config.root_version())
    .root_threshold(repo_instance.repo_config.root_threshold())
    .use_local_mirror(repo_instance.repo_config.use_local_mirror())
    .repo_storage_type(repo_instance.repo_config.repo_storage_type().clone());

    for key in repo_instance.repo_config.root_keys() {
        config = config.add_root_key(key.clone());
    }

    match repo_proxy.add(&config.build().into()).await {
        Ok(Ok(())) => {}
        Ok(Err(err)) => {
            tracing::error!("failed to add config: {:#?}", Status::from_raw(err));
            return Err(ffx::RepositoryError::RepositoryManagerError);
        }
        Err(err) => {
            tracing::error!("failed to add config: {:#?}", err);
            return Err(ffx::RepositoryError::TargetCommunicationFailure);
        }
    }
    if !aliases.is_empty() {
        create_aliases_fidl(rewrite_engine_proxy, repo_name, &aliases).await
    } else {
        Ok(())
    }
}

/// Reads the alias mappings from the device via the EngineProxy.
async fn read_alias_repos(
    engine_proxy: EngineProxy,
) -> Result<HashMap<String, Vec<String>>, ffx::RepositoryError> {
    let (rule_iterator, rule_iterator_server) = fidl::endpoints::create_proxy();

    engine_proxy.list(rule_iterator_server).map_err(|e| {
        tracing::error!("Failed to list rules: {e}");
        ffx::RepositoryError::RewriteEngineError
    })?;

    let mut alias_to_repo: HashMap<String, Vec<String>> = HashMap::new();

    let mut rules: Vec<Rule> = vec![];
    loop {
        let rule = rule_iterator.next().await.map_err(|e| {
            tracing::error!("Failed iterating while listing rules: {e}");
            ffx::RepositoryError::RewriteEngineError
        })?;
        if rule.is_empty() {
            break;
        }
        rules.extend(rule.into_iter().map(|r| r.try_into().unwrap()));
    }

    for r in rules {
        let alias = r.host_match();
        let repo = r.host_replacement();
        if let Some(repo_list) = alias_to_repo.get_mut(alias) {
            repo_list.push(repo.to_string());
        } else {
            alias_to_repo.insert(alias.into(), vec![repo.into()]);
        }
    }

    Ok(alias_to_repo)
}

pub async fn register_target_with_fidl_proxies(
    repo_proxy: RepositoryManagerProxy,
    rewrite_engine_proxy: EngineProxy,
    repo_target_info: &RepositoryTarget,
    target: &ffx::TargetInfo,
    repo_server_listen_addr: SocketAddr,
    repo: &Arc<RwLock<RepoClient<Box<dyn RepoProvider>>>>,
    _alias_conflict_mode: RepositoryRegistrationAliasConflictMode,
) -> Result<(), ffx::RepositoryError> {
    let repo_name: &str = &repo_target_info.repo_name;
    let target_ssh_host_address = target.ssh_host_address.clone();

    tracing::info!(
        "Registering repository {:?} for target {:?}",
        repo_name,
        repo_target_info.target_identifier
    );

    // Before we register the repository, we need to decide which address the
    // target device should use to reach the repository. If the server is
    // running on a loopback device, then a tunnel will be created for the
    // device to access the server.
    let (_, repo_host) = create_repo_host(
        repo_server_listen_addr,
        target_ssh_host_address.ok_or_else(|| {
            tracing::error!(
                "target {:?} does not have a host address",
                repo_target_info.target_identifier
            );
            ffx::RepositoryError::TargetCommunicationFailure
        })?,
    );

    // Make sure the repository is up to date.
    update_repository(repo_name, &repo).await?;

    let repo_url = RepositoryUrl::parse_host(repo_name.to_owned()).map_err(|err| {
        tracing::error!("failed to parse repository name {}: {:#}", repo_name, err);
        ffx::RepositoryError::InvalidUrl
    })?;

    let mirror_url = format!("http://{}/{}", repo_host, repo_name);
    let mirror_url = mirror_url.parse().map_err(|err| {
        tracing::error!("failed to parse mirror url {}: {:#}", mirror_url, err);
        ffx::RepositoryError::InvalidUrl
    })?;

    let (config, aliases) = {
        let repo = repo.read().await;

        let config = repo
            .get_config(
                repo_url,
                mirror_url,
                repo_target_info
                    .storage_type
                    .as_ref()
                    .map(|storage_type| storage_type.clone().into()),
            )
            .map_err(|e| {
                tracing::error!("failed to get config: {}", e);
                return ffx::RepositoryError::RepositoryManagerError;
            })?;

        // Use the repository aliases if the registration doesn't have any.
        let aliases = if let Some(aliases) = &repo_target_info.aliases {
            aliases.clone()
        } else {
            repo.aliases().clone()
        };

        (config, aliases)
    };

    match repo_proxy.add(&config.into()).await {
        Ok(Ok(())) => {}
        Ok(Err(err)) => {
            tracing::error!("failed to add config: {:#?}", Status::from_raw(err));
            return Err(ffx::RepositoryError::RepositoryManagerError);
        }
        Err(err) => {
            tracing::error!("failed to add config: {:#?}", err);
            return Err(ffx::RepositoryError::TargetCommunicationFailure);
        }
    }

    if !aliases.is_empty() {
        let () = create_aliases_fidl(rewrite_engine_proxy, repo_name, &aliases).await?;
    }

    Ok(())
}

fn aliases_to_rules(
    repo_name: &str,
    aliases: &BTreeSet<String>,
) -> Result<Vec<Rule>, ffx::RepositoryError> {
    let rules = aliases
        .iter()
        .map(|alias| {
            let mut split_alias = alias.split("/").collect::<Vec<&str>>();
            let host_match = split_alias.remove(0);
            let path_prefix = split_alias.join("/");
            Rule::new(
                host_match.to_string(),
                repo_name.to_string(),
                format!("/{path_prefix}"),
                format!("/{path_prefix}"),
            )
        })
        .collect::<Result<Vec<_>, _>>()
        .map_err(|err| {
            tracing::warn!("failed to construct rule: {:#?}", err);
            ffx::RepositoryError::RewriteEngineError
        })?;

    Ok(rules)
}

async fn create_aliases_fidl(
    rewrite_proxy: EngineProxy,
    repo_name: &str,
    aliases: &BTreeSet<String>,
) -> Result<(), ffx::RepositoryError> {
    let alias_rules = aliases_to_rules(repo_name, &aliases)?;

    // Check flag here for "overwrite" style
    do_transaction(&rewrite_proxy, |transaction| async {
        // Prepend the alias rules to the front so they take priority.
        let mut rules = alias_rules.iter().cloned().rev().collect::<Vec<_>>();

        // These are rules to re-evaluate...
        let repo_rules_state = transaction.list_dynamic().await?;
        rules.extend(repo_rules_state);

        // Clear the list, since we'll be adding it back later.
        transaction.reset_all()?;

        // Remove duplicated rules while preserving order.
        let mut unique_rules = HashSet::new();
        rules.retain(|r| unique_rules.insert(r.clone()));

        // Add the rules back into the transaction. We do it in reverse, because `.add()`
        // always inserts rules into the front of the list.
        for rule in rules.into_iter().rev() {
            transaction.add(rule).await?
        }

        Ok(transaction)
    })
    .await
    .map_err(|err| {
        tracing::warn!("failed to create transactions: {:#?}", err);
        ffx::RepositoryError::RewriteEngineError
    })?;

    Ok(())
}

/// Decide which repo host we should use when creating a repository config, and
/// whether or not we need to create a tunnel in order for the device to talk to
/// the repository.
pub fn create_repo_host(
    listen_addr: SocketAddr,
    host_address: ffx::SshHostAddrInfo,
) -> (bool, String) {
    // We need to decide which address the target device should use to reach the
    // repository. If the server is running on a loopback device, then we need
    // to create a tunnel for the device to access the server.
    if listen_addr.ip().is_loopback() {
        return (true, listen_addr.to_string());
    }

    // However, if it's not a loopback address, then configure the device to
    // communicate by way of the ssh host's address. This is helpful when the
    // device can access the repository only through a specific interface.

    // FIXME(https://fxbug.dev/42168560): Once the tunnel bug is fixed, we may
    // want to default all traffic going through the tunnel. Consider
    // creating an ffx config variable to decide if we want to always
    // tunnel, or only tunnel if the server is on a loopback address.

    // IPv6 addresses can contain a ':', IPv4 cannot.
    let repo_host = if host_address.address.contains(':') {
        if let Some(pos) = host_address.address.rfind('%') {
            let ip = &host_address.address[..pos];
            let scope_id = &host_address.address[pos + 1..];
            format!("[{}%25{}]:{}", ip, scope_id, listen_addr.port())
        } else {
            format!("[{}]:{}", host_address.address, listen_addr.port())
        }
    } else {
        format!("{}:{}", host_address.address, listen_addr.port())
    };

    (false, repo_host)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fuchsia_hyper::new_https_client;
    use std::fs;

    const EMPTY_REPO_PATH: &str =
        concat!(env!("ROOT_OUT_DIR"), "/test_data/ffx_lib_pkg/empty-repo");

    fn pm_repo_spec() -> RepositorySpec {
        let path = fs::canonicalize(EMPTY_REPO_PATH).unwrap();
        RepositorySpec::Pm {
            path: path.try_into().unwrap(),
            aliases: BTreeSet::from(["anothercorp.com".into(), "mycorp.com".into()]),
        }
    }

    fn filesystem_repo_spec() -> RepositorySpec {
        let repo = fs::canonicalize(EMPTY_REPO_PATH).unwrap();
        let metadata_repo_path = repo.join("repository");
        let blob_repo_path = metadata_repo_path.join("blobs");
        RepositorySpec::FileSystem {
            metadata_repo_path: metadata_repo_path.try_into().unwrap(),
            blob_repo_path: blob_repo_path.try_into().unwrap(),
            aliases: BTreeSet::new(),
        }
    }

    #[fuchsia::test]
    async fn test_pm_repo_spec_to_backend() {
        let spec = pm_repo_spec();
        let backend = repo_spec_to_backend(&spec, new_https_client()).unwrap();
        assert_eq!(spec, backend.spec());
    }

    #[fuchsia::test]
    async fn test_filesystem_repo_spec_to_backend() {
        let spec = filesystem_repo_spec();
        let backend = repo_spec_to_backend(&spec, new_https_client()).unwrap();
        assert_eq!(spec, backend.spec());
    }
}
