// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::Error,
    fidl::{endpoints::create_proxy, prelude::*},
    fidl_fuchsia_hardware_power_statecontrol as fstatecontrol, fidl_fuchsia_io as fio,
    fidl_fuchsia_power_broker as fbroker, fidl_fuchsia_power_system as fsystem,
    fidl_fuchsia_sys2 as fsys, fuchsia_async as fasync,
    fuchsia_component::server as fserver,
    fuchsia_component_test::LocalComponentHandles,
    fuchsia_zircon as zx,
    futures::{channel::mpsc, future::BoxFuture, FutureExt, StreamExt, TryFutureExt, TryStreamExt},
    power_broker_client::{basic_update_fn_factory, run_power_element, PowerElementContext},
    std::sync::Arc,
    tracing::{info, warn},
};

struct ActivityGovernor {
    send_signals: mpsc::UnboundedSender<Signal>,
    wake_handling: PowerElementContext,
}
impl ActivityGovernor {
    async fn new(
        topology: &fbroker::TopologyProxy,
        send_signals: mpsc::UnboundedSender<Signal>,
    ) -> Arc<Self> {
        let wake_handling = PowerElementContext::builder(
            topology,
            "wake_handling",
            &[
                fsystem::WakeHandlingLevel::Inactive.into_primitive(),
                fsystem::WakeHandlingLevel::Active.into_primitive(),
            ],
        )
        .build()
        .await
        .unwrap();

        Arc::new(Self { send_signals, wake_handling })
    }

    async fn run(&self) {
        let update_fn = Arc::new(basic_update_fn_factory(&self.wake_handling));
        let send_signals = self.send_signals.clone();

        run_power_element(
            self.wake_handling.name(),
            &self.wake_handling.required_level,
            fsystem::WakeHandlingLevel::Inactive.into_primitive(),
            None,
            Box::new(move |new_power_level: fbroker::PowerLevel| {
                let update_fn = update_fn.clone();
                let send_signals = send_signals.clone();

                async move {
                    update_fn(new_power_level).await;

                    if new_power_level == 0 {
                        send_signals
                            .unbounded_send(Signal::ShutdownControlLease(LeaseState::Dropped))
                            .unwrap();
                    } else {
                        send_signals
                            .unbounded_send(Signal::ShutdownControlLease(LeaseState::Acquired))
                            .unwrap();
                    }
                }
                .boxed_local()
            }),
        )
        .await;
    }
}

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
        let topology = handles.connect_to_protocol::<fbroker::TopologyMarker>()?;
        let sag = ActivityGovernor::new(&topology, send_signals.clone()).await;
        let sag2 = sag.clone();
        fasync::Task::local(async move {
            sag2.run().await;
        })
        .detach();

        fs.dir("svc").add_fidl_service(move |stream| {
            fasync::Task::spawn(run_activity_governor(sag.clone(), stream)).detach();
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
    let (proxy, _server_end) = create_proxy::<fio::DirectoryMarker>()?;
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
        // TODO(https://fxbug.dev/332392008): Remove or explain #[allow(dead_code)].
        #[allow(dead_code)] fsys::SystemControllerShutdownResponder,
    ),
    ShutdownControlLease(LeaseState),
}

async fn run_activity_governor(
    sag: Arc<ActivityGovernor>,
    mut stream: fsystem::ActivityGovernorRequestStream,
) {
    info!("new connection to {}", fsystem::ActivityGovernorMarker::DEBUG_NAME);

    while let Ok(Some(request)) = stream.try_next().await {
        match request {
            fsystem::ActivityGovernorRequest::GetPowerElements { responder } => {
                responder
                    .send(fsystem::PowerElements {
                        wake_handling: Some(fsystem::WakeHandling {
                            active_dependency_token: Some(
                                sag.wake_handling.active_dependency_token(),
                            ),
                            ..Default::default()
                        }),
                        ..Default::default()
                    })
                    .unwrap();
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
