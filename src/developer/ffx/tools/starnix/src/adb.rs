// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{anyhow, bail, Context, Result};
use argh::FromArgs;
use async_net::{TcpListener, TcpStream};
use component_debug::cli;
use fho::SimpleWriter;
use fidl::Status;
use fidl_fuchsia_developer_remotecontrol as rc;
use fidl_fuchsia_starnix_container::ControllerMarker;
use fidl_fuchsia_sys2 as fsys;
use fuchsia_async as fasync;
use futures::io::AsyncReadExt;
use futures::stream::StreamExt;
use lazy_static::lazy_static;
use regex::Regex;
use signal_hook::{consts::signal::SIGINT, iterator::Signals};

const ADB_DEFAULT_PORT: u32 = 5555;

async fn serve_adb_connection(mut stream: TcpStream, bridge_socket: fidl::Socket) -> Result<()> {
    let mut bridge = fidl::AsyncSocket::from_socket(bridge_socket)?;
    let (breader, mut bwriter) = (&mut bridge).split();
    let (sreader, mut swriter) = (&mut stream).split();

    let copy_futures = futures::future::try_join(
        futures::io::copy(breader, &mut swriter),
        futures::io::copy(sreader, &mut bwriter),
    );

    copy_futures.await?;

    Ok(())
}

const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

/// Returns the absolute moniker for the container in the session, if there is one.
async fn find_session_container(rcs_proxy: &rc::RemoteControlProxy) -> Result<String> {
    lazy_static! {
        // Example: /core/session-manager/session:session/elements:5udqa81zlypamvgu/container
        static ref SESSION_CONTAINER: Regex =
            Regex::new(r"^/core/session-manager/session:session/elements:\w+/container$")
                .unwrap();
    }

    let (query_proxy, query_server_end) = fidl::endpoints::create_proxy::<fsys::RealmQueryMarker>()
        .context("creating query proxy")?;
    rcs_proxy
        .root_realm_query(query_server_end)
        .await?
        .map_err(|i| Status::ok(i).unwrap_err())
        .context("opening realm query")?;

    let instances = cli::list::get_instances_matching_filter(None, &query_proxy).await?;
    let containers: Vec<_> = instances
        .into_iter()
        .filter(|i| {
            let moniker = i.moniker.to_string();
            (*SESSION_CONTAINER).is_match(&moniker)
        })
        .collect();

    if containers.is_empty() {
        println!("Unable to find Starnix container in the session.");
        println!("Please specify a container with --moniker");
        bail!("cannot find container")
    }

    if containers.len() > 1 {
        println!("Found multiple Starnix containers in the session:");
        for container in containers.iter() {
            println!("  {}", container.moniker.to_string())
        }
        println!("Please specify a container with --moniker");
        bail!("too many containers")
    }

    Ok(containers[0].moniker.to_string())
}

async fn find_moniker(
    rcs_proxy: &rc::RemoteControlProxy,
    command: &StarnixAdbCommand,
) -> Result<String> {
    if let Some(moniker) = &command.moniker {
        return Ok(moniker.clone());
    }
    find_session_container(&rcs_proxy).await
}

fn moniker_to_selector(moniker: String) -> String {
    let moniker = if moniker.starts_with('/') { &moniker[1..] } else { &moniker };
    return format!("{}:expose:fuchsia.starnix.container.Controller", moniker.replace(":", "\\:"));
}

#[derive(FromArgs, Debug, PartialEq)]
#[argh(
    subcommand,
    name = "adb",
    example = "ffx starnix adb",
    description = "Bridge from host adb to adbd running inside starnix"
)]
pub struct StarnixAdbCommand {
    /// the moniker of the container running adbd
    /// (defaults to looking for a container in the current session)
    #[argh(option, short = 'm')]
    pub moniker: Option<String>,

    /// which port to serve the adb server on
    #[argh(option, short = 'p', default = "5556")]
    pub port: u16,
}

pub async fn starnix_adb(
    command: &StarnixAdbCommand,
    rcs_proxy: &rc::RemoteControlProxy,
    _writer: SimpleWriter,
) -> Result<()> {
    let (controller_proxy, controller_server_end) =
        fidl::endpoints::create_proxy::<ControllerMarker>().context("failed to create proxy")?;

    let selector = moniker_to_selector(find_moniker(&rcs_proxy, &command).await?);
    rcs::connect_with_timeout(TIMEOUT, &selector, &rcs_proxy, controller_server_end.into_channel())
        .await?;

    println!("starnix adb - listening");

    let mut signals = Signals::new(&[SIGINT]).unwrap();
    let handle = signals.handle();
    let thread = std::thread::spawn(move || {
        for signal in signals.forever() {
            match signal {
                SIGINT => {
                    eprintln!("Caught interrupt. Shutting down starnix adb bridge...");
                    std::process::exit(0);
                }
                _ => unreachable!(),
            }
        }
    });

    let address = &format!("127.0.0.1:{}", &command.port);
    let listener = TcpListener::bind(address).await.expect("cannot bind to adb address");
    println!("The adb bridge is listening on {}", address);
    println!("To connect: adb connect {}", address);
    while let Some(stream) = listener.incoming().next().await {
        let stream = stream?;
        let (sbridge, cbridge) = fidl::Socket::create_stream();

        controller_proxy
            .vsock_connect(ADB_DEFAULT_PORT, sbridge)
            .map_err(|e| anyhow!("Error connecting to adbd: {:?}", e))?;

        fasync::Task::spawn(async move {
            serve_adb_connection(stream, cbridge)
                .await
                .unwrap_or_else(|e| println!("serve_adb_connection returned with {:?}", e));
        })
        .detach();
    }

    handle.close();
    thread.join().expect("signal thread to shutdown without panic");
    Ok(())
}

#[cfg(test)]
mod test {
    use super::*;
    use futures::AsyncWriteExt;

    async fn run_connection(listener: TcpListener, socket: fidl::Socket) {
        if let Some(stream) = listener.incoming().next().await {
            let stream = stream.unwrap();
            serve_adb_connection(stream, socket).await.unwrap();
        } else {
            panic!("did not get a connection");
        }
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_adb_relay() {
        let any_local_address = "127.0.0.1:0";
        let listener = TcpListener::bind(any_local_address).await.unwrap();
        let local_address = listener.local_addr().unwrap();

        let port = local_address.port();

        let (sbridge, cbridge) = fidl::Socket::create_stream();

        fasync::Task::spawn(async move {
            run_connection(listener, sbridge).await;
        })
        .detach();

        let connect_address = format!("127.0.0.1:{}", port);
        let mut stream = TcpStream::connect(connect_address).await.unwrap();

        let test_data_1: Vec<u8> = vec![1, 2, 3, 4, 5];
        stream.write_all(&test_data_1).await.unwrap();

        let mut buf = [0u8; 64];
        let mut async_socket = fidl::AsyncSocket::from_socket(cbridge).unwrap();
        let bytes_read = async_socket.read(&mut buf).await.unwrap();
        assert_eq!(test_data_1.len(), bytes_read);
        for (a, b) in test_data_1.iter().zip(buf[..bytes_read].iter()) {
            assert_eq!(a, b);
        }

        let test_data_2: Vec<u8> = vec![6, 7, 8, 9, 10, 11];
        let bytes_written = async_socket.write(&test_data_2).await.unwrap();
        assert_eq!(bytes_written, test_data_2.len());

        let mut buf = [0u8; 64];
        let bytes_read = stream.read(&mut buf).await.unwrap();
        assert_eq!(bytes_written, bytes_written);

        for (a, b) in test_data_2.iter().zip(buf[..bytes_read].iter()) {
            assert_eq!(a, b);
        }
    }
}
