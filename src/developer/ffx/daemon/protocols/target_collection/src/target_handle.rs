// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::reboot;
use anyhow::{anyhow, Context as _, Result};
use ffx_daemon_events::TargetEvent;
use ffx_daemon_target::target::Target;
use ffx_daemon_target::target_collection::TargetCollection;
use ffx_ssh::ssh::SshError;
use ffx_stream_util::TryStreamUtilExt;
use fidl::endpoints::ServerEnd;
use fidl_fuchsia_developer_ffx::{self as ffx};
use futures::{channel, TryStreamExt};
use protocols::Context;
use std::cell::RefCell;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::time::Duration;

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
        target_collection: Rc<TargetCollection>,
    ) -> Result<Pin<Box<dyn Future<Output = ()>>>> {
        let reboot_controller = reboot::RebootController::new(target.clone(), cx.overnet_node()?);
        let keep_alive = target.keep_alive();
        let inner = TargetHandleInner {
            target: RefCell::new(target),
            target_collection,
            reboot_controller,
        };
        let stream = handle.into_stream();
        let fut = Box::pin(async move {
            let _ = stream
                .map_err(|err| anyhow!("{}", err))
                .try_for_each_concurrent_while_connected(None, |req| inner.handle(&cx, req))
                .await;
            drop(keep_alive);
        });
        Ok(fut)
    }
}

struct TargetHandleInner {
    target: RefCell<Rc<Target>>,
    target_collection: Rc<TargetCollection>,
    reboot_controller: reboot::RebootController,
}

impl TargetHandleInner {
    #[tracing::instrument(skip(self, cx))]
    async fn handle(&self, cx: &Context, req: ffx::TargetRequest) -> Result<()> {
        tracing::debug!("handling request {req:?}");
        match req {
            ffx::TargetRequest::GetSshLogs { responder } => {
                let logs = self.target.borrow().host_pipe_log_buffer().lines();
                responder.send(&logs.join("\n")).map_err(Into::into)
            }
            ffx::TargetRequest::GetSshAddress { responder } => {
                // Product state and manual state are the two states where an
                // address is guaranteed. If the target is not in that state,
                // then wait for its state to change.
                let connection_state = self.target.borrow().get_connection_state();
                if !connection_state.is_product() && !connection_state.is_manual() {
                    let events = self.target.borrow().events.clone();
                    events
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
                    if let Some(addr) = self.target.borrow().ssh_address_info() {
                        return responder.send(&addr).map_err(Into::into);
                    }
                    fuchsia_async::Timer::new(poll_duration).await;
                }
            }
            ffx::TargetRequest::SetPreferredSshAddress { ip, responder } => {
                let result = self
                    .target
                    .borrow()
                    .set_preferred_ssh_address(ip.into())
                    .then_some(())
                    .ok_or(ffx::TargetError::AddressNotFound);

                if result.is_ok() {
                    self.target.borrow().maybe_reconnect(None);
                }

                responder.send(result).map_err(Into::into)
            }
            ffx::TargetRequest::ClearPreferredSshAddress { responder } => {
                self.target.borrow().clear_preferred_ssh_address();
                self.target.borrow().maybe_reconnect(None);
                responder.send().map_err(Into::into)
            }
            ffx::TargetRequest::OpenRemoteControl { remote_control, responder } => {
                let (roid_sender, roid_receiver) = channel::oneshot::channel();
                self.target
                    .borrow()
                    .run_host_pipe_with_sender(&cx.overnet_node()?, Some(roid_sender));
                let remote_overnet_id = roid_receiver.await?;
                if let Some(roid) = remote_overnet_id {
                    // If we already have a connection to this overnet node, further requests can do that the one instead.
                    self.maybe_redirect_target(roid);
                }
                let rcs = wait_for_rcs(&self.target.borrow(), &cx).await?;
                match rcs {
                    Ok(mut c) => {
                        // TODO(awdavies): Return this as a specific error to
                        // the client with map_err.
                        c.copy_to_channel(remote_control.into_channel())?;
                        responder.send(Ok(())).map_err(Into::into)
                    }
                    Err(e) => {
                        // close connection on error so the next call re-establishes the Overnet connection
                        self.target.borrow().disconnect();
                        responder.send(Err(e)).context("sending error response").map_err(Into::into)
                    }
                }
            }
            ffx::TargetRequest::Reboot { state, responder } => {
                self.reboot_controller.reboot(state, responder).await
            }
            ffx::TargetRequest::Identity { responder } => {
                let target_info = ffx::TargetInfo::from(&**self.target.borrow());
                responder.send(&target_info).map_err(Into::into)
            }
            ffx::TargetRequest::Disconnect { responder } => {
                self.target.borrow().disconnect();
                responder.send().map_err(Into::into)
            }
        }
    }

    fn maybe_redirect_target(&self, remote_overnet_id: u64) {
        if let Some(prev_target) = self.target_collection.find_overnet_id(remote_overnet_id) {
            // We don't want to remove ourselves
            if prev_target.id() != self.target.borrow().id() {
                {
                    let my_target = self.target.borrow();
                    tracing::info!("connection to {:?} reached same target as {:?}. Redirecting further requests", my_target.addrs(), prev_target.addrs());
                    prev_target.extend_addrs_from_other(my_target.clone());
                    self.target_collection.remove_target_from_list(my_target.id());
                }
                // Wait until now to borrow_mut, since
                // extend_addrs_from_other() will also need a borrow
                *self.target.borrow_mut() = prev_target;
            }
        }
    }
}

#[tracing::instrument]
pub(crate) async fn wait_for_rcs(
    t: &Rc<Target>,
    cx: &Context,
) -> Result<Result<rcs::RcsConnection, ffx::TargetConnectionError>> {
    // This setup here is due to the events not having a proper streaming implementation. The
    // closure is intended to have a static lifetime, which forces this to happen to extract an
    // event.
    let seen_event = Rc::new(RefCell::new(Option::<SshError>::None));

    Ok(loop {
        if let Some(rcs) = t.rcs() {
            break Ok(rcs);
        } else if let Some(err) = seen_event.borrow_mut().take() {
            tracing::debug!("host pipe connection failed: {err:?}. Restarting connection.");
            t.disconnect();
            t.run_host_pipe(match &cx.overnet_node() {
                Ok(n) => n,
                Err(e) => {
                    tracing::debug!("unable to get overnet node, forcing connection to exit with last seen SSH error: {err:?}. Overnet node error was {e:?}");
                    t.host_pipe_log_buffer().push_line(err.to_string());
                    break Err(host_pipe_err_to_fidl(err));
                }
            });
            t.host_pipe_log_buffer().push_line(err.to_string());
            break Err(host_pipe_err_to_fidl(err));
        } else {
            tracing::trace!("RCS dropped after event fired. Waiting again.");
        }

        let se_clone = seen_event.clone();

        t.events
            .wait_for(None, move |e| match e {
                TargetEvent::RcsActivated => true,
                TargetEvent::SshHostPipeErr(ssh_error) => {
                    *se_clone.borrow_mut() = Some(ssh_error);
                    true
                }
                _ => false,
            })
            .await
            .context("waiting for RCS")?;
    })
}

#[tracing::instrument]
fn host_pipe_err_to_fidl(ssh_err: SshError) -> ffx::TargetConnectionError {
    match ssh_err {
        SshError::Unknown(s) => {
            tracing::warn!("Unknown host-pipe error received: '{}'", s);
            ffx::TargetConnectionError::UnknownError
        }
        SshError::NetworkUnreachable => ffx::TargetConnectionError::NetworkUnreachable,
        SshError::PermissionDenied => ffx::TargetConnectionError::PermissionDenied,
        SshError::ConnectionRefused => ffx::TargetConnectionError::ConnectionRefused,
        SshError::UnknownNameOrService => ffx::TargetConnectionError::UnknownNameOrService,
        SshError::Timeout => ffx::TargetConnectionError::Timeout,
        SshError::KeyVerificationFailure => ffx::TargetConnectionError::KeyVerificationFailure,
        SshError::NoRouteToHost => ffx::TargetConnectionError::NoRouteToHost,
        SshError::InvalidArgument => ffx::TargetConnectionError::InvalidArgument,
        SshError::TargetIncompatible => ffx::TargetConnectionError::TargetIncompatible,
        SshError::ConnectionClosedByRemoteHost => {
            ffx::TargetConnectionError::ConnectionClosedByRemoteHost
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use ffx_daemon_events::TargetConnectionState;
    use ffx_daemon_target::target::{TargetAddrEntry, TargetAddrStatus, TargetUpdateBuilder};
    use fidl::prelude::*;
    use fuchsia_async::Task;
    use futures::StreamExt;
    use protocols::testing::FakeDaemonBuilder;
    use rcs::RcsConnection;
    use std::net::{IpAddr, SocketAddr};
    use std::str::FromStr;
    use std::sync::Arc;
    use {fidl_fuchsia_developer_remotecontrol as fidl_rcs, fidl_fuchsia_sys2 as fsys};

    #[test]
    fn test_host_pipe_err_to_fidl_conversion() {
        assert_eq!(
            host_pipe_err_to_fidl(SshError::Unknown(String::from("foobar"))),
            ffx::TargetConnectionError::UnknownError
        );
        assert_eq!(
            host_pipe_err_to_fidl(SshError::InvalidArgument),
            ffx::TargetConnectionError::InvalidArgument
        );
        assert_eq!(
            host_pipe_err_to_fidl(SshError::NoRouteToHost),
            ffx::TargetConnectionError::NoRouteToHost
        );
        assert_eq!(
            host_pipe_err_to_fidl(SshError::KeyVerificationFailure),
            ffx::TargetConnectionError::KeyVerificationFailure
        );
        assert_eq!(host_pipe_err_to_fidl(SshError::Timeout), ffx::TargetConnectionError::Timeout);
        assert_eq!(
            host_pipe_err_to_fidl(SshError::UnknownNameOrService),
            ffx::TargetConnectionError::UnknownNameOrService
        );
        assert_eq!(
            host_pipe_err_to_fidl(SshError::ConnectionRefused),
            ffx::TargetConnectionError::ConnectionRefused
        );
        assert_eq!(
            host_pipe_err_to_fidl(SshError::PermissionDenied),
            ffx::TargetConnectionError::PermissionDenied
        );
        assert_eq!(
            host_pipe_err_to_fidl(SshError::NetworkUnreachable),
            ffx::TargetConnectionError::NetworkUnreachable
        );
        assert_eq!(
            host_pipe_err_to_fidl(SshError::TargetIncompatible),
            ffx::TargetConnectionError::TargetIncompatible
        );
    }

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
                TargetAddrStatus::ssh().manually_added(),
            )]
            .into_iter(),
        );
        target.update_connection_state(|_| TargetConnectionState::Mdns(std::time::Instant::now()));
        let (proxy, server) = fidl::endpoints::create_proxy::<ffx::TargetMarker>();
        let tc = Rc::new(TargetCollection::new());
        let _handle = Task::local(TargetHandle::new(target, cx, server, tc.clone()).unwrap());
        let result = proxy.get_ssh_address().await.unwrap();
        if let ffx::TargetIpAddrInfo::IpPort(ffx::TargetIpPort {
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
        mut receiver: futures::channel::mpsc::UnboundedReceiver<fidl::Channel>,
    ) -> Task<()> {
        Task::local(async move {
            while let Some(chan) = receiver.next().await {
                let server_end =
                    fidl::endpoints::ServerEnd::<fidl_rcs::RemoteControlMarker>::new(chan);
                let mut stream = server_end.into_stream();
                let nodename = nodename.clone();
                Task::local(async move {
                    let mut knock_channels = Vec::new();
                    while let Ok(Some(req)) = stream.try_next().await {
                        match req {
                            fidl_rcs::RemoteControlRequest::IdentifyHost { responder } => {
                                let addrs = vec![fidl_fuchsia_net::Subnet {
                                    addr: fidl_fuchsia_net::IpAddress::Ipv4(
                                        fidl_fuchsia_net::Ipv4Address { addr: [192, 168, 1, 2] },
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
                                capability_set,
                                capability_name,
                                server_channel,
                                responder,
                            } => {
                                assert_eq!(capability_set, fsys::OpenDirType::ExposedDir);
                                assert_eq!(moniker, "/core/remote-control");
                                assert_eq!(
                                    capability_name,
                                    "fuchsia.developer.remotecontrol.RemoteControl"
                                );
                                knock_channels.push(server_channel);
                                responder.send(Ok(())).unwrap();
                            }
                            _ => panic!("unsupported for this test"),
                        }
                    }
                })
                .detach();
            }
        })
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_open_rcs_valid() {
        const TEST_NODE_NAME: &'static str = "villete";
        let local_node = overnet_core::Router::new(None).unwrap();
        let node2 = overnet_core::Router::new(None).unwrap();
        let (rx2, tx2) = fidl::Socket::create_stream();
        let (mut rx2, mut tx2) =
            (fidl::AsyncSocket::from_socket(rx2), fidl::AsyncSocket::from_socket(tx2));
        let (rx1, tx1) = fidl::Socket::create_stream();
        let (mut rx1, mut tx1) =
            (fidl::AsyncSocket::from_socket(rx1), fidl::AsyncSocket::from_socket(tx1));
        let (error_sink, _) = futures::channel::mpsc::unbounded();
        let error_sink_clone = error_sink.clone();
        let local_node_clone = Arc::clone(&local_node);
        let _h1_task = Task::local(async move {
            circuit::multi_stream::multi_stream_node_connection_to_async(
                local_node_clone.circuit_node(),
                &mut rx1,
                &mut tx2,
                true,
                circuit::Quality::IN_PROCESS,
                error_sink_clone,
                "h2".to_owned(),
            )
            .await
        });
        let node2_clone = Arc::clone(&node2);
        let _h2_task = Task::local(async move {
            circuit::multi_stream::multi_stream_node_connection_to_async(
                node2_clone.circuit_node(),
                &mut rx2,
                &mut tx1,
                false,
                circuit::Quality::IN_PROCESS,
                error_sink,
                "h1".to_owned(),
            )
            .await
        });
        let (sender, receiver) = futures::channel::mpsc::unbounded();
        let _svc_task = spawn_protocol_provider(TEST_NODE_NAME.to_owned(), receiver);
        node2
            .register_service(
                fidl_rcs::RemoteControlMarker::PROTOCOL_NAME.to_owned(),
                move |chan| {
                    let _ = sender.unbounded_send(chan);
                    Ok(())
                },
            )
            .await
            .unwrap();
        let daemon = FakeDaemonBuilder::new().build();
        let cx = Context::new(daemon);
        let lpc = local_node.new_list_peers_context().await;
        while lpc.list_peers().await.unwrap().iter().all(|x| x.is_self) {}
        let (client, server) = fidl::Channel::create();
        local_node
            .connect_to_service(
                node2.node_id(),
                fidl_rcs::RemoteControlMarker::PROTOCOL_NAME,
                server,
            )
            .await
            .unwrap();
        let rcs_proxy = fidl_rcs::RemoteControlProxy::new(fidl::AsyncChannel::from_channel(client));
        let rcs =
            RcsConnection::new_with_proxy(local_node, rcs_proxy.clone(), &node2.node_id().into());

        let identify = rcs.identify_host().await.unwrap();
        let (update, _) = TargetUpdateBuilder::from_rcs_identify(rcs.clone(), &identify);
        let target = Target::new();
        target.apply_update(update.build());

        let (target_proxy, server) = fidl::endpoints::create_proxy::<ffx::TargetMarker>();
        let tc = Rc::new(TargetCollection::new());
        let _handle = Task::local(TargetHandle::new(target, cx, server, tc.clone()).unwrap());
        let (rcs, rcs_server) = fidl::endpoints::create_proxy::<fidl_rcs::RemoteControlMarker>();
        let res = target_proxy.open_remote_control(rcs_server).await.unwrap();
        assert!(res.is_ok());
        assert_eq!(TEST_NODE_NAME, rcs.identify_host().await.unwrap().unwrap().nodename.unwrap());
    }
}
