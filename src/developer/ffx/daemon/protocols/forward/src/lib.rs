// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{Context as _, Result};
use async_trait::async_trait;
use ffx_config::ConfigLevel;
use ffx_target_net::PortForwarder;
use fidl_fuchsia_developer_ffx as ffx;
use fidl_fuchsia_net::SocketAddress;
use fidl_fuchsia_net_ext::SocketAddress as SocketAddressExt;
use futures::FutureExt as _;
use protocols::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;
use tokio::net::TcpListener;

#[ffx_protocol]
#[derive(Default)]
pub struct Forward(Arc<tasks::TaskManager>);

#[derive(Deserialize, Serialize)]
enum ForwardConfigType {
    Tcp,
}

#[derive(Deserialize, Serialize)]
struct ForwardConfig {
    #[serde(rename = "type")]
    ty: ForwardConfigType,
    target: String,
    host_address: std::net::SocketAddr,
    target_address: std::net::SocketAddr,
}

const TUNNEL_CFG: &'static str = "tunnels";
const FORWARD_SETUP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

impl Forward {
    async fn port_forward_task(
        cx: Context,
        target: String,
        target_address: SocketAddress,
        listener: TcpListener,
    ) {
        let target = match cx.open_remote_control(Some(target.clone())).await {
            Ok(t) => t,
            Err(e) => {
                log::error!("Could not connect to proxy for TCP forwarding: {:?}", e);
                return;
            }
        };

        let forwarder = match PortForwarder::new_with_rcs(FORWARD_SETUP_TIMEOUT, &target).await {
            Ok(p) => p,
            Err(e) => {
                log::error!("Error requesting port forward from RCS: {:?}", e);
                return;
            }
        };

        forwarder
            .forward(listener, SocketAddressExt::from(target_address).0)
            .await
            .unwrap_or_else(|e| log::error!("Error port forwarding: {e:?}"));
    }

    async fn bind_or_log(addr: std::net::SocketAddr) -> Result<TcpListener, ()> {
        TcpListener::bind(addr).await.map_err(|e| {
            log::error!("Could not listen on {:?}: {:?}", addr, e);
        })
    }
}

#[async_trait(?Send)]
impl FidlProtocol for Forward {
    type Protocol = ffx::TunnelMarker;
    type StreamHandler = FidlInstancedStreamHandler<Self>;

    async fn handle(&self, cx: &Context, req: ffx::TunnelRequest) -> Result<()> {
        let cx = cx.clone();

        match req {
            ffx::TunnelRequest::ForwardPort { target, host_address, target_address, responder } => {
                let host_address: SocketAddressExt = host_address.into();
                let host_address = host_address.0;
                let target_address_cfg: SocketAddressExt = target_address.clone().into();
                let target_address_cfg = target_address_cfg.0;
                let listener = match Self::bind_or_log(host_address).await {
                    Ok(t) => t,
                    Err(_) => {
                        return responder
                            .send(Err(ffx::TunnelError::CouldNotListen))
                            .context("error sending response");
                    }
                };

                self.0.spawn(Self::port_forward_task(cx, target.clone(), target_address, listener));

                let cfg = serde_json::to_value(ForwardConfig {
                    ty: ForwardConfigType::Tcp,
                    target,
                    host_address,
                    target_address: target_address_cfg,
                })?;

                let query = ffx_config::query(TUNNEL_CFG).level(Some(ConfigLevel::User));
                if let Err(e) = query.add(cfg).await {
                    log::warn!("Failed to persist tunnel configuration: {:?}", e);
                }

                responder.send(Ok(())).context("error sending response")?;
                Ok(())
            }
            ffx::TunnelRequest::ReversePort { target, host_address, target_address, responder } => {
                let target = match cx.open_remote_control(Some(target)).await {
                    Ok(t) => t,
                    Err(e) => {
                        log::error!("Could not connect to proxy for TCP forwarding: {:?}", e);
                        return responder
                            .send(Err(ffx::TunnelError::TargetConnectFailed))
                            .context("error sending response");
                    }
                };

                let forwarder = match PortForwarder::new_with_rcs(FORWARD_SETUP_TIMEOUT, &target)
                    .await
                {
                    Ok(p) => p,
                    Err(e) => {
                        log::error!("Error connecting to forwarding protocol from RCS: {:?}", e);
                        return responder
                            .send(Err(ffx::TunnelError::TargetConnectFailed))
                            .context("error sending response");
                    }
                };

                let target_listener = match forwarder
                    .socket_provider()
                    .listen(SocketAddressExt::from(target_address).0, None)
                    .await
                {
                    Ok(t) => t,
                    Err(e) => {
                        log::error!("Error creating target-side listener: {:?}", e);
                        return responder
                            .send(Err(ffx::TunnelError::TargetConnectFailed))
                            .context("error sending response");
                    }
                };
                let task =
                    forwarder.reverse(target_listener, SocketAddressExt::from(host_address).0);
                self.0.spawn(
                    task.map(|r| {
                        r.unwrap_or_else(|e| log::error!("Error during forwarding {e:?}"))
                    }),
                );
                responder.send(Ok(())).context("error sending response")?;
                Ok(())
            }
        }
    }

    async fn start(&mut self, cx: &Context) -> Result<()> {
        log::info!("started port forwarding protocol");

        let tunnels: Vec<Value> = ffx_config::get(TUNNEL_CFG).unwrap_or_else(|_| Vec::new());

        for tunnel in tunnels {
            let tunnel: ForwardConfig = match serde_json::from_value(tunnel) {
                Ok(tunnel) => tunnel,
                Err(e) => {
                    log::warn!("Malformed tunnel config: {:?}", e);
                    continue;
                }
            };

            match tunnel.ty {
                ForwardConfigType::Tcp => {
                    let target_address = SocketAddressExt(tunnel.target_address);
                    let listener = match Self::bind_or_log(tunnel.host_address).await {
                        Ok(t) => t,
                        Err(_) => continue,
                    };
                    self.0.spawn(Self::port_forward_task(
                        cx.clone(),
                        tunnel.target,
                        target_address.into(),
                        listener,
                    ));
                }
            }
        }
        Ok(())
    }

    async fn stop(&mut self, _cx: &Context) -> Result<()> {
        log::info!("stopped port forwarding protocol");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ffx::DaemonError;
    use futures::StreamExt;
    use {
        fidl_fuchsia_developer_remotecontrol as rcs, fidl_fuchsia_posix_socket as fsock,
        fidl_fuchsia_sys2 as sys2,
    };

    static HOST_ADDRESS: &str = "127.0.0.1:1234";
    static TARGET_ADDRESS: &str = "127.0.0.1:5678";

    fn host_address() -> SocketAddress {
        SocketAddressExt(HOST_ADDRESS.parse().unwrap()).into()
    }

    fn target_address() -> SocketAddress {
        SocketAddressExt(TARGET_ADDRESS.parse().unwrap()).into()
    }

    async fn test_stream_socket(mut stream: fsock::StreamSocketRequestStream) {
        let mut bound = false;
        let mut listening = false;
        let mut describe_endpoint = None;
        while let Some(Ok(request)) = stream.next().await {
            match request {
                fsock::StreamSocketRequest::Bind { addr, responder } => {
                    assert_eq!(target_address(), addr);
                    assert!(!bound, "bound socket twice");
                    bound = true;
                    responder.send(Ok(())).unwrap();
                }
                fsock::StreamSocketRequest::Describe { responder } => {
                    assert!(describe_endpoint.is_none());
                    let (socket, endpoint) = fidl::Socket::create_stream();
                    describe_endpoint = Some(endpoint);
                    responder
                        .send(fsock::StreamSocketDescribeResponse {
                            socket: Some(socket),
                            ..Default::default()
                        })
                        .unwrap()
                }
                fsock::StreamSocketRequest::Listen { backlog: _, responder } => {
                    assert!(bound, "listened to unbound socket");
                    assert!(!listening, "listened to socket twice");
                    listening = true;
                    responder.send(Ok(())).unwrap();
                }
                fsock::StreamSocketRequest::GetSockName { responder } => {
                    responder.send(Ok(&target_address())).unwrap();
                }
                other => panic!("Unexpected request: {other:?}"),
            }
        }
    }
    async fn test_socket_provider(channel: fidl::Channel) {
        println!("Spawning test provider");
        let channel = fidl::endpoints::ServerEnd::<fsock::ProviderMarker>::from(channel);
        let mut stream = channel.into_stream();

        while let Some(Ok(request)) = stream.next().await {
            match request {
                fsock::ProviderRequest::StreamSocket { domain, proto, responder } => {
                    assert_eq!(fsock::Domain::Ipv4, domain);
                    assert_eq!(fsock::StreamSocketProtocol::Tcp, proto);
                    let (client, stream) =
                        fidl::endpoints::create_request_stream::<fsock::StreamSocketMarker>();
                    fuchsia_async::Task::spawn(test_stream_socket(stream)).detach();
                    responder.send(Ok(client)).unwrap();
                }
                other => panic!("Unexpected request: {other:?}"),
            }
        }
    }

    #[derive(Default, Clone)]
    struct TestDaemon;

    #[async_trait(?Send)]
    impl DaemonProtocolProvider for TestDaemon {
        async fn open_remote_control(
            &self,
            target_identifier: Option<String>,
        ) -> Result<rcs::RemoteControlProxy> {
            let (client, server) = fidl::endpoints::create_endpoints::<rcs::RemoteControlMarker>();
            assert_eq!(target_identifier, Some("dummy_target".to_owned()));

            fuchsia_async::Task::local(async move {
                let mut server = server.into_stream();
                while let Some(request) = server.next().await {
                    match request.unwrap() {
                        rcs::RemoteControlRequest::ConnectCapability {
                            moniker: _,
                            capability_set,
                            capability_name,
                            server_channel,
                            responder,
                        } => {
                            assert_eq!(sys2::OpenDirType::NamespaceDir, capability_set);
                            assert_eq!("svc/fuchsia.posix.socket.Provider", capability_name);
                            fuchsia_async::Task::spawn(test_socket_provider(server_channel))
                                .detach();
                            responder.send(Ok(())).unwrap();
                        }
                        other => panic!("Unexpected request: {:?}", other),
                    }
                }
            })
            .detach();

            Ok(client.into_proxy())
        }

        async fn open_protocol(&self, _name: String) -> Result<fidl::Channel> {
            unimplemented!()
        }

        async fn open_target_proxy(
            &self,
            _target_identifier: Option<String>,
            _moniker: &str,
            _capability_name: &str,
        ) -> Result<fidl::Channel> {
            unimplemented!()
        }

        async fn open_target_proxy_with_info(
            &self,
            _target_identifier: Option<String>,
            _moniker: &str,
            _capability_name: &str,
        ) -> Result<(ffx::TargetInfo, fidl::Channel)> {
            unimplemented!()
        }

        async fn get_target_info(
            &self,
            _target_identifier: Option<String>,
        ) -> Result<ffx::TargetInfo, DaemonError> {
            unimplemented!()
        }
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_forward() {
        let forward = Forward::default();
        let context = Context::new(TestDaemon);
        let (client, server) = fidl::endpoints::create_endpoints::<ffx::TunnelMarker>();

        fuchsia_async::Task::local(async move {
            let mut server = server.into_stream();
            while let Some(request) = server.next().await {
                let request = request.unwrap();
                forward.handle(&context, request).await.unwrap();
            }
        })
        .detach();

        client
            .into_proxy()
            .forward_port("dummy_target", &host_address(), &target_address())
            .await
            .unwrap()
            .unwrap();
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_reverse() {
        let forward = Forward::default();
        let context = Context::new(TestDaemon);
        let (client, server) = fidl::endpoints::create_endpoints::<ffx::TunnelMarker>();

        fuchsia_async::Task::local(async move {
            let mut server = server.into_stream();
            while let Some(request) = server.next().await {
                let request = request.unwrap();
                forward.handle(&context, request).await.unwrap();
            }
        })
        .detach();

        client
            .into_proxy()
            .reverse_port("dummy_target", &host_address(), &target_address())
            .await
            .unwrap()
            .unwrap();
    }
}
