// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    async_lock::RwLock,
    async_trait::async_trait,
    ffx_daemon_core::events::{EventHandler, Status as EventStatus},
    ffx_daemon_events::{DaemonEvent, TargetEvent, TargetInfo},
    ffx_daemon_target::target::Target,
    ffx_ssh::ssh::build_ssh_command,
    fidl::endpoints::ServerEnd,
    fidl_fuchsia_developer_ffx as ffx,
    fidl_fuchsia_developer_ffx_ext::{
        RepositoryRegistrationAliasConflictMode, RepositorySpec, RepositoryTarget, ServerStatus,
    },
    fidl_fuchsia_net_ext::SocketAddress,
    fidl_fuchsia_pkg::RepositoryManagerMarker,
    fidl_fuchsia_pkg_rewrite::EngineMarker as RewriteEngineMarker,
    fidl_fuchsia_pkg_rewrite_ext::{do_transaction, Rule, RuleConfig},
    fuchsia_async as fasync,
    fuchsia_repo::{
        repo_client::RepoClient,
        repository::{self, FileSystemRepository, HttpRepository, PmRepository, RepoProvider},
    },
    fuchsia_url::RepositoryUrl,
    fuchsia_zircon_status::Status,
    fuchsia_zircon_types::ZX_CHANNEL_MAX_MSG_BYTES,
    futures::{FutureExt as _, StreamExt as _},
    measure_fuchsia_developer_ffx::Measurable,
    pkg::config as pkg_config,
    pkg::metrics,
    pkg::repo::{Registrar, RepoInner, SaveConfig, ServerState},
    protocols::prelude::*,
    shared_child::SharedChild,
    std::{
        collections::{BTreeSet, HashSet},
        convert::{TryFrom, TryInto},
        net::SocketAddr,
        rc::Rc,
        sync::Arc,
        time::Duration,
    },
    url::Url,
};

const PKG_RESOLVER_MONIKER: &str = "/core/pkg-resolver";

const TARGET_CONNECT_TIMEOUT: Duration = Duration::from_secs(60);

// Registrar shift.
// Event handler not needed for shifting.
#[ffx_protocol]
pub struct Repo<T: EventHandlerProvider<R> = RealEventHandlerProvider, R: Registrar = RealRegistrar>
{
    inner: Arc<RwLock<RepoInner>>,
    event_handler_provider: T,
    registrar: Arc<R>,
}

#[async_trait::async_trait(?Send)]
pub trait EventHandlerProvider<R: Registrar> {
    async fn setup_event_handlers(
        &mut self,
        cx: Context,
        inner: Arc<RwLock<RepoInner>>,
        registrar: Arc<R>,
    );
}

#[derive(Default)]
pub struct RealEventHandlerProvider;

#[async_trait::async_trait(?Send)]
impl<R: Registrar + 'static> EventHandlerProvider<R> for RealEventHandlerProvider {
    async fn setup_event_handlers(
        &mut self,
        cx: Context,
        inner: Arc<RwLock<RepoInner>>,
        registrar: Arc<R>,
    ) {
        let q = cx.daemon_event_queue().await;
        q.add_handler(DaemonEventHandler { cx, inner, registrar }).await;
    }
}

#[async_trait::async_trait(?Send)]
pub trait SshProvider {
    async fn run_ssh_command(
        &self,
        device_addr: SocketAddr,
        args: Vec<&str>,
    ) -> Result<(), ffx::RepositoryError>;
}

#[derive(Default)]
pub struct RealSshProvider;

#[async_trait::async_trait(?Send)]
impl SshProvider for RealSshProvider {
    async fn run_ssh_command(
        &self,
        device_addr: SocketAddr,
        args: Vec<&str>,
    ) -> Result<(), ffx::RepositoryError> {
        let mut ssh_command = match build_ssh_command(device_addr, args).await {
            Ok(ssh) => ssh,
            Err(e) => {
                tracing::error!("failed to build ssh command: {:?}", e);
                return Err(ffx::RepositoryError::InternalError);
            }
        };

        tracing::debug!("Spawning command '{:?}'", ssh_command);
        SharedChild::spawn(&mut ssh_command).map_err(|err| {
            tracing::error!("failed to register ssh endpoint: {:?}", err);
            ffx::RepositoryError::TargetCommunicationFailure
        })?;

        Ok(())
    }
}

#[derive(Default)]
pub struct RealRegistrar<S: SshProvider = RealSshProvider> {
    ssh_provider: Arc<S>,
}

#[async_trait::async_trait(?Send)]
impl<S: SshProvider> Registrar for RealRegistrar<S> {
    async fn register_target(
        &self,
        cx: &Context,
        target_info: RepositoryTarget,
        save_config: SaveConfig,
        inner: Arc<RwLock<RepoInner>>,
        alias_conflict_mode: RepositoryRegistrationAliasConflictMode,
    ) -> Result<(), ffx::RepositoryError> {
        let repository_mode = pkg::config::repository_registration_mode().await.map_err(|err| {
            tracing::error!("Failed to get repository registration mode: {:#?}", err);
            ffx::RepositoryError::InternalError
        })?;

        match repository_mode.as_str() {
            "fidl" => {
                self.register_target_with_fidl(
                    cx,
                    target_info,
                    save_config,
                    inner,
                    alias_conflict_mode,
                )
                .await
            }
            "ssh" => {
                self.register_target_with_ssh(
                    cx,
                    target_info,
                    save_config,
                    inner,
                    alias_conflict_mode,
                )
                .await
            }
            _ => {
                tracing::error!("Unrecognized repository registration mode {:?}", repository_mode);
                return Err(ffx::RepositoryError::InternalError);
            }
        }
    }

    async fn register_target_with_fidl(
        &self,
        cx: &Context,
        mut target_info: RepositoryTarget,
        save_config: SaveConfig,
        inner: Arc<RwLock<RepoInner>>,
        alias_conflict_mode: RepositoryRegistrationAliasConflictMode,
    ) -> Result<(), ffx::RepositoryError> {
        let repo_name = &target_info.repo_name;

        tracing::info!(
            "Registering repository {:?} for target {:?}",
            repo_name,
            target_info.target_identifier
        );

        let repo = inner
            .read()
            .await
            .manager
            .get(repo_name)
            .ok_or_else(|| ffx::RepositoryError::NoMatchingRepository)?;

        let (target, proxy) = futures::select! {
            res = cx.open_target_proxy_with_info::<RepositoryManagerMarker>(
                target_info.target_identifier.clone(),
                PKG_RESOLVER_MONIKER,
            ).fuse() => {
                res.map_err(|err| {
                    tracing::error!(
                        "failed to open target proxy with target name {:?}: {:#?}",
                        target_info.target_identifier,
                        err
                    );
                    ffx::RepositoryError::TargetCommunicationFailure
                })?
            }
            _ = fasync::Timer::new(TARGET_CONNECT_TIMEOUT).fuse() => {
                tracing::error!("Timed out connecting to target name {:?}", target_info.target_identifier);
                return Err(ffx::RepositoryError::TargetCommunicationFailure);
            }
        };

        let target_nodename = target.nodename.ok_or_else(|| {
            tracing::error!("target {:?} does not have a nodename", target_info.target_identifier);
            ffx::RepositoryError::TargetCommunicationFailure
        })?;

        let listen_addr = match inner.read().await.server.listen_addr() {
            Some(listen_addr) => listen_addr,
            None => {
                tracing::error!("repository server is not running");
                return Err(ffx::RepositoryError::ServerNotRunning);
            }
        };

        // Before we register the repository, we need to decide which address the
        // target device should use to reach the repository. If the server is
        // running on a loopback device, then we need to create a tunnel for the
        // device to access the server.
        let (should_make_tunnel, repo_host) = create_repo_host(
            listen_addr,
            target.ssh_host_address.ok_or_else(|| {
                tracing::error!(
                    "target {:?} does not have a host address",
                    target_info.target_identifier
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
                    target_info
                        .storage_type
                        .as_ref()
                        .map(|storage_type| storage_type.clone().into()),
                )
                .map_err(|e| {
                    tracing::error!("failed to get config: {}", e);
                    return ffx::RepositoryError::RepositoryManagerError;
                })?;

            // Use the repository aliases if the registration doesn't have any.
            let aliases = if let Some(aliases) = &target_info.aliases {
                aliases.clone()
            } else {
                repo.aliases().clone()
            };

            // Checking for registration alias conflicts.
            let check_alias_conflict = pkg::config::check_registration_alias_conflict(
                repo_name.as_str(),
                target_nodename.as_str(),
                aliases.clone().into_iter().collect(),
            )
            .await
            .map_err(|e| {
                tracing::error!("{e}");
                ffx::RepositoryError::ConflictingRegistration
            });
            if alias_conflict_mode == RepositoryRegistrationAliasConflictMode::ErrorOut {
                check_alias_conflict?
            }

            (config, aliases)
        };

        match proxy.add(&config.into()).await {
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
            let () = create_aliases_fidl(cx, repo_name, &target_nodename, &aliases).await?;
        }

        if should_make_tunnel {
            // Start the tunnel to the device if one isn't running already.
            start_tunnel(&cx, &inner, &target_nodename).await.map_err(|err| {
                tracing::error!(
                    "Failed to start tunnel to target {:?}: {:#}",
                    target_nodename,
                    err
                );
                ffx::RepositoryError::TargetCommunicationFailure
            })?;
        }

        if save_config == SaveConfig::Save {
            // Make sure we update the target info with the real nodename.
            target_info.target_identifier = Some(target_nodename.clone());

            pkg::config::set_registration(&target_nodename, &target_info).await.map_err(|err| {
                tracing::error!("Failed to save registration to config: {:#?}", err);
                ffx::RepositoryError::InternalError
            })?;
        }

        Ok(())
    }

    async fn register_target_with_ssh(
        &self,
        cx: &Context,
        mut target_info: RepositoryTarget,
        save_config: SaveConfig,
        inner: Arc<RwLock<RepoInner>>,
        alias_conflict_mode: RepositoryRegistrationAliasConflictMode,
    ) -> Result<(), ffx::RepositoryError> {
        if alias_conflict_mode == RepositoryRegistrationAliasConflictMode::ErrorOut {
            tracing::info!(
                "RepositoryRegistrationAliasConflictMode::ErrorOut is not available for SSH registrations.",
            );
        }

        let repo_name = &target_info.repo_name;

        let repo = inner
            .read()
            .await
            .manager
            .get(repo_name)
            .ok_or_else(|| ffx::RepositoryError::NoMatchingRepository)?;

        // Make sure the repository is up to date.
        update_repository(repo_name, &repo).await?;

        let target = match cx.get_target_collection().await {
            Ok(target_collection) => {
                match target_collection.get(target_info.target_identifier.clone()) {
                    Some(target) => target,
                    None => {
                        tracing::error!("failed to get target from target collection");
                        return Err(ffx::RepositoryError::TargetCommunicationFailure);
                    }
                }
            }
            Err(e) => {
                tracing::error!("failed to get target collection: {}", e);
                return Err(ffx::RepositoryError::TargetCommunicationFailure);
            }
        };

        let target_nodename = target.nodename().ok_or_else(|| {
            tracing::error!("target {:?} does not have a nodename", target_info.target_identifier);
            ffx::RepositoryError::TargetCommunicationFailure
        })?;
        let host_address = match target.ssh_host_address_info() {
            Some(host_address) => host_address,
            None => {
                tracing::error!("failed to get host address");
                return Err(ffx::RepositoryError::TargetCommunicationFailure);
            }
        };
        let listen_addr = match inner.read().await.server.listen_addr() {
            Some(listen_addr) => listen_addr,
            None => {
                tracing::error!("repository server is not running");
                return Err(ffx::RepositoryError::ServerNotRunning);
            }
        };

        // ssh workflow does not touch tunneling logic
        let (_should_make_tunnel, repo_host) = create_repo_host(listen_addr, host_address);
        let repo_config_endpoint = format!("http://{}/{}/repo.config", repo_host, repo_name);

        let device_addr = match target.ssh_address() {
            Some(ssh_address) => ssh_address,
            None => {
                tracing::error!("failed to get ssh address of target");
                return Err(ffx::RepositoryError::TargetCommunicationFailure);
            }
        };

        // Adding repo via pkgctl
        self.ssh_provider
            .run_ssh_command(
                device_addr,
                vec!["pkgctl", "repo", "add", "url", &repo_config_endpoint],
            )
            .await?;

        let aliases = {
            let repo = repo.read().await;

            // Use the repository aliases if the registration doesn't have any.
            let aliases = if let Some(aliases) = &target_info.aliases {
                aliases.clone()
            } else {
                repo.aliases().clone()
            };

            aliases
        };

        if !aliases.is_empty() {
            let alias_rules = aliases_to_rules(repo_name, &aliases)?;
            let rules_config_json_string =
                rules_config_to_json_string(RuleConfig::Version1(alias_rules))?;

            self.ssh_provider
                .run_ssh_command(
                    device_addr,
                    vec!["pkgctl", "rule", "replace", "json", &rules_config_json_string],
                )
                .await?;
        }

        if save_config == SaveConfig::Save {
            // Make sure we update the target info with the real nodename.
            target_info.target_identifier = Some(target_nodename.clone());

            pkg::config::set_registration(&target_nodename, &target_info).await.map_err(|err| {
                tracing::error!("Failed to save registration to config: {:#?}", err);
                ffx::RepositoryError::InternalError
            })?;
        }

        Ok(())
    }
}

async fn start_tunnel(
    cx: &Context,
    inner: &Arc<RwLock<RepoInner>>,
    target_nodename: &str,
) -> anyhow::Result<()> {
    inner.read().await.server.start_tunnel(&cx, &target_nodename).await
}

async fn repo_spec_to_backend(
    repo_spec: &RepositorySpec,
    inner: &Arc<RwLock<RepoInner>>,
) -> Result<Box<dyn RepoProvider>, ffx::RepositoryError> {
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

            let https_client = inner.read().await.https_client.clone();

            Ok(Box::new(HttpRepository::new(
                https_client,
                metadata_repo_url,
                blob_repo_url,
                aliases.clone(),
            )))
        }
        RepositorySpec::Gcs { .. } => {
            // FIXME(fxbug.dev/98994): Implement support for daemon-side GCS repositories.
            tracing::error!("Trying to register a GCS repository, but that's not supported yet");
            Err(ffx::RepositoryError::UnknownRepositorySpec)
        }
    }
}

async fn add_repository(
    repo_name: &str,
    repo_spec: &RepositorySpec,
    save_config: SaveConfig,
    inner: Arc<RwLock<RepoInner>>,
) -> Result<(), ffx::RepositoryError> {
    tracing::info!("Adding repository {} {:?}", repo_name, repo_spec);

    // Create the repository.
    let backend = repo_spec_to_backend(&repo_spec, &inner).await?;
    let repo = RepoClient::from_trusted_remote(backend).await.map_err(|err| {
        tracing::error!("Unable to create repository: {:#?}", err);

        match err {
            repository::Error::Tuf(tuf::Error::ExpiredMetadata(_)) => {
                ffx::RepositoryError::ExpiredRepositoryMetadata
            }
            _ => ffx::RepositoryError::IoError,
        }
    })?;

    if save_config == SaveConfig::Save {
        // Save the filesystem configuration.
        pkg::config::set_repository(repo_name, &repo_spec).await.map_err(|err| {
            tracing::error!("Failed to save repository: {:#?}", err);
            ffx::RepositoryError::IoError
        })?;
    }

    // Finally add the repository.
    let inner = inner.write().await;
    inner.manager.add(repo_name, repo);

    metrics::add_repository_event(&repo_spec).await;

    Ok(())
}

/// Decide which repo host we should use when creating a repository config, and
/// whether or not we need to create a tunnel in order for the device to talk to
/// the repository.
fn create_repo_host(listen_addr: SocketAddr, host_address: ffx::SshHostAddrInfo) -> (bool, String) {
    // We need to decide which address the target device should use to reach the
    // repository. If the server is running on a loopback device, then we need
    // to create a tunnel for the device to access the server.
    if listen_addr.ip().is_loopback() {
        return (true, listen_addr.to_string());
    }

    // However, if it's not a loopback address, then configure the device to
    // communicate by way of the ssh host's address. This is helpful when the
    // device can access the repository only through a specific interface.

    // FIXME(fxbug.dev/87439): Once the tunnel bug is fixed, we may
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

fn aliases_to_rules(
    repo_name: &str,
    aliases: &BTreeSet<String>,
) -> Result<Vec<Rule>, ffx::RepositoryError> {
    let rules = aliases
        .iter()
        .map(|alias| {
            Rule::new(alias.to_string(), repo_name.to_string(), "/".to_string(), "/".to_string())
        })
        .collect::<Result<Vec<_>, _>>()
        .map_err(|err| {
            tracing::warn!("failed to construct rule: {:#?}", err);
            ffx::RepositoryError::RewriteEngineError
        })?;

    Ok(rules)
}

fn rules_config_to_json_string(rule_config: RuleConfig) -> Result<String, ffx::RepositoryError> {
    let rule_config_string = serde_json::to_string(&rule_config).map_err(|err| {
        tracing::error!("Failed to convert RulesConfig to json String: {:#?}", err);
        ffx::RepositoryError::InternalError
    })?;

    // Must wrap json string as '{}'.
    Ok(format!("'{}'", rule_config_string))
}

async fn create_aliases_fidl(
    cx: &Context,
    repo_name: &str,
    target_nodename: &str,
    aliases: &BTreeSet<String>,
) -> Result<(), ffx::RepositoryError> {
    let alias_rules = aliases_to_rules(repo_name, &aliases)?;

    let rewrite_proxy = match cx
        .open_target_proxy::<RewriteEngineMarker>(
            Some(target_nodename.to_string()),
            PKG_RESOLVER_MONIKER,
        )
        .await
    {
        Ok(p) => p,
        Err(err) => {
            tracing::warn!(
                "Failed to open Rewrite Engine target proxy with target name {:?}: {:#?}",
                target_nodename,
                err
            );
            return Err(ffx::RepositoryError::TargetCommunicationFailure);
        }
    };

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

impl<T: EventHandlerProvider<R>, R: Registrar> Repo<T, R> {
    async fn remove_repository(&self, cx: &Context, repo_name: &str) -> bool {
        tracing::info!("Removing repository {:?}", repo_name);

        // First, remove any registrations for this repository.
        for (target_nodename, _) in pkg::config::get_repository_registrations(repo_name).await {
            match self
                .deregister_target(cx, repo_name.to_string(), Some(target_nodename.to_string()))
                .await
            {
                Ok(()) => {}
                Err(err) => {
                    tracing::warn!(
                        "failed to deregister repository {:?} from target {:?}: {:#?}",
                        repo_name,
                        target_nodename,
                        err
                    );
                }
            }
        }

        // If we are removing the default repository, make sure to remove it from the configuration
        // as well.
        match pkg::config::get_default_repository().await {
            Ok(Some(default_repo_name)) if repo_name == default_repo_name => {
                if let Err(err) = pkg::config::unset_default_repository().await {
                    tracing::warn!("failed to remove default repository: {:#?}", err);
                }
            }
            Ok(_) => {}
            Err(err) => {
                tracing::warn!("failed to determine default repository name: {:#?}", err);
            }
        }

        if let Err(err) = pkg::config::remove_repository(repo_name).await {
            tracing::warn!("failed to remove repository from config: {:#?}", err);
        }

        // Finally, stop serving the repository.
        let mut inner = self.inner.write().await;
        let ret = inner.manager.remove(repo_name);

        if inner.manager.repositories().next().is_none() {
            if let Err(err) = inner.stop_server().await {
                tracing::error!("failed to stop server: {:#?}", err);
            }
        }

        ret
    }

    /// Deregister the repository from the target.
    ///
    /// This only works for repositories managed by `ffx`. If the repository named `repo_name` is
    /// unknown to this protocol, error out rather than trying to remove the registration.
    async fn deregister_target(
        &self,
        cx: &Context,
        repo_name: String,
        target_identifier: Option<String>,
    ) -> Result<(), ffx::RepositoryError> {
        tracing::info!(
            "Deregistering repository {:?} from target {:?}",
            repo_name,
            target_identifier
        );

        let target = cx.get_target_info(target_identifier.clone()).await.map_err(|err| {
            tracing::warn!(
                "Failed to look up target info with target name {:?}: {:#?}",
                target_identifier,
                err
            );
            ffx::RepositoryError::TargetCommunicationFailure
        })?;

        let target_nodename = target.nodename.ok_or_else(|| {
            tracing::warn!("Target {:?} does not have a nodename", target_identifier);
            ffx::RepositoryError::InternalError
        })?;

        // Look up the the registration info. Error out if we don't have any registrations for this
        // repository on this device.
        let _registration_info = pkg::config::get_registration(&repo_name, &target_nodename)
            .await
            .map_err(|err| {
                tracing::warn!(
                    "Failed to find registration info for repo {:?} and target {:?}: {:#?}",
                    repo_name,
                    target_nodename,
                    err
                );
                ffx::RepositoryError::InternalError
            })?
            .ok_or_else(|| ffx::RepositoryError::NoMatchingRegistration)?;

        // Finally, remove the registration config from the ffx config.
        pkg::config::remove_registration(&repo_name, &target_nodename).await.map_err(|err| {
            tracing::warn!("Failed to remove registration from config: {:#?}", err);
            ffx::RepositoryError::InternalError
        })?;

        Ok(())
    }

    async fn list_packages(
        &self,
        repository_name: &str,
        iterator: ServerEnd<ffx::RepositoryPackagesIteratorMarker>,
        include_fields: ffx::ListFields,
    ) -> Result<(), ffx::RepositoryError> {
        let mut stream = match iterator.into_stream() {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("error converting iterator to stream: {}", e);
                return Err(ffx::RepositoryError::InternalError);
            }
        };

        let repo = if let Some(r) = self.inner.read().await.manager.get(&repository_name) {
            r
        } else {
            return Err(ffx::RepositoryError::NoMatchingRepository);
        };

        // Make sure the repository is up to date.
        update_repository(repository_name, &repo).await?;

        let mut values = repo.read().await.list_packages(include_fields).await.map_err(|err| {
            tracing::error!("Unable to list packages: {:#?}", err);

            match err {
                repository::Error::Tuf(tuf::Error::ExpiredMetadata(_)) => {
                    ffx::RepositoryError::ExpiredRepositoryMetadata
                }
                _ => ffx::RepositoryError::IoError,
            }
        })?;

        fasync::Task::spawn(async move {
            let mut chunks = SliceChunker::new(&mut values);

            while let Some(request) = stream.next().await {
                match request {
                    Ok(ffx::RepositoryPackagesIteratorRequest::Next { responder }) => {
                        let chunk = chunks.next();

                        if let Err(e) = responder.send(chunk) {
                            tracing::warn!(
                                "Error responding to RepositoryPackagesIterator request: {:?}",
                                e
                            );
                            break;
                        }

                        if chunk.is_empty() {
                            break;
                        }
                    }
                    Err(e) => {
                        tracing::warn!(
                            "Error in RepositoryPackagesIterator request stream: {:?}",
                            e
                        );
                        break;
                    }
                }
            }
        })
        .detach();

        Ok(())
    }

    async fn show_package(
        &self,
        repository_name: &str,
        package_name: &str,
        iterator: ServerEnd<ffx::PackageEntryIteratorMarker>,
    ) -> Result<(), ffx::RepositoryError> {
        let mut stream = match iterator.into_stream() {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("error converting iterator to stream: {}", e);
                return Err(ffx::RepositoryError::InternalError);
            }
        };

        let repo = if let Some(r) = self.inner.read().await.manager.get(&repository_name) {
            r
        } else {
            return Err(ffx::RepositoryError::NoMatchingRepository);
        };

        // Make sure the repository is up to date.
        update_repository(repository_name, &repo).await?;

        let values =
            repo.read().await.show_package(package_name.to_owned()).await.map_err(|err| {
                tracing::error!("Unable to list package contents {:?}: {}", package_name, err);
                ffx::RepositoryError::IoError
            })?;
        let Some(mut values) = values else {
            return Err(ffx::RepositoryError::NoMatchingPackage);
        };

        fasync::Task::spawn(async move {
            let mut chunks = SliceChunker::new(&mut values);

            while let Some(request) = stream.next().await {
                match request {
                    Ok(ffx::PackageEntryIteratorRequest::Next { responder }) => {
                        let chunk = chunks.next();

                        if let Err(e) = responder.send(chunk) {
                            tracing::warn!(
                                "Error responding to PackageEntryIteratorRequest request: {:?}",
                                e
                            );
                            break;
                        }

                        if chunk.is_empty() {
                            break;
                        }
                    }
                    Err(e) => {
                        tracing::warn!(
                            "Error in PackageEntryIteratorRequest request stream: {:?}",
                            e
                        );
                        break;
                    }
                }
            }
        })
        .detach();

        Ok(())
    }
}

impl<T: EventHandlerProvider<R> + Default, R: Registrar + Default> Default for Repo<T, R> {
    fn default() -> Self {
        Repo {
            inner: RepoInner::new(),
            event_handler_provider: T::default(),
            registrar: Arc::new(R::default()),
        }
    }
}

#[async_trait(?Send)]
impl<
        T: EventHandlerProvider<R> + Default + Unpin + 'static,
        R: Registrar + Default + Unpin + 'static,
    > FidlProtocol for Repo<T, R>
{
    type Protocol = ffx::RepositoryRegistryMarker;
    type StreamHandler = FidlStreamHandler<Self>;

    async fn handle(
        &self,
        cx: &Context,
        req: ffx::RepositoryRegistryRequest,
    ) -> Result<(), anyhow::Error> {
        match req {
            ffx::RepositoryRegistryRequest::ServerStart { address, responder } => {
                let mut res = async {
                    let mut inner = self.inner.write().await;

                    if matches!(inner.server, ServerState::Disabled) {
                        return Err(ffx::RepositoryError::ServerNotRunning);
                    }

                    pkg_config::set_repository_server_enabled(true).await.map_err(|err| {
                        tracing::error!("failed to save server enabled flag to config: {:#?}", err);
                        ffx::RepositoryError::InternalError
                    })?;

                    let address = address.map(|addr| SocketAddress::from(*addr).0);

                    match inner.start_server(address).await {
                        Ok(Some(addr)) => Ok(SocketAddress(addr).into()),
                        Ok(None) => {
                            tracing::warn!("Not starting server because the server is disabled");
                            Err(ffx::RepositoryError::ServerNotRunning)
                        }
                        Err(err) => Err(err.into()),
                    }
                }
                .await;

                // If we started the server, make sure we've registered all the repositories on our
                // targets in the background.
                if res.is_ok() {
                    let cx = cx.clone();
                    let inner = Arc::clone(&self.inner);
                    let registrar = Arc::clone(&self.registrar);
                    fasync::Task::local(async move {
                        load_registrations_from_config(&cx, &inner, None, registrar).await;
                    })
                    .detach();
                }

                responder.send(&mut res)?;

                Ok(())
            }
            ffx::RepositoryRegistryRequest::ServerStop { responder } => {
                let res = async {
                    pkg_config::set_repository_server_enabled(false).await.map_err(|err| {
                        tracing::error!(
                            "failed to save server disabled flag to config: {:#?}",
                            err
                        );
                        ffx::RepositoryError::InternalError
                    })?;

                    pkg_config::set_repository_server_last_address_used("".to_string())
                        .await
                        .map_err(|err| {
                            tracing::error!(
                                "failed to save server last address used flag to config: {:#?}",
                                err
                            );
                            ffx::RepositoryError::InternalError
                        })?;

                    self.inner.write().await.stop_server().await?;

                    Ok(())
                }
                .await;

                responder.send(res)?;

                Ok(())
            }
            ffx::RepositoryRegistryRequest::ServerStatus { responder } => {
                let status = match self.inner.read().await.server {
                    ServerState::Running(ref info) => {
                        ServerStatus::Running { address: info.server.local_addr() }
                    }
                    ServerState::Stopped => ServerStatus::Stopped,
                    ServerState::Disabled => ServerStatus::Disabled,
                };

                responder.send(&status.into())?;

                Ok(())
            }
            ffx::RepositoryRegistryRequest::AddRepository { name, repository, responder } => {
                let res = match repository.try_into() {
                    Ok(repo_spec) => {
                        add_repository(&name, &repo_spec, SaveConfig::Save, Arc::clone(&self.inner))
                            .await
                    }
                    Err(err) => Err(err.into()),
                };

                responder.send(res)?;

                Ok(())
            }
            ffx::RepositoryRegistryRequest::RemoveRepository { name, responder } => {
                responder.send(self.remove_repository(cx, &name).await)?;

                metrics::remove_repository_event().await;

                Ok(())
            }
            ffx::RepositoryRegistryRequest::RegisterTarget {
                target_info,
                responder,
                alias_conflict_mode,
            } => {
                let alias_conflict_mode =
                    RepositoryRegistrationAliasConflictMode::try_from(alias_conflict_mode).unwrap();
                let res = match RepositoryTarget::try_from(target_info) {
                    Ok(target_info) => {
                        self.registrar
                            .register_target(
                                cx,
                                target_info,
                                SaveConfig::Save,
                                Arc::clone(&self.inner),
                                alias_conflict_mode,
                            )
                            .await
                    }
                    Err(err) => Err(err.into()),
                };

                responder.send(res)?;

                metrics::register_repository_event().await;

                Ok(())
            }
            ffx::RepositoryRegistryRequest::DeregisterTarget {
                repository_name,
                target_identifier,
                responder,
            } => {
                responder
                    .send(self.deregister_target(cx, repository_name, target_identifier).await)?;

                metrics::deregister_repository_event().await;

                Ok(())
            }
            ffx::RepositoryRegistryRequest::ListPackages {
                name,
                iterator,
                include_fields,
                responder,
            } => {
                responder.send(self.list_packages(&name, iterator, include_fields).await)?;
                Ok(())
            }
            ffx::RepositoryRegistryRequest::ShowPackage {
                repository_name,
                package_name,
                iterator,
                responder,
            } => {
                responder
                    .send(self.show_package(&repository_name, &package_name, iterator).await)?;
                Ok(())
            }
            ffx::RepositoryRegistryRequest::ListRepositories { iterator, .. } => {
                let mut stream = iterator.into_stream()?;

                let repositories =
                    self.inner.read().await.manager.repositories().collect::<Vec<_>>();

                let mut values = Vec::with_capacity(repositories.len());
                for (name, repo) in repositories {
                    values.push(ffx::RepositoryConfig {
                        name,
                        spec: repo.read().await.spec().into(),
                    });
                }

                fasync::Task::spawn(async move {
                    let mut chunks = SliceChunker::new(&mut values);

                    while let Some(request) = stream.next().await {
                        match request {
                            Ok(ffx::RepositoryIteratorRequest::Next { responder }) => {
                                let chunk = chunks.next();

                                if let Err(err) = responder.send(chunk) {
                                    tracing::warn!(
                                        "Error responding to RepositoryIterator request: {:#?}",
                                        err
                                    );
                                    break;
                                }

                                if chunk.is_empty() {
                                    break;
                                }
                            }
                            Err(err) => {
                                tracing::warn!(
                                    "Error in RepositoryIterator request stream: {:#?}",
                                    err
                                );
                                break;
                            }
                        }
                    }
                })
                .detach();
                Ok(())
            }
            ffx::RepositoryRegistryRequest::ListRegisteredTargets { iterator, .. } => {
                let mut stream = iterator.into_stream()?;
                let mut values = pkg::config::get_registrations()
                    .await
                    .into_values()
                    .map(|targets| targets.into_values())
                    .flatten()
                    .map(|x| x.into())
                    .collect::<Vec<_>>();

                fasync::Task::spawn(async move {
                    let mut chunks = SliceChunker::new(&mut values);

                    while let Some(request) = stream.next().await {
                        match request {
                            Ok(ffx::RepositoryTargetsIteratorRequest::Next { responder }) => {
                                let chunk = chunks.next();

                                if let Err(err) = responder.send(chunk) {
                                    tracing::warn!(
                                        "Error responding to RepositoryTargetsIterator request: {:?}",
                                        err
                                    );
                                    break;
                                }

                                if chunk.is_empty() {
                                    break;
                                }
                            }
                            Err(err) => {
                                tracing::warn!("Error in RepositoryTargetsIterator request stream: {:?}", err);
                                break;
                            }
                        }
                    }
                })
                .detach();
                Ok(())
            }
        }
    }

    async fn start(&mut self, cx: &Context) -> Result<(), anyhow::Error> {
        tracing::debug!("Starting repository protocol");

        // Log the server mode to get an understanding of the distribution of users between pm and
        // the ffx repository server.
        match pkg_config::repository_server_mode().await {
            Ok(mode) => {
                metrics::server_mode_event(&mode).await;
            }
            Err(err) => {
                tracing::warn!("Failed to determine if server is enabled from config: {:#}", err);
            }
        }

        // Make sure the server is initially off.
        {
            let mut inner = self.inner.write().await;
            inner.server = ServerState::Stopped;
        }

        // Start the server if it is enabled.
        if pkg_config::get_repository_server_enabled().await? {
            match fetch_repo_address().await {
                Ok(Some(last_addr)) => {
                    if let Err(err) = self.inner.write().await.start_server(Some(last_addr)).await {
                        tracing::error!("failed to start server: {:#}", err);
                    }
                }
                Ok(None) => {
                    tracing::warn!("repository server is enabled, but we are not configured to listen on an address");
                }
                Err(err) => {
                    tracing::error!("failed to read last address used from config: {:#}", err);
                }
            }
        }

        load_repositories_from_config(&self.inner).await;

        self.event_handler_provider
            .setup_event_handlers(cx.clone(), Arc::clone(&self.inner), Arc::clone(&self.registrar))
            .await;

        Ok(())
    }

    async fn stop(&mut self, _cx: &Context) -> Result<(), anyhow::Error> {
        if let Err(err) = self.inner.write().await.stop_server().await {
            tracing::error!("Failed to stop the server: {:#?}", err);
        }

        Ok(())
    }
}

async fn fetch_repo_address() -> anyhow::Result<Option<SocketAddr>> {
    if let Some(last_addr) = pkg_config::get_repository_server_last_address_used().await? {
        Ok(Some(last_addr))
    } else {
        pkg_config::repository_listen_addr().await
    }
}

async fn load_repositories_from_config(inner: &Arc<RwLock<RepoInner>>) {
    for (name, repo_spec) in pkg::config::get_repositories().await {
        if inner.read().await.manager.get(&name).is_some() {
            continue;
        }

        // Add the repository.
        if let Err(err) =
            add_repository(&name, &repo_spec, SaveConfig::DoNotSave, Arc::clone(inner)).await
        {
            tracing::warn!("failed to add the repository {:?}: {:?}", name, err);
        }
    }
}

async fn load_registrations_from_config<R: Registrar>(
    cx: &Context,
    inner: &Arc<RwLock<RepoInner>>,
    target_identifier: Option<String>,
    registrar: Arc<R>,
) {
    // Find any saved registrations for this target and register them on the device.
    for (repo_name, targets) in pkg::config::get_registrations().await {
        for (target_nodename, target_info) in targets {
            if let Some(ref target_identifier) = target_identifier {
                if target_identifier != &target_nodename {
                    continue;
                }
            }

            // Uh oh...
            if let Err(err) = registrar
                .register_target(
                    &cx,
                    target_info,
                    SaveConfig::DoNotSave,
                    Arc::clone(&inner),
                    RepositoryRegistrationAliasConflictMode::Replace,
                )
                .await
            {
                tracing::warn!(
                    "failed to register target {:?} {:?}: {:?}",
                    repo_name,
                    target_nodename,
                    err
                );
                continue;
            } else {
                tracing::info!(
                    "successfully registered repository {:?} on target {:?}",
                    repo_name,
                    target_nodename,
                );
            }
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
            repository::Error::Tuf(tuf::Error::ExpiredMetadata(_)) => {
                ffx::RepositoryError::ExpiredRepositoryMetadata
            }
            _ => ffx::RepositoryError::IoError,
        }
    })
}

#[derive(Clone)]
struct DaemonEventHandler<R: Registrar> {
    cx: Context,
    inner: Arc<RwLock<RepoInner>>,
    registrar: Arc<R>,
}

impl<R: Registrar> DaemonEventHandler<R> {
    /// pub(crate) so that this is visible to tests.
    pub(crate) fn build_matcher(t: TargetInfo) -> Option<String> {
        if let Some(nodename) = t.nodename {
            Some(nodename)
        } else {
            // If this target doesn't have a nodename, we fall back to matching on IP/port.
            // Since this is only used for matching and not connecting,
            // we simply choose the first address in the list.
            if let Some(addr) = t.addresses.first() {
                let addr_str =
                    if addr.ip().is_ipv6() { format!("[{}]", addr) } else { format!("{}", addr) };

                if let Some(p) = t.ssh_port.as_ref() {
                    Some(format!("{}:{}", addr_str, p))
                } else {
                    Some(format!("{}", addr))
                }
            } else {
                None
            }
        }
    }
}

#[async_trait(?Send)]
impl<R: Registrar + 'static> EventHandler<DaemonEvent> for DaemonEventHandler<R> {
    async fn on_event(&self, event: DaemonEvent) -> anyhow::Result<EventStatus> {
        match event {
            DaemonEvent::NewTarget(info) => {
                let matcher = if let Some(s) = Self::build_matcher(info) {
                    s
                } else {
                    return Ok(EventStatus::Waiting);
                };
                let (t, q) = self.cx.get_target_event_queue(Some(matcher)).await?;
                q.add_handler(TargetEventHandler::new(
                    self.cx.clone(),
                    Arc::clone(&self.inner),
                    t,
                    Arc::clone(&self.registrar),
                ))
                .await;
            }
            _ => {}
        }
        Ok(EventStatus::Waiting)
    }
}

#[derive(Clone)]
struct TargetEventHandler<R: Registrar> {
    cx: Context,
    inner: Arc<RwLock<RepoInner>>,
    target: Rc<Target>,
    registrar: Arc<R>,
}

impl<R: Registrar> TargetEventHandler<R> {
    fn new(
        cx: Context,
        inner: Arc<RwLock<RepoInner>>,
        target: Rc<Target>,
        registrar: Arc<R>,
    ) -> Self {
        Self { cx, inner, target, registrar }
    }
}

#[async_trait(?Send)]
impl<R: Registrar> EventHandler<TargetEvent> for TargetEventHandler<R> {
    async fn on_event(&self, event: TargetEvent) -> anyhow::Result<EventStatus> {
        if !matches!(event, TargetEvent::RcsActivated) {
            return Ok(EventStatus::Waiting);
        }

        // Make sure we pick up any repositories that have been added since the last event.
        load_repositories_from_config(&self.inner).await;

        let source_nodename = if let Some(n) = self.target.nodename() {
            n
        } else {
            tracing::warn!("not registering target due to missing nodename {:?}", self.target);
            return Ok(EventStatus::Waiting);
        };

        load_registrations_from_config(
            &self.cx,
            &self.inner,
            Some(source_nodename),
            Arc::clone(&self.registrar),
        )
        .await;

        Ok(EventStatus::Waiting)
    }
}

/// Helper to split a slice of items into chunks that will fit in a single FIDL vec response.
///
/// Note, SliceChunker assumes the fixed overhead of a single fidl response header and a single vec
/// header per chunk.  It must not be used with more complex responses.
struct SliceChunker<'a, I> {
    items: &'a mut [I],
}

impl<'a, I> SliceChunker<'a, I>
where
    I: Measurable,
{
    fn new(items: &'a mut [I]) -> Self {
        Self { items }
    }

    /// Produce the next chunk of items to respond with. Iteration stops when this method returns
    /// an empty slice, which occurs when either:
    /// * All items have been returned
    /// * SliceChunker encounters an item so large that it cannot even be stored in a response
    ///   dedicated to just that one item.
    ///
    /// Once next() returns an empty slice, it will continue to do so in future calls.
    fn next(&mut self) -> &'a mut [I] {
        let entry_count = how_many_items_fit_in_fidl_vec_response(self.items.iter());
        // tmp/swap dance to appease the borrow checker.
        let tmp = std::mem::replace(&mut self.items, &mut []);
        let (chunk, rest) = tmp.split_at_mut(entry_count);
        self.items = rest;
        chunk
    }
}

// FIXME(52297) This constant would ideally be exported by the `fidl` crate.
// sizeof(TransactionHeader) + sizeof(VectorHeader)
const FIDL_VEC_RESPONSE_OVERHEAD_BYTES: usize = 32;

/// Assumes the fixed overhead of a single fidl response header and a single vec header per chunk.
/// It must not be used with more complex responses.
fn how_many_items_fit_in_fidl_vec_response<'a, I, T>(items: I) -> usize
where
    I: IntoIterator<Item = &'a T>,
    T: Measurable + 'a,
{
    let mut bytes_used: usize = FIDL_VEC_RESPONSE_OVERHEAD_BYTES;
    let mut count = 0;

    for item in items {
        bytes_used += item.measure().num_bytes;
        if bytes_used > ZX_CHANNEL_MAX_MSG_BYTES as usize {
            break;
        }
        count += 1;
    }
    count
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        addr::TargetAddr,
        assert_matches::assert_matches,
        ffx_config::ConfigLevel,
        fidl::{self, endpoints::Request},
        fidl_fuchsia_developer_ffx as ffx,
        fidl_fuchsia_developer_ffx_ext::RepositoryStorageType,
        fidl_fuchsia_developer_remotecontrol as rcs,
        fidl_fuchsia_net::{IpAddress, Ipv4Address},
        fidl_fuchsia_pkg::{
            MirrorConfig, RepositoryConfig, RepositoryKeyConfig, RepositoryManagerRequest,
        },
        fidl_fuchsia_pkg_rewrite::{
            EditTransactionRequest, EngineMarker as RewriteEngineMarker,
            EngineRequest as RewriteEngineRequest, RuleIteratorRequest,
        },
        fuchsia_repo::{manager::RepositoryManager, server::RepositoryServer},
        futures::TryStreamExt,
        pretty_assertions::assert_eq,
        protocols::testing::FakeDaemonBuilder,
        std::{
            cell::RefCell,
            convert::TryInto,
            fs,
            future::Future,
            net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
            rc::Rc,
            str::FromStr,
            sync::{Arc, Mutex},
        },
    };

    const REPO_NAME: &str = "some-repo";
    const TARGET_NODENAME: &str = "some-target";
    const HOST_ADDR: &str = "1.2.3.4";
    const DEVICE_ADDR: &str = "127.0.0.1:5";
    const DEVICE_PORT: u16 = 5;
    const EMPTY_REPO_PATH: &str = "host_x64/test_data/ffx_daemon_protocol_repo/empty-repo";

    macro_rules! rule {
        ($host_match:expr => $host_replacement:expr,
         $path_prefix_match:expr => $path_prefix_replacement:expr) => {
            Rule::new($host_match, $host_replacement, $path_prefix_match, $path_prefix_replacement)
                .unwrap()
        };
    }

    macro_rules! assert_vec_empty {
        ($input_vector:expr) => {
            assert_eq!($input_vector, vec![]);
        };
    }

    async fn test_repo_config_fidl<S: SshProvider + 'static>(
        repo: &Rc<RefCell<Repo<TestEventHandlerProvider, RealRegistrar<S>>>>,
    ) -> RepositoryConfig {
        test_repo_config_fidl_with_repo_host(repo, None, REPO_NAME.into()).await
    }

    async fn test_repo_config_fidl_with_repo_host<S: SshProvider + 'static>(
        repo: &Rc<RefCell<Repo<TestEventHandlerProvider, RealRegistrar<S>>>>,
        repo_host: Option<String>,
        repo_name: String,
    ) -> RepositoryConfig {
        // The repository server started on a random address, so look it up.
        let inner = Arc::clone(&repo.borrow().inner);
        let addr = if let Some(addr) = inner.read().await.server.listen_addr() {
            addr
        } else {
            panic!("server is not running");
        };

        let repo_host = if let Some(repo_host) = repo_host {
            format!("{}:{}", repo_host, addr.port())
        } else {
            addr.to_string()
        };

        RepositoryConfig {
            mirrors: Some(vec![MirrorConfig {
                mirror_url: Some(format!("http://{}/{}", repo_host, repo_name)),
                subscribe: Some(true),
                ..Default::default()
            }]),
            repo_url: Some(format!("fuchsia-pkg://{}", repo_name)),
            root_keys: Some(vec![RepositoryKeyConfig::Ed25519Key(vec![
                29, 76, 86, 76, 184, 70, 108, 73, 249, 127, 4, 47, 95, 63, 36, 35, 101, 255, 212,
                33, 10, 154, 26, 130, 117, 157, 125, 88, 175, 214, 109, 113,
            ])]),
            root_version: Some(1),
            root_threshold: Some(1),
            use_local_mirror: Some(false),
            storage_type: Some(fidl_fuchsia_pkg::RepositoryStorageType::Ephemeral),
            ..Default::default()
        }
    }

    async fn test_repo_config_ssh<S: SshProvider + 'static>(
        repo: &Rc<RefCell<Repo<TestEventHandlerProvider, RealRegistrar<S>>>>,
    ) -> Vec<String> {
        test_repo_config_ssh_with_repo_host(repo, None, REPO_NAME.into()).await
    }

    async fn test_repo_config_ssh_with_repo_host<S: SshProvider + 'static>(
        repo: &Rc<RefCell<Repo<TestEventHandlerProvider, RealRegistrar<S>>>>,
        repo_host: Option<String>,
        repo_name: String,
    ) -> Vec<String> {
        // The repository server started on a random address, so look it up.
        let inner = Arc::clone(&repo.borrow().inner);
        let addr = if let Some(addr) = inner.read().await.server.listen_addr() {
            addr
        } else {
            panic!("server is not running");
        };

        let repo_host = if let Some(repo_host) = repo_host {
            format!("{}:{}", repo_host, addr.port())
        } else {
            addr.to_string()
        };

        let repo_config_endpoint = format!("http://{}/{}/repo.config", repo_host, repo_name);

        let args = vec!["pkgctl", "repo", "add", "url", repo_config_endpoint.as_str()];
        args.into_iter().map(|s| s.to_string()).collect()
    }

    async fn test_target_alias_ssh<S: SshProvider + 'static>(
        repo: &Rc<RefCell<Repo<TestEventHandlerProvider, RealRegistrar<S>>>>,
        repo_name: &str,
        target: &ffx::RepositoryTarget,
    ) -> Vec<String> {
        let aliases = if let Some(aliases) = &target.aliases {
            BTreeSet::<String>::from_iter(aliases.clone().into_iter())
        } else {
            // Fallback to repo aliases
            repo.borrow()
                .inner
                .read()
                .await
                .manager
                .get(repo_name)
                .unwrap()
                .read()
                .await
                .aliases()
                .clone()
        };

        let alias_rules = aliases_to_rules(repo_name, &aliases).unwrap();
        let rules_config_json_string =
            rules_config_to_json_string(RuleConfig::Version1(alias_rules)).unwrap();

        let repo_args = vec!["pkgctl", "rule", "replace", "json", &rules_config_json_string];
        repo_args.into_iter().map(|s| s.to_string()).collect()
    }

    // Communication with device.
    enum TestRunMode {
        Fidl,
        Ssh,
    }

    struct FakeRepositoryManager {
        events: Arc<Mutex<Vec<RepositoryManagerEvent>>>,
    }

    impl FakeRepositoryManager {
        fn new() -> (
            Self,
            impl Fn(&Context, Request<RepositoryManagerMarker>) -> Result<(), anyhow::Error> + 'static,
        ) {
            let events = Arc::new(Mutex::new(Vec::new()));
            let events_closure = Arc::clone(&events);

            let closure = move |_cx: &Context, req| match req {
                RepositoryManagerRequest::Add { repo, responder } => {
                    events_closure.lock().unwrap().push(RepositoryManagerEvent::Add { repo });
                    responder.send(Ok(()))?;
                    Ok(())
                }
                RepositoryManagerRequest::Remove { repo_url, responder } => {
                    events_closure
                        .lock()
                        .unwrap()
                        .push(RepositoryManagerEvent::Remove { repo_url });
                    responder.send(Ok(()))?;
                    Ok(())
                }
                _ => panic!("unexpected request: {:?}", req),
            };

            (Self { events }, closure)
        }

        fn take_events(&self) -> Vec<RepositoryManagerEvent> {
            self.events.lock().unwrap().drain(..).collect::<Vec<_>>()
        }
    }

    #[derive(Debug, PartialEq)]
    struct PkgctlCommandEvent {
        device_addr: SocketAddr,
        args: Vec<String>,
    }

    enum PkgctlCommandType {
        RepoAdd,
        RuleReplace,
    }

    #[derive(Debug, PartialEq)]
    enum RepositoryManagerEvent {
        Add { repo: RepositoryConfig },
        Remove { repo_url: String },
    }

    struct ErroringRepositoryManager {
        events: Arc<Mutex<Vec<RepositoryManagerEvent>>>,
    }

    impl ErroringRepositoryManager {
        fn new() -> (
            Self,
            impl Fn(&Context, Request<RepositoryManagerMarker>) -> Result<(), anyhow::Error> + 'static,
        ) {
            let events = Arc::new(Mutex::new(Vec::new()));
            let events_closure = Arc::clone(&events);

            let closure = move |_cx: &Context, req| match req {
                RepositoryManagerRequest::Add { repo, responder } => {
                    events_closure.lock().unwrap().push(RepositoryManagerEvent::Add { repo });
                    responder.send(Err(1)).unwrap();
                    Ok(())
                }
                RepositoryManagerRequest::Remove { repo_url: _, responder } => {
                    responder.send(Ok(())).unwrap();
                    Ok(())
                }
                _ => {
                    panic!("unexpected RepositoryManager request {:?}", req);
                }
            };

            (Self { events }, closure)
        }

        fn take_events(&self) -> Vec<RepositoryManagerEvent> {
            self.events.lock().unwrap().drain(..).collect::<Vec<_>>()
        }
    }

    struct FakeRewriteEngine {
        events: Arc<Mutex<Vec<RewriteEngineEvent>>>,
    }

    impl FakeRewriteEngine {
        fn new() -> (
            Self,
            impl Fn(&Context, Request<RewriteEngineMarker>) -> Result<(), anyhow::Error> + 'static,
        ) {
            Self::with_rules(vec![])
        }

        fn with_rules(
            rules: Vec<Rule>,
        ) -> (
            Self,
            impl Fn(&Context, Request<RewriteEngineMarker>) -> Result<(), anyhow::Error> + 'static,
        ) {
            let rules = Arc::new(Mutex::new(rules));
            let events = Arc::new(Mutex::new(Vec::new()));
            let events_closure = Arc::clone(&events);

            let closure = move |_cx: &Context, req| {
                match req {
                    RewriteEngineRequest::StartEditTransaction {
                        transaction,
                        control_handle: _,
                    } => {
                        let rules = Arc::clone(&rules);
                        let events_closure = Arc::clone(&events_closure);
                        fasync::Task::local(async move {
                            let mut stream = transaction.into_stream().unwrap();
                            while let Some(request) = stream.next().await {
                                let request = request.unwrap();
                                match request {
                                    EditTransactionRequest::ResetAll { control_handle: _ } => {
                                        events_closure
                                            .lock()
                                            .unwrap()
                                            .push(RewriteEngineEvent::ResetAll);
                                    }
                                    EditTransactionRequest::ListDynamic {
                                        iterator,
                                        control_handle: _,
                                    } => {
                                        events_closure
                                            .lock()
                                            .unwrap()
                                            .push(RewriteEngineEvent::ListDynamic);
                                        let mut stream = iterator.into_stream().unwrap();

                                        let mut rules = rules.lock().unwrap().clone().into_iter();

                                        while let Some(req) = stream.try_next().await.unwrap() {
                                            let RuleIteratorRequest::Next { responder } = req;
                                            events_closure
                                                .lock()
                                                .unwrap()
                                                .push(RewriteEngineEvent::IteratorNext);

                                            if let Some(rule) = rules.next() {
                                                responder.send(&[rule.into()]).unwrap();
                                            } else {
                                                responder.send(&[]).unwrap();
                                            }
                                        }
                                    }
                                    EditTransactionRequest::Add { rule, responder } => {
                                        events_closure.lock().unwrap().push(
                                            RewriteEngineEvent::EditTransactionAdd {
                                                rule: rule.try_into().unwrap(),
                                            },
                                        );
                                        responder.send(Ok(())).unwrap()
                                    }
                                    EditTransactionRequest::Commit { responder } => {
                                        events_closure
                                            .lock()
                                            .unwrap()
                                            .push(RewriteEngineEvent::EditTransactionCommit);
                                        responder.send(Ok(())).unwrap()
                                    }
                                }
                            }
                        })
                        .detach();
                    }
                    _ => panic!("unexpected request: {:?}", req),
                }

                Ok(())
            };

            (Self { events }, closure)
        }

        fn take_events(&self) -> Vec<RewriteEngineEvent> {
            self.events.lock().unwrap().drain(..).collect::<Vec<_>>()
        }
    }

    #[derive(Debug, PartialEq)]
    enum RewriteEngineEvent {
        ResetAll,
        ListDynamic,
        IteratorNext,
        EditTransactionAdd { rule: Rule },
        EditTransactionCommit,
    }

    struct FakeRcs {
        events: Arc<Mutex<Vec<RcsEvent>>>,
    }

    impl FakeRcs {
        fn new() -> (Self, impl Fn(rcs::RemoteControlRequest, Option<String>) -> ()) {
            let events = Arc::new(Mutex::new(Vec::new()));
            let events_closure = Arc::clone(&events);

            let closure = move |req: rcs::RemoteControlRequest, target: Option<String>| {
                tracing::info!("got a rcs request: {:?} {:?}", req, target);

                match (req, target.as_deref()) {
                    (
                        rcs::RemoteControlRequest::ReverseTcp { responder, .. },
                        Some(TARGET_NODENAME),
                    ) => {
                        events_closure.lock().unwrap().push(RcsEvent::ReverseTcp);
                        responder.send(Ok(())).unwrap()
                    }
                    (req, target) => {
                        panic!("Unexpected request {:?}: {:?}", target, req)
                    }
                }
            };

            (Self { events }, closure)
        }

        fn take_events(&self) -> Vec<RcsEvent> {
            self.events.lock().unwrap().drain(..).collect()
        }
    }

    #[derive(Debug, PartialEq)]
    enum RcsEvent {
        ReverseTcp,
    }

    #[derive(Default)]
    struct TestEventHandlerProvider;

    #[async_trait::async_trait(?Send)]
    impl<R: Registrar + 'static> EventHandlerProvider<R> for TestEventHandlerProvider {
        async fn setup_event_handlers(
            &mut self,
            cx: Context,
            inner: Arc<RwLock<RepoInner>>,
            registrar: Arc<R>,
        ) {
            let target = Target::new_named(TARGET_NODENAME.to_string());

            // Used for ssh-workflows.
            let device_addr = TargetAddr::from_str(DEVICE_ADDR).unwrap();
            target.addrs_insert(device_addr);
            assert!(target.set_preferred_ssh_address(device_addr));
            target.set_ssh_port(Some(DEVICE_PORT));

            let handler = TargetEventHandler::new(cx, inner, target, registrar);
            handler.on_event(TargetEvent::RcsActivated).await.unwrap();
        }
    }

    #[derive(Default)]
    struct TestSshProvider {
        repo_register_commands: Arc<Mutex<Vec<PkgctlCommandEvent>>>,
        rule_replace_commands: Arc<Mutex<Vec<PkgctlCommandEvent>>>,
    }

    impl TestSshProvider {
        fn new() -> Self {
            let repo_register_commands = Arc::new(Mutex::new(Vec::new()));
            let rule_replace_commands = Arc::new(Mutex::new(Vec::new()));

            Self { repo_register_commands, rule_replace_commands }
        }

        fn take_events(&self, pkgctl_command_type: PkgctlCommandType) -> Vec<PkgctlCommandEvent> {
            match pkgctl_command_type {
                PkgctlCommandType::RepoAdd => {
                    self.repo_register_commands.lock().unwrap().drain(..).collect()
                }
                PkgctlCommandType::RuleReplace => {
                    self.rule_replace_commands.lock().unwrap().drain(..).collect()
                }
            }
        }
    }

    #[async_trait::async_trait(?Send)]
    impl SshProvider for TestSshProvider {
        async fn run_ssh_command(
            &self,
            device_addr: SocketAddr,
            args: Vec<&str>,
        ) -> Result<(), ffx::RepositoryError> {
            let string_args: Vec<String> = args.into_iter().map(|s| s.to_string()).collect();
            assert!(string_args.len() == 5);

            match string_args[1].as_str() {
                "repo" => {
                    self.repo_register_commands
                        .lock()
                        .unwrap()
                        .push(PkgctlCommandEvent { device_addr, args: string_args });
                }
                "rule" => {
                    self.rule_replace_commands
                        .lock()
                        .unwrap()
                        .push(PkgctlCommandEvent { device_addr, args: string_args });
                }
                _ => {
                    tracing::error!("Unknown pkgctl event in test...");
                    return Err(ffx::RepositoryError::InternalError);
                }
            }

            Ok(())
        }
    }

    impl Repo<TestEventHandlerProvider, RealRegistrar<TestSshProvider>> {
        fn take_events(&self, pkgctl_command_type: PkgctlCommandType) -> Vec<PkgctlCommandEvent> {
            self.registrar.ssh_provider.take_events(pkgctl_command_type)
        }
    }

    #[derive(Default)]
    struct ErroringSshProvider {
        repo_register_commands: Arc<Mutex<Vec<PkgctlCommandEvent>>>,
        rule_replace_commands: Arc<Mutex<Vec<PkgctlCommandEvent>>>,
    }

    impl ErroringSshProvider {
        fn new() -> Self {
            let repo_register_commands = Arc::new(Mutex::new(Vec::new()));
            let rule_replace_commands = Arc::new(Mutex::new(Vec::new()));

            Self { repo_register_commands, rule_replace_commands }
        }

        fn take_events(&self, pkgctl_command_type: PkgctlCommandType) -> Vec<PkgctlCommandEvent> {
            match pkgctl_command_type {
                PkgctlCommandType::RepoAdd => {
                    self.repo_register_commands.lock().unwrap().drain(..).collect()
                }
                PkgctlCommandType::RuleReplace => {
                    self.rule_replace_commands.lock().unwrap().drain(..).collect()
                }
            }
        }
    }

    #[async_trait::async_trait(?Send)]
    impl SshProvider for ErroringSshProvider {
        async fn run_ssh_command(
            &self,
            device_addr: SocketAddr,
            args: Vec<&str>,
        ) -> Result<(), ffx::RepositoryError> {
            let string_args: Vec<String> = args.into_iter().map(|s| s.to_string()).collect();

            match string_args[1].as_str() {
                "repo" => {
                    self.repo_register_commands
                        .lock()
                        .unwrap()
                        .push(PkgctlCommandEvent { device_addr, args: string_args });
                }
                "rule" => {
                    self.rule_replace_commands
                        .lock()
                        .unwrap()
                        .push(PkgctlCommandEvent { device_addr, args: string_args });
                }
                _ => {
                    tracing::error!("Unknown pkgctl event in test...");
                    return Err(ffx::RepositoryError::InternalError);
                }
            }

            Err(ffx::RepositoryError::RepositoryManagerError)
        }
    }

    impl Repo<TestEventHandlerProvider, RealRegistrar<ErroringSshProvider>> {
        fn take_events(&self, pkgctl_command_type: PkgctlCommandType) -> Vec<PkgctlCommandEvent> {
            self.registrar.ssh_provider.take_events(pkgctl_command_type)
        }
    }

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

    async fn add_repo(proxy: &ffx::RepositoryRegistryProxy, repo_name: &str) {
        let spec = pm_repo_spec();
        proxy
            .add_repository(repo_name, &spec.into())
            .await
            .expect("communicated with proxy")
            .expect("adding repository to succeed");
    }

    async fn register_target(proxy: &ffx::RepositoryRegistryProxy, target: ffx::RepositoryTarget) {
        // We need to start the server before we can register a repository
        // on a target.
        proxy
            .server_start(None)
            .await
            .expect("communicated with proxy")
            .expect("starting the server to succeed");

        proxy
            .register_target(
                &target,
                fidl_fuchsia_developer_ffx::RepositoryRegistrationAliasConflictMode::Replace,
            )
            .await
            .expect("communicated with proxy")
            .expect("target registration to succeed");
    }

    async fn get_repositories(proxy: &ffx::RepositoryRegistryProxy) -> Vec<ffx::RepositoryConfig> {
        let (client, server) = fidl::endpoints::create_endpoints();
        proxy.list_repositories(server).unwrap();
        let client = client.into_proxy().unwrap();

        let mut repositories = vec![];
        loop {
            let chunk = client.next().await.unwrap();
            if chunk.is_empty() {
                break;
            }
            repositories.extend(chunk);
        }

        repositories
    }

    async fn get_target_registrations(
        proxy: &ffx::RepositoryRegistryProxy,
    ) -> Vec<ffx::RepositoryTarget> {
        let (client, server) = fidl::endpoints::create_endpoints();
        proxy.list_registered_targets(server).unwrap();
        let client = client.into_proxy().unwrap();

        let mut registrations = vec![];
        loop {
            let chunk = client.next().await.unwrap();
            if chunk.is_empty() {
                break;
            }
            registrations.extend(chunk);
        }

        registrations
    }

    lazy_static::lazy_static! {
        static ref TEST_LOCK: Arc<Mutex<()>> = Arc::new(Mutex::new(()));
    }

    // FIXME(fxbug.dev/80740): Rust tests on host use panic=unwind, which causes all the tests to
    // run in the same process. Unfortunately ffx_config is global, and so each of these tests
    // could step on each others ffx_config entries if run in parallel. To avoid this, we will:
    //
    // * use a global lock to make sure each test runs sequentially
    // * clear out the config keys before we run each test to make sure state isn't leaked across
    //   tests.
    fn run_test<F: Future>(mode: TestRunMode, fut: F) -> F::Output {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|_| {
            panic!("the test lock is poisoned, which probably means another test failed")
        });

        let _ = simplelog::SimpleLogger::init(
            simplelog::LevelFilter::Debug,
            simplelog::Config::default(),
        );

        fuchsia_async::TestExecutor::new().run_singlethreaded(async move {
            let _env = ffx_config::test_init().await.unwrap();

            // Since ffx_config is global, it's possible to leave behind entries
            // across tests. Let's clean them up.
            let _ = ffx_config::query("repository").remove().await;

            // Most tests want the server to be running.
            ffx_config::query("repository.server.mode")
                .level(Some(ConfigLevel::User))
                .set("ffx".into())
                .await
                .unwrap();

            // Repo will automatically start a server, so make sure it picks a random local port.
            let addr: SocketAddr = (Ipv4Addr::LOCALHOST, 0).into();
            ffx_config::query("repository.server.listen")
                .level(Some(ConfigLevel::User))
                .set(addr.to_string().into())
                .await
                .unwrap();

            match mode {
                TestRunMode::Fidl => {
                    ffx_config::query("repository.registration-mode")
                        .level(Some(ConfigLevel::User))
                        .set("fidl".to_string().into())
                        .await
                        .unwrap();
                }
                TestRunMode::Ssh => {
                    ffx_config::query("repository.registration-mode")
                        .level(Some(ConfigLevel::User))
                        .set("ssh".to_string().into())
                        .await
                        .unwrap();
                }
            }

            fut.await
        })
    }

    #[test]
    fn test_load_from_config_empty() {
        run_test(TestRunMode::Fidl, async {
            // Initialize a simple repository.
            ffx_config::query("repository")
                .level(Some(ConfigLevel::User))
                .set(serde_json::json!({}))
                .await
                .unwrap();

            let daemon = FakeDaemonBuilder::new()
                .register_fidl_protocol::<Repo<TestEventHandlerProvider>>()
                .build();
            let proxy = daemon.open_proxy::<ffx::RepositoryRegistryMarker>().await;

            assert_vec_empty!(get_repositories(&proxy).await);
            assert_vec_empty!(get_target_registrations(&proxy).await);
        })
    }

    async fn check_load_from_config_with_data(test_run_mode: TestRunMode) {
        // Initialize a simple repository.
        let repo_path = fs::canonicalize(EMPTY_REPO_PATH).unwrap().to_str().unwrap().to_string();

        ffx_config::query("repository")
            .level(Some(ConfigLevel::User))
            .set(serde_json::json!({
                "repositories": {
                    "repo1": {
                        "type": "pm",
                        "path": repo_path,
                    },
                    "repo2": {
                        "type": "pm",
                        "path": repo_path,
                        "aliases": ["corp2.com"],
                    },
                    "repo3": {
                        "type": "pm",
                        "path": repo_path,
                        "aliases": ["corp3.com"],
                    },
                },
                "registrations": {
                    "repo1": {
                        TARGET_NODENAME: {
                            "repo_name": "repo1",
                            "target_identifier": TARGET_NODENAME,
                            "aliases": [ "fuchsia.com", "example.com" ],
                            "storage_type": "ephemeral",
                        },
                    },
                    "repo2": {
                        TARGET_NODENAME: {
                            "repo_name": "repo2",
                            "target_identifier": TARGET_NODENAME,
                            "aliases": (),
                            "storage_type": "ephemeral",
                        },
                    },
                    "repo3": {
                        TARGET_NODENAME: {
                            "repo_name": "repo3",
                            "target_identifier": TARGET_NODENAME,
                            "aliases": [ "anothercorp3.com" ],
                            "storage_type": "ephemeral",
                        },
                    },
                },
                "server": {
                    "enabled": true,
                    "mode": "ffx",
                    "listen": SocketAddr::from((Ipv4Addr::LOCALHOST, 0)).to_string(),
                },
            }))
            .await
            .unwrap();

        match test_run_mode {
            TestRunMode::Fidl => {
                ffx_config::query("repository.registration-mode")
                    .level(Some(ConfigLevel::User))
                    .set("fidl".to_string().into())
                    .await
                    .unwrap();
            }
            TestRunMode::Ssh => {
                ffx_config::query("repository.registration-mode")
                    .level(Some(ConfigLevel::User))
                    .set("ssh".to_string().into())
                    .await
                    .unwrap();
            }
        }

        let repo = Rc::new(RefCell::new(Repo {
            inner: RepoInner::new(),
            event_handler_provider: TestEventHandlerProvider,
            registrar: Arc::new(RealRegistrar { ssh_provider: Arc::new(TestSshProvider::new()) }),
        }));
        let (_fake_rcs, fake_rcs_closure) = FakeRcs::new();
        let (fake_repo_manager, fake_repo_manager_closure) = FakeRepositoryManager::new();
        let (fake_engine, fake_engine_closure) = FakeRewriteEngine::new();
        let device_address = ffx::TargetAddrInfo::IpPort(ffx::TargetIpPort {
            ip: IpAddress::Ipv4(Ipv4Address { addr: [127, 0, 0, 1] }),
            scope_id: 0,
            port: DEVICE_PORT,
        });

        let daemon = FakeDaemonBuilder::new()
            .rcs_handler(fake_rcs_closure)
            .register_instanced_protocol_closure::<RepositoryManagerMarker, _>(
                fake_repo_manager_closure,
            )
            .register_instanced_protocol_closure::<RewriteEngineMarker, _>(fake_engine_closure)
            .inject_fidl_protocol(Rc::clone(&repo))
            .target(ffx::TargetInfo {
                nodename: Some(TARGET_NODENAME.to_string()),
                ssh_host_address: Some(ffx::SshHostAddrInfo { address: HOST_ADDR.to_string() }),
                addresses: Some(vec![device_address.clone()]),
                ssh_address: Some(device_address.clone()),
                ..Default::default()
            })
            .build();

        let proxy = daemon.open_proxy::<ffx::RepositoryRegistryMarker>().await;

        // The server should have started.
        {
            let inner = Arc::clone(&repo.borrow().inner);
            assert_matches!(inner.read().await.server, ServerState::Running(_));
        }

        // Make sure we set up the repository and rewrite rules on the device.
        match test_run_mode {
            TestRunMode::Fidl => {
                assert_eq!(
                    fake_repo_manager.take_events(),
                    vec![
                        RepositoryManagerEvent::Add {
                            repo: test_repo_config_fidl_with_repo_host(&repo, None, "repo1".into())
                                .await
                        },
                        RepositoryManagerEvent::Add {
                            repo: test_repo_config_fidl_with_repo_host(&repo, None, "repo2".into())
                                .await
                        },
                        RepositoryManagerEvent::Add {
                            repo: test_repo_config_fidl_with_repo_host(&repo, None, "repo3".into())
                                .await
                        },
                    ],
                );

                // Expect SSH flow untouched.
                assert_vec_empty!(repo.borrow().take_events(PkgctlCommandType::RepoAdd));
            }
            TestRunMode::Ssh => {
                assert_eq!(
                    repo.borrow().take_events(PkgctlCommandType::RepoAdd),
                    vec![
                        PkgctlCommandEvent {
                            device_addr: SocketAddr::from_str(DEVICE_ADDR).unwrap(),
                            args: test_repo_config_ssh_with_repo_host(&repo, None, "repo1".into())
                                .await
                        },
                        PkgctlCommandEvent {
                            device_addr: SocketAddr::from_str(DEVICE_ADDR).unwrap(),
                            args: test_repo_config_ssh_with_repo_host(&repo, None, "repo2".into())
                                .await
                        },
                        PkgctlCommandEvent {
                            device_addr: SocketAddr::from_str(DEVICE_ADDR).unwrap(),
                            args: test_repo_config_ssh_with_repo_host(&repo, None, "repo3".into())
                                .await
                        },
                    ],
                );

                // Expect FIDL flow untouched.
                assert_vec_empty!(fake_repo_manager.take_events());
            }
        }

        // Make sure we set up the repository and rewrite rules on the device.
        match test_run_mode {
            TestRunMode::Fidl => {
                assert_eq!(
                    fake_engine.take_events(),
                    vec![
                        RewriteEngineEvent::ListDynamic,
                        RewriteEngineEvent::IteratorNext,
                        RewriteEngineEvent::ResetAll,
                        RewriteEngineEvent::EditTransactionAdd {
                            rule: rule!("example.com" => "repo1", "/" => "/"),
                        },
                        RewriteEngineEvent::EditTransactionAdd {
                            rule: rule!("fuchsia.com" => "repo1", "/" => "/"),
                        },
                        RewriteEngineEvent::EditTransactionCommit,
                        RewriteEngineEvent::ListDynamic,
                        RewriteEngineEvent::IteratorNext,
                        RewriteEngineEvent::ResetAll,
                        RewriteEngineEvent::EditTransactionAdd {
                            rule: rule!("corp2.com" => "repo2", "/" => "/"),
                        },
                        RewriteEngineEvent::EditTransactionCommit,
                        RewriteEngineEvent::ListDynamic,
                        RewriteEngineEvent::IteratorNext,
                        RewriteEngineEvent::ResetAll,
                        RewriteEngineEvent::EditTransactionAdd {
                            rule: rule!("anothercorp3.com" => "repo3", "/" => "/"),
                        },
                        RewriteEngineEvent::EditTransactionCommit,
                    ],
                );

                // Expect SSH flow untouched.
                assert_vec_empty!(repo.borrow().take_events(PkgctlCommandType::RepoAdd));
            }
            TestRunMode::Ssh => {
                assert_eq!(
                    repo.borrow().take_events(PkgctlCommandType::RuleReplace),
                    vec![
                        PkgctlCommandEvent {
                            device_addr: SocketAddr::from_str(DEVICE_ADDR).unwrap(),
                            args: test_target_alias_ssh(
                                &repo,
                                "repo1",
                                &ffx::RepositoryTarget {
                                    repo_name: Some("repo1".to_string()),
                                    target_identifier: Some(TARGET_NODENAME.to_string()),
                                    aliases: Some(vec![
                                        "fuchsia.com".to_string(),
                                        "example.com".to_string()
                                    ]),
                                    storage_type: Some(ffx::RepositoryStorageType::Ephemeral),
                                    ..Default::default()
                                }
                            )
                            .await
                        },
                        PkgctlCommandEvent {
                            device_addr: SocketAddr::from_str(DEVICE_ADDR).unwrap(),
                            args: test_target_alias_ssh(
                                &repo,
                                "repo2",
                                &ffx::RepositoryTarget {
                                    repo_name: Some("repo2".to_string()),
                                    target_identifier: Some(TARGET_NODENAME.to_string()),
                                    storage_type: Some(ffx::RepositoryStorageType::Ephemeral),
                                    ..Default::default()
                                }
                            )
                            .await
                        },
                        PkgctlCommandEvent {
                            device_addr: SocketAddr::from_str(DEVICE_ADDR).unwrap(),
                            args: test_target_alias_ssh(
                                &repo,
                                "repo3",
                                &ffx::RepositoryTarget {
                                    repo_name: Some("repo3".to_string()),
                                    target_identifier: Some(TARGET_NODENAME.to_string()),
                                    aliases: Some(vec!["anothercorp3.com".to_string()]),
                                    storage_type: Some(ffx::RepositoryStorageType::Ephemeral),
                                    ..Default::default()
                                }
                            )
                            .await
                        }
                    ]
                );

                // Expect FIDL flow untouched.
                assert_vec_empty!(fake_repo_manager.take_events());
            }
        }

        // Make sure we can read back the repositories.
        assert_eq!(
            get_repositories(&proxy).await,
            vec![
                ffx::RepositoryConfig {
                    name: "repo1".to_string(),
                    spec: ffx::RepositorySpec::Pm(ffx::PmRepositorySpec {
                        path: Some(repo_path.clone()),
                        aliases: None,
                        ..Default::default()
                    }),
                },
                ffx::RepositoryConfig {
                    name: "repo2".to_string(),
                    spec: ffx::RepositorySpec::Pm(ffx::PmRepositorySpec {
                        path: Some(repo_path.clone()),
                        aliases: Some(vec!["corp2.com".into()]),
                        ..Default::default()
                    }),
                },
                ffx::RepositoryConfig {
                    name: "repo3".to_string(),
                    spec: ffx::RepositorySpec::Pm(ffx::PmRepositorySpec {
                        path: Some(repo_path.clone()),
                        aliases: Some(vec!["corp3.com".into()]),
                        ..Default::default()
                    }),
                },
            ]
        );

        // Make sure we can read back the target registrations.
        assert_eq!(
            get_target_registrations(&proxy).await,
            vec![
                ffx::RepositoryTarget {
                    repo_name: Some("repo1".to_string()),
                    target_identifier: Some(TARGET_NODENAME.to_string()),
                    aliases: Some(vec!["example.com".to_string(), "fuchsia.com".to_string()]),
                    storage_type: Some(ffx::RepositoryStorageType::Ephemeral),
                    ..Default::default()
                },
                ffx::RepositoryTarget {
                    repo_name: Some("repo2".to_string()),
                    target_identifier: Some(TARGET_NODENAME.to_string()),
                    aliases: None,
                    storage_type: Some(ffx::RepositoryStorageType::Ephemeral),
                    ..Default::default()
                },
                ffx::RepositoryTarget {
                    repo_name: Some("repo3".to_string()),
                    target_identifier: Some(TARGET_NODENAME.to_string()),
                    aliases: Some(vec!["anothercorp3.com".to_string()]),
                    storage_type: Some(ffx::RepositoryStorageType::Ephemeral),
                    ..Default::default()
                },
            ],
        );
    }

    #[test]
    fn test_load_from_config_with_data_with_fidl() {
        run_test(TestRunMode::Fidl, async {
            check_load_from_config_with_data(TestRunMode::Fidl).await;
        });
    }

    #[test]
    fn test_load_from_config_with_data_with_ssh() {
        run_test(TestRunMode::Ssh, async {
            check_load_from_config_with_data(TestRunMode::Ssh).await;
        });
    }

    async fn check_load_from_config_with_disabled_server(test_run_mode: TestRunMode) {
        // Initialize a simple repository.
        let repo_path = fs::canonicalize(EMPTY_REPO_PATH).unwrap().to_str().unwrap().to_string();

        ffx_config::query("repository")
            .level(Some(ConfigLevel::User))
            .set(serde_json::json!({
                "repositories": {
                    REPO_NAME: {
                        "type": "pm",
                        "path": repo_path
                    },
                },
                "registrations": {
                    REPO_NAME: {
                        TARGET_NODENAME: {
                            "repo_name": REPO_NAME,
                            "target_identifier": TARGET_NODENAME,
                            "aliases": [ "example.com", "fuchsia.com" ],
                            "storage_type": "ephemeral",
                        },
                    }
                },
                "server": {
                    "enabled": false,
                    "mode": "ffx",
                    "listen": SocketAddr::from((Ipv4Addr::LOCALHOST, 0)).to_string(),
                },
            }))
            .await
            .unwrap();

        match test_run_mode {
            TestRunMode::Fidl => {
                ffx_config::query("repository.registration-mode")
                    .level(Some(ConfigLevel::User))
                    .set("fidl".to_string().into())
                    .await
                    .unwrap();
            }
            TestRunMode::Ssh => {
                ffx_config::query("repository.registration-mode")
                    .level(Some(ConfigLevel::User))
                    .set("ssh".to_string().into())
                    .await
                    .unwrap();
            }
        }

        let repo = Rc::new(RefCell::new(Repo {
            inner: RepoInner::new(),
            event_handler_provider: TestEventHandlerProvider,
            registrar: Arc::new(RealRegistrar { ssh_provider: Arc::new(TestSshProvider::new()) }),
        }));
        let (_fake_rcs, fake_rcs_closure) = FakeRcs::new();
        let (fake_repo_manager, fake_repo_manager_closure) = FakeRepositoryManager::new();
        let (fake_engine, fake_engine_closure) = FakeRewriteEngine::new();
        let device_address = ffx::TargetAddrInfo::IpPort(ffx::TargetIpPort {
            ip: IpAddress::Ipv4(Ipv4Address { addr: [127, 0, 0, 1] }),
            scope_id: 0,
            port: DEVICE_PORT,
        });

        let daemon = FakeDaemonBuilder::new()
            .rcs_handler(fake_rcs_closure)
            .register_instanced_protocol_closure::<RepositoryManagerMarker, _>(
                fake_repo_manager_closure,
            )
            .register_instanced_protocol_closure::<RewriteEngineMarker, _>(fake_engine_closure)
            .inject_fidl_protocol(Rc::clone(&repo))
            .target(ffx::TargetInfo {
                nodename: Some(TARGET_NODENAME.to_string()),
                ssh_host_address: Some(ffx::SshHostAddrInfo { address: HOST_ADDR.to_string() }),
                addresses: Some(vec![device_address.clone()]),
                ssh_address: Some(device_address.clone()),
                ..Default::default()
            })
            .build();

        let proxy = daemon.open_proxy::<ffx::RepositoryRegistryMarker>().await;

        // The server should be stopped.
        {
            let inner = Arc::clone(&repo.borrow().inner);
            assert_matches!(inner.read().await.server, ServerState::Stopped);
        }

        // Make sure we can read back the repositories.
        assert_eq!(
            get_repositories(&proxy).await,
            vec![ffx::RepositoryConfig {
                name: REPO_NAME.to_string(),
                spec: ffx::RepositorySpec::Pm(ffx::PmRepositorySpec {
                    path: Some(repo_path.clone()),
                    ..Default::default()
                }),
            }]
        );

        // Make sure we can read back the target registrations.
        assert_eq!(
            get_target_registrations(&proxy).await,
            vec![ffx::RepositoryTarget {
                repo_name: Some(REPO_NAME.to_string()),
                target_identifier: Some(TARGET_NODENAME.to_string()),
                aliases: Some(vec!["example.com".to_string(), "fuchsia.com".to_string()]),
                storage_type: Some(ffx::RepositoryStorageType::Ephemeral),
                ..Default::default()
            }],
        );

        // We should not have tried to register any repositories on the device since the server
        // has not been started.
        assert_vec_empty!(fake_repo_manager.take_events());
        assert_vec_empty!(fake_engine.take_events());
        assert_vec_empty!(repo.borrow().take_events(PkgctlCommandType::RuleReplace));

        // Start the server.
        proxy.server_start(None).await.unwrap().unwrap();

        // Make sure we set up the repository and rewrite rules on the device.
        match test_run_mode {
            TestRunMode::Fidl => {
                assert_eq!(
                    fake_repo_manager.take_events(),
                    vec![RepositoryManagerEvent::Add { repo: test_repo_config_fidl(&repo).await }],
                );

                // Expect SSH flow untouched.
                assert_vec_empty!(repo.borrow().take_events(PkgctlCommandType::RepoAdd));
            }
            TestRunMode::Ssh => {
                assert_eq!(
                    repo.borrow().take_events(PkgctlCommandType::RepoAdd),
                    vec![PkgctlCommandEvent {
                        device_addr: SocketAddr::from_str(DEVICE_ADDR).unwrap(),
                        args: test_repo_config_ssh(&repo).await
                    }],
                );

                // Expect FIDL flow untouched.
                assert_vec_empty!(fake_repo_manager.take_events());
            }
        }

        // Check rewrite rules
        match test_run_mode {
            TestRunMode::Fidl => {
                assert_eq!(
                    fake_engine.take_events(),
                    vec![
                        RewriteEngineEvent::ListDynamic,
                        RewriteEngineEvent::IteratorNext,
                        RewriteEngineEvent::ResetAll,
                        RewriteEngineEvent::EditTransactionAdd {
                            rule: rule!("example.com" => REPO_NAME, "/" => "/"),
                        },
                        RewriteEngineEvent::EditTransactionAdd {
                            rule: rule!("fuchsia.com" => REPO_NAME, "/" => "/"),
                        },
                        RewriteEngineEvent::EditTransactionCommit,
                    ],
                );

                // Expect SSH flow untouched.
                assert_vec_empty!(repo.borrow().take_events(PkgctlCommandType::RuleReplace));
            }
            TestRunMode::Ssh => {
                assert_eq!(
                    repo.borrow().take_events(PkgctlCommandType::RuleReplace),
                    vec![PkgctlCommandEvent {
                        device_addr: SocketAddr::from_str(DEVICE_ADDR).unwrap(),
                        args: test_target_alias_ssh(
                            &repo,
                            REPO_NAME,
                            &&ffx::RepositoryTarget {
                                repo_name: Some(REPO_NAME.to_string()),
                                target_identifier: Some(TARGET_NODENAME.to_string()),
                                aliases: Some(vec![
                                    "example.com".to_string(),
                                    "fuchsia.com".to_string()
                                ]),
                                storage_type: Some(ffx::RepositoryStorageType::Ephemeral),
                                ..Default::default()
                            }
                        )
                        .await
                    }],
                );

                // Expect FIDL flow untouched.
                assert_vec_empty!(fake_repo_manager.take_events());
            }
        }
    }

    #[test]
    fn test_load_from_config_with_disabled_server_with_fidl() {
        run_test(TestRunMode::Fidl, async {
            check_load_from_config_with_disabled_server(TestRunMode::Fidl).await;
        });
    }

    #[test]
    fn test_load_from_config_with_disabled_server_with_ssh() {
        run_test(TestRunMode::Ssh, async {
            check_load_from_config_with_disabled_server(TestRunMode::Ssh).await;
        });
    }

    #[test]
    fn test_start_stop_server() {
        run_test(TestRunMode::Fidl, async {
            let repo = Rc::new(RefCell::new(Repo {
                inner: RepoInner::new(),
                event_handler_provider: TestEventHandlerProvider,
                registrar: Arc::new(RealRegistrar {
                    ssh_provider: Arc::new(TestSshProvider::new()),
                }),
            }));
            let (_fake_rcs, fake_rcs_closure) = FakeRcs::new();

            let daemon = FakeDaemonBuilder::new()
                .rcs_handler(fake_rcs_closure)
                .inject_fidl_protocol(Rc::clone(&repo))
                .build();

            let proxy = daemon.open_proxy::<ffx::RepositoryRegistryMarker>().await;

            assert_eq!(
                ServerStatus::try_from(proxy.server_status().await.unwrap()).unwrap(),
                ServerStatus::Stopped,
            );

            let actual_address =
                SocketAddress::from(proxy.server_start(None).await.unwrap().unwrap()).0;
            let expected_address = repo.borrow().inner.read().await.server.listen_addr().unwrap();
            assert_eq!(actual_address, expected_address);

            assert_eq!(
                ServerStatus::try_from(proxy.server_status().await.unwrap()).unwrap(),
                ServerStatus::Running { address: expected_address },
            );

            assert_matches!(proxy.server_stop().await.unwrap(), Ok(()));

            assert_eq!(
                ServerStatus::try_from(proxy.server_status().await.unwrap()).unwrap(),
                ServerStatus::Stopped,
            );
        })
    }

    #[test]
    fn test_start_stop_server_runtime_address() {
        run_test(TestRunMode::Fidl, async {
            let config_addr: SocketAddr = (Ipv4Addr::LOCALHOST, 80).into();
            ffx_config::query("repository.server.listen")
                .level(Some(ConfigLevel::User))
                .set(config_addr.to_string().into())
                .await
                .unwrap();

            let repo = Rc::new(RefCell::new(Repo {
                inner: RepoInner::new(),
                event_handler_provider: TestEventHandlerProvider,
                registrar: Arc::new(RealRegistrar {
                    ssh_provider: Arc::new(TestSshProvider::new()),
                }),
            }));
            let (_fake_rcs, fake_rcs_closure) = FakeRcs::new();

            let daemon = FakeDaemonBuilder::new()
                .rcs_handler(fake_rcs_closure)
                .inject_fidl_protocol(Rc::clone(&repo))
                .build();

            let proxy = daemon.open_proxy::<ffx::RepositoryRegistryMarker>().await;

            let runtime_address = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 0);

            proxy
                .server_start(Some(&SocketAddress(runtime_address).into()))
                .await
                .unwrap()
                .unwrap();
            let actual_address = repo.borrow().inner.read().await.server.listen_addr().unwrap();
            assert_ne!(config_addr, actual_address);

            assert_matches!(proxy.server_stop().await.unwrap(), Ok(()));
        })
    }

    #[test]
    fn test_start_server_starts_a_disabled_server() {
        run_test(TestRunMode::Fidl, async {
            pkg_config::set_repository_server_enabled(false).await.unwrap();

            let repo = Rc::new(RefCell::new(Repo {
                inner: RepoInner::new(),
                event_handler_provider: TestEventHandlerProvider,
                registrar: Arc::new(RealRegistrar {
                    ssh_provider: Arc::new(TestSshProvider::new()),
                }),
            }));
            let (_fake_rcs, fake_rcs_closure) = FakeRcs::new();

            let daemon = FakeDaemonBuilder::new()
                .rcs_handler(fake_rcs_closure)
                .inject_fidl_protocol(Rc::clone(&repo))
                .build();

            let proxy = daemon.open_proxy::<ffx::RepositoryRegistryMarker>().await;

            let actual_address =
                SocketAddress::from(proxy.server_start(None).await.unwrap().unwrap()).0;
            let expected_address = repo.borrow().inner.read().await.server.listen_addr().unwrap();
            assert_eq!(actual_address, expected_address);

            assert!(pkg_config::get_repository_server_enabled().await.unwrap());
        })
    }

    #[test]
    fn test_add_remove() {
        run_test(TestRunMode::Fidl, async {
            let repo = Rc::new(RefCell::new(Repo {
                inner: RepoInner::new(),
                event_handler_provider: TestEventHandlerProvider,
                registrar: Arc::new(RealRegistrar {
                    ssh_provider: Arc::new(TestSshProvider::new()),
                }),
            }));
            let (fake_rcs, fake_rcs_closure) = FakeRcs::new();

            let daemon = FakeDaemonBuilder::new()
                .rcs_handler(fake_rcs_closure)
                .inject_fidl_protocol(Rc::clone(&repo))
                .build();

            let proxy = daemon.open_proxy::<ffx::RepositoryRegistryMarker>().await;
            let spec = ffx::RepositorySpec::Pm(ffx::PmRepositorySpec {
                path: Some(EMPTY_REPO_PATH.to_owned()),
                ..Default::default()
            });

            // Initially no server should be running.
            {
                let inner = Arc::clone(&repo.borrow().inner);
                assert_matches!(inner.read().await.server, ServerState::Stopped);
            }

            proxy
                .add_repository(REPO_NAME, &spec)
                .await
                .expect("communicated with proxy")
                .expect("adding repository to succeed");

            // Make sure the repository was added.
            assert_eq!(
                get_repositories(&proxy).await,
                vec![ffx::RepositoryConfig { name: REPO_NAME.to_string(), spec }]
            );

            // Adding a repository does not start the server.
            {
                let inner = Arc::clone(&repo.borrow().inner);
                assert_matches!(inner.read().await.server, ServerState::Stopped);
            }

            // Adding a repository should not create a tunnel, since we haven't registered the
            // repository on a device.
            assert_vec_empty!(fake_rcs.take_events());

            assert!(proxy.remove_repository(REPO_NAME).await.unwrap());

            // Make sure the repository was removed.
            assert_vec_empty!(get_repositories(&proxy).await);
        })
    }

    async fn check_removing_repo_also_deregisters_from_target(test_run_mode: TestRunMode) {
        let registrar = RealRegistrar { ssh_provider: Arc::new(TestSshProvider::new()) };

        let repo = Rc::new(RefCell::new(Repo {
            inner: RepoInner::new(),
            event_handler_provider: TestEventHandlerProvider,
            registrar: Arc::new(registrar),
        }));
        let (fake_repo_manager, fake_repo_manager_closure) = FakeRepositoryManager::new();
        let (fake_engine, fake_engine_closure) = FakeRewriteEngine::new();
        let (_fake_rcs, fake_rcs_closure) = FakeRcs::new();
        let device_address = ffx::TargetAddrInfo::IpPort(ffx::TargetIpPort {
            ip: IpAddress::Ipv4(Ipv4Address { addr: [127, 0, 0, 1] }),
            scope_id: 0,
            port: DEVICE_PORT,
        });

        let daemon = FakeDaemonBuilder::new()
            .rcs_handler(fake_rcs_closure)
            .register_instanced_protocol_closure::<RepositoryManagerMarker, _>(
                fake_repo_manager_closure,
            )
            .register_instanced_protocol_closure::<RewriteEngineMarker, _>(fake_engine_closure)
            .inject_fidl_protocol(Rc::clone(&repo))
            .target(ffx::TargetInfo {
                nodename: Some(TARGET_NODENAME.to_string()),
                ssh_host_address: Some(ffx::SshHostAddrInfo { address: HOST_ADDR.to_string() }),
                addresses: Some(vec![device_address.clone()]),
                ssh_address: Some(device_address.clone()),
                ..Default::default()
            })
            .build();

        let proxy = daemon.open_proxy::<ffx::RepositoryRegistryMarker>().await;

        // Make sure there is nothing in the registry.
        assert_vec_empty!(fake_engine.take_events());
        assert_vec_empty!(repo.borrow().take_events(PkgctlCommandType::RuleReplace));
        assert_vec_empty!(get_repositories(&proxy).await);
        assert_vec_empty!(get_target_registrations(&proxy).await);

        add_repo(&proxy, REPO_NAME).await;

        // We shouldn't have added repositories or rewrite rules to the fuchsia device yet.

        assert_vec_empty!(fake_repo_manager.take_events());
        assert_vec_empty!(fake_engine.take_events());
        assert_vec_empty!(repo.borrow().take_events(PkgctlCommandType::RuleReplace));

        let target = ffx::RepositoryTarget {
            repo_name: Some(REPO_NAME.to_string()),
            target_identifier: Some(TARGET_NODENAME.to_string()),
            storage_type: Some(ffx::RepositoryStorageType::Ephemeral),
            aliases: Some(vec!["fuchsia.com".to_string(), "example.com".to_string()]),
            ..Default::default()
        };

        register_target(&proxy, target.clone()).await;

        // Registering the target should have set up a repository.
        match test_run_mode {
            TestRunMode::Fidl => {
                assert_eq!(
                    fake_repo_manager.take_events(),
                    vec![RepositoryManagerEvent::Add { repo: test_repo_config_fidl(&repo).await }]
                );

                // Expect SSH flow untouched.
                assert_vec_empty!(repo.borrow().take_events(PkgctlCommandType::RepoAdd));
            }
            TestRunMode::Ssh => {
                assert_eq!(
                    repo.borrow().take_events(PkgctlCommandType::RepoAdd),
                    vec![PkgctlCommandEvent {
                        device_addr: SocketAddr::from_str(DEVICE_ADDR).unwrap(),
                        args: test_repo_config_ssh(&repo).await
                    },]
                );

                // Expect FIDL flow untouched.
                assert_vec_empty!(fake_repo_manager.take_events());
            }
        }

        // Adding the registration should have set up rewrite rules.
        match test_run_mode {
            TestRunMode::Fidl => {
                assert_eq!(
                    fake_engine.take_events(),
                    vec![
                        RewriteEngineEvent::ListDynamic,
                        RewriteEngineEvent::IteratorNext,
                        RewriteEngineEvent::ResetAll,
                        RewriteEngineEvent::EditTransactionAdd {
                            rule: rule!("example.com" => REPO_NAME, "/" => "/"),
                        },
                        RewriteEngineEvent::EditTransactionAdd {
                            rule: rule!("fuchsia.com" => REPO_NAME, "/" => "/"),
                        },
                        RewriteEngineEvent::EditTransactionCommit,
                    ],
                );

                // Expect SSH flow untouched.
                assert_vec_empty!(repo.borrow().take_events(PkgctlCommandType::RuleReplace));
            }
            TestRunMode::Ssh => {
                assert_eq!(
                    repo.borrow().take_events(PkgctlCommandType::RuleReplace),
                    vec![PkgctlCommandEvent {
                        device_addr: SocketAddr::from_str(DEVICE_ADDR).unwrap(),
                        args: test_target_alias_ssh(&repo, REPO_NAME, &target).await
                    },]
                );

                // Expect FIDL flow untouched.
                assert_vec_empty!(fake_engine.take_events());
            }
        }

        // The RepositoryRegistry should remember we set up the registrations.
        assert_eq!(
            get_target_registrations(&proxy).await,
            vec![ffx::RepositoryTarget {
                repo_name: Some(REPO_NAME.to_string()),
                target_identifier: Some(TARGET_NODENAME.to_string()),
                storage_type: Some(ffx::RepositoryStorageType::Ephemeral),
                aliases: Some(vec!["example.com".to_string(), "fuchsia.com".to_string()]),
                ..Default::default()
            },],
        );

        // We should have saved the registration to the config.
        assert_matches!(
            pkg::config::get_registration(REPO_NAME, TARGET_NODENAME).await,
            Ok(Some(reg)) if reg == RepositoryTarget {
                repo_name: REPO_NAME.to_string(),
                target_identifier: Some(TARGET_NODENAME.to_string()),
                aliases: Some(BTreeSet::from(["example.com".to_string(), "fuchsia.com".to_string()])),
                storage_type: Some(RepositoryStorageType::Ephemeral),
            }
        );

        assert!(proxy.remove_repository(REPO_NAME).await.expect("communicated with proxy"));

        // We should not have communicated with the device.
        assert_vec_empty!(fake_engine.take_events());
        assert_vec_empty!(repo.borrow().take_events(PkgctlCommandType::RuleReplace));

        assert_vec_empty!(get_target_registrations(&proxy).await);

        // The registration should have been cleared from the config.
        assert_matches!(pkg::config::get_registration(REPO_NAME, TARGET_NODENAME).await, Ok(None));
    }

    #[test]
    fn test_removing_repo_also_deregisters_from_target_with_ssh() {
        run_test(TestRunMode::Ssh, async {
            check_removing_repo_also_deregisters_from_target(TestRunMode::Ssh).await
        });
    }

    #[test]
    fn test_removing_repo_also_deregisters_from_target_with_fidl() {
        run_test(TestRunMode::Fidl, async {
            check_removing_repo_also_deregisters_from_target(TestRunMode::Fidl).await
        });
    }

    async fn check_add_register_deregister_with_repository_aliases(test_run_mode: TestRunMode) {
        let repo = Rc::new(RefCell::new(Repo {
            inner: RepoInner::new(),
            event_handler_provider: TestEventHandlerProvider,
            registrar: Arc::new(RealRegistrar { ssh_provider: Arc::new(TestSshProvider::new()) }),
        }));
        let (fake_repo_manager, fake_repo_manager_closure) = FakeRepositoryManager::new();
        let (fake_engine, fake_engine_closure) = FakeRewriteEngine::new();
        let (fake_rcs, fake_rcs_closure) = FakeRcs::new();
        let device_address = ffx::TargetAddrInfo::IpPort(ffx::TargetIpPort {
            ip: IpAddress::Ipv4(Ipv4Address { addr: [127, 0, 0, 1] }),
            scope_id: 0,
            port: DEVICE_PORT,
        });

        let daemon = FakeDaemonBuilder::new()
            .rcs_handler(fake_rcs_closure)
            .register_instanced_protocol_closure::<RepositoryManagerMarker, _>(
                fake_repo_manager_closure,
            )
            .register_instanced_protocol_closure::<RewriteEngineMarker, _>(fake_engine_closure)
            .inject_fidl_protocol(Rc::clone(&repo))
            .target(ffx::TargetInfo {
                nodename: Some(TARGET_NODENAME.to_string()),
                ssh_host_address: Some(ffx::SshHostAddrInfo { address: HOST_ADDR.to_string() }),
                addresses: Some(vec![device_address.clone()]),
                ssh_address: Some(device_address.clone()),
                ..Default::default()
            })
            .build();

        let proxy = daemon.open_proxy::<ffx::RepositoryRegistryMarker>().await;

        // Make sure there is nothing in the registry.
        assert_vec_empty!(fake_engine.take_events());
        assert_vec_empty!(repo.borrow().take_events(PkgctlCommandType::RuleReplace));
        assert_vec_empty!(get_repositories(&proxy).await);
        assert_vec_empty!(get_target_registrations(&proxy).await);

        add_repo(&proxy, REPO_NAME).await;

        // We shouldn't have added repositories or rewrite rules to the fuchsia device yet.
        assert_vec_empty!(fake_repo_manager.take_events());
        assert_vec_empty!(fake_engine.take_events());
        assert_vec_empty!(repo.borrow().take_events(PkgctlCommandType::RuleReplace));

        let target = ffx::RepositoryTarget {
            repo_name: Some(REPO_NAME.to_string()),
            target_identifier: Some(TARGET_NODENAME.to_string()),
            storage_type: Some(ffx::RepositoryStorageType::Ephemeral),
            aliases: None,
            ..Default::default()
        };

        register_target(&proxy, target.clone()).await;

        // Registering the target should have set up a repository.
        match test_run_mode {
            TestRunMode::Fidl => {
                assert_eq!(
                    fake_repo_manager.take_events(),
                    vec![RepositoryManagerEvent::Add { repo: test_repo_config_fidl(&repo).await }]
                );

                // Registering a repository should create a tunnel.
                assert_eq!(fake_rcs.take_events(), vec![RcsEvent::ReverseTcp]);

                // Expect SSH flow untouched.
                assert_vec_empty!(repo.borrow().take_events(PkgctlCommandType::RepoAdd));
            }
            TestRunMode::Ssh => {
                assert_eq!(
                    repo.borrow().take_events(PkgctlCommandType::RepoAdd),
                    vec![PkgctlCommandEvent {
                        device_addr: SocketAddr::from_str(DEVICE_ADDR).unwrap(),
                        args: test_repo_config_ssh(&repo).await
                    },]
                );

                // Registering a repository won't create a tunnel.
                assert_vec_empty!(fake_rcs.take_events());

                // Expect FIDL flow untouched.
                assert_vec_empty!(fake_repo_manager.take_events());
            }
        }

        // Adding the registration should have set up rewrite rules from the repository
        // aliases.
        match test_run_mode {
            TestRunMode::Fidl => {
                assert_eq!(
                    fake_engine.take_events(),
                    vec![
                        RewriteEngineEvent::ListDynamic,
                        RewriteEngineEvent::IteratorNext,
                        RewriteEngineEvent::ResetAll,
                        RewriteEngineEvent::EditTransactionAdd {
                            rule: rule!("anothercorp.com" => REPO_NAME, "/" => "/"),
                        },
                        RewriteEngineEvent::EditTransactionAdd {
                            rule: rule!("mycorp.com" => REPO_NAME, "/" => "/"),
                        },
                        RewriteEngineEvent::EditTransactionCommit,
                    ],
                );

                // Expect SSH flow untouched.
                assert_vec_empty!(repo.borrow().take_events(PkgctlCommandType::RuleReplace));
            }
            TestRunMode::Ssh => {
                assert_eq!(
                    repo.borrow().take_events(PkgctlCommandType::RuleReplace),
                    vec![PkgctlCommandEvent {
                        device_addr: SocketAddr::from_str(DEVICE_ADDR).unwrap(),
                        args: test_target_alias_ssh(&repo, REPO_NAME, &target).await
                    },]
                );

                // Expect FIDL flow untouched.
                assert_vec_empty!(fake_engine.take_events());
            }
        }

        // The RepositoryRegistry should remember we set up the registrations.
        assert_eq!(
            get_target_registrations(&proxy).await,
            vec![ffx::RepositoryTarget {
                repo_name: Some(REPO_NAME.to_string()),
                target_identifier: Some(TARGET_NODENAME.to_string()),
                storage_type: Some(ffx::RepositoryStorageType::Ephemeral),
                aliases: None,
                ..Default::default()
            }],
        );

        // We should have saved the registration to the config.
        assert_matches!(
            pkg::config::get_registration(REPO_NAME, TARGET_NODENAME).await,
            Ok(Some(reg)) if reg == RepositoryTarget {
                repo_name: "some-repo".to_string(),
                target_identifier: Some("some-target".to_string()),
                aliases: None,
                storage_type: Some(RepositoryStorageType::Ephemeral),
            }
        );

        proxy
            .deregister_target(REPO_NAME, Some(TARGET_NODENAME))
            .await
            .expect("communicated with proxy")
            .expect("target unregistration to succeed");

        // We should not have communicated with the device.
        assert_vec_empty!(fake_engine.take_events());
        assert_vec_empty!(repo.borrow().take_events(PkgctlCommandType::RuleReplace));

        assert_vec_empty!(get_target_registrations(&proxy).await);

        // The registration should have been cleared from the config.
        assert_matches!(pkg::config::get_registration(REPO_NAME, TARGET_NODENAME).await, Ok(None));
    }

    #[test]
    fn test_add_register_deregister_with_repository_aliases_with_fidl() {
        run_test(TestRunMode::Fidl, async {
            check_add_register_deregister_with_repository_aliases(TestRunMode::Fidl).await
        });
    }

    #[test]
    fn test_add_register_deregister_with_repository_aliases_with_ssh() {
        run_test(TestRunMode::Ssh, async {
            check_add_register_deregister_with_repository_aliases(TestRunMode::Ssh).await
        });
    }

    async fn check_add_register_deregister_with_registration_aliases(test_run_mode: TestRunMode) {
        let repo = Rc::new(RefCell::new(Repo {
            inner: RepoInner::new(),
            event_handler_provider: TestEventHandlerProvider,
            registrar: Arc::new(RealRegistrar { ssh_provider: Arc::new(TestSshProvider::new()) }),
        }));
        let (fake_repo_manager, fake_repo_manager_closure) = FakeRepositoryManager::new();
        let (fake_engine, fake_engine_closure) = FakeRewriteEngine::new();
        let (fake_rcs, fake_rcs_closure) = FakeRcs::new();
        let device_address = ffx::TargetAddrInfo::IpPort(ffx::TargetIpPort {
            ip: IpAddress::Ipv4(Ipv4Address { addr: [127, 0, 0, 1] }),
            scope_id: 0,
            port: DEVICE_PORT,
        });

        let daemon = FakeDaemonBuilder::new()
            .rcs_handler(fake_rcs_closure)
            .register_instanced_protocol_closure::<RepositoryManagerMarker, _>(
                fake_repo_manager_closure,
            )
            .register_instanced_protocol_closure::<RewriteEngineMarker, _>(fake_engine_closure)
            .inject_fidl_protocol(Rc::clone(&repo))
            .target(ffx::TargetInfo {
                nodename: Some(TARGET_NODENAME.to_string()),
                ssh_host_address: Some(ffx::SshHostAddrInfo { address: HOST_ADDR.to_string() }),
                addresses: Some(vec![device_address.clone()]),
                ssh_address: Some(device_address.clone()),
                ..Default::default()
            })
            .build();

        let proxy = daemon.open_proxy::<ffx::RepositoryRegistryMarker>().await;

        // Make sure there is nothing in the registry.
        assert_vec_empty!(fake_engine.take_events());
        assert_vec_empty!(repo.borrow().take_events(PkgctlCommandType::RuleReplace));
        assert_vec_empty!(get_repositories(&proxy).await);
        assert_vec_empty!(get_target_registrations(&proxy).await);

        add_repo(&proxy, REPO_NAME).await;

        // We shouldn't have added repositories or rewrite rules to the fuchsia device yet.
        assert_vec_empty!(fake_repo_manager.take_events());
        assert_vec_empty!(fake_engine.take_events());
        assert_vec_empty!(repo.borrow().take_events(PkgctlCommandType::RuleReplace));

        let target = ffx::RepositoryTarget {
            repo_name: Some(REPO_NAME.to_string()),
            target_identifier: Some(TARGET_NODENAME.to_string()),
            storage_type: Some(ffx::RepositoryStorageType::Ephemeral),
            aliases: Some(vec!["example.com".to_string(), "fuchsia.com".to_string()]),
            ..Default::default()
        };

        register_target(&proxy, target.clone()).await;

        // Registering the target should have set up a repository.
        match test_run_mode {
            TestRunMode::Fidl => {
                assert_eq!(
                    fake_repo_manager.take_events(),
                    vec![RepositoryManagerEvent::Add { repo: test_repo_config_fidl(&repo).await }]
                );

                // Registering a repository should create a tunnel.
                assert_eq!(fake_rcs.take_events(), vec![RcsEvent::ReverseTcp]);

                // Expect SSH flow untouched.
                assert_vec_empty!(repo.borrow().take_events(PkgctlCommandType::RepoAdd));
            }
            TestRunMode::Ssh => {
                assert_eq!(
                    repo.borrow().take_events(PkgctlCommandType::RepoAdd),
                    vec![PkgctlCommandEvent {
                        device_addr: SocketAddr::from_str(DEVICE_ADDR).unwrap(),
                        args: test_repo_config_ssh(&repo).await
                    },]
                );

                // Registering a repository won't create a tunnel.
                assert_vec_empty!(fake_rcs.take_events());

                // Expect FIDL flow untouched.
                assert_vec_empty!(fake_repo_manager.take_events());
            }
        }

        // Adding the registration should have set up rewrite rules from the registration
        // aliases.
        match test_run_mode {
            TestRunMode::Fidl => {
                assert_eq!(
                    fake_engine.take_events(),
                    vec![
                        RewriteEngineEvent::ListDynamic,
                        RewriteEngineEvent::IteratorNext,
                        RewriteEngineEvent::ResetAll,
                        RewriteEngineEvent::EditTransactionAdd {
                            rule: rule!("example.com" => REPO_NAME, "/" => "/"),
                        },
                        RewriteEngineEvent::EditTransactionAdd {
                            rule: rule!("fuchsia.com" => REPO_NAME, "/" => "/"),
                        },
                        RewriteEngineEvent::EditTransactionCommit,
                    ],
                );

                // Expect SSH flow untouched.
                assert_vec_empty!(repo.borrow().take_events(PkgctlCommandType::RuleReplace));
            }
            TestRunMode::Ssh => {
                assert_eq!(
                    repo.borrow().take_events(PkgctlCommandType::RuleReplace),
                    vec![PkgctlCommandEvent {
                        device_addr: SocketAddr::from_str(DEVICE_ADDR).unwrap(),
                        args: test_target_alias_ssh(&repo, REPO_NAME, &target).await
                    },]
                );

                // Expect FIDL flow untouched.
                assert_vec_empty!(fake_engine.take_events());
            }
        }

        // The RepositoryRegistry should remember we set up the registrations.
        assert_eq!(
            get_target_registrations(&proxy).await,
            vec![ffx::RepositoryTarget {
                repo_name: Some(REPO_NAME.to_string()),
                target_identifier: Some(TARGET_NODENAME.to_string()),
                storage_type: Some(ffx::RepositoryStorageType::Ephemeral),
                aliases: Some(vec!["example.com".to_string(), "fuchsia.com".to_string()]),
                ..Default::default()
            }],
        );

        // We should have saved the registration to the config.
        assert_matches!(
            pkg::config::get_registration(REPO_NAME, TARGET_NODENAME).await,
            Ok(Some(reg)) if reg == RepositoryTarget {
                repo_name: "some-repo".to_string(),
                target_identifier: Some("some-target".to_string()),
                aliases: Some(BTreeSet::from(["example.com".to_string(), "fuchsia.com".to_string()])),
                storage_type: Some(RepositoryStorageType::Ephemeral),
            }
        );

        proxy
            .deregister_target(REPO_NAME, Some(TARGET_NODENAME))
            .await
            .expect("communicated with proxy")
            .expect("target unregistration to succeed");

        // We should not have communicated with the device.
        assert_vec_empty!(fake_engine.take_events());
        assert_vec_empty!(repo.borrow().take_events(PkgctlCommandType::RuleReplace));

        assert_vec_empty!(get_target_registrations(&proxy).await);

        // The registration should have been cleared from the config.
        assert_matches!(pkg::config::get_registration(REPO_NAME, TARGET_NODENAME).await, Ok(None));
    }

    #[test]
    fn test_add_register_deregister_with_registration_aliases_with_fidl() {
        run_test(TestRunMode::Fidl, async {
            check_add_register_deregister_with_registration_aliases(TestRunMode::Fidl).await
        });
    }

    #[test]
    fn test_add_register_deregister_with_registration_aliases_with_ssh() {
        run_test(TestRunMode::Ssh, async {
            check_add_register_deregister_with_registration_aliases(TestRunMode::Ssh).await
        });
    }

    #[test]
    fn test_duplicate_registration_aliases_error() {
        run_test(TestRunMode::Fidl, async {
            let conflicting_alias = "fuchsia.com".to_string();

            let repo = Rc::new(RefCell::new(Repo::<TestEventHandlerProvider>::default()));
            let (fake_repo_manager, fake_repo_manager_closure) = FakeRepositoryManager::new();
            let (fake_engine, fake_engine_closure) = FakeRewriteEngine::new();
            let (fake_rcs, fake_rcs_closure) = FakeRcs::new();

            let daemon = FakeDaemonBuilder::new()
                .rcs_handler(fake_rcs_closure)
                .register_instanced_protocol_closure::<RepositoryManagerMarker, _>(
                    fake_repo_manager_closure,
                )
                .register_instanced_protocol_closure::<RewriteEngineMarker, _>(fake_engine_closure)
                .inject_fidl_protocol(Rc::clone(&repo))
                .target(ffx::TargetInfo {
                    nodename: Some(TARGET_NODENAME.to_string()),
                    ssh_host_address: Some(ffx::SshHostAddrInfo { address: HOST_ADDR.to_string() }),
                    ..Default::default()
                })
                .build();

            let proxy = daemon.open_proxy::<ffx::RepositoryRegistryMarker>().await;

            // Make sure there is nothing in the registry.
            assert_eq!(fake_engine.take_events(), vec![]);
            assert_eq!(get_repositories(&proxy).await, vec![]);
            assert_eq!(get_target_registrations(&proxy).await, vec![]);

            add_repo(&proxy, REPO_NAME).await;

            // We shouldn't have added repositories or rewrite rules to the fuchsia device yet.
            assert_eq!(fake_repo_manager.take_events(), vec![]);
            assert_eq!(fake_engine.take_events(), vec![]);

            register_target(
                &proxy,
                ffx::RepositoryTarget {
                    repo_name: Some(REPO_NAME.to_string()),
                    target_identifier: Some(TARGET_NODENAME.to_string()),
                    storage_type: Some(ffx::RepositoryStorageType::Ephemeral),
                    aliases: Some(vec!["example.com".to_string(), conflicting_alias.clone()]),
                    ..Default::default()
                },
            )
            .await;

            // Registering the target should have set up a repository.
            assert_eq!(
                fake_repo_manager.take_events(),
                vec![RepositoryManagerEvent::Add { repo: test_repo_config_fidl(&repo).await }]
            );

            // Adding the registration should have set up rewrite rules from the registration
            // aliases.
            assert_eq!(
                fake_engine.take_events(),
                vec![
                    RewriteEngineEvent::ListDynamic,
                    RewriteEngineEvent::IteratorNext,
                    RewriteEngineEvent::ResetAll,
                    RewriteEngineEvent::EditTransactionAdd {
                        rule: rule!("example.com".to_string() => REPO_NAME, "/" => "/"),
                    },
                    RewriteEngineEvent::EditTransactionAdd {
                        rule: rule!("fuchsia.com".to_string() => REPO_NAME, "/" => "/"),
                    },
                    RewriteEngineEvent::EditTransactionCommit,
                ],
            );

            // Registering a repository should create a tunnel.
            assert_eq!(fake_rcs.take_events(), vec![RcsEvent::ReverseTcp]);

            // The RepositoryRegistry should remember we set up the registrations.
            assert_eq!(
                get_target_registrations(&proxy).await,
                vec![ffx::RepositoryTarget {
                    repo_name: Some(REPO_NAME.to_string()),
                    target_identifier: Some(TARGET_NODENAME.to_string()),
                    storage_type: Some(ffx::RepositoryStorageType::Ephemeral),
                    aliases: Some(vec!["example.com".to_string(), "fuchsia.com".to_string()]),
                    ..Default::default()
                }],
            );

            add_repo(&proxy, "other-repo").await;

            // Introducing conflicting alias...
            assert_eq!(
                proxy
                    .register_target(&ffx::RepositoryTarget {
                        repo_name: Some("other-repo".to_string()),
                        target_identifier: Some(TARGET_NODENAME.to_string()),
                        storage_type: Some(ffx::RepositoryStorageType::Ephemeral),
                        // Conflicting alias will collide with REPO_NAME registration...
                        aliases: Some(vec![conflicting_alias.clone()]),
                        ..Default::default()
                    }, fidl_fuchsia_developer_ffx::RepositoryRegistrationAliasConflictMode::ErrorOut)
                    .await
                    .expect("communicated with proxy")
                    .unwrap_err(),
                ffx::RepositoryError::ConflictingRegistration
            );

            // Make sure we didn't add the repo.
            assert_eq!(fake_repo_manager.take_events(), vec![]);

            // Make sure we didn't communicate with the device.
            assert_eq!(fake_engine.take_events(), vec![]);

            // Make sure only previous repository registration is present.
            assert_eq!(
                get_target_registrations(&proxy).await,
                vec![ffx::RepositoryTarget {
                    repo_name: Some(REPO_NAME.to_string()),
                    target_identifier: Some(TARGET_NODENAME.to_string()),
                    storage_type: Some(ffx::RepositoryStorageType::Ephemeral),
                    aliases: Some(vec!["example.com".to_string(), conflicting_alias.clone()]),
                    ..Default::default()
                }],
            );
        })
    }

    async fn check_add_register_server(
        listen_addr: SocketAddr,
        ssh_host_addr: String,
        expected_repo_host: String,
        test_run_mode: TestRunMode,
    ) {
        ffx_config::query("repository.server.listen")
            .level(Some(ConfigLevel::User))
            .set(format!("{}", listen_addr).into())
            .await
            .unwrap();

        let repo = Rc::new(RefCell::new(Repo {
            inner: RepoInner::new(),
            event_handler_provider: TestEventHandlerProvider,
            registrar: Arc::new(RealRegistrar { ssh_provider: Arc::new(TestSshProvider::new()) }),
        }));
        let (fake_repo_manager, fake_repo_manager_closure) = FakeRepositoryManager::new();
        let (fake_engine, fake_engine_closure) = FakeRewriteEngine::new();
        let (_fake_rcs, fake_rcs_closure) = FakeRcs::new();
        let device_address = ffx::TargetAddrInfo::IpPort(ffx::TargetIpPort {
            ip: IpAddress::Ipv4(Ipv4Address { addr: [127, 0, 0, 1] }),
            scope_id: 0,
            port: DEVICE_PORT,
        });

        let daemon = FakeDaemonBuilder::new()
            .rcs_handler(fake_rcs_closure)
            .register_instanced_protocol_closure::<RepositoryManagerMarker, _>(
                fake_repo_manager_closure,
            )
            .register_instanced_protocol_closure::<RewriteEngineMarker, _>(fake_engine_closure)
            .inject_fidl_protocol(Rc::clone(&repo))
            .target(ffx::TargetInfo {
                nodename: Some(TARGET_NODENAME.to_string()),
                ssh_host_address: Some(ffx::SshHostAddrInfo { address: ssh_host_addr.clone() }),
                addresses: Some(vec![device_address.clone()]),
                ssh_address: Some(device_address.clone()),
                ..Default::default()
            })
            .build();

        let proxy = daemon.open_proxy::<ffx::RepositoryRegistryMarker>().await;

        // Make sure there is nothing in the registry.
        assert_vec_empty!(fake_engine.take_events());
        assert_vec_empty!(repo.borrow().take_events(PkgctlCommandType::RuleReplace));
        assert_vec_empty!(get_repositories(&proxy).await);
        assert_vec_empty!(get_target_registrations(&proxy).await);

        add_repo(&proxy, REPO_NAME).await;

        // We shouldn't have added repositories or rewrite rules to the fuchsia device yet.
        assert_vec_empty!(fake_repo_manager.take_events());
        assert_vec_empty!(fake_engine.take_events());
        assert_vec_empty!(repo.borrow().take_events(PkgctlCommandType::RuleReplace));

        register_target(
            &proxy,
            ffx::RepositoryTarget {
                repo_name: Some(REPO_NAME.to_string()),
                target_identifier: Some(TARGET_NODENAME.to_string()),
                storage_type: Some(ffx::RepositoryStorageType::Ephemeral),
                aliases: None,
                ..Default::default()
            },
        )
        .await;

        // Registering the target should have set up a repository.
        let args = test_repo_config_ssh_with_repo_host(
            &repo,
            Some(expected_repo_host.clone()),
            REPO_NAME.into(),
        )
        .await;
        match test_run_mode {
            TestRunMode::Fidl => {
                let repo_config = test_repo_config_fidl_with_repo_host(
                    &repo,
                    Some(expected_repo_host.clone()),
                    REPO_NAME.into(),
                )
                .await;

                assert_eq!(
                    fake_repo_manager.take_events(),
                    vec![RepositoryManagerEvent::Add { repo: repo_config }]
                );

                // Expect SSH flow untouched.
                assert_vec_empty!(repo.borrow().take_events(PkgctlCommandType::RuleReplace));
            }
            TestRunMode::Ssh => {
                let device_addr = SocketAddr::from_str(DEVICE_ADDR).unwrap();
                assert_eq!(
                    repo.borrow().take_events(PkgctlCommandType::RepoAdd),
                    vec![PkgctlCommandEvent { device_addr, args }]
                );

                // Expect FIDL flow untouched.
                assert_vec_empty!(fake_repo_manager.take_events());
            }
        }
    }

    #[test]
    fn test_add_register_server_loopback_ipv4_with_fidl() {
        run_test(TestRunMode::Fidl, async {
            check_add_register_server(
                (Ipv4Addr::LOCALHOST, 0).into(),
                Ipv4Addr::LOCALHOST.to_string(),
                Ipv4Addr::LOCALHOST.to_string(),
                TestRunMode::Fidl,
            )
            .await
        });
    }

    #[test]
    fn test_add_register_server_loopback_ipv4_with_ssh() {
        run_test(TestRunMode::Ssh, async {
            check_add_register_server(
                (Ipv4Addr::LOCALHOST, 0).into(),
                Ipv4Addr::LOCALHOST.to_string(),
                Ipv4Addr::LOCALHOST.to_string(),
                TestRunMode::Ssh,
            )
            .await
        });
    }

    #[test]
    fn test_add_register_server_loopback_ipv6_with_fidl() {
        run_test(TestRunMode::Fidl, async {
            check_add_register_server(
                (Ipv6Addr::LOCALHOST, 0).into(),
                Ipv6Addr::LOCALHOST.to_string(),
                format!("[{}]", Ipv6Addr::LOCALHOST),
                TestRunMode::Fidl,
            )
            .await
        });
    }

    #[test]
    fn test_add_register_server_loopback_ipv6_with_ssh() {
        run_test(TestRunMode::Ssh, async {
            check_add_register_server(
                (Ipv6Addr::LOCALHOST, 0).into(),
                Ipv6Addr::LOCALHOST.to_string(),
                format!("[{}]", Ipv6Addr::LOCALHOST),
                TestRunMode::Ssh,
            )
            .await
        });
    }

    #[test]
    fn test_add_register_server_non_loopback_ipv4_with_fidl() {
        run_test(TestRunMode::Fidl, async {
            check_add_register_server(
                (Ipv4Addr::UNSPECIFIED, 0).into(),
                Ipv4Addr::UNSPECIFIED.to_string(),
                Ipv4Addr::UNSPECIFIED.to_string(),
                TestRunMode::Fidl,
            )
            .await
        });
    }

    #[test]
    fn test_add_register_server_non_loopback_ipv4_with_ssh() {
        run_test(TestRunMode::Ssh, async {
            check_add_register_server(
                (Ipv4Addr::UNSPECIFIED, 0).into(),
                Ipv4Addr::UNSPECIFIED.to_string(),
                Ipv4Addr::UNSPECIFIED.to_string(),
                TestRunMode::Ssh,
            )
            .await
        });
    }

    #[test]
    fn test_add_register_server_non_loopback_ipv6_with_fidl() {
        run_test(TestRunMode::Fidl, async {
            check_add_register_server(
                (Ipv6Addr::UNSPECIFIED, 0).into(),
                Ipv6Addr::UNSPECIFIED.to_string(),
                format!("[{}]", Ipv6Addr::UNSPECIFIED),
                TestRunMode::Fidl,
            )
            .await
        });
    }

    #[test]
    fn test_add_register_server_non_loopback_ipv6_with_ssh() {
        run_test(TestRunMode::Ssh, async {
            check_add_register_server(
                (Ipv6Addr::UNSPECIFIED, 0).into(),
                Ipv6Addr::UNSPECIFIED.to_string(),
                format!("[{}]", Ipv6Addr::UNSPECIFIED),
                TestRunMode::Ssh,
            )
            .await
        });
    }

    #[test]
    fn test_add_register_server_non_loopback_ipv6_with_scope_with_fidl() {
        run_test(TestRunMode::Fidl, async {
            check_add_register_server(
                (Ipv6Addr::UNSPECIFIED, 0).into(),
                format!("{}%eth1", Ipv6Addr::UNSPECIFIED),
                format!("[{}%25eth1]", Ipv6Addr::UNSPECIFIED),
                TestRunMode::Fidl,
            )
            .await
        });
    }

    #[test]
    fn test_add_register_server_non_loopback_ipv6_with_scope_with_ssh() {
        run_test(TestRunMode::Ssh, async {
            check_add_register_server(
                (Ipv6Addr::UNSPECIFIED, 0).into(),
                format!("{}%eth1", Ipv6Addr::UNSPECIFIED),
                format!("[{}%25eth1]", Ipv6Addr::UNSPECIFIED),
                TestRunMode::Ssh,
            )
            .await
        });
    }

    #[test]
    fn test_register_deduplicates_rules() {
        run_test(TestRunMode::Fidl, async {
            let (_fake_rcs, fake_rcs_closure) = FakeRcs::new();
            let (_fake_repo_manager, fake_repo_manager_closure) = FakeRepositoryManager::new();
            let (fake_engine, fake_engine_closure) = FakeRewriteEngine::with_rules(vec![
                rule!("fuchsia.com" => REPO_NAME, "/" => "/"),
                rule!("fuchsia.com" => "example.com", "/" => "/"),
                rule!("fuchsia.com" => "example.com", "/" => "/"),
                rule!("fuchsia.com" => "mycorp.com", "/" => "/"),
                rule!("example.com" => REPO_NAME, "/" => "/"),
                rule!("fuchsia.com" => REPO_NAME, "/" => "/"),
            ]);

            let daemon = FakeDaemonBuilder::new()
                .rcs_handler(fake_rcs_closure)
                .register_instanced_protocol_closure::<RepositoryManagerMarker, _>(
                    fake_repo_manager_closure,
                )
                .register_instanced_protocol_closure::<RewriteEngineMarker, _>(fake_engine_closure)
                .register_fidl_protocol::<Repo<TestEventHandlerProvider>>()
                .target(ffx::TargetInfo {
                    nodename: Some(TARGET_NODENAME.to_string()),
                    ssh_host_address: Some(ffx::SshHostAddrInfo { address: HOST_ADDR.to_string() }),
                    ..Default::default()
                })
                .build();

            let proxy = daemon.open_proxy::<ffx::RepositoryRegistryMarker>().await;
            add_repo(&proxy, REPO_NAME).await;

            register_target(
                &proxy,
                ffx::RepositoryTarget {
                    repo_name: Some(REPO_NAME.to_string()),
                    target_identifier: Some(TARGET_NODENAME.to_string()),
                    storage_type: Some(ffx::RepositoryStorageType::Ephemeral),
                    aliases: Some(vec!["example.com".to_string(), "fuchsia.com".to_string()]),
                    ..Default::default()
                },
            )
            .await;

            // Adding the registration should have set up rewrite rules.
            assert_eq!(
                fake_engine.take_events(),
                vec![
                    RewriteEngineEvent::ListDynamic,
                    RewriteEngineEvent::IteratorNext,
                    RewriteEngineEvent::IteratorNext,
                    RewriteEngineEvent::IteratorNext,
                    RewriteEngineEvent::IteratorNext,
                    RewriteEngineEvent::IteratorNext,
                    RewriteEngineEvent::IteratorNext,
                    RewriteEngineEvent::IteratorNext,
                    RewriteEngineEvent::ResetAll,
                    RewriteEngineEvent::EditTransactionAdd {
                        rule: rule!("fuchsia.com" => "mycorp.com", "/" => "/"),
                    },
                    RewriteEngineEvent::EditTransactionAdd {
                        rule: rule!("fuchsia.com" => "example.com", "/" => "/"),
                    },
                    RewriteEngineEvent::EditTransactionAdd {
                        rule: rule!("example.com" => REPO_NAME, "/" => "/"),
                    },
                    RewriteEngineEvent::EditTransactionAdd {
                        rule: rule!("fuchsia.com" => REPO_NAME, "/" => "/"),
                    },
                    RewriteEngineEvent::EditTransactionCommit,
                ],
            );
        })
    }

    #[test]
    fn test_remove_default_repository() {
        run_test(TestRunMode::Fidl, async {
            let (_fake_repo_manager, fake_repo_manager_closure) = FakeRepositoryManager::new();

            let daemon = FakeDaemonBuilder::new()
                .register_instanced_protocol_closure::<RepositoryManagerMarker, _>(
                    fake_repo_manager_closure,
                )
                .register_fidl_protocol::<Repo<TestEventHandlerProvider>>()
                .target(ffx::TargetInfo {
                    nodename: Some(TARGET_NODENAME.to_string()),
                    ssh_host_address: Some(ffx::SshHostAddrInfo { address: HOST_ADDR.to_string() }),
                    ..Default::default()
                })
                .build();

            let proxy = daemon.open_proxy::<ffx::RepositoryRegistryMarker>().await;
            add_repo(&proxy, REPO_NAME).await;

            let default_repo_name = "default-repo";
            pkg::config::set_default_repository(default_repo_name).await.unwrap();

            add_repo(&proxy, default_repo_name).await;

            // Remove the non-default repo, which shouldn't change the default repo.
            assert!(proxy.remove_repository(REPO_NAME).await.unwrap());
            assert_eq!(
                pkg::config::get_default_repository().await.unwrap(),
                Some(default_repo_name.to_string())
            );

            // Removing the default repository should also remove the config setting.
            assert!(proxy.remove_repository(default_repo_name).await.unwrap());
            assert_eq!(pkg::config::get_default_repository().await.unwrap(), None);
        })
    }

    async fn check_add_register_default_target(test_run_mode: TestRunMode) {
        let repo = Rc::new(RefCell::new(Repo {
            inner: RepoInner::new(),
            event_handler_provider: TestEventHandlerProvider,
            registrar: Arc::new(RealRegistrar { ssh_provider: Arc::new(TestSshProvider::new()) }),
        }));
        let (_fake_rcs, fake_rcs_closure) = FakeRcs::new();
        let (fake_repo_manager, fake_repo_manager_closure) = FakeRepositoryManager::new();
        let (fake_engine, fake_engine_closure) = FakeRewriteEngine::new();

        let device_address = ffx::TargetAddrInfo::IpPort(ffx::TargetIpPort {
            ip: IpAddress::Ipv4(Ipv4Address { addr: [127, 0, 0, 1] }),
            scope_id: 0,
            port: DEVICE_PORT,
        });

        let daemon = FakeDaemonBuilder::new()
            .rcs_handler(fake_rcs_closure)
            .register_instanced_protocol_closure::<RepositoryManagerMarker, _>(
                fake_repo_manager_closure,
            )
            .register_instanced_protocol_closure::<RewriteEngineMarker, _>(fake_engine_closure)
            .inject_fidl_protocol(Rc::clone(&repo))
            .target(ffx::TargetInfo {
                nodename: Some(TARGET_NODENAME.to_string()),
                ssh_host_address: Some(ffx::SshHostAddrInfo { address: HOST_ADDR.to_string() }),
                addresses: Some(vec![device_address.clone()]),
                ssh_address: Some(device_address.clone()),
                ..Default::default()
            })
            .build();

        let proxy = daemon.open_proxy::<ffx::RepositoryRegistryMarker>().await;
        add_repo(&proxy, REPO_NAME).await;

        let target = ffx::RepositoryTarget {
            repo_name: Some(REPO_NAME.to_string()),
            target_identifier: None,
            storage_type: Some(ffx::RepositoryStorageType::Ephemeral),
            ..Default::default()
        };

        register_target(&proxy, target.clone()).await;

        // Registering the target should have set up a repository.
        match test_run_mode {
            TestRunMode::Fidl => {
                assert_eq!(
                    fake_repo_manager.take_events(),
                    vec![RepositoryManagerEvent::Add { repo: test_repo_config_fidl(&repo).await }]
                );

                // Expect SSH flow untouched.
                assert_vec_empty!(repo.borrow().take_events(PkgctlCommandType::RepoAdd));
            }
            TestRunMode::Ssh => {
                assert_eq!(
                    repo.borrow().take_events(PkgctlCommandType::RepoAdd),
                    vec![PkgctlCommandEvent {
                        device_addr: SocketAddr::from_str(DEVICE_ADDR).unwrap(),
                        args: test_repo_config_ssh(&repo).await
                    },]
                );

                // Expect FIDL flow untouched.
                assert_vec_empty!(fake_repo_manager.take_events());
            }
        }

        // Adding the registration should have set up rewrite rules.
        match test_run_mode {
            TestRunMode::Fidl => {
                assert_eq!(
                    fake_engine.take_events(),
                    vec![
                        RewriteEngineEvent::ListDynamic,
                        RewriteEngineEvent::IteratorNext,
                        RewriteEngineEvent::ResetAll,
                        RewriteEngineEvent::EditTransactionAdd {
                            rule: rule!("anothercorp.com" => REPO_NAME, "/" => "/"),
                        },
                        RewriteEngineEvent::EditTransactionAdd {
                            rule: rule!("mycorp.com" => REPO_NAME, "/" => "/"),
                        },
                        RewriteEngineEvent::EditTransactionCommit,
                    ],
                );

                // Expect SSH flow untouched.
                assert_vec_empty!(repo.borrow().take_events(PkgctlCommandType::RuleReplace));
            }
            TestRunMode::Ssh => {
                assert_eq!(
                    repo.borrow().take_events(PkgctlCommandType::RuleReplace),
                    vec![PkgctlCommandEvent {
                        device_addr: SocketAddr::from_str(DEVICE_ADDR).unwrap(),
                        args: test_target_alias_ssh(&repo, REPO_NAME, &target).await
                    },]
                );

                // Expect FIDL flow untouched.
                assert_vec_empty!(fake_engine.take_events());
            }
        }
    }

    #[test]
    fn test_add_register_default_target_with_fidl() {
        run_test(TestRunMode::Fidl, async {
            check_add_register_default_target(TestRunMode::Fidl).await
        });
    }

    #[test]
    fn test_add_register_default_target_with_ssh() {
        run_test(TestRunMode::Ssh, async {
            check_add_register_default_target(TestRunMode::Ssh).await
        });
    }

    async fn check_add_register_empty_aliases(test_run_mode: TestRunMode) {
        let repo = Rc::new(RefCell::new(Repo {
            inner: RepoInner::new(),
            event_handler_provider: TestEventHandlerProvider,
            registrar: Arc::new(RealRegistrar { ssh_provider: Arc::new(TestSshProvider::new()) }),
        }));
        let (_fake_rcs, fake_rcs_closure) = FakeRcs::new();
        let (fake_repo_manager, fake_repo_manager_closure) = FakeRepositoryManager::new();
        let (fake_engine, fake_engine_closure) = FakeRewriteEngine::new();

        let device_address = ffx::TargetAddrInfo::IpPort(ffx::TargetIpPort {
            ip: IpAddress::Ipv4(Ipv4Address { addr: [127, 0, 0, 1] }),
            scope_id: 0,
            port: DEVICE_PORT,
        });

        let daemon = FakeDaemonBuilder::new()
            .rcs_handler(fake_rcs_closure)
            .register_instanced_protocol_closure::<RepositoryManagerMarker, _>(
                fake_repo_manager_closure,
            )
            .register_instanced_protocol_closure::<RewriteEngineMarker, _>(fake_engine_closure)
            .inject_fidl_protocol(Rc::clone(&repo))
            .target(ffx::TargetInfo {
                nodename: Some(TARGET_NODENAME.to_string()),
                ssh_host_address: Some(ffx::SshHostAddrInfo { address: HOST_ADDR.to_string() }),
                addresses: Some(vec![device_address.clone()]),
                ssh_address: Some(device_address.clone()),
                ..Default::default()
            })
            .build();

        let proxy = daemon.open_proxy::<ffx::RepositoryRegistryMarker>().await;
        add_repo(&proxy, REPO_NAME).await;

        // Make sure there's no repositories or registrations on the device.
        assert_vec_empty!(fake_repo_manager.take_events());
        assert_vec_empty!(fake_engine.take_events());
        assert_vec_empty!(repo.borrow().take_events(PkgctlCommandType::RuleReplace));

        // Make sure the registry doesn't have any registrations.
        assert_vec_empty!(get_target_registrations(&proxy).await);

        register_target(
            &proxy,
            ffx::RepositoryTarget {
                repo_name: Some(REPO_NAME.to_string()),
                target_identifier: Some(TARGET_NODENAME.to_string()),
                storage_type: Some(ffx::RepositoryStorageType::Ephemeral),
                aliases: Some(vec![]),
                ..Default::default()
            },
        )
        .await;

        // We should have added a repository to the device, but not added any rewrite rules.
        match test_run_mode {
            TestRunMode::Fidl => {
                assert_eq!(
                    fake_repo_manager.take_events(),
                    vec![RepositoryManagerEvent::Add { repo: test_repo_config_fidl(&repo).await }]
                );

                // Expected SSH flow untouched.
                assert_vec_empty!(repo.borrow().take_events(PkgctlCommandType::RepoAdd));
            }
            TestRunMode::Ssh => {
                assert_eq!(
                    repo.borrow().take_events(PkgctlCommandType::RepoAdd),
                    vec![PkgctlCommandEvent {
                        device_addr: SocketAddr::from_str(DEVICE_ADDR).unwrap(),
                        args: test_repo_config_ssh(&repo).await
                    }]
                );

                // Expected FIDL flow untouched.
                assert_vec_empty!(fake_repo_manager.take_events());
            }
        }

        // Make sure we didn't communicate with the device.
        assert_vec_empty!(fake_engine.take_events());
        assert_vec_empty!(repo.borrow().take_events(PkgctlCommandType::RuleReplace));

        // Make sure we can query the registration.
        assert_eq!(
            get_target_registrations(&proxy).await,
            vec![ffx::RepositoryTarget {
                repo_name: Some(REPO_NAME.to_string()),
                target_identifier: Some(TARGET_NODENAME.to_string()),
                storage_type: Some(ffx::RepositoryStorageType::Ephemeral),
                aliases: Some(vec![]),
                ..Default::default()
            }],
        );
    }

    #[test]
    fn test_add_register_empty_aliases_with_fidl() {
        run_test(TestRunMode::Fidl, async {
            check_add_register_empty_aliases(TestRunMode::Fidl).await
        });
    }

    #[test]
    fn test_add_register_empty_aliases_with_ssh() {
        run_test(TestRunMode::Ssh, async {
            check_add_register_empty_aliases(TestRunMode::Ssh).await
        });
    }

    async fn check_add_register_none_aliases(test_run_mode: TestRunMode) {
        let repo = Rc::new(RefCell::new(Repo {
            inner: RepoInner::new(),
            event_handler_provider: TestEventHandlerProvider,
            registrar: Arc::new(RealRegistrar { ssh_provider: Arc::new(TestSshProvider::new()) }),
        }));
        let (fake_repo_manager, fake_repo_manager_closure) = FakeRepositoryManager::new();
        let (fake_engine, fake_engine_closure) = FakeRewriteEngine::new();
        let (_fake_rcs, fake_rcs_closure) = FakeRcs::new();
        let device_address = ffx::TargetAddrInfo::IpPort(ffx::TargetIpPort {
            ip: IpAddress::Ipv4(Ipv4Address { addr: [127, 0, 0, 1] }),
            scope_id: 0,
            port: DEVICE_PORT,
        });

        let daemon = FakeDaemonBuilder::new()
            .rcs_handler(fake_rcs_closure)
            .register_instanced_protocol_closure::<RepositoryManagerMarker, _>(
                fake_repo_manager_closure,
            )
            .register_instanced_protocol_closure::<RewriteEngineMarker, _>(fake_engine_closure)
            .inject_fidl_protocol(Rc::clone(&repo))
            .target(ffx::TargetInfo {
                nodename: Some(TARGET_NODENAME.to_string()),
                ssh_host_address: Some(ffx::SshHostAddrInfo { address: HOST_ADDR.to_string() }),
                addresses: Some(vec![device_address.clone()]),
                ssh_address: Some(device_address.clone()),
                ..Default::default()
            })
            .build();

        let proxy = daemon.open_proxy::<ffx::RepositoryRegistryMarker>().await;
        add_repo(&proxy, REPO_NAME).await;

        let target = ffx::RepositoryTarget {
            repo_name: Some(REPO_NAME.to_string()),
            target_identifier: Some(TARGET_NODENAME.to_string()),
            storage_type: Some(ffx::RepositoryStorageType::Ephemeral),
            aliases: None,
            ..Default::default()
        };

        register_target(&proxy, target.clone()).await;

        // Make sure we set up the repository on the device.
        match test_run_mode {
            TestRunMode::Fidl => {
                assert_eq!(
                    fake_repo_manager.take_events(),
                    vec![RepositoryManagerEvent::Add { repo: test_repo_config_fidl(&repo).await }]
                );

                // Expect SSH flow untouched.
                assert_vec_empty!(repo.borrow().take_events(PkgctlCommandType::RepoAdd));
            }
            TestRunMode::Ssh => {
                assert_eq!(
                    repo.borrow().take_events(PkgctlCommandType::RepoAdd),
                    vec![PkgctlCommandEvent {
                        device_addr: SocketAddr::from_str(DEVICE_ADDR).unwrap(),
                        args: test_repo_config_ssh(&repo).await
                    },]
                );

                // Expect FIDL flow untouched.
                assert_vec_empty!(fake_repo_manager.take_events());
            }
        }

        // We should have set up the default rewrite rules.
        match test_run_mode {
            TestRunMode::Fidl => {
                assert_eq!(
                    fake_engine.take_events(),
                    vec![
                        RewriteEngineEvent::ListDynamic,
                        RewriteEngineEvent::IteratorNext,
                        RewriteEngineEvent::ResetAll,
                        RewriteEngineEvent::EditTransactionAdd {
                            rule: rule!("anothercorp.com" => REPO_NAME, "/" => "/"),
                        },
                        RewriteEngineEvent::EditTransactionAdd {
                            rule: rule!("mycorp.com" => REPO_NAME, "/" => "/"),
                        },
                        RewriteEngineEvent::EditTransactionCommit,
                    ],
                );

                // Expect SSH flow untouched.
                assert_vec_empty!(repo.borrow().take_events(PkgctlCommandType::RuleReplace));
            }
            TestRunMode::Ssh => {
                assert_eq!(
                    repo.borrow().take_events(PkgctlCommandType::RuleReplace),
                    vec![PkgctlCommandEvent {
                        device_addr: SocketAddr::from_str(DEVICE_ADDR).unwrap(),
                        args: test_target_alias_ssh(&repo, REPO_NAME, &target).await
                    },]
                );

                // Expect FIDL flow untouched.
                assert_vec_empty!(fake_engine.take_events());
            }
        }

        assert_eq!(
            get_target_registrations(&proxy).await,
            vec![ffx::RepositoryTarget {
                repo_name: Some(REPO_NAME.to_string()),
                target_identifier: Some(TARGET_NODENAME.to_string()),
                storage_type: Some(ffx::RepositoryStorageType::Ephemeral),
                aliases: None,
                ..Default::default()
            }],
        );
    }

    #[test]
    fn test_add_register_none_aliases_with_fidl() {
        run_test(TestRunMode::Fidl, async {
            check_add_register_none_aliases(TestRunMode::Fidl).await
        });
    }

    #[test]
    fn test_add_register_none_aliases_with_ssh() {
        run_test(TestRunMode::Ssh, async {
            check_add_register_none_aliases(TestRunMode::Ssh).await
        });
    }

    async fn check_add_register_repo_manager_error(test_run_mode: TestRunMode) {
        let repo = Rc::new(RefCell::new(Repo {
            inner: RepoInner::new(),
            event_handler_provider: TestEventHandlerProvider,
            registrar: Arc::new(RealRegistrar {
                ssh_provider: Arc::new(ErroringSshProvider::new()),
            }),
        }));
        let (erroring_repo_manager, erroring_repo_manager_closure) =
            ErroringRepositoryManager::new();
        let (fake_engine, fake_engine_closure) = FakeRewriteEngine::new();

        let device_address = ffx::TargetAddrInfo::IpPort(ffx::TargetIpPort {
            ip: IpAddress::Ipv4(Ipv4Address { addr: [127, 0, 0, 1] }),
            scope_id: 0,
            port: DEVICE_PORT,
        });

        let daemon = FakeDaemonBuilder::new()
            .register_instanced_protocol_closure::<RepositoryManagerMarker, _>(
                erroring_repo_manager_closure,
            )
            .register_instanced_protocol_closure::<RewriteEngineMarker, _>(fake_engine_closure)
            .inject_fidl_protocol(Rc::clone(&repo))
            .target(ffx::TargetInfo {
                nodename: Some(TARGET_NODENAME.to_string()),
                ssh_host_address: Some(ffx::SshHostAddrInfo { address: HOST_ADDR.to_string() }),
                addresses: Some(vec![device_address.clone()]),
                ssh_address: Some(device_address.clone()),
                ..Default::default()
            })
            .build();

        let proxy = daemon.open_proxy::<ffx::RepositoryRegistryMarker>().await;

        // We need to start the server before we can register a repository
        // on a target.
        proxy
            .server_start(None)
            .await
            .expect("communicated with proxy")
            .expect("starting the server to succeed");

        add_repo(&proxy, REPO_NAME).await;

        assert_eq!(
            proxy
                .register_target(
                    &ffx::RepositoryTarget {
                        repo_name: Some(REPO_NAME.to_string()),
                        target_identifier: Some(TARGET_NODENAME.to_string()),
                        storage_type: Some(ffx::RepositoryStorageType::Ephemeral),
                        aliases: None,
                        ..Default::default()
                    },
                    fidl_fuchsia_developer_ffx::RepositoryRegistrationAliasConflictMode::Replace
                )
                .await
                .unwrap()
                .unwrap_err(),
            ffx::RepositoryError::RepositoryManagerError
        );

        // Make sure we tried to add the repository.
        match test_run_mode {
            TestRunMode::Fidl => {
                assert_eq!(
                    erroring_repo_manager.take_events(),
                    vec![RepositoryManagerEvent::Add { repo: test_repo_config_fidl(&repo).await }]
                );

                // Expect SSH flow untouched.
                assert_vec_empty!(repo.borrow().take_events(PkgctlCommandType::RepoAdd));
            }
            TestRunMode::Ssh => {
                assert_eq!(
                    repo.borrow().take_events(PkgctlCommandType::RepoAdd),
                    vec![PkgctlCommandEvent {
                        device_addr: SocketAddr::from_str(DEVICE_ADDR).unwrap(),
                        args: test_repo_config_ssh(&repo).await
                    }]
                );

                // Expect FIDL flow untouched.
                assert_vec_empty!(erroring_repo_manager.take_events());
            }
        }

        // Make sure we didn't communicate with the device.
        assert_vec_empty!(fake_engine.take_events());
        assert_vec_empty!(repo.borrow().take_events(PkgctlCommandType::RuleReplace));

        // Make sure the repository registration wasn't added.
        assert_vec_empty!(get_target_registrations(&proxy).await);

        // Make sure nothing was saved to the config.
        assert_matches!(pkg::config::get_registration(REPO_NAME, TARGET_NODENAME).await, Ok(None));
    }

    #[test]
    fn test_add_register_repo_manager_error_with_fidl() {
        run_test(TestRunMode::Fidl, async {
            check_add_register_repo_manager_error(TestRunMode::Fidl).await
        });
    }

    #[test]
    fn test_add_register_repo_manager_error_with_ssh() {
        run_test(TestRunMode::Ssh, async {
            check_add_register_repo_manager_error(TestRunMode::Ssh).await
        });
    }

    async fn check_register_non_existent_repo() {
        let repo = Rc::new(RefCell::new(Repo {
            inner: RepoInner::new(),
            event_handler_provider: TestEventHandlerProvider,
            registrar: Arc::new(RealRegistrar { ssh_provider: Arc::new(TestSshProvider::new()) }),
        }));
        let (erroring_repo_manager, erroring_repo_manager_closure) =
            ErroringRepositoryManager::new();
        let (fake_engine, fake_engine_closure) = FakeRewriteEngine::new();

        let device_address = ffx::TargetAddrInfo::IpPort(ffx::TargetIpPort {
            ip: IpAddress::Ipv4(Ipv4Address { addr: [127, 0, 0, 1] }),
            scope_id: 0,
            port: DEVICE_PORT,
        });

        let daemon = FakeDaemonBuilder::new()
            .register_instanced_protocol_closure::<RepositoryManagerMarker, _>(
                erroring_repo_manager_closure,
            )
            .register_instanced_protocol_closure::<RewriteEngineMarker, _>(fake_engine_closure)
            .inject_fidl_protocol(Rc::clone(&repo))
            .target(ffx::TargetInfo {
                nodename: Some(TARGET_NODENAME.to_string()),
                ssh_host_address: Some(ffx::SshHostAddrInfo { address: HOST_ADDR.to_string() }),
                addresses: Some(vec![device_address.clone()]),
                ssh_address: Some(device_address.clone()),
                ..Default::default()
            })
            .build();

        let proxy = daemon.open_proxy::<ffx::RepositoryRegistryMarker>().await;
        assert_eq!(
            proxy
                .register_target(
                    &ffx::RepositoryTarget {
                        repo_name: Some(REPO_NAME.to_string()),
                        target_identifier: Some(TARGET_NODENAME.to_string()),
                        storage_type: Some(ffx::RepositoryStorageType::Ephemeral),
                        aliases: None,
                        ..Default::default()
                    },
                    fidl_fuchsia_developer_ffx::RepositoryRegistrationAliasConflictMode::Replace
                )
                .await
                .unwrap()
                .unwrap_err(),
            ffx::RepositoryError::NoMatchingRepository
        );

        // Make sure we didn't communicate with the device.
        assert_vec_empty!(erroring_repo_manager.take_events());
        assert_vec_empty!(repo.borrow().take_events(PkgctlCommandType::RepoAdd));
        assert_vec_empty!(fake_engine.take_events());
        assert_vec_empty!(repo.borrow().take_events(PkgctlCommandType::RuleReplace));
    }

    #[test]
    fn test_register_non_existent_repo_with_fidl() {
        run_test(TestRunMode::Fidl, async { check_register_non_existent_repo().await });
    }

    #[test]
    fn test_register_non_existent_repo_with_ssh() {
        run_test(TestRunMode::Ssh, async { check_register_non_existent_repo().await });
    }

    #[test]
    fn test_deregister_non_existent_repo() {
        run_test(TestRunMode::Fidl, async {
            let repo = Rc::new(RefCell::new(Repo {
                inner: RepoInner::new(),
                event_handler_provider: TestEventHandlerProvider,
                registrar: Arc::new(RealRegistrar {
                    ssh_provider: Arc::new(TestSshProvider::new()),
                }),
            }));
            let (erroring_repo_manager, erroring_repo_manager_closure) =
                ErroringRepositoryManager::new();
            let (fake_engine, fake_engine_closure) = FakeRewriteEngine::new();

            let daemon = FakeDaemonBuilder::new()
                .register_instanced_protocol_closure::<RepositoryManagerMarker, _>(
                    erroring_repo_manager_closure,
                )
                .register_instanced_protocol_closure::<RewriteEngineMarker, _>(fake_engine_closure)
                .inject_fidl_protocol(Rc::clone(&repo))
                .target(ffx::TargetInfo {
                    nodename: Some(TARGET_NODENAME.to_string()),
                    ssh_host_address: Some(ffx::SshHostAddrInfo { address: HOST_ADDR.to_string() }),
                    ..Default::default()
                })
                .build();

            let proxy = daemon.open_proxy::<ffx::RepositoryRegistryMarker>().await;
            assert_eq!(
                proxy
                    .deregister_target(REPO_NAME, Some(TARGET_NODENAME))
                    .await
                    .unwrap()
                    .unwrap_err(),
                ffx::RepositoryError::NoMatchingRegistration
            );

            // Make sure we didn't communicate with the device.
            assert_vec_empty!(erroring_repo_manager.take_events());
            assert_vec_empty!(fake_engine.take_events());
            assert_vec_empty!(repo.borrow().take_events(PkgctlCommandType::RuleReplace));
        });
    }

    #[test]
    fn test_build_matcher_nodename() {
        assert_eq!(
            DaemonEventHandler::<RealRegistrar>::build_matcher(TargetInfo {
                nodename: Some(TARGET_NODENAME.to_string()),
                ..TargetInfo::default()
            }),
            Some(TARGET_NODENAME.to_string())
        );

        assert_eq!(
            DaemonEventHandler::<RealRegistrar>::build_matcher(TargetInfo {
                nodename: Some(TARGET_NODENAME.to_string()),
                addresses: vec![TargetAddr::from_str("[fe80::1%1000]:0").unwrap()],
                ..TargetInfo::default()
            }),
            Some(TARGET_NODENAME.to_string())
        )
    }

    #[test]
    fn test_build_matcher_missing_nodename_no_port() {
        assert_eq!(
            DaemonEventHandler::<RealRegistrar>::build_matcher(TargetInfo {
                addresses: vec![TargetAddr::from_str("[fe80::1%1000]:0").unwrap()],
                ..TargetInfo::default()
            }),
            Some("fe80::1%1000".to_string())
        )
    }

    #[test]
    fn test_build_matcher_missing_nodename_with_port() {
        assert_eq!(
            DaemonEventHandler::<RealRegistrar>::build_matcher(TargetInfo {
                addresses: vec![TargetAddr::from_str("[fe80::1%1000]:0").unwrap()],
                ssh_port: Some(9182),
                ..TargetInfo::default()
            }),
            Some("[fe80::1%1000]:9182".to_string())
        )
    }

    #[test]
    fn test_create_repo_port_loopback() {
        for (listen_addr, expected) in [
            ((Ipv4Addr::LOCALHOST, 1234).into(), "127.0.0.1:1234"),
            ((Ipv6Addr::LOCALHOST, 1234).into(), "[::1]:1234"),
        ] {
            // The host address should be ignored, but lets confirm it.
            for host_addr in
                ["1.2.3.4", "fe80::111:2222:3333:444:1234", "fe80::111:2222:3333:444:1234%ethxc2"]
            {
                assert_eq!(
                    create_repo_host(
                        listen_addr,
                        ffx::SshHostAddrInfo { address: host_addr.into() },
                    ),
                    (true, expected.to_string()),
                );
            }
        }
    }

    #[test]
    fn test_create_repo_port_non_loopback() {
        for listen_addr in
            [(Ipv4Addr::UNSPECIFIED, 1234).into(), (Ipv6Addr::UNSPECIFIED, 1234).into()]
        {
            for (host_addr, expected) in [
                ("1.2.3.4", "1.2.3.4:1234"),
                ("fe80::111:2222:3333:444", "[fe80::111:2222:3333:444]:1234"),
                ("fe80::111:2222:3333:444%ethxc2", "[fe80::111:2222:3333:444%25ethxc2]:1234"),
            ] {
                assert_eq!(
                    create_repo_host(
                        listen_addr,
                        ffx::SshHostAddrInfo { address: host_addr.into() },
                    ),
                    (false, expected.to_string()),
                );
            }
        }
    }

    #[test]
    fn test_pm_repo_spec_to_backend() {
        run_test(TestRunMode::Fidl, async {
            let repo = RepoInner::new();
            let spec = pm_repo_spec();
            let backend = repo_spec_to_backend(&spec, &repo).await.unwrap();
            assert_eq!(spec, backend.spec());
        })
    }

    #[test]
    fn test_filesystem_repo_spec_to_backend() {
        run_test(TestRunMode::Fidl, async {
            let repo = RepoInner::new();
            let spec = filesystem_repo_spec();
            let backend = repo_spec_to_backend(&spec, &repo).await.unwrap();
            assert_eq!(spec, backend.spec());
        })
    }

    #[test]
    fn test_http_repo_spec_to_backend() {
        run_test(TestRunMode::Fidl, async {
            // Serve the empty repository
            let repo_path = fs::canonicalize(EMPTY_REPO_PATH).unwrap();
            let pm_backend = PmRepository::new(repo_path.try_into().unwrap());

            let pm_repo =
                RepoClient::from_trusted_remote(Box::new(pm_backend) as Box<_>).await.unwrap();
            let manager = RepositoryManager::new();
            manager.add("tuf", pm_repo);

            let addr = (Ipv4Addr::LOCALHOST, 0).into();
            let (server_fut, _, server) =
                RepositoryServer::builder(addr, Arc::clone(&manager)).start().await.unwrap();

            // Run the server in the background.
            let _task = fasync::Task::local(server_fut);

            let http_spec = RepositorySpec::Http {
                metadata_repo_url: server.local_url() + "/tuf/",
                blob_repo_url: server.local_url() + "/tuf/blobs/",
                aliases: BTreeSet::new(),
            };

            let repo = RepoInner::new();
            let http_backend = repo_spec_to_backend(&http_spec, &repo).await.unwrap();
            assert_eq!(http_spec, http_backend.spec());

            // It rejects invalid urls.
            assert_matches!(
                repo_spec_to_backend(
                    &RepositorySpec::Http {
                        metadata_repo_url: "hello there".to_string(),
                        blob_repo_url: server.local_url() + "/tuf/blobs",
                        aliases: BTreeSet::new(),
                    },
                    &repo
                )
                .await,
                Err(ffx::RepositoryError::InvalidUrl)
            );

            assert_matches!(
                repo_spec_to_backend(
                    &RepositorySpec::Http {
                        metadata_repo_url: server.local_url() + "/tuf",
                        blob_repo_url: "hello there".to_string(),
                        aliases: BTreeSet::new(),
                    },
                    &repo
                )
                .await,
                Err(ffx::RepositoryError::InvalidUrl)
            );
        })
    }
}
