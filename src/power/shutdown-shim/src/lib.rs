// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
mod reboot_reasons;
mod shutdown_watcher;

use crate::reboot_reasons::RebootReasons;
use crate::shutdown_watcher::ShutdownWatcher;
use anyhow::{format_err, Context};
use fidl::endpoints::{DiscoverableProtocolMarker, ServerEnd};
use fidl::HandleBased;
use fidl_fuchsia_hardware_power_statecontrol::{
    AdminMexecRequest, AdminRequest, AdminRequestStream, RebootMethodsWatcherRegisterRequestStream,
    RebootOptions, RebootReason, RebootReason2,
};
use fidl_fuchsia_power::CollaborativeRebootInitiatorRequestStream;
use fidl_fuchsia_power_internal::{
    CollaborativeRebootReason, CollaborativeRebootSchedulerRequestStream,
};
use fidl_fuchsia_sys2::SystemControllerMarker;
use fidl_fuchsia_system_state::{
    SystemPowerState, SystemStateTransitionRequest, SystemStateTransitionRequestStream,
};
use fuchsia_component::client;
use fuchsia_component::directory::{AsRefDirectory, Directory};
use fuchsia_component::server::ServiceFs;
use fuchsia_sync::Mutex;
use futures::channel::mpsc;
use futures::lock::Mutex as AMutex;
use futures::prelude::*;
use futures::select;
use std::pin::pin;
use std::sync::{Arc, LazyLock};
use std::time::Duration;
use {fidl_fuchsia_io as fio, fidl_fuchsia_power_system as fsystem, fuchsia_async as fasync};

mod collaborative_reboot;

// The amount of time that the shim will spend waiting for a manually trigger
// system shutdown to finish before forcefully restarting the system.
const MANUAL_SYSTEM_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(60 * 60);

enum IncomingRequest {
    SystemStateTransition(SystemStateTransitionRequestStream),
    Admin(AdminRequestStream),
    CollaborativeRebootInitiator(CollaborativeRebootInitiatorRequestStream),
    CollaborativeRebootScheduler(CollaborativeRebootSchedulerRequestStream),
    RebootMethodsWatcherRegister(RebootMethodsWatcherRegisterRequestStream),
}

pub async fn main(
    svc: impl Directory + AsRefDirectory + 'static,
    directory_request: ServerEnd<fio::DirectoryMarker>,
) -> Result<(), anyhow::Error> {
    println!("[shutdown-shim]: started");

    // Initialize the inspect framework.
    //
    // Note that shutdown-shim is a builtin component of ComponentManager, which
    // means we must setup the inspector in a non-conventional way:
    // * We must initialize the connection to the inspect sink relative to the
    // `svc` directory.
    // * We must not use the global `fuchsia_inspect::component::inspector()`;
    //   this instance instance is a singleton and would be shared with
    //   ComponentManager. Instead we declare our own local inspector.
    let inspector = fuchsia_inspect::Inspector::new(fuchsia_inspect::InspectorConfig::default());
    let (client, server) =
        fidl::endpoints::create_endpoints::<fidl_fuchsia_inspect::InspectSinkMarker>();
    // Note: The inspect server is detached, so we need not poll it.
    let _inspect_server_task = inspect_runtime::publish(
        &inspector,
        inspect_runtime::PublishOptions::default().on_inspect_sink_client(client),
    )
    .ok_or_else(|| format_err!("failed to initialize inspect framework"))?;
    svc.as_ref_directory()
        .open(
            fidl_fuchsia_inspect::InspectSinkMarker::PROTOCOL_NAME,
            fio::Flags::PROTOCOL_SERVICE,
            server.into_channel().into(),
        )
        .context("failed to connect to InspectSink")?;

    let mut service_fs = ServiceFs::new();
    service_fs.dir("svc").add_fidl_service(IncomingRequest::Admin);
    service_fs.dir("svc").add_fidl_service(IncomingRequest::RebootMethodsWatcherRegister);
    service_fs.dir("svc").add_fidl_service(IncomingRequest::SystemStateTransition);
    service_fs.dir("svc").add_fidl_service(IncomingRequest::CollaborativeRebootInitiator);
    service_fs.dir("svc").add_fidl_service(IncomingRequest::CollaborativeRebootScheduler);
    service_fs.serve_connection(directory_request).context("failed to serve outgoing namespace")?;

    let (abort_tx, mut abort_rx) = mpsc::unbounded::<()>();
    let (cr_state, cr_cancellations) = collaborative_reboot::new(&inspector);
    let ctx = ProgramContext {
        svc,
        abort_tx,
        collaborative_reboot: cr_state,
        shutdown_pending: Arc::new(AMutex::new(false)),
        shutdown_watcher: ShutdownWatcher::new(),
    };

    let shutdown_watcher = ctx.shutdown_watcher.clone();
    let mut service_fut = service_fs
        .for_each_concurrent(None, |request: IncomingRequest| async {
            match request {
                IncomingRequest::Admin(stream) => ctx.handle_admin_request(stream).await,
                IncomingRequest::RebootMethodsWatcherRegister(stream) => {
                    shutdown_watcher.clone().handle_reboot_register_request(stream).await;
                }
                IncomingRequest::SystemStateTransition(stream) => {
                    ctx.handle_system_state_transition(stream).await
                }
                IncomingRequest::CollaborativeRebootInitiator(stream) => {
                    ctx.collaborative_reboot.handle_initiator_requests(stream, &ctx).await
                }
                IncomingRequest::CollaborativeRebootScheduler(stream) => {
                    ctx.collaborative_reboot.handle_scheduler_requests(stream).await
                }
            }
        })
        .fuse();
    let collaborative_reboot_cancellation_fut = pin!(cr_cancellations.run());
    let mut collaborative_reboot_cancellation_fut = collaborative_reboot_cancellation_fut.fuse();
    let mut abort_fut = abort_rx.next().fuse();

    select! {
        () = service_fut => {},
        () = collaborative_reboot_cancellation_fut => unreachable!(),
        _ = abort_fut => {},
    };

    Err(format_err!("exited unexpectedly"))
}

struct ProgramContext<D: Directory + AsRefDirectory> {
    svc: D,
    abort_tx: mpsc::UnboundedSender<()>,
    collaborative_reboot: collaborative_reboot::State,

    /// Tracks the current shutdown request state. Used to ignore shutdown requests while a current
    /// request is being processed.
    shutdown_pending: Arc<AMutex<bool>>,

    shutdown_watcher: Arc<ShutdownWatcher>,
}

impl<D: Directory + AsRefDirectory> ProgramContext<D> {
    async fn handle_admin_request(&self, mut stream: AdminRequestStream) {
        while let Ok(Some(request)) = stream.try_next().await {
            match request {
                AdminRequest::PowerFullyOn { responder, .. } => {
                    let _ = responder.send(Err(zx::Status::NOT_SUPPORTED.into_raw()));
                }
                // TODO(https://fxbug.dev/385742868): Delete this method once
                // it's removed from the API.
                AdminRequest::Reboot { reason, responder } => {
                    let _reboot_control_lease = self.acquire_shutdown_control_lease().await;
                    let target_state = if reason == RebootReason::OutOfMemory {
                        SystemPowerState::RebootKernelInitiated
                    } else {
                        SystemPowerState::Reboot
                    };
                    set_system_power_state(target_state);
                    let res = self
                        .forward_command(
                            target_state,
                            Some(RebootReasons::from_deprecated(&reason)),
                            None,
                        )
                        .await;
                    let _ = responder.send(res.map_err(|s| s.into_raw()));
                }
                AdminRequest::PerformReboot { options, responder } => {
                    let res = self.perform_reboot(options).await;
                    let _ = responder.send(res.map_err(|s| s.into_raw()));
                }
                AdminRequest::RebootToBootloader { responder } => {
                    let _reboot_control_lease = self.acquire_shutdown_control_lease().await;
                    let target_state = SystemPowerState::RebootBootloader;
                    set_system_power_state(target_state);
                    let res = self.forward_command(target_state, None, None).await;
                    let _ = responder.send(res.map_err(|s| s.into_raw()));
                }
                AdminRequest::RebootToRecovery { responder } => {
                    let _reboot_control_lease = self.acquire_shutdown_control_lease().await;
                    let target_state = SystemPowerState::RebootRecovery;
                    set_system_power_state(target_state);
                    let res = self.forward_command(target_state, None, None).await;
                    let _ = responder.send(res.map_err(|s| s.into_raw()));
                }
                AdminRequest::Poweroff { responder } => {
                    let _reboot_control_lease = self.acquire_shutdown_control_lease().await;
                    let target_state = SystemPowerState::Poweroff;
                    set_system_power_state(target_state);
                    let res = self.forward_command(target_state, None, None).await;
                    let _ = responder.send(res.map_err(|s| s.into_raw()));
                }
                AdminRequest::SuspendToRam { responder } => {
                    let target_state = SystemPowerState::SuspendRam;
                    set_system_power_state(target_state);
                    let res = self.forward_command(target_state, None, None).await;
                    let _ = responder.send(res.map_err(|s| s.into_raw()));
                }
                AdminRequest::Mexec { responder, kernel_zbi, data_zbi } => {
                    let _reboot_control_lease = self.acquire_shutdown_control_lease().await;
                    let res = async move {
                        let target_state = SystemPowerState::Mexec;
                        {
                            // Duplicate the VMOs now, as forwarding the mexec request to power-manager
                            // will consume them.
                            let kernel_zbi =
                                kernel_zbi.duplicate_handle(zx::Rights::SAME_RIGHTS)?;
                            let data_zbi = data_zbi.duplicate_handle(zx::Rights::SAME_RIGHTS)?;

                            set_system_power_state(SystemPowerState::Mexec);
                            set_mexec_kernel_zbi(kernel_zbi);
                            set_mexec_data_zbi(data_zbi);
                        }

                        self.forward_command(
                            target_state,
                            None,
                            Some(AdminMexecRequest { kernel_zbi, data_zbi }),
                        )
                        .await
                    }
                    .await;
                    let _ = responder.send(res.map_err(|s| s.into_raw()));
                }
            }
        }
    }

    async fn handle_system_state_transition(&self, mut stream: SystemStateTransitionRequestStream) {
        while let Ok(Some(request)) = stream.try_next().await {
            match request {
                SystemStateTransitionRequest::GetTerminationSystemState { responder } => {
                    let state = (*SYSTEM_STATE).lock();
                    let _ = responder.send(state.power_state);
                }
                SystemStateTransitionRequest::GetMexecZbis { responder } => {
                    let mut state = (*SYSTEM_STATE).lock();
                    if state.power_state != SystemPowerState::Mexec {
                        let _ = responder.send(Err(zx::Status::BAD_STATE.into_raw()));
                        continue;
                    }
                    let kernel_zbi = std::mem::replace(
                        &mut state.mexec_kernel_zbi,
                        zx::Handle::invalid().into(),
                    );
                    let data_zbi =
                        std::mem::replace(&mut state.mexec_data_zbi, zx::Handle::invalid().into());
                    let _ = responder.send(Ok((kernel_zbi, data_zbi)));
                }
            }
        }
    }

    // A handler for the `Admin.PerformReboot` method.
    async fn perform_reboot(&self, options: RebootOptions) -> Result<(), zx::Status> {
        let _reboot_control_lease = self.acquire_shutdown_control_lease().await;
        let target_state = if options
            .reasons
            .as_ref()
            .is_some_and(|reasons| reasons.contains(&RebootReason2::OutOfMemory))
        {
            SystemPowerState::RebootKernelInitiated
        } else {
            SystemPowerState::Reboot
        };
        set_system_power_state(target_state);
        let reasons = match options.reasons {
            Some(reasons) => Some(RebootReasons(reasons)),
            None => None,
        };
        self.forward_command(target_state, reasons, None).await
    }

    async fn forward_command(
        &self,
        _fallback_state: SystemPowerState,
        reboot_reasons: Option<RebootReasons>,
        _mexec_request: Option<AdminMexecRequest>,
    ) -> Result<(), zx::Status> {
        // Return if shutdown is already pending
        {
            let mut shutdown_pending = self.shutdown_pending.lock().await;
            if *shutdown_pending {
                return Err(zx::Status::ALREADY_EXISTS);
            }
            *shutdown_pending = true;
        }

        if let Some(reasons) = reboot_reasons {
            self.shutdown_watcher.handle_system_shutdown_message(reasons).await;
        }

        self.drive_shutdown_manually().await;

        // We should block on fuchsia.sys.SystemController forever on this task, if
        // it returns something has gone wrong.
        eprintln!("[shutdown-shim]: we shouldn't still be running, crashing the system");
        Self::abort(self.abort_tx.clone()).await
    }

    async fn drive_shutdown_manually(&self) {
        eprintln!("[shutdown-shim]: driving shutdown manually");
        let abort_tx = self.abort_tx.clone();
        fasync::Task::spawn(async {
            fasync::Timer::new(MANUAL_SYSTEM_SHUTDOWN_TIMEOUT).await;
            // We shouldn't still be running at this point
            Self::abort(abort_tx).await;
        })
        .detach();

        if let Err(e) = self.initiate_component_shutdown().await {
            eprintln!(
                "[shutdown-shim]: error initiating component shutdown, system shutdown impossible: {e}"
            );
            // Recovery from this state is impossible. Exit with a non-zero exit code,
            // so our critical marking causes the system to forcefully restart.
            Self::abort(self.abort_tx.clone()).await;
        }
        eprintln!("[shutdown-shim]: manual shutdown successfully initiated");
    }

    async fn initiate_component_shutdown(&self) -> Result<(), anyhow::Error> {
        let system_controller_client = self
            .connect_to_protocol::<SystemControllerMarker>()
            .context("error connecting to component_manager")?;

        println!("[shutdown-shim]: calling system_controller_client.Shutdown()");
        system_controller_client.shutdown().await.context("failed to initiate shutdown")
    }

    async fn acquire_shutdown_control_lease(&self) -> Option<zx::EventPair> {
        let res = async {
            let activity_governor = self
                .connect_to_protocol::<fsystem::ActivityGovernorMarker>()
                .context("error connecting to system_activity_governor")?;
            activity_governor
                .take_wake_lease("shutdown_control")
                .await
                .context("failed to take wake lease")
        }
        .await;
        res.map_err(|e| {
            eprintln!("[shutdown-shim]: {e}");
            ()
        })
        .ok()
    }

    /// Cause the program to terminate.
    async fn abort(mut abort_tx: mpsc::UnboundedSender<()>) -> ! {
        let _ = abort_tx.send(()).await;
        std::future::pending::<()>().await;
        unreachable!();
    }

    fn connect_to_protocol<P: DiscoverableProtocolMarker>(
        &self,
    ) -> Result<P::Proxy, anyhow::Error> {
        client::connect_to_protocol_at_dir_root::<P>(&self.svc)
    }
}

impl<D: Directory + AsRefDirectory> collaborative_reboot::RebootActuator for ProgramContext<D> {
    async fn perform_reboot(
        &self,
        reasons: Vec<CollaborativeRebootReason>,
    ) -> Result<(), zx::Status> {
        // Transform the reasons, and dispatch the request along the standard
        // reboot pipeline.
        let reasons = reasons
            .into_iter()
            .map(|reason| match reason {
                CollaborativeRebootReason::NetstackMigration => RebootReason2::NetstackMigration,
                CollaborativeRebootReason::SystemUpdate => RebootReason2::SystemUpdate,
            })
            .collect();
        self.perform_reboot(RebootOptions { reasons: Some(reasons), ..Default::default() }).await
    }
}

struct SystemState {
    power_state: SystemPowerState,
    mexec_kernel_zbi: zx::Vmo,
    mexec_data_zbi: zx::Vmo,
}

impl SystemState {
    fn new() -> Self {
        Self {
            power_state: SystemPowerState::FullyOn,
            mexec_kernel_zbi: zx::Handle::invalid().into(),
            mexec_data_zbi: zx::Handle::invalid().into(),
        }
    }
}

static SYSTEM_STATE: LazyLock<Mutex<SystemState>> =
    LazyLock::new(|| Mutex::new(SystemState::new()));

fn set_system_power_state(new: SystemPowerState) {
    let mut s = (*SYSTEM_STATE).lock();
    s.power_state = new;
}

fn set_mexec_kernel_zbi(new: zx::Vmo) {
    let mut s = (*SYSTEM_STATE).lock();
    s.mexec_kernel_zbi = new;
}

fn set_mexec_data_zbi(new: zx::Vmo) {
    let mut s = (*SYSTEM_STATE).lock();
    s.mexec_data_zbi = new;
}
