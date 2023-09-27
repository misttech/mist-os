// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{Context as _, Error};
use clap::{App, Arg, SubCommand};
use fidl::endpoints::{ClientEnd, ServerEnd};
use fidl::prelude::*;
use fidl_fuchsia_overnet::{ServiceProviderRequest, ServiceProviderRequestStream};
use fidl_fuchsia_overnet_examples_interfacepassing as interfacepassing;
use fidl_test_placeholders as echo;
use futures::prelude::*;
use std::sync::Arc;

fn app<'a, 'b>() -> App<'a, 'b> {
    App::new("overnet-interface-passing")
        .version("0.1.0")
        .about("Interface passing example for overnet")
        .author("Fuchsia Team")
        .subcommand(SubCommand::with_name("client").about("Run as client").arg(
            Arg::with_name("text").help("Text string to echo back and forth").takes_value(true),
        ))
        .subcommand(SubCommand::with_name("server").about("Run as server"))
}

////////////////////////////////////////////////////////////////////////////////
// Client implementation

async fn exec_client(node: Arc<overnet_core::Router>, text: Option<&str>) -> Result<(), Error> {
    let list_peers_context = node.new_list_peers_context().await;
    loop {
        let peers = list_peers_context.list_peers().await?;
        println!("Got peers: {:?}", peers);
        for peer in peers {
            if peer.description.services.is_none() {
                continue;
            }
            if peer
                .description
                .services
                .unwrap()
                .iter()
                .find(|name| *name == interfacepassing::ExampleMarker::PROTOCOL_NAME)
                .is_none()
            {
                continue;
            }
            let (s, p) = fidl::Channel::create();
            if let Err(e) = node
                .connect_to_service(
                    peer.id.into(),
                    interfacepassing::ExampleMarker::PROTOCOL_NAME,
                    s,
                )
                .await
            {
                println!("{:?}", e);
                continue;
            }
            let proxy =
                fidl::AsyncChannel::from_channel(p).context("failed to make async channel")?;
            let cli = interfacepassing::ExampleProxy::new(proxy);

            let (s1, p1) = fidl::Channel::create();
            let proxy_echo =
                fidl::AsyncChannel::from_channel(p1).context("failed to make async channel")?;
            let cli_echo = echo::EchoProxy::new(proxy_echo);
            println!("Sending {:?} to {:?}", text, peer.id);
            if let Err(e) = cli.request(ServerEnd::new(s1)) {
                println!("ERROR REQUESTING INTERFACE: {:?}", e);
                continue;
            }
            println!("received {:?}", cli_echo.echo_string(text).await?);
            return Ok(());
        }
    }
}

////////////////////////////////////////////////////////////////////////////////
// Server implementation

async fn echo_server(chan: ServerEnd<echo::EchoMarker>, quiet: bool) -> Result<(), Error> {
    chan.into_stream()?
        .map_err(Into::into)
        .try_for_each_concurrent(
            None,
            |echo::EchoRequest::EchoString { value, responder }| async move {
                if !quiet {
                    println!("Received echo request for string {:?}", value);
                }
                responder.send(value.as_ref().map(|s| &**s)).context("error sending response")?;
                if !quiet {
                    println!("echo response sent successfully");
                }
                Ok(())
            },
        )
        .await
}

async fn example_server(chan: fidl::AsyncChannel, quiet: bool) -> Result<(), Error> {
    interfacepassing::ExampleRequestStream::from_channel(chan)
        .map_err(Into::into)
        .try_for_each_concurrent(
            None,
            |interfacepassing::ExampleRequest::Request { iface, .. }| {
                if !quiet {
                    println!("Received interface request");
                }
                echo_server(iface, quiet)
            },
        )
        .await
}

async fn exec_server(node: Arc<overnet_core::Router>, quiet: bool) -> Result<(), Error> {
    let (s, p) = fidl::Channel::create();
    let chan = fidl::AsyncChannel::from_channel(s).context("failed to make async channel")?;
    node.register_service(
        interfacepassing::ExampleMarker::PROTOCOL_NAME.to_owned(),
        ClientEnd::new(p),
    )
    .await?;
    ServiceProviderRequestStream::from_channel(chan)
        .map_err(Into::into)
        .try_for_each_concurrent(
            None,
            |ServiceProviderRequest::ConnectToService {
                 chan,
                 info: _,
                 control_handle: _control_handle,
             }| {
                async move {
                    if !quiet {
                        println!("Received service request for service");
                    }
                    let chan = fidl::AsyncChannel::from_channel(chan)
                        .context("failed to make async channel")?;
                    example_server(chan, quiet).await
                }
            },
        )
        .await
}

////////////////////////////////////////////////////////////////////////////////
// main

#[fuchsia_async::run_singlethreaded]
async fn main() -> Result<(), Error> {
    let hoist = hoist::Hoist::new(None)?;
    let _t = hoist.start_default_link()?;

    let args = app().get_matches();
    let node = hoist.node();

    match args.subcommand() {
        ("server", Some(_)) => exec_server(node, args.is_present("quiet")).await,
        ("client", Some(cmd)) => {
            let r = exec_client(node, cmd.value_of("text")).await;
            println!("finished client");
            r
        }
        (_, _) => unimplemented!(),
    }
}
