// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! A component that serves a FIDL API `fuchsia.hardware.hrtimer/Device`.
//!
//! Used in hermetic integration tests that exercise hrtimer functionality. It
//! implements the wake alarm expiry behavior - but not suspension - and
//! interacts with the power subsystem as a real driver would.
//!
//! The intention is for this implementation to be high fidelity with respect to
//! realistic high-resolution timer drivers.  It implements the same API, and is
//! mostly true to the hrtimer behavior. Some fidelity concessions are made which
//! may be removed in the future, such as limited support for multiple timers,
//! and limited support for different timer resolutions.
//!
//! Start by calling [FakeHrtimerServer::new], followed by [FakeHrtimerServer::serve].

use anyhow::{Context, Result};
use fidl::endpoints::create_proxy;
use fidl::AsHandleRef;
use fuchsia_component::client::connect_to_protocol;
use fuchsia_component::server::ServiceFs;
use futures::channel::mpsc;
use futures::{select, SinkExt, StreamExt};
use scopeguard::defer;
use std::cell::RefCell;
use std::collections::HashMap;
use std::pin::{pin, Pin};
use std::rc::Rc;
use {
    fidl_fuchsia_hardware_hrtimer as ffhh, fidl_fuchsia_power_broker as ffpb,
    fidl_fuchsia_power_system as fps, fuchsia_async as fasync,
};

/// The timer's resolution. The FIDL API supports multiple resolutions, but we
/// limit support in the fake to one resolution only. Actual hardware can be much
/// coarser at 1s resolution, for example, yielding differences between tests and
/// actual behavior.
const TIMER_RESOLUTION: zx::BootDuration = zx::BootDuration::from_millis(1);

/// Max ticks that hrtimers supported by this fake can be programmed to. This
/// is a generous upper bound.
const MAX_TICKS: u64 = 10_000_000_000;

/// The maximum number of timers that this fake hrtimer supports. Usually no
/// more than 1 is required.
const MAX_TIMERS: usize = 2;

enum Incoming {
    Hrtimer(ffhh::ServiceRequest),
}

#[derive(Debug)]
struct Timer {
    /// The timer's task - waits until a timeout expires, then triggers.
    task: fasync::Task<()>,
    /// Send to this channel to cancel the above `task`.
    canceler: mpsc::Sender<()>,
}

/// Interior mutable state of FakeHrtimerServer.
#[derive(Debug)]
struct Inner {
    timers: HashMap<u64, Timer>,
    events: HashMap<u64, zx::Event>,
}

/// The state of the hrtimer server.
#[derive(Debug)]
struct FakeHrtimerServer {
    lessor_proxy: ffpb::LessorProxy,
    _element_control_proxy: ffpb::ElementControlProxy,
    _current_level_proxy: ffpb::CurrentLevelProxy,
    // Max timer ID supported. Supported timer IDs are 1..max_timers.
    max_timers: usize,
    // Ensure inner is not borrowed across `.await` points.
    inner: RefCell<Inner>,
}

// Computes an alarm deadline from current time, resolution and ticks
fn as_deadline(resolution: ffhh::Resolution, ticks: u64) -> zx::BootInstant {
    let timeout = match resolution {
        ffhh::Resolution::Duration(d) => {
            // We assume that overflow here is unlikely.
            zx::BootDuration::from_nanos(d * ticks as i64)
        }
        _ => zx::BootDuration::ZERO,
    };
    fasync::BootInstant::after(timeout).into()
}

impl FakeHrtimerServer {
    /// Create a new [FakeHrtimerServer].
    pub fn new(
        max_timers: usize,
        lessor_proxy: ffpb::LessorProxy,
        _element_control_proxy: ffpb::ElementControlProxy,
        _current_level_proxy: ffpb::CurrentLevelProxy,
    ) -> Rc<Self> {
        let inner = RefCell::new(Inner { timers: HashMap::new(), events: HashMap::new() });
        Rc::new(Self {
            lessor_proxy,
            _element_control_proxy,
            _current_level_proxy,
            inner,
            max_timers,
        })
    }

    /// Returns a closure that waits for expiry of a given timer.
    ///
    /// # Args
    /// - `notification`: a function that will be notified of the result of waiting
    ///   on a timer.  It is passed the result of the wait which must be propagated
    ///   to the caller.
    /// - `canceler`: a channel used to stop a timer early. Sending something on this
    ///   channel will stop the wait for the timer to expire, and will issue the
    ///   appropriate cancelation response to the FIDL API caller.
    fn get_timer_closure<F>(
        self: &Rc<Self>,
        deadline: zx::BootInstant,
        id: u64,
        mut canceler: mpsc::Receiver<()>,
        notification: F,
    ) -> Pin<Box<impl futures::Future<Output = ()>>>
    where
        F: FnOnce(
            Result<(), ffhh::DriverError>,
        ) -> Pin<Box<dyn futures::Future<Output = Result<()>>>>,
    {
        let self_clone = self.clone();
        let fut = async move {
            let mut timer_fut = pin!(fasync::Timer::new(deadline));
            let result = select! {
                _ = canceler.next() => {
                    log::debug!("timer {id} CANCELED");
                    Err(ffhh::DriverError::Canceled)
                },
                _ = timer_fut => {
                    log::debug!("timer {id} EXPIRED");
                    Ok(())
                },
            };
            if let Err(err) = notification(result).await {
                log::error!("while triggering an existing timer: {err:?}, can not recover.");
            };
            {
                let mut guard = self_clone.inner.borrow_mut();
                guard.timers.remove(&id);
                // For alarms that have a bound event, signal the event.
                guard.events.remove(&id).map(|v| {
                    v.as_handle_ref().signal_handle(zx::Signals::NONE, zx::Signals::EVENT_SIGNALED)
                });
            }
        };
        log::debug!("timer {id} STARTED with deadline {deadline:?}");
        Box::pin(fut)
    }

    /// Schedules a new wake alarm with the given properties.
    ///
    /// # Args
    /// - `responder_fn`: a function called to notify the appropriate FIDL
    ///   responder when the alarm expires. When called, the function is passed
    ///   the result that must be propagated back to the client.
    async fn schedule_wake_alarm<F>(
        self: &Rc<Self>,
        id: u64,
        resolution: ffhh::Resolution,
        ticks: u64,
        responder_fn: F,
    ) -> Result<()>
    where
        F: FnOnce(Result<fps::LeaseToken, ffhh::DriverError>) -> Result<()> + 'static,
    {
        let deadline = as_deadline(resolution, ticks);
        let self_clone = self.clone();
        let (canceler, rcv) = mpsc::channel(1);
        let notification = |result| -> Pin<Box<dyn futures::Future<Output = Result<()>>>> {
            Box::pin(async move {
                match result {
                    Err(err) => {
                        let _result = responder_fn(Err(err))?;
                    }
                    Ok(_) => {
                        // Lease is held until we respond to the FIDL call.
                        let _lease = self_clone
                            .lessor_proxy
                            .lease(ffpb::BinaryPowerLevel::On.into_primitive())
                            .await
                            .map_err(|err| anyhow::anyhow!("lease FIDL error: {err:?}"))?
                            .map_err(|err| anyhow::anyhow!("lease error: {err:?}"))?;

                        // The original C++ implementation also created fake tokens.
                        // While this is enough for tests, at some point we may want to
                        // improve the fidelity of this operation.
                        let (_local_wake, remote_wake) = zx::EventPair::create();
                        let _result = responder_fn(Ok(remote_wake))?;
                    }
                }

                Ok(())
            })
        };
        let task = fasync::Task::local(self.get_timer_closure(deadline, id, rcv, notification));
        self.inner.borrow_mut().timers.insert(id, Timer { task, canceler });
        Ok(())
    }

    /// Returns an appropriate error if a start-like FIDL request is somehow not
    /// what is expected.
    ///
    /// The conditions checked are self-explanatory in code.  An error code is
    /// returned in case of request problems, and an error message is logged
    /// for debugging purposes as a side effect.
    fn validate_start_request(
        self: &Rc<Self>,
        id: u64,
        resolution: &ffhh::Resolution,
        ticks: u64,
    ) -> Result<(), ffhh::DriverError> {
        if ticks > MAX_TICKS {
            log::error!("ticks value not supported: {ticks}");
            return Err(ffhh::DriverError::InvalidArgs);
        }
        if id as usize > self.max_timers {
            log::error!("requested to start timer {id}, which is not a valid timer ID.");
            return Err(ffhh::DriverError::InvalidArgs);
        }
        if let Some(_) = self.inner.borrow().timers.get(&id) {
            log::error!("requested to start timer {id}, which is already running.");
            return Err(ffhh::DriverError::BadState);
        }
        match resolution {
            ffhh::Resolution::Duration(d) => {
                // We currently support only one resolution. If more are needed, it will require
                // a code change.
                if *d as i64 != TIMER_RESOLUTION.into_nanos() {
                    log::error!("requested to start timer {id}, with unsupported resolution: {resolution:?}");
                    return Err(ffhh::DriverError::InvalidArgs);
                }
            }
            _ => {
                log::error!(
                    "requested to start timer {id}, with unsupported resolution: {resolution:?}"
                );
                return Err(ffhh::DriverError::InvalidArgs);
            }
        }
        Ok(())
    }

    /// Serve a single connection to the hrtimer device by a single client.
    async fn serve(self: Rc<Self>, mut stream: ffhh::DeviceRequestStream) -> Result<()> {
        let mut num_requests: usize = 0;
        while let Some(request) = stream.next().await {
            num_requests += 1;
            log::debug!("request[{num_requests}]: {request:?}");
            match request {
                // The support for different timer properties is limited for now.
                Ok(ffhh::DeviceRequest::GetProperties { responder, .. }) => {
                    let _ = responder
                        .send(ffhh::Properties {
                            timers_properties: Some(vec![ffhh::TimerProperties {
                                id: Some(1),
                                supported_resolutions: Some(vec![ffhh::Resolution::Duration(
                                    TIMER_RESOLUTION.into_nanos(),
                                )]),
                                max_ticks: Some(MAX_TICKS),
                                supports_event: Some(true),
                                supports_wait: Some(true),
                                supports_read: Some(false),

                                ..Default::default()
                            }]),

                            ..Default::default()
                        })
                        .context("while responding to GetProperties")?;
                }

                Ok(ffhh::DeviceRequest::Start { id, resolution, ticks, responder }) => {
                    let validation = self.validate_start_request(id, &resolution, ticks);
                    if let Err(err) = validation {
                        let _result = responder.send(Err(err))?;
                    } else {
                        let (canceler, rcv) = mpsc::channel(1);
                        let deadline = as_deadline(resolution, ticks);
                        let task = fasync::Task::local(self.get_timer_closure(
                            deadline,
                            id,
                            rcv,
                            // Executed when the alarm is expired or canceled,
                            // and needs notification. The Start FIDL method does
                            // not need anything done here, since notification is
                            // via the event passed in from SetEvent.
                            |_| Box::pin(async { Ok(()) }),
                        ));
                        self.inner.borrow_mut().timers.insert(id, Timer { task, canceler });
                        responder.send(Ok(()))?;
                        log::debug!("started timer {id}");
                    }
                }

                Ok(ffhh::DeviceRequest::Stop { id, responder }) => {
                    if id as usize > self.max_timers {
                        log::error!("requested to stop timer {id}, which is not a valid timer ID.");
                        let _result = responder.send(Err(ffhh::DriverError::InvalidArgs))?;
                    } else {
                        // Evicted timer needs to be disposed off properly. The
                        // scoped `guard` allows us to avoid holding a lock across
                        // the .await points below.
                        let maybe_timer = {
                            let mut guard = self.inner.borrow_mut();
                            guard.events.remove(&id);
                            guard.timers.remove(&id)
                        };
                        if let Some(mut timer) = maybe_timer {
                            // Cancel the timer, and wait for its task to complete
                            // so that cancelation can be propagated to the API
                            // caller.  Note that cancelation can race with
                            // alarm expiry. This is an expected
                            // corner case and is documented in the hrtimer FIDL
                            // API.
                            timer.canceler.send(()).await?;
                            timer.task.await;
                        }
                        let _result = responder.send(Ok(()))?;
                        log::debug!("timer {id} stopped.");
                    }
                }

                Ok(ffhh::DeviceRequest::StartAndWait {
                    id,
                    resolution,
                    ticks,
                    setup_event,
                    responder,
                }) => {
                    let validation = self.validate_start_request(id, &resolution, ticks);
                    if let Err(err) = validation {
                        let _result = responder.send(Err(err))?;
                    } else {
                        defer! {
                            setup_event.as_handle_ref()
                                .signal_handle(
                                    zx::Signals::NONE,
                                    zx::Signals::EVENT_SIGNALED)
                               .expect("setup event should be signaled");
                        }
                        let notify_responder = move |result| -> Result<(), anyhow::Error> {
                            let _result = responder.send(result)?;
                            Ok(())
                        };
                        self.schedule_wake_alarm(id, resolution, ticks, notify_responder).await?;
                        log::debug!("started timer {id}");
                    }
                }

                Ok(ffhh::DeviceRequest::StartAndWait2 {
                    id,
                    resolution,
                    ticks,
                    setup_keep_alive: _keep,
                    responder,
                }) => {
                    let validation = self.validate_start_request(id, &resolution, ticks);
                    if let Err(err) = validation {
                        let _result = responder.send(Err(err))?;
                    } else {
                        let notify_responder = move |result| -> Result<(), anyhow::Error> {
                            responder.send(result)?;
                            Ok(())
                        };
                        self.schedule_wake_alarm(id, resolution, ticks, notify_responder).await?;
                        log::debug!("started timer {id}");
                    }
                }
                Ok(ffhh::DeviceRequest::ReadTimer { responder, .. }) => {
                    let _result = responder.send(Err(ffhh::DriverError::NotSupported))?;
                }
                Ok(ffhh::DeviceRequest::ReadClock { responder, .. }) => {
                    let _result = responder.send(Err(ffhh::DriverError::NotSupported))?;
                }
                Ok(ffhh::DeviceRequest::SetEvent { id, event, responder }) => {
                    let mut guard = self.inner.borrow_mut();
                    guard.events.insert(id, event);
                    let _result = responder.send(Ok(()))?;
                    log::debug!("set event for timer {id}");
                }
                Ok(ffhh::DeviceRequest::GetTicksLeft { responder, .. }) => {
                    let _result = responder.send(Err(ffhh::DriverError::NotSupported))?;
                }
                Ok(ffhh::DeviceRequest::_UnknownMethod { ordinal, .. }) => {
                    log::warn!("ignoring unknown method: {ordinal}");
                }
                Err(err) => {
                    log::error!("error while serving requests, exiting: {err:?}");
                    break;
                }
            }
        }
        log::warn!("Exiting serving loop. This should only happen at the end of tests.");
        Ok(())
    }
}

#[fuchsia::main(logging_tags = ["test"])]
async fn main() -> Result<()> {
    fuchsia_trace_provider::trace_provider_create_with_fdio();
    log::info!("Starting fake hrtimer service. Set log level to DEBUG for detailed diagnostics.");

    // Provide inspect.
    let inspector = fuchsia_inspect::component::inspector();
    let _inspect_server_task =
        inspect_runtime::publish(inspector, inspect_runtime::PublishOptions::default());

    // Integrate with the power topology
    let topology_proxy = connect_to_protocol::<ffpb::TopologyMarker>()
        .context("while connecting to fuchsia.power.broker/Topology")?;
    let (lessor_proxy, lessor_server_end) = create_proxy::<ffpb::LessorMarker>();
    let (element_proxy, element_server_end) = create_proxy::<ffpb::ElementControlMarker>();
    let (current_level_proxy, _current_level_server_end) =
        create_proxy::<ffpb::CurrentLevelMarker>();

    let schema = ffpb::ElementSchema {
        element_name: Some("fake-hrtimer".into()),
        initial_current_level: Some(ffpb::BinaryPowerLevel::Off.into_primitive()),
        valid_levels: Some(vec![
            ffpb::BinaryPowerLevel::Off.into_primitive(),
            ffpb::BinaryPowerLevel::On.into_primitive(),
        ]),
        lessor_channel: Some(lessor_server_end),
        element_control: Some(element_server_end),
        ..Default::default()
    };
    let _result = topology_proxy
        .add_element(schema)
        .await
        .context("while calling fuchsia.power.broker/Topology.AddElement")?;

    // Serve the FIDL APIs.
    let mut fs = ServiceFs::new();
    // The service will end up being at:
    // /svc/fuchsia.hardware.hrtimer.Service/hrtimer/instance.  See the
    // component manifest file for serving details.
    fs.dir("svc").add_fidl_service_instance("hrtimer", Incoming::Hrtimer);
    fs.take_and_serve_directory_handle()
        .context("while trying to serve fuchsia.hardware.hrtimer/Service")?;

    let hrtimer_server =
        FakeHrtimerServer::new(MAX_TIMERS, lessor_proxy, element_proxy, current_level_proxy);

    fs.for_each_concurrent(/*limit=*/ None, move |connection| {
        let hrtimer_server_clone = hrtimer_server.clone();
        async move {
            log::info!("got a connection");
            match connection {
                Incoming::Hrtimer(ffhh::ServiceRequest::Device(stream)) => {
                    fasync::Task::local(async move {
                        let _ignore = hrtimer_server_clone.serve(stream).await.map_err(|err| {
                            log::error!("stopped serving fuchsia.hardware.hrtimer/Service: {err:?}")
                        });
                    })
                    .detach();
                }
            }
        }
    })
    .await;
    log::debug!("Stopped fake hrtimer service. No further requests will be serviced.");
    Ok(())
}
