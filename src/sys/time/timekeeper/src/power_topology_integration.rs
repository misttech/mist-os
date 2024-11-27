// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{anyhow, Context, Result};
use fidl::endpoints::{create_proxy, ProtocolMarker};
use fuchsia_component::client;
use futures::channel::mpsc;
use futures::{future, Future, SinkExt, StreamExt};
use tracing::{debug, error};
use {fidl_fuchsia_power_broker as fpb, fidl_fuchsia_power_system as fps, fuchsia_async as fasync};

const ELEMENT_NAME: &str = "timekeeper-pe";

const POWER_ON: u8 = 0xff;
const POWER_OFF: u8 = 0x00;

const REQUIRED_LEVEL: u8 = fps::ExecutionStateLevel::Suspending.into_primitive();

pub async fn manage(activity_signal: mpsc::Sender<super::Command>) -> Result<fasync::Task<()>> {
    let governor_proxy = client::connect_to_protocol::<fps::ActivityGovernorMarker>()
        .with_context(|| {
            format!("while connecting to: {:?}", fps::ActivityGovernorMarker::DEBUG_NAME)
        })?;

    let topology_proxy = client::connect_to_protocol::<fpb::TopologyMarker>()
        .with_context(|| format!("while connecting to: {:?}", fpb::TopologyMarker::DEBUG_NAME))?;

    manage_internal(governor_proxy, topology_proxy, activity_signal, management_loop).await
}

async fn manage_internal<F, G>(
    governor_proxy: fps::ActivityGovernorProxy,
    topology_proxy: fpb::TopologyProxy,
    mut activity: mpsc::Sender<super::Command>,
    // Injected in tests.
    loop_fn: F,
) -> Result<fasync::Task<()>>
where
    G: Future<Output = fasync::Task<()>>,
    F: Fn(fpb::CurrentLevelProxy, fpb::RequiredLevelProxy) -> G,
{
    let power_elements = governor_proxy
        .get_power_elements()
        .await
        .context("in a call to ActivityGovernor/GetPowerElements")?;

    let _ignore = activity.send(super::Command::PowerManagement).await;
    if let Some(execution_state) = power_elements.execution_state {
        if let Some(token) = execution_state.opportunistic_dependency_token {
            let deps = vec![fpb::LevelDependency {
                dependency_type: fpb::DependencyType::Opportunistic,
                dependent_level: POWER_ON,
                requires_token: token,
                requires_level_by_preference: vec![REQUIRED_LEVEL],
            }];

            let (current, current_level_channel) = create_proxy::<fpb::CurrentLevelMarker>();
            let (required, required_level_channel) = create_proxy::<fpb::RequiredLevelMarker>();
            let result = topology_proxy
                .add_element(fpb::ElementSchema {
                    element_name: Some(ELEMENT_NAME.into()),
                    initial_current_level: Some(POWER_ON),
                    valid_levels: Some(vec![POWER_ON, POWER_OFF]),
                    dependencies: Some(deps),
                    level_control_channels: Some(fpb::LevelControlChannels {
                        current: current_level_channel,
                        required: required_level_channel,
                    }),
                    ..Default::default()
                })
                .await
                .context("while calling fuchsia.power.broker.Topology/AddElement")?;
            match result {
                Ok(_) => return Ok(loop_fn(current, required).await),
                Err(e) => return Err(anyhow!("error while adding element: {:?}", e)),
            }
        }
    } else {
        debug!(
            "no execution state power token found, power management integration is shutting down."
        );
    }
    Ok(fasync::Task::local(async {}))
}

// Loop around and react to level control messages. Use separate tasks to ensure
// we can insert a power transition process in between.
//
// Returns the task spawned for transition control.
async fn management_loop(
    current: fpb::CurrentLevelProxy,
    required: fpb::RequiredLevelProxy,
) -> fasync::Task<()> {
    // The Sender is used to ensure that rcv_task does not send before send_task
    // is done.
    let (mut send, mut rcv) = mpsc::channel::<(u8, mpsc::Sender<()>)>(1);

    let rcv_task = fasync::Task::local(async move {
        loop {
            let result = required.watch().await;
            match result {
                Ok(Ok(level)) => {
                    let (s, mut r) = mpsc::channel::<()>(1);
                    // For now, we just echo the power level back to the power broker.
                    if let Err(e) = send.send((level, s)).await {
                        error!("error while processing power level, bailing: {:?}", e);
                        break;
                    }
                    // Wait until rcv_task propagates the new required level.
                    r.next().await.unwrap();
                }
                Ok(Err(e)) => {
                    error!("error while watching level, bailing: {:?}", e);
                    break;
                }
                Err(e) => {
                    error!("error while watching level, bailing: {:?}", e);
                    break;
                }
            }
        }
        debug!("no longer watching required level");
    });
    let send_task = fasync::Task::local(async move {
        while let Some((new_level, mut s)) = rcv.next().await {
            match current.update(new_level).await {
                Ok(Ok(())) => {
                    // Allow rcv_task to proceed.
                    s.send(()).await.unwrap();
                }
                Ok(Err(e)) => {
                    error!("error while watching level, bailing: {:?}", e);
                    break;
                }
                Err(e) => {
                    error!("error while watching level, bailing: {:?}", e);
                    break;
                }
            }
        }
        debug!("no longer reporting required level");
    });
    fasync::Task::local(async move {
        future::join(rcv_task, send_task).await;
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Command;
    use fidl::endpoints;

    use tracing::debug;

    // Returns immediately.
    async fn async_send_via(s: &mut mpsc::Sender<u8>, value: u8) {
        let mut c = s.clone();
        fasync::Task::local(async move {
            c.send(value).await.expect("always succeeds");
        })
        .detach();
    }

    // Waits for a value to be available on the receiver.
    async fn block_recv_from(s: &mut mpsc::Receiver<u8>) -> u8 {
        let level = s.next().await.expect("always succeeds");
        level
    }

    #[fuchsia::test]
    async fn propagate_level() {
        let (current, mut current_stream) =
            endpoints::create_proxy_and_stream::<fpb::CurrentLevelMarker>()
                .expect("always succeeds");
        let (required, mut required_stream) =
            endpoints::create_proxy_and_stream::<fpb::RequiredLevelMarker>()
                .expect("always succeeds");

        // Send the power level in from test into the handler.
        let (mut in_send, mut in_recv) = mpsc::channel(1);

        // Get the power level out from the handler into the test.
        let (mut out_send, mut out_recv) = mpsc::channel(1);

        // Serve the topology streams asynchronously.
        fasync::Task::local(async move {
            debug!("topology: start listening for requests.");
            while let Some(next) = current_stream.next().await {
                let request: fpb::CurrentLevelRequest = next.unwrap();
                debug!("topology: request: {:?}", request);
                match request {
                    fpb::CurrentLevelRequest::Update { current_level, responder, .. } => {
                        out_send.send(current_level).await.expect("always succeeds");
                        responder.send(Ok(())).unwrap();
                    }
                    _ => {
                        unimplemented!();
                    }
                }
            }
        })
        .detach();
        fasync::Task::local(async move {
            debug!("topology: start listening for requests.");
            while let Some(next) = required_stream.next().await {
                let request: fpb::RequiredLevelRequest = next.unwrap();
                debug!("topology: request: {:?}", request);
                match request {
                    fpb::RequiredLevelRequest::Watch { responder, .. } => {
                        // Emulate hanging get response: block on a new value, then report that
                        // value.
                        let new_level = in_recv.next().await.expect("always succeeds");
                        responder.send(Ok(new_level)).unwrap();
                    }
                    _ => {
                        unimplemented!();
                    }
                }
            }
        })
        .detach();

        // Management loop is also asynchronous.
        fasync::Task::local(async move {
            management_loop(current, required).await.await;
        })
        .detach();

        async_send_via(&mut in_send, POWER_ON).await;
        assert_eq!(POWER_ON, block_recv_from(&mut out_recv).await);

        async_send_via(&mut in_send, POWER_OFF).await;
        assert_eq!(POWER_OFF, block_recv_from(&mut out_recv).await);

        async_send_via(&mut in_send, POWER_ON).await;
        assert_eq!(POWER_ON, block_recv_from(&mut out_recv).await);
    }

    async fn empty_loop(_: fpb::CurrentLevelProxy, _: fpb::RequiredLevelProxy) -> fasync::Task<()> {
        fasync::Task::local(async move {})
    }

    #[fuchsia::test]
    async fn test_manage_internal() {
        let (g_proxy, mut g_stream) =
            endpoints::create_proxy_and_stream::<fps::ActivityGovernorMarker>()
                .expect("infallible");
        let (t_proxy, mut _t_stream) =
            endpoints::create_proxy_and_stream::<fpb::TopologyMarker>().expect("infallible");
        let (_activity_s, mut activity_r) = mpsc::channel::<Command>(1);

        // Run the server side activity governor.
        fasync::Task::local(async move {
            while let Some(request) = g_stream.next().await {
                match request {
                    Ok(fps::ActivityGovernorRequest::GetPowerElements { responder }) => {
                        let event = zx::Event::create();
                        responder
                            .send(fps::PowerElements {
                                execution_state: Some(fps::ExecutionState {
                                    opportunistic_dependency_token: Some(event),
                                    ..Default::default()
                                }),
                                ..Default::default()
                            })
                            .expect("never fails");
                    }
                    Ok(_) | Err(_) => unimplemented!(),
                }
            }
            debug!("governor server side test exiting");
        })
        .detach();

        // Run the server side topology proxy
        fasync::Task::local(async move {
            while let Some(request) = _t_stream.next().await {
                match request {
                    Ok(fpb::TopologyRequest::AddElement { payload: _, responder }) => {
                        responder.send(Ok(())).expect("never fails");
                    }
                    Ok(_) | Err(_) => unimplemented!(),
                }
            }
        })
        .detach();

        fasync::Task::local(async move {
            manage_internal(g_proxy, t_proxy, _activity_s, empty_loop).await.unwrap().await;
        })
        .detach();

        activity_r.next().await.unwrap();
    }
}
