// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{format_err, Context};
use fidl::endpoints::{self, DiscoverableProtocolMarker, ServerEnd};
use fidl::HandleBased;
use fidl_fuchsia_hardware_power_statecontrol::{
    AdminMarker, AdminMexecRequest, AdminProxy, AdminRequest, AdminRequestStream,
    RebootMethodsWatcherRegisterMarker, RebootOptions, RebootReason, RebootReason2,
    RebootWatcherMarker, RebootWatcherRequest,
};
use fidl_fuchsia_sys2::SystemControllerMarker;
use fidl_fuchsia_system_state::{
    SystemPowerState, SystemStateTransitionRequest, SystemStateTransitionRequestStream,
};
use fuchsia_component::client;
use fuchsia_component::directory::{AsRefDirectory, Directory};
use fuchsia_component::server::ServiceFs;
use futures::channel::mpsc;
use futures::prelude::*;
use futures::select;
use std::pin::pin;
use std::sync::{LazyLock, Mutex};
use std::time::Duration;
use {fidl_fuchsia_io as fio, fidl_fuchsia_power_system as fsystem, fuchsia_async as fasync};

// The amount of time that the shim will spend trying to connect to
// power_manager before giving up.
// TODO(https://fxbug.dev/42131944): increase this timeout
const SERVICE_CONNECTION_TIMEOUT: Duration = Duration::from_secs(2);

// The amount of time that the shim will spend waiting for a manually trigger
// system shutdown to finish before forcefully restarting the system.
const MANUAL_SYSTEM_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(60 * 60);

enum IncomingRequest {
    SystemStateTransition(SystemStateTransitionRequestStream),
    Admin(AdminRequestStream),
}

pub async fn main(
    svc: impl Directory + AsRefDirectory + Send + Sync + 'static,
    directory_request: ServerEnd<fio::DirectoryMarker>,
) -> Result<(), anyhow::Error> {
    println!("[shutdown-shim]: started");

    let mut service_fs = ServiceFs::new();
    service_fs.dir("svc").add_fidl_service(IncomingRequest::Admin);
    service_fs.dir("svc").add_fidl_service(IncomingRequest::SystemStateTransition);
    service_fs.serve_connection(directory_request).context("failed to serve outgoing namespace")?;

    let (abort_tx, mut abort_rx) = mpsc::unbounded::<()>();
    let ctx = ProgramContext { svc, abort_tx };
    let mut service_fut = service_fs
        .for_each_concurrent(None, |request: IncomingRequest| async {
            match request {
                IncomingRequest::Admin(stream) => ctx.handle_admin_request(stream).await,
                IncomingRequest::SystemStateTransition(stream) => {
                    ctx.handle_system_state_transition(stream).await
                }
            }
        })
        .fuse();
    let reboot_watcher_fut = pin!(async {
        ctx.run_reboot_watcher().await;
        // Don't terminate the program if the reboot watcher finishes.
        std::future::pending::<()>().await;
    });
    let mut reboot_watcher_fut = reboot_watcher_fut.fuse();
    let mut abort_fut = abort_rx.next().fuse();
    select! {
        () = service_fut => {},
        () = reboot_watcher_fut => unreachable!(),
        _ = abort_fut => {},
    };

    Err(format_err!("exited unexpectedly"))
}

struct ProgramContext<D: Directory + AsRefDirectory + Send + Sync> {
    svc: D,
    abort_tx: mpsc::UnboundedSender<()>,
}

impl<D: Directory + AsRefDirectory + Send + Sync> ProgramContext<D> {
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
                        .forward_command(target_state, Some(RebootArguments::Reboot(reason)), None)
                        .await;
                    let _ = responder.send(res.map_err(|s| s.into_raw()));
                }
                AdminRequest::PerformReboot { options, responder } => {
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
                    let res = self
                        .forward_command(
                            target_state,
                            Some(RebootArguments::PerformReboot(options)),
                            None,
                        )
                        .await;
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
                    let state = (*SYSTEM_STATE).lock().unwrap();
                    let _ = responder.send(state.power_state);
                }
                SystemStateTransitionRequest::GetMexecZbis { responder } => {
                    let mut state = (*SYSTEM_STATE).lock().unwrap();
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

    async fn run_reboot_watcher(&self) {
        // TODO(https://fxbug.dev/368440548) Fix deeper problems and run this
        // immediately.
        fasync::Timer::new(Duration::from_secs(15)).await;
        match self.connect_to_protocol::<RebootMethodsWatcherRegisterMarker>() {
            Ok(local) => {
                let (client, watcher) = endpoints::create_endpoints::<RebootWatcherMarker>();
                match local.register_watcher(client).await {
                    Ok(()) => eprintln!("[shutdown-shim]: RegisterWithAck succeeded"),
                    Err(e) => {
                        eprintln!("[shutdown-shim]: RegisterWithAck failed: {e}");
                        return;
                    }
                }
                let mut stream = watcher.into_stream();
                while let Ok(Some(request)) = stream.try_next().await {
                    match request {
                        RebootWatcherRequest::OnReboot { options, responder } => {
                            let RebootOptions { reasons, __source_breaking: _ } = options;
                            // Ignore other reasons because they are from shutdown-shim and are
                            // processed already.
                            if reasons.is_some_and(|r| r.contains(&RebootReason2::HighTemperature))
                            {
                                set_system_power_state(SystemPowerState::Reboot);
                            }
                            let _ = responder.send();
                        }
                    }
                }
            }
            Err(_) => {
                // It's fine this is not available in bootstrap
                eprintln!("[shutdown-shim]: Not able to connect to RebootMethodsWatcherRegister");
            }
        }
    }

    async fn forward_command(
        &self,
        fallback_state: SystemPowerState,
        reboot_arguments: Option<RebootArguments>,
        mexec_request: Option<AdminMexecRequest>,
    ) -> Result<(), zx::Status> {
        let local = self.connect_to_protocol_with_timeout::<AdminMarker>();
        match local {
            Ok(local) => {
                eprintln!("[shutdown-shim]: forwarding command {fallback_state:?}");
                match self
                    .send_command(local, fallback_state, reboot_arguments, mexec_request)
                    .await
                {
                    Ok(()) => return Ok(()),
                    e @ Err(zx::Status::UNAVAILABLE | zx::Status::NOT_SUPPORTED) => {
                        // Power manager may decide not to support suspend. We should respect that and
                        // not attempt to suspend manually.
                        if fallback_state == SystemPowerState::SuspendRam {
                            return e;
                        }
                        eprintln!("[shutdown-shim]: failed to forward command to power_manager");
                    }
                    e => return e,
                }
            }
            Err(e) => {
                eprintln!(
                    "[shutdown-shim]: failed to connect to power_manager to forward command: {e}"
                );
            }
        }

        self.drive_shutdown_manually().await;

        // We should block on fuchsia.sys.SystemController forever on this task, if
        // it returns something has gone wrong.
        eprintln!("[shutdown-shim]: we shouldn't still be running, crashing the system");
        Self::abort(self.abort_tx.clone()).await
    }

    async fn send_command(
        &self,
        statecontrol_client: AdminProxy,
        fallback_state: SystemPowerState,
        reboot_arguments: Option<RebootArguments>,
        mexec_request: Option<AdminMexecRequest>,
    ) -> Result<(), zx::Status> {
        match fallback_state {
            SystemPowerState::Reboot | SystemPowerState::RebootKernelInitiated => {
                if let None = reboot_arguments {
                    eprintln!("[shutdown-shim]: internal error, no arguments for reboot");
                    return Err(zx::Status::INTERNAL);
                }
                match reboot_arguments.unwrap() {
                    RebootArguments::Reboot(reason) => statecontrol_client.reboot(reason).await,
                    RebootArguments::PerformReboot(options) => {
                        statecontrol_client.perform_reboot(&options).await
                    }
                }
                .map_err(|_| zx::Status::UNAVAILABLE)?
                .map_err(zx::Status::from_raw)
            }
            SystemPowerState::RebootBootloader => statecontrol_client
                .reboot_to_bootloader()
                .await
                .map_err(|_| zx::Status::UNAVAILABLE)?
                .map_err(zx::Status::from_raw),
            SystemPowerState::RebootRecovery => statecontrol_client
                .reboot_to_recovery()
                .await
                .map_err(|_| zx::Status::UNAVAILABLE)?
                .map_err(zx::Status::from_raw),
            SystemPowerState::Poweroff => statecontrol_client
                .poweroff()
                .await
                .map_err(|_| zx::Status::UNAVAILABLE)?
                .map_err(zx::Status::from_raw),
            SystemPowerState::Mexec => {
                if let None = mexec_request {
                    eprintln!("[shutdown-shim]: internal error, no reason for mexec");
                    return Err(zx::Status::INTERNAL);
                }
                let AdminMexecRequest { kernel_zbi, data_zbi } = mexec_request.unwrap();
                statecontrol_client
                    .mexec(kernel_zbi, data_zbi)
                    .await
                    .map_err(|_| zx::Status::UNAVAILABLE)?
                    .map_err(zx::Status::from_raw)
            }
            SystemPowerState::SuspendRam => statecontrol_client
                .suspend_to_ram()
                .await
                .map_err(|_| zx::Status::UNAVAILABLE)?
                .map_err(zx::Status::from_raw),
            SystemPowerState::FullyOn => Err(zx::Status::INTERNAL),
        }
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

    fn connect_to_protocol_with_timeout<P: DiscoverableProtocolMarker>(
        &self,
    ) -> Result<P::Proxy, anyhow::Error> {
        // TODO: this needs to actually set the timeout.
        let (channel, server) = zx::Channel::create();
        self.svc.open(P::PROTOCOL_NAME, fio::Flags::empty(), server.into())?;
        // We want to use the zx_channel_call syscall directly here, because there's
        // no way to set the timeout field on the call using the FIDL bindings.
        let garbage_data: [u8; 6] = [0, 1, 2, 3, 4, 5];
        let mut handles: [zx::Handle; 0] = [];
        let mut buf = zx::MessageBuf::new();
        let timeout = zx::MonotonicInstant::after(SERVICE_CONNECTION_TIMEOUT.into());
        match channel.call(timeout, &garbage_data, &mut handles, &mut buf) {
            Err(zx::Status::PEER_CLOSED) => self.connect_to_protocol::<P>(),
            Err(zx::Status::TIMED_OUT) => {
                Err(format_err!("timed out connecting to {}", P::DEBUG_NAME))
            }
            Err(s) => Err(format_err!("unexpected response from {}: {s}", P::DEBUG_NAME)),
            Ok(()) => Err(format_err!("unexpected ok from {}", P::DEBUG_NAME)),
        }
    }
}

// The arguments for various reboot methods available on the `Admin` protocol.
enum RebootArguments {
    // Corresponds to the deprecated `Admin.Reboot` method.
    Reboot(RebootReason),
    // Corresponds to the `Admin.PerformReboot` method.
    PerformReboot(RebootOptions),
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
    let mut s = (*SYSTEM_STATE).lock().unwrap();
    s.power_state = new;
}

fn set_mexec_kernel_zbi(new: zx::Vmo) {
    let mut s = (*SYSTEM_STATE).lock().unwrap();
    s.mexec_kernel_zbi = new;
}

fn set_mexec_data_zbi(new: zx::Vmo) {
    let mut s = (*SYSTEM_STATE).lock().unwrap();
    s.mexec_data_zbi = new;
}
