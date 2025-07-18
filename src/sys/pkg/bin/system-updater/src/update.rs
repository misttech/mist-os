// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{anyhow, Context as _, Error};
use async_trait::async_trait;
use fetch_url::fetch_url;
use fidl::endpoints::ProtocolMarker as _;
use fuchsia_async::TimeoutExt as _;
use fuchsia_hash::Hash;
use fuchsia_sync::Mutex;
use fuchsia_url::{AbsoluteComponentUrl, AbsolutePackageUrl, PinnedAbsolutePackageUrl};
use futures::channel::oneshot;
use futures::future::FutureExt as _;
use futures::stream::{FusedStream, StreamExt as _, TryStreamExt as _};
use futures::Future;
use include_str_from_working_dir::include_str_from_working_dir_env;
use log::{error, info, warn};
use std::collections::HashSet;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;
use update_package::manifest::OtaManifestV1;
use {
    fidl_fuchsia_mem as fmem, fidl_fuchsia_paver as fpaver, fidl_fuchsia_pkg as fpkg,
    fidl_fuchsia_pkg_ext as fpkg_ext, fidl_fuchsia_space as fspace,
    fidl_fuchsia_update_installer_ext as fupdate_installer_ext,
};

mod config;
mod environment;
mod genutil;
mod history;
mod metrics;
mod paver;
mod reboot;
mod resolver;
mod state;

pub(super) use config::Config;
pub(super) use environment::{
    BuildInfo, CobaltConnector, Environment, EnvironmentConnector, NamespaceEnvironmentConnector,
    SystemInfo,
};
pub(super) use genutil::GeneratorExt;
pub(super) use history::UpdateHistory;
pub(super) use reboot::{ControlRequest, RebootController};
pub(super) use resolver::ResolveError;

#[cfg(test)]
pub(super) use {
    config::ConfigBuilder,
    environment::{NamespaceBuildInfo, NamespaceCobaltConnector, NamespaceSystemInfo},
};

const COBALT_FLUSH_TIMEOUT: Duration = Duration::from_secs(30);
const SOURCE_EPOCH_RAW: &str = include_str_from_working_dir_env!("EPOCH_PATH");

/// Error encountered in the Prepare state.
#[derive(Debug, thiserror::Error)]
enum PrepareError {
    #[error("while determining source epoch: '{0:?}'")]
    ParseSourceEpochError(String, #[source] serde_json::Error),

    #[error("while determining target epoch")]
    ParseTargetEpochError(#[source] update_package::ParseEpochError),

    #[error("while determining packages to fetch")]
    ParsePackages(#[source] update_package::ParsePackageError),

    #[error("while determining which images to fetch")]
    ParseImages(#[source] update_package::ImagePackagesError),

    #[error("while determining update mode")]
    ParseUpdateMode(#[source] update_package::ParseUpdateModeError),

    #[error("while preparing partitions for update")]
    PreparePartitionMetdata(#[source] paver::PreparePartitionMetadataError),

    #[error("while resolving the update package")]
    ResolveUpdate(#[source] ResolveError),

    #[error(
        "downgrades from epoch {src} to {target} are not allowed. For more context, see RFC-0071: https://fuchsia.dev/fuchsia-src/contribute/governance/rfcs/0071_ota_backstop."
    )]
    UnsupportedDowngrade { src: u64, target: u64 },

    #[error("while verifying board name")]
    VerifyBoard(#[source] anyhow::Error),

    #[error("while verifying images to write")]
    VerifyImages(#[source] update_package::VerifyError),

    #[error("while verifying update package name")]
    VerifyName(#[source] update_package::VerifyNameError),

    #[error("force-recovery mode is incompatible with skip-recovery option")]
    VerifyUpdateMode,

    #[error("while parsing update package url")]
    ParseUpdatePackageUrl(#[source] fuchsia_url::ParseError),

    #[error("while fetching update url")]
    FetchUrl(#[source] fetch_url::errors::FetchUrlError),

    #[error("while parsing OTA manifest")]
    ParseManifest(#[source] update_package::manifest::OtaManifestError),

    #[error("while opening blobfs")]
    OpenBlobfs(#[source] blobfs::BlobfsError),
}

impl PrepareError {
    fn reason(&self) -> fupdate_installer_ext::PrepareFailureReason {
        match self {
            Self::ResolveUpdate(ResolveError::Error(fpkg_ext::ResolveError::NoSpace, _)) => {
                fupdate_installer_ext::PrepareFailureReason::OutOfSpace
            }
            Self::UnsupportedDowngrade { .. } => {
                fupdate_installer_ext::PrepareFailureReason::UnsupportedDowngrade
            }
            _ => fupdate_installer_ext::PrepareFailureReason::Internal,
        }
    }
}

/// Error encountered in the Stage state.
#[derive(Debug, thiserror::Error)]
enum StageError {
    #[error("while attempting to open the image")]
    OpenImageError(#[source] update_package::OpenImageError),

    #[error("while persisting target boot slot")]
    PaverFlush(#[source] anyhow::Error),

    #[error("while resolving an image package")]
    Resolve(#[source] ResolveError),

    #[error("while communicating over fidl")]
    Fidl(#[source] fidl::Error),

    #[error("while fetching a blob")]
    FetchBlob(#[source] fpkg_ext::ResolveError),

    #[error("while getting a blob vmo")]
    GetBlobVmo(#[source] blobfs::GetBlobVmoError),

    #[error("while writing images")]
    Write(#[source] anyhow::Error),
}

impl StageError {
    fn reason(&self) -> fupdate_installer_ext::StageFailureReason {
        match self {
            Self::Resolve(ResolveError::Error(fpkg_ext::ResolveError::NoSpace, _))
            | Self::FetchBlob(fpkg_ext::ResolveError::NoSpace) => {
                fupdate_installer_ext::StageFailureReason::OutOfSpace
            }
            _ => fupdate_installer_ext::StageFailureReason::Internal,
        }
    }
}

/// Error encountered in the Fetch state.
#[derive(Debug, thiserror::Error)]
enum FetchError {
    #[error("while resolving a package")]
    Resolve(#[source] ResolveError),

    #[error("while syncing pkg-cache")]
    Sync(#[source] anyhow::Error),
}

impl FetchError {
    fn reason(&self) -> fupdate_installer_ext::FetchFailureReason {
        match self {
            Self::Resolve(ResolveError::Error(fpkg_ext::ResolveError::NoSpace, _)) => {
                fupdate_installer_ext::FetchFailureReason::OutOfSpace
            }
            _ => fupdate_installer_ext::FetchFailureReason::Internal,
        }
    }
}

/// Error encountered during an update attempt.
#[derive(Debug, thiserror::Error)]
enum AttemptError {
    #[error("during prepare state")]
    Prepare(#[from] PrepareError),

    #[error("during stage state")]
    Stage(#[from] StageError),

    #[error("during fetch state")]
    Fetch(#[from] FetchError),

    #[error("during commit state")]
    Commit(#[source] anyhow::Error),

    #[error("update was canceled")]
    UpdateCanceled,

    #[error("cancel sender dropped")]
    CancelSenderDropped(oneshot::Canceled),
}

#[derive(Debug, PartialEq, Eq)]
pub enum CommitAction {
    /// A reboot is required to apply the update, which should be performed by the system updater.
    Reboot,

    /// A reboot is required to apply the update, but the initiator of the update requested to
    /// perform the reboot themselves.
    RebootDeferred,
}

/// A trait to update the system in the given `Environment` using the provided config options.
#[async_trait(?Send)]
pub trait Updater {
    type UpdateStream: FusedStream<Item = fupdate_installer_ext::State>;

    async fn update(
        &mut self,
        config: Config,
        env: Environment,
        reboot_controller: RebootController,
        cancel_receiver: oneshot::Receiver<()>,
    ) -> (String, Self::UpdateStream);
}

pub struct RealUpdater {
    history: Arc<Mutex<UpdateHistory>>,
    structured_config: system_updater_config::Config,
}

impl RealUpdater {
    pub fn new(
        history: Arc<Mutex<UpdateHistory>>,
        structured_config: system_updater_config::Config,
    ) -> Self {
        Self { history, structured_config }
    }
}

#[async_trait(?Send)]
impl Updater for RealUpdater {
    type UpdateStream = Pin<Box<dyn FusedStream<Item = fupdate_installer_ext::State>>>;

    async fn update(
        &mut self,
        config: Config,
        env: Environment,
        reboot_controller: RebootController,
        cancel_receiver: oneshot::Receiver<()>,
    ) -> (String, Self::UpdateStream) {
        let (attempt_id, attempt) = update(
            config,
            env,
            Arc::clone(&self.history),
            reboot_controller,
            self.structured_config.concurrent_package_resolves.into(),
            self.structured_config.concurrent_blob_fetches.into(),
            self.structured_config.enable_attempt_v2,
            cancel_receiver,
        )
        .await;
        (attempt_id, Box::pin(attempt))
    }
}

/// Updates the system in the given `Environment` using the provided config options.
///
/// Reboot vs RebootDeferred behavior is determined in the following priority order:
/// * is mode ForceRecovery? If so, reboot.
/// * is there a reboot controller? If so, yield reboot to the controller.
/// * if none of the above are true, reboot depending on the value of `Config::should_reboot`.
///
/// If a reboot is deferred, the initiator of the update is responsible for triggering
/// the reboot.
async fn update(
    config: Config,
    env: Environment,
    history: Arc<Mutex<UpdateHistory>>,
    reboot_controller: RebootController,
    concurrent_package_resolves: usize,
    concurrent_blob_fetches: usize,
    enable_attempt_v2: bool,
    mut cancel_receiver: oneshot::Receiver<()>,
) -> (String, impl FusedStream<Item = fupdate_installer_ext::State>) {
    let attempt_fut = history.lock().start_update_attempt(
        fupdate_installer_ext::Options {
            initiator: config.initiator.into(),
            allow_attach_to_existing_attempt: config.allow_attach_to_existing_attempt,
            should_write_recovery: config.should_write_recovery,
        },
        &config.update_url,
        config.start_time,
        &env.data_sink,
        &env.boot_manager,
        &env.build_info,
        &env.system_info,
    );
    let attempt = attempt_fut.await;
    let source_version = attempt.source_version().clone();
    let power_state_control = env.power_state_control.clone();

    let history_clone = Arc::clone(&history);
    let attempt_id = attempt.attempt_id().to_string();
    let stream = async_generator::generate(move |mut co| async move {
        let history = history_clone;
        // The only operation allowed to fail in this function is update_attempt. The rest of the
        // functionality here sets up the update attempt and takes the appropriate actions based on
        // whether the update attempt succeeds or fails.

        let mut phase = metrics::Phase::Tufupdate;
        let (mut cobalt, cobalt_forwarder_task) = env.cobalt_connector.connect();
        let cobalt_forwarder_task = fuchsia_async::Task::spawn(cobalt_forwarder_task);

        info!(config:?; "starting system update");
        cobalt.log_ota_start(config.initiator, config.start_time);

        let mut target_version = history::Version::default();

        let attempt_res = {
            let attempt_fut = match config.update_url.scheme() {
                "http" | "https" if enable_attempt_v2 => {
                    AttemptV2 { config: &config, env: &env, concurrent_blob_fetches }
                        .run(&mut co, &mut phase, &mut target_version)
                        .left_future()
                        .fuse()
                }
                _ => Attempt { config: &config, env: &env, concurrent_package_resolves }
                    .run(&mut co, &mut phase, &mut target_version)
                    .right_future()
                    .fuse(),
            };

            futures::pin_mut!(attempt_fut);

            let attempt_res = futures::select! {
                attempt_res = attempt_fut => attempt_res,
                cancel_res = cancel_receiver => {
                    match cancel_res {
                        Ok(()) => Err(AttemptError::UpdateCanceled),
                        Err(e) => Err(AttemptError::CancelSenderDropped(e)),
                    }
                }
            };
            // at this point the attempt has finished running, drop the receiver to indicate that
            // the update can no longer be canceled.
            drop(cancel_receiver);
            attempt_res
        };

        if let Err(AttemptError::UpdateCanceled) = attempt_res.as_ref() {
            co.yield_(fupdate_installer_ext::State::Canceled).await;
        }

        info!("system update attempt completed, logging metrics");
        let status_code = metrics::result_to_status_code(attempt_res.as_ref().map(|_| ()));
        cobalt.log_ota_result_attempt(
            config.initiator,
            history.lock().attempts_for(&source_version, &target_version) + 1,
            phase,
            status_code,
        );
        cobalt.log_ota_result_duration(
            config.initiator,
            phase,
            status_code,
            config.start_time_mono.elapsed(),
        );
        drop(cobalt);

        // wait for all cobalt events to be flushed to the service.
        info!("flushing cobalt events");
        let () = flush_cobalt(cobalt_forwarder_task, COBALT_FLUSH_TIMEOUT).await;

        let (state, mode) = match attempt_res {
            Ok(ok) => ok,
            Err(e) => {
                error!("system update failed: {:#}", anyhow!(e));
                return target_version;
            }
        };

        info!(mode:?; "checking if reboot is required or should be deferred");
        // Figure out if we should reboot.
        match mode {
            // First priority: Always reboot on ForceRecovery success, even if the caller
            // asked to defer the reboot.
            update_package::UpdateMode::ForceRecovery => {
                info!("system update in ForceRecovery mode complete, rebooting...");
            }
            // Second priority: Use the attached reboot controller.
            update_package::UpdateMode::Normal => {
                info!("system update complete, waiting for initiator to signal reboot.");
                match reboot_controller.wait_to_reboot().await {
                    CommitAction::Reboot => {
                        info!("initiator ready to reboot, rebooting...");
                    }
                    CommitAction::RebootDeferred => {
                        info!("initiator deferred reboot to caller.");
                        state.enter_defer_reboot(&mut co).await;
                        return target_version;
                    }
                }
            }
        }

        state.enter_reboot(&mut co).await;
        target_version
    })
    .when_done(
        move |last_state: Option<fupdate_installer_ext::State>, target_version| async move {
            let last_state = last_state.unwrap_or(fupdate_installer_ext::State::Prepare);

            let should_reboot = matches!(last_state, fupdate_installer_ext::State::Reboot { .. });

            let attempt = attempt.finish(target_version, last_state);
            history.lock().record_update_attempt(attempt);
            let save_fut = history.lock().save();
            save_fut.await;

            if should_reboot {
                reboot::reboot(&power_state_control).await;
            }
        },
    );
    (attempt_id, stream)
}

async fn flush_cobalt(cobalt_forwarder_task: impl Future<Output = ()>, flush_timeout: Duration) {
    cobalt_forwarder_task.on_timeout(flush_timeout, || {
        error!(
            "Couldn't flush cobalt events within the timeout. Proceeding, but may have dropped metrics."
        );
    })
    .await;
}

/// Struct representing images that need to be written during the update.
/// Determined by parsing `images.json`.
struct ImagesToWrite {
    fuchsia: BootSlot,
    recovery: BootSlot,
    /// Unordered vector of (firmware_type, url).
    firmware: Vec<(String, AbsoluteComponentUrl)>,
}

impl ImagesToWrite {
    /// Default fields indicate that no images need to be written.
    fn new() -> Self {
        ImagesToWrite { fuchsia: BootSlot::new(), recovery: BootSlot::new(), firmware: vec![] }
    }

    fn is_empty(&self) -> bool {
        self.fuchsia.is_empty() && self.recovery.is_empty() && self.firmware.is_empty()
    }

    fn get_url_hashes(&self) -> HashSet<fuchsia_hash::Hash> {
        let mut hashes = HashSet::new();
        hashes.extend(&mut self.fuchsia.get_url_hashes().iter());
        hashes.extend(&mut self.recovery.get_url_hashes().iter());

        for firmware_hash in self.firmware.iter().filter_map(|(_, url)| url.package_url().hash()) {
            hashes.insert(firmware_hash);
        }

        hashes
    }

    fn get_urls(&self) -> HashSet<AbsolutePackageUrl> {
        let mut package_urls = HashSet::new();
        for (_, absolute_component_url) in &self.firmware {
            package_urls.insert(absolute_component_url.package_url().to_owned());
        }

        package_urls.extend(self.fuchsia.get_urls());
        package_urls.extend(self.recovery.get_urls());

        package_urls
    }

    fn print_list(&self) -> Vec<String> {
        if self.is_empty() {
            return vec!["ImagesToWrite is empty".to_string()];
        }

        let mut image_list = vec![];
        for (filename, _) in &self.firmware {
            image_list.push(filename.to_string());
        }

        for fuchsia_file in &self.fuchsia.print_names() {
            image_list.push(format!("Fuchsia{fuchsia_file}"));
        }
        for recovery_file in &self.recovery.print_names() {
            image_list.push(format!("Recovery{recovery_file}"));
        }

        image_list
    }

    async fn write(
        &self,
        pkg_resolver: &fpkg::PackageResolverProxy,
        target_config: paver::TargetConfiguration,
        data_sink: &fpaver::DataSinkProxy,
        concurrent_package_resolves: usize,
    ) -> Result<(), StageError> {
        let package_urls = self.get_urls();

        let url_directory_map = resolver::resolve_image_packages(
            pkg_resolver,
            package_urls.into_iter(),
            concurrent_package_resolves,
        )
        .await
        .map_err(StageError::Resolve)?;

        for (type_, absolute_component_url) in &self.firmware {
            let () = write_image_from_package(
                &url_directory_map[absolute_component_url.package_url()],
                absolute_component_url.resource(),
                data_sink,
                target_config,
                ImageType::Firmware { type_ },
            )
            .await?;
        }

        if let Some(zbi) = &self.fuchsia.zbi {
            let () = write_image_from_package(
                &url_directory_map[zbi.package_url()],
                zbi.resource(),
                data_sink,
                target_config,
                ImageType::Asset(fpaver::Asset::Kernel),
            )
            .await?;
        }

        if let Some(vbmeta) = &self.fuchsia.vbmeta {
            let () = write_image_from_package(
                &url_directory_map[vbmeta.package_url()],
                vbmeta.resource(),
                data_sink,
                target_config,
                ImageType::Asset(fpaver::Asset::VerifiedBootMetadata),
            )
            .await?;
        }

        if let Some(zbi) = &self.recovery.zbi {
            let () = write_image_from_package(
                &url_directory_map[zbi.package_url()],
                zbi.resource(),
                data_sink,
                paver::TargetConfiguration::Single(fpaver::Configuration::Recovery),
                ImageType::Asset(fpaver::Asset::Kernel),
            )
            .await?;
        }

        if let Some(vbmeta) = &self.recovery.vbmeta {
            let () = write_image_from_package(
                &url_directory_map[vbmeta.package_url()],
                vbmeta.resource(),
                data_sink,
                paver::TargetConfiguration::Single(fpaver::Configuration::Recovery),
                ImageType::Asset(fpaver::Asset::VerifiedBootMetadata),
            )
            .await?;
        }

        Ok(())
    }
}

struct BootSlot {
    zbi: Option<AbsoluteComponentUrl>,
    vbmeta: Option<AbsoluteComponentUrl>,
}

impl BootSlot {
    fn new() -> Self {
        BootSlot { zbi: None, vbmeta: None }
    }

    fn set_zbi(&mut self, zbi: AbsoluteComponentUrl) {
        self.zbi = Some(zbi);
    }

    fn set_vbmeta(&mut self, vbmeta: AbsoluteComponentUrl) {
        self.vbmeta = Some(vbmeta);
    }

    fn is_empty(&self) -> bool {
        matches!(self, BootSlot { zbi: None, vbmeta: None })
    }

    fn print_names(&self) -> Vec<String> {
        let mut image_names = vec![];
        if self.zbi.is_some() {
            image_names.push("Zbi".to_string());
        }
        if self.vbmeta.is_some() {
            image_names.push("Vbmeta".to_string());
        }

        image_names
    }

    fn get_url_hashes(&self) -> HashSet<fuchsia_hash::Hash> {
        let mut hashes = HashSet::new();

        if let Some(zbi) = &self.zbi {
            if let Some(zbi_hash) = zbi.package_url().hash() {
                hashes.insert(zbi_hash);
            }
        }

        if let Some(vbmeta) = &self.vbmeta {
            if let Some(vbmeta_hash) = vbmeta.package_url().hash() {
                hashes.insert(vbmeta_hash);
            }
        }

        hashes
    }

    fn get_urls(&self) -> HashSet<AbsolutePackageUrl> {
        let mut urls = HashSet::new();
        if let Some(zbi) = &self.zbi {
            urls.insert(zbi.package_url().to_owned());
        }
        if let Some(vbmeta) = &self.vbmeta {
            urls.insert(vbmeta.package_url().to_owned());
        }
        urls
    }
}

struct Attempt<'a> {
    config: &'a Config,
    env: &'a Environment,
    concurrent_package_resolves: usize,
}

impl Attempt<'_> {
    // Run the update attempt, if update is canceled, any await during this attempt could be an
    // early return point.
    async fn run(
        mut self,
        co: &mut async_generator::Yield<fupdate_installer_ext::State>,
        phase: &mut metrics::Phase,
        target_version: &mut history::Version,
    ) -> Result<(state::WaitToReboot, update_package::UpdateMode), AttemptError> {
        // Prepare
        let state = state::Prepare::enter(co).await;

        let (update_pkg, mode, packages_to_fetch, images_to_write, current_configuration) =
            match self.prepare(target_version).await {
                Ok((
                    update_pkg,
                    mode,
                    packages_to_fetch,
                    images_to_write,
                    current_configuration,
                )) => (update_pkg, mode, packages_to_fetch, images_to_write, current_configuration),
                Err(e) => {
                    state.fail(co, e.reason()).await;
                    return Err(e.into());
                }
            };

        // Write images
        let mut state = state
            .enter_stage(
                co,
                fupdate_installer_ext::UpdateInfo::builder().download_size(0).build(),
                packages_to_fetch.len() as u64 + 1,
            )
            .await;
        *phase = metrics::Phase::ImageWrite;

        let () = match self
            .stage_images(
                co,
                &mut state,
                &update_pkg,
                current_configuration,
                images_to_write,
                &packages_to_fetch,
            )
            .await
        {
            Ok(()) => (),
            Err(e) => {
                state.fail(co, e.reason()).await;
                return Err(e.into());
            }
        };

        // Fetch packages
        let mut state = state.enter_fetch(co).await;
        *phase = metrics::Phase::PackageDownload;

        let () = match self
            .fetch_packages(co, &mut state, packages_to_fetch, mode, update_pkg.1)
            .await
        {
            Ok(()) => (),
            Err(e) => {
                state.fail(co, e.reason()).await;
                return Err(e.into());
            }
        };

        // Commit the update
        let state = state.enter_commit(co).await;
        *phase = metrics::Phase::ImageCommit;

        let () = match self.commit_images(mode, current_configuration).await {
            Ok(()) => (),
            Err(e) => {
                state.fail(co).await;
                return Err(AttemptError::Commit(e));
            }
        };

        // Success!
        let state = state.enter_wait_to_reboot(co).await;
        *phase = metrics::Phase::SuccessPendingReboot;

        Ok((state, mode))
    }

    /// Acquire the necessary data to perform the update.
    ///
    /// This includes fetching the update package, which contains the list of packages in the
    /// target OS and kernel images that need written.
    async fn prepare(
        &mut self,
        target_version: &mut history::Version,
    ) -> Result<
        (
            (update_package::UpdatePackage, Option<Hash>),
            update_package::UpdateMode,
            Vec<PinnedAbsolutePackageUrl>,
            ImagesToWrite,
            paver::CurrentConfiguration,
        ),
        PrepareError,
    > {
        // Ensure that the partition boot metadata is ready for the update to begin. Specifically:
        // - the current configuration must be Healthy and Active, and
        // - the non-current configuration must be Unbootable.
        //
        // If anything goes wrong, abort the update. See the comments in
        // `prepare_partition_metadata` for why this is justified.
        //
        // We do this here rather than just before we write images because this location allows us
        // to "unstage" a previously staged OS in the non-current configuration that would otherwise
        // become active on next reboot. If we moved this to just before writing images, we would be
        // susceptible to a bug of the form:
        // - A is active/current running system version 1.
        // - Stage an OTA of version 2 to B, B is now marked active. Defer reboot.
        // - Start to stage a new OTA (version 3). Fetch packages encounters an error after fetching
        //   half of the updated packages.
        // - Retry the attempt for the new OTA (version 3). This GC may delete packages from the
        //   not-yet-booted system (version 2).
        // - Interrupt the update attempt, reboot.
        // - System attempts to boot to B (version 2), but the packages are not all present anymore
        let current_config = paver::prepare_partition_metadata(&self.env.boot_manager)
            .await
            .map_err(PrepareError::PreparePartitionMetdata)?;

        let update_url = AbsolutePackageUrl::from_url(&self.config.update_url)
            .map_err(PrepareError::ParseUpdatePackageUrl)?;
        let update_pkg = resolve_update_package(
            &self.env.pkg_resolver,
            &update_url,
            &self.env.space_manager,
            &self.env.retained_packages,
        )
        .await
        .map_err(PrepareError::ResolveUpdate)?;

        let update_package_hash = if let Some(hash) = update_url.hash() {
            Some(hash)
        } else {
            match update_pkg.hash().await {
                Ok(hash) => Some(hash),
                Err(e) => {
                    error!(
                        "unable to obtain the hash of the resolved update package: {:#}",
                        anyhow!(e)
                    );
                    None
                }
            }
        };

        *target_version = history::Version::for_update_package(&update_pkg).await;
        let () = update_pkg.verify_name().await.map_err(PrepareError::VerifyName)?;

        let mode = update_mode(&update_pkg).await.map_err(PrepareError::ParseUpdateMode)?;
        match mode {
            update_package::UpdateMode::Normal => {}
            update_package::UpdateMode::ForceRecovery => {
                if !self.config.should_write_recovery {
                    return Err(PrepareError::VerifyUpdateMode);
                }
            }
        }

        verify_board(&self.env.build_info, &update_pkg).await.map_err(PrepareError::VerifyBoard)?;

        let packages_to_fetch = match mode {
            update_package::UpdateMode::Normal => {
                update_pkg.packages().await.map_err(PrepareError::ParsePackages)?
            }
            update_package::UpdateMode::ForceRecovery => vec![],
        };

        let epoch =
            update_pkg.epoch().await.map_err(PrepareError::ParseTargetEpochError)?.unwrap_or_else(
                || {
                    info!("no epoch in update package, assuming it's 0");
                    0
                },
            );
        let () = validate_epoch(SOURCE_EPOCH_RAW, epoch)?;

        let images_metadata =
            update_pkg.images_metadata().await.map_err(PrepareError::ParseImages)?;
        let () = images_metadata.verify(mode).map_err(PrepareError::VerifyImages)?;
        let mut images_to_write = ImagesToWrite::new();

        let target_config = current_config.to_non_current_configuration().to_target_configuration();
        if let Some(fuchsia) = images_metadata.fuchsia() {
            // Determine if the fuchsia zbi has changed in this update.
            if should_write_image(
                fuchsia.zbi().sha256(),
                fuchsia.zbi().size(),
                current_config,
                target_config,
                &self.env.data_sink,
                ImageType::Asset(fpaver::Asset::Kernel),
            )
            .await
            {
                images_to_write.fuchsia.set_zbi(fuchsia.zbi().url().clone());
            }

            if let Some(vbmeta) = fuchsia.vbmeta() {
                target_version.vbmeta_hash = vbmeta.sha256().to_string();
                // Determine if the vbmeta has changed in this update.
                if should_write_image(
                    vbmeta.sha256(),
                    vbmeta.size(),
                    current_config,
                    target_config,
                    &self.env.data_sink,
                    ImageType::Asset(fpaver::Asset::VerifiedBootMetadata),
                )
                .await
                {
                    images_to_write.fuchsia.set_vbmeta(vbmeta.url().clone());
                }
            }
        }

        // Only check these images if we have to.
        if self.config.should_write_recovery {
            if let Some(recovery) = images_metadata.recovery() {
                let target_config =
                    paver::TargetConfiguration::Single(fpaver::Configuration::Recovery);
                if should_write_image(
                    recovery.zbi().sha256(),
                    recovery.zbi().size(),
                    current_config,
                    target_config,
                    &self.env.data_sink,
                    ImageType::Asset(fpaver::Asset::Kernel),
                )
                .await
                {
                    images_to_write.recovery.set_zbi(recovery.zbi().url().clone());
                }

                if let Some(vbmeta_image) = recovery.vbmeta() {
                    // Determine if the vbmeta has changed in this update.
                    if should_write_image(
                        vbmeta_image.sha256(),
                        vbmeta_image.size(),
                        current_config,
                        target_config,
                        &self.env.data_sink,
                        ImageType::Asset(fpaver::Asset::VerifiedBootMetadata),
                    )
                    .await
                    {
                        images_to_write.recovery.set_vbmeta(vbmeta_image.url().clone())
                    }
                }
            }
        }

        for (type_, metadata) in images_metadata.firmware() {
            if should_write_image(
                metadata.sha256(),
                metadata.size(),
                current_config,
                target_config,
                &self.env.data_sink,
                ImageType::Firmware { type_ },
            )
            .await
            {
                images_to_write.firmware.push((type_.clone(), metadata.url().clone()))
            }
        }

        Ok((
            (update_pkg, update_package_hash),
            mode,
            packages_to_fetch,
            images_to_write,
            current_config,
        ))
    }

    /// Pave the various raw images (zbi, firmware, vbmeta) for fuchsia and/or recovery.
    async fn stage_images(
        &mut self,
        co: &mut async_generator::Yield<fupdate_installer_ext::State>,
        state: &mut state::Stage,
        update_pkg: &(update_package::UpdatePackage, Option<Hash>),
        current_configuration: paver::CurrentConfiguration,
        images_to_write: ImagesToWrite,
        packages_to_fetch: &[PinnedAbsolutePackageUrl],
    ) -> Result<(), StageError> {
        if images_to_write.is_empty() {
            // This is possible if the images for the update were on one of the partitions already
            // and written during State::Prepare.
            //
            // This is a separate block so that we avoid unnecessarily replacing the retained index
            // and garbage collecting.
            info!("Images have already been written!");

            // Be sure to persist those images that were written during State::Prepare!
            paver::paver_flush_data_sink(&self.env.data_sink)
                .await
                .map_err(StageError::PaverFlush)?;

            state.add_progress(co, 1).await;
            return Ok(());
        }

        let () = replace_retained_packages(
            packages_to_fetch
                .iter()
                .map(|url| url.hash())
                .chain(images_to_write.get_url_hashes())
                .chain(update_pkg.1),
            &self.env.retained_packages,
        )
        .await
        .unwrap_or_else(|e| {
            error!(
                "unable to replace retained packages set before gc in preparation \
                    for fetching image packages listed in update package: {:#}",
                anyhow!(e)
            )
        });

        if let Err(e) = gc(&self.env.space_manager).await {
            error!(
                "unable to gc packages in preparation to write image packages: {:#}",
                anyhow!(e)
            );
        }

        info!("Images to write: {:?}", images_to_write.print_list());
        let desired_config = current_configuration.to_non_current_configuration();
        info!("Targeting configuration: {:?}", desired_config);

        write_image_packages(
            images_to_write,
            &self.env.pkg_resolver,
            desired_config.to_target_configuration(),
            &self.env.data_sink,
            update_pkg.1,
            &self.env.retained_packages,
            &self.env.space_manager,
            self.concurrent_package_resolves,
        )
        .await?;

        paver::paver_flush_data_sink(&self.env.data_sink).await.map_err(StageError::PaverFlush)?;

        state.add_progress(co, 1).await;
        Ok(())
    }

    /// Fetch all packages needed by the target OS.
    async fn fetch_packages(
        &mut self,
        co: &mut async_generator::Yield<fupdate_installer_ext::State>,
        state: &mut state::Fetch,
        packages_to_fetch: Vec<PinnedAbsolutePackageUrl>,
        mode: update_package::UpdateMode,
        update_pkg: Option<Hash>,
    ) -> Result<(), FetchError> {
        // Remove ImagesToWrite from the retained_index.
        // GC to remove the ImagesToWrite from blobfs.
        let () = replace_retained_packages(
            packages_to_fetch.iter().map(|url| url.hash()).chain(update_pkg),
            &self.env.retained_packages,
        )
        .await
        .unwrap_or_else(|e| {
            error!(
                "unable to replace retained packages set before gc in preparation \
                 for fetching packages listed in update package: {:#}",
                anyhow!(e)
            )
        });

        if let Err(e) = gc(&self.env.space_manager).await {
            error!("unable to gc packages during Fetch state: {:#}", anyhow!(e));
        }

        let mut package_dir_futs = futures::stream::iter(packages_to_fetch)
            .map(async |url| resolver::resolve_package(&self.env.pkg_resolver, &url.into()).await)
            .buffer_unordered(self.concurrent_package_resolves);

        while let Some(_package_dir) =
            package_dir_futs.try_next().await.map_err(FetchError::Resolve)?
        {
            state.add_progress(co, 1).await;
        }

        match mode {
            update_package::UpdateMode::Normal => {
                sync_package_cache(&self.env.pkg_cache).await.map_err(FetchError::Sync)?
            }
            update_package::UpdateMode::ForceRecovery => {}
        }

        Ok(())
    }

    /// Configure the non-current configuration (or recovery) as active for the next boot.
    async fn commit_images(
        &self,
        mode: update_package::UpdateMode,
        current_configuration: paver::CurrentConfiguration,
    ) -> Result<(), Error> {
        let desired_config = current_configuration.to_non_current_configuration();

        match mode {
            update_package::UpdateMode::Normal => {
                let () =
                    paver::set_configuration_active(&self.env.boot_manager, desired_config).await?;
            }
            update_package::UpdateMode::ForceRecovery => {
                let () = paver::set_recovery_configuration_active(&self.env.boot_manager).await?;
            }
        }

        match desired_config {
            paver::NonCurrentConfiguration::A | paver::NonCurrentConfiguration::B => {
                paver::paver_flush_boot_manager(&self.env.boot_manager).await?;
            }
            paver::NonCurrentConfiguration::NotSupported => {}
        }

        Ok(())
    }
}

// Update attempt that uses a manifest instead of update package.
struct AttemptV2<'a> {
    config: &'a Config,
    env: &'a Environment,
    concurrent_blob_fetches: usize,
}

impl AttemptV2<'_> {
    // Run the update attempt, if update is canceled, any await during this attempt could be an
    // early return point.
    async fn run(
        mut self,
        co: &mut async_generator::Yield<fupdate_installer_ext::State>,
        phase: &mut metrics::Phase,
        target_version: &mut history::Version,
    ) -> Result<(state::WaitToReboot, update_package::UpdateMode), AttemptError> {
        // Prepare
        let state = state::Prepare::enter(co).await;

        let (current_configuration, manifest) = match self.prepare(target_version).await {
            Ok(tuple) => tuple,
            Err(e) => {
                state.fail(co, e.reason()).await;
                return Err(e.into());
            }
        };

        let blobfs = blobfs::Client::builder()
            .readable()
            .build()
            .await
            .map_err(|e| AttemptError::Prepare(PrepareError::OpenBlobfs(e)))?;

        // Write images
        let mut state = state
            .enter_stage(
                co,
                fupdate_installer_ext::UpdateInfo::builder().download_size(0).build(),
                (manifest.images.len() + manifest.blobs.len()) as u64,
            )
            .await;
        *phase = metrics::Phase::ImageWrite;

        let () = match self
            .stage_images(co, &mut state, current_configuration, &manifest, &blobfs)
            .await
        {
            Ok(()) => (),
            Err(e) => {
                state.fail(co, e.reason()).await;
                return Err(e.into());
            }
        };

        unimplemented!("AttemptV2 is still work in progress");
    }

    /// Acquire the necessary data to perform the update.
    ///
    /// This includes fetching the update manifest, which contains the list of blobs in the
    /// target OS and partition images that need written.
    async fn prepare(
        &mut self,
        target_version: &mut history::Version,
    ) -> Result<(paver::CurrentConfiguration, OtaManifestV1), PrepareError> {
        // Ensure that the partition boot metadata is ready for the update to begin. Specifically:
        // - the current configuration must be Healthy and Active, and
        // - the non-current configuration must be Unbootable.
        //
        // If anything goes wrong, abort the update. See the comments in
        // `prepare_partition_metadata` for why this is justified.
        //
        // We do this here rather than just before we write images because this location allows us
        // to "unstage" a previously staged OS in the non-current configuration that would otherwise
        // become active on next reboot. If we moved this to just before writing images, we would be
        // susceptible to a bug of the form:
        // - A is active/current running system version 1.
        // - Stage an OTA of version 2 to B, B is now marked active. Defer reboot.
        // - Start to stage a new OTA (version 3). Fetch packages encounters an error after fetching
        //   half of the updated packages.
        // - Retry the attempt for the new OTA (version 3). This GC may delete packages from the
        //   not-yet-booted system (version 2).
        // - Interrupt the update attempt, reboot.
        // - System attempts to boot to B (version 2), but the packages are not all present anymore
        let current_config = paver::prepare_partition_metadata(&self.env.boot_manager)
            .await
            .map_err(PrepareError::PreparePartitionMetdata)?;

        let manifest_bytes = fetch_url(self.config.update_url.as_ref(), None)
            .await
            .map_err(PrepareError::FetchUrl)?;

        let manifest = update_package::manifest::parse_ota_manifest(&manifest_bytes)
            .map_err(PrepareError::ParseManifest)?;

        *target_version = history::Version::for_manifest(&manifest);

        let () = verify_board_in_manifest(&self.env.build_info, &manifest)
            .await
            .map_err(PrepareError::VerifyBoard)?;

        let () = validate_epoch(SOURCE_EPOCH_RAW, manifest.epoch)?;

        Ok((current_config, manifest))
    }

    /// Pave the various raw images (zbi, firmware, vbmeta) for fuchsia and/or recovery.
    async fn stage_images(
        &mut self,
        co: &mut async_generator::Yield<fupdate_installer_ext::State>,
        state: &mut state::Stage,
        current_configuration: paver::CurrentConfiguration,
        manifest: &OtaManifestV1,
        blobfs: &blobfs::Client,
    ) -> Result<(), StageError> {
        // Protect all blobs to guarantee forward progress, this might cause out of space issue in a
        // rare condition: a previous update attempt was almost done before it was stopped and then
        // we attempt a new update which contains a different image but most blobs are the same. If
        // we encounter out of space error fetching images, fallback to only retain blobs for
        // images.
        let () = replace_retained_blobs(
            manifest
                .images
                .iter()
                .map(|image| image.fuchsia_merkle_root)
                .chain(manifest.blobs.iter().map(|blob| blob.fuchsia_merkle_root)),
            &self.env.retained_blobs,
        )
        .await
        .unwrap_or_else(|e| {
            error!(
                "unable to replace retained blobs set before gc in preparation \
                    for fetching images listed in OTA manifest: {:#}",
                anyhow!(e)
            )
        });

        if let Err(e) = gc(&self.env.space_manager).await {
            error!("unable to gc blobs in preparation to write image blobs: {:#}", anyhow!(e));
        }

        info!("Images to write: {:?}", manifest.images);
        let desired_config = current_configuration.to_non_current_configuration();
        info!("Targeting configuration: {:?}", desired_config);
        let target_config = desired_config.to_target_configuration();

        let mut stream = futures::stream::iter(manifest.images.iter())
            .map(async |image| {
                if !self.config.should_write_recovery
                    && image.slot == update_package::manifest::Slot::R
                {
                    return Ok(());
                }
                let image_type = (&image.image_type).into();
                if should_write_image(
                    image.sha256,
                    image.size,
                    current_configuration,
                    target_config,
                    &self.env.data_sink,
                    image_type,
                )
                .await
                {
                    let blob_id = fpkg_ext::BlobId::from(image.fuchsia_merkle_root).into();
                    match self
                        .env
                        .ota_downloader
                        .fetch_blob(&blob_id, manifest.blob_base_url.as_ref())
                        .await
                        .map_err(StageError::Fidl)?
                    {
                        Ok(()) => {}
                        Err(fpkg::ResolveError::NoSpace) => {
                            let () = replace_retained_blobs(
                                manifest.images.iter().map(|image| image.fuchsia_merkle_root),
                                &self.env.retained_blobs,
                            )
                            .await
                            .unwrap_or_else(|e| {
                                error!(
                                    "while fetching images, unable to minimize retained blobs set \
                                     before second gc attempt: {:#}",
                                    anyhow!(e)
                                )
                            });

                            if let Err(e) = gc(&self.env.space_manager).await {
                                error!(
                                    "unable to gc blobs before retry fetching blobs: {:#}",
                                    anyhow!(e)
                                );
                            }
                            let () = self
                                .env
                                .ota_downloader
                                .fetch_blob(&blob_id, manifest.blob_base_url.as_ref())
                                .await
                                .map_err(StageError::Fidl)?
                                .map_err(|e| StageError::FetchBlob(e.into()))?;
                        }
                        Err(e) => {
                            return Err(StageError::FetchBlob(e.into()));
                        }
                    }
                    let vmo = blobfs
                        .get_blob_vmo(&image.fuchsia_merkle_root)
                        .await
                        .map_err(StageError::GetBlobVmo)?;
                    // The paver service requires VMOs that are resizable, and blobfs does not give
                    // out resizable VMOs. Fortunately, a copy-on-write child clone of the vmo can
                    // be made resizable.
                    let vmo = vmo
                        .create_child(
                            zx::VmoChildOptions::SNAPSHOT_AT_LEAST_ON_WRITE
                                | zx::VmoChildOptions::RESIZABLE,
                            0,
                            image.size,
                        )
                        .map_err(|status| {
                            StageError::OpenImageError(
                                update_package::OpenImageError::CloneBuffer {
                                    path: image.fuchsia_merkle_root.to_string(),
                                    status,
                                },
                            )
                        })?;
                    let buffer = fmem::Buffer { vmo, size: image.size };

                    paver::write_image(&self.env.data_sink, buffer, target_config, image_type)
                        .await
                        .map_err(StageError::Write)?;
                }
                Ok(())
            })
            .buffer_unordered(self.concurrent_blob_fetches);

        while let Some(()) = stream.try_next().await? {
            state.add_progress(co, 1).await;
        }

        paver::paver_flush_data_sink(&self.env.data_sink).await.map_err(StageError::PaverFlush)?;

        Ok(())
    }
}

async fn write_image_from_package(
    package: &update_package::UpdateImagePackage,
    resource_path: &str,
    data_sink: &fpaver::DataSinkProxy,
    target_config: paver::TargetConfiguration,
    image_type: ImageType<'_>,
) -> Result<(), StageError> {
    let buffer = package.open_image(resource_path).await.map_err(StageError::OpenImageError)?;
    paver::write_image(data_sink, buffer, target_config, image_type)
        .await
        .map_err(StageError::Write)
}

/// Returning false indicates that the asset is on the device in the desired configuration.
/// If the asset is on the active configuration, this function will write it to the desired
/// configuration before returning false.
///
/// Returning true indicates that the asset in the update differs from what is on the device.
async fn should_write_image(
    image_sha256: fuchsia_hash::Sha256,
    image_size: u64,
    current_config: paver::CurrentConfiguration,
    target_config: paver::TargetConfiguration,
    data_sink: &fpaver::DataSinkProxy,
    image_type: ImageType<'_>,
) -> bool {
    if let paver::TargetConfiguration::Single(single_target_config) = target_config {
        if get_image_buffer_if_hash_and_size_match(
            data_sink,
            single_target_config,
            image_type,
            image_sha256,
            image_size,
        )
        .await
        .is_some()
        {
            info!(
                target_config:?,
                image_type:?,
                image_sha256:?,
                image_size;
                "Target configuration already contains the desired target image, skip writing"
            );
            return false;
        }
    }
    // Skip if writing to recovery slot, because the recovery images won't have the same content as
    // current slot.
    if target_config != paver::TargetConfiguration::Single(fpaver::Configuration::Recovery) {
        if let Some(current_config) = current_config.to_configuration() {
            if let Some(buffer) = get_image_buffer_if_hash_and_size_match(
                data_sink,
                current_config,
                image_type,
                image_sha256,
                image_size,
            )
            .await
            {
                info!(
                    current_config:?,
                    target_config:?,
                    image_type:?,
                    image_sha256:?,
                    image_size;
                    "Current configuration contains the desired target image, \
                    copying to avoid a download"
                );
                if let Err(e) =
                    paver::write_image(data_sink, buffer, target_config, image_type).await
                {
                    error!("Error copying {image_type:?}, fallback to download: {:#}", anyhow!(e));
                    return true;
                }
                return false;
            }
        }
    }
    true
}

#[derive(Debug, Clone, Copy)]
enum ImageType<'a> {
    Asset(fpaver::Asset),
    Firmware { type_: &'a str },
}

impl<'a> From<&'a update_package::manifest::ImageType> for ImageType<'a> {
    fn from(image_type: &'a update_package::manifest::ImageType) -> Self {
        match image_type {
            update_package::manifest::ImageType::Asset(asset) => ImageType::Asset(match asset {
                update_package::images::AssetType::Zbi => fpaver::Asset::Kernel,
                update_package::images::AssetType::Vbmeta => fpaver::Asset::VerifiedBootMetadata,
            }),
            update_package::manifest::ImageType::Firmware(type_) => ImageType::Firmware { type_ },
        }
    }
}

async fn get_image_buffer_if_hash_and_size_match(
    data_sink: &fpaver::DataSinkProxy,
    configuration: fpaver::Configuration,
    image_type: ImageType<'_>,
    image_sha256: fuchsia_hash::Sha256,
    image_size: u64,
) -> Option<fmem::Buffer> {
    let fmem::Buffer { vmo, size } =
        match paver::read_image(data_sink, configuration, image_type).await {
            Ok(buffer) => buffer,
            Err(e) => {
                warn!(
                    configuration:?,
                    image_type:?,
                    image_sha256:?,
                    image_size;
                    "Error reading image, so it will not be used to avoid a download: {:#}",
                    anyhow!(e)
                );
                return None;
            }
        };

    // The size field of the fuchsia.mem.Buffer is either the size of the entire partition or just
    // the image.
    if size < image_size {
        return None;
    }
    let buffer = fmem::Buffer { vmo, size: image_size };
    let buffer_hash = match sha256_buffer(&buffer) {
        Ok(hash) => hash,
        Err(e) => {
            warn!(
                configuration:?,
                image_type:?,
                image_sha256:?,
                image_size;
                "Error hashing image so it will not be used to avoid a download: {:#}",
                anyhow!(e)
            );
            return None;
        }
    };
    if buffer_hash == image_sha256 {
        Some(buffer)
    } else {
        None
    }
}

fn sha256_buffer(
    fmem::Buffer { vmo, size }: &fmem::Buffer,
) -> anyhow::Result<fuchsia_hash::Sha256> {
    let mapping =
        mapped_vmo::ImmutableMapping::create_from_vmo(vmo, true).context("mapping the buffer")?;
    let size: usize = (*size).try_into().context("buffer size as usize")?;
    if size > mapping.len() {
        anyhow::bail!("buffer size {size} larger than vmo size {}", mapping.len());
    }
    Ok(From::from(*AsRef::<[u8; 32]>::as_ref(&<sha2::Sha256 as sha2::Digest>::digest(
        &mapping[..size],
    ))))
}

async fn sync_package_cache(pkg_cache: &fpkg::PackageCacheProxy) -> Result<(), Error> {
    async move {
        pkg_cache
            .sync()
            .await
            .context("while performing sync call")?
            .map_err(zx::Status::from_raw)
            .context("sync responded with")
    }
    .await
    .context("while flushing packages to persistent storage")
}

async fn gc(space_manager: &fspace::ManagerProxy) -> Result<(), Error> {
    let () = space_manager
        .gc()
        .await
        .context("while performing gc call")?
        .map_err(|e| anyhow!("garbage collection responded with {:?}", e))?;
    Ok(())
}

// Resolve and write the image packages to their appropriate partitions,
// incorporating an increasingly aggressive GC and retry strategy.
async fn write_image_packages(
    images_to_write: ImagesToWrite,
    pkg_resolver: &fpkg::PackageResolverProxy,
    target_config: paver::TargetConfiguration,
    data_sink: &fpaver::DataSinkProxy,
    update_pkg: Option<Hash>,
    retained_packages: &fpkg::RetainedPackagesProxy,
    space_manager: &fspace::ManagerProxy,
    concurrent_package_resolves: usize,
) -> Result<(), StageError> {
    match images_to_write
        .write(pkg_resolver, target_config, data_sink, concurrent_package_resolves)
        .await
    {
        Ok(()) => return Ok(()),
        Err(StageError::Resolve(ResolveError::Error(fpkg_ext::ResolveError::NoSpace, _))) => {}
        Err(e) => return Err(e),
    };

    let to_protect = images_to_write.get_url_hashes().into_iter().chain(update_pkg);
    let () = replace_retained_packages(to_protect, retained_packages).await.unwrap_or_else(|e| {
        error!(
            "while resolving image packages, unable to minimize retained packages set before \
                    second gc attempt: {:#}",
            anyhow!(e)
        )
    });
    if let Err(e) = gc(space_manager).await {
        error!(
            "unable to gc base packages before second image package write retry: {:#}",
            anyhow!(e)
        );
    }

    images_to_write.write(pkg_resolver, target_config, data_sink, concurrent_package_resolves).await
}

/// Resolve the update package, incorporating an increasingly aggressive GC and retry strategy.
async fn resolve_update_package(
    pkg_resolver: &fpkg::PackageResolverProxy,
    update_url: &AbsolutePackageUrl,
    space_manager: &fspace::ManagerProxy,
    retained_packages: &fpkg::RetainedPackagesProxy,
) -> Result<update_package::UpdatePackage, ResolveError> {
    // First, attempt to resolve the update package.
    match resolver::resolve_update_package(pkg_resolver, update_url).await {
        Ok(update_pkg) => return Ok(update_pkg),
        Err(ResolveError::Error(fpkg_ext::ResolveError::NoSpace, _)) => (),
        Err(e) => return Err(e),
    }

    // If the first attempt fails with NoSpace, perform a GC and retry.
    if let Err(e) = gc(space_manager).await {
        error!("unable to gc packages before first resolve retry: {:#}", anyhow!(e));
    }
    match resolver::resolve_update_package(pkg_resolver, update_url).await {
        Ok(update_pkg) => return Ok(update_pkg),
        Err(ResolveError::Error(fpkg_ext::ResolveError::NoSpace, _)) => (),
        Err(e) => return Err(e),
    }

    // If the second attempt fails with NoSpace, remove packages we aren't sure we need from the
    // retained packages set, perform a GC and retry. If the third attempt fails,
    // return the error regardless of type.
    let () = async {
        if let Some(hash) = update_url.hash() {
            let () = replace_retained_packages(std::iter::once(hash), retained_packages)
                .await
                .context("serve_blob_id_iterator")?;
        } else {
            let () = retained_packages.clear().await.context("calling RetainedPackages.Clear")?;
        }
        Ok(())
    }
    .await
    .unwrap_or_else(|e: anyhow::Error| {
        error!(
            "while resolving update package, unable to minimize retained packages set before \
             second gc attempt: {:#}",
            anyhow!(e)
        )
    });

    if let Err(e) = gc(space_manager).await {
        error!("unable to gc packages before second resolve retry: {:#}", anyhow!(e));
    }
    resolver::resolve_update_package(pkg_resolver, update_url).await
}

async fn verify_board<B>(build_info: &B, pkg: &update_package::UpdatePackage) -> Result<(), Error>
where
    B: BuildInfo,
{
    let current_board = build_info.board().await.context("while determining current board")?;
    if let Some(current_board) = current_board {
        let () = pkg.verify_board(&current_board).await.context("while verifying target board")?;
    }
    Ok(())
}

async fn verify_board_in_manifest<B>(build_info: &B, manifest: &OtaManifestV1) -> Result<(), Error>
where
    B: BuildInfo,
{
    let current_board = build_info.board().await.context("while determining current board")?;
    if let Some(current_board) = current_board {
        if manifest.board != current_board {
            return Err(anyhow!("expected board name {current_board} found {}", manifest.board));
        }
    }
    Ok(())
}

async fn update_mode(
    pkg: &update_package::UpdatePackage,
) -> Result<update_package::UpdateMode, update_package::ParseUpdateModeError> {
    pkg.update_mode().await.map(|opt| {
        opt.unwrap_or_else(|| {
            let mode = update_package::UpdateMode::default();
            info!("update-mode file not found, using default mode: {:?}", mode);
            mode
        })
    })
}

/// Verify that epoch is non-decreasing. For more context, see
/// [RFC-0071](https://fuchsia.dev/fuchsia-src/contribute/governance/rfcs/0071_ota_backstop).
#[allow(clippy::result_large_err)]
fn validate_epoch(source_epoch_raw: &str, target: u64) -> Result<(), PrepareError> {
    let src = match serde_json::from_str(source_epoch_raw)
        .map_err(|e| PrepareError::ParseSourceEpochError(source_epoch_raw.to_string(), e))?
    {
        epoch::EpochFile::Version1 { epoch } => epoch,
    };
    if target < src {
        return Err(PrepareError::UnsupportedDowngrade { src, target });
    }
    Ok(())
}

async fn replace_retained_packages(
    hashes: impl IntoIterator<Item = fuchsia_hash::Hash>,
    retained_packages: &fpkg::RetainedPackagesProxy,
) -> Result<(), anyhow::Error> {
    let (client_end, stream) = fidl::endpoints::create_request_stream();
    let replace_resp = retained_packages.replace(client_end);
    let () = fpkg_ext::serve_fidl_iterator_from_slice(
        stream,
        hashes.into_iter().map(|hash| fpkg_ext::BlobId::from(hash).into()).collect::<Vec<_>>(),
    )
    .await
    .unwrap_or_else(|e| {
        error!(
            "error serving {} protocol: {:#}",
            fpkg::RetainedPackagesMarker::DEBUG_NAME,
            anyhow!(e)
        )
    });
    replace_resp.await.context("calling RetainedPackages.Replace")
}

async fn replace_retained_blobs(
    hashes: impl IntoIterator<Item = fuchsia_hash::Hash>,
    retained_blobs: &fpkg::RetainedBlobsProxy,
) -> Result<(), anyhow::Error> {
    let (client_end, stream) = fidl::endpoints::create_request_stream();
    let replace_resp = retained_blobs.replace(client_end);
    let () = fpkg_ext::serve_fidl_iterator_from_slice(
        stream,
        hashes.into_iter().map(|hash| fpkg_ext::BlobId::from(hash).into()).collect::<Vec<_>>(),
    )
    .await
    .unwrap_or_else(|e| {
        error!("error serving {} protocol: {:#}", fpkg::RetainedBlobsMarker::DEBUG_NAME, anyhow!(e))
    });
    replace_resp.await.context("calling RetainedBlobs.Replace")
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_matches::assert_matches;
    use fuchsia_async as fasync;
    use fuchsia_pkg_testing::make_epoch_json;

    // Simulate the cobalt test hanging indefinitely, and ensure we time out correctly.
    // This test deliberately logs an error.
    #[fasync::run_singlethreaded(test)]
    async fn flush_cobalt_succeeds_when_cobalt_hangs() {
        let hung_task = futures::future::pending();
        flush_cobalt(hung_task, Duration::from_secs(2)).await;
    }

    #[test]
    fn validate_epoch_success() {
        let source = make_epoch_json(1);
        let target = 2;

        let res = validate_epoch(&source, target);

        assert_matches!(res, Ok(()));
    }

    #[test]
    fn validate_epoch_fail_unsupported_downgrade() {
        let source = make_epoch_json(2);
        let target = 1;

        let res = validate_epoch(&source, target);

        assert_matches!(res, Err(PrepareError::UnsupportedDowngrade { src: 2, target: 1 }));
    }

    #[test]
    fn validate_epoch_fail_parse_source() {
        let res = validate_epoch("invalid source epoch.json", 1);

        assert_matches!(
            res,
            Err(PrepareError::ParseSourceEpochError(s, _)) if s == "invalid source epoch.json"
        );
    }
}

#[cfg(test)]
mod test_sha256_buffer {
    use super::*;
    use assert_matches::assert_matches;

    fn make_buffer(payload: Vec<u8>) -> fmem::Buffer {
        let vmo = zx::Vmo::create(payload.len().try_into().unwrap()).unwrap();
        let () = vmo.write(&payload, 0).unwrap();
        fmem::Buffer { vmo, size: payload.len().try_into().unwrap() }
    }

    #[test]
    fn empty() {
        let buffer = make_buffer(vec![]);
        let calc_hash = sha256_buffer(&buffer).unwrap();
        assert_eq!(
            calc_hash,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855".parse().unwrap()
        );
    }

    #[test]
    fn large() {
        let buffer = make_buffer(vec![0; 1024 * 50]);
        let calc_hash = sha256_buffer(&buffer).unwrap();
        assert_eq!(
            calc_hash,
            "16fa66a7dc98d93f2a4c5d20baf5177f59c4c37fc62face65690c11c15fe6ff9".parse().unwrap()
        );
    }

    #[test]
    fn buffer_size_smaller_than_vmo_size_uses_buffer_size() {
        let mut buffer = make_buffer(vec![0; 1024 * 51]);
        buffer.size = 1024 * 50;
        let calc_hash = sha256_buffer(&buffer).unwrap();
        assert_eq!(
            calc_hash,
            "16fa66a7dc98d93f2a4c5d20baf5177f59c4c37fc62face65690c11c15fe6ff9".parse().unwrap()
        );
    }

    #[test]
    fn buffer_size_larger_than_vmo_size_errors() {
        let mut buffer = make_buffer(vec![0; 10]);
        // vmo size will be rounded up to nearest page multiple
        buffer.size = 4097;
        assert_matches!(sha256_buffer(&buffer), Err(_));
    }
}
