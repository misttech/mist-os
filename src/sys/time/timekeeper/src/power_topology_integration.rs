// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{anyhow, Context, Result};
use fidl::endpoints::ProtocolMarker;
use fidl_fuchsia_power_broker as fpb;
use fidl_fuchsia_power_system as fps;
use fuchsia_async as fasync;
use fuchsia_component::client;
use futures::channel::mpsc;
use futures::future;
use futures::{Future, SinkExt, StreamExt};
use tracing::{debug, error};

const ELEMENT_NAME: &str = "timekeeper-pe";

const POWER_ON: u8 = 0xff;
const POWER_OFF: u8 = 0x00;

const REQUIRED_LEVEL: u8 = fps::ExecutionStateLevel::WakeHandling.into_primitive();

pub async fn manage(activity_signal: mpsc::Sender<()>) -> Result<fasync::Task<()>> {
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
    mut activity: mpsc::Sender<()>,
    // Injected in tests.
    loop_fn: F,
) -> Result<fasync::Task<()>>
where
    G: Future<Output = fasync::Task<()>>,
    F: Fn(fpb::LevelControlProxy) -> G,
{
    let power_elements = governor_proxy
        .get_power_elements()
        .await
        .context("in a call to ActivityGovernor/GetPowerElements")?;

    let _ignore = activity.send(()).await;
    if let Some(execution_state) = power_elements.execution_state {
        if let Some(token) = execution_state.passive_dependency_token {
            let deps = vec![fpb::LevelDependency {
                dependency_type: fpb::DependencyType::Passive,
                dependent_level: POWER_ON,
                requires_token: token,
                requires_level: REQUIRED_LEVEL,
            }];

            let result = topology_proxy
                .add_element(
                    ELEMENT_NAME,
                    POWER_ON,
                    &vec![POWER_ON, POWER_OFF],
                    deps,
                    vec![],
                    vec![],
                )
                .await
                .context("while calling fuchsia.power.broker.Topology/AddElement")?;
            match result {
                Ok((_element_control_ch, _lease_control_ch, level_control_ch)) => {
                    return Ok(loop_fn(level_control_ch.into_proxy().expect("never fails")).await);
                }
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
async fn management_loop(proxy: fpb::LevelControlProxy) -> fasync::Task<()> {
    let clone = proxy.clone();
    // The Sender is used to ensure that rcv_task does not send before send_task
    // is done.
    let (mut send, mut rcv) = mpsc::channel::<(u8, mpsc::Sender<()>)>(1);

    let rcv_task = fasync::Task::local(async move {
        let mut last_required_level: u8 = POWER_ON;
        loop {
            let result = clone.watch_required_level(last_required_level).await;
            match result {
                Ok(Ok(level)) => {
                    last_required_level = level;
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
            match proxy.update_current_power_level(new_level).await {
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
    use fidl::endpoints;
    use fidl::endpoints::Proxy;
    use fuchsia_zircon as zx;
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
        let (proxy, mut stream) = endpoints::create_proxy_and_stream::<fpb::LevelControlMarker>()
            .expect("always succeeds");

        // Send the power level in from test into the handler.
        let (mut in_send, mut in_recv) = mpsc::channel(1);

        // Get the power level out from the handler into the test.
        let (mut out_send, mut out_recv) = mpsc::channel(1);

        // Serve the topology stream asynchronously.
        fasync::Task::local(async move {
            debug!("topology: start listening for requests.");
            while let Some(next) = stream.next().await {
                let request: fpb::LevelControlRequest = next.unwrap();
                debug!("topology: request: {:?}", request);
                match request {
                    fpb::LevelControlRequest::WatchRequiredLevel { responder, .. } => {
                        // Emulate hanging get response: block on a new value, then report that
                        // value.
                        let new_level = in_recv.next().await.expect("always succeeds");
                        responder.send(Ok(new_level)).unwrap();
                    }
                    fpb::LevelControlRequest::UpdateCurrentPowerLevel {
                        current_level,
                        responder,
                        ..
                    } => {
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

        // Management loop is also asynchronous.
        fasync::Task::local(async move {
            management_loop(proxy).await.await;
        })
        .detach();

        async_send_via(&mut in_send, POWER_ON).await;
        assert_eq!(POWER_ON, block_recv_from(&mut out_recv).await);

        async_send_via(&mut in_send, POWER_OFF).await;
        assert_eq!(POWER_OFF, block_recv_from(&mut out_recv).await);

        async_send_via(&mut in_send, POWER_ON).await;
        assert_eq!(POWER_ON, block_recv_from(&mut out_recv).await);
    }

    async fn empty_loop(_: fpb::LevelControlProxy) -> fasync::Task<()> {
        fasync::Task::local(async move {})
    }

    // Get a client end for the given protocol marker T, assuming that
    // will never get used.
    fn dummy_client_end<T>() -> endpoints::ClientEnd<T>
    where
        T: fidl::endpoints::ProtocolMarker,
        <T as fidl::endpoints::ProtocolMarker>::Proxy: std::fmt::Debug,
    {
        let (p, _) = endpoints::create_proxy_and_stream::<T>().expect("never fails");
        p.into_client_end().expect("never fails")
    }

    #[fuchsia::test]
    async fn test_manage_internal() {
        let (g_proxy, mut g_stream) =
            endpoints::create_proxy_and_stream::<fps::ActivityGovernorMarker>()
                .expect("infallible");
        let (t_proxy, mut _t_stream) =
            endpoints::create_proxy_and_stream::<fpb::TopologyMarker>().expect("infallible");
        let (_activity_s, mut activity_r) = mpsc::channel::<()>(1);

        // Run the server side activity governor.
        fasync::Task::local(async move {
            while let Some(request) = g_stream.next().await {
                match request {
                    Ok(fps::ActivityGovernorRequest::GetPowerElements { responder }) => {
                        let event = zx::Event::create();
                        responder
                            .send(fps::PowerElements {
                                execution_state: Some(fps::ExecutionState {
                                    passive_dependency_token: Some(event),
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
                    Ok(fpb::TopologyRequest::AddElement {
                        element_name: _,
                        initial_current_level: _,
                        valid_levels: _,
                        dependencies: _,
                        active_dependency_tokens_to_register: _,
                        passive_dependency_tokens_to_register: _,
                        responder,
                    }) => {
                        responder
                            .send(Ok((dummy_client_end(), dummy_client_end(), dummy_client_end())))
                            .expect("never fails");
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
