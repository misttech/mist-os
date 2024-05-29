// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{Context, Result};
use argh::{ArgsInfo, FromArgs};
use async_net::{Ipv4Addr, TcpListener, TcpStream};
use fho::Connector;
use fidl_fuchsia_developer_remotecontrol as rc;
use fidl_fuchsia_starnix_container as fstarcontainer;
use fuchsia_async as fasync;
use futures::io::AsyncReadExt;
use futures::stream::StreamExt;
use futures::FutureExt;
use signal_hook::{consts::signal::SIGINT, iterator::Signals};
use std::io::ErrorKind;
use std::net::{SocketAddrV4, TcpListener as SyncTcpListener};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tracing::info;

use crate::common::*;

const ADB_DEFAULT_PORT: u32 = 5555;

#[derive(ArgsInfo, FromArgs, Debug, PartialEq)]
#[argh(
    subcommand,
    name = "adb",
    example = "ffx starnix adb proxy",
    description = "Bridge from host adb to adbd running inside starnix"
)]
pub struct StarnixAdbCommand {
    /// path to the adb client command
    #[argh(option, default = "String::from(\"adb\")")]
    adb: String,
    #[argh(subcommand)]
    subcommand: AdbSubcommand,
}

#[derive(ArgsInfo, FromArgs, Debug, PartialEq)]
#[argh(subcommand)]
enum AdbSubcommand {
    Proxy(AdbProxyArgs),
}

impl StarnixAdbCommand {
    pub async fn run(&self, rcs_connector: &Connector<rc::RemoteControlProxy>) -> Result<()> {
        match &self.subcommand {
            AdbSubcommand::Proxy(args) => args.run_proxy(&self.adb, rcs_connector).await,
        }
    }
}

async fn serve_adb_connection(mut stream: TcpStream, bridge_socket: fidl::Socket) -> Result<()> {
    let mut bridge = fidl::AsyncSocket::from_socket(bridge_socket);
    let (breader, mut bwriter) = (&mut bridge).split();
    let (sreader, mut swriter) = (&mut stream).split();

    let copy_result = futures::select! {
        r = futures::io::copy(breader, &mut swriter).fuse() => {
            r
        },
        r = futures::io::copy(sreader, &mut bwriter).fuse() => {
            r
        },
    };

    copy_result.map(|_| ()).map_err(|e| e.into())
}

fn find_open_port(start: u16) -> u16 {
    let mut addr = SocketAddrV4::new(Ipv4Addr::new(127, 0, 0, 1), start);

    info!("probing for an open port for the adb bridge...");
    loop {
        info!("probing {addr:?}...");
        match SyncTcpListener::bind(addr) {
            Ok(_) => {
                info!("{addr:?} appears to be available");
                return addr.port();
            }
            Err(e) => {
                info!("{addr:?} appears unavailable: {e:?}");
                addr.set_port(
                    addr.port().checked_add(1).expect("should find open port before overflow"),
                );
            }
        }
    }
}

#[derive(ArgsInfo, FromArgs, Debug, PartialEq)]
#[argh(
    subcommand,
    name = "proxy",
    description = "Bridge from host adb to adbd running inside starnix"
)]
struct AdbProxyArgs {
    /// the moniker of the container running adbd
    /// (defaults to looking for a container in the current session)
    #[argh(option, short = 'm')]
    pub moniker: Option<String>,

    /// which port to serve the adb server on
    #[argh(option, short = 'p', default = "find_open_port(5556)")]
    pub port: u16,

    /// disable automatically running "adb connect"
    #[argh(switch)]
    pub no_autoconnect: bool,
}

impl AdbProxyArgs {
    async fn run_proxy(
        &self,
        adb: &str,
        rcs_connector: &Connector<rc::RemoteControlProxy>,
    ) -> Result<()> {
        let reconnect = || async {
            let rcs_proxy = connect_to_rcs(rcs_connector).await?;
            anyhow::Ok((
                connect_to_contoller(&rcs_proxy, self.moniker.clone()).await?,
                Arc::new(AtomicBool::new(false)),
            ))
        };
        let mut controller_proxy = reconnect().await?;

        let mut signals = Signals::new(&[SIGINT]).unwrap();
        let handle = signals.handle();
        let signal_thread = std::thread::spawn(move || {
            if let Some(signal) = signals.forever().next() {
                assert_eq!(signal, SIGINT);
                eprintln!("Caught interrupt. Shutting down starnix adb bridge...");
                std::process::exit(0);
            }
        });

        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, self.port))
            .await
            .expect("cannot bind to adb address");
        let listen_address = listener.local_addr().expect("cannot get adb server address");

        if !self.no_autoconnect {
            // It's necessary to run adb connect on a separate thread so it doesn't block the async
            // executor running the socket listener.
            let adb_path = adb.to_string();
            std::thread::spawn(move || {
                let mut adb_command = Command::new(&adb_path);
                adb_command.arg("connect").arg(listen_address.to_string());
                match adb_command.status() {
                    Ok(_) => {}
                    Err(io_err) if io_err.kind() == ErrorKind::NotFound => {
                        panic!("Could not find adb binary named `{adb_path}`. If your adb is not in your $PATH, use the --adb flag to specify where to find it.");
                    }
                    Err(io_err) => {
                        panic!("Failed to run `${adb_command:?}`: {io_err:?}");
                    }
                }
            });
        } else {
            println!("ADB bridge started. To connect: adb connect {listen_address}");
        }

        while let Some(stream) = listener.incoming().next().await {
            if controller_proxy.1.load(Ordering::SeqCst) {
                controller_proxy = reconnect().await?;
            }

            let stream = stream?;
            let (sbridge, cbridge) = fidl::Socket::create_stream();

            controller_proxy
                .0
                .vsock_connect(fstarcontainer::ControllerVsockConnectRequest {
                    port: Some(ADB_DEFAULT_PORT),
                    bridge_socket: Some(sbridge),
                    ..Default::default()
                })
                .context("connecting to adbd")?;

            let reconnect_flag = Arc::clone(&controller_proxy.1);
            fasync::Task::spawn(async move {
                serve_adb_connection(stream, cbridge)
                    .await
                    .unwrap_or_else(|e| println!("serve_adb_connection returned with {:?}", e));
                reconnect_flag.store(true, std::sync::atomic::Ordering::SeqCst);
            })
            .detach();
        }

        handle.close();
        signal_thread.join().expect("signal thread to shutdown without panic");
        Ok(())
    }
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
        let mut async_socket = fidl::AsyncSocket::from_socket(cbridge);
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
