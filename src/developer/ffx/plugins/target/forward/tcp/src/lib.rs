// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use async_trait::async_trait;
use errors::ffx_bail;
use ffx_config::keys::TARGET_DEFAULT_KEY;
use ffx_target_forward_tcp_args::TcpCommand;
use fho::{daemon_protocol, FfxContext, FfxMain, FfxTool, SimpleWriter};
use fidl_fuchsia_developer_ffx as ffx;

#[derive(FfxTool)]
pub struct ForwardTcpTool {
    #[command]
    cmd: TcpCommand,
    #[with(daemon_protocol())]
    forward_port: ffx::TunnelProxy,
}

fho::embedded_plugin!(ForwardTcpTool);

#[async_trait(?Send)]
impl FfxMain for ForwardTcpTool {
    type Writer = SimpleWriter;
    async fn main(self, _writer: Self::Writer) -> fho::Result<()> {
        forward_tcp(self.forward_port, self.cmd).await
    }
}

async fn forward_tcp(forward_port: ffx::TunnelProxy, cmd: TcpCommand) -> fho::Result<()> {
    let target: Option<String> = ffx_config::get(TARGET_DEFAULT_KEY)
        .await
        .user_message("Failed to get default target from config")?;
    let target = if let Some(target) = target {
        target
    } else {
        ffx_bail!("Specify a target or configure a default target")
    };

    forward_tcp_impl(&target, forward_port, cmd).await
}

async fn forward_tcp_impl(
    target: &str,
    forward_port: ffx::TunnelProxy,
    cmd: TcpCommand,
) -> fho::Result<()> {
    match forward_port
        .forward_port(target, &cmd.host_address, &cmd.target_address)
        .await
        .user_message("Failed to call ForwardPort: {err}")?
    {
        Ok(()) => Ok(()),
        Err(ffx::TunnelError::CouldNotListen) => {
            ffx_bail!("Could not listen on address {:?}", cmd.host_address)
        }
        Err(e) => ffx_bail!("Unexpected error forwarding port: {e:?}"),
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use fidl_fuchsia_net as net;

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_forward() {
        let host_address_test = net::SocketAddress::Ipv4(net::Ipv4SocketAddress {
            address: net::Ipv4Address { addr: [1, 2, 3, 4] },
            port: 1234,
        });
        let target_address_test = net::SocketAddress::Ipv4(net::Ipv4SocketAddress {
            address: net::Ipv4Address { addr: [5, 6, 7, 8] },
            port: 5678,
        });

        let forward_port_proxy = fho::testing::fake_proxy({
            let host_address_test = host_address_test.clone();
            let target_address_test = target_address_test.clone();
            move |req| match req {
                ffx::TunnelRequest::ForwardPort {
                    target,
                    host_address,
                    target_address,
                    responder,
                } => {
                    assert_eq!("dummy_target", target);
                    assert_eq!(host_address_test, host_address);
                    assert_eq!(target_address_test, target_address);
                    responder.send(Ok(())).expect("should send response");
                }
                other => panic!("Unexpected request: {:?}", other),
            }
        });

        forward_tcp_impl(
            "dummy_target",
            forward_port_proxy,
            TcpCommand {
                host_address: host_address_test.clone(),
                target_address: target_address_test.clone(),
            },
        )
        .await
        .expect("should succeed");
    }
}
