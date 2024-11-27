// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Error;
use fidl::endpoints::create_proxy;
use fidl::prelude::*;
use fuchsia_component::server as fserver;
use fuchsia_component_test::LocalComponentHandles;
use futures::channel::mpsc;
use futures::future::BoxFuture;
use futures::{FutureExt, StreamExt, TryFutureExt, TryStreamExt};
use tracing::{info, warn};
use {
    fidl_fuchsia_hardware_power_statecontrol as fstatecontrol, fidl_fuchsia_io as fio,
    fidl_fuchsia_power_system as fsystem, fidl_fuchsia_sys2 as fsys, fuchsia_async as fasync,
};

pub fn new_mocks_provider(
    is_power_framework_available: bool,
) -> (
    impl Fn(LocalComponentHandles) -> BoxFuture<'static, Result<(), anyhow::Error>>
        + Sync
        + Send
        + 'static,
    mpsc::UnboundedReceiver<Signal>,
) {
    let (send_signals, recv_signals) = mpsc::unbounded();

    let mock = move |handles: LocalComponentHandles| {
        run_mocks(send_signals.clone(), handles, is_power_framework_available).boxed()
    };

    (mock, recv_signals)
}

async fn run_mocks(
    send_signals: mpsc::UnboundedSender<Signal>,
    handles: LocalComponentHandles,
    is_power_framework_available: bool,
) -> Result<(), Error> {
    let mut fs = fserver::ServiceFs::new();

    if is_power_framework_available {
        let send_sag_signals = send_signals.clone();
        fs.dir("svc").add_fidl_service(move |stream| {
            fasync::Task::spawn(run_activity_governor(send_sag_signals.clone(), stream)).detach();
        });
    }

    let send_admin_signals = send_signals.clone();
    fs.dir("svc").add_fidl_service(move |stream| {
        fasync::Task::spawn(run_statecontrol_admin(send_admin_signals.clone(), stream)).detach();
    });

    let send_sys2_signals = send_signals.clone();
    fs.dir("svc").add_fidl_service(move |stream| {
        fasync::Task::spawn(run_sys2_system_controller(send_sys2_signals.clone(), stream)).detach();
    });

    // The black_hole directory points to a channel we will never answer, so that capabilities
    // provided from this directory will behave similarly to as if they were from an unlaunched
    // component.
    let (proxy, _server_end) = create_proxy::<fio::DirectoryMarker>();
    fs.add_remote("black_hole", proxy);

    fs.serve_connection(handles.outgoing_dir)?;
    fs.collect::<()>().await;

    Ok(())
}

#[derive(Debug, PartialEq)]
pub enum Admin {
    Reboot(fstatecontrol::RebootReason),
    RebootToBootloader,
    RebootToRecovery,
    Poweroff,
    Mexec,
    SuspendToRam,
}

#[derive(Debug)]
pub enum LeaseState {
    Acquired,
    Dropped,
}

#[derive(Debug)]
pub enum Signal {
    Statecontrol(Admin),
    Sys2Shutdown(
        // The responder is held here to keep the current request and request stream alive, but is
        // not actively read, hence the 'dead_code' attribute.
        #[allow(dead_code)] fsys::SystemControllerShutdownResponder,
    ),
    ShutdownControlLease(LeaseState),
}

async fn run_activity_governor(
    send_signals: mpsc::UnboundedSender<Signal>,
    mut stream: fsystem::ActivityGovernorRequestStream,
) {
    info!("new connection to {}", fsystem::ActivityGovernorMarker::DEBUG_NAME);

    while let Ok(Some(request)) = stream.try_next().await {
        match request {
            fsystem::ActivityGovernorRequest::TakeWakeLease { name: _, responder } => {
                let (client_token, server_token) = fsystem::LeaseToken::create();
                let send_signals2 = send_signals.clone();
                fasync::Task::spawn(async move {
                    fasync::OnSignals::new(&server_token, zx::Signals::EVENTPAIR_PEER_CLOSED)
                        .await
                        .unwrap();
                    send_signals2
                        .unbounded_send(Signal::ShutdownControlLease(LeaseState::Dropped))
                        .expect("receiver dropped");
                })
                .detach();
                send_signals
                    .unbounded_send(Signal::ShutdownControlLease(LeaseState::Acquired))
                    .expect("receiver dropped");
                responder.send(client_token).unwrap();
            }
            _ => unreachable!("Unexpected request to ActivityGovernor"),
        }
    }
}

async fn run_statecontrol_admin(
    send_signals: mpsc::UnboundedSender<Signal>,
    mut stream: fstatecontrol::AdminRequestStream,
) {
    info!("new connection to {}", fstatecontrol::AdminMarker::DEBUG_NAME);
    async move {
        match stream.try_next().await? {
            Some(fstatecontrol::AdminRequest::PowerFullyOn { responder }) => {
                // PowerFullyOn is unsupported
                responder.send(Err(zx::Status::NOT_SUPPORTED.into_raw()))?;
            }
            Some(fstatecontrol::AdminRequest::Reboot { reason, responder }) => {
                info!("Reboot called");
                send_signals.unbounded_send(Signal::Statecontrol(Admin::Reboot(reason)))?;
                responder.send(Ok(()))?;
            }
            Some(fstatecontrol::AdminRequest::RebootToBootloader { responder }) => {
                info!("RebootToBootloader called");
                send_signals.unbounded_send(Signal::Statecontrol(Admin::RebootToBootloader))?;
                responder.send(Ok(()))?;
            }
            Some(fstatecontrol::AdminRequest::RebootToRecovery { responder }) => {
                info!("RebootToRecovery called");
                send_signals.unbounded_send(Signal::Statecontrol(Admin::RebootToRecovery))?;
                responder.send(Ok(()))?;
            }
            Some(fstatecontrol::AdminRequest::Poweroff { responder }) => {
                info!("Poweroff called");
                send_signals.unbounded_send(Signal::Statecontrol(Admin::Poweroff))?;
                responder.send(Ok(()))?;
            }
            Some(fstatecontrol::AdminRequest::Mexec { responder, .. }) => {
                info!("Mexec called");
                send_signals.unbounded_send(Signal::Statecontrol(Admin::Mexec))?;
                responder.send(Ok(()))?;
            }
            Some(fstatecontrol::AdminRequest::SuspendToRam { responder }) => {
                info!("SuspendToRam called");
                send_signals.unbounded_send(Signal::Statecontrol(Admin::SuspendToRam))?;
                responder.send(Ok(()))?;
            }
            _ => (),
        }
        Ok(())
    }
    .unwrap_or_else(|e: anyhow::Error| {
        // Note: the shim checks liveness by writing garbage data on its first connection and
        // observing PEER_CLOSED, so we're expecting this warning to happen once.
        warn!("couldn't run {}: {:?}", fstatecontrol::AdminMarker::DEBUG_NAME, e);
    })
    .await
}

async fn run_sys2_system_controller(
    send_signals: mpsc::UnboundedSender<Signal>,
    mut stream: fsys::SystemControllerRequestStream,
) {
    info!("new connection to {}", fsys::SystemControllerMarker::DEBUG_NAME);
    async move {
        match stream.try_next().await? {
            Some(fsys::SystemControllerRequest::Shutdown { responder }) => {
                info!("Shutdown called");
                // Send the responder out with the signal.
                // The responder keeps the current request and the request stream alive.
                send_signals.unbounded_send(Signal::Sys2Shutdown(responder))?;
            }
            _ => (),
        }
        Ok(())
    }
    .unwrap_or_else(|e: anyhow::Error| {
        panic!("couldn't run {}: {:?}", fsys::SystemControllerMarker::PROTOCOL_NAME, e);
    })
    .await
}
