// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::reboot;
use anyhow::{anyhow, Context as _, Result};
use ffx_daemon_events::TargetEvent;
use ffx_daemon_target::target::Target;
use ffx_stream_util::TryStreamUtilExt;
use fidl::endpoints::ServerEnd;
use fidl_fuchsia_developer_ffx::{self as ffx};
use futures::TryStreamExt;
use protocols::Context;
use std::{future::Future, pin::Pin, rc::Rc, time::Duration};

// TODO(awdavies): Abstract this to use similar utilities to an actual protocol.
// This functionally behaves the same with the only caveat being that some
// initial state is set by the caller (the target Rc).
#[derive(Debug)]
pub(crate) struct TargetHandle {}

impl TargetHandle {
    pub(crate) fn new(
        target: Rc<Target>,
        cx: Context,
        handle: ServerEnd<ffx::TargetMarker>,
    ) -> Result<Pin<Box<dyn Future<Output = ()>>>> {
        let reboot_controller = reboot::RebootController::new(target.clone());
        let inner = TargetHandleInner { target, reboot_controller };
        let stream = handle.into_stream()?;
        let fut = Box::pin(async move {
            let _ = stream
                .map_err(|err| anyhow!("{}", err))
                .try_for_each_concurrent_while_connected(None, |req| inner.handle(&cx, req))
                .await;
        });
        Ok(fut)
    }
}

struct TargetHandleInner {
    target: Rc<Target>,
    reboot_controller: reboot::RebootController,
}

impl TargetHandleInner {
    #[tracing::instrument(skip(self, _cx))]
    async fn handle(&self, _cx: &Context, req: ffx::TargetRequest) -> Result<()> {
        tracing::debug!("handling request {req:?}");
        match req {
            ffx::TargetRequest::GetSshLogs { responder } => {
                let logs = self.target.host_pipe_log_buffer().lines();
                responder.send(&logs.join("\n")).map_err(Into::into)
            }
            ffx::TargetRequest::GetSshAddress { responder } => {
                // Product state and manual state are the two states where an
                // address is guaranteed. If the target is not in that state,
                // then wait for its state to change.
                let connection_state = self.target.get_connection_state();
                if !connection_state.is_product() && !connection_state.is_manual() {
                    self.target
                        .events
                        .wait_for(None, |e| {
                            if let TargetEvent::ConnectionStateChanged(_, state) = e {
                                // It's not clear if it is possible to change
                                // the state to `Manual`, but check for it just
                                // in case.
                                state.is_product() || state.is_manual()
                            } else {
                                false
                            }
                        })
                        .await
                        .context("waiting for connection state changes")?;
                }
                // After the event fires it should be guaranteed that the
                // SSH address is written to the target.
                let poll_duration = Duration::from_millis(15);
                loop {
                    if let Some(addr) = self.target.ssh_address_info() {
                        return responder.send(&addr).map_err(Into::into);
                    }
                    fuchsia_async::Timer::new(poll_duration).await;
                }
            }
            ffx::TargetRequest::SetPreferredSshAddress { ip, responder } => {
                let result = self
                    .target
                    .set_preferred_ssh_address(ip.into())
                    .then(|| ())
                    .ok_or(ffx::TargetError::AddressNotFound);

                if result.is_ok() {
                    self.target.maybe_reconnect();
                }

                responder.send(result).map_err(Into::into)
            }
            ffx::TargetRequest::ClearPreferredSshAddress { responder } => {
                self.target.clear_preferred_ssh_address();
                self.target.maybe_reconnect();
                responder.send().map_err(Into::into)
            }
            ffx::TargetRequest::OpenRemoteControl { remote_control, responder } => {
                tracing::debug!("In OpenRemoteControl, running host pipe");
                let rcs = self.target.wait_for_rcs().await?;
                match rcs {
                    Ok(mut c) => {
                        // TODO(awdavies): Return this as a specific error to
                        // the client with map_err.
                        c.copy_to_channel(remote_control.into_channel())?;
                        responder.send(Ok(())).map_err(Into::into)
                    }
                    Err(e) => {
                        // close connection on error so the next call re-establishes the Overnet connection
                        self.target.disconnect();
                        responder.send(Err(e)).context("sending error response").map_err(Into::into)
                    }
                }
            }
            ffx::TargetRequest::OpenFastboot { fastboot, .. } => {
                self.reboot_controller.spawn_fastboot(fastboot).await.map_err(Into::into)
            }
            ffx::TargetRequest::Reboot { state, responder } => {
                self.reboot_controller.reboot(state, responder).await
            }
            ffx::TargetRequest::Identity { responder } => {
                let target_info = ffx::TargetInfo::from(&*self.target);
                responder.send(&target_info).map_err(Into::into)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use ffx_daemon_events::TargetConnectionState;
    use ffx_daemon_target::target::{TargetAddrEntry, TargetAddrType};
    use fidl::prelude::*;
    use fidl_fuchsia_developer_remotecontrol as fidl_rcs;
    use fidl_fuchsia_io as fio;
    use fuchsia_async::Task;
    use hoist::{Hoist, OvernetInstance};
    use protocols::testing::FakeDaemonBuilder;
    use rcs::RcsConnection;
    use std::{
        net::{IpAddr, SocketAddr},
        str::FromStr,
    };

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_valid_target_state() {
        const TEST_SOCKETADDR: &'static str = "[fe80::1%1]:22";
        let daemon = FakeDaemonBuilder::new().build();
        let cx = Context::new(daemon);
        let target = Target::new_with_addr_entries(
            Some("pride-and-prejudice"),
            vec![TargetAddrEntry::new(
                SocketAddr::from_str(TEST_SOCKETADDR).unwrap().into(),
                Utc::now(),
                TargetAddrType::Ssh,
            )]
            .into_iter(),
        );
        target.update_connection_state(|_| TargetConnectionState::Mdns(std::time::Instant::now()));
        let (proxy, server) = fidl::endpoints::create_proxy::<ffx::TargetMarker>().unwrap();
        let _handle = Task::local(TargetHandle::new(target, cx, server).unwrap());
        let result = proxy.get_ssh_address().await.unwrap();
        if let ffx::TargetAddrInfo::IpPort(ffx::TargetIpPort {
            ip: fidl_fuchsia_net::IpAddress::Ipv6(fidl_fuchsia_net::Ipv6Address { addr }),
            ..
        }) = result
        {
            assert_eq!(IpAddr::from(addr), SocketAddr::from_str(TEST_SOCKETADDR).unwrap().ip());
        } else {
            panic!("incorrect address received: {:?}", result);
        }
    }

    fn spawn_protocol_provider(
        nodename: String,
        server: fidl::endpoints::ServerEnd<fidl_fuchsia_overnet::ServiceProviderMarker>,
    ) -> Task<()> {
        Task::local(async move {
            let mut stream = server.into_stream().unwrap();
            while let Ok(Some(req)) = stream.try_next().await {
                match req {
                    fidl_fuchsia_overnet::ServiceProviderRequest::ConnectToService {
                        chan, ..
                    } => {
                        let server_end =
                            fidl::endpoints::ServerEnd::<fidl_rcs::RemoteControlMarker>::new(chan);
                        let mut stream = server_end.into_stream().unwrap();
                        let nodename = nodename.clone();
                        Task::local(async move {
                            let mut knock_channels = Vec::new();
                            while let Ok(Some(req)) = stream.try_next().await {
                                match req {
                                    fidl_rcs::RemoteControlRequest::IdentifyHost { responder } => {
                                        let addrs = vec![fidl_fuchsia_net::Subnet {
                                            addr: fidl_fuchsia_net::IpAddress::Ipv4(
                                                fidl_fuchsia_net::Ipv4Address {
                                                    addr: [192, 168, 1, 2],
                                                },
                                            ),
                                            prefix_len: 24,
                                        }];
                                        let nodename = Some(nodename.clone());
                                        responder
                                            .send(Ok(&fidl_rcs::IdentifyHostResponse {
                                                nodename,
                                                addresses: Some(addrs),
                                                ..Default::default()
                                            }))
                                            .unwrap();
                                    }
                                    fidl_rcs::RemoteControlRequest::ConnectCapability {
                                        moniker,
                                        capability_name,
                                        server_chan,
                                        flags,
                                        responder,
                                    } => {
                                        assert_eq!(flags, fio::OpenFlags::empty());
                                        assert_eq!(moniker, "/core/remote-control");
                                        assert_eq!(
                                            capability_name,
                                            "fuchsia.developer.remotecontrol.RemoteControl"
                                        );
                                        knock_channels.push(server_chan);
                                        responder.send(Ok(())).unwrap();
                                    }
                                    _ => panic!("unsupported for this test"),
                                }
                            }
                        })
                        .detach();
                    }
                }
            }
        })
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_open_rcs_valid() {
        const TEST_NODE_NAME: &'static str = "villete";
        let local_hoist = Hoist::new(None).unwrap();
        let hoist2 = Hoist::new(None).unwrap();
        let (rx2, tx2) = fidl::Socket::create_stream();
        let (mut rx2, mut tx2) = (
            fidl::AsyncSocket::from_socket(rx2).unwrap(),
            fidl::AsyncSocket::from_socket(tx2).unwrap(),
        );
        let (rx1, tx1) = fidl::Socket::create_stream();
        let (mut rx1, mut tx1) = (
            fidl::AsyncSocket::from_socket(rx1).unwrap(),
            fidl::AsyncSocket::from_socket(tx1).unwrap(),
        );
        let (error_sink, _) = futures::channel::mpsc::unbounded();
        let h1_hoist = local_hoist.clone();
        let error_sink_clone = error_sink.clone();
        let _h1_task = Task::local(async move {
            circuit::multi_stream::multi_stream_node_connection_to_async(
                h1_hoist.node().circuit_node(),
                &mut rx1,
                &mut tx2,
                true,
                circuit::Quality::IN_PROCESS,
                error_sink_clone,
                "h2".to_owned(),
            )
            .await
        });
        let hoist2_node = hoist2.node();
        let _h2_task = Task::local(async move {
            circuit::multi_stream::multi_stream_node_connection_to_async(
                hoist2_node.circuit_node(),
                &mut rx2,
                &mut tx1,
                false,
                circuit::Quality::IN_PROCESS,
                error_sink,
                "h1".to_owned(),
            )
            .await
        });
        let (client, server) =
            fidl::endpoints::create_endpoints::<fidl_fuchsia_overnet::ServiceProviderMarker>();
        let _svc_task = spawn_protocol_provider(TEST_NODE_NAME.to_owned(), server);
        hoist2
            .connect_as_service_publisher()
            .unwrap()
            .publish_service(fidl_rcs::RemoteControlMarker::PROTOCOL_NAME, client)
            .unwrap();
        let daemon = FakeDaemonBuilder::new().build();
        let cx = Context::new(daemon);
        let service_consumer = local_hoist.connect_as_service_consumer().unwrap();
        while service_consumer.list_peers().await.unwrap().iter().all(|x| x.is_self) {}
        let (client, server) = fidl::Channel::create();
        service_consumer
            .connect_to_service(
                &hoist2.node().node_id().into(),
                fidl_rcs::RemoteControlMarker::PROTOCOL_NAME,
                server,
            )
            .unwrap();
        let rcs_proxy =
            fidl_rcs::RemoteControlProxy::new(fidl::AsyncChannel::from_channel(client).unwrap());
        let target = Target::from_rcs_connection(RcsConnection::new_with_proxy(
            &local_hoist,
            rcs_proxy.clone(),
            &hoist2.node().node_id().into(),
        ))
        .await
        .unwrap();
        let (target_proxy, server) = fidl::endpoints::create_proxy::<ffx::TargetMarker>().unwrap();
        let _handle = Task::local(TargetHandle::new(target, cx, server).unwrap());
        let (rcs, rcs_server) =
            fidl::endpoints::create_proxy::<fidl_rcs::RemoteControlMarker>().unwrap();
        let res = target_proxy.open_remote_control(rcs_server).await.unwrap();
        assert!(res.is_ok());
        assert_eq!(TEST_NODE_NAME, rcs.identify_host().await.unwrap().unwrap().nodename.unwrap());
    }
}
