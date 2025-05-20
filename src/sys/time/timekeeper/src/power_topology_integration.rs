// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{anyhow, Context, Error, Result};
use fidl::endpoints::{create_endpoints, ProtocolMarker};
use fuchsia_component::client;
use futures::channel::mpsc;
use futures::{future, Future, SinkExt, StreamExt, TryStreamExt};
use log::{debug, error};
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
    F: Fn(fpb::ElementRunnerRequestStream) -> G,
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

            let (element_runner_client, element_runner_server) =
                create_endpoints::<fpb::ElementRunnerMarker>();
            let result = topology_proxy
                .add_element(fpb::ElementSchema {
                    element_name: Some(ELEMENT_NAME.into()),
                    initial_current_level: Some(POWER_ON),
                    valid_levels: Some(vec![POWER_ON, POWER_OFF]),
                    dependencies: Some(deps),
                    element_runner: Some(element_runner_client),
                    ..Default::default()
                })
                .await
                .context("while calling fuchsia.power.broker.Topology/AddElement")?;
            let element_runner = element_runner_server.into_stream();
            match result {
                Ok(_) => return Ok(loop_fn(element_runner).await),
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
async fn management_loop(mut element_runner: fpb::ElementRunnerRequestStream) -> fasync::Task<()> {
    // The Sender is used to ensure that rcv_task does not send before send_task
    // is done.
    let (mut send, mut rcv) = mpsc::channel::<(u8, mpsc::Sender<()>)>(1);

    let rcv_task = fasync::Task::local(async move {
        loop {
            let Ok(Some(request)) = element_runner.try_next().await else {
                error!("error while waiting for element runner request, bailing");
                break;
            };
            let result = handle_element_runner_set_level(request).await;
            match result {
                Ok((level, responder)) => {
                    let (s, mut r) = mpsc::channel::<()>(1);
                    // For now, we just echo the power level back to the power broker.
                    if let Err(e) = send.send((level, s)).await {
                        error!("error while processing power level, bailing: {:?}", e);
                        break;
                    }
                    // Wait until rcv_task propagates the new required level.
                    r.next().await.unwrap();
                    // Respond to SetLevel, indicating the level transition is complete.
                    responder.send().expect("set_level resp failed");
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
            match new_level {
                POWER_OFF => {
                    debug!("new required level: power off");
                }
                POWER_ON => {
                    debug!("new required level: power on");
                }
                _ => {
                    error!("invalid required level: {}", new_level);
                }
            }
            // Handle level transition here.
            s.send(()).await.unwrap();
        }
        debug!("no longer reporting required level");
    });
    fasync::Task::local(async move {
        future::join(rcv_task, send_task).await;
    })
}

async fn handle_element_runner_set_level(
    request: fpb::ElementRunnerRequest,
) -> Result<(u8, fpb::ElementRunnerSetLevelResponder), Error> {
    match request {
        fpb::ElementRunnerRequest::SetLevel { level, responder } => {
            return Ok((level, responder));
        }
        fidl_fuchsia_power_broker::ElementRunnerRequest::_UnknownMethod { .. } => {
            return Err(Error::msg("ElementRunnerRequest::_UnknownMethod received"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Command;
    use fidl::endpoints;
    use log::debug;

    #[fuchsia::test]
    async fn propagate_level() {
        let (element_runner_client, element_runner_server) =
            create_endpoints::<fpb::ElementRunnerMarker>();
        let element_runner = element_runner_server.into_stream();
        let element_runner_proxy = element_runner_client.into_proxy();

        // Management loop is also asynchronous.
        fasync::Task::local(async move {
            management_loop(element_runner).await.await;
        })
        .detach();

        assert!(element_runner_proxy.set_level(POWER_ON).await.is_ok());

        assert!(element_runner_proxy.set_level(POWER_OFF).await.is_ok());

        assert!(element_runner_proxy.set_level(POWER_ON).await.is_ok());
    }

    async fn empty_loop(_: fpb::ElementRunnerRequestStream) -> fasync::Task<()> {
        fasync::Task::local(async move {})
    }

    #[fuchsia::test]
    async fn test_manage_internal() {
        let (g_proxy, mut g_stream) =
            endpoints::create_proxy_and_stream::<fps::ActivityGovernorMarker>();
        let (t_proxy, mut _t_stream) = endpoints::create_proxy_and_stream::<fpb::TopologyMarker>();
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
